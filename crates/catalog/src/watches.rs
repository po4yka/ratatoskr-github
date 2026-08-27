//! User-owned metadata watches and durable Knowledge analysis request linkage.

use ratatoskr_github_contracts::{
    AnalysisFailureCode, ReadmeAbsenceReason, ReadmeRevision, RepositoryAnalysisAttributes,
    RepositoryAnalysisCompleted, RepositoryAnalysisContract, RepositoryAnalysisFailed,
    RepositoryAnalysisRequested, RepositoryAnalysisRevision, RepositoryDescription,
    RepositoryFullName, RepositoryLanguage,
};
use ratatoskr_identifiers::{
    ContentDigest, DigestAlgorithm, DigestHex, Extensions, RepositoryAnalysisRequestId,
    RepositoryId, TenantRef,
};
use sha2::{Digest as _, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::provider::ProviderRepositoryBody;

const REQUESTED_CONTRACT: &str = "repository_analysis";
const REQUESTED_SUBJECT: &str = "knowledge.repository_analysis.requested.v1";
const COMPLETED_SUBJECT: &str = "knowledge.repository_analysis.completed.v1";
const FAILED_SUBJECT: &str = "knowledge.repository_analysis.failed.v1";
const DISPATCH_SPACING: Duration = Duration::seconds(1);

/// Visible lifecycle state for a repository-analysis request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryAnalysisRequestStatus {
    /// The Catalog has accepted work but has not yet dispatched it to Knowledge.
    Queued,
    /// Knowledge has been asked and no terminal fact has been matched yet.
    Pending,
    /// Knowledge returned an opaque result reference.
    Completed,
    /// Knowledge returned a safe terminal failure.
    Failed,
}

/// The result of creating or re-enabling a metadata-delta watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchRegistration {
    /// Stable local identity of the registered watch.
    pub watch_id: Uuid,
}

/// Result of evaluating every enabled watch after a metadata observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvaluation {
    /// No enabled watch needed a newer revision.
    Unchanged,
    /// This many new immutable requests were durably queued.
    Queued {
        /// Count of requests created for the observed revision.
        requests: u64,
    },
}

/// Result of attempting one due request dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisDispatch {
    /// No queued request is due.
    NotDue,
    /// One request was moved to visible pending state and written to the outbox.
    Pending {
        /// Contract request identity.
        request_id: RepositoryAnalysisRequestId,
    },
}

/// Result of consuming a Knowledge terminal fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFactOutcome {
    /// One matching pending request moved to a terminal state.
    Resolved,
    /// The delivery was previously consumed.
    Duplicate,
    /// The fact did not match a pending request and changed nothing.
    Ignored,
}

/// Visible request state and optional opaque result linkage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisRequestState {
    /// Current pending or terminal state.
    pub status: RepositoryAnalysisRequestStatus,
    /// Knowledge-owned result reference after a successful completion.
    pub analysis_result_ref: Option<String>,
}

