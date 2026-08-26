//! The incremental star scan flow: watermark-bounded ingestion of the
//! starred listing's newest entries under the shared rate budget.
//!
//! An incremental pass is a partial scan by construction, so it never
//! establishes a removal: it only upserts what it actually sees. Its safety
//! rests on provider ordering - items arrive newest-first by `starred_at`,
//! and coverage of the newer-than-watermark window is proven either by
//! observing an item at or below the mark or by the listing reporting
//! exhaustion. Any anomaly that breaks that ordering proof is a gap and
//! forces a full rescan; this module never papers over one.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::identity::{AliasKind, IdentityError, apply_alias_observation, upsert_repository};
use crate::provider::{GithubApi, StarredItem};
use crate::rate_limit::{AcquireError, RateLimitLedger, TokenRef};
use crate::snapshot::fail_run;

/// Recorded when the provider ordering can no longer prove what the scan
/// covered. The specific break travels alongside it in the failure reason.
const GAP_REASON: &str = "starred_at ordering gap detected";

/// What one incremental-scan attempt established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncrementalScanOutcome {
    /// Everything newer than the watermark was ingested and the mark
    /// advanced to the oldest timestamp the run saw.
    Completed {
        /// The run that owns the result.
        run_id: Uuid,
        /// Pages fetched, including the page that proved coverage.
        pages_processed: u32,
        /// Items ingested - only those strictly newer than the watermark.
        items_observed: u32,
    },
    /// The shared budget withheld the next page until `retry_at`; the run
    /// stays open and its checkpoint stands.
    Paused {
        /// The run waiting to continue.
        run_id: Uuid,
        /// When acquisition may proceed.
        retry_at: std::time::SystemTime,
    },
    /// The provider ordering could not be trusted (a missing or malformed
    /// `starred_at`, or a non-monotonic sequence); the run failed without
    /// side effects and a full rescan is required.
    GapDetected {
        /// The terminated run.
        run_id: Uuid,
    },
    /// The provider failed permanently partway through; prior authority and
    /// the watermark are untouched.
    Failed {
        /// The terminated run.
        run_id: Uuid,
    },
    /// No watermark existed for the account, so coverage could not be
    /// bounded; a full snapshot ran instead and its outcome applies.
    DeferredToFull(crate::FullSnapshotOutcome),
}

/// Failures of the incremental-scan flow beyond its outcomes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IncrementalScanError {
    /// Identity or alias handling failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Persistence failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// The provider exchange failed or was unclassifiable.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
    /// The delegated full-snapshot flow failed.
    #[error(transparent)]
    Snapshot(#[from] crate::snapshot::SnapshotError),
}

/// Runs one watermark-bounded incremental star scan for one account.
///
/// Pages are fetched newest-first by provider `starred_at`; items strictly
/// newer than the account's persisted high-water mark are ingested page by
/// page, each page durably recorded with its checkpoint before the next is
/// requested. The pass ends at the first item at or below the mark, or when
/// the provider reports exhaustion, and only then advances the watermark -
/// to the oldest timestamp the run saw, never past an established mark.
///
/// # Errors
///
/// Returns [`IncrementalScanError`] for provider or persistence failures.
pub async fn run_incremental_scan<G>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    account_id: Uuid,
) -> Result<IncrementalScanOutcome, IncrementalScanError>
where
    G: GithubApi,
{
    let watermark = read_watermark(database, account_id).await?;
    let Some(watermark) = watermark else {
        // No baseline exists, so coverage cannot be bounded; the full
        // snapshot establishes it instead.
        let outcome =
            crate::run_full_snapshot(database, gateway, ledger, token, account_id).await?;
        return Ok(IncrementalScanOutcome::DeferredToFull(outcome));
    };

    let (run_id, mut page, mut oldest_seen) = open_or_resume_run(database, account_id).await?;
    let mut items_observed: u32 = 0;
    // The oldest timestamp ingested anywhere in this run - the watermark's
    // advancement anchor once coverage is proven.
    let mut oldest_ingested: Option<OffsetDateTime> = None;
    loop {
        if let Err(AcquireError::RateLimited { retry_at }) = ledger.acquire(token) {
            return Ok(IncrementalScanOutcome::Paused { run_id, retry_at });
        }
        let reply = match gateway.list_starred_newest_first(None, page).await {
            Ok(reply) => reply,
            Err(error) => {
                fail_run(database, run_id, &error.to_string()).await?;
                return Ok(IncrementalScanOutcome::Failed { run_id });
            }
        };
        ledger.observe(token, &reply.rate_limit);

        // Ordering proof first: nothing is ingested from a page whose
        // sequence cannot be trusted.
        let parsed = match order_proof(&reply.page.items, oldest_seen) {
            Ok(parsed) => parsed,
            Err(reason_detail) => {
                fail_run(database, run_id, &format!("{GAP_REASON}: {reason_detail}")).await?;
                return Ok(IncrementalScanOutcome::GapDetected { run_id });
            }
        };

        // Ingestion takes the strictly-newer prefix; because the proven
        // sequence never increases, whatever follows proves coverage.
        let mut ingested: Vec<(OffsetDateTime, &StarredItem)> = Vec::with_capacity(parsed.len());
        for entry in &parsed {
            if entry.0 > watermark {
                ingested.push(*entry);
            }
        }
        let coverage_proven = ingested.len() < parsed.len();
        ingest_window(database, run_id, account_id, &ingested).await?;
        items_observed += counted_u32(ingested.len());
        if let Some((page_tail, _)) = ingested.last() {
            oldest_ingested = Some(match oldest_ingested {
                Some(established) => established.min(*page_tail),
                None => *page_tail,
            });
        }
        if let Some((newest_tail, _)) = parsed.last() {
            oldest_seen = Some(match oldest_seen {
                Some(seen) => seen.min(*newest_tail),
                None => *newest_tail,
            });
        }

        record_incremental_page(
            database,
            run_id,
            page,
            counted_i32(ingested.len()),
            oldest_seen,
        )
        .await?;

        // Coverage is proven by the first item at or below the watermark, or
        // by the provider reporting exhaustion; only then does the mark move,
        // anchored at the oldest timestamp ingested anywhere in the run.
        if coverage_proven || reply.page.items.is_empty() {
            complete_run(
                database,
                run_id,
                account_id,
                page,
                items_observed,
                oldest_ingested,
            )
            .await?;
            return Ok(IncrementalScanOutcome::Completed {
                run_id,
                pages_processed: page,
                items_observed,
            });
        }
        page += 1;
    }
}

