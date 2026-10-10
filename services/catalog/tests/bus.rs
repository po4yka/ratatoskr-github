//! The GitHub Catalog bus relay, result consumers and dispatch loops against a real `JetStream`
//! broker (XR-021 CONTRACTS.md S02, S04 and S09).

use std::net::SocketAddr;
use std::time::Duration;

use ratatoskr_backup_contracts::{PolicyAcknowledged, PolicyOutcome};
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AnalysisDispatch, RepositoryAnalysisRequestStatus, mark_backup_policy_dirty,
    repository_analysis_request_state,
};
use ratatoskr_github_catalog_service::Lifecycle;
use ratatoskr_github_catalog_service::bus::{Bus, BusError, BusSettings};
use ratatoskr_github_contracts::{
    AnalysisFailureCode, RepositoryAnalysisCompleted, RepositoryAnalysisFailed,
};
use ratatoskr_identifiers::{EntityRef, Extensions, WireTimestamp};
use time::OffsetDateTime;
use tokio::sync::watch;
use uuid::Uuid;

mod bus_support;
use bus_support::{
    ACKNOWLEDGED_DURABLE, ACKNOWLEDGED_SUBJECT, APPLY_SUBJECT, BROKER, COMMANDS_STREAM,
    COMPLETED_DURABLE, COMPLETED_SUBJECT, EVENTS_STREAM, FAILED_SUBJECT, GITHUB_DURABLES,
    REQUESTED_SUBJECT, TestResult, broker_url, eventually, fact_envelope, provision, publish_acked,
    queue_analysis_request, stored_request,
};

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000961";

fn settings(url: &str) -> BusSettings {
    BusSettings {
        url: url.to_owned(),
        nkey_seed_path: None,
    }
}

/// A running service bus: the supervised future plus the handle that stops it.
struct Running {
    stop: watch::Sender<bool>,
    lifecycle: Lifecycle,
    task: tokio::task::JoinHandle<Result<(), BusError>>,
}

async fn start(url: &str, database: &TestDatabase) -> Result<Running, Box<dyn std::error::Error>> {
    let bus = Bus::connect(&settings(url)).await?;
    let lifecycle = Lifecycle::starting();
    lifecycle.mark_ready();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(bus.supervise(database.database.clone(), lifecycle.clone(), shutdown));
    Ok(Running {
        stop,
        lifecycle,
        task,
    })
}

impl Running {
    async fn stop(self) -> TestResult {
        self.stop.send(true)?;
        self.task.await??;
        Ok(())
    }
}

async fn stream_messages(
    context: &async_nats::jetstream::Context,
    stream: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(context
        .get_stream(stream)
        .await?
        .get_info()
        .await?
        .state
        .messages)
}

#[tokio::test]
async fn a_queued_analysis_request_is_dispatched_and_relayed_exactly_once() -> TestResult {
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    let context = provision(&client, &GITHUB_DURABLES).await?;
    let database = TestDatabase::create().await?;
    let repository_id = queue_analysis_request(&database, 961_001, OWNER).await?;

    let running = start(&url, &database).await?;
    let published = eventually("the outbox row to be published", || async {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "select message_id from github_catalog.outbox_events where published_at is not null",
        )
        .fetch_optional(database.database.pool())
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(row.map(|(id,)| id))
    })
    .await?;
    let (message_id, request) = stored_request(&database, repository_id).await?;
    assert_eq!(published, message_id);
    running.stop().await?;

    assert_eq!(stream_messages(&context, EVENTS_STREAM).await?, 1);
    let stream = context.get_stream(EVENTS_STREAM).await?;
    let message = stream
        .get_last_raw_message_by_subject(REQUESTED_SUBJECT)
        .await?;
    let envelope = EventEnvelope::from_json(&message.payload)?;
    assert_eq!(envelope.event_id.to_string(), message_id.to_string());
    assert_eq!(
        message
            .headers
            .get("Nats-Msg-Id")
            .map(ToString::to_string)
            .as_deref(),
        Some(message_id.to_string().as_str())
    );
    let published: ratatoskr_github_contracts::RepositoryAnalysisRequested =
        envelope.payload_as()?;
    assert_eq!(published.request_id, request.request_id);
    database.cleanup().await?;
    Ok(())
}

async fn dispatched_request(
    database: &TestDatabase,
    provider_id: i64,
) -> Result<
    (
        Uuid,
        ratatoskr_github_contracts::RepositoryAnalysisRequested,
    ),
    Box<dyn std::error::Error>,
> {
    let repository_id = queue_analysis_request(database, provider_id, OWNER).await?;
    let dispatched = ratatoskr_github_catalog::dispatch_due_repository_analysis(
        &database.database,
        bus_support::fixed_now() + time::Duration::seconds(30),
    )
    .await?;
    assert!(matches!(dispatched, AnalysisDispatch::Pending { .. }));
    stored_request(database, repository_id).await
}

