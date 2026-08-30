use ratatoskr_event_envelope::{
    CommandEnvelope, CommandType, EnvelopeSchemaVersion, EventEnvelope, EventPayload, ProducerName,
};
use ratatoskr_identifiers::{CommandId, EntityRef, EventId, Extensions, TenantRef, WireTimestamp};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use std::future::Future;
use uuid::Uuid;

use crate::{Database, PersistenceError};

pub(crate) const ANALYSIS_SUBJECT: &str = "evt.knowledge.repository_analysis.requested.v1";
pub(crate) const POLICY_SUBJECT: &str = "cmd.vault.target.desired.v1";

pub(crate) fn event_bytes<P: EventPayload>(
    message_id: Uuid,
    aggregate_id: EntityRef,
    correlation_id: EntityRef,
    tenant_id: Option<TenantRef>,
    payload: &P,
) -> Result<Vec<u8>, serde_json::Error> {
    let payload = serde_json::to_value(payload)?;
    let serde_json::Value::Object(payload) = payload else {
        return Err(serde_json::Error::io(std::io::Error::other(
            "event payload must be an object",
        )));
    };
    let envelope = EventEnvelope {
        event_id: EventId(message_id),
        event_type: P::event_type(),
        occurred_at: WireTimestamp::now(),
        producer: ProducerName::parse("ratatoskr-github")
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?,
        aggregate_id,
        correlation_id,
        causation_id: None,
        tenant_id,
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload,
        extensions: Extensions::new(),
    };
    envelope
        .to_canonical_json()
        .map(String::into_bytes)
        .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
}

pub(crate) fn policy_command_bytes<P: Serialize>(
    message_id: Uuid,
    payload: &P,
) -> Result<Vec<u8>, serde_json::Error> {
    let payload = serde_json::to_value(payload)?;
    let serde_json::Value::Object(payload) = payload else {
        return Err(serde_json::Error::io(std::io::Error::other(
            "command payload must be an object",
        )));
    };
    let command_id = CommandId(message_id);
    let envelope = CommandEnvelope {
        command_id,
        command_type: CommandType::parse("vault.target.desired.v1")
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?,
        issued_at: WireTimestamp::now(),
        producer: ProducerName::parse("ratatoskr-github")
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?,
        aggregate_id: EntityRef::parse("backup-policy:catalog")
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?,
        correlation_id: command_id.as_entity_ref(),
        causation_id: None,
        tenant_id: None,
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload,
        extensions: Extensions::new(),
    };
    envelope
        .to_canonical_json()
        .map(String::into_bytes)
        .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
}

pub(crate) async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
    subject: &str,
    envelope: &[u8],
    ordering_key: &str,
    owner_ref: Option<&str>,
) -> Result<(), PersistenceError> {
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
        .bind(ordering_key)
        .execute(&mut **tx)
        .await
        .map_err(PersistenceError::Query)?;
    let sequence: i64 = sqlx::query_scalar(
        "select coalesce(max(ordering_sequence), 0) + 1
         from github_catalog.outbox_events where ordering_key = $1",
    )
    .bind(ordering_key)
    .fetch_one(&mut **tx)
    .await
    .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into github_catalog.outbox_events
             (message_id, subject, envelope, ordering_key, ordering_sequence, owner_ref)
         values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(message_id)
    .bind(subject)
    .bind(envelope)
    .bind(ordering_key)
    .bind(sequence)
    .bind(owner_ref)
    .execute(&mut **tx)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}

/// One leased, byte-final message ready for publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedOutboxMessage {
    /// Stable event or command identity and broker deduplication key.
    pub message_id: Uuid,
    /// Classified transport subject.
    pub subject: String,
    /// Exact canonical envelope bytes committed by the domain transaction.
    pub envelope: Vec<u8>,
    /// Aggregate ordering partition.
    pub ordering_key: String,
    /// Monotonic sequence within the ordering partition.
    pub ordering_sequence: i64,
    /// Attempt number including this claim.
    pub attempt_count: i32,
}

