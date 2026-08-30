//! Supervised seven-worker fleet-bus runtime.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, AckKind};
use futures_util::StreamExt as _;
use ratatoskr_backup_contracts::PolicyAcknowledged;
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_github_catalog::{
    Config, Database, InboxClaimOutcome, InboxDelivery, SyncCommandError, claim_inbox_delivery,
    consume_repository_analysis_completed_delivery, consume_repository_analysis_failed_delivery,
    dispatch_due_repository_analysis, handle_authenticated_sync_delivery,
    publish_due_backup_policy, publish_outbox_batch, record_backup_policy_acknowledgment_delivery,
    reject_inbox_delivery,
};
use ratatoskr_github_contracts::{RepositoryAnalysisCompleted, RepositoryAnalysisFailed};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{CONSUMERS, ConsumerSpec, FleetBus, Lifecycle};

const WORKER_COUNT: u8 = 7;

/// A redacted runtime failure that makes readiness false and stops the process.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Serving configuration became unavailable after role validation.
    #[error("the fleet-bus serving configuration is invalid")]
    Config(#[from] ratatoskr_github_catalog::ConfigError),
    /// Bus connection or topology verification failed.
    #[error(transparent)]
    Bus(#[from] crate::BusError),
    /// A fixed durable could not start or ended unexpectedly.
    #[error("a fixed GitHub fleet-bus consumer stopped")]
    Consumer,
    /// A supervised worker returned unexpectedly.
    #[error("a supervised GitHub fleet-bus worker stopped")]
    WorkerStopped,
    /// A supervised worker panicked or was cancelled.
    #[error("a supervised GitHub fleet-bus worker failed")]
    WorkerJoin(#[source] tokio::task::JoinError),
    /// Workers did not finish within the configured join bound.
    #[error("GitHub fleet-bus workers did not stop within the shutdown bound")]
    JoinTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryDecision {
    Ack,
    Retry,
    Reject,
}

/// Connects, verifies, and supervises one publisher, four consumers, and two due workers.
///
/// The function returns only after cancellation has stopped new claims, every worker has joined,
/// and the NATS connection has drained. The caller therefore remains responsible for closing the
/// database after this future completes.
///
/// # Errors
///
/// Returns [`RuntimeError`] for startup drift, unexpected worker exit, join timeout, or bus drain
/// failure.
pub async fn run_fleet_bus_runtime(
    config: Config,
    database: Database,
    lifecycle: Lifecycle,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), RuntimeError> {
    let connect_timeout = Duration::from_millis(config.bus.connect_timeout_ms);
    let ack_timeout = Duration::from_millis(config.bus.publish_ack_timeout_ms);
    let bus = FleetBus::connect(
        &config.bus.url,
        Path::new(config.bus.nkey_seed_path()?),
        connect_timeout,
        ack_timeout,
    )
    .await?;
    lifecycle.set_bus_ready(bus.is_connected());
    bus.verify_topology().await?;
    lifecycle.set_topology_ready(true);

    let mut consumers = Vec::with_capacity(CONSUMERS.len());
    for spec in CONSUMERS {
        consumers.push((spec, bus.consumer(spec).await?));
    }

    let (worker_stop_tx, worker_stop_rx) = tokio::sync::watch::channel(false);
    let mut workers = JoinSet::new();
    let config = Arc::new(config);
    workers.spawn(publisher_worker(
        database.clone(),
        bus.clone(),
        lifecycle.clone(),
        worker_stop_rx.clone(),
        Arc::clone(&config),
    ));
    for (spec, consumer) in consumers {
        workers.spawn(consumer_worker(
            database.clone(),
            consumer,
            spec,
            lifecycle.clone(),
            worker_stop_rx.clone(),
            Arc::clone(&config),
        ));
    }
    workers.spawn(analysis_worker(
        database.clone(),
        lifecycle.clone(),
        worker_stop_rx.clone(),
        Duration::from_millis(config.bus.poll_interval_ms),
    ));
    workers.spawn(policy_worker(
        database,
        lifecycle.clone(),
        worker_stop_rx,
        Duration::from_millis(config.bus.poll_interval_ms),
    ));
    lifecycle.set_live_workers(WORKER_COUNT);
    lifecycle.mark_serving();

    let topology_probe =
        Duration::from_millis(config.bus.poll_interval_ms).max(Duration::from_secs(5));
    let mut probe = tokio::time::interval(topology_probe);
    probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let runtime_outcome = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow_and_update() {
                    break Ok(());
                }
            }
            joined = workers.join_next() => {
                lifecycle.set_live_workers(WORKER_COUNT.saturating_sub(1));
                break match joined {
                    Some(Ok(Err(error))) => Err(error),
                    Some(Err(error)) => Err(RuntimeError::WorkerJoin(error)),
                    Some(Ok(Ok(()))) | None => Err(RuntimeError::WorkerStopped),
                };
            }
            _ = probe.tick() => {
                let connected = bus.is_connected();
                lifecycle.set_bus_ready(connected);
                if connected {
                    lifecycle.set_topology_ready(bus.verify_topology().await.is_ok());
                } else {
                    lifecycle.set_topology_ready(false);
                }
            }
        }
    };

    lifecycle.begin_drain();
    let _ignored = worker_stop_tx.send(true);
    let join_timeout = Duration::from_millis(config.bus.worker_join_timeout_ms);
    tokio::time::timeout(join_timeout, async {
        while let Some(joined) = workers.join_next().await {
            if let Err(error) = joined {
                tracing::warn!(%error, "a fleet-bus worker did not join cleanly");
            }
        }
    })
    .await
    .map_err(|_| RuntimeError::JoinTimeout)?;
    lifecycle.set_live_workers(0);
    lifecycle.set_bus_ready(false);
    lifecycle.set_topology_ready(false);
    bus.drain().await?;
    runtime_outcome
}

