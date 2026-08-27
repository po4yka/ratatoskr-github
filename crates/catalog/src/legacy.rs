//! Bounded, temporary reads from the retired `PostgreSQL` schema.

use std::collections::BTreeMap;
use std::fmt;

use ratatoskr_identifiers::TenantRef;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Database;

/// A temporary legacy `PostgreSQL` source with no credential access API.
#[derive(Clone)]
pub struct LegacySource {
    pool: PgPool,
}

impl fmt::Debug for LegacySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacySource([REDACTED])")
    }
}

/// Non-secret records selected from one inspected legacy source.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacySnapshot {
    /// Repositories owned by the retired schema.
    pub repositories: Vec<LegacyRepository>,
    /// Connection metadata with no credential material.
    pub integrations: Vec<LegacyIntegration>,
}

/// One non-secret legacy repository observation.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyRepository {
    /// Source-local primary key used only for idempotent import evidence.
    pub legacy_repository_id: i64,
    /// Stable numeric GitHub repository identity.
    pub provider_repository_id: i64,
    /// Observed GitHub owner name.
    pub owner: String,
    /// Observed GitHub repository name.
    pub name: String,
    /// Retired user identifier requiring an explicit current-owner mapping.
    pub legacy_user_id: i64,
    /// Retired current-star claim.
    pub starred: bool,
    /// Retired source observation time, never a provider star timestamp.
    pub observed_at: Option<OffsetDateTime>,
    /// Retired list names, which have no native provider list identity.
    pub list_names: Vec<String>,
}

/// Non-secret legacy GitHub integration metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyIntegration {
    /// Retired user identifier requiring an explicit current-owner mapping.
    pub legacy_user_id: i64,
    /// Retired granted-scope representation.
    pub granted_scopes: Option<String>,
    /// Retired observed GitHub login, never an owner identity.
    pub login: Option<String>,
    /// Retired observed numeric GitHub identity.
    pub provider_user_id: Option<i64>,
    /// Retired non-secret connection status.
    pub status: String,
}

/// A refused legacy source operation without source values or credentials.
#[derive(Debug, thiserror::Error)]
pub enum LegacySourceError {
    /// The isolated source could not be connected.
    #[error("legacy source could not be connected")]
    Connect(#[source] sqlx::Error),
    /// The source did not expose exactly the columns required by the importer.
    #[error("legacy source does not match the approved catalog archive schema")]
    Schema,
    /// A fixed, allow-listed query failed.
    #[error("legacy source query failed")]
    Query(#[source] sqlx::Error),
    /// A selected non-secret value could not be represented safely.
    #[error("legacy source contains invalid catalog data")]
    InvalidData,
}

/// One validated mapping from a retired numeric user ID to a current tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyOwnerMap {
    owners: BTreeMap<i64, String>,
}

/// A rejected owner map with no mapping values in the diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum LegacyOwnerMapError {
    /// The JSON document did not have the accepted array shape.
    #[error("legacy owner map is invalid")]
    InvalidDocument,
    /// A retired user was repeated in one map.
    #[error("legacy owner map repeats a retired user")]
    DuplicateLegacyUser,
    /// A retired user could not be mapped.
    #[error("legacy owner map does not cover a retired user")]
    UnmappedLegacyUser,
}

#[derive(Deserialize)]
struct LegacyOwnerMapEntry {
    legacy_user_id: i64,
    owner_ref: String,
}

impl LegacyOwnerMap {
    /// Parses a complete JSON owner map with current Platform tenant references.
    ///
    /// # Errors
    ///
    /// Returns [`LegacyOwnerMapError`] for invalid, duplicate, or non-tenant
    /// mappings without echoing supplied data.
    pub fn from_json(document: &str) -> Result<Self, LegacyOwnerMapError> {
        let entries: Vec<LegacyOwnerMapEntry> =
            serde_json::from_str(document).map_err(|_| LegacyOwnerMapError::InvalidDocument)?;
        let mut owners = BTreeMap::new();
        for entry in entries {
            if entry.legacy_user_id <= 0 || TenantRef::parse(&entry.owner_ref).is_err() {
                return Err(LegacyOwnerMapError::InvalidDocument);
            }
            if owners
                .insert(entry.legacy_user_id, entry.owner_ref)
                .is_some()
            {
                return Err(LegacyOwnerMapError::DuplicateLegacyUser);
            }
        }
        Ok(Self { owners })
    }