/// Stable redacted publication failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxFailureCode {
    /// Broker connection is unavailable.
    BusUnavailable,
    /// Broker refused the classified publication.
    PublishRejected,
    /// Persistence acknowledgement did not arrive before the deadline.
    AckTimeout,
}

/// Narrow publication capability; topology operations are intentionally absent.
pub trait OutboxTransport: Sync {
    /// Publishes exact bytes with the stable message identity as the broker deduplication key.
    fn publish<'a>(
        &'a self,
        subject: &'a str,
        envelope: &'a [u8],
        message_id: Uuid,
    ) -> impl Future<Output = Result<(), OutboxFailureCode>> + Send + 'a;
}

/// Summary of one bounded publication iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxPublishReport {
    /// Rows whose broker persistence acknowledgement was committed.
    pub published: u32,
    /// Rows released or dead-lettered after a classified failure.
    pub failed: u32,
    /// Failed rows that reached the finite attempt ceiling in this iteration.
    pub dead_lettered: u32,
}

impl OutboxFailureCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BusUnavailable => "bus_unavailable",
            Self::PublishRejected => "publish_rejected",
            Self::AckTimeout => "ack_timeout",
        }
    }
}

/// Claims a bounded due batch while preserving strict order within each key.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the claim transaction fails.
pub async fn claim_due_outbox(
    database: &Database,
    lease_owner: Uuid,
    now: time::OffsetDateTime,
    lease_duration: time::Duration,
    batch_size: u32,
) -> Result<Vec<ClaimedOutboxMessage>, PersistenceError> {
    let lease_expires_at = now + lease_duration;
    let rows: Vec<(Uuid, String, Vec<u8>, String, i64, i32)> = sqlx::query_as(
        "with candidates as (
             select candidate.message_id
             from github_catalog.outbox_events candidate
             where candidate.published_at is null
               and candidate.dead_lettered_at is null
               and candidate.next_attempt_at <= $1
               and (candidate.lease_expires_at is null or candidate.lease_expires_at <= $1)
               and not exists (
                   select 1 from github_catalog.outbox_events earlier
                   where earlier.ordering_key = candidate.ordering_key
                     and earlier.ordering_sequence < candidate.ordering_sequence
                     and earlier.published_at is null
               )
             order by candidate.created_at, candidate.message_id
             limit $2 for update skip locked
         )
         update github_catalog.outbox_events outbox
         set lease_owner = $3, lease_expires_at = $4,
             attempt_count = outbox.attempt_count + 1
         from candidates where outbox.message_id = candidates.message_id
         returning outbox.message_id, outbox.subject, outbox.envelope,
                   outbox.ordering_key, outbox.ordering_sequence, outbox.attempt_count",
    )
    .bind(now)
    .bind(i64::from(batch_size))
    .bind(lease_owner)
    .bind(lease_expires_at)
    .fetch_all(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(rows
        .into_iter()
        .map(
            |(message_id, subject, envelope, ordering_key, ordering_sequence, attempt_count)| {
                ClaimedOutboxMessage {
                    message_id,
                    subject,
                    envelope,
                    ordering_key,
                    ordering_sequence,
                    attempt_count,
                }
            },
        )
        .collect())
}

/// Marks a still-owned row published after the broker persistence acknowledgement.
///
/// # Errors
///
/// Returns [`PersistenceError`] when persistence fails or the lease is no longer owned.
pub async fn confirm_outbox_published(
    database: &Database,
    message_id: Uuid,
    lease_owner: Uuid,
    now: time::OffsetDateTime,
) -> Result<bool, PersistenceError> {
    let dead_lettered: Option<bool> = sqlx::query_scalar(
        "update github_catalog.outbox_events
         set published_at = $3, lease_owner = null, lease_expires_at = null, failure_code = null
         where message_id = $1 and lease_owner = $2 and published_at is null
           and dead_lettered_at is null
         returning dead_lettered_at is not null",
    )
    .bind(message_id)
    .bind(lease_owner)
    .bind(now)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    dead_lettered.ok_or_else(|| {
        PersistenceError::Query(sqlx::Error::Protocol(
            "outbox lease is not owned".to_owned(),
        ))
    })
}