async fn publisher_worker(
    database: Database,
    bus: FleetBus,
    lifecycle: Lifecycle,
    mut stop: tokio::sync::watch::Receiver<bool>,
    config: Arc<Config>,
) -> Result<(), RuntimeError> {
    let poll = Duration::from_millis(config.bus.poll_interval_ms);
    let lease =
        time::Duration::milliseconds(i64::try_from(config.bus.lease_ms).unwrap_or(i64::MAX));
    let retry = time::Duration::milliseconds(
        i64::try_from(config.bus.poll_interval_ms).unwrap_or(i64::MAX),
    );
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        match publish_outbox_batch(
            &database,
            &bus,
            Uuid::now_v7(),
            OffsetDateTime::now_utc(),
            lease,
            config.bus.batch_size,
            config.bus.max_attempts,
            retry,
        )
        .await
        {
            Ok(report) => {
                for _ in 0..report.failed {
                    lifecycle.record_retry();
                }
                for _ in 0..report.dead_lettered {
                    lifecycle.record_dead_letter();
                }
            }
            Err(error) => {
                lifecycle.record_retry();
                tracing::error!(%error, "the outbox publication iteration failed");
            }
        }
        wait_tick(&mut stop, poll).await;
    }
}

async fn consumer_worker(
    database: Database,
    consumer: jetstream::consumer::PullConsumer,
    spec: ConsumerSpec,
    lifecycle: Lifecycle,
    mut stop: tokio::sync::watch::Receiver<bool>,
    config: Arc<Config>,
) -> Result<(), RuntimeError> {
    let mut messages = consumer
        .stream()
        .max_messages_per_batch(config.bus.batch_size as usize)
        .expires(Duration::from_millis(config.bus.poll_interval_ms))
        .messages()
        .await
        .map_err(|_| RuntimeError::Consumer)?;
    loop {
        let next = tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow_and_update() {
                    return Ok(());
                }
                continue;
            }
            next = messages.next() => next,
        };
        let Some(message) = next else {
            return Err(RuntimeError::Consumer);
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                lifecycle.record_retry();
                tracing::warn!(%error, durable = spec.durable, "a durable fetch failed");
                continue;
            }
        };
        let decision = process_delivery(&database, &message, spec, &lifecycle, &config).await;
        acknowledge(&message, decision, &lifecycle, &config).await;
    }
}

