//! Metadata projection persistence with bounded revision history.
//!
//! The projection carries the current observed values plus the conditional
//! request validator; every distinct observed body is appended as one raw
//! revision, and only the most recent bounded window of revisions is kept.

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use ratatoskr_github_contracts::{
    ReadmeRevision, RepositoryAnalysisAttributes, RepositoryDescription, RepositoryFullName,
    RepositoryLanguage,
};
use ratatoskr_identifiers::{
    BlobOwner, BlobRef, ContentDigest, DigestAlgorithm, DigestHex, MediaType,
};

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

/// Immutable source input a repository revision is recorded with.
#[derive(Debug, Clone)]
pub struct RepositoryAnalysisSource {
    /// Immutable README state, with bytes represented only by a stored `BlobRef`.
    pub readme: ReadmeRevision,
    /// README conditional validator retained with this source revision.
    pub readme_etag: Option<String>,
}

/// Failure while recording an immutable repository source revision.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepositorySourceError {
    /// Catalog-owned persistence failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// Provider metadata cannot be represented by the bounded shared contract.
    #[error("repository metadata violates the published analysis contract")]
    Contract,
    /// A JSON contract value could not be encoded for durable storage.
    #[error("the repository source revision could not be encoded")]
    Encode(#[source] serde_json::Error),
}

/// Failure while preserving bounded README bytes into the Catalog-owned blob boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadmeBlobError {
    /// A Catalog-owned durable operation failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// A generated reference unexpectedly violates the published identifiers contract.
    #[error("the generated README blob reference is invalid")]
    Contract,
    /// The byte length does not fit the database representation.
    #[error("the README byte length exceeds the supported storage range")]
    LengthOverflow,
}

/// Preserves one bounded README body under its SHA-256 identity and returns only its immutable
/// cross-service reference. Repeated preservation of equal bytes converges on one row.
///
/// # Errors
///
/// Returns [`ReadmeBlobError`] when durable storage or the published reference construction fails.
pub async fn store_readme(database: &Database, bytes: &[u8]) -> Result<BlobRef, ReadmeBlobError> {
    let digest_hex = sha256_hex(&Sha256::digest(bytes));
    let length_bytes = i64::try_from(bytes.len()).map_err(|_| ReadmeBlobError::LengthOverflow)?;
    sqlx::query(
        "insert into github_catalog.repository_readme_blobs
             (content_digest, bytes, media_type, length_bytes)
         values ($1, $2, 'text/markdown', $3)
         on conflict (content_digest) do nothing",
    )
    .bind(&digest_hex)
    .bind(bytes)
    .bind(length_bytes)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?;

    Ok(BlobRef {
        owner_service: BlobOwner::parse("ratatoskr-github")
            .map_err(|_| ReadmeBlobError::Contract)?,
        digest: ContentDigest {
            algorithm: DigestAlgorithm::Sha256,
            hex: DigestHex::parse(&digest_hex).map_err(|_| ReadmeBlobError::Contract)?,
        },
        media_type: MediaType::parse("text/markdown").map_err(|_| ReadmeBlobError::Contract)?,
        length_bytes: u64::try_from(length_bytes).map_err(|_| ReadmeBlobError::LengthOverflow)?,
    })
}

