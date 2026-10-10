//! `JetStream` relay, result consumers and dispatch loops (XR-021 CONTRACTS.md S02, S04, S09).
//!
//! The GitHub Catalog publishes two subjects and consumes three Edge-provisioned durables. Every
//! task is supervised: when one returns before an orderly shutdown the supervisor returns an
//! error, so the process marks itself not ready and exits non-zero instead of logging and
//! staying ready.

use std::path::Path;
use std::time::Duration;

use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer};
use async_nats::jetstream::{self, AckKind};
use futures_util::StreamExt as _;
use ratatoskr_backup_contracts::PolicyAcknowledged;
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_github_catalog::{
    AnalysisDispatch, BackupPolicyError, Database, WatchError,
    consume_repository_analysis_completed, consume_repository_analysis_failed,
    dispatch_due_repository_analysis, publish_due_backup_policy,
    record_backup_policy_acknowledgment,
};
use ratatoskr_github_contracts::{RepositoryAnalysisCompleted, RepositoryAnalysisFailed};
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::Lifecycle;

/// The events stream every GitHub subject lives on, except the Vault command.
const EVENTS_STREAM: &str = "ratatoskr_events";
/// Producer allowed to publish repository analysis results.
const KNOWLEDGE_PRODUCER: &str = "ratatoskr-knowledge";
/// Producer allowed to publish the Vault policy acknowledgement.
const VAULT_PRODUCER: &str = "ratatoskr-vault";

const REQUESTED_SUBJECT: &str = "evt.knowledge.repository_analysis.requested.v1";
const APPLY_SUBJECT: &str = "cmd.vault.backup_policy.apply_requested.v1";
/// The closed list of subjects the relay publishes (S02 rule 2). An outbox subject outside it is a
/// programming error: the relay stops with [`BusError::UnknownSubject`] and never skips the row.
const RELAYED_SUBJECTS: [&str; 2] = [REQUESTED_SUBJECT, APPLY_SUBJECT];

const TICK: Duration = Duration::from_secs(1);
const PUBACK_TIMEOUT: Duration = Duration::from_secs(5);
const NAK_DELAY: Duration = Duration::from_secs(2);
const RELAY_BATCH: i64 = 32;
const DISPATCH_BATCH: u32 = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// One Edge-provisioned durable this service verifies and never creates.
#[derive(Debug, Clone, Copy)]
struct DurableSpec {
    stream: &'static str,
    durable: &'static str,
    filter: &'static str,
    ack_wait: Duration,
}

const ANALYSIS_COMPLETED: DurableSpec = DurableSpec {
    stream: EVENTS_STREAM,
    durable: "ratatoskr_github_analysis_completed",
    filter: "evt.knowledge.repository_analysis.completed.v1",
    ack_wait: Duration::from_secs(30),
};
const ANALYSIS_FAILED: DurableSpec = DurableSpec {
    stream: EVENTS_STREAM,
    durable: "ratatoskr_github_analysis_failed",
    filter: "evt.knowledge.repository_analysis.failed.v1",
    ack_wait: Duration::from_secs(30),
};
const POLICY_ACKNOWLEDGED: DurableSpec = DurableSpec {
    stream: EVENTS_STREAM,
    durable: "ratatoskr_github_policy_acknowledged",
    filter: "evt.vault.backup_policy.acknowledged.v1",
    ack_wait: Duration::from_secs(30),
};

/// A bus failure that stops the service. No variant carries an endpoint, a seed or content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BusError {
    /// The nkey seed file could not be read.
    #[error("the bus nkey seed could not be read")]
    Seed,
    /// The broker could not be reached or refused the identity.
    #[error("the bus connection could not be established")]
    Connect,
    /// A pre-provisioned durable is missing or differs from the fixed contract.
    #[error("the bus durable {durable} is missing or differs from the fixed contract")]
    Durable {
        /// Name of the durable that failed verification.
        durable: &'static str,
    },
    /// A durable's message stream failed or ended.
    #[error("the bus consumer stream failed")]
    Consume,
    /// An acknowledgement could not be delivered.
    #[error("the bus acknowledgement failed")]
    Ack,
    /// The broker connection was lost.
    #[error("the bus connection was lost")]
    ConnectionLost,
    /// A catalog operation failed.
    #[error("a catalog operation failed in the bus worker")]
    Catalog,
    /// An outbox row carries a subject the relay does not publish.
    #[error("an outbox row carries a subject outside the relayed list")]
    UnknownSubject,
    /// A supervised task stopped, or panicked, before shutdown.
    #[error("a supervised bus task stopped before shutdown")]
    TaskStopped,
}

/// Where the broker is and how this service authenticates to it.
#[derive(Debug, Clone)]
pub struct BusSettings {
    /// Broker URL, `nats://` for a local broker or `tls://` with an nkey seed.
    pub url: String,
    /// Absolute path of the nkey seed file; read at connect time and never logged.
    pub nkey_seed_path: Option<std::path::PathBuf>,
}

