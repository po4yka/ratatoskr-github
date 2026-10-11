//! The GitHub identity against an authorization-enabled broker built from the reviewed fragment
//! `deploy/nats/identity.conf` (XR-021 CONTRACTS.md S03).
//!
//! A denied publish is invisible to the client (`Publish Violation` appears only in the broker log),
//! so every refusal below is observed as a missing acknowledgement or a missing stream message.

use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_backup_contracts::{PolicyAcknowledged, PolicyOutcome};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    RepositoryAnalysisRequestStatus, mark_backup_policy_dirty, repository_analysis_request_state,
};
use ratatoskr_github_catalog_service::Lifecycle;
use ratatoskr_github_catalog_service::bus::{Bus, BusSettings};
use ratatoskr_github_contracts::RepositoryAnalysisCompleted;
use ratatoskr_identifiers::{EntityRef, Extensions};
use tokio::sync::watch;
use uuid::Uuid;

mod bus_support;
use bus_support::authorized_broker::AuthorizedBroker;
use bus_support::{
    ACKNOWLEDGED_SUBJECT, APPLY_SUBJECT, COMPLETED_SUBJECT, EVENTS_STREAM, GITHUB_DURABLES,
    REQUESTED_SUBJECT, TestResult, eventually, fact_envelope, provision, publish_acked,
    queue_analysis_request, stored_request,
};

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000971";
const KNOWLEDGE_DURABLE: &str = "ratatoskr_knowledge_repository_requests";

#[tokio::test]
async fn the_github_identity_relays_and_consumes_what_it_needs_and_is_refused_the_rest()
-> TestResult {
    let broker = AuthorizedBroker::start()?;
    let admin = broker.connect(&broker.admin_seed).await?;
    let context = provision(&admin, &GITHUB_DURABLES).await?;
    context
        .get_stream(EVENTS_STREAM)
        .await?
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(KNOWLEDGE_DURABLE.to_owned()),
            filter_subject: REQUESTED_SUBJECT.to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ..jetstream::consumer::pull::Config::default()
        })
        .await?;
    let database = TestDatabase::create().await?;
    let repository_id = queue_analysis_request(&database, 971_001, OWNER).await?;
    sqlx::query(
        "insert into github_catalog.repositories (repository_id, provider_repository_id, mode)
         values ($1, 971002, 'tracked')",
    )
    .bind(Uuid::now_v7())
    .execute(database.database.pool())
    .await?;
    mark_backup_policy_dirty(&database.database, bus_support::fixed_now()).await?;

    let bus = Bus::connect(&BusSettings {
        url: broker.url(),
        nkey_seed_path: Some(broker.github_seed.clone()),
    })
    .await?;
    let lifecycle = Lifecycle::starting();
    lifecycle.mark_ready();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(bus.supervise(database.database.clone(), lifecycle, shutdown));

    // Allowed publishes: both subjects are acknowledged by their streams.
    eventually("both outbox rows to be published", || async {
        let published: i64 = sqlx::query_scalar(
            "select count(*) from github_catalog.outbox_events where published_at is not null",
        )
        .fetch_one(database.database.pool())
        .await?;
        Ok::<_, Box<dyn std::error::Error>>((published == 2).then_some(()))
    })
    .await?;
    let events = context.get_stream(EVENTS_STREAM).await?;
    events
        .get_last_raw_message_by_subject(REQUESTED_SUBJECT)
        .await?;
    context
        .get_stream(bus_support::COMMANDS_STREAM)
        .await?
        .get_last_raw_message_by_subject(APPLY_SUBJECT)
        .await?;

    assert_refusals(&broker, &events).await?;

    // Allowed consumption: the three result durables fetch and acknowledge.
    let (_message_id, request) = stored_request(&database, repository_id).await?;
    dispatch_is_pending(&database, &request).await?;
    publish_results(&context, &request).await?;
    eventually("the result durables to deliver and settle", || async {
        let state =
            repository_analysis_request_state(&database.database, request.request_id).await?;
        let feedback: i64 =
            sqlx::query_scalar("select count(*) from github_catalog.backup_policy_feedback")
                .fetch_one(database.database.pool())
                .await?;
        Ok(
            (state.is_some_and(|state| state.status == RepositoryAnalysisRequestStatus::Completed)
                && feedback == 1)
                .then_some(()),
        )
    })
    .await?;

    stop.send(true)?;
    task.await??;
    database.cleanup().await?;
    Ok(())
}

