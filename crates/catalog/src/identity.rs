//! Stable repository identity and mutable alias handling.
//!
//! GitHub's numeric repository ID is the stable upstream identity; owner/name
//! and URLs are mutable aliases that must never serve as the primary key.

use uuid::Uuid;

use crate::database::{Database, PersistenceError};

/// Failure modes of identity and alias operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityError {
    /// The requested alias is already the live alias of another repository.
    #[error("the requested alias is already the live alias of another repository")]
    LiveAliasConflict,
    /// The underlying database operation failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// The internal, provider-independent identity of one known repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepositoryIdentity {
    /// The catalog-owned stable identifier of the repository.
    pub repository_id: Uuid,
}

/// The kinds of mutable aliases a repository carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasKind {
    /// The mutable `owner/name` path of the repository.
    OwnerName,
    /// The derived canonical web URL.
    HtmlUrl,
    /// The derived Git clone URL.
    CloneUrl,
}

impl AliasKind {
    /// The database representation of the alias kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OwnerName => "owner_name",
            Self::HtmlUrl => "html_url",
            Self::CloneUrl => "clone_url",
        }
    }
}

/// Records an alias as the live alias of a repository.
/// Records an alias as the live alias of a repository.
///
/// # Errors
///
/// Returns [`IdentityError::LiveAliasConflict`] when another repository holds
/// the alias live, and [`IdentityError::Persistence`] when the database
/// refuses the operation.
pub async fn record_alias(
    database: &Database,
    repository_id: Uuid,
    kind: AliasKind,
    value: &str,
) -> Result<(), IdentityError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    if let Some((_, holder)) = active_alias_holder_in_tx(&mut transaction, kind, value).await? {
        if holder != repository_id {
            return Err(IdentityError::LiveAliasConflict);
        }
        // Already live and ours: recording again changes nothing.
        transaction
            .commit()
            .await
            .map_err(PersistenceError::Query)?;
        return Ok(());
    }
    reactivate_alias_in_tx(&mut transaction, repository_id, kind, value).await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Resolves an alias value to its holding repository's internal identity.
/// Superseded history still resolves, so a rename keeps the old name
/// redirecting to the same repository.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the database refuses the operation.
pub async fn resolve_alias(
    database: &Database,
    kind: AliasKind,
    value: &str,
) -> Result<Option<Uuid>, PersistenceError> {
    let repository_id: Option<Uuid> = sqlx::query_scalar(
        "select repository_id from github_catalog.repository_aliases
         where alias_kind = $1 and alias_value = $2
         order by (status = 'active') desc, created_at desc
         limit 1",
    )
    .bind(kind.as_str())
    .bind(value)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(repository_id)
}

/// Creates or finds the logical repository for a GitHub numeric repository ID.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the database refuses the operation.
pub async fn upsert_repository(
    database: &Database,
    provider_repository_id: i64,
) -> Result<RepositoryIdentity, PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let repository_id = upsert_repository_in_tx(&mut transaction, provider_repository_id).await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(RepositoryIdentity { repository_id })
}

async fn upsert_repository_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    provider_repository_id: i64,
) -> Result<Uuid, PersistenceError> {
    let repository_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.repositories
             (repository_id, provider_repository_id)
         values ($1, $2)
         on conflict (provider_repository_id) do nothing",
    )
    .bind(repository_id)
    .bind(provider_repository_id)
    .execute(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    let stored_id: Uuid = sqlx::query_scalar(
        "select repository_id from github_catalog.repositories
         where provider_repository_id = $1",
    )
    .bind(provider_repository_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(stored_id)
}

async fn active_alias_holder_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: AliasKind,
    value: &str,
) -> Result<Option<(Uuid, Uuid)>, PersistenceError> {
    let holder: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select alias_id, repository_id from github_catalog.repository_aliases
         where alias_kind = $1 and alias_value = $2 and status = 'active'",
    )
    .bind(kind.as_str())
    .bind(value)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(holder)
}

async fn reactivate_alias_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repository_id: Uuid,
    kind: AliasKind,
    value: &str,
) -> Result<Uuid, PersistenceError> {
    let reactivated: Option<Uuid> = sqlx::query_scalar(
        "update github_catalog.repository_aliases
         set status = 'active', redirect_to = null
         where repository_id = $1 and alias_kind = $2 and alias_value = $3
           and status <> 'active'
         returning alias_id",
    )
    .bind(repository_id)
    .bind(kind.as_str())
    .bind(value)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    if let Some(alias_id) = reactivated {
        return Ok(alias_id);
    }
    let inserted: Uuid = sqlx::query_scalar(
        "insert into github_catalog.repository_aliases
             (alias_id, repository_id, alias_kind, alias_value)
         values ($1, $2, $3, $4)
         returning alias_id",
    )
    .bind(Uuid::now_v7())
    .bind(repository_id)
    .bind(kind.as_str())
    .bind(value)
    .fetch_one(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(inserted)
}

async fn supersede_alias_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: AliasKind,
    superseded_value: &str,
    redirect_to: Uuid,
) -> Result<(), PersistenceError> {
    sqlx::query(
        "update github_catalog.repository_aliases
         set status = 'superseded', redirect_to = $1
         where alias_kind = $2 and alias_value = $3 and status = 'active'",
    )
    .bind(redirect_to)
    .bind(kind.as_str())
    .bind(superseded_value)
    .execute(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Applies rename or transfer evidence: the observed value becomes the live
/// alias of the repository behind `provider_repository_id`, and the superseded
/// alias stays resolvable as redirect history.
///
/// # Errors
///
/// Returns [`IdentityError::LiveAliasConflict`] when another repository holds
/// the observed alias live, and [`IdentityError::Persistence`] when the
/// database refuses the operation.
pub async fn apply_alias_observation(
    database: &Database,
    provider_repository_id: i64,
    kind: AliasKind,
    superseded_value: Option<&str>,
    observed_value: &str,
) -> Result<RepositoryIdentity, IdentityError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let repository_id = upsert_repository_in_tx(&mut transaction, provider_repository_id).await?;

    // The observed value must end up as this repository's live alias. When it
    // is already live but held by another repository the claim is rejected;
    // when it is already ours the existing row is reused.
    let alias_id = match active_alias_holder_in_tx(&mut transaction, kind, observed_value).await? {
        Some((alias_id, holder)) => {
            if holder != repository_id {
                return Err(IdentityError::LiveAliasConflict);
            }
            alias_id
        }
        None => {
            reactivate_alias_in_tx(&mut transaction, repository_id, kind, observed_value).await?
        }
    };

    // Superseding is idempotent: re-delivered rename evidence finds no live
    // row for the old value and updates nothing.
    if let Some(old_value) = superseded_value.filter(|old| *old != observed_value) {
        supersede_alias_in_tx(&mut transaction, kind, old_value, alias_id).await?;
    }

    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(RepositoryIdentity { repository_id })
}
