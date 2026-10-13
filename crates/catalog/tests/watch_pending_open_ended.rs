//! A dispatched repository-analysis request stays `pending` for as long as Knowledge has not settled it (XR-021 CONTRACTS.md R6-01 rule 4).
//!
//! These are characterization tests of behaviour the Knowledge repository handler relies on: it
//! redelivers an operator-fixable provider failure at every delivery and settles nothing, so from
//! this side the request is simply `pending` for as long as the outage lasts. Time is an explicit
//! parameter of the dispatch and the evaluation, so each test advances it by an offset from
//! `fixed_now()` and never reads the real clock.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "assertions and synthetic fixture construction in a test binary"
)]

use ratatoskr_github_catalog::provider::ProviderRepositoryBody;
use ratatoskr_github_catalog::test_support::{TestDatabase, record_body};
use ratatoskr_github_catalog::{
    AnalysisDispatch, RepositoryAnalysisRequestStatus, TerminalFactOutcome, WatchBaseline,
    WatchEvaluation, consume_repository_analysis_completed, consume_repository_analysis_failed,
    dispatch_due_repository_analysis, evaluate_metadata_watches,
    register_repository_analysis_watch, repository_analysis_request_state,
};
use ratatoskr_github_contracts::{
    AnalysisFailureCode, RepositoryAnalysisCompleted, RepositoryAnalysisFailed,
    RepositoryAnalysisRequested,
};
use ratatoskr_identifiers::{
    EntityRef, Extensions, RepositoryAnalysisRequestId, TenantRef, WireTimestamp,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000961";
const PROVIDER_ID: i64 = 961_001;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339).expect("fixed RFC3339 instant")
}

fn fixed_wire_instant() -> WireTimestamp {
    WireTimestamp::parse("2026-01-01T00:00:00Z").expect("fixed wire instant")
}

fn body(description: &str) -> ProviderRepositoryBody {
    ProviderRepositoryBody {
        provider_repository_id: PROVIDER_ID,
        full_name: "acme/waiting".to_owned(),
        description: Some(description.to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 3,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-27T00:00:00Z".to_owned()),
    }
}

/// A repository with a watch whose first changed revision was queued, dispatched and left waiting.
struct Waiting {
    database: TestDatabase,
    repository_id: Uuid,
    request_id: RepositoryAnalysisRequestId,
    outbox_message_id: Uuid,
}

async fn waiting_request() -> Result<Waiting, Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, PROVIDER_ID).await?;
    record_body(
        &database.database,
        repository.repository_id,
        &body("before"),
        None,
    )
    .await?;
    register_repository_analysis_watch(
        &database.database,
        TenantRef::parse(OWNER)?,
        repository.repository_id,
        WatchBaseline::Current,
    )
    .await?;
    let request_id =
        queue_and_dispatch(&database, repository.repository_id, "after", fixed_now()).await?;
    let outbox_message_id = outbox_message_id(&database, request_id).await?;
    Ok(Waiting {
        database,
        repository_id: repository.repository_id,
        request_id,
        outbox_message_id,
    })
}

/// Records a changed revision, queues the request it creates and dispatches it.
async fn queue_and_dispatch(
    database: &TestDatabase,
    repository_id: Uuid,
    description: &str,
    now: OffsetDateTime,
) -> Result<RepositoryAnalysisRequestId, Box<dyn std::error::Error>> {
    record_body(&database.database, repository_id, &body(description), None).await?;
    assert_eq!(
        evaluate_metadata_watches(&database.database, repository_id, now).await?,
        WatchEvaluation::Queued { requests: 1 }
    );
    let AnalysisDispatch::Pending { request_id } =
        dispatch_due_repository_analysis(&database.database, now + Duration::seconds(5)).await?
    else {
        return Err("one queued request must dispatch".into());
    };
    Ok(request_id)
}

async fn outbox_message_id(
    database: &TestDatabase,
    request_id: RepositoryAnalysisRequestId,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "select outbox_message_id from github_catalog.repository_analysis_requests
         where request_id = $1",
    )
    .bind(request_id.to_string().parse::<Uuid>()?)
    .fetch_one(database.database.pool())
    .await?)
}

async fn requested(
    database: &TestDatabase,
    request_id: RepositoryAnalysisRequestId,
) -> Result<RepositoryAnalysisRequested, Box<dyn std::error::Error>> {
    let payload: serde_json::Value = sqlx::query_scalar(
        "select request_payload from github_catalog.repository_analysis_requests
         where request_id = $1",
    )
    .bind(request_id.to_string().parse::<Uuid>()?)
    .fetch_one(database.database.pool())
    .await?;
    Ok(serde_json::from_value(payload)?)
}