    /// Resolves one retired user to its validated current tenant reference.
    ///
    /// # Errors
    ///
    /// Returns [`LegacyOwnerMapError::UnmappedLegacyUser`] when no mapping is
    /// available before any target write begins.
    pub fn owner_for(&self, legacy_user_id: i64) -> Result<&str, LegacyOwnerMapError> {
        self.owners
            .get(&legacy_user_id)
            .map(String::as_str)
            .ok_or(LegacyOwnerMapError::UnmappedLegacyUser)
    }
}

/// All non-secret material required to import one inspected legacy snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyImportRequest {
    /// Stable non-secret operator label for the isolated archive source.
    pub source_id: String,
    /// Exhaustive retired-user to current-tenant map.
    pub owner_map: LegacyOwnerMap,
    /// Fixed-query source data with no credential fields.
    pub snapshot: LegacySnapshot,
}

/// The durable result of one idempotent legacy import attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyImportOutcome {
    /// Catalog-owned audit identifier for this attempt.
    pub import_run_id: Uuid,
    /// New account bindings created by the attempt.
    pub accounts_imported: u32,
    /// New retired repository records imported by the attempt.
    pub repositories_imported: u32,
    /// New true-star claims imported by the attempt.
    pub star_claims_imported: u32,
    /// New legacy list-name claims imported by the attempt.
    pub list_claims_imported: u32,
}

