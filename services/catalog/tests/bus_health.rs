//! Relay health reaches readiness, and an envelope the broker can never accept is classed
//! undeliverable instead of retried forever (XR-021 CONTRACTS.md R2-03 and R2-04 rule 2).
//!
//! Both tests run the real relay against an authorization-enabled broker built from the reviewed
//! GITHUB fragment, so a refused subject is the same Publish Violation an ACL drift produces.

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog_service::bus::{Bus, BusSettings};
use ratatoskr_github_catalog_service::{Lifecycle, admin_router};
use tokio::sync::watch;
use tower::ServiceExt as _;
use uuid::Uuid;

mod bus_support;
use bus_support::authorized_broker::{AuthorizedBroker, BrokerOptions};
use bus_support::{
    APPLY_SUBJECT, GITHUB_DURABLES, REQUESTED_SUBJECT, TestResult, eventually, provision,
};

async fn get(app: &axum::Router, path: &str) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status().as_u16();
    let body = response.into_body().collect().await?.to_bytes();
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn enqueue(
    database: &TestDatabase,
    subject: &str,
    padding: usize,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let message_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.outbox_events (message_id, subject, payload)
         values ($1, $2, $3)",
    )
    .bind(message_id)
    .bind(subject)
    .bind(serde_json::json!({ "padding": "x".repeat(padding) }))
    .execute(database.database.pool())
    .await?;
    Ok(message_id)
}

struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), ratatoskr_github_catalog_service::bus::BusError>>,
    app: axum::Router,
}

async fn run_relay(
    broker: &AuthorizedBroker,
    database: &TestDatabase,
) -> Result<Running, Box<dyn std::error::Error>> {
    let bus = Bus::connect(&BusSettings {
        url: broker.url(),
        nkey_seed_path: Some(broker.github_seed.clone()),
    })
    .await?;
    let lifecycle = Lifecycle::starting();
    lifecycle.mark_ready();
    let app = admin_router(lifecycle.clone());
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(bus.supervise(database.database.clone(), lifecycle, shutdown));
    Ok(Running { stop, task, app })
}

#[tokio::test]
async fn a_refused_subject_flips_readiness_and_the_next_clean_pass_restores_it() -> TestResult {
    let broker = AuthorizedBroker::start_with(&BrokerOptions {
        omit_publish: Some(APPLY_SUBJECT.to_owned()),
        ..BrokerOptions::default()
    })?;
    let admin = broker.connect(&broker.admin_seed).await?;
    provision(&admin, &GITHUB_DURABLES).await?;
    let database = TestDatabase::create().await?;
    let allowed = enqueue(&database, REQUESTED_SUBJECT, 16).await?;
    let refused = enqueue(&database, APPLY_SUBJECT, 16).await?;
    let running = run_relay(&broker, &database).await?;

    // The refused row fails three times; moving its backoff back avoids waiting for it.
    eventually("the refused row to fail three attempts", || async {
        sqlx::query(
            "update github_catalog.outbox_events set next_attempt_at = now()
             where message_id = $1 and published_at is null",
        )
        .bind(refused)
        .execute(database.database.pool())
        .await?;
        let attempts: i32 = sqlx::query_scalar(
            "select attempt_count from github_catalog.outbox_events where message_id = $1",
        )
        .bind(refused)
        .fetch_one(database.database.pool())
        .await?;
        Ok::<_, Box<dyn std::error::Error>>((attempts >= 3).then_some(()))
    })
    .await?;
    let (status, body) = eventually("readiness to report the failing publisher", || async {
        let answer = get(&running.app, "/ready").await?;
        Ok::<_, Box<dyn std::error::Error>>((answer.0 == 503).then_some(answer))
    })
    .await?;
    assert_eq!(status, 503);
    assert!(body.contains("bus_publish"), "ready body: {body}");
    let published: bool = sqlx::query_scalar(
        "select published_at is not null from github_catalog.outbox_events where message_id = $1",
    )
    .bind(allowed)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        published,
        "a refused row must not stop the others publishing"
    );

    sqlx::query("delete from github_catalog.outbox_events where message_id = $1")
        .bind(refused)
        .execute(database.database.pool())
        .await?;
    eventually("the next clean pass to restore readiness", || async {
        Ok::<_, Box<dyn std::error::Error>>(
            (get(&running.app, "/ready").await?.0 == 200).then_some(()),
        )
    })
    .await?;

    running.stop.send(true)?;
    running.task.await??;
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn an_oversize_envelope_is_undeliverable_and_never_flips_readiness() -> TestResult {
    let broker = AuthorizedBroker::start_with(&BrokerOptions {
        max_payload: Some(65_536),
        ..BrokerOptions::default()
    })?;
    let admin = broker.connect(&broker.admin_seed).await?;
    provision(&admin, &GITHUB_DURABLES).await?;
    let database = TestDatabase::create().await?;
    let small = enqueue(&database, REQUESTED_SUBJECT, 16).await?;
    let oversize = enqueue(&database, REQUESTED_SUBJECT, 70_000).await?;
    let running = run_relay(&broker, &database).await?;

    eventually("the small row to publish and the large row to be classed", || async {
        let (small_published, large_class): (bool, Option<String>) = sqlx::query_as(
            "select
                 (select published_at is not null from github_catalog.outbox_events where message_id = $1),
                 (select last_error from github_catalog.outbox_events
                   where message_id = $2 and undeliverable_at is not null)",
        )
        .bind(small)
        .bind(oversize)
        .fetch_one(database.database.pool())
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(
            (small_published && large_class.as_deref() == Some("payload_too_large")).then_some(()),
        )
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let attempts: i32 = sqlx::query_scalar(
        "select attempt_count from github_catalog.outbox_events where message_id = $1",
    )
    .bind(oversize)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(attempts, 0, "an undeliverable row is never retried");
    assert_eq!(get(&running.app, "/ready").await?.0, 200);
    let (_status, metrics) = get(&running.app, "/metrics").await?;
    assert!(
        metrics
            .contains("github_catalog_outbox_undeliverable_total{class=\"payload_too_large\"} 1"),
        "metrics: {metrics}"
    );

    running.stop.send(true)?;
    running.task.await??;
    database.cleanup().await?;
    Ok(())
}
