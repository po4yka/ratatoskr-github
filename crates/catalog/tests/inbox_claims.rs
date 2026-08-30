//! Resumable inbox lease and terminal duplicate behavior.

use ratatoskr_github_catalog::{
    InboxClaimOutcome, InboxDelivery, claim_inbox_delivery, complete_inbox_delivery,
    reject_inbox_delivery, retry_inbox_delivery, test_support::TestDatabase,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn retryable_and_expired_processing_claims_resume() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    let message_id = Uuid::now_v7();
    let delivery = delivery(message_id, 1);
    let InboxClaimOutcome::Claimed { lease_owner } =
        claim_inbox_delivery(&database.database, &delivery, now, Duration::seconds(1)).await?
    else {
        return Err("first delivery was not claimed".into());
    };
    assert_eq!(
        claim_inbox_delivery(&database.database, &delivery, now, Duration::seconds(1)).await?,
        InboxClaimOutcome::Busy
    );
    retry_inbox_delivery(
        &database.database,
        message_id,
        lease_owner,
        "provider_unavailable",
        now,
    )
    .await?;
    assert!(matches!(
        claim_inbox_delivery(&database.database, &delivery, now, Duration::seconds(1)).await?,
        InboxClaimOutcome::Claimed { .. }
    ));
    sqlx::query("update github_catalog.inbox_events set lease_expires_at=$2 where message_id=$1")
        .bind(message_id)
        .bind(now)
        .execute(database.database.pool())
        .await?;
    assert!(matches!(
        claim_inbox_delivery(
            &database.database,
            &delivery,
            now + Duration::seconds(1),
            Duration::seconds(1)
        )
        .await?,
        InboxClaimOutcome::Claimed { .. }
    ));
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn terminal_completion_and_rejection_are_duplicates() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    for rejected in [false, true] {
        let message_id = Uuid::now_v7();
        let delivery = delivery(message_id, if rejected { 2 } else { 1 });
        let InboxClaimOutcome::Claimed { lease_owner } =
            claim_inbox_delivery(&database.database, &delivery, now, Duration::seconds(10)).await?
        else {
            return Err("delivery was not claimed".into());
        };
        if rejected {
            reject_inbox_delivery(
                &database.database,
                message_id,
                lease_owner,
                "invalid_envelope",
                now,
            )
            .await?;
        } else {
            complete_inbox_delivery(&database.database, message_id, lease_owner, "applied", now)
                .await?;
        }
        assert_eq!(
            claim_inbox_delivery(&database.database, &delivery, now, Duration::seconds(10)).await?,
            InboxClaimOutcome::TerminalDuplicate
        );
    }
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn reused_message_identity_with_other_bytes_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let now = OffsetDateTime::now_utc();
    let message_id = Uuid::now_v7();
    claim_inbox_delivery(
        &database.database,
        &delivery(message_id, 1),
        now,
        Duration::seconds(10),
    )
    .await?;
    let altered = InboxDelivery {
        envelope: br#"{"changed":true}"#,
        ..delivery(message_id, 2)
    };
    assert!(
        claim_inbox_delivery(&database.database, &altered, now, Duration::seconds(10))
            .await
            .is_err()
    );
    database.cleanup().await?;
    Ok(())
}

fn delivery(message_id: Uuid, delivery_count: i32) -> InboxDelivery<'static> {
    InboxDelivery {
        message_id,
        subject: "cmd.github.sync.requested.v1",
        envelope: br#"{"schema_version":1}"#,
        stream_name: "ratatoskr_commands",
        consumer_name: "ratatoskr_github_sync",
        stream_sequence: i64::from(delivery_count),
        delivery_count,
        owner_ref: None,
    }
}
