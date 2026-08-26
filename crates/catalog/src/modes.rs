//! Repository modes: whose decision governs each catalog entry.
//!
//! A repository is `auto` (presence governed by star state), `tracked`
//! (explicitly kept without a star), or `ignored` (deliberately excluded);
//! the absence of a mode means known but never classified. Explicit requests
//! may set only `tracked` and `ignored` - `auto` is reached solely through
//! star effects, such as the first star observation over an unclassified
//! entry or a star mutation. An ignored repository must be unstarred first,
//! and starring cannot bypass an ignore. Every validated transition - no-op
//! confirmations included - leaves one audit entry, and a retried request
//! with the same idempotency key converges on the recorded truth.

use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::mutation_trail::{AuditOutcome, insert_audit_row, successful_outcome_exists};
use crate::mutations::{
    MutationContext, MutationError, MutationOutcome, MutationStatus, RefusalReason,
};

/// The mode vocabulary as callers may request it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedMode {
    /// Star-driven governance; never directly requestable.
    Auto,
    /// Explicitly keep the entry regardless of stars.
    Tracked,
    /// Deliberately exclude the entry.
    Ignored,
}

impl RequestedMode {
    /// The database representation of the mode.
    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Auto => Some("auto"),
            Self::Tracked => Some("tracked"),
            Self::Ignored => Some("ignored"),
        }
    }
}

/// One requested mode transition with its replay identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetModeRequest {
    /// The stable GitHub numeric repository id.
    pub provider_repository_id: i64,
    /// The requested mode.
    pub mode: RequestedMode,
    /// The operation's idempotency key.
    pub idempotency_key: String,
}

/// Applies one authorized mode transition and reports its truthful outcome.
///
/// # Errors
///
/// Returns [`MutationError`] when the audit trail itself cannot be read or
/// written; refusals and confirmations arrive as [`MutationOutcome`] data.
pub async fn set_repository_mode(
    database: &Database,
    context: &MutationContext,
    request: SetModeRequest,
) -> Result<MutationOutcome, MutationError> {
    let outcome_label = "mode_set";

    // A consumed key replays its recorded confirmation.
    if successful_outcome_exists(database, &request.idempotency_key).await? {
        return Ok(MutationOutcome {
            idempotency_key: request.idempotency_key,
            status: MutationStatus::AlreadyApplied,
        });
    }

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let repository_id =
        crate::identity::upsert_repository_in_tx(&mut transaction, request.provider_repository_id)
            .await?;

    let current_mode: Option<String> =
        sqlx::query_scalar("select mode from github_catalog.repositories where repository_id = $1")
            .bind(repository_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(PersistenceError::Query)?;

    let starred: bool = sqlx::query_scalar(
        "select exists (
             select 1 from github_catalog.current_star_state
             where account_id = $1 and repository_id = $2 and starred
         )",
    )
    .bind(context.account_id)
    .bind(repository_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;

    // Validation before any state change; refusals are audited in the same
    // transaction even though nothing changed.
    if let Some(label) = validate(request.mode, current_mode.as_deref(), starred) {
        return mode_refused(
            transaction,
            context,
            repository_id,
            outcome_label,
            &request.idempotency_key,
            label,
        )
        .await;
    }

    let unchanged = current_mode.as_deref() == request.mode.as_str();
    if !unchanged {
        sqlx::query(
            "update github_catalog.repositories
             set mode = $2, updated_at = now()
             where repository_id = $1",
        )
        .bind(repository_id)
        .bind(request.mode.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(PersistenceError::Query)?;
        crate::backup_policy::mark_backup_policy_dirty_in_tx(&mut transaction)
            .await
            .map_err(MutationError::from)?;
    }

    let inserted = insert_audit_row(
        &mut transaction,
        context,
        repository_id,
        outcome_label,
        &request.idempotency_key,
        if unchanged {
            AuditOutcome::AlreadyApplied
        } else {
            AuditOutcome::Applied
        },
        serde_json::json!({
            "from": mode_label(current_mode.as_deref()),
            "to": mode_label(request.mode.as_str()),
        }),
    )
    .await?;
    if inserted == 0 {
        // A racing duplicate won the key; its recorded truth stands.
        transaction
            .rollback()
            .await
            .map_err(PersistenceError::Query)?;
        return Ok(MutationOutcome {
            idempotency_key: request.idempotency_key,
            status: MutationStatus::AlreadyApplied,
        });
    }
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;

    Ok(MutationOutcome {
        idempotency_key: request.idempotency_key,
        status: if unchanged {
            MutationStatus::AlreadyApplied
        } else {
            MutationStatus::Applied
        },
    })
}

/// The enforcement matrix for explicit mode requests. Returns the audit
/// label of the violated rule when the request must be refused.
const fn validate(
    requested: RequestedMode,
    _current: Option<&str>,
    starred: bool,
) -> Option<&'static str> {
    match requested {
        RequestedMode::Auto => Some("auto_not_directly_requestable"),
        RequestedMode::Ignored if starred => Some("repository_currently_starred"),
        // Tracked is always acceptable; ignored over an unstarred entry too.
        // Same-value re-requests are no-op confirmations, not refusals.
        RequestedMode::Tracked | RequestedMode::Ignored => None,
    }
}

/// The audit label for any mode value, unclassified included.
fn mode_label(mode: Option<&str>) -> &'static str {
    match mode {
        Some("auto") => "auto",
        Some("tracked") => "tracked",
        Some("ignored") => "ignored",
        _ => "unclassified",
    }
}

/// Audits one refused mode request inside its transaction and reports the
/// rejection as data.
async fn mode_refused(
    mut transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    context: &MutationContext,
    repository_id: Uuid,
    operation_label: &'static str,
    idempotency_key: &str,
    label: &'static str,
) -> Result<MutationOutcome, MutationError> {
    insert_audit_row(
        &mut transaction,
        context,
        repository_id,
        operation_label,
        idempotency_key,
        AuditOutcome::Rejected,
        serde_json::json!({ "reason": label }),
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(MutationOutcome {
        idempotency_key: idempotency_key.to_owned(),
        status: MutationStatus::Rejected {
            reason: refusal_from_label(label),
        },
    })
}

/// Maps an audit label back to its public refusal reason.
fn refusal_from_label(label: &'static str) -> RefusalReason {
    match label {
        "auto_not_directly_requestable" => RefusalReason::AutoNotDirectlyRequestable,
        _ => RefusalReason::RepositoryCurrentlyStarred,
    }
}