async fn process_delivery(
    database: &Database,
    message: &jetstream::Message,
    spec: ConsumerSpec,
    lifecycle: &Lifecycle,
    config: &Config,
) -> DeliveryDecision {
    if message.subject.as_str() != spec.subject {
        lifecycle.record_rejection();
        return DeliveryDecision::Reject;
    }
    let info = match message.info() {
        Ok(info) => info,
        Err(error) => {
            lifecycle.record_retry();
            tracing::warn!(%error, "a durable delivery lacked JetStream coordinates");
            return DeliveryDecision::Retry;
        }
    };
    let Ok(stream_sequence) = i64::try_from(info.stream_sequence) else {
        return reject_malformed(database, message, spec, &info, lifecycle, config).await;
    };
    let Ok(delivery_count) = i32::try_from(info.delivered) else {
        return reject_malformed(database, message, spec, &info, lifecycle, config).await;
    };
    let lease = time::Duration::milliseconds(i64_bound(config.bus.lease_ms));
    let retry = time::Duration::milliseconds(i64_bound(config.bus.poll_interval_ms));
    match spec.subject {
        "cmd.github.sync.requested.v1" => {
            let Some(message_id) = envelope_identity(&message.payload) else {
                return reject_malformed(database, message, spec, &info, lifecycle, config).await;
            };
            let owner = command_owner(&message.payload);
            let delivery = InboxDelivery {
                message_id,
                subject: spec.subject,
                envelope: &message.payload,
                stream_name: spec.stream,
                consumer_name: spec.durable,
                stream_sequence,
                delivery_count,
                owner_ref: owner.as_deref(),
            };
            let Ok(key) = config.credentials.encryption_key() else {
                return DeliveryDecision::Retry;
            };
            match handle_authenticated_sync_delivery(
                database,
                &config.provider.base_url,
                &key,
                &delivery,
                lease,
                retry,
            )
            .await
            {
                Ok(ratatoskr_github_catalog::ConsumedSyncCommand::Duplicate) => {
                    lifecycle.record_duplicate();
                    DeliveryDecision::Ack
                }
                Ok(ratatoskr_github_catalog::ConsumedSyncCommand::Handled(_)) => {
                    DeliveryDecision::Ack
                }
                Err(
                    SyncCommandError::Invalid(_)
                    | SyncCommandError::UnknownAccount
                    | SyncCommandError::AccountNotConnected,
                ) => reject_claimed(database, &delivery, lifecycle, config).await,
                Err(_) => {
                    lifecycle.record_retry();
                    DeliveryDecision::Retry
                }
            }
        }
        _ => process_event_delivery(database, message, spec, &info, lifecycle, config).await,
    }
}

