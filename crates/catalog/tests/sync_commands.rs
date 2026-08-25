//! End-to-end sync-command consumption: the platform scheduler command
//! envelope validated, claimed durably through the inbox, dispatched to the
//! scan flows, and escalated to a forced full rescan on ordering gaps.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    ConsumedSyncCommand, FullSnapshotOutcome, IncrementalScanOutcome, RequestedSyncMode,
    StarListSnapshotOutcome, handle_sync_command,
};
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// Builds one platform scheduler command envelope for `github.sync.requested.v1`.
fn envelope(command_id: Uuid, account: &str, mode: Option<&str>) -> String {
    let mode_part = mode
        .map(|text| format!(r#", "mode": "{text}""#))
        .unwrap_or_default();
    format!(
        r#"{{
            "command_id": "{command_id}",
            "command_type": "github.sync.requested.v1",
            "requested_at": "2026-08-25T00:00:00Z",
            "operation_id": "{}",
            "tenant_id": "user:{}",
            "correlation_id": "sched/github-sync/occurrence",
            "idempotency_key": "{command_id}",
            "payload": {{ "account": "{account}"{mode_part} }}
        }}"#,
        Uuid::now_v7(),
        Uuid::now_v7()
    )
}

/// Seeds one connected account row with an explicit owner reference.
async fn seed_account(
    database: &ratatoskr_github_catalog::Database,
    owner_ref: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'connected')",
    )
    .bind(account_id)
    .bind(owner_ref)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

/// Seeds the account's persisted high-water mark so commanded incremental
/// scans have a baseline to bound their window.
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