fn completed_for(
    request: &ratatoskr_github_contracts::RepositoryAnalysisRequested,
) -> Result<RepositoryAnalysisCompleted, Box<dyn std::error::Error>> {
    Ok(RepositoryAnalysisCompleted {
        owner: request.owner,
        repository_id: request.repository_id,
        github_repository_numeric_id: request.github_repository_numeric_id,
        request_id: request.request_id,
        source_revision: request.source_revision.clone(),
        analysis_result_ref: EntityRef::parse(&format!(
            "repository_analysis_result:{}",
            Uuid::now_v7()
        ))?,
        completed_at: WireTimestamp::now(),
        extensions: Extensions::default(),
    })
}

#[tokio::test]
async fn completed_and_failed_facts_settle_their_requests_and_redelivery_changes_nothing()
-> TestResult {
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    let context = provision(&client, &GITHUB_DURABLES).await?;
    let database = TestDatabase::create().await?;
    let (_first_id, first) = dispatched_request(&database, 961_011).await?;
    let (_second_id, second) = dispatched_request(&database, 961_012).await?;
    let running = start(&url, &database).await?;

    let completed = completed_for(&first)?;
    let event_id = Uuid::now_v7();
    let bytes = fact_envelope(
        event_id,
        "ratatoskr-knowledge",
        Some(first.owner),
        &completed,
    )?;
    publish_acked(&context, COMPLETED_SUBJECT, event_id, bytes.clone()).await?;
    // The same envelope again under a different stream message id: a redelivery of the same fact.
    publish_acked(&context, COMPLETED_SUBJECT, Uuid::now_v7(), bytes).await?;
    let failed = RepositoryAnalysisFailed {
        owner: second.owner,
        repository_id: second.repository_id,
        github_repository_numeric_id: second.github_repository_numeric_id,
        request_id: second.request_id,
        source_revision: second.source_revision.clone(),
        failure_code: AnalysisFailureCode::SourceUnavailable,
        retryable: true,
        failed_at: WireTimestamp::now(),
        extensions: Extensions::default(),
    };
    let failed_id = Uuid::now_v7();
    let failed_bytes = fact_envelope(
        failed_id,
        "ratatoskr-knowledge",
        Some(second.owner),
        &failed,
    )?;
    publish_acked(&context, FAILED_SUBJECT, failed_id, failed_bytes).await?;

    let state = eventually("both requests to settle", || async {
        let first_state =
            repository_analysis_request_state(&database.database, first.request_id).await?;
        let second_state =
            repository_analysis_request_state(&database.database, second.request_id).await?;
        Ok::<_, Box<dyn std::error::Error>>(match (first_state, second_state) {
            (Some(a), Some(b))
                if a.status == RepositoryAnalysisRequestStatus::Completed
                    && b.status == RepositoryAnalysisRequestStatus::Failed =>
            {
                Some((a, b))
            }
            _ => None,
        })
    })
    .await?;
    assert_eq!(
        state.0.analysis_result_ref.as_deref(),
        Some(completed.analysis_result_ref.to_wire().as_str())
    );
    let inbox: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.inbox_events where subject = $1")
            .bind(COMPLETED_SUBJECT)
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(inbox, 1, "the redelivered fact must be recorded once");
    running.stop().await?;
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_fact_from_the_wrong_producer_is_terminated_without_changing_state() -> TestResult {
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    let context = provision(&client, &GITHUB_DURABLES).await?;
    let database = TestDatabase::create().await?;
    let (_id, request) = dispatched_request(&database, 961_021).await?;
    let running = start(&url, &database).await?;

    let forged = fact_envelope(
        Uuid::now_v7(),
        "ratatoskr-x",
        Some(request.owner),
        &completed_for(&request)?,
    )?;
    publish_acked(&context, COMPLETED_SUBJECT, Uuid::now_v7(), forged).await?;

    let consumer: async_nats::jetstream::consumer::PullConsumer = context
        .get_consumer_from_stream(COMPLETED_DURABLE, EVENTS_STREAM)
        .await?;
    eventually("the forged fact to be settled by the durable", || async {
        let mut consumer = consumer.clone();
        let info = consumer.info().await?;
        Ok::<_, Box<dyn std::error::Error>>(
            (info.delivered.consumer_sequence >= 1
                && info.num_ack_pending == 0
                && info.num_pending == 0)
                .then_some(()),
        )
    })
    .await?;
    let state = repository_analysis_request_state(&database.database, request.request_id).await?;
    assert_eq!(
        state.map(|state| state.status),
        Some(RepositoryAnalysisRequestStatus::Pending)
    );
    let inbox: i64 = sqlx::query_scalar("select count(*) from github_catalog.inbox_events")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(inbox, 0);
    running.stop().await?;
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_due_backup_policy_is_published_once_and_its_acknowledgement_is_recorded_once()
-> TestResult {
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    let context = provision(&client, &GITHUB_DURABLES).await?;
    let database = TestDatabase::create().await?;
    sqlx::query(
        "insert into github_catalog.repositories (repository_id, provider_repository_id, mode)
         values ($1, 961031, 'tracked')",
    )
    .bind(Uuid::now_v7())
    .execute(database.database.pool())
    .await?;
    mark_backup_policy_dirty(
        &database.database,
        OffsetDateTime::now_utc() - time::Duration::seconds(300),
    )
    .await?;
    let running = start(&url, &database).await?;

    eventually("the policy command to be published", || async {
        let published: i64 = sqlx::query_scalar(
            "select count(*) from github_catalog.outbox_events
             where subject = $1 and published_at is not null",
        )
        .bind(APPLY_SUBJECT)
        .fetch_one(database.database.pool())
        .await?;
        Ok::<_, Box<dyn std::error::Error>>((published == 1).then_some(()))
    })
    .await?;
    // Several more ticks must not publish the same policy version again.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(stream_messages(&context, COMMANDS_STREAM).await?, 1);

    let acknowledged = PolicyAcknowledged {
        acknowledged_policy_version: 1,
        outcome: PolicyOutcome::Accepted,
        reasons: Vec::new(),
        last_applied_policy_version: 0,
        extensions: Extensions::default(),
    };
    let event_id = Uuid::now_v7();
    let bytes = fact_envelope(event_id, "ratatoskr-vault", None, &acknowledged)?;
    publish_acked(&context, ACKNOWLEDGED_SUBJECT, event_id, bytes.clone()).await?;
    publish_acked(&context, ACKNOWLEDGED_SUBJECT, Uuid::now_v7(), bytes).await?;
    let consumer: async_nats::jetstream::consumer::PullConsumer = context
        .get_consumer_from_stream(ACKNOWLEDGED_DURABLE, EVENTS_STREAM)
        .await?;
    eventually("both deliveries to be acknowledged", || async {
        let mut consumer = consumer.clone();
        let info = consumer.info().await?;
        Ok::<_, Box<dyn std::error::Error>>(
            (info.delivered.consumer_sequence >= 2
                && info.num_ack_pending == 0
                && info.num_pending == 0)
                .then_some(()),
        )
    })
    .await?;
    let feedback: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.backup_policy_feedback")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        feedback, 1,
        "a redelivered acknowledgement must record feedback once"
    );
    running.stop().await?;
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_missing_or_mismatched_durable_refuses_to_start() -> TestResult {
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    // Two of the three durables exist: the third is missing.
    provision(&client, &GITHUB_DURABLES[..2]).await?;
    let missing = Bus::connect(&settings(&url)).await;
    assert!(
        matches!(missing, Err(BusError::Durable { durable }) if durable == ACKNOWLEDGED_DURABLE),
        "a missing durable must refuse start, got {missing:?}"
    );

    // A durable with the wrong ack wait is refused rather than modified.
    let context = provision(&client, &GITHUB_DURABLES).await?;
    let events = context.get_stream(EVENTS_STREAM).await?;
    events.delete_consumer(COMPLETED_DURABLE).await?;
    events
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(COMPLETED_DURABLE.to_owned()),
            filter_subject: COMPLETED_SUBJECT.to_owned(),
            ack_wait: Duration::from_secs(7),
            ..async_nats::jetstream::consumer::pull::Config::default()
        })
        .await?;
    let mismatched = Bus::connect(&settings(&url)).await;
    assert!(
        matches!(mismatched, Err(BusError::Durable { durable }) if durable == COMPLETED_DURABLE),
        "a mismatched durable must refuse start, got {mismatched:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_lost_broker_connection_marks_the_service_not_ready_and_ends_with_an_error() -> TestResult
{
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    provision(&client, &GITHUB_DURABLES).await?;
    let target: SocketAddr = url.trim_start_matches("nats://").parse()?;
    let proxy = bus_support::proxy_to(target).await?;
    let database = TestDatabase::create().await?;
    let running = start(&format!("nats://{}", proxy.address), &database).await?;
    assert!(running.lifecycle.is_ready());

    proxy.sever.send(true)?;

    let outcome = tokio::time::timeout(Duration::from_secs(20), running.task).await??;
    assert!(
        matches!(outcome, Err(BusError::ConnectionLost)),
        "a lost connection must end the supervised future with an error, got {outcome:?}"
    );
    assert!(
        !running.lifecycle.is_ready(),
        "readiness must be false once the bus is gone"
    );
    database.cleanup().await?;
    Ok(())
}