/// A refused import that never includes source or credential values.
#[derive(Debug, thiserror::Error)]
pub enum LegacyImportError {
    /// The source label is not a bounded non-secret identifier.
    #[error("legacy import source identifier is invalid")]
    InvalidSourceId,
    /// An owner mapping did not cover the complete snapshot.
    #[error(transparent)]
    OwnerMap(#[from] LegacyOwnerMapError),
    /// The legacy snapshot had conflicting non-secret records.
    #[error("legacy snapshot has conflicting catalog records")]
    ConflictingSourceData,
    /// A mutable alias is already owned by a different repository.
    #[error("legacy repository alias conflicts with catalog identity")]
    AliasConflict,
    /// The catalog rejected the atomic import operation.
    #[error("legacy catalog import could not be persisted")]
    Persistence(#[source] sqlx::Error),
}

/// Deterministic, redacted reconciliation result for one imported source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyShadowReport {
    #[serde(skip_serializing)]
    /// Catalog-owned identifier for the stored report.
    pub report_id: Uuid,
    #[serde(skip_serializing)]
    /// SHA-256 digest of the canonical report body.
    pub report_digest: String,
    /// Non-secret source label.
    pub source_id: String,
    /// Imported accounts that still require a fresh credential.
    pub accounts_reauthorization_required: u32,
    /// Connected imported accounts without a completed full star snapshot.
    pub full_snapshots_missing: u32,
    /// Connected imported accounts without a completed native-list snapshot.
    pub list_snapshots_missing: u32,
    /// Imported current-star claims that disagree with the catalog projection.
    pub star_claims_mismatched: u32,
    /// Imported stars whose provider time remains unknown.
    pub provider_star_times_unknown: u32,
    /// Imported legacy list-name claims absent from provider list membership.
    pub list_claims_missing_from_provider: u32,
    /// Whether the report can be presented for owner cutover review.
    pub cutover_reviewable: bool,
}

/// Shadow-report generation failure with no source or credential values.
#[derive(Debug, thiserror::Error)]
pub enum LegacyShadowError {
    /// The requested source label was invalid.
    #[error("legacy shadow source identifier is invalid")]
    InvalidSourceId,
    /// A catalog query could not complete.
    #[error("legacy shadow report could not be read")]
    Query(#[source] sqlx::Error),
    /// The canonical report could not be encoded.
    #[error("legacy shadow report could not be encoded")]
    Encoding(#[source] serde_json::Error),
    /// The catalog could not persist the report.
    #[error("legacy shadow report could not be persisted")]
    Persistence(#[source] sqlx::Error),
    /// No clean report is available for owner review.
    #[error("legacy shadow report is not ready for cutover review")]
    NotReviewable,
}

/// Digest-bound evidence an owner may review before an external cutover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCutoverReadiness {
    /// Stored redacted report identifier.
    pub report_id: Uuid,
    /// Canonical report digest to bind in the owner approval.
    pub report_digest: String,
}

impl LegacyShadowReport {
    /// Renders the exact JSON body addressed by [`Self::report_digest`].
    ///
    /// # Errors
    ///
    /// Returns [`LegacyShadowError`] when serializing the fixed report shape
    /// fails.
    pub fn canonical_json(&self) -> Result<String, LegacyShadowError> {
        serde_json::to_string(self).map_err(LegacyShadowError::Encoding)
    }

    /// Renders a compact deterministic summary for an operator console.
    #[must_use]
    pub fn concise_text(&self) -> String {
        format!(
            "source_id={}\nreauthorization_required={}\nfull_snapshots_missing={}\nlist_snapshots_missing={}\nstar_claims_mismatched={}\nprovider_star_times_unknown={}\nlist_claims_missing_from_provider={}\ncutover_reviewable={}\nreport_digest={}",
            self.source_id,
            self.accounts_reauthorization_required,
            self.full_snapshots_missing,
            self.list_snapshots_missing,
            self.star_claims_mismatched,
            self.provider_star_times_unknown,
            self.list_claims_missing_from_provider,
            self.cutover_reviewable,
            self.report_digest,
        )
    }
}

mod shadow;

pub use shadow::{
    generate_legacy_shadow_report, legacy_cutover_readiness, legacy_shadow_account_ids,
};

/// Imports an already inspected non-secret snapshot in one target transaction.
///
/// # Errors
///
/// Returns [`LegacyImportError`] before opening a transaction for invalid
/// mappings and rolls every target write back when a persistence check fails.
pub async fn import_legacy_snapshot(
    database: &Database,
    request: LegacyImportRequest,
) -> Result<LegacyImportOutcome, LegacyImportError> {
    if !valid_source_id(&request.source_id) {
        return Err(LegacyImportError::InvalidSourceId);
    }
    let integrations = integrations_by_user(&request.snapshot.integrations)?;
    let mut owner_refs = BTreeMap::new();
    for legacy_user_id in request
        .snapshot
        .repositories
        .iter()
        .map(|repository| repository.legacy_user_id)
        .chain(integrations.keys().copied())
    {
        owner_refs.insert(
            legacy_user_id,
            request.owner_map.owner_for(legacy_user_id)?.to_owned(),
        );
    }

    let import_run_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.legacy_import_runs (import_run_id, source_id, status)
         values ($1, $2, 'running')",
    )
    .bind(import_run_id)
    .bind(&request.source_id)
    .execute(database.pool())
    .await
    .map_err(LegacyImportError::Persistence)?;

    let outcome = import_legacy_transaction(database, &request, import_run_id, &owner_refs).await;
    if let Err(error) = &outcome {
        record_import_failure(database, import_run_id, failure_code(error)).await;
    }
    outcome
}

async fn import_legacy_transaction(
    database: &Database,
    request: &LegacyImportRequest,
    import_run_id: Uuid,
    owner_refs: &BTreeMap<i64, String>,
) -> Result<LegacyImportOutcome, LegacyImportError> {
    let integrations = integrations_by_user(&request.snapshot.integrations)?;
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(LegacyImportError::Persistence)?;
    let (accounts, accounts_imported) = import_legacy_accounts(
        &mut transaction,
        &request.source_id,
        owner_refs,
        &integrations,
    )
    .await?;
    let mut counts = ImportCounts::default();
    for repository in &request.snapshot.repositories {
        counts.add(
            import_legacy_repository(&mut transaction, &request.source_id, repository, &accounts)
                .await?,
        );
    }
    complete_import_run(&mut transaction, import_run_id, accounts_imported, counts).await?;
    transaction
        .commit()
        .await
        .map_err(LegacyImportError::Persistence)?;
    let outcome = LegacyImportOutcome {
        import_run_id,
        accounts_imported,
        repositories_imported: counts.repositories,
        star_claims_imported: counts.star_claims,
        list_claims_imported: counts.list_claims,
    };
    tracing::info!(
        import_run_id = %outcome.import_run_id,
        accounts_imported = outcome.accounts_imported,
        repositories_imported = outcome.repositories_imported,
        star_claims_imported = outcome.star_claims_imported,
        list_claims_imported = outcome.list_claims_imported,
        "legacy catalog import completed"
    );
    Ok(outcome)
}

#[derive(Default, Copy, Clone)]
struct ImportCounts {
    repositories: u32,
    star_claims: u32,
    list_claims: u32,
}

impl ImportCounts {
    fn add(&mut self, value: Self) {
        self.repositories = self.repositories.saturating_add(value.repositories);
        self.star_claims = self.star_claims.saturating_add(value.star_claims);
        self.list_claims = self.list_claims.saturating_add(value.list_claims);
    }
}

async fn import_legacy_accounts(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: &str,
    owner_refs: &BTreeMap<i64, String>,
    integrations: &BTreeMap<i64, &LegacyIntegration>,
) -> Result<(BTreeMap<i64, Uuid>, u32), LegacyImportError> {
    let mut accounts = BTreeMap::new();
    let mut imported = 0_u32;
    for (legacy_user_id, owner_ref) in owner_refs {
        let (account_id, was_imported) = imported_account_id(
            transaction,
            source_id,
            *legacy_user_id,
            owner_ref,
            integrations.get(legacy_user_id),
        )
        .await?;
        imported = imported.saturating_add(u32::from(was_imported));
        accounts.insert(*legacy_user_id, account_id);
    }
    Ok((accounts, imported))
}

async fn import_legacy_repository(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: &str,
    repository: &LegacyRepository,
    accounts: &BTreeMap<i64, Uuid>,
) -> Result<ImportCounts, LegacyImportError> {
    let already_imported: Option<i64> = sqlx::query_scalar(
        "select legacy_repository_id
         from github_catalog.legacy_import_repository_records
         where source_id = $1 and legacy_repository_id = $2",
    )
    .bind(source_id)
    .bind(repository.legacy_repository_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    if already_imported.is_some() {
        return Ok(ImportCounts::default());
    }
    let account_id = accounts
        .get(&repository.legacy_user_id)
        .copied()
        .ok_or(LegacyImportError::ConflictingSourceData)?;
    let repository_id =
        crate::identity::upsert_repository_in_tx(transaction, repository.provider_repository_id)
            .await
            .map_err(map_identity_error)?;
    record_legacy_alias(
        transaction,
        repository_id,
        &repository.owner,
        &repository.name,
    )
    .await?;
    let observed_at = repository
        .observed_at
        .unwrap_or_else(OffsetDateTime::now_utc);
    let star_claims = u32::from(repository.starred);
    if repository.starred {
        import_legacy_star(transaction, account_id, repository_id, observed_at).await?;
    }
    let list_claims = import_legacy_list_claims(
        transaction,
        source_id,
        repository,
        account_id,
        repository_id,
        observed_at,
    )
    .await?;
    sqlx::query(
        "insert into github_catalog.legacy_import_repository_records
             (source_id, legacy_repository_id, account_id, repository_id, starred)
         values ($1, $2, $3, $4, $5)",
    )
    .bind(source_id)
    .bind(repository.legacy_repository_id)
    .bind(account_id)
    .bind(repository_id)
    .bind(repository.starred)
    .execute(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    Ok(ImportCounts {
        repositories: 1,
        star_claims,
        list_claims,
    })
}

fn map_identity_error(error: crate::PersistenceError) -> LegacyImportError {
    match error {
        crate::PersistenceError::Query(error)
        | crate::PersistenceError::Connect(error)
        | crate::PersistenceError::Schema(error) => LegacyImportError::Persistence(error),
    }
}

async fn import_legacy_star(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    repository_id: Uuid,
    observed_at: OffsetDateTime,
) -> Result<(), LegacyImportError> {
    sqlx::query(
        "insert into github_catalog.star_observations
             (observation_id, account_id, repository_id, starred,
              provider_starred_at, provider_starred_at_unknown, observed_at)
         values ($1, $2, $3, true, null, true, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(repository_id)
    .bind(observed_at)
    .execute(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at,
              provider_starred_at_unknown, last_observed_at)
         values ($1, $2, true, null, true, $3)
         on conflict (account_id, repository_id) do update set
             starred = true,
             starred_at = null,
             provider_starred_at_unknown = true,
             last_observed_at = excluded.last_observed_at,
             observed_unstarred_at = null,
             evidence_run_id = null",
    )
    .bind(account_id)
    .bind(repository_id)
    .bind(observed_at)
    .execute(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    Ok(())
}

async fn import_legacy_list_claims(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: &str,
    repository: &LegacyRepository,
    account_id: Uuid,
    repository_id: Uuid,
    observed_at: OffsetDateTime,
) -> Result<u32, LegacyImportError> {
    let mut count = 0_u32;
    for list_name in &repository.list_names {
        let list_name = list_name.trim();
        if list_name.is_empty() || list_name.len() > 256 {
            return Err(LegacyImportError::ConflictingSourceData);
        }
        let inserted = sqlx::query(
            "insert into github_catalog.legacy_list_claims
                 (source_id, legacy_repository_id, account_id, repository_id, list_name, observed_at)
             values ($1, $2, $3, $4, $5, $6)
             on conflict do nothing",
        )
        .bind(source_id)
        .bind(repository.legacy_repository_id)
        .bind(account_id)
        .bind(repository_id)
        .bind(list_name)
        .bind(observed_at)
        .execute(&mut **transaction)
        .await
        .map_err(LegacyImportError::Persistence)?;
        count = count.saturating_add(u32::try_from(inserted.rows_affected()).unwrap_or(u32::MAX));
    }
    Ok(count)
}

async fn complete_import_run(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    import_run_id: Uuid,
    accounts_imported: u32,
    counts: ImportCounts,
) -> Result<(), LegacyImportError> {
    sqlx::query(
        "update github_catalog.legacy_import_runs
         set status = 'completed', accounts_imported = $2, repositories_imported = $3,
             star_claims_imported = $4, list_claims_imported = $5, finished_at = now()
         where import_run_id = $1",
    )
    .bind(import_run_id)
    .bind(i32::try_from(accounts_imported).unwrap_or(i32::MAX))
    .bind(i32::try_from(counts.repositories).unwrap_or(i32::MAX))
    .bind(i32::try_from(counts.star_claims).unwrap_or(i32::MAX))
    .bind(i32::try_from(counts.list_claims).unwrap_or(i32::MAX))
    .execute(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    Ok(())
}

fn failure_code(error: &LegacyImportError) -> &'static str {
    match error {
        LegacyImportError::ConflictingSourceData => "conflicting_source_data",
        LegacyImportError::AliasConflict => "alias_conflict",
        LegacyImportError::Persistence(_)
        | LegacyImportError::InvalidSourceId
        | LegacyImportError::OwnerMap(_) => "persistence",
    }
}

async fn record_import_failure(database: &Database, import_run_id: Uuid, failure_code: &str) {
    let result = sqlx::query(
        "update github_catalog.legacy_import_runs
         set status = 'failed', failure_code = $2, finished_at = now()
         where import_run_id = $1 and status = 'running'",
    )
    .bind(import_run_id)
    .bind(failure_code)
    .execute(database.pool())
    .await;
    if result.is_err() {
        tracing::error!(import_run_id = %import_run_id, "legacy import failure evidence could not be recorded");
    }
}

fn valid_source_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.len() <= 64
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn integrations_by_user(
    integrations: &[LegacyIntegration],
) -> Result<BTreeMap<i64, &LegacyIntegration>, LegacyImportError> {
    let mut result = BTreeMap::new();
    for integration in integrations {
        if integration.legacy_user_id <= 0
            || integration
                .provider_user_id
                .is_some_and(|provider_user_id| provider_user_id <= 0)
            || result
                .insert(integration.legacy_user_id, integration)
                .is_some()
        {
            return Err(LegacyImportError::ConflictingSourceData);
        }
    }
    Ok(result)
}

async fn imported_account_id(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: &str,
    legacy_user_id: i64,
    owner_ref: &str,
    integration: Option<&&LegacyIntegration>,
) -> Result<(Uuid, bool), LegacyImportError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select account_id from github_catalog.legacy_import_accounts
         where source_id = $1 and legacy_user_id = $2",
    )
    .bind(source_id)
    .bind(legacy_user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    if let Some(account_id) = existing {
        return Ok((account_id, false));
    }
    let provider_user_id = integration.and_then(|value| value.provider_user_id);
    let provider_login = integration.and_then(|value| value.login.as_deref());
    let scopes = integration
        .and_then(|value| value.granted_scopes.as_deref())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let existing_provider_account: Option<Uuid> = if let Some(provider_user_id) = provider_user_id {
        sqlx::query_scalar(
            "select account_id from github_catalog.github_accounts
             where owner_ref = $1 and provider_user_id = $2",
        )
        .bind(owner_ref)
        .bind(provider_user_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(LegacyImportError::Persistence)?
    } else {
        None
    };
    let account_id = if let Some(account_id) = existing_provider_account {
        account_id
    } else {
        let account_id = Uuid::now_v7();
        sqlx::query(
            "insert into github_catalog.github_accounts
                 (account_id, owner_ref, status, provider_user_id, provider_login, granted_scopes)
             values ($1, $2, 'reauthorization_required', $3, $4, $5)",
        )
        .bind(account_id)
        .bind(owner_ref)
        .bind(provider_user_id)
        .bind(provider_login)
        .bind(scopes)
        .execute(&mut **transaction)
        .await
        .map_err(LegacyImportError::Persistence)?;
        account_id
    };
    sqlx::query(
        "insert into github_catalog.legacy_import_accounts (source_id, legacy_user_id, account_id)
         values ($1, $2, $3)",
    )
    .bind(source_id)
    .bind(legacy_user_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    Ok((account_id, true))
}

async fn record_legacy_alias(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repository_id: Uuid,
    owner: &str,
    name: &str,
) -> Result<(), LegacyImportError> {
    if owner.is_empty() || name.is_empty() {
        return Err(LegacyImportError::ConflictingSourceData);
    }
    let alias_value = format!("{owner}/{name}");
    let holder: Option<Uuid> = sqlx::query_scalar(
        "select repository_id from github_catalog.repository_aliases
         where alias_kind = 'owner_name' and alias_value = $1 and status = 'active'",
    )
    .bind(&alias_value)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(LegacyImportError::Persistence)?;
    match holder {
        Some(holder) if holder != repository_id => Err(LegacyImportError::AliasConflict),
        Some(_) => Ok(()),
        None => {
            sqlx::query(
                "insert into github_catalog.repository_aliases
                     (alias_id, repository_id, alias_kind, alias_value)
                 values ($1, $2, 'owner_name', $3)",
            )
            .bind(Uuid::now_v7())
            .bind(repository_id)
            .bind(alias_value)
            .execute(&mut **transaction)
            .await
            .map_err(LegacyImportError::Persistence)?;
            Ok(())
        }
    }
}

mod source;