/// A listing entry whose provider timestamp is absent.
fn starred_item_without_timestamp(id: i64, name: &str) -> String {
    starred_item(id, name, "").replace(r#""starred_at": """#, r#""starred_at": null"#)
}

/// Matches requests that do not carry a named query parameter - the shape
/// of the unordered listing calls a full snapshot makes.
#[derive(Debug)]
struct WithoutQueryParam {
    key: &'static str,
}

impl Match for WithoutQueryParam {
    fn matches(&self, request: &Request) -> bool {
        !request.url.query_pairs().any(|(key, _)| key == self.key)
    }
}

/// Mounts the newest-first listing page a commanded incremental scan fetches.
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
        .up_to_n_times(1)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts one unordered listing page - the wire shape a full snapshot uses.
async fn mount_unordered_page(
    server: &MockServer,
    page: u32,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", page.to_string()))
        .and(WithoutQueryParam { key: "sort" })
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts one GraphQL star-list enumeration page matched by continuation.
async fn mount_graphql_page(
    server: &MockServer,
    carries_cursor: bool,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let continuation = if carries_cursor {
        r#""after":"MQ""#
    } else {
        r#""after":null"#
    };
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(continuation))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .up_to_n_times(2)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts the empty GraphQL enumeration the chained star-list snapshot
/// fetches when a test's focus is the star-mode flow.
async fn mount_empty_graphql(server: &MockServer) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(r#""after":null"#))
        .respond_with(ResponseTemplate::new(200).set_body_string(lists_page(&[], None)))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Ok(())
}

fn list_edge(gid: &str, name: &str, database_id: i64, name_with_owner: &str) -> serde_json::Value {
    json!({
        "node": {
            "id": gid,
            "name": name,
            "items": {
                "pageInfo": {"hasNextPage": false},
                "edges": [
                    {"node": {"__typename": "Repository", "databaseId": database_id, "nameWithOwner": name_with_owner}}
                ]
            }
        }
    })
}

fn lists_page(edges: &[serde_json::Value], end_cursor: Option<&str>) -> String {
    json!({
        "data": {
            "viewer": {
                "lists": {
                    "pageInfo": {"hasNextPage": true, "endCursor": end_cursor},
                    "edges": edges,
                }
            },
            "rateLimit": {"cost": 1, "remaining": 4998, "resetAt": "2026-08-25T22:00:00Z"},
        }
    })
    .to_string()
}

#[tokio::test]
async fn commanded_full_sync_chains_independent_list_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database, "tester").await?;
    let server = MockServer::start().await;

    mount_unordered_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_085, "acme/commanded-full", "2026-06-20T00:00:00Z")
        ),
    )
    .await?;
    mount_unordered_page(&server, 2, "[]".to_owned()).await?;
    mount_graphql_page(
        &server,
        false,
        lists_page(
            &[list_edge(
                "gid://UserList/7",
                "commanded list",
                300_000_085,
                "acme/commanded-full",
            )],
            Some("MQ"),
        ),
    )
    .await?;
    mount_graphql_page(&server, true, lists_page(&[], None)).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let envelope_json = envelope(Uuid::now_v7(), "tester", Some("full"));

    let consumed = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-list-e2e"),
        &envelope_json,
    )
    .await?;
    let ConsumedSyncCommand::Handled(handled) = consumed else {
        return Err(format!("a fresh command must be handled, got {consumed:?}").into());
    };

    assert!(
        matches!(handled.full, Some(FullSnapshotOutcome::Completed { .. })),
        "the commanded full star snapshot must complete"
    );
    assert!(
        matches!(
            handled.star_lists,
            Some(StarListSnapshotOutcome::Completed { .. })
        ),
        "the chained list snapshot must complete too, got {:?}",
        handled.star_lists
    );

    // Two peer runs recorded for the same account: the star mode and the
    // independent list mode.
    let modes: Vec<(String, String)> = sqlx::query_as(
        "select mode, status from github_catalog.sync_runs where account_id = $1 order by mode",
    )
    .bind(account_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(
        modes,
        vec![
            ("full".to_owned(), "completed".to_owned()),
            ("star_lists".to_owned(), "completed".to_owned()),
        ],
        "one command yields one star-mode run and one independent star_lists run"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn list_failure_never_invalidates_star_outcome() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    seed_account(&database.database, "tester").await?;
    let server = MockServer::start().await;

    // The star listing works; every GraphQL call fails.
    mount_unordered_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_086, "acme/star-wins", "2026-06-21T00:00:00Z")
        ),
    )
    .await?;
    mount_unordered_page(&server, 2, "[]".to_owned()).await?;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(4)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let envelope_json = envelope(Uuid::now_v7(), "tester", Some("full"));

    let consumed = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-independence-e2e"),
        &envelope_json,
    )
    .await?;
    let ConsumedSyncCommand::Handled(handled) = consumed else {
        return Err(format!("a fresh command must be handled, got {consumed:?}").into());
    };

    assert!(
        matches!(handled.full, Some(FullSnapshotOutcome::Completed { .. })),
        "the star outcome must survive the list failure untouched"
    );
    assert!(
        matches!(
            handled.star_lists,
            Some(StarListSnapshotOutcome::Failed { .. })
        ),
        "the failed list snapshot is reported truthfully, got {:?}",
        handled.star_lists
    );

    // The star run's authority is exactly as it wrote it.
    let starred_rows: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.current_star_state where starred")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        starred_rows, 1,
        "the successful star snapshot's authority stands despite the list failure"
    );
    // The command is still consumed exactly once.
    let inbox_count: i64 = sqlx::query_scalar("select count(*) from github_catalog.inbox_events")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(inbox_count, 1);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn valid_envelope_dispatches_incremental_scan_and_records_inbox()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database, "tester").await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let server = MockServer::start().await;

    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_080, "acme/commanded", "2026-06-15T00:00:00Z")
        ),
    )
    .await?;
    mount_newest_first_page(&server, 2, "[]".to_owned()).await?;
    mount_empty_graphql(&server).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let envelope_json = envelope(Uuid::now_v7(), "tester", None);

    let consumed = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await?;

    let ConsumedSyncCommand::Handled(handled) = consumed else {
        return Err(format!("a fresh command must be handled, got {consumed:?}").into());
    };
    assert_eq!(handled.account_id, account_id);
    assert_eq!(handled.requested_mode, RequestedSyncMode::Incremental);
    assert!(
        matches!(
            handled.incremental,
            Some(IncrementalScanOutcome::Completed { .. })
        ),
        "the dispatched incremental scan must complete, got {:?}",
        handled.incremental
    );
    assert!(
        handled.full.is_none(),
        "an incremental dispatch must not run a full snapshot"
    );

    // The consumption is recorded in the owned inbox and marked consumed.
    let inbox_row: (Uuid, Option<String>, bool) = sqlx::query_as(
        "select message_id, subject, consumed_at is not null
             from github_catalog.inbox_events",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        inbox_row.0, handled.command_id,
        "the claim keys on the command identity"
    );
    assert_eq!(inbox_row.1.as_deref(), Some("github.sync.requested.v1"));
    assert!(
        inbox_row.2,
        "a handled command carries its consumption time"
    );

    // The delivery produced the incremental run plus its independent
    // star-list snapshot, both completed.
    let run_modes: Vec<String> =
        sqlx::query_scalar("select mode from github_catalog.sync_runs order by mode")
            .fetch_all(database.database.pool())
            .await?
            .into_iter()
            .collect();
    assert_eq!(
        run_modes,
        ["incremental", "star_lists"],
        "one command yields the requested scan and the independent list refresh"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn duplicate_command_redelivery_performs_no_second_effect()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database, "tester").await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let server = MockServer::start().await;

    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_081, "acme/once", "2026-06-15T00:00:00Z")
        ),
    )
    .await?;
    mount_newest_first_page(&server, 2, "[]".to_owned()).await?;
    mount_empty_graphql(&server).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let command_id = Uuid::now_v7();
    let envelope_json = envelope(command_id, "tester", None);

    let first = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await?;
    assert!(
        matches!(first, ConsumedSyncCommand::Handled(_)),
        "the first delivery must be handled, got {first:?}"
    );

    // Redelivery of the byte-identical envelope is inert.
    let second = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await?;
    assert_eq!(
        second,
        ConsumedSyncCommand::Duplicate,
        "the redelivered identity must short-circuit"
    );

    let star_run_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.sync_runs where mode in ('incremental', 'full')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        star_run_count, 1,
        "redelivery must not open a second star run"
    );
    let total_run_count: i64 = sqlx::query_scalar("select count(*) from github_catalog.sync_runs")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(
        total_run_count, 2,
        "the first delivery opened exactly the star run and its independent list run"
    );

    let inbox_count: i64 = sqlx::query_scalar("select count(*) from github_catalog.inbox_events")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(inbox_count, 1, "redelivery must not write a second claim");

    // The page mocks allow exactly one fetch each; any refetch would fail
    // verification below.
    server.verify().await;

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn foreign_command_type_is_rejected_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    seed_account(&database.database, "tester").await?;
    let gateway = ReqwestGithubApi::for_base_url(&MockServer::start().await.uri())?;

    let envelope_json = envelope(Uuid::now_v7(), "tester", None).replace(
        "github.sync.requested.v1",
        "github.backup_policy.changed.v1",
    );
    let outcome = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(ratatoskr_github_catalog::SyncCommandError::Invalid(_))
        ),
        "a foreign command type must be rejected as invalid, got {outcome:?}"
    );
    let side_effects: (i64, i64) = sqlx::query_as(
        "select (select count(*) from github_catalog.inbox_events),
                (select count(*) from github_catalog.sync_runs)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(side_effects, (0, 0), "rejection must leave no rows");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn malformed_tenant_is_rejected_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    seed_account(&database.database, "tester").await?;
    let gateway = ReqwestGithubApi::for_base_url(&MockServer::start().await.uri())?;

    let envelope_json = envelope(Uuid::now_v7(), "tester", None).replace("\"user:", "\"team:");
    let outcome = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(ratatoskr_github_catalog::SyncCommandError::Invalid(_))
        ),
        "a malformed tenant must be rejected as invalid, got {outcome:?}"
    );
    let side_effects: (i64, i64) = sqlx::query_as(
        "select (select count(*) from github_catalog.inbox_events),
                (select count(*) from github_catalog.sync_runs)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(side_effects, (0, 0), "rejection must leave no rows");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn unknown_account_reference_is_rejected_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    seed_account(&database.database, "tester").await?;
    let gateway = ReqwestGithubApi::for_base_url(&MockServer::start().await.uri())?;

    let envelope_json = envelope(Uuid::now_v7(), "ghost", None);
    let outcome = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(ratatoskr_github_catalog::SyncCommandError::UnknownAccount)
        ),
        "an unknown account reference must be rejected, got {outcome:?}"
    );
    let side_effects: (i64, i64) = sqlx::query_as(
        "select (select count(*) from github_catalog.inbox_events),
                (select count(*) from github_catalog.sync_runs)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(side_effects, (0, 0), "rejection must leave no rows");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn disconnected_account_reference_is_rejected_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database, "tester").await?;
    sqlx::query(
        "update github_catalog.github_accounts set status = 'revoked' where account_id = $1",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    let gateway = ReqwestGithubApi::for_base_url(&MockServer::start().await.uri())?;

    let envelope_json = envelope(Uuid::now_v7(), "tester", None);
    let outcome = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(ratatoskr_github_catalog::SyncCommandError::AccountNotConnected)
        ),
        "a disconnected account must be rejected, got {outcome:?}"
    );
    let side_effects: (i64, i64) = sqlx::query_as(
        "select (select count(*) from github_catalog.inbox_events),
                (select count(*) from github_catalog.sync_runs)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(side_effects, (0, 0), "rejection must leave no rows");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn unsupported_payload_mode_is_rejected_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    seed_account(&database.database, "tester").await?;
    let gateway = ReqwestGithubApi::for_base_url(&MockServer::start().await.uri())?;

    let envelope_json = envelope(Uuid::now_v7(), "tester", Some("rebuild"));
    let outcome = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(ratatoskr_github_catalog::SyncCommandError::Invalid(_))
        ),
        "an unsupported mode must be rejected as invalid, got {outcome:?}"
    );
    let side_effects: (i64, i64) = sqlx::query_as(
        "select (select count(*) from github_catalog.inbox_events),
                (select count(*) from github_catalog.sync_runs)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(side_effects, (0, 0), "rejection must leave no rows");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn gap_during_commanded_incremental_chains_full_rescan()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database, "tester").await?;
    seed_watermark(&database.database, account_id, "2026-05-01T00:00:00Z").await?;
    let server = MockServer::start().await;

    // The commanded incremental scan hits an unorderable page immediately.
    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item_without_timestamp(300_000_090, "acme/gap")
        ),
    )
    .await?;
    // The chained full rescan enumerates through the unordered listing and
    // finds one repository.
    mount_unordered_page(
        &server,
        1,
        format!(
            "[{}]",
            starred_item(300_000_091, "acme/rescued", "2026-06-20T00:00:00Z")
        ),
    )
    .await?;
    mount_unordered_page(&server, 2, "[]".to_owned()).await?;
    mount_empty_graphql(&server).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let envelope_json = envelope(Uuid::now_v7(), "tester", None);

    let consumed = handle_sync_command(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("commands-e2e"),
        &envelope_json,
    )
    .await?;

    let ConsumedSyncCommand::Handled(handled) = consumed else {
        return Err(format!("the gap-carrying command must be handled, got {consumed:?}").into());
    };
    let incremental = handled
        .incremental
        .expect("an incremental dispatch must report its outcome");
    let IncrementalScanOutcome::GapDetected { .. } = incremental else {
        return Err(format!("handling must report the gap, got {incremental:?}").into());
    };
    assert!(
        matches!(
            handled.full,
            Some(ratatoskr_github_catalog::FullSnapshotOutcome::Completed { .. })
        ),
        "the gap must chain into a completed full rescan, got {:?}",
        handled.full
    );
    assert!(
        matches!(
            handled.star_lists,
            Some(ratatoskr_github_catalog::StarListSnapshotOutcome::Completed { .. })
        ),
        "the chained list snapshot still completes independently, got {:?}",
        handled.star_lists
    );

    // Three runs tell the story: the failed incremental pass, the completing
    // full rescan, and the independent list refresh - in deterministic order.
    let run_rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "select mode, status, failure_reason from github_catalog.sync_runs order by mode",
    )
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .collect();
    assert_eq!(run_rows.len(), 3, "the gap chains into exactly one rescan");
    assert_eq!(run_rows[0].0, "full");
    assert_eq!(run_rows[0].1, "completed");
    assert_eq!(run_rows[1].0, "incremental");
    assert_eq!(run_rows[1].1, "failed");
    assert!(
        run_rows[1]
            .2
            .as_deref()
            .is_some_and(|reason| reason.starts_with("starred_at ordering gap detected")),
        "the failed pass names its gap, got {:?}",
        run_rows[1].2
    );
    assert_eq!(run_rows[2].0, "star_lists");
    assert_eq!(run_rows[2].1, "completed");

    // Authority afterwards reflects the full snapshot's enumeration.
    let provider_ids: Vec<i64> =
        sqlx::query_scalar("select provider_repository_id from github_catalog.repositories order by provider_repository_id")
            .fetch_all(database.database.pool())
            .await?
            .into_iter()
            .collect();
    assert_eq!(
        provider_ids,
        [300_000_091],
        "the rescan's enumeration is the surviving authority"
    );

    database.cleanup().await?;
    Ok(())
}
