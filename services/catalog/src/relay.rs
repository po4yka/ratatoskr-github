//! The outbox relay and the dispatch loops (XR-021 CONTRACTS.md S02, R2-03 and R2-04 rule 2).
//!
//! Every tick the relay moves due catalog work into the outbox, publishes every due row and sets
//! `published_at` only after the `PubAck`. It reports its health to [`Lifecycle`] so a relay that
//! keeps running but cannot publish (an ACL drift, a refused subject, an unreachable broker) turns
//! readiness off instead of leaving the service silently ready. An envelope larger than the
//! broker's payload limit can never be published, so it is classed undeliverable at once and never
//! counts against readiness.

use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_github_catalog::{
    AnalysisDispatch, Database, dispatch_due_repository_analysis, publish_due_backup_policy,
};
use time::OffsetDateTime;
use tokio::sync::watch;

use crate::Lifecycle;
use crate::bus::BusError;

const REQUESTED_SUBJECT: &str = "evt.knowledge.repository_analysis.requested.v1";
const APPLY_SUBJECT: &str = "cmd.vault.backup_policy.apply_requested.v1";
/// The closed list of subjects the relay publishes (S02 rule 2). An outbox subject outside it is a
/// programming error: the relay stops with [`BusError::UnknownSubject`] and never skips the row.
const RELAYED_SUBJECTS: [&str; 2] = [REQUESTED_SUBJECT, APPLY_SUBJECT];

const TICK: Duration = Duration::from_secs(1);
const PUBACK_TIMEOUT: Duration = Duration::from_secs(5);
const RELAY_BATCH: i64 = 32;
const DISPATCH_BATCH: u32 = 64;
/// Room the broker keeps for the subject and headers of a message (R2-04 rule 2).
const PAYLOAD_HEADROOM: usize = 1024;
/// The oldest unpublished row is stuck after this many failed attempts or this many seconds.
const STUCK_ATTEMPTS: i32 = 3;
const STUCK_AGE_SECONDS: f64 = 300.0;

pub(crate) async fn relay_loop(
    database: Database,
    context: jetstream::Context,
    lifecycle: Lifecycle,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), BusError> {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // cancel-safe: Interval::tick and watch::Receiver::wait_for retain no partial work.
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|stopping| *stopping) => return Ok(()),
            _ = ticker.tick() => {}
        }
        dispatch_due(&database).await?;
        flush_outbox(&database, &context, &lifecycle).await?;
    }
}

/// Moves due catalog work into the outbox: queued analysis requests and the dirty backup policy.
async fn dispatch_due(database: &Database) -> Result<(), BusError> {
    for _ in 0..DISPATCH_BATCH {
        let dispatched = dispatch_due_repository_analysis(database, OffsetDateTime::now_utc())
            .await
            .map_err(|_| BusError::Catalog)?;
        if dispatched == AnalysisDispatch::NotDue {
            break;
        }
    }
    publish_due_backup_policy(database, OffsetDateTime::now_utc())
        .await
        .map_err(|_| BusError::Catalog)?;
    Ok(())
}

/// What one flush pass attempted and how it went.
#[derive(Debug, Default, Clone, Copy)]
struct FlushTally {
    attempted: u32,
    failed: u32,
}