fn completion_for(
    request: &RepositoryAnalysisRequested,
    result: &EntityRef,
) -> RepositoryAnalysisCompleted {
    RepositoryAnalysisCompleted {
        owner: request.owner,
        repository_id: request.repository_id,
        github_repository_numeric_id: request.github_repository_numeric_id,
        request_id: request.request_id,
        source_revision: request.source_revision.clone(),
        analysis_result_ref: result.clone(),
        completed_at: fixed_wire_instant(),
        extensions: Extensions::new(),
    }
}

fn failure_for(request: &RepositoryAnalysisRequested) -> RepositoryAnalysisFailed {
    RepositoryAnalysisFailed {
        owner: request.owner,
        repository_id: request.repository_id,
        github_repository_numeric_id: request.github_repository_numeric_id,
        request_id: request.request_id,
        source_revision: request.source_revision.clone(),
        failure_code: AnalysisFailureCode::DependencyUnavailable,
        retryable: true,
        failed_at: fixed_wire_instant(),
        extensions: Extensions::new(),
    }
}

async fn status_of(
    database: &TestDatabase,
    request_id: RepositoryAnalysisRequestId,
) -> Result<RepositoryAnalysisRequestStatus, Box<dyn std::error::Error>> {
    Ok(
        repository_analysis_request_state(&database.database, request_id)
            .await?
            .expect("the request stays visible")
            .status,
    )
}

async fn analysis_messages(database: &TestDatabase) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from github_catalog.outbox_events
         where subject = 'evt.knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await
}

async fn request_count(database: &TestDatabase) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("select count(*) from github_catalog.repository_analysis_requests")
        .fetch_one(database.database.pool())
        .await
}

