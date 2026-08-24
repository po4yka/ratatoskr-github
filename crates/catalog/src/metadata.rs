//! Metadata projection persistence with bounded revision history.
//!
//! The projection carries the current observed values plus the conditional
//! request validator; every distinct observed body is appended as one raw
//! revision, and only the most recent bounded window of revisions is kept.

use serde_json::Value;
use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::provider::ProviderRepositoryBody;

/// How many most recent revisions stay retained per repository.
pub const REVISION_HISTORY_LIMIT: usize = 10;

/// The result of applying a fresh provider body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedOutcome {
    /// The first body for this repository: projection and revision created.
    Created,
    /// A body differing from the current revision: projection refreshed and
    /// a new revision appended.
    Updated,
    /// A body identical to the current revision: nothing but fetch
    /// bookkeeping moved.
    Unchanged,
}

/// Applies one fresh provider body to the repository's metadata projection.
///
/// A body identical to the current revision only moves fetch bookkeeping;
/// a differing or first body refreshes the projection and appends exactly
/// one raw revision, pruning history beyond [`REVISION_HISTORY_LIMIT`].
///
/// # Errors
///
/// Returns [`PersistenceError`] when the database refuses the operation.
pub async fn apply_fresh_body(
    database: &Database,
    repository_id: Uuid,
    body: &ProviderRepositoryBody,
    etag: Option<&str>,
) -> Result<AppliedOutcome, PersistenceError> {
    let payload = normalized_payload(body);
    let payload_text = payload.to_string();

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;

    let current_hash: Option<String> = sqlx::query_scalar(
        "select content_hash from github_catalog.repository_metadata
         where repository_id = $1 for update",
    )
    .bind(repository_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    let new_hash: String = sqlx::query_scalar("select md5($1)")
        .bind(&payload_text)
        .fetch_one(&mut *transaction)
        .await
        .map_err(PersistenceError::Query)?;

    if current_hash.as_deref() == Some(new_hash.as_str()) {
        sqlx::query("update github_catalog.repository_metadata set fetched_at = now() where repository_id = $1")
            .bind(repository_id)
            .execute(&mut *transaction)
            .await
            .map_err(PersistenceError::Query)?;
        transaction
            .commit()
            .await
            .map_err(PersistenceError::Query)?;
        return Ok(AppliedOutcome::Unchanged);
    }

    sqlx::query(
        "insert into github_catalog.repository_metadata
             (repository_id, description, language, stargazers_count, topics,
              default_branch, pushed_at, provider_etag, content_hash, fetched_at)
         values ($1, $2, $3, $4, $5, $6, $7::timestamptz, $8, $9, now())
         on conflict (repository_id) do update set
             description = excluded.description,
             language = excluded.language,
             stargazers_count = excluded.stargazers_count,
             topics = excluded.topics,
             default_branch = excluded.default_branch,
             pushed_at = excluded.pushed_at,
             provider_etag = excluded.provider_etag,
             content_hash = excluded.content_hash,
             fetched_at = now()",
    )
    .bind(repository_id)
    .bind(body.description.clone())
    .bind(body.language.clone())
    .bind(body.stargazers)
    .bind(serde_json::to_value(&body.topics).unwrap_or_default())
    .bind(body.default_branch.clone())
    .bind(body.pushed_at.clone())
    .bind(etag)
    .bind(&new_hash)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;

    sqlx::query(
        "insert into github_catalog.repository_metadata_revisions
             (revision_id, repository_id, payload, content_hash, observed_at)
         values ($1, $2, $3, $4, now())",
    )
    .bind(Uuid::now_v7())
    .bind(repository_id)
    .bind(payload)
    .bind(&new_hash)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;

    prune_history_in_tx(&mut transaction, repository_id).await?;

    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    match current_hash {
        None => Ok(AppliedOutcome::Created),
        Some(_) => Ok(AppliedOutcome::Updated),
    }
}

fn normalized_payload(body: &ProviderRepositoryBody) -> Value {
    serde_json::json!({
        "provider_repository_id": body.provider_repository_id,
        "full_name": body.full_name,
        "description": body.description,
        "language": body.language,
        "stargazers": body.stargazers,
        "topics": body.topics,
        "default_branch": body.default_branch,
        "pushed_at": body.pushed_at,
    })
}

async fn prune_history_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repository_id: Uuid,
) -> Result<(), PersistenceError> {
    sqlx::query(
        "delete from github_catalog.repository_metadata_revisions
         where repository_id = $1 and revision_id not in (
             select revision_id from github_catalog.repository_metadata_revisions
             where repository_id = $1
             order by observed_at desc, revision_id desc
             limit $2
         )",
    )
    .bind(repository_id)
    .bind(i64::try_from(REVISION_HISTORY_LIMIT).unwrap_or_default())
    .execute(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Records a not-modified refresh cheaply: no projection rewrite and no new
/// revision.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the database refuses the operation.
pub async fn apply_not_modified(
    database: &Database,
    repository_id: Uuid,
) -> Result<(), PersistenceError> {
    sqlx::query(
        "update github_catalog.repository_metadata set fetched_at = now() where repository_id = $1",
    )
    .bind(repository_id)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}
