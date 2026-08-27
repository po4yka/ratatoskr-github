//! End-to-end incremental star scans: wiremock provider plus disposable
//! catalog database, exercising watermark windows, ordering-gap detection,
//! and baseline deferral to full snapshots.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    FullSnapshotOutcome, IncrementalScanOutcome, run_incremental_scan, upsert_repository,
};
use uuid::Uuid;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Seeds one connected account row and returns its id.
async fn seed_account(
    database: &ratatoskr_github_catalog::Database,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts
             (account_id, owner_ref, status, provider_user_id)
         values ($1, 'tester', 'connected', 1)",
    )
    .bind(account_id)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

/// Seeds the account's persisted high-water mark.
async fn seed_watermark(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    high_water_mark: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "insert into github_catalog.star_watermarks (account_id, high_water_mark)
         values ($1, $2::timestamptz)",
    )
    .bind(account_id)
    .bind(high_water_mark)
    .execute(database.pool())
    .await?;
    Ok(())
}

/// Seeds one starred authority row with an explicit established timestamp
/// and returns the local repository id.
async fn seed_prior_star(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    provider_repository_id: i64,
    starred_at: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let identity = upsert_repository(database, provider_repository_id).await?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, $3::timestamptz, now())",
    )
    .bind(account_id)
    .bind(identity.repository_id)
    .bind(starred_at)
    .execute(database.pool())
    .await?;
    Ok(identity.repository_id)
}

fn starred_item(id: i64, name: &str, starred_at: &str) -> String {
    format!(
        r#"{{"starred_at": "{starred_at}", "repo": {{
            "id": {id},
            "full_name": "{name}",
            "description": null,
            "language": "Rust",
            "stargazers_count": 1,
            "topics": [],
            "default_branch": "main",
            "pushed_at": null
        }}}}"#
    )
}

/// A listing entry whose provider timestamp is absent - the anomaly an
/// incremental scan can never order.
fn starred_item_without_timestamp(id: i64, name: &str) -> String {
    format!(
        r#"{{"starred_at": null, "repo": {{
            "id": {id},
            "full_name": "{name}",
            "description": null,
            "language": "Rust",
            "stargazers_count": 1,
            "topics": [],
            "default_branch": "main",
            "pushed_at": null
        }}}}"#
    )
}

/// Mounts one newest-first listing page served exactly once; the sort and
/// direction matchers pin the wire shape the watermark logic depends on.
async fn mount_newest_first_page(
    server: &MockServer,
    page: u32,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", page.to_string()))
        .and(query_param("sort", "created"))
        .and(query_param("direction", "desc"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4999")
                .insert_header("x-ratelimit-reset", "1787000000"),
        )
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts one unordered listing page served exactly once - the wire shape
/// the full-snapshot flow uses when an incremental request defers to it.
async fn mount_unordered_page(
    server: &MockServer,
    page: u32,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", page.to_string()))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4999")
                .insert_header("x-ratelimit-reset", "1787000000"),
        )
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts one newest-first listing page served at most once with a chosen
/// `x-ratelimit-remaining`, so budget refusal can park a run mid-scan.
async fn mount_newest_first_page_with_remaining(
    server: &MockServer,
    page: u32,
    body: String,
    remaining: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", page.to_string()))
        .and(query_param("sort", "created"))
        .and(query_param("direction", "desc"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", remaining)
                .insert_header("x-ratelimit-reset", exhausted_reset_epoch().to_string()),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
    Ok(())
}

fn exhausted_reset_epoch() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or_default()
        + 3600
}

/// Reads the page numbers the server received, in request order.
async fn requested_pages(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find_map(|(key, value)| (key == "page").then(|| value.into_owned()))
        })
        .collect()
}

/// Checks whether the account's persisted watermark equals a timestamp.
async fn watermark_is(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    timestamp: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let equal: bool = sqlx::query_scalar(
        "select high_water_mark = $2::timestamptz
             from github_catalog.star_watermarks where account_id = $1",
    )
    .bind(account_id)
    .bind(timestamp)
    .fetch_one(database.pool())
    .await?;
    Ok(equal)
}

/// Lists every known provider repository id in stable numeric order.
async fn all_provider_ids(
    database: &ratatoskr_github_catalog::Database,
) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "select provider_repository_id from github_catalog.repositories
         order by provider_repository_id",
    )
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .collect();
    Ok(ids)
}

