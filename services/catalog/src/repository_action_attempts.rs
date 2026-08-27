//! Durable replay boundary for confirmed repository actions.

use ratatoskr_github_catalog::Database;
use ratatoskr_github_contracts::{RepositoryActionRequest, RepositoryActionResult};

/// Result of atomically claiming an owner/key pair.
pub(crate) enum ActionClaim {
    /// This caller inserted the in-progress row and may execute the action.
    Execute,
    /// An exact completed request already has recorded component truth.
    Replay(RepositoryActionResult),
    /// The key belongs to another request/owner or is still in progress.
    Conflict,
}

/// Storage failures at the replay boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ActionAttemptError {
    /// The request or result could not be represented as bounded JSON.
    #[error("repository action JSON could not be represented")]
    Json(#[from] serde_json::Error),
    /// `PostgreSQL` could not read or write the action attempt.
    #[error("repository action attempt persistence failed")]
    Persistence(#[from] sqlx::Error),
}

/// Claims one action or returns its exact terminal replay/conflict state.
pub(crate) async fn claim_action(
    database: &Database,
    owner_ref: &str,
    request: &RepositoryActionRequest,
) -> Result<ActionClaim, ActionAttemptError> {
    let fingerprint = serde_json::to_value(request)?;
    let inserted = sqlx::query(
        "insert into github_catalog.repository_action_attempts
             (owner_ref, idempotency_key, request_fingerprint, mode,
              github_repository_numeric_id, repository_full_name, canonical_url,
              account_ref, confirmation_evidence_ref)
         values ($1, $2, $3, $4, $5::numeric, $6, $7, $8, $9)
         on conflict do nothing",
    )
    .bind(owner_ref)
    .bind(request.idempotency_key.as_str())
    .bind(&fingerprint)
    .bind(action_mode(request))
    .bind(
        request
            .target
            .github_repository_numeric_id
            .get()
            .to_string(),
    )
    .bind(request.target.repository_full_name.as_str())
    .bind(request.target.canonical_url.as_str())
    .bind(
        request
            .account_ref
            .as_ref()
            .map(ratatoskr_github_contracts::GitHubAccountRef::as_str),
    )
    .bind(request.confirmation_evidence_ref.as_str())
    .execute(database.pool())
    .await?
    .rows_affected();
    if inserted == 1 {
        return Ok(ActionClaim::Execute);
    }

    let existing =
        sqlx::query_as::<_, (String, serde_json::Value, String, Option<serde_json::Value>)>(
            "select owner_ref, request_fingerprint, status, result
         from github_catalog.repository_action_attempts
         where idempotency_key = $1",
        )
        .bind(request.idempotency_key.as_str())
        .fetch_optional(database.pool())
        .await?;
    let Some((existing_owner, existing_fingerprint, status, result)) = existing else {
        return Ok(ActionClaim::Conflict);
    };
    if existing_owner != owner_ref || existing_fingerprint != fingerprint {
        return Ok(ActionClaim::Conflict);
    }
    match (status.as_str(), result) {
        ("completed", Some(result)) => Ok(ActionClaim::Replay(serde_json::from_value(result)?)),
        _ => Ok(ActionClaim::Conflict),
    }
}

/// Records one terminal safe result. The caller may still return its observed
/// result if this persistence step fails after a provider-confirmed write.
pub(crate) async fn complete_action(
    database: &Database,
    owner_ref: &str,
    request: &RepositoryActionRequest,
    result: &RepositoryActionResult,
) -> Result<(), ActionAttemptError> {
    let fingerprint = serde_json::to_value(request)?;
    let result = serde_json::to_value(result)?;
    let updated = sqlx::query(
        "update github_catalog.repository_action_attempts
         set status = 'completed', result = $4, completed_at = now()
         where owner_ref = $1 and idempotency_key = $2
           and request_fingerprint = $3 and status = 'in_progress'",
    )
    .bind(owner_ref)
    .bind(request.idempotency_key.as_str())
    .bind(fingerprint)
    .bind(result)
    .execute(database.pool())
    .await?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(ActionAttemptError::Persistence(sqlx::Error::RowNotFound))
    }
}

fn action_mode(request: &RepositoryActionRequest) -> &'static str {
    use ratatoskr_github_contracts::RepositoryActionCapability;

    match request.mode {
        RepositoryActionCapability::Metadata => "metadata",
        RepositoryActionCapability::Track => "track",
        RepositoryActionCapability::Star => "star",
        _ => "unsupported",
    }
}
