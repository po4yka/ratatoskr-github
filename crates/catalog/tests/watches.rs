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
    AnalysisDispatch, apply_fresh_body, dispatch_due_repository_analysis,
    evaluate_metadata_watches, register_repository_analysis_watch,
};
use ratatoskr_github_contracts::RepositoryAnalysisRequested;
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