/// Releases or dead-letters a still-owned failed publication.
///
/// # Errors
///
/// Returns [`PersistenceError`] when persistence fails or the lease is no longer owned.
pub async fn fail_outbox_publication(
    database: &Database,
    message_id: Uuid,
    lease_owner: Uuid,
    now: time::OffsetDateTime,
    retry_at: time::OffsetDateTime,
    max_attempts: i32,
    failure: OutboxFailureCode,
) -> Result<bool, PersistenceError> {
    let dead_lettered: Option<bool> = sqlx::query_scalar(
        "update github_catalog.outbox_events
         set lease_owner = null, lease_expires_at = null,
             next_attempt_at = case when attempt_count < $4 then $5 else next_attempt_at end,
             dead_lettered_at = case when attempt_count >= $4 then $3 else null end,
             failure_code = $6
         where message_id = $1 and lease_owner = $2 and published_at is null
           and dead_lettered_at is null
         returning dead_lettered_at is not null",
    )
    .bind(message_id)
    .bind(lease_owner)
    .bind(now)
    .bind(max_attempts)
    .bind(retry_at)
    .bind(failure.as_str())
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    dead_lettered.ok_or_else(|| {
        PersistenceError::Query(sqlx::Error::Protocol(
            "outbox lease is not owned".to_owned(),
        ))
    })
}

/// Requeues exactly one unpublished dead-letter without changing its wire identity or bytes.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the row does not exist in the required state or persistence
/// fails.
pub async fn requeue_dead_letter(
    database: &Database,
    message_id: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), PersistenceError> {
    let changed = sqlx::query(
        "update github_catalog.outbox_events
         set dead_lettered_at = null, failure_code = null, attempt_count = 0,
             next_attempt_at = $2, lease_owner = null, lease_expires_at = null
         where message_id = $1 and published_at is null and dead_lettered_at is not null",
    )
    .bind(message_id)
    .bind(now)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if changed == 1 {
        Ok(())
    } else {
        Err(PersistenceError::Query(sqlx::Error::Protocol(
            "outbox row is not an unpublished dead letter".to_owned(),
        )))
    }
}

/// Claims and publishes one bounded batch, continuing after an unrelated-key failure.
///
/// # Errors
///
/// Returns [`PersistenceError`] when claim or state persistence fails. Transport failures are
/// durably classified in their rows and reported in the returned counters.
#[expect(
    clippy::too_many_arguments,
    reason = "one publication iteration receives every finite lease and retry boundary explicitly"
)]
pub async fn publish_outbox_batch<T: OutboxTransport>(
    database: &Database,
    transport: &T,
    lease_owner: Uuid,
    now: time::OffsetDateTime,
    lease_duration: time::Duration,
    batch_size: u32,
    max_attempts: i32,
    retry_delay: time::Duration,
) -> Result<OutboxPublishReport, PersistenceError> {
    let claims = claim_due_outbox(database, lease_owner, now, lease_duration, batch_size).await?;
    let mut report = OutboxPublishReport {
        published: 0,
        failed: 0,
        dead_lettered: 0,
    };
    for row in claims {
        match transport
            .publish(&row.subject, &row.envelope, row.message_id)
            .await
        {
            Ok(()) => {
                confirm_outbox_published(database, row.message_id, lease_owner, now).await?;
                report.published = report.published.saturating_add(1);
            }
            Err(failure) => {
                let dead_lettered = fail_outbox_publication(
                    database,
                    row.message_id,
                    lease_owner,
                    now,
                    now + retry_delay,
                    max_attempts,
                    failure,
                )
                .await?;
                report.failed = report.failed.saturating_add(1);
                if dead_lettered {
                    report.dead_lettered = report.dead_lettered.saturating_add(1);
                }
            }
        }
    }
    Ok(report)
}
