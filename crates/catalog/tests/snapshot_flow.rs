//! End-to-end full star snapshot: wiremock provider plus disposable catalog
//! database, exercising enumeration, checkpoints, authority swap, and run
//! accounting.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{FullSnapshotOutcome, run_full_snapshot};
use uuid::Uuid;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Seeds one connected account row and returns its id.
async fn seed_account(
    database: &ratatoskr_github_catalog::Database,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'tester', 'connected')",
    )
    .bind(account_id)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

/// Seeds one starred authority row for the account over a fresh repository.
async fn seed_prior_star(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    provider_repository_id: i64,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let identity =
        ratatoskr_github_catalog::upsert_repository(database, provider_repository_id).await?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, $3::timestamptz, now())",
    )
    .bind(account_id)
    .bind(identity.repository_id)
    .bind("2025-12-01T00:00:00Z")
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

async fn mount_page(
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

/// Mounts one page served at most once with a chosen `x-ratelimit-remaining`
/// value; the cap is what makes an unexpected refetch fail loudly instead of
/// silently replaying the page.
async fn mount_page_with_remaining(
    server: &MockServer,
    page: u32,
    body: String,
    remaining: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", page.to_string()))
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

#[tokio::test]
async fn full_snapshot_enumerates_all_pages_records_completed_run_and_statistics()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let server = MockServer::start().await;

    mount_page(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z"),
            starred_item(300_000_002, "acme/beta", "2026-02-02T00:00:00Z")
        ),
    )
    .await?;
    mount_page(
        &server,
        2,
        format!(
            "[{}]",
            starred_item(300_000_003, "acme/gamma", "2026-03-03T00:00:00Z")
        ),
    )
    .await?;
    mount_page(&server, 3, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("snapshot-e2e");

    let outcome = ratatoskr_github_catalog::run_full_snapshot(
        &database.database,
        &gateway,
        &ledger,
        &token,
        account_id,
    )
    .await?;
    let ratatoskr_github_catalog::FullSnapshotOutcome::Completed {
        run_id,
        pages_processed,
        items_observed,
        ..
    } = outcome
    else {
        return Err(format!("the first full snapshot must complete, got {outcome:?}").into());
    };
    assert_eq!(
        pages_processed, 3,
        "two item pages plus the empty terminator"
    );
    assert_eq!(items_observed, 3);

    // Every listed repository exists under stable numeric identity.
    let provider_ids: Vec<i64> = sqlx::query_scalar(
        "select provider_repository_id from github_catalog.repositories
         order by provider_repository_id",
    )
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(provider_ids, [300_000_001, 300_000_002, 300_000_003]);

    // Exactly one completed run with truthful statistics.
    let row: (String, String, bool, i32, i32) = sqlx::query_as(
        "select status, mode, finished_at is not null, pages_processed, items_observed
             from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    let (status, mode, finished, pages, items) = row;
    assert_eq!((status.as_str(), mode.as_str()), ("completed", "full"));
    assert!(finished, "a completed run must carry its finish time");
    assert_eq!((pages, items), (3, 3));

    // One checkpoint per durably processed page, pointing at the next page.
    let next_pages: Vec<i64> = sqlx::query_scalar(
        "select next_page from github_catalog.sync_checkpoints
         where sync_run_id = $1 order by recorded_at, next_page",
    )
    .bind(run_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(next_pages, [2, 3, 4]);

    server.verify().await;
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn budget_refusal_pauses_run_without_touching_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let keeper = seed_prior_star(&database.database, account_id, 300_000_999).await?;
    let server = MockServer::start().await;

    // The first page reports an exhausted budget; acquiring the second page
    // must therefore be refused.
    mount_page_with_remaining(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z")
        ),
        "0",
    )
    .await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("pause-e2e");

    let outcome =
        run_full_snapshot(&database.database, &gateway, &ledger, &token, account_id).await?;
    let FullSnapshotOutcome::Paused { run_id, retry_at } = outcome else {
        return Err(format!("a refused acquisition must pause the run, got {outcome:?}").into());
    };
    assert!(
        retry_at > std::time::SystemTime::now(),
        "the pause must name a future retry time"
    );

    let row: (String, bool) = sqlx::query_as(
        "select status, finished_at is not null from github_catalog.sync_runs
         where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        row,
        ("running".to_owned(), false),
        "a paused run must stay open with no finish time"
    );

    let next_pages: Vec<i64> = sqlx::query_scalar(
        "select next_page from github_catalog.sync_checkpoints where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        next_pages,
        [2],
        "the processed page must have its checkpoint"
    );

    let authority: Vec<(Uuid, bool)> = sqlx::query_as(
        "select repository_id, starred from github_catalog.current_star_state
         where account_id = $1",
    )
    .bind(account_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        authority,
        [(keeper, true)],
        "a paused scan must not touch star authority"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn interrupted_scan_resumes_from_checkpoint_without_refetching_completed_pages()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let server = MockServer::start().await;

    // Page one exhausts the reported budget, forcing a pause before page two.
    mount_page_with_remaining(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z"),
            starred_item(300_000_002, "acme/beta", "2026-02-02T00:00:00Z")
        ),
        "0",
    )
    .await?;
    mount_page(
        &server,
        2,
        format!(
            "[{}]",
            starred_item(300_000_003, "acme/gamma", "2026-03-03T00:00:00Z")
        ),
    )
    .await?;
    mount_page(&server, 3, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let token = TokenRef::from_label("resume-e2e");

    let paused = run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    let FullSnapshotOutcome::Paused { run_id, .. } = paused else {
        return Err(format!("the seeded budget must pause the first call, got {paused:?}").into());
    };

    // A fresh ledger stands in for time passing or a new process.
    let resumed = run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    let FullSnapshotOutcome::Completed {
        run_id: resumed_run_id,
        pages_processed,
        items_observed,
        ..
    } = resumed
    else {
        return Err(format!("the resumed run must complete, got {resumed:?}").into());
    };
    assert_eq!(
        resumed_run_id, run_id,
        "resume must continue the same run, not start another"
    );
    assert_eq!(
        (pages_processed, items_observed),
        (3, 3),
        "statistics must accumulate across the interruption"
    );

    let requested_pages: Vec<String> = server
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
        .collect();
    assert_eq!(
        requested_pages,
        ["1", "2", "3"],
        "completed pages must never be fetched twice across the interruption"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn authority_swaps_atomically_and_readers_never_see_partial_snapshots()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let keeper = seed_prior_star(&database.database, account_id, 300_000_999).await?;
    let server = MockServer::start().await;

    // Page one exhausts the reported budget so the scan pauses mid-flight;
    // the listing ultimately adds alpha and beta and drops keeper.
    mount_page_with_remaining(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z")
        ),
        "0",
    )
    .await?;
    mount_page(
        &server,
        2,
        format!(
            "[{}]",
            starred_item(300_000_002, "acme/beta", "2026-02-02T00:00:00Z")
        ),
    )
    .await?;
    mount_page(&server, 3, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let token = TokenRef::from_label("swap-e2e");

    let paused = run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    assert!(
        matches!(paused, FullSnapshotOutcome::Paused { .. }),
        "the seeded budget must pause the first call"
    );

    let mid_flight: Vec<(Uuid, bool)> = sqlx::query_as(
        "select repository_id, starred from github_catalog.current_star_state
         where account_id = $1",
    )
    .bind(account_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        mid_flight,
        [(keeper, true)],
        "while pages are still arriving readers must see only the prior authority"
    );

    let resumed = run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    let FullSnapshotOutcome::Completed { run_id, .. } = resumed else {
        return Err(format!("the resumed run must complete, got {resumed:?}").into());
    };

    // One consistent post-swap state: both additions and the removal appear
    // together, and every row cites the completing run as its evidence.
    let swapped: Vec<(Uuid, bool, Option<Uuid>)> = sqlx::query_as(
        "select s.repository_id, s.starred, s.evidence_run_id
         from github_catalog.current_star_state s
         join github_catalog.repositories r on r.repository_id = s.repository_id
         where s.account_id = $1
         order by r.provider_repository_id",
    )
    .bind(account_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    let alpha = ratatoskr_github_catalog::resolve_alias(
        &database.database,
        ratatoskr_github_catalog::AliasKind::OwnerName,
        "acme/alpha",
    )
    .await?;
    let beta = ratatoskr_github_catalog::resolve_alias(
        &database.database,
        ratatoskr_github_catalog::AliasKind::OwnerName,
        "acme/beta",
    )
    .await?;
    assert_eq!(
        swapped,
        [
            (alpha.expect("alpha must be known"), true, Some(run_id)),
            (beta.expect("beta must be known"), true, Some(run_id)),
            (keeper, false, Some(run_id)),
        ],
        "the completed snapshot must replace authority wholesale"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn absent_repositories_become_evidenced_unstar_observations()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let keeper = seed_prior_star(&database.database, account_id, 300_000_999).await?;
    let server = MockServer::start().await;

    mount_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z")
        ),
    )
    .await?;
    mount_page(&server, 2, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("unstar-e2e"),
        account_id,
    )
    .await?;
    let FullSnapshotOutcome::Completed {
        run_id, unstars, ..
    } = outcome
    else {
        return Err(format!("the snapshot must complete, got {outcome:?}").into());
    };
    assert_eq!(
        unstars, 1,
        "exactly one prior star is absent from the listing"
    );

    let keeper_row: (bool, Option<Uuid>) = sqlx::query_as(
        "select s.starred, s.evidence_run_id
         from github_catalog.current_star_state s
         where s.account_id = $1 and s.repository_id = $2",
    )
    .bind(account_id)
    .bind(keeper)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        keeper_row,
        (false, Some(run_id)),
        "an absent repository must be marked unstarred with the establishing run as evidence"
    );
    let unstar_time_recorded: bool = sqlx::query_scalar(
        "select observed_unstarred_at is not null from github_catalog.current_star_state
         where account_id = $1 and repository_id = $2",
    )
    .bind(account_id)
    .bind(keeper)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        unstar_time_recorded,
        "the unstar must carry its observation time"
    );

    // Append-only evidence: exactly one unstar observation row, nothing deleted.
    let observation_rows: Vec<(bool,)> = sqlx::query_as(
        "select o.starred from github_catalog.star_observations o
         where o.account_id = $1 and o.repository_id = $2",
    )
    .bind(account_id)
    .bind(keeper)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        observation_rows,
        [(false,)],
        "the unstar must exist as append-only observation evidence"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn continuing_stars_keep_their_established_starred_at()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_prior_star(&database.database, account_id, 300_000_999).await?;

    let listing =
        |starred_at: &str| format!("[{}]", starred_item(300_000_999, "acme/keeper", starred_at));
    let empty = "[]".to_owned();

    let first_server = MockServer::start().await;
    mount_page(&first_server, 1, listing("2026-04-01T00:00:00Z")).await?;
    mount_page(&first_server, 2, empty.clone()).await?;
    let first_gateway = ReqwestGithubApi::for_base_url(&first_server.uri())?;
    let first = run_full_snapshot(
        &database.database,
        &first_gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("continuity-e2e"),
        account_id,
    )
    .await?;
    assert!(
        matches!(first, FullSnapshotOutcome::Completed { .. }),
        "the first snapshot must complete"
    );

    let second_server = MockServer::start().await;
    mount_page(&second_server, 1, listing("2026-07-01T00:00:00Z")).await?;
    mount_page(&second_server, 2, empty).await?;
    let second_gateway = ReqwestGithubApi::for_base_url(&second_server.uri())?;
    let second = run_full_snapshot(
        &database.database,
        &second_gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("continuity-e2e"),
        account_id,
    )
    .await?;
    assert!(
        matches!(second, FullSnapshotOutcome::Completed { .. }),
        "the second snapshot must complete"
    );

    let kept_first_value: bool = sqlx::query_scalar(
        "select s.starred_at = '2025-12-01T00:00:00Z'::timestamptz
         from github_catalog.current_star_state s
         join github_catalog.repositories r on r.repository_id = s.repository_id
         where s.account_id = $1 and r.provider_repository_id = 300000999",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        kept_first_value,
        "confirmations must preserve the earliest established starred-at"
    );

    let observed_times: Vec<(bool, bool)> = sqlx::query_as(
        "select o.provider_starred_at = '2026-04-01T00:00:00Z'::timestamptz,
                o.provider_starred_at = '2026-07-01T00:00:00Z'::timestamptz
         from github_catalog.star_observations o
         join github_catalog.repositories r on r.repository_id = o.repository_id
         where o.account_id = $1 and r.provider_repository_id = 300000999 and o.starred",
    )
    .bind(account_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        observed_times.len(),
        2,
        "each snapshot must leave its own observation fact"
    );
    assert!(
        observed_times.iter().any(|(first, _)| *first)
            && observed_times.iter().any(|(_, second)| *second),
        "observations must keep every reported starred-at fact"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn mid_run_provider_failure_preserves_prior_authority_and_records_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let keeper = seed_prior_star(&database.database, account_id, 300_000_999).await?;
    let server = MockServer::start().await;

    mount_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z")
        ),
    )
    .await?;
    // Page two answers a permanent provider failure.
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_full_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("failure-e2e"),
        account_id,
    )
    .await?;
    let FullSnapshotOutcome::Failed { run_id } = outcome else {
        return Err(
            format!("a permanent provider failure must fail the run, got {outcome:?}").into(),
        );
    };

    let row: (String, bool, Option<String>) = sqlx::query_as(
        "select status, finished_at is not null, failure_reason
         from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    let (status, finished, reason) = row;
    assert_eq!(status, "failed", "the run must terminate as failed");
    assert!(finished, "a failed run must carry its finish time");
    assert!(
        reason.is_some_and(|value| !value.is_empty()),
        "a failed run must name its failure"
    );

    // Prior authority untouched: no additions, no unstars, no observation rows.
    let authority: Vec<(Uuid, bool)> = sqlx::query_as(
        "select repository_id, starred from github_catalog.current_star_state
         where account_id = $1",
    )
    .bind(account_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        authority,
        [(keeper, true)],
        "a failed scan must leave star authority exactly as it was"
    );
    let observation_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_observations where account_id = $1",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        observation_count, 0,
        "no removal may be recorded from a dead run"
    );

    // The dead run's staging is cleared; it must not leak into later runs.
    let staging_left: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.snapshot_items where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(staging_left, 0, "failed-run staging must be cleared");

    database.cleanup().await?;
    Ok(())
}
