//! The published policy is a complete Vault command envelope (XR-021 CONTRACTS.md S09).

use ratatoskr_backup_contracts::VaultBackupPolicyApplyRequested;
use ratatoskr_event_envelope::CommandEnvelope;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    PublicationOutcome, mark_backup_policy_dirty, publish_due_backup_policy,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn published_policy_is_a_vault_apply_command_envelope() {
    let fixture = TestDatabase::create().await.expect("test database");
    let repository_id = Uuid::now_v7();
    sqlx::query("insert into github_catalog.repositories (repository_id, provider_repository_id, mode) values ($1, 942001, 'tracked')")
        .bind(repository_id)
        .execute(fixture.database.pool())
        .await
        .expect("repository");
    let now = OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339).expect("anchor");
    mark_backup_policy_dirty(&fixture.database, now)
        .await
        .expect("dirty");
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now + Duration::seconds(60))
            .await
            .expect("published"),
        PublicationOutcome::Published { policy_version: 1 }
    );

    let (message_id, subject, payload): (Uuid, String, serde_json::Value) =
        sqlx::query_as("select message_id, subject, payload from github_catalog.outbox_events")
            .fetch_one(fixture.database.pool())
            .await
            .expect("outbox row");
    assert_eq!(subject, "cmd.vault.backup_policy.apply_requested.v1");
    let envelope = CommandEnvelope::from_json(payload.to_string().as_bytes()).expect("envelope");
    assert_eq!(envelope.command_id.to_string(), message_id.to_string());
    assert_eq!(envelope.producer.to_string(), "ratatoskr-github");
    assert_eq!(envelope.aggregate_id.to_wire(), "backup_policy:1");
    assert_eq!(envelope.correlation_id.to_wire(), "backup_policy:1");
    assert!(envelope.tenant_id.is_none(), "the policy is catalog-wide");
    let command: VaultBackupPolicyApplyRequested = envelope.payload_as().expect("payload");
    assert_eq!(command.policy.policy_version, 1);
    assert_eq!(
        command.policy.repositories[0].repository_ref.to_wire(),
        format!("repository:{repository_id}")
    );
    fixture.cleanup().await.expect("cleanup");
}
