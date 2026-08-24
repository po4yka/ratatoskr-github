//! The metadata observe flow: identity, aliases, conditional fetch, rate
//! budget, and metadata persistence composed into one operation.

use std::time::SystemTime;

use uuid::Uuid;

use crate::database::Database;
use crate::identity::{AliasKind, apply_alias_observation, resolve_alias, upsert_repository};
use crate::metadata::{apply_fresh_body, apply_not_modified};
use crate::provider::OwnerName;
use crate::rate_limit::{AcquireError, RateLimitHeaders, RateLimitLedger, TokenRef};

/// What one observe operation established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// A fresh payload was applied to the catalog.
    Observed {
        /// The internal repository identity.
        repository_id: Uuid,
    },
    /// The provider confirmed the stored validator; state stayed untouched.
    NotModified {
        /// The internal repository identity.
        repository_id: Uuid,
    },
    /// The provider permanently moved the alias; the caller should re-observe
    /// at the target, which will record the rename with full evidence.
    MovedTo {
        /// The `owner/name` the provider points at.
        target: OwnerName,
    },
    /// The shared budget withheld this operation until `retry_at`.
    RateLimited {
        /// When the operation may proceed.
        retry_at: SystemTime,
    },
}

/// Failures of the observe flow beyond its outcomes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ObserveError {
    /// Alias or identity handling failed.
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    /// Metadata or lookup persistence failed.
    #[error(transparent)]
    Persistence(#[from] crate::database::PersistenceError),
    /// The provider exchange failed or was unclassifiable.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
    /// A not-modified answer arrived for a repository the catalog does not
    /// hold a validator for.
    #[error("a not-modified response arrived without catalog state")]
    MissingRepositoryState,
}

async fn stored_validator(
    database: &Database,
    repository_id: Uuid,
) -> Result<Option<String>, crate::database::PersistenceError> {
    sqlx::query_scalar(
        "select provider_etag from github_catalog.repository_metadata
         where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_optional(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)
}

/// Observes one repository by alias: enforces the token budget, fetches
/// conditionally, applies rename evidence when present, and records fresh
/// bodies into the projection and revision history.
///
/// # Errors
///
/// Returns [`ObserveError`] for identity, provider, or persistence failures.
pub async fn observe_repository<G>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    owner: &str,
    name: &str,
) -> Result<ObserveOutcome, ObserveError>
where
    G: crate::provider::GithubApi,
{
    // Budget first: a withheld operation touches nothing else.
    if let Err(AcquireError::RateLimited { retry_at }) = ledger.acquire(token) {
        return Ok(ObserveOutcome::RateLimited { retry_at });
    }

    let requested_value = format!("{owner}/{name}");
    let known_repository = resolve_alias(database, AliasKind::OwnerName, &requested_value).await?;
    let stored_etag = match known_repository {
        Some(repository_id) => stored_validator(database, repository_id).await?,
        None => None,
    };

    let reply = gateway
        .fetch_repository(None, owner, name, stored_etag.as_deref())
        .await?;
    ledger.observe(token, &reply.rate_limit);

    match reply.outcome {
        crate::provider::FetchOutcome::Fresh(fresh) => {
            let identity = upsert_repository(database, fresh.body.provider_repository_id).await?;
            // Rename evidence: the payload declares another owner/name than
            // the alias that was requested.
            let observed_value = fresh.body.owner_name().map(|o: OwnerName| o.to_string());
            let superseded_value = observed_value
                .as_deref()
                .filter(|observed| *observed != requested_value);
            apply_alias_observation(
                database,
                fresh.body.provider_repository_id,
                AliasKind::OwnerName,
                superseded_value,
                observed_value
                    .as_deref()
                    .unwrap_or(requested_value.as_str()),
            )
            .await?;
            apply_fresh_body(
                database,
                identity.repository_id,
                &fresh.body,
                fresh.etag.as_deref(),
            )
            .await?;
            Ok(ObserveOutcome::Observed {
                repository_id: identity.repository_id,
            })
        }
        crate::provider::FetchOutcome::NotModified => {
            let repository_id = known_repository.ok_or(ObserveError::MissingRepositoryState)?;
            apply_not_modified(database, repository_id).await?;
            Ok(ObserveOutcome::NotModified { repository_id })
        }
        crate::provider::FetchOutcome::MovedPermanently { target } => {
            Ok(ObserveOutcome::MovedTo { target })
        }
    }
}

// Re-exported for callers composing the flow from outside the crate.
const _: fn() = || {
    let _ = (
        std::any::type_name::<RateLimitHeaders>(),
        std::any::type_name::<Uuid>(),
    );
};
