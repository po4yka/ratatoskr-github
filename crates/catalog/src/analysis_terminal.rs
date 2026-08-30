//! Transactional projection of terminal Knowledge analysis facts.

use ratatoskr_github_contracts::{
    AnalysisFailureCode, RepositoryAnalysisCompleted, RepositoryAnalysisFailed,
    RepositoryAnalysisRevision,
};
use ratatoskr_identifiers::{RepositoryAnalysisRequestId, RepositoryId, TenantRef};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::watches::{TerminalFactOutcome, WatchError};
use crate::{InboxClaimOutcome, InboxDelivery, claim_inbox_delivery, retry_inbox_delivery};

const COMPLETED_SUBJECT: &str = "evt.knowledge.repository_analysis.completed.v1";
const FAILED_SUBJECT: &str = "evt.knowledge.repository_analysis.failed.v1";

/// Consumes one completion fact and links its opaque result to the matching pending request.
///
/// # Errors
///
/// Returns [`WatchError`] when persistence or payload serialization fails.
pub async fn consume_repository_analysis_completed(
    database: &Database,
    message_id: Uuid,
    completed: &RepositoryAnalysisCompleted,
) -> Result<TerminalFactOutcome, WatchError> {
    consume_terminal_fact(
        database,
        message_id,
        COMPLETED_SUBJECT,
        completed,
        "completed",
        Some(completed.analysis_result_ref.to_wire()),
        None,
        None,
    )
    .await
}

/// Consumes one failure fact and closes the matching pending request without a result reference.
///
/// # Errors
///
/// Returns [`WatchError`] when persistence or payload serialization fails.
pub async fn consume_repository_analysis_failed(
    database: &Database,
    message_id: Uuid,
    failed: &RepositoryAnalysisFailed,
) -> Result<TerminalFactOutcome, WatchError> {
    let failure_code = failure_code(failed.failure_code)?;
    consume_terminal_fact(
        database,
        message_id,
        FAILED_SUBJECT,
        failed,
        "failed",
        None,
        Some(failure_code),
        Some(failed.retryable),
    )
    .await
}

/// Consumes one exact completed-event broker delivery under a resumable inbox lease.
///
/// # Errors
///
/// Returns [`WatchError`] when inbox claiming, projection, or terminal commit fails.
pub async fn consume_repository_analysis_completed_delivery(
    database: &Database,
    delivery: &InboxDelivery<'_>,
    completed: &RepositoryAnalysisCompleted,
    now: OffsetDateTime,
    lease_duration: Duration,
    retry_delay: Duration,
) -> Result<TerminalFactOutcome, WatchError> {
    consume_terminal_delivery(
        database,
        delivery,
        completed,
        "completed",
        Some(completed.analysis_result_ref.to_wire()),
        None,
        None,
        now,
        lease_duration,
        retry_delay,
    )
    .await
}

