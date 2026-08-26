//! The append-only mutation audit trail and its outcome helpers.
//!
//! Every attempt - authorized or refused, succeeding, already-applied, or
//! failing - lands here keyed by an idempotency key that only successful
//! outcomes may claim. Failures never consume a key, so a retry after
/// failure can complete; refusals keep their account claim without a
/// foreign key because the trail records claims rather than vouching for
/// them.
use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::mutations::{
    MutationContext, MutationOutcome, MutationRequest, MutationStatus, RefusalReason,
};

/// Where an attempt ended in the audit vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuditOutcome {
    Applied,
    AlreadyApplied,
    Rejected,
    Failed,
}

impl AuditOutcome {
    /// The database representation of the outcome.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyApplied => "already_applied",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

/// Whether the idempotency key already owns a successful audit record.
pub(crate) async fn successful_outcome_exists(
    database: &Database,
    idempotency_key: &str,
) -> Result<bool, PersistenceError> {
    let exists: bool = sqlx::query_scalar(
        "select exists (
             select 1 from github_catalog.mutation_audit
             where idempotency_key = $1
               and outcome in ('applied', 'already_applied')
         )",
    )
    .bind(idempotency_key)
    .fetch_one(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(exists)
}

/// Inserts one audit row inside the caller's transaction and reports whether
/// it won the idempotency key (rows affected 1) or lost to a racing
/// duplicate (0, partial unique index).
pub(crate) async fn insert_audit_row(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    context: &MutationContext,
    repository_id: Uuid,
    kind: &str,
    idempotency_key: &str,
    outcome: AuditOutcome,
    detail: serde_json::Value,
) -> Result<u64, PersistenceError> {
    let inserted = sqlx::query(
        "insert into github_catalog.mutation_audit
             (audit_id, idempotency_key, account_id, repository_id,
              operation_kind, principal, source, outcome, detail)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb)
         on conflict do nothing",
    )
    .bind(Uuid::now_v7())
    .bind(idempotency_key)
    .bind(context.account_id)
    .bind(repository_id)
    .bind(kind)
    .bind(&context.principal)
    .bind(context.source.as_str())
    .bind(outcome.as_str())
    .bind(serde_json::to_string(&detail).unwrap_or_else(|_| "{}".to_owned()))
    .execute(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    Ok(inserted)
}

/// Records one attempt in its own transaction, resolving the local
/// repository identity first so every attempt references a real target.
pub(crate) async fn record_attempt(
    database: &Database,
    context: &MutationContext,
    request: &MutationRequest,
    kind: &'static str,
    outcome: AuditOutcome,
    detail: serde_json::Value,
) -> Result<(), PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let repository_id = crate::identity::upsert_repository_in_tx(
        &mut transaction,
        request.repository().provider_repository_id,
    )
    .await?;
    let _won = insert_audit_row(
        &mut transaction,
        context,
        repository_id,
        kind,
        request.idempotency_key(),
        outcome,
        detail,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Records an audited refusal and reports it as data.
pub(crate) async fn refused_outcome(
    database: &Database,
    context: &MutationContext,
    request: &MutationRequest,
    reason: RefusalReason,
) -> Result<MutationOutcome, crate::mutations::MutationError> {
    record_attempt(
        database,
        context,
        request,
        crate::mutations::kind_label(request),
        AuditOutcome::Rejected,
        serde_json::json!({ "reason": label(reason) }),
    )
    .await?;
    Ok(MutationOutcome {
        idempotency_key: request.idempotency_key().to_owned(),
        status: MutationStatus::Rejected { reason },
    })
}

/// Records an audited budget pause and reports it as data.
pub(crate) async fn paused_outcome(
    database: &Database,
    context: &MutationContext,
    request: &MutationRequest,
    retry_at: std::time::SystemTime,
) -> Result<MutationOutcome, crate::mutations::MutationError> {
    record_attempt(
        database,
        context,
        request,
        crate::mutations::kind_label(request),
        AuditOutcome::Failed,
        serde_json::json!({ "reason": format!("rate limited until {retry_at:?}") }),
    )
    .await?;
    Ok(MutationOutcome {
        idempotency_key: request.idempotency_key().to_owned(),
        status: MutationStatus::Failed {
            reason: "the account's provider budget is cooling down".to_owned(),
        },
    })
}

/// Records one classified failure against the audit trail and returns its
/// truthful outcome. Failures never occupy the idempotency key.
pub(crate) async fn failed_outcome(
    database: &Database,
    context: &MutationContext,
    kind: &'static str,
    provider_repository_id: i64,
    idempotency_key: &str,
    reason: String,
) -> Result<MutationOutcome, crate::mutations::MutationError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let repository_id =
        crate::identity::upsert_repository_in_tx(&mut transaction, provider_repository_id).await?;
    insert_audit_row(
        &mut transaction,
        context,
        repository_id,
        kind,
        idempotency_key,
        AuditOutcome::Failed,
        serde_json::json!({ "reason": reason }),
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(MutationOutcome {
        idempotency_key: idempotency_key.to_owned(),
        status: MutationStatus::Failed { reason },
    })
}

/// The database label of a refusal reason for the audit detail.
const fn label(reason: RefusalReason) -> &'static str {
    match reason {
        RefusalReason::AccountNotConnected => "account_not_connected",
        RefusalReason::MissingScope => "missing_scope",
        RefusalReason::AutoNotDirectlyRequestable => "auto_not_directly_requestable",
        RefusalReason::RepositoryCurrentlyStarred => "repository_currently_starred",
        RefusalReason::RepositoryIgnored => "repository_ignored",
    }
}