/// A safe watch or analysis-request persistence failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WatchError {
    /// Catalog metadata is required before a watch can establish its baseline.
    #[error("the repository has no metadata revision to watch")]
    MissingMetadata,
    /// Provider metadata cannot be represented by the bounded analysis contract.
    #[error("the observed repository metadata cannot be requested for analysis")]
    InvalidMetadata,
    /// A stored identity could not be reconstructed from Catalog-owned state.
    #[error("the stored repository-analysis identity is invalid")]
    InvalidStoredIdentity,
    /// The typed contract payload could not be serialized safely.
    #[error("the repository-analysis contract payload could not be serialized")]
    Serialization(#[source] serde_json::Error),
    /// Catalog-owned persistence failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// Registers or re-enables one user-owned metadata-delta analysis watch.
///
/// The current metadata hash becomes the baseline so registration never analyses old state.
///
/// # Errors
///
/// Returns [`WatchError`] when the repository has no metadata or persistence fails.
pub async fn register_repository_analysis_watch(
    database: &Database,
    owner: TenantRef,
    repository_id: Uuid,
) -> Result<WatchRegistration, WatchError> {
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let baseline: String = sqlx::query_scalar(
        "select content_hash from github_catalog.repository_metadata
         where repository_id = $1 for update",
    )
    .bind(repository_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?
    .ok_or(WatchError::MissingMetadata)?;
    let watch_id: Uuid = sqlx::query_scalar(
        "insert into github_catalog.repository_watches
             (watch_id, owner_ref, repository_id, trigger_type, downstream_action,
              enabled, last_evaluated_content_hash)
         values ($1, $2, $3, 'metadata_changed', 'repository_analysis', true, $4)
         on conflict (owner_ref, repository_id, trigger_type, downstream_action) do update set
             enabled = true,
             last_evaluated_content_hash = excluded.last_evaluated_content_hash,
             updated_at = now()
         returning watch_id",
    )
    .bind(Uuid::now_v7())
    .bind(owner.to_string())
    .bind(repository_id)
    .bind(baseline)
    .fetch_one(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(WatchRegistration { watch_id })
}

/// Pauses or re-enables a registered metadata-delta watch.
///
/// # Errors
///
/// Returns [`WatchError`] when persistence fails.
pub async fn set_repository_analysis_watch_enabled(
    database: &Database,
    watch_id: Uuid,
    enabled: bool,
) -> Result<(), WatchError> {
    sqlx::query(
        "update github_catalog.repository_watches set enabled = $2, updated_at = now()
         where watch_id = $1",
    )
    .bind(watch_id)
    .bind(enabled)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Queues all enabled watches whose metadata checkpoint differs from this body.
///
/// The request is durable but only the dispatcher writes it to the external outbox.
///
/// # Errors
///
/// Returns [`WatchError`] when metadata cannot form the shared contract or persistence fails.
#[expect(
    clippy::too_many_lines,
    reason = "one transaction must lock watches, allocate paced due times, enqueue requests, and advance their checkpoints together"
)]
pub async fn evaluate_metadata_watches(
    database: &Database,
    repository_id: Uuid,
    body: &ProviderRepositoryBody,
    now: OffsetDateTime,
) -> Result<WatchEvaluation, WatchError> {
    let attributes = analysis_attributes(body)?;
    let attributes_digest = digest_value(&attributes)?;
    let source_revision = RepositoryAnalysisRevision {
        attributes_digest: attributes_digest.clone(),
        readme: ReadmeRevision::Absent {
            reason: ReadmeAbsenceReason::NotPreserved,
        },
    };
    let repository = RepositoryId::parse(&repository_id.to_string())
        .map_err(|_| WatchError::InvalidStoredIdentity)?;
    let provider_id =
        u64::try_from(body.provider_repository_id).map_err(|_| WatchError::InvalidMetadata)?;
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let metadata_hash: String = sqlx::query_scalar(
        "select content_hash from github_catalog.repository_metadata where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?
    .ok_or(WatchError::MissingMetadata)?;
    let watches: Vec<(Uuid, String)> = sqlx::query_as(
        "select watch_id, owner_ref from github_catalog.repository_watches
         where repository_id = $1 and enabled
           and last_evaluated_content_hash is distinct from $2
         order by watch_id for update",
    )
    .bind(repository_id)
    .bind(&metadata_hash)
    .fetch_all(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    if watches.is_empty() {
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(WatchEvaluation::Unchanged);
    }
    sqlx::query(
        "insert into github_catalog.repository_analysis_dispatch_cursor (scope, next_not_before)
         values ('repository_analysis', $1) on conflict do nothing",
    )
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    let mut next_not_before: OffsetDateTime = sqlx::query_scalar(
        "select next_not_before from github_catalog.repository_analysis_dispatch_cursor
         where scope = 'repository_analysis' for update",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    let mut queued = 0_u64;
    for (watch_id, owner_ref) in watches {
        let owner = TenantRef::parse(&owner_ref).map_err(|_| WatchError::InvalidStoredIdentity)?;
        let request = analysis_request(
            owner,
            repository,
            provider_id,
            source_revision.clone(),
            attributes.clone(),
        )?;
        let payload = serde_json::to_value(&request).map_err(WatchError::Serialization)?;
        let due = next_not_before.max(now);
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "insert into github_catalog.repository_analysis_requests
                 (request_id, watch_id, owner_ref, repository_id, github_repository_numeric_id,
                  source_revision, repository_attributes, request_payload, attributes_digest_hex,
                  idempotency_digest_hex, requested_contract, not_before)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'repository_analysis', $11)
             on conflict (watch_id, attributes_digest_hex, requested_contract) do nothing
             returning request_id",
        )
        .bind(
            request
                .request_id
                .to_string()
                .parse::<Uuid>()
                .map_err(|_| WatchError::InvalidStoredIdentity)?,
        )
        .bind(watch_id)
        .bind(owner_ref)
        .bind(repository_id)
        .bind(i64::try_from(provider_id).map_err(|_| WatchError::InvalidMetadata)?)
        .bind(serde_json::to_value(&request.source_revision).map_err(WatchError::Serialization)?)
        .bind(
            serde_json::to_value(&request.repository_attributes)
                .map_err(WatchError::Serialization)?,
        )
        .bind(payload)
        .bind(request.source_revision.attributes_digest.hex.as_str())
        .bind(request.idempotency_key.hex.as_str())
        .bind(due)
        .fetch_optional(&mut *tx)
        .await
        .map_err(PersistenceError::Query)?;
        sqlx::query(
            "update github_catalog.repository_watches
             set last_evaluated_content_hash = $2, updated_at = now() where watch_id = $1",
        )
        .bind(watch_id)
        .bind(&metadata_hash)
        .execute(&mut *tx)
        .await
        .map_err(PersistenceError::Query)?;
        if inserted.is_some() {
            queued = queued.saturating_add(1);
            next_not_before = due + DISPATCH_SPACING;
        }
    }
    if queued > 0 {
        sqlx::query(
            "update github_catalog.repository_analysis_dispatch_cursor
             set next_not_before = $1 where scope = 'repository_analysis'",
        )
        .bind(next_not_before)
        .execute(&mut *tx)
        .await
        .map_err(PersistenceError::Query)?;
    }
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(WatchEvaluation::Queued { requests: queued })
}

/// Dispatches at most one due request into the transactional outbox.
///
/// # Errors
///
/// Returns [`WatchError`] when owned persistence fails.
pub async fn dispatch_due_repository_analysis(
    database: &Database,
    now: OffsetDateTime,
) -> Result<AnalysisDispatch, WatchError> {
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let request: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select request_id, request_payload
         from github_catalog.repository_analysis_requests
         where status = 'queued' and not_before <= $1
         order by not_before, request_id
         limit 1 for update skip locked",
    )
    .bind(now)
    .fetch_optional(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    let Some((request_id, payload)) = request else {
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(AnalysisDispatch::NotDue);
    };
    let outbox_message_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.outbox_events (message_id, subject, payload)
         values ($1, $2, $3)",
    )
    .bind(outbox_message_id)
    .bind(REQUESTED_SUBJECT)
    .bind(payload)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    sqlx::query(
        "update github_catalog.repository_analysis_requests
         set status = 'pending', outbox_message_id = $2 where request_id = $1",
    )
    .bind(request_id)
    .bind(outbox_message_id)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(AnalysisDispatch::Pending {
        request_id: RepositoryAnalysisRequestId::parse(&request_id.to_string())
            .map_err(|_| WatchError::InvalidStoredIdentity)?,
    })
}

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
    let failure_code = match failed.failure_code {
        AnalysisFailureCode::SourceUnavailable => "source_unavailable",
        AnalysisFailureCode::ContractInvalid => "contract_invalid",
        AnalysisFailureCode::DependencyUnavailable => "dependency_unavailable",
        AnalysisFailureCode::NotAuthorized => "not_authorized",
        _ => return Err(WatchError::InvalidStoredIdentity),
    };
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

/// Reads the visible pending/terminal state for one request.
///
/// # Errors
///
/// Returns [`WatchError`] when persistence fails or stored state is invalid.
pub async fn repository_analysis_request_state(
    database: &Database,
    request_id: RepositoryAnalysisRequestId,
) -> Result<Option<AnalysisRequestState>, WatchError> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "select status, analysis_result_ref from github_catalog.repository_analysis_requests
         where request_id = $1",
    )
    .bind(
        request_id
            .to_string()
            .parse::<Uuid>()
            .map_err(|_| WatchError::InvalidStoredIdentity)?,
    )
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    row.map(|(status, analysis_result_ref)| {
        let status = match status.as_str() {
            "queued" => RepositoryAnalysisRequestStatus::Queued,
            "pending" => RepositoryAnalysisRequestStatus::Pending,
            "completed" => RepositoryAnalysisRequestStatus::Completed,
            "failed" => RepositoryAnalysisRequestStatus::Failed,
            _ => return Err(WatchError::InvalidStoredIdentity),
        };
        Ok(AnalysisRequestState {
            status,
            analysis_result_ref,
        })
    })
    .transpose()
}

