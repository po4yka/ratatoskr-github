//! The metadata observe flow: identity, aliases, conditional fetch, rate
//! budget, and metadata persistence composed into one operation.

use std::time::SystemTime;

use uuid::Uuid;

use crate::database::Database;
use crate::identity::{AliasKind, apply_alias_observation, resolve_alias, upsert_repository};
use ratatoskr_github_contracts::{ReadmeAbsenceReason, ReadmeRevision};
use ratatoskr_identifiers::TenantRef;

use crate::metadata::{
    AppliedOutcome, ReadmeBlobError, RepositoryAnalysisSource, RepositorySourceError,
    apply_fresh_source, apply_not_modified, store_readme,
};
use crate::provider::{FreshRepository, GithubApi, OwnerName, ReadmeFetchOutcome};
use crate::rate_limit::{AcquireError, RateLimitHeaders, RateLimitLedger, TokenRef};
use crate::watches::{
    WatchBaseline, WatchError, evaluate_metadata_watches, register_repository_analysis_watch,
};

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
    /// The source revision could not be represented or committed.
    #[error(transparent)]
    Source(#[from] RepositorySourceError),
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

/// A repository that has an enabled watch, with the `owner/name` alias to observe it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedRepository {
    /// The internal repository identity.
    pub repository_id: Uuid,
    /// The current active `owner/name` alias.
    pub alias: OwnerName,
}

/// Selects at most `batch` repositories that have an enabled watch and an active `owner/name`
/// alias, the least recently attempted first (XR-021 CONTRACTS.md R2-08 c).
///
/// Never-attempted repositories come first, then those attempted longest ago; the order rotates on
/// the attempt recorded by [`record_observation_attempt`], not on success, so repositories the
/// observer cannot read never hold the front of the queue.
///
/// # Errors
///
/// Returns [`crate::database::PersistenceError`] when the database refuses the query.
pub async fn select_watched_repositories(
    database: &Database,
    batch: u32,
) -> Result<Vec<WatchedRepository>, crate::database::PersistenceError> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select metadata.repository_id, alias.alias_value
         from github_catalog.repository_metadata metadata
         join lateral (
             select alias_value from github_catalog.repository_aliases
             where repository_id = metadata.repository_id
               and alias_kind = 'owner_name' and status = 'active'
             order by created_at desc limit 1
         ) alias on true
         where exists (
             select 1 from github_catalog.repository_watches watch
             where watch.repository_id = metadata.repository_id and watch.enabled)
         order by metadata.observe_attempted_at nulls first, metadata.fetched_at,
                  metadata.repository_id
         limit $1",
    )
    .bind(i64::from(batch))
    .fetch_all(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(rows
        .into_iter()
        .filter_map(|(repository_id, alias)| {
            let (owner, name) = alias.split_once('/')?;
            Some(WatchedRepository {
                repository_id,
                alias: OwnerName {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                },
            })
        })
        .collect())
}

/// Records that the periodic observer attempted `repository_id`, whatever the outcome, so the
/// next selection rotates to the repositories attempted longer ago.
///
/// # Errors
///
/// Returns [`crate::database::PersistenceError`] when the database refuses the update.
pub async fn record_observation_attempt(
    database: &Database,
    repository_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "update github_catalog.repository_metadata set observe_attempted_at = now()
         where repository_id = $1",
    )
    .bind(repository_id)
    .execute(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// The provider side of one observation: the gateway, the shared budget, and the credential the
/// requests carry (none for the unauthenticated observer).
#[derive(Debug)]
pub struct ProviderAccess<'a, G> {
    /// The provider gateway every request goes through.
    pub gateway: &'a G,
    /// The shared rate-limit ledger that gates and learns from each response.
    pub ledger: &'a RateLimitLedger,
    /// The non-secret budget label of the credential.
    pub token: &'a TokenRef,
    /// The bearer credential, or `None` for unauthenticated access.
    pub credential: Option<&'a str>,
}

/// Records one fresh repository body as the immutable source revision it is: fetches the README
/// through the same gateway and ledger, preserves its bytes, applies the metadata and README
/// evidence, optionally registers an unevaluated analysis watch for the tracking user, and
/// evaluates every enabled watch so a changed repository queues a request for each owner.
///
/// This is the one function the action route and the periodic observer share (XR-021
/// CONTRACTS.md R2-08 b). `now` is the instant the evaluation is made at, so a request it queues
/// is due from `now`.
///
/// # Errors
///
/// Returns [`ObserveError`] for provider, persistence, or watch failures.
pub async fn record_fresh_repository<G>(
    database: &Database,
    access: &ProviderAccess<'_, G>,
    repository_id: Uuid,
    alias: &OwnerName,
    fresh: &FreshRepository,
    track_for: Option<TenantRef>,
    now: time::OffsetDateTime,
) -> Result<AppliedOutcome, ObserveError>
where
    G: GithubApi,
{
    let (_, stored_readme_etag, stored_readme_revision) =
        stored_validators(database, repository_id).await?;
    let readme_reply = access
        .gateway
        .fetch_readme(
            access.credential,
            &alias.owner,
            &alias.name,
            stored_readme_etag.as_deref(),
        )
        .await?;
    access
        .ledger
        .observe(access.token, &readme_reply.rate_limit);
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
    let applied = apply_fresh_source(
        database,
        repository_id,
        &fresh.body,
        fresh.etag.as_deref(),
        &RepositoryAnalysisSource {
            readme,
            readme_etag,
        },
    )
    .await?;
    if let Some(owner) = track_for {
        register_repository_analysis_watch(
            database,
            owner,
            repository_id,
            WatchBaseline::Unevaluated,
        )
        .await?;
    }
    evaluate_metadata_watches(database, repository_id, &fresh.body, now).await?;
    Ok(applied)
}

/// Observes the repository `alias` names with unauthenticated access: enforces the token budget,
/// fetches conditionally, applies rename evidence when present, and records fresh bodies through
/// [`record_fresh_repository`]. `now` is the instant metadata watches are evaluated at.
///
/// # Errors
///
/// Returns [`ObserveError`] for identity, provider, or persistence failures.
pub async fn observe_repository<G>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    alias: &OwnerName,
    now: time::OffsetDateTime,
) -> Result<ObserveOutcome, ObserveError>
where
    G: GithubApi,
{
    // Budget first: a withheld operation touches nothing else.
    if let Err(AcquireError::RateLimited { retry_at }) = ledger.acquire(token) {
        return Ok(ObserveOutcome::RateLimited { retry_at });
    }

    let requested_value = alias.to_string();
    let known_repository = resolve_alias(database, AliasKind::OwnerName, &requested_value).await?;
    let stored_etag = match known_repository {
        Some(repository_id) => stored_validators(database, repository_id).await?.0,
        None => None,
    };

    let reply = gateway
        .fetch_repository(None, &alias.owner, &alias.name, stored_etag.as_deref())
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
            let access = ProviderAccess {
                gateway,
                ledger,
                token,
                credential: None,
            };
            record_fresh_repository(
                database,
                &access,
                identity.repository_id,
                alias,
                &fresh,
                None,
                now,
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
