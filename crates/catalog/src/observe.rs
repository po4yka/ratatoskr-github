//! The metadata observe flow: identity, aliases, conditional fetch, rate
//! budget, and metadata persistence composed into one operation.

use std::time::SystemTime;

use uuid::Uuid;

use crate::database::Database;
use crate::identity::{AliasKind, apply_alias_observation, resolve_alias, upsert_repository};
use ratatoskr_github_contracts::{ReadmeAbsenceReason, ReadmeRevision};
use ratatoskr_identifiers::TenantRef;

use crate::metadata::{
    ReadmeBlobError, RepositoryAnalysisSource, apply_fresh_source, apply_not_modified, store_readme,
};
use crate::provider::{OwnerName, ReadmeFetchOutcome};
use crate::rate_limit::{AcquireError, RateLimitHeaders, RateLimitLedger, TokenRef};
use crate::watches::{WatchError, evaluate_metadata_watches};

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
    /// A metadata delta could not be represented or queued for an enabled watch.
    #[error(transparent)]
    Watch(#[from] WatchError),
    /// The provider exchange failed or was unclassifiable.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
    /// README evidence could not be preserved before the source revision was committed.
    #[error(transparent)]
    ReadmeBlob(#[from] ReadmeBlobError),
    /// The source could not be represented by the published analysis request contract.
    #[error(transparent)]
    AnalysisPublication(#[from] crate::metadata::RepositoryAnalysisPublicationError),
    /// A not-modified answer arrived for a repository the catalog does not
    /// hold a validator for.
    #[error("a not-modified response arrived without catalog state")]
    MissingRepositoryState,
}

async fn stored_validators(
    database: &Database,
    repository_id: Uuid,
) -> Result<
    (Option<String>, Option<String>, Option<serde_json::Value>),
    crate::database::PersistenceError,
> {
    let stored = sqlx::query_as(
        "select provider_etag, readme_etag, readme_revision from github_catalog.repository_metadata
         where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_optional(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(stored.unwrap_or((None, None, None)))
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
    analysis_owner: TenantRef,
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
    let (stored_etag, stored_readme_etag, stored_readme_revision) = match known_repository {
        Some(repository_id) => stored_validators(database, repository_id).await?,
        None => (None, None, None),
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
            let readme_reply = gateway
                .fetch_readme(None, owner, name, stored_readme_etag.as_deref())
                .await?;
            ledger.observe(token, &readme_reply.rate_limit);
            let (readme, readme_etag) = match readme_reply.outcome {
                ReadmeFetchOutcome::Fresh(fresh_readme) => (
                    ReadmeRevision::Present {
                        content_ref: store_readme(database, &fresh_readme.bytes).await?,
                    },
                    fresh_readme.etag,
                ),
                ReadmeFetchOutcome::NotModified => (
                    serde_json::from_value(
                        stored_readme_revision.ok_or(ObserveError::MissingRepositoryState)?,
                    )
                    .map_err(|_| ObserveError::MissingRepositoryState)?,
                    stored_readme_etag,
                ),
                ReadmeFetchOutcome::NotFound => (
                    ReadmeRevision::Absent {
                        reason: ReadmeAbsenceReason::NotFound,
                    },
                    None,
                ),
                ReadmeFetchOutcome::NotAuthorized => (
                    ReadmeRevision::Absent {
                        reason: ReadmeAbsenceReason::NotAuthorized,
                    },
                    None,
                ),
            };
            apply_fresh_source(
                database,
                identity.repository_id,
                &fresh.body,
                fresh.etag.as_deref(),
                &RepositoryAnalysisSource {
                    owner: analysis_owner,
                    readme,
                    readme_etag,
                },
            )
            .await?;
            evaluate_metadata_watches(
                database,
                identity.repository_id,
                &fresh.body,
                time::OffsetDateTime::now_utc(),
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
