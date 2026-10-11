//! The outbox holds complete class-prefixed envelopes (XR-021 CONTRACTS.md S02 and S09).

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions and synthetic fixture construction in a test binary"
)]

use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_github_catalog::provider::ProviderRepositoryBody;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AnalysisDispatch, RepositoryAnalysisSource, WatchBaseline, WatchEvaluation, apply_fresh_body,
    apply_fresh_source, dispatch_due_repository_analysis, evaluate_metadata_watches,
    register_repository_analysis_watch, store_readme,
};
use ratatoskr_github_contracts::{ReadmeRevision, RepositoryAnalysisRequested};
use ratatoskr_identifiers::TenantRef;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000941";

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339).expect("fixed RFC3339 instant")
}

fn body(description: &str) -> ProviderRepositoryBody {
    ProviderRepositoryBody {
        provider_repository_id: 941_001,
        full_name: "acme/enveloped".to_owned(),
        description: Some(description.to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 7,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-27T00:00:00Z".to_owned()),
    }
}

#[tokio::test]
async fn dispatched_analysis_request_is_a_complete_event_envelope()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 941_001).await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body("before"),
        None,
    )
    .await?;
    register_repository_analysis_watch(
        &database.database,
        TenantRef::parse(OWNER)?,
        repository.repository_id,
        WatchBaseline::Current,
    )
    .await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body("after"),
        None,
    )
    .await?;
    evaluate_metadata_watches(
        &database.database,
        repository.repository_id,
        &body("after"),
        fixed_now(),
    )
    .await?;
    let AnalysisDispatch::Pending { request_id } =
        dispatch_due_repository_analysis(&database.database, fixed_now()).await?
    else {
        return Err("one queued request must dispatch".into());
    };

    let (message_id, subject, payload): (Uuid, String, serde_json::Value) =
        sqlx::query_as("select message_id, subject, payload from github_catalog.outbox_events")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(subject, "evt.knowledge.repository_analysis.requested.v1");
    let envelope = EventEnvelope::from_json(payload.to_string().as_bytes())?;
    assert_eq!(envelope.event_id.to_string(), message_id.to_string());
    assert_eq!(envelope.producer.to_string(), "ratatoskr-github");
    assert_eq!(
        envelope.aggregate_id.to_wire(),
        format!("repository:{}", repository.repository_id)
    );
    assert_eq!(
        envelope.correlation_id.to_wire(),
        format!("repository_analysis:{request_id}")
    );
    assert_eq!(
        envelope.tenant_id.map(|tenant| tenant.to_string()),
        Some(OWNER.to_owned())
    );
    let requested: RepositoryAnalysisRequested = envelope.payload_as()?;
    assert_eq!(requested.request_id, request_id);
    assert_eq!(requested.owner.to_string(), OWNER);

    database.cleanup().await?;
    Ok(())
}

const OTHER_OWNER: &str = "user:018f0000-0000-7000-8000-000000000942";

#[tokio::test]
async fn an_unevaluated_baseline_queues_the_first_analysis()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 941_001).await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body("only"),
        None,
    )
    .await?;
    for (owner, baseline) in [
        (OWNER, WatchBaseline::Unevaluated),
        (OTHER_OWNER, WatchBaseline::Current),
    ] {
        register_repository_analysis_watch(
            &database.database,
            TenantRef::parse(owner)?,
            repository.repository_id,
            baseline,
        )
        .await?;
    }

    let evaluation = evaluate_metadata_watches(
        &database.database,
        repository.repository_id,
        &body("only"),
        fixed_now(),
    )
    .await?;

    assert_eq!(evaluation, WatchEvaluation::Queued { requests: 1 });
    let owners: Vec<String> =
        sqlx::query_scalar("select owner_ref from github_catalog.repository_analysis_requests")
            .fetch_all(database.database.pool())
            .await?;
    assert_eq!(owners, vec![OWNER.to_owned()]);
    assert_eq!(
        evaluate_metadata_watches(
            &database.database,
            repository.repository_id,
            &body("only"),
            fixed_now(),
        )
        .await?,
        WatchEvaluation::Unchanged,
        "evaluating the same revision again queues nothing"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_readme_only_change_queues_a_new_request() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 941_001).await?;
    let owner = TenantRef::parse(OWNER)?;
    let current = body("unchanged attributes");
    let mut digests = Vec::new();
    for (position, readme) in [b"# first\n".as_slice(), b"# second\n".as_slice()]
        .into_iter()
        .enumerate()
    {
        let content_ref = store_readme(&database.database, readme).await?;
        digests.push(content_ref.digest.hex.to_string());
        apply_fresh_source(
            &database.database,
            repository.repository_id,
            &current,
            None,
            &RepositoryAnalysisSource {
                readme: ReadmeRevision::Present { content_ref },
                readme_etag: None,
            },
        )
        .await?;
        if position == 0 {
            register_repository_analysis_watch(
                &database.database,
                owner,
                repository.repository_id,
                WatchBaseline::Unevaluated,
            )
            .await?;
        }
        assert_eq!(
            evaluate_metadata_watches(
                &database.database,
                repository.repository_id,
                &current,
                fixed_now(),
            )
            .await?,
            WatchEvaluation::Queued { requests: 1 },
            "revision {position} must queue one request"
        );
    }

    let queued: Vec<String> = sqlx::query_scalar(
        "select source_revision #>> '{readme,content_ref,digest,hex}'
         from github_catalog.repository_analysis_requests order by created_at, request_id",
    )
    .fetch_all(database.database.pool())
    .await?;
    assert_eq!(queued, digests);

    database.cleanup().await?;
    Ok(())
}
