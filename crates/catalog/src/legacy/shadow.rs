//! Redacted shadow comparison and owner-readiness evidence.

use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::{LegacyCutoverReadiness, LegacyShadowError, LegacyShadowReport};
use crate::Database;

/// Compares imported evidence against catalog projections and stores a digest.
///
/// This function performs no provider request itself: callers must first run
/// the regular full star and list snapshots for each connected account.
///
/// # Errors
///
/// Returns [`LegacyShadowError`] without exposing source locations,
/// credentials, raw provider responses, or repository names.
pub async fn generate_legacy_shadow_report(
    database: &Database,
    source_id: &str,
) -> Result<LegacyShadowReport, LegacyShadowError> {
    if !super::valid_source_id(source_id) {
        return Err(LegacyShadowError::InvalidSourceId);
    }
    let counts = collect_shadow_counts(database, source_id).await?;
    let mut report = LegacyShadowReport {
        report_id: Uuid::now_v7(),
        report_digest: String::new(),
        source_id: source_id.to_owned(),
        accounts_reauthorization_required: counts.accounts_reauthorization_required,
        full_snapshots_missing: counts.full_snapshots_missing,
        list_snapshots_missing: counts.list_snapshots_missing,
        star_claims_mismatched: counts.star_claims_mismatched,
        provider_star_times_unknown: counts.provider_star_times_unknown,
        list_claims_missing_from_provider: counts.list_claims_missing_from_provider,
        cutover_reviewable: counts.is_clean(),
    };
    persist_shadow_report(database, &mut report).await?;
    Ok(report)
}

/// Lists connected imported accounts eligible for one fresh shadow snapshot.
///
/// # Errors
///
/// Returns [`LegacyShadowError`] without revealing owner references or source
/// locations.
pub async fn legacy_shadow_account_ids(
    database: &Database,
    source_id: &str,
) -> Result<Vec<Uuid>, LegacyShadowError> {
    if !super::valid_source_id(source_id) {
        return Err(LegacyShadowError::InvalidSourceId);
    }
    sqlx::query_scalar(
        "select ia.account_id from github_catalog.legacy_import_accounts ia join github_catalog.github_accounts a on a.account_id = ia.account_id where ia.source_id = $1 and a.status = 'connected' order by ia.account_id",
    )
    .bind(source_id)
    .fetch_all(database.pool())
    .await
    .map_err(LegacyShadowError::Query)
}

/// Loads the latest clean shadow evidence for an external owner review.
///
/// This validates readiness only; it neither records approval nor changes a
/// read or write route.
///
/// # Errors
///
/// Returns [`LegacyShadowError::NotReviewable`] when no clean report exists.
pub async fn legacy_cutover_readiness(
    database: &Database,
    source_id: &str,
) -> Result<LegacyCutoverReadiness, LegacyShadowError> {
    if !super::valid_source_id(source_id) {
        return Err(LegacyShadowError::InvalidSourceId);
    }
    let report: Option<(Uuid, String)> = sqlx::query_as(
        "select report_id, report_digest from github_catalog.legacy_shadow_reports where source_id = $1 and cutover_reviewable order by created_at desc, report_id desc limit 1",
    )
    .bind(source_id)
    .fetch_optional(database.pool())
    .await
    .map_err(LegacyShadowError::Query)?;
    let (report_id, report_digest) = report.ok_or(LegacyShadowError::NotReviewable)?;
    Ok(LegacyCutoverReadiness {
        report_id,
        report_digest,
    })
}

#[derive(Default)]
struct ShadowCounts {
    accounts_reauthorization_required: u32,
    full_snapshots_missing: u32,
    list_snapshots_missing: u32,
    star_claims_mismatched: u32,
    provider_star_times_unknown: u32,
    list_claims_missing_from_provider: u32,
}

impl ShadowCounts {
    fn is_clean(&self) -> bool {
        self.accounts_reauthorization_required == 0
            && self.full_snapshots_missing == 0
            && self.list_snapshots_missing == 0
            && self.star_claims_mismatched == 0
            && self.provider_star_times_unknown == 0
            && self.list_claims_missing_from_provider == 0
    }
}

