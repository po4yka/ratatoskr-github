//! Real-broker proof for read-only topology use and ack-before-outbox-mark publication.

use std::time::Duration;

use async_nats::jetstream;
use futures_util::TryStreamExt as _;
use ratatoskr_github_catalog::{
    OutboxTransport as _, publish_outbox_batch, test_support::TestDatabase,
};
use ratatoskr_github_catalog_service::{CONSUMERS, FleetBus};
use time::OffsetDateTime;
use uuid::Uuid;

#[expect(
    clippy::disallowed_methods,
    reason = "test-only broker location is not process configuration"
)]
fn nats_url() -> String {
    std::env::var("GITHUB_CATALOG_TEST_NATS_URL")
        .unwrap_or_else(|_| "nats://127.0.0.1:14227".to_owned())
}

#[tokio::test]
async fn publisher_uses_stored_identity_and_does_not_mutate_topology()
-> Result<(), Box<dyn std::error::Error>> {
    let client = async_nats::connect(nats_url()).await?;
    let context = jetstream::new(client.clone());
    provision_fixture_topology(&context).await?;
    let before = topology_inventory(&context).await?;

    let database = TestDatabase::create().await?;
    let message_id = Uuid::now_v7();
    let envelope = format!("{{\"event_id\":\"{message_id}\",\"fixture\":true}}\n");
    sqlx::query(
        "insert into github_catalog.outbox_events
             (message_id,subject,envelope,ordering_key,ordering_sequence,next_attempt_at)
         values ($1,'evt.knowledge.repository_analysis.requested.v1',$2,'fixture',1,
                 now() - interval '1 minute')",
    )
    .bind(message_id)
    .bind(envelope.as_bytes())
    .execute(database.database.pool())
    .await?;

    let bus = FleetBus::from_client(client, Duration::from_secs(2));
    bus.verify_topology().await?;
    let mut stream = context.get_stream("ratatoskr_events").await?;
    let before_messages = stream.info().await?.state.messages;
    bus.publish(
        "evt.knowledge.repository_analysis.requested.v1",
        envelope.as_bytes(),
        message_id,
    )
    .await
    .map_err(|error| format!("initial broker publication failed: {error:?}"))?;
    let report = publish_outbox_batch(
        &database.database,
        &bus,
        Uuid::now_v7(),
        OffsetDateTime::now_utc(),
        time::Duration::seconds(30),
        1,
        3,
        time::Duration::seconds(1),
    )
    .await?;
    assert_eq!(report.published, 1);
    let marked: bool = sqlx::query_scalar(
        "select published_at is not null from github_catalog.outbox_events where message_id=$1",
    )
    .bind(message_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(marked);

    assert_eq!(stream.info().await?.state.messages, before_messages + 1);
    let stored = stream
        .get_last_raw_message_by_subject("evt.knowledge.repository_analysis.requested.v1")
        .await?;
    assert_eq!(stored.payload.as_ref(), envelope.as_bytes());
    let expected_id = message_id.to_string();
    assert_eq!(
        stored
            .headers
            .get_last("Nats-Msg-Id")
            .map(async_nats::HeaderValue::as_str),
        Some(expected_id.as_str())
    );
    assert_eq!(topology_inventory(&context).await?, before);

    bus.drain().await?;
    database.cleanup().await?;
    Ok(())
}

async fn provision_fixture_topology(
    context: &jetstream::Context,
) -> Result<(), Box<dyn std::error::Error>> {
    for (name, subjects) in [
        ("ratatoskr_commands", vec!["cmd.>".to_owned()]),
        ("ratatoskr_events", vec!["evt.>".to_owned()]),
    ] {
        context
            .get_or_create_stream(jetstream::stream::Config {
                name: name.to_owned(),
                subjects,
                ..jetstream::stream::Config::default()
            })
            .await?;
    }
    for spec in CONSUMERS {
        let stream = context.get_stream(spec.stream).await?;
        stream
            .get_or_create_consumer(
                spec.durable,
                jetstream::consumer::pull::Config {
                    durable_name: Some(spec.durable.to_owned()),
                    filter_subject: spec.subject.to_owned(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_mins(2),
                    max_deliver: 10,
                    ..jetstream::consumer::pull::Config::default()
                },
            )
            .await?;
    }
    Ok(())
}

async fn topology_inventory(
    context: &jetstream::Context,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut inventory = context.stream_names().try_collect::<Vec<_>>().await?;
    for stream_name in ["ratatoskr_commands", "ratatoskr_events"] {
        let stream = context.get_stream(stream_name).await?;
        inventory.extend(
            stream
                .consumer_names()
                .map_ok(|name| format!("{stream_name}/{name}"))
                .try_collect::<Vec<_>>()
                .await?,
        );
    }
    inventory.sort();
    Ok(inventory)
}
