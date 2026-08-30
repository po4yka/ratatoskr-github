//! Durable outbox ordering, lease, retry and recovery behavior.

use ratatoskr_github_catalog::{
    OutboxFailureCode, OutboxTransport, claim_due_outbox, confirm_outbox_published,
    fail_outbox_publication, publish_outbox_batch, requeue_dead_letter, test_support::TestDatabase,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[derive(Debug, Default)]
struct RecordingTransport(std::sync::Mutex<Vec<(String, Vec<u8>, Uuid)>>);

impl OutboxTransport for RecordingTransport {
    async fn publish(
        &self,
        subject: &str,
        envelope: &[u8],
        message_id: Uuid,
    ) -> Result<(), OutboxFailureCode> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((subject.to_owned(), envelope.to_vec(), message_id));
        Ok(())
    }
}

#[tokio::test]
async fn publisher_uses_stored_bytes_identity_and_marks_only_after_ack()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    let message_id = insert(&database, "publish", 1, now).await?;
    let stored: (String, Vec<u8>) = sqlx::query_as(
        "select subject, envelope from github_catalog.outbox_events where message_id = $1",
    )
    .bind(message_id)
    .fetch_one(database.database.pool())
    .await?;
    let transport = RecordingTransport::default();
    let report = publish_outbox_batch(
        &database.database,
        &transport,
        Uuid::now_v7(),
        now,
        Duration::seconds(30),
        4,
        3,
        Duration::seconds(1),
    )
    .await?;
    assert_eq!(report.published, 1);
    {
        let calls = transport
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(calls.as_slice(), &[(stored.0, stored.1, message_id)]);
    }
    let published: bool = sqlx::query_scalar(
        "select published_at is not null from github_catalog.outbox_events where message_id = $1",
    )
    .bind(message_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(published);
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn claims_due_rows_with_bounded_leases() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    insert(&database, "a", 1, now).await?;
    insert(&database, "b", 1, now).await?;
    let owner = Uuid::now_v7();
    let claims = claim_due_outbox(&database.database, owner, now, Duration::seconds(30), 1).await?;
    assert_eq!(claims.len(), 1);
    let lease: (Uuid, OffsetDateTime) = sqlx::query_as(
        "select lease_owner, lease_expires_at from github_catalog.outbox_events
         where message_id = $1",
    )
    .bind(claims[0].message_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(lease, (owner, now + Duration::seconds(30)));
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn same_key_preserves_sequence_and_unrelated_key_bypasses_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    let first = insert(&database, "blocked", 1, now).await?;
    insert(&database, "blocked", 2, now).await?;
    let unrelated = insert(&database, "free", 1, now).await?;
    let owner = Uuid::now_v7();
    let claims = claim_due_outbox(&database.database, owner, now, Duration::seconds(10), 8).await?;
    assert_eq!(claims.len(), 2);
    assert!(claims.iter().any(|row| row.message_id == first));
    assert!(claims.iter().any(|row| row.message_id == unrelated));
    fail_outbox_publication(
        &database.database,
        first,
        owner,
        now,
        now + Duration::minutes(5),
        3,
        OutboxFailureCode::BusUnavailable,
    )
    .await?;
    confirm_outbox_published(&database.database, unrelated, owner, now).await?;
    let later = claim_due_outbox(
        &database.database,
        Uuid::now_v7(),
        now + Duration::seconds(1),
        Duration::seconds(10),
        8,
    )
    .await?;
    assert!(later.is_empty(), "later same-key row must not overtake");
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn expired_lease_is_reclaimed() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    let message_id = insert(&database, "reclaim", 1, now).await?;
    let first_owner = Uuid::now_v7();
    claim_due_outbox(
        &database.database,
        first_owner,
        now,
        Duration::seconds(1),
        1,
    )
    .await?;
    let second_owner = Uuid::now_v7();
    let reclaimed = claim_due_outbox(
        &database.database,
        second_owner,
        now + Duration::seconds(2),
        Duration::seconds(10),
        1,
    )
    .await?;
    assert_eq!(reclaimed[0].message_id, message_id);
    assert_eq!(reclaimed[0].attempt_count, 2);
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn exhausted_row_dead_letters_and_exact_requeue_preserves_wire_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    let message_id = insert(&database, "dead", 1, now).await?;
    let before: (String, Vec<u8>) = sqlx::query_as(
        "select subject, envelope from github_catalog.outbox_events where message_id = $1",
    )
    .bind(message_id)
    .fetch_one(database.database.pool())
    .await?;
    let owner = Uuid::now_v7();
    claim_due_outbox(&database.database, owner, now, Duration::seconds(10), 1).await?;
    fail_outbox_publication(
        &database.database,
        message_id,
        owner,
        now,
        now + Duration::seconds(1),
        1,
        OutboxFailureCode::PublishRejected,
    )
    .await?;
    requeue_dead_letter(&database.database, message_id, now + Duration::seconds(2)).await?;
    let after: (String, Vec<u8>, i32, Option<OffsetDateTime>) = sqlx::query_as(
        "select subject, envelope, attempt_count, dead_lettered_at
         from github_catalog.outbox_events where message_id = $1",
    )
    .bind(message_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!((after.0, after.1), before);
    assert_eq!(after.2, 0);
    assert!(after.3.is_none());
    assert!(
        requeue_dead_letter(&database.database, message_id, now)
            .await
            .is_err(),
        "a non-dead-letter must be refused"
    );
    assert!(
        requeue_dead_letter(&database.database, Uuid::now_v7(), now)
            .await
            .is_err(),
        "an unknown identity must be refused"
    );
    let publisher = Uuid::now_v7();
    let claim = claim_due_outbox(
        &database.database,
        publisher,
        now + Duration::seconds(3),
        Duration::seconds(10),
        1,
    )
    .await?;
    confirm_outbox_published(&database.database, claim[0].message_id, publisher, now).await?;
    assert!(
        requeue_dead_letter(&database.database, claim[0].message_id, now)
            .await
            .is_err(),
        "a published identity must be refused"
    );
    database.cleanup().await?;
    Ok(())
}

async fn insert(
    database: &TestDatabase,
    key: &str,
    sequence: i64,
    due: OffsetDateTime,
) -> Result<Uuid, sqlx::Error> {
    let message_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.outbox_events
             (message_id, subject, envelope, ordering_key, ordering_sequence, next_attempt_at)
         values ($1, 'evt.knowledge.repository_analysis.requested.v1', $2, $3, $4, $5)",
    )
    .bind(message_id)
    .bind(br#"{"schema_version":1}"#.as_slice())
    .bind(key)
    .bind(sequence)
    .bind(due)
    .execute(database.database.pool())
    .await?;
    Ok(message_id)
}
