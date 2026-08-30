//! Metadata projection persistence with bounded revision history.
//!
//! The projection carries the current observed values plus the conditional
//! request validator; every distinct observed body is appended as one raw
//! revision, and only the most recent bounded window of revisions is kept.

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use ratatoskr_github_contracts::{
    ReadmeRevision, RepositoryAnalysisAttributes, RepositoryAnalysisContract,
    RepositoryAnalysisRequested, RepositoryAnalysisRevision, RepositoryDescription,
    RepositoryFullName, RepositoryLanguage,
};
use ratatoskr_identifiers::{
    BlobOwner, BlobRef, ContentDigest, DigestAlgorithm, DigestHex, MediaType,
    RepositoryAnalysisRequestId, RepositoryId, TenantRef,
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

/// Immutable source input admitted to repository analysis.
#[derive(Debug, Clone)]
pub struct RepositoryAnalysisSource {
    /// Tenant authorized to analyse this repository revision.
    pub owner: TenantRef,
    /// Immutable README state, with bytes represented only by a stored `BlobRef`.
    pub readme: ReadmeRevision,
    /// README conditional validator retained with this source revision.
    pub readme_etag: Option<String>,
}

/// Failure while constructing a published repository-analysis command.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepositoryAnalysisPublicationError {
    /// Catalog-owned persistence failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// Provider metadata cannot be represented by the bounded shared contract.
    #[error("repository metadata violates the published analysis contract")]
    Contract,
    /// A JSON contract value could not be encoded for durable storage.
    #[error("the repository-analysis command could not be encoded")]
    Encode(#[source] serde_json::Error),
    /// The numeric GitHub repository identity cannot be represented by the contract.
    #[error("the GitHub repository identity is outside the published contract range")]
    NumericIdentity,
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

/// Failure while resolving immutable README evidence for its authorized analysis owner.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveReadmeError {
    /// No exact owner, repository, and content-reference publication exists.
    #[error("the README evidence is unavailable")]
    NotFound,
    /// The stored README exceeds the fixed analysis-input limit.
    #[error("the README evidence exceeds the response limit")]
    TooLarge,
    /// Stored bytes no longer agree with their immutable reference.
    #[error("the README evidence failed integrity verification")]
    Integrity,
    /// A retained publication no longer decodes as its published contract.
    #[error("the repository-analysis publication is invalid")]
    Contract(#[source] serde_json::Error),
    /// Catalog-owned durable storage was unavailable.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
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

/// Resolves one exact Catalog-owned README reference only when a retained analysis publication
/// binds it to the supplied tenant and repository.
///
/// The returned bytes are independently checked against the reference even though database
/// constraints also preserve length and media type.
///
/// # Errors
///
/// Returns [`ResolveReadmeError`] when authorization evidence is absent, the body is oversized,
/// stored state conflicts with the immutable reference, or storage is unavailable.
pub async fn resolve_authorized_readme(
    database: &Database,
    owner: &TenantRef,
    repository_id: RepositoryId,
    content_ref: &BlobRef,
    max_bytes: usize,
) -> Result<Vec<u8>, ResolveReadmeError> {
    if content_ref.owner_service.as_str() != "ratatoskr-github"
        || content_ref.digest.algorithm != DigestAlgorithm::Sha256
        || content_ref.media_type.as_str() != "text/markdown"
    {
        return Err(ResolveReadmeError::NotFound);
    }

    let publication: Option<Value> = sqlx::query_scalar(
        "select payload
         from github_catalog.repository_analysis_publications
         where repository_id = $1
           and payload ->> 'owner' = $2
           and payload ->> 'repository_id' = $3
           and payload #>> '{source_revision,readme,content_ref,digest,hex}' = $4
         limit 1",
    )
    .bind(repository_id.0)
    .bind(owner.to_string())
    .bind(repository_id.to_string())
    .bind(content_ref.digest.hex.as_str())
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    let publication = publication.ok_or(ResolveReadmeError::NotFound)?;
    let request: RepositoryAnalysisRequested =
        serde_json::from_value(publication).map_err(ResolveReadmeError::Contract)?;
    let ReadmeRevision::Present {
        content_ref: published_ref,
    } = request.source_revision.readme
    else {
        return Err(ResolveReadmeError::NotFound);
    };
    if request.owner != *owner
        || request.repository_id != repository_id
        || published_ref != *content_ref
    {
        return Err(ResolveReadmeError::NotFound);
    }

    let max_length = i64::try_from(max_bytes).map_err(|_| ResolveReadmeError::TooLarge)?;
    let stored: Option<(Vec<u8>, String, i64)> = sqlx::query_as(
        "select bytes, media_type, length_bytes
         from github_catalog.repository_readme_blobs
         where content_digest = $1 and length_bytes <= $2",
    )
    .bind(content_ref.digest.hex.as_str())
    .bind(max_length)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    let Some((bytes, media_type, length_bytes)) = stored else {
        let stored_length: Option<i64> = sqlx::query_scalar(
            "select length_bytes from github_catalog.repository_readme_blobs
             where content_digest = $1",
        )
        .bind(content_ref.digest.hex.as_str())
        .fetch_optional(database.pool())
        .await
        .map_err(PersistenceError::Query)?;
        return match stored_length {
            Some(length) if length > max_length => Err(ResolveReadmeError::TooLarge),
            Some(_) | None => Err(ResolveReadmeError::NotFound),
        };
    };

    let actual_digest = sha256_hex(&Sha256::digest(&bytes));
    let expected_length = u64::try_from(length_bytes).map_err(|_| ResolveReadmeError::Integrity)?;
    if media_type != content_ref.media_type.as_str()
        || expected_length != content_ref.length_bytes
        || bytes.len() > max_bytes
        || actual_digest != content_ref.digest.hex.as_str()
    {
        return Err(ResolveReadmeError::Integrity);
    }
    Ok(bytes)
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
    let new_hash = sha256_hex(&Sha256::digest(payload_text.as_bytes()));

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

/// Applies one complete immutable source revision and atomically appends its analysis command.
///
/// Unlike [`apply_fresh_body`], this path includes README evidence in the SHA-256 source identity.
/// Replays of an equal metadata/README pair converge without a second revision or outbox row.
///
/// # Errors
///
/// Returns [`RepositoryAnalysisPublicationError`] when the source cannot be represented or
/// durable state cannot be committed.
#[allow(
    clippy::too_many_lines,
    reason = "one transaction keeps repository metadata, immutable source revision, and outbox command atomic"
)]
pub async fn apply_fresh_source(
    database: &Database,
    repository_id: Uuid,
    body: &ProviderRepositoryBody,
    etag: Option<&str>,
    source: &RepositoryAnalysisSource,
) -> Result<AppliedOutcome, RepositoryAnalysisPublicationError> {
    let attributes = repository_attributes(body)?;
    let attributes_value =
        serde_json::to_value(&attributes).map_err(RepositoryAnalysisPublicationError::Encode)?;
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
        &serde_json::to_value(&source.readme)
            .map_err(RepositoryAnalysisPublicationError::Encode)?,
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

    let request = RepositoryAnalysisRequested {
        owner: source.owner,
        repository_id: RepositoryId::parse(&repository_id.to_string())
            .map_err(|_| RepositoryAnalysisPublicationError::Contract)?,
        github_repository_numeric_id: u64::try_from(body.provider_repository_id)
            .map_err(|_| RepositoryAnalysisPublicationError::NumericIdentity)?,
        request_id: RepositoryAnalysisRequestId::new_v7(),
        source_revision: RepositoryAnalysisRevision {
            attributes_digest,
            readme: source.readme.clone(),
        },
        repository_attributes: attributes,
        requested_contract: RepositoryAnalysisContract::RepositoryAnalysis,
        idempotency_key: source_digest.clone(),
        extensions: ratatoskr_identifiers::Extensions::new(),
    };
    let payload =
        serde_json::to_value(&request).map_err(RepositoryAnalysisPublicationError::Encode)?;
    let message_id = Uuid::now_v7();
    let inserted = sqlx::query(
        "insert into github_catalog.repository_analysis_publications
             (repository_id, source_digest, message_id, payload)
         values ($1, $2, $3, $4)
         on conflict (repository_id, source_digest) do nothing",
    )
    .bind(repository_id)
    .bind(source_digest.hex.as_str())
    .bind(message_id)
    .bind(&payload)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    if inserted.rows_affected() != 1 {
        return Err(RepositoryAnalysisPublicationError::Persistence(
            PersistenceError::Query(sqlx::Error::Protocol(
                "repository source publication identity diverged".to_owned(),
            )),
        ));
    }
    sqlx::query(
        "insert into github_catalog.outbox_events (message_id, subject, payload)
         values ($1, 'knowledge.repository_analysis.requested.v1', $2)",
    )
    .bind(message_id)
    .bind(payload)
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
) -> Result<RepositoryAnalysisAttributes, RepositoryAnalysisPublicationError> {
    let description = body
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(RepositoryDescription::parse)
        .transpose()
        .map_err(|_| RepositoryAnalysisPublicationError::Contract)?;
    let primary_language = body
        .language
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(RepositoryLanguage::parse)
        .transpose()
        .map_err(|_| RepositoryAnalysisPublicationError::Contract)?;
    Ok(RepositoryAnalysisAttributes {
        repository_full_name: RepositoryFullName::parse(&body.full_name)
            .map_err(|_| RepositoryAnalysisPublicationError::Contract)?,
        description,
        primary_language,
    })
}

fn content_digest(value: &Value) -> Result<ContentDigest, RepositoryAnalysisPublicationError> {
    let serialized =
        serde_json::to_vec(value).map_err(RepositoryAnalysisPublicationError::Encode)?;
    let hex = sha256_hex(&Sha256::digest(serialized));
    Ok(ContentDigest {
        algorithm: DigestAlgorithm::Sha256,
        hex: DigestHex::parse(&hex).map_err(|_| RepositoryAnalysisPublicationError::Contract)?,
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