async fn collect_shadow_counts(
    database: &Database,
    source_id: &str,
) -> Result<ShadowCounts, LegacyShadowError> {
    Ok(ShadowCounts {
        accounts_reauthorization_required: shadow_count(database, "select count(*) from github_catalog.legacy_import_accounts ia join github_catalog.github_accounts a on a.account_id = ia.account_id where ia.source_id = $1 and a.status <> 'connected'", source_id).await?,
        full_snapshots_missing: snapshot_missing_count(database, source_id, "full").await?,
        list_snapshots_missing: snapshot_missing_count(database, source_id, "star_lists").await?,
        star_claims_mismatched: shadow_count(database, "select count(*) from github_catalog.legacy_import_repository_records ir left join github_catalog.current_star_state cs on cs.account_id = ir.account_id and cs.repository_id = ir.repository_id where ir.source_id = $1 and ir.starred is distinct from coalesce(cs.starred, false)", source_id).await?,
        provider_star_times_unknown: shadow_count(database, "select count(*) from github_catalog.legacy_import_repository_records ir join github_catalog.current_star_state cs on cs.account_id = ir.account_id and cs.repository_id = ir.repository_id where ir.source_id = $1 and ir.starred and cs.starred and cs.provider_starred_at_unknown", source_id).await?,
        list_claims_missing_from_provider: shadow_count(database, "select count(*) from github_catalog.legacy_list_claims lc left join github_catalog.star_lists sl on sl.account_id = lc.account_id and sl.name = lc.list_name and sl.status = 'active' left join github_catalog.star_list_memberships lm on lm.list_id = sl.list_id and lm.repository_id = lc.repository_id and lm.member where lc.source_id = $1 and lm.list_id is null", source_id).await?,
    })
}

async fn snapshot_missing_count(
    database: &Database,
    source_id: &str,
    mode: &str,
) -> Result<u32, LegacyShadowError> {
    shadow_count(database, &format!("select count(*) from github_catalog.legacy_import_accounts ia join github_catalog.github_accounts a on a.account_id = ia.account_id left join github_catalog.github_account_credentials credential on credential.account_id = ia.account_id where ia.source_id = $1 and a.status = 'connected' and (credential.account_id is null or not exists (select 1 from github_catalog.sync_runs sr where sr.account_id = ia.account_id and sr.mode = '{mode}' and sr.status = 'completed' and sr.finished_at >= credential.updated_at))"), source_id).await
}

async fn shadow_count(
    database: &Database,
    statement: &str,
    source_id: &str,
) -> Result<u32, LegacyShadowError> {
    let count: i64 = sqlx::query_scalar(statement)
        .bind(source_id)
        .fetch_one(database.pool())
        .await
        .map_err(LegacyShadowError::Query)?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

async fn persist_shadow_report(
    database: &Database,
    report: &mut LegacyShadowReport,
) -> Result<(), LegacyShadowError> {
    let body = report.canonical_json()?;
    report.report_digest = format!("{:x}", Sha256::digest(body.as_bytes()));
    sqlx::query("insert into github_catalog.legacy_shadow_reports (report_id, source_id, report_digest, report, cutover_reviewable) values ($1, $2, $3, $4, $5)")
        .bind(report.report_id).bind(&report.source_id).bind(&report.report_digest)
        .bind(serde_json::to_value(&*report).map_err(LegacyShadowError::Encoding)?)
        .bind(report.cutover_reviewable).execute(database.pool()).await.map_err(LegacyShadowError::Persistence)?;
    tracing::info!(report_id = %report.report_id, report_digest = %report.report_digest, cutover_reviewable = report.cutover_reviewable, accounts_reauthorization_required = report.accounts_reauthorization_required, full_snapshots_missing = report.full_snapshots_missing, list_snapshots_missing = report.list_snapshots_missing, star_claims_mismatched = report.star_claims_mismatched, provider_star_times_unknown = report.provider_star_times_unknown, list_claims_missing_from_provider = report.list_claims_missing_from_provider, "legacy shadow report generated");
    Ok(())
}
