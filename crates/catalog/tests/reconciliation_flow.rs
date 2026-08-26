//! End-to-end reconciliation: wiremock provider plus disposable catalog
//! database, exercising drift detection between prior state and a completed
//! full enumeration, the explicit recording of repairs, and their
//! idempotence when reconciliation repeats on converged state.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{FullSnapshotOutcome, run_full_snapshot, upsert_repository};
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

/// Seeds one starred authority row with an established timestamp.
async fn seed_starred(
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

/// Seeds one unstarred authority row carrying its removal evidence.
async fn seed_unstarred(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    provider_repository_id: i64,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let identity = upsert_repository(database, provider_repository_id).await?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at,
              last_observed_at, observed_unstarred_at)
         values ($1, $2, false, null, now(), $3::timestamptz)",
    )
    .bind(account_id)
    .bind(identity.repository_id)
    .bind("2026-01-05T00:00:00Z")
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

async fn mount_listing(
    server: &MockServer,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "1"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4999")
                .insert_header("x-ratelimit-reset", "1787000000"),
        )
        .up_to_n_times(2)
        .mount(server)
        .await;
    Ok(())
}

async fn mount_exhaustion(server: &MockServer) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "2"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .up_to_n_times(2)
        .mount(server)
        .await;
    Ok(())
}

/// The drift fixture: A starred but about to be absent, B unstarred but
/// listed again, C starred and steady. Serves the same listing for two
/// consecutive snapshots and returns everything a test needs.
struct DriftFixture {
    database: TestDatabase,
    gateway: ReqwestGithubApi,
    token: TokenRef,
    account_id: Uuid,
    drifted_out: Uuid,
    missed_return: Uuid,
    steady: Uuid,
}

async fn seed_drift_fixture() -> Result<DriftFixture, Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let drifted_out = seed_starred(
        &database.database,
        account_id,
        300_000_070,
        "2026-01-10T00:00:00Z",
    )
    .await?;
    let missed_return = seed_unstarred(&database.database, account_id, 300_000_071).await?;
    let steady = seed_starred(
        &database.database,
        account_id,
        300_000_072,
        "2026-02-02T00:00:00Z",
    )
    .await?;
    let server = MockServer::start().await;

    let listing = format!(
        "[{}, {}]",
        starred_item(300_000_071, "acme/returned", "2026-06-05T00:00:00Z"),
        starred_item(300_000_072, "acme/steady", "2026-02-02T00:00:00Z")
    );
    mount_listing(&server, listing).await?;
    mount_exhaustion(&server).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let token = TokenRef::from_label("reconciliation-e2e");
    Ok(DriftFixture {
        database,
        gateway,
        token,
        account_id,
        drifted_out,
        missed_return,
        steady,
    })
}

/// Reads every repair row bound to a run as (provider id, action) pairs.
async fn repairs_for(
    database: &ratatoskr_github_catalog::Database,
    run_id: Uuid,
) -> Result<Vec<(i64, String)>, Box<dyn std::error::Error>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "select r.provider_repository_id, rr.action
             from github_catalog.reconciliation_repairs rr
             join github_catalog.repositories r on r.repository_id = rr.repository_id
             where rr.sync_run_id = $1
             order by r.provider_repository_id",
    )
    .bind(run_id)
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .collect();
    Ok(rows)
}

/// Dumps current star state keyed by provider id, in stable order.
async fn state_dump(
    database: &ratatoskr_github_catalog::Database,
) -> Result<Vec<(i64, bool, Option<String>)>, Box<dyn std::error::Error>> {
    let rows: Vec<(i64, bool, Option<String>)> = sqlx::query_as(
        "select r.provider_repository_id, c.starred, c.starred_at::text
             from github_catalog.current_star_state c
             join github_catalog.repositories r on r.repository_id = c.repository_id
             order by r.provider_repository_id",
    )
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .collect();
    Ok(rows)
}

