//! `PostgreSQL` behavior for repository watches and Knowledge analysis linkage.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions and synthetic fixture construction in a test binary"
)]

use ratatoskr_github_catalog::provider::{ProviderRepositoryBody, ReqwestGithubApi};
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AnalysisDispatch, TerminalFactOutcome, WatchEvaluation, apply_fresh_body,
    consume_repository_analysis_completed, dispatch_due_repository_analysis,
    evaluate_metadata_watches, observe_repository, register_repository_analysis_watch,
    repository_analysis_request_state,
};
use ratatoskr_github_contracts::RepositoryAnalysisCompleted;
use ratatoskr_identifiers::{EntityRef, Extensions, TenantRef, WireTimestamp};
use time::OffsetDateTime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn watch_registration_baselines_existing_metadata() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 904_001).await?;
    sqlx::query(
        "insert into github_catalog.repository_metadata
             (repository_id, stargazers_count, content_hash, fetched_at)
         values ($1, 0, 'current-metadata', now())",
    )
    .bind(repository.repository_id)
    .execute(database.database.pool())
    .await?;

    let registration = register_repository_analysis_watch(
        &database.database,
        TenantRef::parse("user:018f0000-0000-7000-8000-000000000901")?,
        repository.repository_id,
    )
    .await?;
    let checkpoint: String = sqlx::query_scalar(
        "select last_evaluated_content_hash from github_catalog.repository_watches
         where watch_id = $1",
    )
    .bind(registration.watch_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(checkpoint, "current-metadata");
    let requests: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.repository_analysis_requests")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(requests, 0);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn metadata_delta_queues_and_dispatches_one_analysis_request()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 904_002).await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &repository_body("initial description"),
        None,
    )
    .await?;
    Mock::given(method("GET"))
        .and(path("/repos/acme/watched"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining", "4998")
                .set_body_json(serde_json::json!({
                    "id": 904_002,
                    "full_name": "acme/watched",
                    "description": "changed description",
                    "language": "Rust",
                    "stargazers_count": 42,
                    "topics": [],
                    "default_branch": "main",
                    "pushed_at": "2026-08-27T00:00:00Z"
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("watch-account");
    let analysis_owner = TenantRef::parse("user:018f0000-0000-7000-8000-000000000902")?;
    register_repository_analysis_watch(
        &database.database,
        analysis_owner,
        repository.repository_id,
    )
    .await?;
    observe_repository(
        &database.database,
        &gateway,
        &ledger,
        &token,
        analysis_owner,
        "acme",
        "watched",
    )
    .await?;

    let dispatch =
        dispatch_due_repository_analysis(&database.database, OffsetDateTime::now_utc()).await?;
    assert!(matches!(dispatch, AnalysisDispatch::Pending { .. }));
    assert_eq!(
        evaluate_metadata_watches(
            &database.database,
            repository.repository_id,
            &repository_body("changed description"),
            OffsetDateTime::now_utc(),
        )
        .await?,
        WatchEvaluation::Unchanged
    );
    assert_eq!(
        dispatch_due_repository_analysis(&database.database, OffsetDateTime::now_utc()).await?,
        AnalysisDispatch::NotDue
    );
    let requested: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.outbox_events
         where subject = 'knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    // The fresh source revision publishes immediately with README evidence;
    // the separately registered watch retains its paced policy command.
    assert_eq!(requested, 2);

    server.verify().await;
    database.cleanup().await?;
    Ok(())
}

fn repository_body(description: &str) -> ProviderRepositoryBody {
    ProviderRepositoryBody {
        provider_repository_id: 904_002,
        full_name: "acme/watched".to_owned(),
        description: Some(description.to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 42,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-27T00:00:00Z".to_owned()),
    }
}

#[tokio::test]
async fn matching_completion_resolves_the_pending_request_once()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 904_003).await?;
    let initial = ProviderRepositoryBody {
        provider_repository_id: 904_003,
        full_name: "acme/completed".to_owned(),
        description: Some("initial".to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 0,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: None,
    };
    apply_fresh_body(&database.database, repository.repository_id, &initial, None).await?;
    register_repository_analysis_watch(
        &database.database,
        TenantRef::parse("user:018f0000-0000-7000-8000-000000000903")?,
        repository.repository_id,
    )
    .await?;
    let changed = ProviderRepositoryBody {
        description: Some("changed".to_owned()),
        ..initial
    };
    apply_fresh_body(&database.database, repository.repository_id, &changed, None).await?;
    ratatoskr_github_catalog::evaluate_metadata_watches(
        &database.database,
        repository.repository_id,
        &changed,
        OffsetDateTime::now_utc(),
    )
    .await?;
    let AnalysisDispatch::Pending { request_id } =
        dispatch_due_repository_analysis(&database.database, OffsetDateTime::now_utc()).await?
    else {
        return Err("one queued request must dispatch".into());
    };
    let payload: serde_json::Value = sqlx::query_scalar(
        "select payload from github_catalog.outbox_events
         where subject = 'knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    let request: ratatoskr_github_contracts::RepositoryAnalysisRequested =
        serde_json::from_value(payload)?;
    let result = EntityRef::parse("analysis:018f0000-0000-7000-8000-000000000904")?;
    let completion = RepositoryAnalysisCompleted {
        owner: request.owner,
        repository_id: request.repository_id,
        github_repository_numeric_id: request.github_repository_numeric_id,
        request_id,
        source_revision: request.source_revision,
        analysis_result_ref: result.clone(),
        completed_at: WireTimestamp::now(),
        extensions: Extensions::new(),
    };
    assert_eq!(
        consume_repository_analysis_completed(
            &database.database,
            uuid::Uuid::now_v7(),
            &completion
        )
        .await?,
        TerminalFactOutcome::Resolved
    );
    assert_eq!(
        repository_analysis_request_state(&database.database, request_id)
            .await?
            .expect("request remains visible")
            .analysis_result_ref
            .as_deref(),
        Some(result.to_wire().as_str())
    );
    let linked: String = sqlx::query_scalar(
        "select analysis_result_ref from github_catalog.repository_analysis_links
         where repository_id = $1 and owner_ref = $2",
    )
    .bind(repository.repository_id)
    .bind(completion.owner.to_string())
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(linked, result.to_wire());

    database.cleanup().await?;
    Ok(())
}