async fn process_event_delivery(
    database: &Database,
    message: &jetstream::Message,
    spec: ConsumerSpec,
    info: &jetstream::message::Info<'_>,
    lifecycle: &Lifecycle,
    config: &Config,
) -> DeliveryDecision {
    let Ok(envelope) = EventEnvelope::from_json(&message.payload) else {
        return reject_malformed(database, message, spec, info, lifecycle, config).await;
    };
    let message_id = envelope.event_id.0;
    let owner = envelope.tenant_id.map(|owner| owner.to_string());
    let Ok(stream_sequence) = i64::try_from(info.stream_sequence) else {
        return reject_malformed(database, message, spec, info, lifecycle, config).await;
    };
    let Ok(delivery_count) = i32::try_from(info.delivered) else {
        return reject_malformed(database, message, spec, info, lifecycle, config).await;
    };
    let delivery = InboxDelivery {
        message_id,
        subject: spec.subject,
        envelope: &message.payload,
        stream_name: spec.stream,
        consumer_name: spec.durable,
        stream_sequence,
        delivery_count,
        owner_ref: owner.as_deref(),
    };
    let now = OffsetDateTime::now_utc();
    let lease = time::Duration::milliseconds(i64_bound(config.bus.lease_ms));
    let retry = time::Duration::milliseconds(i64_bound(config.bus.poll_interval_ms));
    match spec.subject {
        "evt.knowledge.repository_analysis.completed.v1" => {
            let Ok(payload) = envelope.payload_as::<RepositoryAnalysisCompleted>() else {
                return reject_claimed(database, &delivery, lifecycle, config).await;
            };
            let outcome = consume_repository_analysis_completed_delivery(
                database, &delivery, &payload, now, lease, retry,
            )
            .await;
            terminal_decision(&outcome, lifecycle)
        }
        "evt.knowledge.repository_analysis.failed.v1" => {
            let Ok(payload) = envelope.payload_as::<RepositoryAnalysisFailed>() else {
                return reject_claimed(database, &delivery, lifecycle, config).await;
            };
            let outcome = consume_repository_analysis_failed_delivery(
                database, &delivery, &payload, now, lease, retry,
            )
            .await;
            terminal_decision(&outcome, lifecycle)
        }
        "evt.vault.backup_policy.acknowledged.v1" => {
            let Ok(payload) = envelope.payload_as::<PolicyAcknowledged>() else {
                return reject_claimed(database, &delivery, lifecycle, config).await;
            };
            match record_backup_policy_acknowledgment_delivery(
                database, &delivery, &payload, now, lease,
            )
            .await
            {
                Ok(ratatoskr_github_catalog::FeedbackOutcome::Duplicate) => {
                    lifecycle.record_duplicate();
                    DeliveryDecision::Ack
                }
                Ok(ratatoskr_github_catalog::FeedbackOutcome::Recorded) => DeliveryDecision::Ack,
                Err(_) => {
                    lifecycle.record_retry();
                    DeliveryDecision::Retry
                }
            }
        }
        _ => reject_claimed(database, &delivery, lifecycle, config).await,
    }
}

fn terminal_decision(
    outcome: &Result<
        ratatoskr_github_catalog::TerminalFactOutcome,
        ratatoskr_github_catalog::WatchError,
    >,
    lifecycle: &Lifecycle,
) -> DeliveryDecision {
    match outcome {
        Ok(ratatoskr_github_catalog::TerminalFactOutcome::Duplicate) => {
            lifecycle.record_duplicate();
            DeliveryDecision::Ack
        }
        Ok(
            ratatoskr_github_catalog::TerminalFactOutcome::Resolved
            | ratatoskr_github_catalog::TerminalFactOutcome::Ignored,
        ) => DeliveryDecision::Ack,
        Err(_) => {
            lifecycle.record_retry();
            DeliveryDecision::Retry
        }
    }
}

async fn reject_malformed(
    database: &Database,
    message: &jetstream::Message,
    spec: ConsumerSpec,
    info: &jetstream::message::Info<'_>,
    lifecycle: &Lifecycle,
    config: &Config,
) -> DeliveryDecision {
    let message_id = envelope_identity(&message.payload)
        .unwrap_or_else(|| deterministic_message_id(spec.subject, &message.payload));
    let stream_sequence = i64::try_from(info.stream_sequence).unwrap_or(i64::MAX);
    let delivery_count = i32::try_from(info.delivered).unwrap_or(i32::MAX);
    let delivery = InboxDelivery {
        message_id,
        subject: spec.subject,
        envelope: &message.payload,
        stream_name: spec.stream,
        consumer_name: spec.durable,
        stream_sequence,
        delivery_count,
        owner_ref: None,
    };
    reject_claimed(database, &delivery, lifecycle, config).await
}