/// Applies one complete immutable source revision: the metadata projection, its README evidence
/// and one raw history revision.
///
/// The SHA-256 source identity covers the analysis attributes and the README evidence, so replays
/// of an equal pair converge without a second revision, and a differing or first source refreshes
/// the projection and appends exactly one raw revision, pruning history beyond
/// [`REVISION_HISTORY_LIMIT`]. It queues no analysis request: requests originate only from
/// watches, which the caller evaluates afterwards with `evaluate_metadata_watches`.
///
/// # Errors
///
/// Returns [`RepositorySourceError`] when the source cannot be represented or durable state cannot
/// be committed.
pub async fn apply_fresh_source(
    database: &Database,
    repository_id: Uuid,
    body: &ProviderRepositoryBody,
    etag: Option<&str>,
    source: &RepositoryAnalysisSource,
) -> Result<AppliedOutcome, RepositorySourceError> {
    let attributes = repository_attributes(body)?;
    let attributes_value =
        serde_json::to_value(&attributes).map_err(RepositorySourceError::Encode)?;
    let attributes_digest = content_digest(&attributes_value)?;
    let source_value = serde_json::json!({
        "attributes_digest": attributes_digest,
        "readme": source.readme,
    });
    let source_digest = content_digest(&source_value)?;
    let raw_payload = normalized_payload(body);
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
    if current_hash.as_deref() == Some(source_digest.hex.as_str()) {
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

    write_metadata_projection(
        &mut transaction,
        repository_id,
        body,
        etag,
        source.readme_etag.as_deref(),
        &serde_json::to_value(&source.readme).map_err(RepositorySourceError::Encode)?,
        source_digest.hex.as_str(),
    )
    .await?;
    sqlx::query(
        "insert into github_catalog.repository_metadata_revisions
             (revision_id, repository_id, payload, content_hash, observed_at)
         values ($1, $2, $3, $4, now())",
    )
    .bind(Uuid::now_v7())
    .bind(repository_id)
    .bind(raw_payload)
    .bind(source_digest.hex.as_str())
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    prune_history_in_tx(&mut transaction, repository_id).await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(if current_hash.is_some() {
        AppliedOutcome::Updated
    } else {
        AppliedOutcome::Created
    })
}

fn repository_attributes(
    body: &ProviderRepositoryBody,
) -> Result<RepositoryAnalysisAttributes, RepositorySourceError> {
    let description = body
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(RepositoryDescription::parse)
        .transpose()
        .map_err(|_| RepositorySourceError::Contract)?;
    let primary_language = body
        .language
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(RepositoryLanguage::parse)
        .transpose()
        .map_err(|_| RepositorySourceError::Contract)?;
    Ok(RepositoryAnalysisAttributes {
        repository_full_name: RepositoryFullName::parse(&body.full_name)
            .map_err(|_| RepositorySourceError::Contract)?,
        description,
        primary_language,
    })
}

fn content_digest(value: &Value) -> Result<ContentDigest, RepositorySourceError> {
    let serialized = serde_json::to_vec(value).map_err(RepositorySourceError::Encode)?;
    let hex = sha256_hex(&Sha256::digest(serialized));
    Ok(ContentDigest {
        algorithm: DigestAlgorithm::Sha256,
        hex: DigestHex::parse(&hex).map_err(|_| RepositorySourceError::Contract)?,
    })
}

async fn write_metadata_projection(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repository_id: Uuid,
    body: &ProviderRepositoryBody,
    etag: Option<&str>,
    readme_etag: Option<&str>,
    readme_revision: &Value,
    content_hash: &str,
) -> Result<(), PersistenceError> {
    sqlx::query(
        "insert into github_catalog.repository_metadata
             (repository_id, description, language, stargazers_count, topics,
              default_branch, pushed_at, provider_etag, readme_etag, readme_revision, content_hash, fetched_at)
         values ($1, $2, $3, $4, $5, $6, $7::timestamptz, $8, $9, $10, $11, now())
         on conflict (repository_id) do update set
             description = excluded.description, language = excluded.language,
             stargazers_count = excluded.stargazers_count, topics = excluded.topics,
             default_branch = excluded.default_branch, pushed_at = excluded.pushed_at,
             provider_etag = excluded.provider_etag, readme_etag = excluded.readme_etag,
             readme_revision = excluded.readme_revision,
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
    .bind(readme_etag)
    .bind(readme_revision)
    .bind(content_hash)
    .execute(&mut **transaction)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
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

fn sha256_hex(digest: &[u8]) -> String {
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        for nibble in [byte >> 4, byte & 0x0f] {
            encoded.push(match nibble {
                0 => '0',
                1 => '1',
                2 => '2',
                3 => '3',
                4 => '4',
                5 => '5',
                6 => '6',
                7 => '7',
                8 => '8',
                9 => '9',
                10 => 'a',
                11 => 'b',
                12 => 'c',
                13 => 'd',
                14 => 'e',
                _ => 'f',
            });
        }
    }
    encoded
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