fn analysis_attributes(
    body: &ProviderRepositoryBody,
) -> Result<RepositoryAnalysisAttributes, WatchError> {
    let description = body
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(RepositoryDescription::parse)
        .transpose()
        .map_err(|_| WatchError::InvalidMetadata)?;
    let primary_language = body
        .language
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(RepositoryLanguage::parse)
        .transpose()
        .map_err(|_| WatchError::InvalidMetadata)?;
    Ok(RepositoryAnalysisAttributes {
        repository_full_name: RepositoryFullName::parse(&body.full_name)
            .map_err(|_| WatchError::InvalidMetadata)?,
        description,
        primary_language,
    })
}

fn analysis_request(
    owner: TenantRef,
    repository_id: RepositoryId,
    github_repository_numeric_id: u64,
    source_revision: RepositoryAnalysisRevision,
    repository_attributes: RepositoryAnalysisAttributes,
) -> Result<RepositoryAnalysisRequested, WatchError> {
    let idempotency_key = digest_value(&serde_json::json!({
        "owner": owner,
        "repository_id": repository_id,
        "github_repository_numeric_id": github_repository_numeric_id,
        "source_revision": source_revision,
        "repository_attributes": repository_attributes,
        "requested_contract": REQUESTED_CONTRACT,
    }))?;
    let request_id = RepositoryAnalysisRequestId::parse(&Uuid::now_v7().to_string())
        .map_err(|_| WatchError::InvalidStoredIdentity)?;
    Ok(RepositoryAnalysisRequested {
        owner,
        repository_id,
        github_repository_numeric_id,
        request_id,
        source_revision,
        repository_attributes,
        requested_contract: RepositoryAnalysisContract::RepositoryAnalysis,
        idempotency_key,
        extensions: Extensions::new(),
    })
}