/// Consumes one exact failed-event broker delivery under a resumable inbox lease.
///
/// # Errors
///
/// Returns [`WatchError`] when inbox claiming, projection, or terminal commit fails.
pub async fn consume_repository_analysis_failed_delivery(
    database: &Database,
    delivery: &InboxDelivery<'_>,
    failed: &RepositoryAnalysisFailed,
    now: OffsetDateTime,
    lease_duration: Duration,
    retry_delay: Duration,
) -> Result<TerminalFactOutcome, WatchError> {
    let failure_code = failure_code(failed.failure_code)?;
    consume_terminal_delivery(
        database,
        delivery,
        failed,
        "failed",
        None,
        Some(failure_code),
        Some(failed.retryable),
        now,
        lease_duration,
        retry_delay,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "the terminal contract supplies identity while the caller supplies its explicit state transition fields"
)]
async fn consume_terminal_fact<T>(
    database: &Database,
    message_id: Uuid,
    subject: &str,
    terminal: &T,
    status: &str,
    analysis_result_ref: Option<String>,
    failure_code: Option<&str>,
    retryable: Option<bool>,
) -> Result<TerminalFactOutcome, WatchError>
where
    T: serde::Serialize + TerminalIdentity,
{
    let envelope = serde_json::to_vec(terminal).map_err(WatchError::Serialization)?;
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let claimed = sqlx::query(
        "insert into github_catalog.inbox_events
             (message_id, subject, envelope, owner_ref, stream_name, consumer_name, stream_sequence,
              delivery_count, state)
         values ($1, $2, $3, $4, 'domain', $2,
                 (select coalesce(max(stream_sequence), 0) + 1 from github_catalog.inbox_events),
                 1, 'processing')
         on conflict do nothing",
    )
    .bind(message_id)
    .bind(subject)
    .bind(envelope)
    .bind(terminal.owner().to_string())
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if claimed == 0 {
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(TerminalFactOutcome::Duplicate);
    }
    let outcome = apply_terminal_projection(
        &mut tx,
        terminal,
        status,
        analysis_result_ref,
        failure_code,
        retryable,
    )
    .await?;
    sqlx::query(
        "update github_catalog.inbox_events
         set state = 'consumed', terminal_outcome = $2, consumed_at = now()
         where message_id = $1",
    )
    .bind(message_id)
    .bind(status)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(outcome)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the exact transport delivery and typed terminal transition are intentionally explicit"
)]
async fn consume_terminal_delivery<T>(
    database: &Database,
    delivery: &InboxDelivery<'_>,
    terminal: &T,
    status: &'static str,
    analysis_result_ref: Option<String>,
    failure_code: Option<&'static str>,
    retryable: Option<bool>,
    now: OffsetDateTime,
    lease_duration: Duration,
    retry_delay: Duration,
) -> Result<TerminalFactOutcome, WatchError>
where
    T: serde::Serialize + TerminalIdentity,
{
    let lease_owner = match claim_inbox_delivery(database, delivery, now, lease_duration).await? {
        InboxClaimOutcome::Claimed { lease_owner } => lease_owner,
        InboxClaimOutcome::TerminalDuplicate => return Ok(TerminalFactOutcome::Duplicate),
        InboxClaimOutcome::Busy => {
            return Err(PersistenceError::Query(sqlx::Error::Protocol(
                "inbox delivery is already processing".to_owned(),
            ))
            .into());
        }
    };
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let projected = apply_terminal_projection(
        &mut tx,
        terminal,
        status,
        analysis_result_ref,
        failure_code,
        retryable,
    )
    .await;
    let outcome = match projected {
        Ok(outcome) => outcome,
        Err(error) => {
            drop(tx);
            retry_inbox_delivery(
                database,
                delivery.message_id,
                lease_owner,
                "analysis_projection_failed",
                now + retry_delay,
            )
            .await?;
            return Err(error);
        }
    };
    let changed = sqlx::query(
        "update github_catalog.inbox_events
         set state='consumed',terminal_outcome=$3,consumed_at=$4,
             lease_owner=null,lease_expires_at=null,failure_code=null
         where message_id=$1 and lease_owner=$2 and state='processing'",
    )
    .bind(delivery.message_id)
    .bind(lease_owner)
    .bind(status)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if changed != 1 {
        return Err(PersistenceError::Query(sqlx::Error::Protocol(
            "inbox lease is not owned".to_owned(),
        ))
        .into());
    }
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(outcome)
}