/// A connected, verified bus: the client plus the three result durables.
#[derive(Debug)]
pub struct Bus {
    context: jetstream::Context,
    completed: PullConsumer,
    failed: PullConsumer,
    acknowledged: PullConsumer,
    connection_lost: watch::Receiver<bool>,
}

impl Bus {
    /// Connects and verifies the three durables against the fixed contract of S04.
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] when the seed cannot be read, the broker refuses the connection, or a
    /// durable is missing or differs from its fixed specification.
    pub async fn connect(settings: &BusSettings) -> Result<Self, BusError> {
        let (lost_tx, connection_lost) = watch::channel(false);
        let options = match &settings.nkey_seed_path {
            Some(path) => async_nats::ConnectOptions::with_nkey(read_seed(path).await?),
            None => async_nats::ConnectOptions::new(),
        };
        let client = options
            .event_callback(move |event| {
                let lost_tx = lost_tx.clone();
                async move {
                    if matches!(event, async_nats::Event::Disconnected) {
                        let _ignored = lost_tx.send(true);
                    }
                }
            })
            .connect(&settings.url)
            .await
            .map_err(|_| BusError::Connect)?;
        let context = jetstream::new(client);
        Ok(Self {
            completed: open_durable(&context, &ANALYSIS_COMPLETED).await?,
            failed: open_durable(&context, &ANALYSIS_FAILED).await?,
            acknowledged: open_durable(&context, &POLICY_ACKNOWLEDGED).await?,
            context,
            connection_lost,
        })
    }

    /// Runs the relay, the dispatch loops and the three consumers until `shutdown` is signalled.
    ///
    /// Returns `Ok` only after an orderly shutdown. Any task that returns earlier, panics, or a
    /// lost broker connection returns an error so the caller can leave the process not ready.
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] when a supervised task stops or the connection is lost.
    pub async fn supervise(
        self,
        database: Database,
        lifecycle: Lifecycle,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), BusError> {
        let Self {
            context,
            completed,
            failed,
            acknowledged,
            mut connection_lost,
        } = self;
        let mut tasks: JoinSet<Result<(), BusError>> = JoinSet::new();
        tasks.spawn(relay_loop(database.clone(), context, shutdown.clone()));
        tasks.spawn(consume(completed, shutdown.clone(), {
            let database = database.clone();
            move |bytes| {
                let database = database.clone();
                async move { settle_completed(&database, &bytes).await }
            }
        }));
        tasks.spawn(consume(failed, shutdown.clone(), {
            let database = database.clone();
            move |bytes| {
                let database = database.clone();
                async move { settle_failed(&database, &bytes).await }
            }
        }));
        tasks.spawn(consume(acknowledged, shutdown.clone(), {
            move |bytes| {
                let database = database.clone();
                async move { settle_acknowledged(&database, &bytes).await }
            }
        }));
        let outcome = tokio::select! {
            biased;
            _ = shutdown.wait_for(|stopping| *stopping) => Ok(()),
            _ = connection_lost.wait_for(|lost| *lost) => Err(BusError::ConnectionLost),
            joined = tasks.join_next() => Err(match joined {
                Some(Ok(Err(error))) => error,
                _ => BusError::TaskStopped,
            }),
        };
        let _ignored = tokio::time::timeout(SHUTDOWN_GRACE, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.shutdown().await;
        if outcome.is_err() {
            lifecycle.mark_failed();
        }
        outcome
    }
}

async fn read_seed(path: &Path) -> Result<String, BusError> {
    let seed = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| BusError::Seed)?;
    Ok(seed.trim().to_owned())
}

async fn open_durable(
    context: &jetstream::Context,
    spec: &DurableSpec,
) -> Result<PullConsumer, BusError> {
    let mismatch = BusError::Durable {
        durable: spec.durable,
    };
    let consumer: PullConsumer = context
        .get_consumer_from_stream(spec.durable, spec.stream)
        .await
        .map_err(|_| BusError::Durable {
            durable: spec.durable,
        })?;
    let actual = &consumer.cached_info().config;
    if actual.durable_name.as_deref() != Some(spec.durable)
        || actual.filter_subject != spec.filter
        || actual.ack_policy != AckPolicy::Explicit
        || actual.ack_wait != spec.ack_wait
        || actual.deliver_policy != DeliverPolicy::All
        || actual.deliver_subject.is_some()
        || actual.max_deliver != -1
    {
        return Err(mismatch);
    }
    Ok(consumer)
}

// ---- relay and dispatch -------------------------------------------------------------------