fn digest_value(value: &impl serde::Serialize) -> Result<ContentDigest, WatchError> {
    let bytes = serde_json::to_vec(value).map_err(WatchError::Serialization)?;
    let hex = format!("{:x}", Sha256::digest(bytes));
    Ok(ContentDigest {
        algorithm: DigestAlgorithm::Sha256,
        hex: DigestHex::parse(&hex).map_err(|_| WatchError::InvalidMetadata)?,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the terminal contract supplies identity while the caller supplies its explicit state transition fields"
)]
#[expect(
    clippy::too_many_lines,
    reason = "inbox claiming, exact matching, result linkage, and acknowledgement must commit atomically"
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
    let payload = serde_json::to_value(terminal).map_err(WatchError::Serialization)?;
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let claimed = sqlx::query(
        "insert into github_catalog.inbox_events (message_id, subject, payload)
         values ($1, $2, $3) on conflict do nothing",
    )
    .bind(message_id)
    .bind(subject)
    .bind(payload)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if claimed == 0 {
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(TerminalFactOutcome::Duplicate);
    }
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
    .execute(&mut *tx)
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
        .execute(&mut *tx)
        .await
        .map_err(PersistenceError::Query)?;
    }
    sqlx::query("update github_catalog.inbox_events set consumed_at = now() where message_id = $1")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(PersistenceError::Query)?;
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(if changed == 1 {
        TerminalFactOutcome::Resolved
    } else {
        TerminalFactOutcome::Ignored
    })
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
