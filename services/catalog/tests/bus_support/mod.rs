//! Shared broker provisioning and fixtures for the bus integration tests.
//!
//! Edge provisions the two streams and the three durables this service consumes in production
//! (XR-021 CONTRACTS.md S04); these helpers play Edge for a test broker.

#![allow(
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "each bus test binary compiles this shared module and uses a different subset"
)]

use std::net::SocketAddr;
use std::time::Duration;

use async_nats::jetstream::{self, consumer::AckPolicy, consumer::DeliverPolicy};
use ratatoskr_event_envelope::{EnvelopeSchemaVersion, EventEnvelope, EventPayload, ProducerName};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    apply_fresh_body, evaluate_metadata_watches, register_repository_analysis_watch,
};
use ratatoskr_github_contracts::{RepositoryAnalysisRequested, RepositoryAnalysisRevision};
use ratatoskr_identifiers::{EntityRef, EventId, Extensions, TenantRef, WireTimestamp};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::watch;
use uuid::Uuid;

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub const EVENTS_STREAM: &str = "ratatoskr_events";
pub const COMMANDS_STREAM: &str = "ratatoskr_commands";
pub const REQUESTED_SUBJECT: &str = "evt.knowledge.repository_analysis.requested.v1";
pub const COMPLETED_SUBJECT: &str = "evt.knowledge.repository_analysis.completed.v1";
pub const FAILED_SUBJECT: &str = "evt.knowledge.repository_analysis.failed.v1";
pub const ACKNOWLEDGED_SUBJECT: &str = "evt.vault.backup_policy.acknowledged.v1";
pub const APPLY_SUBJECT: &str = "cmd.vault.backup_policy.apply_requested.v1";

pub const COMPLETED_DURABLE: &str = "ratatoskr_github_analysis_completed";
pub const FAILED_DURABLE: &str = "ratatoskr_github_analysis_failed";
pub const ACKNOWLEDGED_DURABLE: &str = "ratatoskr_github_policy_acknowledged";

/// The three durables of S04 owned by this service: name and filter.
pub const GITHUB_DURABLES: [(&str, &str); 3] = [
    (COMPLETED_DURABLE, COMPLETED_SUBJECT),
    (FAILED_DURABLE, FAILED_SUBJECT),
    (ACKNOWLEDGED_DURABLE, ACKNOWLEDGED_SUBJECT),
];

/// Serializes tests that share the fixed stream and durable names of one broker.
pub static BROKER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[expect(
    clippy::disallowed_methods,
    reason = "test-only broker location is not process configuration"
)]
pub fn broker_url() -> String {
    std::env::var("GITHUB_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4225".to_owned())
}

pub fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339).expect("fixed RFC3339 instant")
}

/// The same fixed instant as a wire timestamp, for fact payloads and envelopes a test builds.
pub fn fixed_wire_now() -> WireTimestamp {
    WireTimestamp::parse("2026-01-01T00:00:00Z").expect("fixed canonical wire timestamp")
}

/// Creates both streams, purges them, and recreates the given durables with the S04 settings.
pub async fn provision(
    client: &async_nats::Client,
    durables: &[(&str, &str)],
) -> Result<jetstream::Context, Box<dyn std::error::Error>> {
    let context = jetstream::new(client.clone());
    for (name, subject) in [(EVENTS_STREAM, "evt.>"), (COMMANDS_STREAM, "cmd.>")] {
        let stream = context
            .get_or_create_stream(jetstream::stream::Config {
                name: name.to_owned(),
                subjects: vec![subject.to_owned()],
                ..jetstream::stream::Config::default()
            })
            .await?;
        stream.purge().await?;
    }
    let events = context.get_stream(EVENTS_STREAM).await?;
    // A durable left by an earlier test would otherwise survive a call that omits it.
    for (durable, _filter) in GITHUB_DURABLES {
        let _absent = events.delete_consumer(durable).await;
    }
    for (durable, filter) in durables {
        events
            .create_consumer(jetstream::consumer::pull::Config {
                durable_name: Some((*durable).to_owned()),
                filter_subject: (*filter).to_owned(),
                ack_policy: AckPolicy::Explicit,
                deliver_policy: DeliverPolicy::All,
                ack_wait: Duration::from_secs(30),
                ..jetstream::consumer::pull::Config::default()
            })
            .await?;
    }
    Ok(context)
}