async fn relay_loop(
    database: Database,
    context: jetstream::Context,
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
        flush_outbox(&database, &context).await?;
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

/// Publishes every due outbox row and sets `published_at` only after the `PubAck` resolves.
///
/// A publish failure leaves the row unpublished, records the attempt with a safe error class and
/// backs the row off, so a failing head row never starves later rows. A denied publish is
/// indistinguishable from a timeout here: check the NATS server log for a Publish Violation.
async fn flush_outbox(database: &Database, context: &jetstream::Context) -> Result<(), BusError> {
    let rows: Vec<(uuid::Uuid, String, serde_json::Value)> = sqlx::query_as(
        "select message_id, subject, payload from github_catalog.outbox_events
         where published_at is null and next_attempt_at <= now()
         order by created_at, message_id limit $1",
    )
    .bind(RELAY_BATCH)
    .fetch_all(database.pool())
    .await
    .map_err(|_| BusError::Catalog)?;
    for (message_id, subject, payload) in rows {
        if !RELAYED_SUBJECTS.contains(&subject.as_str()) {
            return Err(BusError::UnknownSubject);
        }
        let bytes = serde_json::to_vec(&payload).map_err(|_| BusError::Catalog)?;
        match publish(context, &subject, message_id, bytes).await {
            Ok(()) => {
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

// ---- consumers ----------------------------------------------------------------------------

/// What happens to a message once its durable outcome is known (S02 rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// The durable outcome is committed; acknowledge.
    Ack,
    /// The message can never be processed; terminate it.
    Term,
    /// A transient failure; redeliver after a short delay.
    Nak,
}

async fn consume<F, Fut>(
    consumer: PullConsumer,
    mut shutdown: watch::Receiver<bool>,
    handle: F,
) -> Result<(), BusError>
where
    F: Fn(Vec<u8>) -> Fut + Send,
    Fut: std::future::Future<Output = Disposition> + Send,
{
    let mut messages = consumer
        .stream()
        .max_messages_per_batch(16)
        .messages()
        .await
        .map_err(|_| BusError::Consume)?;
    loop {
        // cancel-safe: StreamExt::next and watch::Receiver::wait_for commit no work; the handler
        // runs outside the select so a shutdown never abandons a half-settled message.
        let next = tokio::select! {
            biased;
            _ = shutdown.wait_for(|stopping| *stopping) => return Ok(()),
            next = messages.next() => next,
        };
        let message = next
            .ok_or(BusError::Consume)?
            .map_err(|_| BusError::Consume)?;
        let kind = match handle(message.payload.to_vec()).await {
            Disposition::Ack => AckKind::Ack,
            Disposition::Term => AckKind::Term,
            Disposition::Nak => AckKind::Nak(Some(NAK_DELAY)),
        };
        message.ack_with(kind).await.map_err(|_| BusError::Ack)?;
    }
}

/// Decodes the envelope and returns it with its event id when the producer and tenant are valid.
fn accept_envelope(
    bytes: &[u8],
    producer: &str,
    event_type: &str,
    tenant_required: bool,
) -> Option<(EventEnvelope, uuid::Uuid)> {
    let envelope = EventEnvelope::from_json(bytes).ok()?;
    if envelope.event_type.to_wire() != event_type
        || envelope.producer.as_str() != producer
        || (tenant_required && envelope.tenant_id.is_none())
    {
        return None;
    }
    let event_id = envelope.event_id.0;
    Some((envelope, event_id))
}

async fn settle_completed(database: &Database, bytes: &[u8]) -> Disposition {
    let Some((envelope, event_id)) = accept_envelope(
        bytes,
        KNOWLEDGE_PRODUCER,
        "knowledge.repository_analysis.completed.v1",
        true,
    ) else {
        return Disposition::Term;
    };
    let Ok(completed) = envelope.payload_as::<RepositoryAnalysisCompleted>() else {
        return Disposition::Term;
    };
    if envelope.tenant_id != Some(completed.owner) {
        return Disposition::Term;
    }
    match consume_repository_analysis_completed(database, event_id, &completed).await {
        Ok(_) => Disposition::Ack,
        Err(error) => watch_disposition(&error),
    }
}

async fn settle_failed(database: &Database, bytes: &[u8]) -> Disposition {
    let Some((envelope, event_id)) = accept_envelope(
        bytes,
        KNOWLEDGE_PRODUCER,
        "knowledge.repository_analysis.failed.v1",
        true,
    ) else {
        return Disposition::Term;
    };
    let Ok(failed) = envelope.payload_as::<RepositoryAnalysisFailed>() else {
        return Disposition::Term;
    };
    if envelope.tenant_id != Some(failed.owner) {
        return Disposition::Term;
    }
    match consume_repository_analysis_failed(database, event_id, &failed).await {
        Ok(_) => Disposition::Ack,
        Err(error) => watch_disposition(&error),
    }
}

async fn settle_acknowledged(database: &Database, bytes: &[u8]) -> Disposition {
    let Some((envelope, event_id)) = accept_envelope(
        bytes,
        VAULT_PRODUCER,
        "vault.backup_policy.acknowledged.v1",
        false,
    ) else {
        return Disposition::Term;
    };
    let Ok(acknowledged) = envelope.payload_as::<PolicyAcknowledged>() else {
        return Disposition::Term;
    };
    match record_backup_policy_acknowledgment(database, event_id, &acknowledged).await {
        Ok(_) => Disposition::Ack,
        Err(BackupPolicyError::Persistence(_)) => Disposition::Nak,
        Err(_) => Disposition::Term,
    }
}

/// Only a persistence failure is transient; any other watch error is permanent for this input.
fn watch_disposition(error: &WatchError) -> Disposition {
    match error {
        WatchError::Persistence(_) => Disposition::Nak,
        _ => Disposition::Term,
    }
}