#[tokio::test]
async fn completed_snapshot_records_drift_repairs_exactly_once()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = seed_drift_fixture().await?;

    let first = run_full_snapshot(
        &fixture.database.database,
        &fixture.gateway,
        &RateLimitLedger::new(),
        &fixture.token,
        fixture.account_id,
    )
    .await?;
    let FullSnapshotOutcome::Completed {
        run_id,
        additions,
        unstars,
        ..
    } = first
    else {
        return Err(format!("the first snapshot must complete, got {first:?}").into());
    };
    assert_eq!(
        (additions, unstars),
        (1, 1),
        "one restored star and one drifted-out star"
    );

    // Exactly the two drifted repositories carry named repairs bound to the
    // completing run.
    assert_eq!(
        repairs_for(&fixture.database.database, run_id).await?,
        [
            (300_000_070, "unstar_after_drift".to_owned()),
            (300_000_071, "restore_after_miss".to_owned())
        ],
        "exactly the drifted repositories must carry named repair rows"
    );

    // The drift row for A is the normal evidenced unstar.
    let absent_row: (bool, Option<String>, Option<Uuid>) = sqlx::query_as(
        "select starred, observed_unstarred_at::text, evidence_run_id
             from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(fixture.drifted_out)
    .fetch_one(fixture.database.database.pool())
    .await?;
    assert!(!absent_row.0, "the drifted-away star is unstarred");
    assert!(
        absent_row.1.is_some(),
        "the unstar carries its observation time"
    );
    assert_eq!(
        absent_row.2,
        Some(run_id),
        "the completing run is the evidence"
    );

    // B's return restored star state under the fresh provider timestamp.
    let returned_row: (bool, Option<String>, Option<String>) = sqlx::query_as(
        "select starred, starred_at::text, observed_unstarred_at::text
             from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(fixture.missed_return)
    .fetch_one(fixture.database.database.pool())
    .await?;
    assert_eq!(
        returned_row,
        (true, Some("2026-06-05 00:00:00+00".to_owned()), None),
        "a restored star takes the fresh timestamp without stale removal evidence"
    );

    // C saw no drift and keeps its established timestamp.
    let steady_row: Option<String> = sqlx::query_scalar(
        "select starred_at::text from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(fixture.steady)
    .fetch_one(fixture.database.database.pool())
    .await?;
    assert_eq!(
        steady_row.as_deref(),
        Some("2026-02-02 00:00:00+00"),
        "an undrifted continuation keeps its established timestamp"
    );

    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn repeated_reconciliation_on_converged_state_writes_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = seed_drift_fixture().await?;
    let run = || async {
        run_full_snapshot(
            &fixture.database.database,
            &fixture.gateway,
            &RateLimitLedger::new(),
            &fixture.token,
            fixture.account_id,
        )
        .await
    };
    assert!(
        matches!(run().await?, FullSnapshotOutcome::Completed { .. }),
        "the first reconciliation must complete"
    );

    // Reconciling again over identical upstream writes nothing new and
    // changes nothing: repairs are idempotent.
    let state_before = state_dump(&fixture.database.database).await?;
    let second = run().await?;
    let FullSnapshotOutcome::Completed {
        run_id: second_run,
        additions,
        unstars,
        ..
    } = second
    else {
        return Err(format!("the second snapshot must complete, got {second:?}").into());
    };
    assert_eq!(
        (additions, unstars),
        (0, 0),
        "converged state admits no transitions"
    );

    let second_repair_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.reconciliation_repairs where sync_run_id = $1",
    )
    .bind(second_run)
    .fetch_one(fixture.database.database.pool())
    .await?;
    assert_eq!(
        second_repair_count, 0,
        "reconciling converged state must record no repairs"
    );
    let total_repairs: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.reconciliation_repairs")
            .fetch_one(fixture.database.database.pool())
            .await?;
    assert_eq!(
        total_repairs, 2,
        "no duplicate repair may appear for the same drift"
    );

    assert_eq!(
        state_before,
        state_dump(&fixture.database.database).await?,
        "repeated reconciliation leaves current star state byte-identical"
    );

    fixture.database.cleanup().await?;
    Ok(())
}

/// The completed authority swap honors explicit classifications: tracked and
/// ignored entries survive untouched, and an evidenced unstar releases auto
/// governance back to unclassified while tracked intent persists.
#[tokio::test]
async fn snapshot_authority_respects_tracked_and_ignored_and_releases_auto_on_evidenced_unstar()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = seed_drift_fixture().await?;
    // drifted_out: auto (must be released when its star is evidenced away).
    sqlx::query("update github_catalog.repositories set mode = 'auto' where repository_id = $1")
        .bind(fixture.drifted_out)
        .execute(fixture.database.database.pool())
        .await?;
    // steady: tracked (a star continuation must never promote or demote it).
    sqlx::query("update github_catalog.repositories set mode = 'tracked' where repository_id = $1")
        .bind(fixture.steady)
        .execute(fixture.database.database.pool())
        .await?;
    // missed_return: ignored (its restored star evidence must not override).
    sqlx::query("update github_catalog.repositories set mode = 'ignored' where repository_id = $1")
        .bind(fixture.missed_return)
        .execute(fixture.database.database.pool())
        .await?;

    let completed = run_full_snapshot(
        &fixture.database.database,
        &fixture.gateway,
        &RateLimitLedger::new(),
        &fixture.token,
        fixture.account_id,
    )
    .await?;
    let FullSnapshotOutcome::Completed { .. } = completed else {
        return Err(format!("the snapshot must complete, got {completed:?}").into());
    };

    let modes: Vec<(i64, Option<String>)> = sqlx::query_as(
        "select provider_repository_id, mode from github_catalog.repositories
         order by provider_repository_id",
    )
    .fetch_all(fixture.database.database.pool())
    .await?;
    assert_eq!(
        modes,
        [
            (300_000_070, None),
            (300_000_071, Some("ignored".to_owned())),
            (300_000_072, Some("tracked".to_owned())),
        ],
        "evidenced unstar releases auto; explicit classifications survive the swap"
    );

    fixture.database.cleanup().await?;
    Ok(())
}