/// Polls `check` until it yields `Some` or the deadline passes.
pub async fn eventually<T, F, Fut>(
    what: &str,
    mut check: F,
) -> Result<T, Box<dyn std::error::Error>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>, Box<dyn std::error::Error>>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(found) = check().await? {
            return Ok(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// One repository whose metadata change queued an analysis request for `owner`.
pub async fn queue_analysis_request(
    database: &TestDatabase,
    provider_id: i64,
    owner: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, provider_id).await?;
    let body = |description: &str| ratatoskr_github_catalog::provider::ProviderRepositoryBody {
        provider_repository_id: provider_id,
        full_name: format!("acme/bus-{provider_id}"),
        description: Some(description.to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 3,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-27T00:00:00Z".to_owned()),
    };
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body("before"),
        None,
    )
    .await?;
    register_repository_analysis_watch(
        &database.database,
        TenantRef::parse(owner)?,
        repository.repository_id,
    )
    .await?;
    let changed = body("after");
    apply_fresh_body(&database.database, repository.repository_id, &changed, None).await?;
    evaluate_metadata_watches(
        &database.database,
        repository.repository_id,
        &changed,
        fixed_now(),
    )
    .await?;
    Ok(repository.repository_id)
}

/// Reads the stored request envelope the relay published for `repository_id`.
pub async fn stored_request(
    database: &TestDatabase,
    repository_id: Uuid,
) -> Result<(Uuid, RepositoryAnalysisRequested), Box<dyn std::error::Error>> {
    let (message_id, payload): (Uuid, serde_json::Value) = sqlx::query_as(
        "select message_id, payload from github_catalog.outbox_events
         where subject = $1 and payload ->> 'aggregate_id' = $2",
    )
    .bind(REQUESTED_SUBJECT)
    .bind(format!("repository:{repository_id}"))
    .fetch_one(database.database.pool())
    .await?;
    let envelope = EventEnvelope::from_json(payload.to_string().as_bytes())?;
    Ok((message_id, envelope.payload_as()?))
}

/// A Knowledge-produced fact envelope around `payload`.
pub fn fact_envelope<P: EventPayload>(
    event_id: Uuid,
    producer: &str,
    owner: Option<TenantRef>,
    payload: &P,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut envelope = EventEnvelope {
        event_id: EventId(event_id),
        event_type: P::event_type(),
        occurred_at: fixed_wire_now(),
        producer: ProducerName::parse(producer)?,
        aggregate_id: EntityRef::parse(&format!("repository:{}", Uuid::now_v7()))?,
        correlation_id: EntityRef::parse(&format!("event:{event_id}"))?,
        causation_id: None,
        tenant_id: owner,
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload: serde_json::Map::new(),
        extensions: Extensions::default(),
    };
    envelope.set_payload(payload)?;
    Ok(serde_json::to_vec(&envelope)?)
}

/// Publishes `bytes` to `subject` and waits for the stream acknowledgement.
pub async fn publish_acked(
    context: &jetstream::Context,
    subject: &str,
    message_id: Uuid,
    bytes: Vec<u8>,
) -> TestResult {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", message_id.to_string());
    context
        .publish_with_headers(subject.to_owned(), headers, bytes.into())
        .await?
        .await?;
    Ok(())
}

/// The immutable revision a request names, for building a matching terminal fact.
pub fn revision_of(request: &RepositoryAnalysisRequested) -> RepositoryAnalysisRevision {
    request.source_revision.clone()
}

/// A loopback TCP forwarder whose severing simulates a lost broker connection.
pub struct Proxy {
    pub address: SocketAddr,
    pub sever: watch::Sender<bool>,
}

pub async fn proxy_to(target: SocketAddr) -> Result<Proxy, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (sever, severed) = watch::channel(false);
    tokio::spawn(async move {
        let mut severed_listener = severed.clone();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut inbound, _peer)) = accepted else { return };
                    let mut severed = severed.clone();
                    tokio::spawn(async move {
                        let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await else {
                            return;
                        };
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                            _ = severed.wait_for(|cut| *cut) => {}
                        }
                    });
                }
                _ = severed_listener.wait_for(|cut| *cut) => return,
            }
        }
    });
    Ok(Proxy { address, sever })
}