#[tokio::test]
async fn a_dispatched_request_stays_pending_indefinitely() -> TestResult {
    let waiting = waiting_request().await?;
    assert_eq!(
        status_of(&waiting.database, waiting.request_id).await?,
        RepositoryAnalysisRequestStatus::Pending
    );

    for later in [Duration::hours(1), Duration::hours(24), Duration::days(30)] {
        let now = fixed_now() + later;
        assert_eq!(
            dispatch_due_repository_analysis(&waiting.database.database, now).await?,
            AnalysisDispatch::NotDue,
            "a pending request is never dispatched again, {later} later"
        );
        assert_eq!(
            evaluate_metadata_watches(&waiting.database.database, waiting.repository_id, now)
                .await?,
            WatchEvaluation::Unchanged,
            "the checkpoint already covers the revision, {later} later"
        );
        assert_eq!(
            status_of(&waiting.database, waiting.request_id).await?,
            RepositoryAnalysisRequestStatus::Pending,
            "nothing ages a pending request, {later} later"
        );
        assert_eq!(
            outbox_message_id(&waiting.database, waiting.request_id).await?,
            waiting.outbox_message_id
        );
        assert_eq!(analysis_messages(&waiting.database).await?, 1);
    }

    waiting.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_later_completion_resolves_the_waiting_request_once() -> TestResult {
    let waiting = waiting_request().await?;
    let wait_over = fixed_now() + Duration::days(30);
    assert_eq!(
        dispatch_due_repository_analysis(&waiting.database.database, wait_over).await?,
        AnalysisDispatch::NotDue
    );
    let request = requested(&waiting.database, waiting.request_id).await?;
    let result = EntityRef::parse("analysis:018f0000-0000-7000-8000-000000000962")?;
    let completion = completion_for(&request, &result);
    let message_id = Uuid::now_v7();

    assert_eq!(
        consume_repository_analysis_completed(&waiting.database.database, message_id, &completion)
            .await?,
        TerminalFactOutcome::Resolved
    );
    assert_eq!(
        consume_repository_analysis_completed(&waiting.database.database, message_id, &completion)
            .await?,
        TerminalFactOutcome::Duplicate
    );

    let state = repository_analysis_request_state(&waiting.database.database, waiting.request_id)
        .await?
        .expect("the request stays visible");
    assert_eq!(state.status, RepositoryAnalysisRequestStatus::Completed);
    assert_eq!(
        state.analysis_result_ref.as_deref(),
        Some(result.to_wire().as_str())
    );
    let linked: Vec<String> = sqlx::query_scalar(
        "select analysis_result_ref from github_catalog.repository_analysis_links
         where repository_id = $1",
    )
    .bind(waiting.repository_id)
    .fetch_all(waiting.database.database.pool())
    .await?;
    assert_eq!(linked, vec![result.to_wire()]);

    waiting.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_metadata_change_during_the_wait_queues_a_second_request() -> TestResult {
    let waiting = waiting_request().await?;
    let first = requested(&waiting.database, waiting.request_id).await?;
    let hour_later = fixed_now() + Duration::hours(1);

    record_body(
        &waiting.database.database,
        waiting.repository_id,
        &body("changed while waiting"),
        None,
    )
    .await?;
    assert_eq!(
        evaluate_metadata_watches(
            &waiting.database.database,
            waiting.repository_id,
            hour_later
        )
        .await?,
        WatchEvaluation::Queued { requests: 1 }
    );
    assert_eq!(request_count(&waiting.database).await?, 2);
    assert_eq!(
        status_of(&waiting.database, waiting.request_id).await?,
        RepositoryAnalysisRequestStatus::Pending,
        "the first request is not touched by the second"
    );
    let AnalysisDispatch::Pending {
        request_id: second_id,
    } = dispatch_due_repository_analysis(
        &waiting.database.database,
        hour_later + Duration::seconds(5),
    )
    .await?
    else {
        return Err("the second queued request must dispatch".into());
    };
    assert_ne!(second_id, waiting.request_id);
    assert_eq!(analysis_messages(&waiting.database).await?, 2);
    let second = requested(&waiting.database, second_id).await?;
    assert_ne!(
        second.idempotency_key, first.idempotency_key,
        "a new revision gets a new idempotency key"
    );

    // A fact that carries the first request's identity with the second's revision matches nothing.
    let result_second = EntityRef::parse("analysis:018f0000-0000-7000-8000-000000000963")?;
    let mismatched = RepositoryAnalysisCompleted {
        source_revision: second.source_revision.clone(),
        ..completion_for(&first, &result_second)
    };
    assert_eq!(
        consume_repository_analysis_completed(
            &waiting.database.database,
            Uuid::now_v7(),
            &mismatched
        )
        .await?,
        TerminalFactOutcome::Ignored
    );
    assert_eq!(
        status_of(&waiting.database, waiting.request_id).await?,
        RepositoryAnalysisRequestStatus::Pending
    );

    // Completing the second resolves only the second.
    assert_eq!(
        consume_repository_analysis_completed(
            &waiting.database.database,
            Uuid::now_v7(),
            &completion_for(&second, &result_second)
        )
        .await?,
        TerminalFactOutcome::Resolved
    );
    assert_eq!(
        status_of(&waiting.database, second_id).await?,
        RepositoryAnalysisRequestStatus::Completed
    );
    assert_eq!(
        status_of(&waiting.database, waiting.request_id).await?,
        RepositoryAnalysisRequestStatus::Pending,
        "the first request keeps waiting for its own fact"
    );

    // The first still resolves with its own fact.
    let result_first = EntityRef::parse("analysis:018f0000-0000-7000-8000-000000000964")?;
    assert_eq!(
        consume_repository_analysis_completed(
            &waiting.database.database,
            Uuid::now_v7(),
            &completion_for(&first, &result_first)
        )
        .await?,
        TerminalFactOutcome::Resolved
    );
    assert_eq!(
        status_of(&waiting.database, waiting.request_id).await?,
        RepositoryAnalysisRequestStatus::Completed
    );

    waiting.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_retryable_failed_fact_is_terminal_until_the_metadata_changes() -> TestResult {
    let waiting = waiting_request().await?;
    let request = requested(&waiting.database, waiting.request_id).await?;

    assert_eq!(
        consume_repository_analysis_failed(
            &waiting.database.database,
            Uuid::now_v7(),
            &failure_for(&request)
        )
        .await?,
        TerminalFactOutcome::Resolved
    );

    let (status, code, retryable): (String, Option<String>, Option<bool>) = sqlx::query_as(
        "select status, failure_code, retryable
         from github_catalog.repository_analysis_requests where request_id = $1",
    )
    .bind(waiting.request_id.to_string().parse::<Uuid>()?)
    .fetch_one(waiting.database.database.pool())
    .await?;
    assert_eq!(status, "failed");
    assert_eq!(code.as_deref(), Some("dependency_unavailable"));
    assert_eq!(retryable, Some(true));

    let (checkpoint, stored): (Option<String>, String) = sqlx::query_as(
        "select w.last_evaluated_content_hash, m.content_hash
         from github_catalog.repository_watches w
         join github_catalog.repository_metadata m using (repository_id)
         where w.repository_id = $1",
    )
    .bind(waiting.repository_id)
    .fetch_one(waiting.database.database.pool())
    .await?;
    assert_eq!(checkpoint.as_deref(), Some(stored.as_str()));

    assert_eq!(
        evaluate_metadata_watches(
            &waiting.database.database,
            waiting.repository_id,
            fixed_now() + Duration::days(30)
        )
        .await?,
        WatchEvaluation::Unchanged,
        "a retryable failure does not re-queue the unchanged revision"
    );
    assert_eq!(request_count(&waiting.database).await?, 1);
    assert_eq!(
        dispatch_due_repository_analysis(
            &waiting.database.database,
            fixed_now() + Duration::days(30)
        )
        .await?,
        AnalysisDispatch::NotDue
    );

    waiting.database.cleanup().await?;
    Ok(())
}