async fn apply_terminal_projection<T>(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    terminal: &T,
    status: &str,
    analysis_result_ref: Option<String>,
    failure_code: Option<&str>,
    retryable: Option<bool>,
) -> Result<TerminalFactOutcome, WatchError>
where
    T: TerminalIdentity,
{
    let source_revision =
        serde_json::to_value(terminal.source_revision()).map_err(WatchError::Serialization)?;
    let result_ref_for_link = analysis_result_ref.clone();
    let changed = sqlx::query(
        "update github_catalog.repository_analysis_requests
         set status = $2, analysis_result_ref = $3, failure_code = $4, retryable = $5,
             terminal_at = now()
         where request_id = $1 and owner_ref = $6 and repository_id = $7
           and github_repository_numeric_id = $8 and source_revision = $9 and status = 'pending'",
    )
    .bind(
        terminal
            .request_id()
            .to_string()
            .parse::<Uuid>()
            .map_err(|_| WatchError::InvalidStoredIdentity)?,
    )
    .bind(status)
    .bind(analysis_result_ref)
    .bind(failure_code)
    .bind(retryable)
    .bind(terminal.owner().to_string())
    .bind(
        terminal
            .repository_id()
            .to_string()
            .parse::<Uuid>()
            .map_err(|_| WatchError::InvalidStoredIdentity)?,
    )
    .bind(
        i64::try_from(terminal.github_repository_numeric_id())
            .map_err(|_| WatchError::InvalidStoredIdentity)?,
    )
    .bind(source_revision)
    .execute(&mut **tx)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if changed == 1
        && status == "completed"
        && let Some(analysis_result_ref) = result_ref_for_link
    {
        sqlx::query(
            "insert into github_catalog.repository_analysis_links
                 (owner_ref, repository_id, request_id, analysis_result_ref, completed_at)
             values ($1, $2, $3, $4, now())
             on conflict (owner_ref, repository_id) do update set
                 request_id = excluded.request_id,
                 analysis_result_ref = excluded.analysis_result_ref,
                 completed_at = excluded.completed_at",
        )
        .bind(terminal.owner().to_string())
        .bind(
            terminal
                .repository_id()
                .to_string()
                .parse::<Uuid>()
                .map_err(|_| WatchError::InvalidStoredIdentity)?,
        )
        .bind(
            terminal
                .request_id()
                .to_string()
                .parse::<Uuid>()
                .map_err(|_| WatchError::InvalidStoredIdentity)?,
        )
        .bind(analysis_result_ref)
        .execute(&mut **tx)
        .await
        .map_err(PersistenceError::Query)?;
    }
    Ok(if changed == 1 {
        TerminalFactOutcome::Resolved
    } else {
        TerminalFactOutcome::Ignored
    })
}

fn failure_code(code: AnalysisFailureCode) -> Result<&'static str, WatchError> {
    match code {
        AnalysisFailureCode::SourceUnavailable => Ok("source_unavailable"),
        AnalysisFailureCode::ContractInvalid => Ok("contract_invalid"),
        AnalysisFailureCode::DependencyUnavailable => Ok("dependency_unavailable"),
        AnalysisFailureCode::NotAuthorized => Ok("not_authorized"),
        _ => Err(WatchError::InvalidStoredIdentity),
    }
}

trait TerminalIdentity {
    fn owner(&self) -> TenantRef;
    fn repository_id(&self) -> RepositoryId;
    fn github_repository_numeric_id(&self) -> u64;
    fn request_id(&self) -> RepositoryAnalysisRequestId;
    fn source_revision(&self) -> &RepositoryAnalysisRevision;
}

impl TerminalIdentity for RepositoryAnalysisCompleted {
    fn owner(&self) -> TenantRef {
        self.owner
    }

    fn repository_id(&self) -> RepositoryId {
        self.repository_id
    }

    fn github_repository_numeric_id(&self) -> u64 {
        self.github_repository_numeric_id
    }

    fn request_id(&self) -> RepositoryAnalysisRequestId {
        self.request_id
    }

    fn source_revision(&self) -> &RepositoryAnalysisRevision {
        &self.source_revision
    }
}

impl TerminalIdentity for RepositoryAnalysisFailed {
    fn owner(&self) -> TenantRef {
        self.owner
    }

    fn repository_id(&self) -> RepositoryId {
        self.repository_id
    }

    fn github_repository_numeric_id(&self) -> u64 {
        self.github_repository_numeric_id
    }

    fn request_id(&self) -> RepositoryAnalysisRequestId {
        self.request_id
    }

    fn source_revision(&self) -> &RepositoryAnalysisRevision {
        &self.source_revision
    }
}
