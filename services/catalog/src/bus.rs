//! Least-privilege connection to the Platform-owned GitHub fleet-bus topology.

use std::path::Path;
use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_github_catalog::{OutboxFailureCode, OutboxTransport};
use uuid::Uuid;

/// Platform-owned command stream.
pub const COMMAND_STREAM: &str = "ratatoskr_commands";
/// Platform-owned event stream.
pub const EVENT_STREAM: &str = "ratatoskr_events";

/// One exact fixed durable assigned to this deployable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerSpec {
    /// Platform stream that owns the durable.
    pub stream: &'static str,
    /// Stable durable cursor name.
    pub durable: &'static str,
    /// Sole subject allowed through the durable.
    pub subject: &'static str,
}

/// Complete fixed durable inventory; startup verifies it and never creates it.
pub const CONSUMERS: [ConsumerSpec; 4] = [
    ConsumerSpec {
        stream: COMMAND_STREAM,
        durable: "ratatoskr_github_sync",
        subject: "cmd.github.sync.requested.v1",
    },
    ConsumerSpec {
        stream: EVENT_STREAM,
        durable: "ratatoskr_github_analysis_completed",
        subject: "evt.knowledge.repository_analysis.completed.v1",
    },
    ConsumerSpec {
        stream: EVENT_STREAM,
        durable: "ratatoskr_github_analysis_failed",
        subject: "evt.knowledge.repository_analysis.failed.v1",
    },
    ConsumerSpec {
        stream: EVENT_STREAM,
        durable: "ratatoskr_github_vault_policy_ack",
        subject: "evt.vault.backup_policy.acknowledged.v1",
    },
];

const ACK_WAIT: Duration = Duration::from_mins(2);
const MAX_DELIVER: i64 = 10;

/// A redacted fleet-bus startup or drain failure.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// The protected `NKey` seed could not be read.
    #[error("the fleet-bus credential could not be read")]
    Credential(#[source] std::io::Error),
    /// Connection did not finish inside the declared bound.
    #[error("the fleet-bus connection timed out")]
    ConnectTimeout,
    /// The broker refused or lost the connection.
    #[error("the fleet-bus connection failed")]
    Connect,
    /// A required stream or durable is absent, inaccessible, or drifted.
    #[error("the Platform-owned GitHub fleet-bus topology is unavailable or drifted")]
    Topology,
    /// The connection could not drain cleanly.
    #[error("the fleet-bus connection could not drain")]
    Drain,
}

/// Connected narrow GitHub bus capability.
#[derive(Debug, Clone)]
pub struct FleetBus {
    client: async_nats::Client,
    context: jetstream::Context,
    publish_ack_timeout: Duration,
}

impl FleetBus {
    /// Connects with a protected seed under one finite deadline.
    ///
    /// # Errors
    ///
    /// Returns [`BusError`] for unreadable credentials, timeout, or authentication/connectivity
    /// failure. No seed value is retained by this type or included in an error.
    pub async fn connect(
        url: &str,
        seed_path: &Path,
        connect_timeout: Duration,
        publish_ack_timeout: Duration,
    ) -> Result<Self, BusError> {
        let seed = tokio::fs::read_to_string(seed_path)
            .await
            .map_err(BusError::Credential)?;
        let connect = async_nats::ConnectOptions::with_nkey(seed.trim().to_owned()).connect(url);
        let client = tokio::time::timeout(connect_timeout, connect)
            .await
            .map_err(|_| BusError::ConnectTimeout)?
            .map_err(|_| BusError::Connect)?;
        Ok(Self {
            context: jetstream::new(client.clone()),
            client,
            publish_ack_timeout,
        })
    }

    /// Wraps an already connected client for broker integration tests.
    #[must_use]
    pub fn from_client(client: async_nats::Client, publish_ack_timeout: Duration) -> Self {
        Self {
            context: jetstream::new(client.clone()),
            client,
            publish_ack_timeout,
        }
    }

    /// Reports the client's current connection state without network I/O.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(
            self.client.connection_state(),
            async_nats::connection::State::Connected
        )
    }

    /// Verifies the complete pre-provisioned topology without create/update/delete authority.
    ///
    /// # Errors
    ///
    /// Returns [`BusError::Topology`] when any exact durable cannot be inspected or differs in its
    /// filter, acknowledgement, delivery, replay, or finite redelivery settings.
    pub async fn verify_topology(&self) -> Result<(), BusError> {
        for spec in CONSUMERS {
            let consumer: jetstream::consumer::PullConsumer = self
                .context
                .get_consumer_from_stream(spec.durable, spec.stream)
                .await
                .map_err(|_| BusError::Topology)?;
            let config = &consumer.cached_info().config;
            if config.durable_name.as_deref() != Some(spec.durable)
                || config.filter_subject != spec.subject
                || config.ack_policy != jetstream::consumer::AckPolicy::Explicit
                || config.deliver_subject.is_some()
                || config.deliver_policy != jetstream::consumer::DeliverPolicy::All
                || config.replay_policy != jetstream::consumer::ReplayPolicy::Instant
                || config.ack_wait != ACK_WAIT
                || config.max_deliver != MAX_DELIVER
            {
                return Err(BusError::Topology);
            }
        }
        Ok(())
    }

    /// Opens one of the four already verified fixed durables.
    ///
    /// # Errors
    ///
    /// Returns [`BusError::Topology`] when the assigned durable is no longer accessible.
    pub async fn consumer(
        &self,
        spec: ConsumerSpec,
    ) -> Result<jetstream::consumer::PullConsumer, BusError> {
        self.context
            .get_consumer_from_stream(spec.durable, spec.stream)
            .await
            .map_err(|_| BusError::Topology)
    }

    /// Drains subscriptions and pending client writes before the database is closed.
    ///
    /// # Errors
    ///
    /// Returns [`BusError::Drain`] when the broker connection cannot drain.
    pub async fn drain(&self) -> Result<(), BusError> {
        self.client.drain().await.map_err(|_| BusError::Drain)
    }
}

impl OutboxTransport for FleetBus {
    async fn publish(
        &self,
        subject: &str,
        envelope: &[u8],
        message_id: Uuid,
    ) -> Result<(), OutboxFailureCode> {
        if !self.is_connected() {
            return Err(OutboxFailureCode::BusUnavailable);
        }
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", message_id.to_string());
        let publish = async {
            let acknowledgement = self
                .context
                .publish_with_headers(subject.to_owned(), headers, envelope.to_vec().into())
                .await
                .map_err(|_| OutboxFailureCode::PublishRejected)?;
            acknowledgement
                .await
                .map_err(|_| OutboxFailureCode::PublishRejected)?;
            Ok(())
        };
        tokio::time::timeout(self.publish_ack_timeout, publish)
            .await
            .map_err(|_| OutboxFailureCode::AckTimeout)?
    }
}