/// Publishes every due outbox row and sets `published_at` only after the `PubAck` resolves.
///
/// A publish failure leaves the row unpublished, records the attempt with a safe error class and
/// backs the row off, so a failing head row never starves later rows. A denied publish is
/// indistinguishable from a timeout here: check the NATS server log for a Publish Violation.
async fn flush_outbox(
    database: &Database,
    context: &jetstream::Context,
    lifecycle: &Lifecycle,
) -> Result<(), BusError> {
    let max_payload = context.client().server_info().max_payload;
    let rows: Vec<(uuid::Uuid, String, serde_json::Value)> = sqlx::query_as(
        "select message_id, subject, payload from github_catalog.outbox_events
         where published_at is null and undeliverable_at is null and next_attempt_at <= now()
         order by created_at, message_id limit $1",
    )
    .bind(RELAY_BATCH)
    .fetch_all(database.pool())
    .await
    .map_err(|_| BusError::Catalog)?;
    let mut tally = FlushTally::default();
    for (message_id, subject, payload) in rows {
        if !RELAYED_SUBJECTS.contains(&subject.as_str()) {
            return Err(BusError::UnknownSubject);
        }
        let bytes = serde_json::to_vec(&payload).map_err(|_| BusError::Catalog)?;
        if max_payload > 0 && bytes.len() > max_payload.saturating_sub(PAYLOAD_HEADROOM) {
            mark_undeliverable(database, lifecycle, message_id).await?;
            continue;
        }
        tally.attempted += 1;
        match publish(context, &subject, message_id, bytes).await {
            Ok(()) => {
                lifecycle.record_publication(true);
                sqlx::query(
                    "update github_catalog.outbox_events
                     set published_at = now(), last_error = null
                     where message_id = $1 and published_at is null",
                )
                .bind(message_id)
                .execute(database.pool())
                .await
                .map_err(|_| BusError::Catalog)?;
            }
            Err(class) => {
                tally.failed += 1;
                lifecycle.record_publication(false);
                tracing::warn!(
                    class,
                    "outbox row was not acknowledged by the bus; check the NATS server log for a Publish Violation"
                );
                sqlx::query(
                    "update github_catalog.outbox_events
                     set attempt_count = attempt_count + 1, last_error = $2,
                         next_attempt_at = now()
                             + make_interval(secs => least(300, power(2, attempt_count + 1))::float8)
                     where message_id = $1 and published_at is null",
                )
                .bind(message_id)
                .bind(class)
                .execute(database.pool())
                .await
                .map_err(|_| BusError::Catalog)?;
            }
        }
    }
    report_health(database, lifecycle, tally).await
}

/// Classes a row whose envelope the broker can never accept; the pass continues with the next one.
async fn mark_undeliverable(
    database: &Database,
    lifecycle: &Lifecycle,
    message_id: uuid::Uuid,
) -> Result<(), BusError> {
    sqlx::query(
        "update github_catalog.outbox_events
         set undeliverable_at = now(), last_error = 'payload_too_large'
         where message_id = $1 and published_at is null and undeliverable_at is null",
    )
    .bind(message_id)
    .execute(database.pool())
    .await
    .map_err(|_| BusError::Catalog)?;
    lifecycle.record_undeliverable_payload_too_large();
    tracing::warn!(
        class = "payload_too_large",
        "outbox row is larger than the broker payload limit and can never be published"
    );
    Ok(())
}

/// Feeds the `bus_publish` readiness check (R2-03): unhealthy when the pass attempted a publish,
/// one failed, and the oldest unpublished publishable row is stuck; healthy again once the oldest
/// row is no longer stuck, or nothing publishable waits at all. A pass that only finds rows
/// backing off keeps the earlier verdict, so readiness does not flap between retries.
async fn report_health(
    database: &Database,
    lifecycle: &Lifecycle,
    tally: FlushTally,
) -> Result<(), BusError> {
    let oldest_stuck: Option<bool> = sqlx::query_scalar(
        "select attempt_count >= $1 or extract(epoch from now() - created_at) > $2
         from github_catalog.outbox_events
         where published_at is null and undeliverable_at is null
         order by created_at, message_id limit 1",
    )
    .bind(STUCK_ATTEMPTS)
    .bind(STUCK_AGE_SECONDS)
    .fetch_optional(database.pool())
    .await
    .map_err(|_| BusError::Catalog)?;
    match (tally.attempted, tally.failed, oldest_stuck) {
        (_, _, None) | (_, 0, Some(false)) => lifecycle.set_publisher_healthy(true),
        (_, failed, Some(stuck)) if failed > 0 => lifecycle.set_publisher_healthy(!stuck),
        _ => {}
    }
    Ok(())
}

/// Publishes one row and waits for its `PubAck`; the error is a safe class, never broker text.
async fn publish(
    context: &jetstream::Context,
    subject: &str,
    message_id: uuid::Uuid,
    bytes: Vec<u8>,
) -> Result<(), &'static str> {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", message_id.to_string());
    let pending = context
        .publish_with_headers(subject.to_owned(), headers, bytes.into())
        .await
        .map_err(|_| "publish_failed")?;
    match tokio::time::timeout(PUBACK_TIMEOUT, pending).await {
        Ok(Ok(_ack)) => Ok(()),
        Ok(Err(_)) => Err("ack_failed"),
        Err(_) => Err("ack_timeout"),
    }
}
