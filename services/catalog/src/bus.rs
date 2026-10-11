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
    BackupPolicyError, Database, WatchError, consume_repository_analysis_completed,
    consume_repository_analysis_failed, record_backup_policy_acknowledgment,
};
use ratatoskr_github_contracts::{RepositoryAnalysisCompleted, RepositoryAnalysisFailed};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::Lifecycle;
use crate::observer::WatchObserver;
use crate::relay::relay_loop;

/// The events stream every GitHub subject lives on, except the Vault command.
const EVENTS_STREAM: &str = "ratatoskr_events";
/// Producer allowed to publish repository analysis results.
const KNOWLEDGE_PRODUCER: &str = "ratatoskr-knowledge";
/// Producer allowed to publish the Vault policy acknowledgement.
const VAULT_PRODUCER: &str = "ratatoskr-vault";

/// The only inbox namespace this identity may subscribe to (XR-021 CONTRACTS.md R3-01). Every
/// JetStream reply, acknowledgement and fetched message arrives on it, so no other identity's
/// deliveries are readable and the reviewed stanza allows `_INBOX_github.>` and nothing broader.
const INBOX_PREFIX: &str = "_INBOX_github";

const NAK_DELAY: Duration = Duration::from_secs(2);
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
    observer: Option<WatchObserver>,
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
            .custom_inbox_prefix(INBOX_PREFIX)
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
            observer: None,
        })
    }

    /// Supervises the periodic watch observer beside the relay and the consumers. `None` (a
    /// disabled interval) leaves the bus as it was.
    #[must_use]
    pub fn with_observer(mut self, observer: Option<WatchObserver>) -> Self {
        self.observer = observer;
        self
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
            observer,
        } = self;
        let mut tasks: JoinSet<Result<(), BusError>> = JoinSet::new();
        tasks.spawn(relay_loop(
            database.clone(),
            context,
            lifecycle.clone(),
            shutdown.clone(),
        ));
        if let Some(observer) = observer {
            // A pass makes provider calls that may each take the whole provider timeout, so it
            // runs as its own supervised task and never delays the relay tick.
            let database = database.clone();
            let shutdown = shutdown.clone();
            tasks.spawn(async move {
                observer
                    .run(database, shutdown)
                    .await
                    .map_err(|_| BusError::Catalog)
            });
        }
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