/// The dispatcher already moved the request to pending when its envelope was stored.
async fn dispatch_is_pending(
    database: &TestDatabase,
    request: &ratatoskr_github_contracts::RepositoryAnalysisRequested,
) -> TestResult {
    let state = repository_analysis_request_state(&database.database, request.request_id).await?;
    assert_eq!(
        state.map(|state| state.status),
        Some(RepositoryAnalysisRequestStatus::Pending)
    );
    Ok(())
}

/// The GitHub identity cannot publish a Knowledge fact or inspect a Knowledge durable.
async fn assert_refusals(
    broker: &AuthorizedBroker,
    events: &jetstream::stream::Stream,
) -> TestResult {
    let github = broker.connect(&broker.github_seed).await?;
    let github_context = jetstream::ContextBuilder::new()
        .timeout(Duration::from_secs(2))
        .build(github);
    let denied = tokio::time::timeout(Duration::from_secs(4), async {
        github_context
            .publish(COMPLETED_SUBJECT, b"{}".to_vec().into())
            .await?
            .await
    })
    .await;
    assert!(
        !matches!(denied, Ok(Ok(_))),
        "the GitHub identity must not be able to publish {COMPLETED_SUBJECT}"
    );
    assert!(
        events
            .get_last_raw_message_by_subject(COMPLETED_SUBJECT)
            .await
            .is_err(),
        "the refused publish must not reach the stream"
    );
    let foreign = tokio::time::timeout(
        Duration::from_secs(4),
        github_context.get_consumer_from_stream::<jetstream::consumer::pull::Config, _, _>(
            KNOWLEDGE_DURABLE,
            EVENTS_STREAM,
        ),
    )
    .await;
    assert!(
        !matches!(foreign, Ok(Ok(_))),
        "the GitHub identity must not be able to inspect a Knowledge durable"
    );
    Ok(())
}

/// Publishes the Knowledge completion and the Vault acknowledgement as their producers would.
async fn publish_results(
    context: &jetstream::Context,
    request: &ratatoskr_github_contracts::RepositoryAnalysisRequested,
) -> TestResult {
    let completed = RepositoryAnalysisCompleted {
        owner: request.owner,
        repository_id: request.repository_id,
        github_repository_numeric_id: request.github_repository_numeric_id,
        request_id: request.request_id,
        source_revision: request.source_revision.clone(),
        analysis_result_ref: EntityRef::parse(&format!(
            "repository_analysis_result:{}",
            Uuid::now_v7()
        ))?,
        completed_at: bus_support::fixed_wire_now(),
        extensions: Extensions::default(),
    };
    let completed_id = Uuid::now_v7();
    publish_acked(
        context,
        COMPLETED_SUBJECT,
        completed_id,
        fact_envelope(
            completed_id,
            "ratatoskr-knowledge",
            Some(request.owner),
            &completed,
        )?,
    )
    .await?;
    let acknowledged = PolicyAcknowledged {
        acknowledged_policy_version: 1,
        outcome: PolicyOutcome::Accepted,
        reasons: Vec::new(),
        last_applied_policy_version: 0,
        extensions: Extensions::default(),
    };
    let acknowledged_id = Uuid::now_v7();
    publish_acked(
        context,
        ACKNOWLEDGED_SUBJECT,
        acknowledged_id,
        fact_envelope(acknowledged_id, "ratatoskr-vault", None, &acknowledged)?,
    )
    .await?;
    Ok(())
}
