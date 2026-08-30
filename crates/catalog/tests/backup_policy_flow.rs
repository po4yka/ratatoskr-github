//! Desired backup-policy publication behavior.

#![expect(
    clippy::expect_used,
    reason = "integration tests stop immediately when their disposable fixture fails"
)]

use ratatoskr_backup_contracts::{
    DesiredBackupPolicy, PolicyAcknowledged, PolicyOutcome, PolicyRejectionCode,
    PolicyRejectionReason,
};
use ratatoskr_github_catalog::{
    BackupPolicyInput, FeedbackOutcome, PublicationOutcome, derive_backup_policy,
    latest_backup_policy_feedback, mark_backup_policy_dirty, publish_due_backup_policy,
    record_backup_policy_acknowledgment, test_support::TestDatabase,
};
use ratatoskr_identifiers::{EntityRef, Extensions};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[test]
fn derived_policy_contains_only_mirror_governed_repositories() {
    let policy = derive_backup_policy(
        1,
        &[
            BackupPolicyInput::mirror(
                "repository:018f0000-0000-7000-8000-000000000501",
                "daily",
                "critical",
                Some(2_147_483_648),
                vec![("refs_matching", "refs/heads/scratch/*")],
            ),
            BackupPolicyInput::excluded("repository:018f0000-0000-7000-8000-000000000502"),
        ],
    )
    .expect("policy derives");

    assert_eq!(policy.repositories.len(), 1);
    assert_eq!(policy.repositories[0].size_hint_bytes, Some(2_147_483_648));
}

#[tokio::test]
async fn published_policy_versions_advance_only_when_derived_state_changes() {
    let fixture = TestDatabase::create().await.expect("test database");
    insert_tracked_mirror(&fixture, "daily", "standard").await;
    let now = OffsetDateTime::now_utc();
    mark_backup_policy_dirty(&fixture.database, now)
        .await
        .expect("dirty");
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now)
            .await
            .expect("pending"),
        PublicationOutcome::Pending
    );
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now + Duration::seconds(60))
            .await
            .expect("first"),
        PublicationOutcome::Published { policy_version: 1 }
    );
    let (message_id, envelope_bytes): (Uuid, Vec<u8>) = sqlx::query_as("select message_id,envelope from github_catalog.outbox_events where subject = 'cmd.vault.target.desired.v1'")
        .fetch_one(fixture.database.pool()).await.expect("published command");
    let envelope = ratatoskr_event_envelope::CommandEnvelope::from_json(&envelope_bytes)
        .expect("canonical command envelope");
    assert_eq!(envelope.command_id.to_string(), message_id.to_string());
    assert_eq!(envelope.command_type.to_wire(), "vault.target.desired.v1");
    let policy: DesiredBackupPolicy =
        serde_json::from_value(serde_json::Value::Object(envelope.payload))
            .expect("typed desired policy");
    assert_eq!(
        policy.repositories[0].exclusions[0].expression.as_str(),
        "refs/heads/scratch/*"
    );
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now + Duration::seconds(61))
            .await
            .expect("unchanged"),
        PublicationOutcome::Pending
    );
    sqlx::query("update github_catalog.backup_policies set priority_hint = 'critical'")
        .execute(fixture.database.pool())
        .await
        .expect("change policy");
    mark_backup_policy_dirty(&fixture.database, now + Duration::seconds(70))
        .await
        .expect("dirty second");
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now + Duration::seconds(130))
            .await
            .expect("second"),
        PublicationOutcome::Published { policy_version: 2 }
    );
    let count: i64 = sqlx::query_scalar("select count(*) from github_catalog.outbox_events where subject = 'cmd.vault.target.desired.v1'").fetch_one(fixture.database.pool()).await.expect("count");
    assert_eq!(count, 2);
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn burst_policy_changes_publish_once_after_the_trailing_deadline() {
    let fixture = TestDatabase::create().await.expect("test database");
    insert_tracked_mirror(&fixture, "daily", "standard").await;
    let now = OffsetDateTime::now_utc();
    mark_backup_policy_dirty(&fixture.database, now)
        .await
        .expect("first dirty");
    mark_backup_policy_dirty(&fixture.database, now + Duration::seconds(30))
        .await
        .expect("second dirty");
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now + Duration::seconds(89))
            .await
            .expect("early"),
        PublicationOutcome::Pending
    );
    assert_eq!(
        publish_due_backup_policy(&fixture.database, now + Duration::seconds(90))
            .await
            .expect("due"),
        PublicationOutcome::Published { policy_version: 1 }
    );
    fixture.cleanup().await.expect("cleanup");
}

#[tokio::test]
async fn rejected_acknowledgment_is_recorded_once_under_redelivery() {
    let fixture = TestDatabase::create().await.expect("test database");
    let acknowledgment = PolicyAcknowledged {
        acknowledged_policy_version: 2,
        outcome: PolicyOutcome::Rejected,
        reasons: vec![PolicyRejectionReason {
            code: PolicyRejectionCode::RepositoryUnknownInCatalog,
            repository_ref: Some(
                EntityRef::parse("repository:018f0000-0000-7000-8000-000000000501")
                    .expect("reference"),
            ),
        }],
        last_applied_policy_version: 1,
        extensions: Extensions::default(),
    };
    let message_id = Uuid::now_v7();
    assert_eq!(
        record_backup_policy_acknowledgment(&fixture.database, message_id, &acknowledgment)
            .await
            .expect("record"),
        FeedbackOutcome::Recorded
    );
    assert_eq!(
        record_backup_policy_acknowledgment(&fixture.database, message_id, &acknowledgment)
            .await
            .expect("duplicate"),
        FeedbackOutcome::Duplicate
    );
    let feedback = latest_backup_policy_feedback(&fixture.database)
        .await
        .expect("feedback")
        .expect("stored");
    assert_eq!(feedback.policy_version, 2);
    assert_eq!(feedback.outcome, PolicyOutcome::Rejected);
    assert_eq!(feedback.last_applied_policy_version, 1);
    fixture.cleanup().await.expect("cleanup");
}

async fn insert_tracked_mirror(fixture: &TestDatabase, cadence: &str, priority: &str) {
    let repository_id = Uuid::now_v7();
    sqlx::query("insert into github_catalog.repositories (repository_id, provider_repository_id, mode) values ($1, $2, 'tracked')")
        .bind(repository_id).bind(91_000_i64).execute(fixture.database.pool()).await.expect("repository");
    sqlx::query("insert into github_catalog.backup_policies (backup_policy_id, repository_id, policy_level, mirror_cadence, priority_hint, size_hint_bytes, exclusions) values ($1, $2, 'git_mirror', $3, $4, 1024, $5)")
        .bind(Uuid::now_v7()).bind(repository_id).bind(cadence).bind(priority)
        .bind(serde_json::json!([{ "scope": "refs_matching", "expression": "refs/heads/scratch/*" }]))
        .execute(fixture.database.pool()).await.expect("policy");
}