/// Parses and validates one page's ordering continuation: timestamps must be
/// present, well-formed, and never increase relative to what the run has
/// already seen. Returns the parsed sequence or the reason the proof broke.
fn order_proof(
    items: &[StarredItem],
    mut previous: Option<OffsetDateTime>,
) -> Result<Vec<(OffsetDateTime, &StarredItem)>, String> {
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        let raw = item.starred_at.as_deref().ok_or_else(|| {
            "a listed item carries no starred_at, so its position cannot be proven".to_owned()
        })?;
        let timestamp = OffsetDateTime::parse(raw, &Rfc3339)
            .map_err(|error| format!("a listed item's starred_at is unparsable: {error}"))?;
        if previous.is_some_and(|seen| timestamp > seen) {
            return Err(format!(
                "the listing increased in starred_at from {previous:?} to {timestamp}"
            ));
        }
        previous = Some(timestamp);
        parsed.push((timestamp, item));
    }
    Ok(parsed)
}

/// Reads the account's persisted high-water mark, if one exists.
async fn read_watermark(
    database: &Database,
    account_id: Uuid,
) -> Result<Option<OffsetDateTime>, PersistenceError> {
    sqlx::query_scalar(
        "select high_water_mark from github_catalog.star_watermarks
         where account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)
}

/// Opens a fresh incremental run, or resumes the account's newest
/// interrupted one from its latest checkpoint's page position and ordering
/// boundary.
async fn open_or_resume_run(
    database: &Database,
    account_id: Uuid,
) -> Result<(Uuid, u32, Option<OffsetDateTime>), PersistenceError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select sync_run_id from github_catalog.sync_runs
         where account_id = $1 and mode = 'incremental' and status = 'running'
         order by started_at desc, sync_run_id desc
         limit 1",
    )
    .bind(account_id)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;

    if let Some(run_id) = existing {
        let checkpoint: Option<(i64, Option<OffsetDateTime>)> = sqlx::query_as(
            "select next_page, boundary_starred_at from github_catalog.sync_checkpoints
             where sync_run_id = $1
             order by recorded_at desc, next_page desc
             limit 1",
        )
        .bind(run_id)
        .fetch_optional(database.pool())
        .await
        .map_err(PersistenceError::Query)?;
        if let Some((next_page, boundary)) = checkpoint {
            let page = u32::try_from(next_page).unwrap_or(1);
            return Ok((run_id, page.max(1), boundary));
        }
        return Ok((run_id, 1, None));
    }

    let run_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.sync_runs (sync_run_id, account_id, mode, status)
         values ($1, $2, 'incremental', 'running')",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok((run_id, 1, None))
}