async fn reject_claimed(
    database: &Database,
    delivery: &InboxDelivery<'_>,
    lifecycle: &Lifecycle,
    config: &Config,
) -> DeliveryDecision {
    let now = OffsetDateTime::now_utc();
    let lease = time::Duration::milliseconds(i64_bound(config.bus.lease_ms));
    match claim_inbox_delivery(database, delivery, now, lease).await {
        Ok(InboxClaimOutcome::Claimed { lease_owner }) => {
            if reject_inbox_delivery(
                database,
                delivery.message_id,
                lease_owner,
                "invalid_owned_delivery",
                now,
            )
            .await
            .is_ok()
            {
                lifecycle.record_rejection();
                DeliveryDecision::Reject
            } else {
                lifecycle.record_retry();
                DeliveryDecision::Retry
            }
        }
        Ok(InboxClaimOutcome::TerminalDuplicate) => {
            lifecycle.record_duplicate();
            DeliveryDecision::Reject
        }
        Ok(InboxClaimOutcome::Busy) | Err(_) => {
            lifecycle.record_retry();
            DeliveryDecision::Retry
        }
    }
}

async fn acknowledge(
    message: &jetstream::Message,
    decision: DeliveryDecision,
    lifecycle: &Lifecycle,
    config: &Config,
) {
    let result = match decision {
        DeliveryDecision::Ack => message.ack_with(AckKind::Ack).await,
        DeliveryDecision::Retry => {
            message
                .ack_with(AckKind::Nak(Some(Duration::from_millis(
                    config.bus.poll_interval_ms,
                ))))
                .await
        }
        DeliveryDecision::Reject => message.ack_with(AckKind::Term).await,
    };
    if let Err(error) = result {
        lifecycle.record_retry();
        tracing::warn!(%error, "a durable delivery acknowledgement failed");
    }
}

async fn analysis_worker(
    database: Database,
    lifecycle: Lifecycle,
    mut stop: tokio::sync::watch::Receiver<bool>,
    poll: Duration,
) -> Result<(), RuntimeError> {
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        if let Err(error) =
            dispatch_due_repository_analysis(&database, OffsetDateTime::now_utc()).await
        {
            lifecycle.record_retry();
            tracing::error!(%error, "the due analysis iteration failed");
        }
        wait_tick(&mut stop, poll).await;
    }
}

async fn policy_worker(
    database: Database,
    lifecycle: Lifecycle,
    mut stop: tokio::sync::watch::Receiver<bool>,
    poll: Duration,
) -> Result<(), RuntimeError> {
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        if let Err(error) = publish_due_backup_policy(&database, OffsetDateTime::now_utc()).await {
            lifecycle.record_retry();
            tracing::error!(%error, "the due policy iteration failed");
        }
        wait_tick(&mut stop, poll).await;
    }
}

async fn wait_tick(stop: &mut tokio::sync::watch::Receiver<bool>, poll: Duration) {
    tokio::select! {
        () = tokio::time::sleep(poll) => {}
        _ = stop.changed() => {}
    }
}

fn envelope_identity(bytes: &[u8]) -> Option<Uuid> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value
        .get("event_id")
        .or_else(|| value.get("command_id"))
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn command_owner(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value
        .get("payload")?
        .get("account")?
        .as_str()
        .map(str::to_owned)
}

fn deterministic_message_id(subject: &str, bytes: &[u8]) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(subject.as_bytes());
    digest.update([0]);
    digest.update(bytes);
    let digest = digest.finalize();
    let mut id = [0_u8; 16];
    if let Some(prefix) = digest.get(..16) {
        id.copy_from_slice(prefix);
    }
    Uuid::from_bytes(id)
}

fn i64_bound(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
