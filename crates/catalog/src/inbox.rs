use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Database, PersistenceError};

/// Transport coordinates and exact bytes of one fixed-durable delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxDelivery<'a> {
    /// Envelope identity.
    pub message_id: Uuid,
    /// Classified transport subject.
    pub subject: &'a str,
    /// Exact broker payload bytes.
    pub envelope: &'a [u8],
    /// Platform stream name.
    pub stream_name: &'a str,
    /// Fixed durable consumer name.
    pub consumer_name: &'a str,
    /// Stream sequence supplied by `JetStream` metadata.
    pub stream_sequence: i64,
    /// Broker delivery count.
    pub delivery_count: i32,
    /// Optional owner resolved from a canonical envelope.
    pub owner_ref: Option<&'a str>,
}

/// Result of attempting to acquire durable processing ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxClaimOutcome {
    /// This caller owns a finite processing lease.
    Claimed {
        /// Random lease identity required by terminal/retry transitions.
        lease_owner: Uuid,
    },
    /// A prior consumed or rejected outcome already committed.
    TerminalDuplicate,
    /// Another live worker still owns the processing lease.
    Busy,
}

/// Records or resumes one delivery without treating an unfinished claim as a duplicate.
///
/// # Errors
///
/// Returns [`PersistenceError`] for database failures or identity reuse with different bytes.
pub async fn claim_inbox_delivery(
    database: &Database,
    delivery: &InboxDelivery<'_>,
    now: OffsetDateTime,
    lease_duration: Duration,
) -> Result<InboxClaimOutcome, PersistenceError> {
    sqlx::query(
        "insert into github_catalog.inbox_events
             (message_id,subject,envelope,owner_ref,stream_name,consumer_name,stream_sequence,
              delivery_count,state)
         values ($1,$2,$3,$4,$5,$6,$7,$8,'received')
         on conflict (message_id) do nothing",
    )
    .bind(delivery.message_id)
    .bind(delivery.subject)
    .bind(delivery.envelope)
    .bind(delivery.owner_ref)
    .bind(delivery.stream_name)
    .bind(delivery.consumer_name)
    .bind(delivery.stream_sequence)
    .bind(delivery.delivery_count)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    let identity_matches: bool = sqlx::query_scalar(
        "select subject=$2 and envelope=$3 and stream_name=$4 and consumer_name=$5
         from github_catalog.inbox_events where message_id=$1",
    )
    .bind(delivery.message_id)
    .bind(delivery.subject)
    .bind(delivery.envelope)
    .bind(delivery.stream_name)
    .bind(delivery.consumer_name)
    .fetch_one(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    if !identity_matches {
        return Err(PersistenceError::Query(sqlx::Error::Protocol(
            "inbox message identity was reused with different transport data".to_owned(),
        )));
    }
    let lease_owner = Uuid::now_v7();
    let claimed: Option<Uuid> = sqlx::query_scalar(
        "update github_catalog.inbox_events
         set state='processing',lease_owner=$2,lease_expires_at=$3,failure_code=null,
             attempt_count=attempt_count+1,delivery_count=greatest(delivery_count,$4)
         where message_id=$1 and (state in ('received','retryable')
              or (state='processing' and lease_expires_at <= $5))
         returning message_id",
    )
    .bind(delivery.message_id)
    .bind(lease_owner)
    .bind(now + lease_duration)
    .bind(delivery.delivery_count)
    .bind(now)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    if claimed.is_some() {
        return Ok(InboxClaimOutcome::Claimed { lease_owner });
    }
    let state: String =
        sqlx::query_scalar("select state from github_catalog.inbox_events where message_id=$1")
            .bind(delivery.message_id)
            .fetch_one(database.pool())
            .await
            .map_err(PersistenceError::Query)?;
    Ok(if matches!(state.as_str(), "consumed" | "rejected") {
        InboxClaimOutcome::TerminalDuplicate
    } else {
        InboxClaimOutcome::Busy
    })
}

/// Commits a successful terminal inbox outcome under the still-owned lease.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the lease is lost or persistence fails.
pub async fn complete_inbox_delivery(
    database: &Database,
    message_id: Uuid,
    lease_owner: Uuid,
    outcome: &'static str,
    now: OffsetDateTime,
) -> Result<(), PersistenceError> {
    terminal_update(
        database,
        message_id,
        lease_owner,
        "consumed",
        outcome,
        None,
        now,
    )
    .await
}

/// Commits one redacted permanent rejection so poison delivery can be terminated.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the lease is lost or persistence fails.
pub async fn reject_inbox_delivery(
    database: &Database,
    message_id: Uuid,
    lease_owner: Uuid,
    failure_code: &'static str,
    now: OffsetDateTime,
) -> Result<(), PersistenceError> {
    terminal_update(
        database,
        message_id,
        lease_owner,
        "rejected",
        "rejected",
        Some(failure_code),
        now,
    )
    .await
}

/// Releases a retryable failure for later redelivery.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the lease is lost or persistence fails.
pub async fn retry_inbox_delivery(
    database: &Database,
    message_id: Uuid,
    lease_owner: Uuid,
    failure_code: &'static str,
    retry_at: OffsetDateTime,
) -> Result<(), PersistenceError> {
    let changed = sqlx::query(
        "update github_catalog.inbox_events set state='retryable',lease_owner=null,
         lease_expires_at=null,next_attempt_at=$3,failure_code=$4
         where message_id=$1 and lease_owner=$2 and state='processing'",
    )
    .bind(message_id)
    .bind(lease_owner)
    .bind(retry_at)
    .bind(failure_code)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    require_changed(changed)
}

async fn terminal_update(
    database: &Database,
    message_id: Uuid,
    lease_owner: Uuid,
    state: &str,
    outcome: &str,
    failure_code: Option<&str>,
    now: OffsetDateTime,
) -> Result<(), PersistenceError> {
    let changed = sqlx::query(
        "update github_catalog.inbox_events set state=$3,terminal_outcome=$4,failure_code=$5,
         consumed_at=$6,lease_owner=null,lease_expires_at=null
         where message_id=$1 and lease_owner=$2 and state='processing'",
    )
    .bind(message_id)
    .bind(lease_owner)
    .bind(state)
    .bind(outcome)
    .bind(failure_code)
    .bind(now)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    require_changed(changed)
}

fn require_changed(changed: u64) -> Result<(), PersistenceError> {
    if changed == 1 {
        Ok(())
    } else {
        Err(PersistenceError::Query(sqlx::Error::Protocol(
            "inbox lease is not owned".to_owned(),
        )))
    }
}