/// Ingests the strictly-newer window of one page into stable identity and
/// star state: additions become starred with their provider timestamp, and
/// continuations keep their established timestamp. Nothing outside the
/// window - and nothing about absence - is written.
async fn ingest_window(
    database: &Database,
    run_id: Uuid,
    account_id: Uuid,
    window: &[(OffsetDateTime, &StarredItem)],
) -> Result<(), IncrementalScanError> {
    for (seen_at, item) in window {
        let identity = upsert_repository(database, item.repo.provider_repository_id).await?;
        if let Some(owner_name) = item.repo.owner_name() {
            apply_alias_observation(
                database,
                item.repo.provider_repository_id,
                AliasKind::OwnerName,
                None,
                owner_name.to_string().as_str(),
            )
            .await?;
        }
        record_ingest(
            database,
            run_id,
            account_id,
            identity.repository_id,
            *seen_at,
        )
        .await?;
    }
    Ok(())
}

/// Durably records one ingested item: an append-only starred observation
/// plus its current-state projection, both carrying the establishing
/// provider timestamp and this run as evidence.
async fn record_ingest(
    database: &Database,
    run_id: Uuid,
    account_id: Uuid,
    repository_id: Uuid,
    seen_at: OffsetDateTime,
) -> Result<(), PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into github_catalog.star_observations
             (observation_id, account_id, repository_id, starred,
              provider_starred_at, observed_at)
         values (gen_random_uuid(), $1, $2, true, $3, now())",
    )
    .bind(account_id)
    .bind(repository_id)
    .bind(seen_at)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at,
              last_observed_at, evidence_run_id)
         values ($1, $2, true, $3, now(), $4)
         on conflict (account_id, repository_id) do update set
             starred = true,
             starred_at = coalesce(
                 github_catalog.current_star_state.starred_at,
                 excluded.starred_at),
             last_observed_at = now(),
             observed_unstarred_at = null,
             evidence_run_id = excluded.evidence_run_id",
    )
    .bind(account_id)
    .bind(repository_id)
    .bind(seen_at)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    // First star evidence over an unclassified entry promotes it to auto;
    // explicit tracked and ignored decisions are never overridden.
    sqlx::query(
        "update github_catalog.repositories
         set mode = 'auto', updated_at = now()
         where repository_id = $1 and mode is null",
    )
    .bind(repository_id)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    transaction.commit().await.map_err(PersistenceError::Query)
}

/// Durably records one processed page: the checkpoint carrying the page
/// position and the ordering boundary, plus the running statistics.
async fn record_incremental_page(
    database: &Database,
    run_id: Uuid,
    page: u32,
    ingested: i32,
    oldest_seen: Option<OffsetDateTime>,
) -> Result<(), PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into github_catalog.sync_checkpoints
             (checkpoint_id, sync_run_id, next_page, boundary_starred_at)
         values ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(run_id)
    .bind(i64::from(page) + 1)
    .bind(oldest_seen)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    sqlx::query(
        "update github_catalog.sync_runs
         set pages_processed = pages_processed + 1,
             items_observed = items_observed + $2
         where sync_run_id = $1",
    )
    .bind(run_id)
    .bind(ingested)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    transaction.commit().await.map_err(PersistenceError::Query)
}

/// Completes the run and advances the watermark in one transaction: the
/// mark moves to the oldest timestamp the run ingested, guarded so it can
/// never retreat below an established value. A pass that ingested nothing
/// advances nothing rather than inventing a mark.
async fn complete_run(
    database: &Database,
    run_id: Uuid,
    account_id: Uuid,
    page: u32,
    items_observed: u32,
    oldest_ingested: Option<OffsetDateTime>,
) -> Result<(), PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    if let Some(mark) = oldest_ingested {
        sqlx::query(
            "insert into github_catalog.star_watermarks (account_id, high_water_mark)
             values ($1, $2)
             on conflict (account_id) do update set
                 high_water_mark = greatest(
                     github_catalog.star_watermarks.high_water_mark,
                     excluded.high_water_mark),
                 updated_at = now()",
        )
        .bind(account_id)
        .bind(mark)
        .execute(&mut *transaction)
        .await
        .map_err(PersistenceError::Query)?;
    }
    sqlx::query(
        "update github_catalog.sync_runs
         set status = 'completed',
             finished_at = now(),
             pages_processed = $2,
             items_observed = $3
         where sync_run_id = $1",
    )
    .bind(run_id)
    .bind(i32::try_from(page).unwrap_or(i32::MAX))
    .bind(i32::try_from(items_observed).unwrap_or(i32::MAX))
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    transaction.commit().await.map_err(PersistenceError::Query)
}

/// Converts an in-memory collection size to the statistics column type
/// without truncating silently.
fn counted_i32(length: usize) -> i32 {
    i32::try_from(length).unwrap_or(i32::MAX)
}

/// Converts an in-memory collection size to the outcome counter type
/// without truncating silently.
fn counted_u32(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}