#[tokio::test]
async fn incremental_scan_ingests_only_items_newer_than_watermark_and_advances_it()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let server = MockServer::start().await;

    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_010, "acme/newest", "2026-07-03T00:00:00Z"),
            starred_item(300_000_011, "acme/middle", "2026-06-02T00:00:00Z")
        ),
    )
    .await?;
    mount_newest_first_page(
        &server,
        2,
        format!(
            "[{}]",
            starred_item(300_000_012, "acme/older", "2026-04-01T00:00:00Z")
        ),
    )
    .await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("incremental-e2e"),
        account_id,
    )
    .await?;

    let IncrementalScanOutcome::Completed {
        run_id,
        pages_processed,
        items_observed,
    } = outcome
    else {
        return Err(format!("the windowed scan must complete, got {outcome:?}").into());
    };
    assert_eq!(
        pages_processed, 2,
        "the page proving coverage is processed before the scan stops"
    );
    assert_eq!(
        items_observed, 2,
        "only items strictly newer than the watermark count as observed"
    );

    // Only the newer-than-watermark repositories exist under stable identity.
    assert_eq!(
        all_provider_ids(&database.database).await?,
        [300_000_010, 300_000_011],
        "the older item must never be ingested by an incremental pass"
    );

    let starred_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.current_star_state c
         join github_catalog.repositories r on r.repository_id = c.repository_id
         where c.account_id = $1 and c.starred
           and r.provider_repository_id in (300000010, 300000011)",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        starred_count, 2,
        "ingested items become starred current state"
    );

    // The run row records an honestly completed incremental pass.
    let row: (String, i32, i32) = sqlx::query_as(
        "select status, pages_processed, items_observed
             from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(row.0, "completed", "the run must reach its terminal state");
    assert_eq!((row.1, row.2), (2, 2), "statistics match what was ingested");

    // The watermark advances exactly to the oldest ingested timestamp.
    assert!(
        watermark_is(&database.database, account_id, "2026-06-02T00:00:00Z").await?,
        "the watermark must advance to the oldest ingested item, not beyond"
    );

    // No page beyond the coverage proof was ever requested.
    assert_eq!(
        requested_pages(&server).await,
        ["1", "2"],
        "coverage proven on page two must end the scan without a third request"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn incremental_request_without_baseline_runs_full_snapshot_instead()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let server = MockServer::start().await;

    // The delegated full snapshot enumerates through the unordered listing.
    mount_unordered_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_020, "acme/baseline", "2026-06-10T00:00:00Z")
        ),
    )
    .await?;
    mount_unordered_page(&server, 2, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("incremental-e2e"),
        account_id,
    )
    .await?;

    let IncrementalScanOutcome::DeferredToFull(FullSnapshotOutcome::Completed {
        pages_processed,
        ..
    }) = outcome
    else {
        return Err(format!(
            "a baseline-less incremental request must defer to a completed full snapshot, got {outcome:?}"
        )
        .into());
    };
    assert_eq!(
        pages_processed, 2,
        "the full enumeration includes its terminator"
    );

    let run_modes: Vec<String> =
        sqlx::query_scalar("select mode from github_catalog.sync_runs order by mode")
            .fetch_all(database.database.pool())
            .await?
            .into_iter()
            .collect();
    assert_eq!(
        run_modes,
        ["full"],
        "deferral must not open an incremental run row at all"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn incremental_scan_never_touches_repositories_outside_its_window()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;

    // X is stale authority below the window; Y is a continuing star whose
    // established timestamp predates the fresh value on the wire.
    let x_repository = seed_prior_star(
        &database.database,
        account_id,
        300_000_030,
        "2025-12-01T00:00:00Z",
    )
    .await?;
    let y_repository = seed_prior_star(
        &database.database,
        account_id,
        300_000_031,
        "2026-05-15T00:00:00Z",
    )
    .await?;
    let server = MockServer::start().await;

    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_031, "acme/continuing", "2026-07-01T00:00:00Z"),
            starred_item(300_000_032, "acme/newcomer", "2026-06-20T00:00:00Z")
        ),
    )
    .await?;
    mount_newest_first_page(&server, 2, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("incremental-e2e"),
        account_id,
    )
    .await?;
    assert!(
        matches!(outcome, IncrementalScanOutcome::Completed { .. }),
        "the scan must complete over the windowed fixtures, got {outcome:?}"
    );

    // X was never listed: it must remain exactly as prior authority left it.
    let x_row: (bool, Option<String>, Option<Uuid>) = sqlx::query_as(
        "select starred, starred_at::text, evidence_run_id
             from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(x_repository)
    .fetch_one(database.database.pool())
    .await?;
    let (x_starred, x_starred_at, x_evidence) = x_row;
    assert!(x_starred, "an unlisted repository must stay starred");
    assert_eq!(
        x_starred_at.as_deref(),
        Some("2025-12-01 00:00:00+00"),
        "an unlisted repository keeps its established timestamp"
    );
    assert!(
        x_evidence.is_none(),
        "an unlisted repository gains no new evidence"
    );

    // No removal was inferred anywhere: no unstarred projection exists.
    let unstarred_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.current_star_state
         where account_id = $1 and not starred",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        unstarred_count, 0,
        "an incremental pass must never establish an unstar"
    );

    // The continuing star keeps its established timestamp despite the wire.
    let y_row: (bool, Option<String>) = sqlx::query_as(
        "select starred, starred_at::text
             from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(y_repository)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        y_row,
        (true, Some("2026-05-15 00:00:00+00".to_owned())),
        "a continuing star keeps the earliest established provider timestamp"
    );

    // The newcomer became starred with its own provider timestamp.
    let newcomer_row: Option<String> = sqlx::query_scalar(
        "select c.starred_at::text from github_catalog.current_star_state c
         join github_catalog.repositories r on r.repository_id = c.repository_id
         where r.provider_repository_id = 300_000_032",
    )
    .fetch_optional(database.database.pool())
    .await?
    .flatten();
    assert_eq!(
        newcomer_row.as_deref(),
        Some("2026-06-20 00:00:00+00"),
        "a newcomer takes its establishing provider timestamp"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn missing_provider_starred_at_fails_run_as_gap_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let stale_repository = seed_prior_star(
        &database.database,
        account_id,
        300_000_040,
        "2025-12-01T00:00:00Z",
    )
    .await?;
    let server = MockServer::start().await;

    // The second entry carries no timestamp, so the page's ordering - and
    // therefore the window's coverage - cannot be proven.
    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_041, "acme/orderable", "2026-06-01T00:00:00Z"),
            starred_item_without_timestamp(300_000_042, "acme/unorderable")
        ),
    )
    .await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("incremental-e2e"),
        account_id,
    )
    .await?;

    let IncrementalScanOutcome::GapDetected { run_id } = outcome else {
        return Err(format!("the missing timestamp must abort as a gap, got {outcome:?}").into());
    };

    let row: (String, Option<String>) = sqlx::query_as(
        "select status, failure_reason from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(row.0, "failed", "a gap terminates its run as failed");
    let reason = row.1.unwrap_or_default();
    assert!(
        reason.starts_with("starred_at ordering gap detected"),
        "the failure must name the ordering gap, got {reason:?}"
    );

    // Nothing from the offending page was ingested.
    let ingested_ids: Vec<i64> =
        sqlx::query_scalar("select provider_repository_id from github_catalog.repositories")
            .fetch_all(database.database.pool())
            .await?
            .into_iter()
            .collect();
    assert_eq!(
        ingested_ids,
        [300_000_040],
        "only the seeded prior authority's repository may exist"
    );

    // The watermark stands still and no checkpoint claims progress.
    let watermark_still: bool = sqlx::query_scalar(
        "select high_water_mark = $2::timestamptz
             from github_catalog.star_watermarks where account_id = $1",
    )
    .bind(account_id)
    .bind("2026-05-01T00:00:00Z")
    .fetch_one(database.database.pool())
    .await?;
    assert!(watermark_still, "a gap must not move the watermark");
    let checkpoint_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.sync_checkpoints where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        checkpoint_count, 0,
        "an aborted first page leaves no checkpoint"
    );

    // Prior authority is untouched.
    let stale_row: (bool,) = sqlx::query_as(
        "select starred from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(stale_repository)
    .fetch_one(database.database.pool())
    .await?;
    assert!(stale_row.0, "prior authority survives a gap");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn out_of_order_resume_boundary_detects_gap() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let server = MockServer::start().await;

    // Page one lands, then the shared budget parks the run before page two:
    // the page reports zero remaining, and acquisition stops at the floor.
    mount_newest_first_page_with_remaining(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_050, "acme/first", "2026-07-03T00:00:00Z"),
            starred_item(300_000_051, "acme/second", "2026-06-02T00:00:00Z")
        ),
        "0",
    )
    .await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let token = TokenRef::from_label("incremental-e2e");
    let paused = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    let IncrementalScanOutcome::Paused { run_id, .. } = paused else {
        return Err(format!("the budget refusal must pause the scan, got {paused:?}").into());
    };

    // The resumed pass serves a page whose leading item is newer than page
    // one's oldest boundary (2026-06-02) - an impossible continuation.
    mount_newest_first_page_with_remaining(
        &server,
        2,
        format!(
            "[{}]",
            starred_item(300_000_052, "acme/impossible", "2026-06-10T00:00:00Z")
        ),
        "4998",
    )
    .await?;

    let resumed = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    let IncrementalScanOutcome::GapDetected { run_id: gap_run } = resumed else {
        return Err(format!("the boundary violation must abort as a gap, got {resumed:?}").into());
    };
    assert_eq!(
        gap_run, run_id,
        "the resumed pass continues the same run, not a new one"
    );

    let reason: Option<String> = sqlx::query_scalar(
        "select failure_reason from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    let reason = reason.unwrap_or_default();
    assert!(
        reason.starts_with("starred_at ordering gap detected"),
        "the boundary violation must be recorded as the gap reason, got {reason:?}"
    );

    // The offending page was never ingested.
    let provider_ids: Vec<i64> =
        sqlx::query_scalar("select provider_repository_id from github_catalog.repositories order by provider_repository_id")
            .fetch_all(database.database.pool())
            .await?
            .into_iter()
            .collect();
    assert_eq!(
        provider_ids,
        [300_000_050, 300_000_051],
        "only page one's items may exist under identity"
    );

    // The watermark still stands at its seeded value.
    let watermark_still: bool = sqlx::query_scalar(
        "select high_water_mark = $2::timestamptz
             from github_catalog.star_watermarks where account_id = $1",
    )
    .bind(account_id)
    .bind("2026-05-01T00:00:00Z")
    .fetch_one(database.database.pool())
    .await?;
    assert!(watermark_still, "a resumed gap must not move the watermark");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn completed_full_snapshot_sets_watermark_to_newest_observed()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let server = MockServer::start().await;

    mount_unordered_page(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_060, "acme/older", "2026-03-01T00:00:00Z"),
            starred_item(300_000_061, "acme/newer", "2026-04-04T00:00:00Z")
        ),
    )
    .await?;
    mount_unordered_page(&server, 2, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = ratatoskr_github_catalog::run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("incremental-e2e"),
        account_id,
    )
    .await?;
    assert!(
        matches!(outcome, FullSnapshotOutcome::Completed { .. }),
        "the snapshot must complete, got {outcome:?}"
    );

    // The completed enumeration re-anchors the incremental baseline at the
    // newest starred-at it observed.
    let anchored: bool = sqlx::query_scalar(
        "select high_water_mark = $2::timestamptz
             from github_catalog.star_watermarks where account_id = $1",
    )
    .bind(account_id)
    .bind("2026-04-04T00:00:00Z")
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        anchored,
        "a completed snapshot must set the watermark to its newest observation"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn empty_completed_snapshot_leaves_the_watermark_unset()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let server = MockServer::start().await;

    mount_unordered_page(&server, 1, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = ratatoskr_github_catalog::run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("incremental-e2e"),
        account_id,
    )
    .await?;
    assert!(
        matches!(outcome, FullSnapshotOutcome::Completed { .. }),
        "the empty enumeration must complete, got {outcome:?}"
    );

    // Nothing was observed, so nothing invents a mark.
    let watermark_still: bool = sqlx::query_scalar(
        "select high_water_mark = $2::timestamptz
             from github_catalog.star_watermarks where account_id = $1",
    )
    .bind(account_id)
    .bind("2026-05-01T00:00:00Z")
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        watermark_still,
        "an empty enumeration must leave the existing watermark alone"
    );
    let watermark_count: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.star_watermarks")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(watermark_count, 1, "no second mark may appear");

    database.cleanup().await?;
    Ok(())
}
