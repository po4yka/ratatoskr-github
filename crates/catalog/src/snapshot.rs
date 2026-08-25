//! The full star snapshot flow: complete enumeration under the shared rate
//! budget, resumable checkpoints, and the atomic promotion of a completed
//! snapshot into star authority.
//!
//! Authority over what is starred lives exclusively in [`crate::observe`]'s
//! sibling tables `current_star_state` and append-only `star_observations`;
//! this module writes them only inside one final transaction, never while
//! pages are still arriving.

use uuid::Uuid;

use crate::database::Database;
use crate::identity::{AliasKind, IdentityError, apply_alias_observation, upsert_repository};
use crate::provider::GithubApi;
use crate::rate_limit::{AcquireError, RateLimitLedger, TokenRef};

/// What one full-snapshot attempt established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullSnapshotOutcome {
    /// The whole listing was traversed and its authority was applied.
    Completed {
        /// The run that owns the result.
        run_id: Uuid,
        /// Pages fetched, including the empty terminator page.
        pages_processed: u32,
        /// Starred-listing entries seen across all pages.
        items_observed: u32,
        /// Repositories newly starred under this snapshot's authority.
        additions: u32,
        /// Prior stars this snapshot evidenced as removed.
        unstars: u32,
    },
    /// The shared budget withheld the next page until `retry_at`; the run
    /// stays open and its checkpoint stands.
    Paused {
        /// The run waiting to continue.
        run_id: Uuid,
        /// When acquisition may proceed.
        retry_at: std::time::SystemTime,
    },
    /// The provider failed permanently partway through; prior authority is
    /// untouched and the failure is recorded on the run row.
    Failed {
        /// The terminated run.
        run_id: Uuid,
    },
}

/// Failures of the full-snapshot flow beyond its outcomes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Identity or alias handling failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Persistence failed.
    #[error(transparent)]
    Persistence(#[from] crate::database::PersistenceError),
    /// The provider exchange failed or was unclassifiable.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
}

/// Runs a complete starred-repository enumeration for one account.
///
/// Pages are fetched in ascending order through the shared budget; each page
/// is durably staged together with its checkpoint before the next page is
/// requested, so an interrupted run can resume without refetching it.
///
/// # Errors
///
/// Returns [`SnapshotError`] for provider or persistence failures.
pub async fn run_full_snapshot<G>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    account_id: Uuid,
) -> Result<FullSnapshotOutcome, SnapshotError>
where
    G: GithubApi,
{
    let (run_id, mut page) = open_or_resume_run(database, account_id).await?;
    loop {
        if let Err(AcquireError::RateLimited { retry_at }) = ledger.acquire(token) {
            return Ok(FullSnapshotOutcome::Paused { run_id, retry_at });
        }
        let reply = match gateway.list_starred(None, page).await {
            Ok(reply) => reply,
            Err(error) => {
                fail_run(database, run_id, &error.to_string()).await?;
                return Ok(FullSnapshotOutcome::Failed { run_id });
            }
        };
        ledger.observe(token, &reply.rate_limit);
        record_page(database, run_id, page, &reply.page.items).await?;
        if reply.page.items.is_empty() {
            let (pages_processed, items_observed, additions, unstars) =
                apply_authority_and_complete(database, run_id, account_id).await?;
            return Ok(FullSnapshotOutcome::Completed {
                run_id,
                pages_processed,
                items_observed,
                additions,
                unstars,
            });
        }
        page += 1;
    }
}

/// Opens a fresh run, or resumes the account's newest interrupted one: an
/// unfinished run keeps its checkpoints, so the scan continues from the next
/// unprocessed page instead of refetching what already landed.
async fn open_or_resume_run(
    database: &Database,
    account_id: Uuid,
) -> Result<(Uuid, u32), crate::database::PersistenceError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select sync_run_id from github_catalog.sync_runs
         where account_id = $1 and mode = 'full' and status = 'running'
         order by started_at desc, sync_run_id desc
         limit 1",
    )
    .bind(account_id)
    .fetch_optional(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    if let Some(run_id) = existing {
        let next_page: Option<i64> = sqlx::query_scalar(
            "select next_page from github_catalog.sync_checkpoints
             where sync_run_id = $1
             order by recorded_at desc, next_page desc
             limit 1",
        )
        .bind(run_id)
        .fetch_optional(database.pool())
        .await
        .map_err(crate::database::PersistenceError::Query)?;
        let page = u32::try_from(next_page.unwrap_or(1)).unwrap_or(1);
        return Ok((run_id, page));
    }

    let run_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.sync_runs (sync_run_id, account_id, mode, status)
         values ($1, $2, 'full', 'running')",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok((run_id, 1))
}

/// Durably records one processed page: identity upserts, staged items, the
/// page checkpoint, and the running statistics, all observable only after
/// they are all true.
async fn record_page(
    database: &Database,
    run_id: Uuid,
    page: u32,
    items: &[crate::provider::StarredItem],
) -> Result<(), SnapshotError> {
    for item in items {
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
        let _ = identity;
    }

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    let base_position: i64 = sqlx::query_scalar(
        "select coalesce(max(position), -1) + 1 from github_catalog.snapshot_items
         where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    for (offset, item) in items.iter().enumerate() {
        sqlx::query(
            "insert into github_catalog.snapshot_items
                 (sync_run_id, position, provider_repository_id, provider_starred_at)
             values ($1, $2, $3, $4::timestamptz)",
        )
        .bind(run_id)
        .bind(
            base_position
                .checked_add(i64::try_from(offset).unwrap_or(i64::MAX))
                .unwrap_or(i64::MAX),
        )
        .bind(item.repo.provider_repository_id)
        .bind(item.starred_at.as_deref())
        .execute(&mut *transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    }
    sqlx::query(
        "insert into github_catalog.sync_checkpoints (checkpoint_id, sync_run_id, next_page)
         values ($1, $2, $3)",
    )
    .bind(Uuid::now_v7())
    .bind(run_id)
    .bind(i64::from(page) + 1)
    .execute(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    sqlx::query(
        "update github_catalog.sync_runs
         set pages_processed = pages_processed + 1,
             items_observed = items_observed + $2
         where sync_run_id = $1",
    )
    .bind(run_id)
    .bind(item_count_as_i32(items))
    .execute(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    transaction
        .commit()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Converts an in-memory collection size to the run statistics column type
/// without truncating silently.
fn item_count_as_i32(items: &[crate::provider::StarredItem]) -> i32 {
    i32::try_from(items.len()).unwrap_or(i32::MAX)
}

/// Terminates a run as failed: the failure reason is recorded, the dead
/// run's staging is cleared so it cannot leak into later runs, and star
/// authority is deliberately untouched.
pub(crate) async fn fail_run(
    database: &Database,
    run_id: Uuid,
    reason: &str,
) -> Result<(), crate::database::PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    sqlx::query(
        "update github_catalog.sync_runs
         set status = 'failed', finished_at = now(), failure_reason = $2
         where sync_run_id = $1 and status = 'running'",
    )
    .bind(run_id)
    .bind(reason)
    .execute(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    sqlx::query("delete from github_catalog.snapshot_items where sync_run_id = $1")
        .bind(run_id)
        .execute(&mut *transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    transaction
        .commit()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Promotes the completed snapshot to be the sole star authority and marks
/// the run finished, all inside one transaction: additions become starred,
/// continuing stars keep their established starred-at, absences become
/// evidenced unstars, staging is cleared, and statistics are finalized.
/// Readers see either the whole previous authority or the whole new one.
async fn apply_authority_and_complete(
    database: &Database,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(u32, u32, u32, u32), crate::database::PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(crate::database::PersistenceError::Query)?;

    let additions = count_additions(&mut transaction, run_id, account_id).await?;
    record_star_observations(&mut transaction, run_id, account_id).await?;
    // Restore repairs must be captured before the promotion flips the very
    // state they describe.
    record_restore_repairs(&mut transaction, run_id, account_id).await?;
    promote_seen_repositories(&mut transaction, run_id, account_id).await?;
    let unstar_count = unstar_absent_repositories(&mut transaction, run_id, account_id).await?;
    reanchor_watermark(&mut transaction, run_id, account_id).await?;
    clear_staging(&mut transaction, run_id).await?;
    let (pages_processed, items_observed) =
        complete_run_row(&mut transaction, run_id, additions, unstar_count).await?;

    transaction
        .commit()
        .await
        .map_err(crate::database::PersistenceError::Query)?;

    Ok((pages_processed, items_observed, additions, unstar_count))
}

/// Records a named drift repair for every repository the fresh listing
/// presents again while local state still holds it unstarred - the mark of
/// an addition an incremental pass missed. Captured before promotion flips
/// that state; keyed per run so repetition cannot duplicate a row.
async fn record_restore_repairs(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.reconciliation_repairs
             (sync_run_id, repository_id, action)
         select $1, r.repository_id, 'restore_after_miss'
         from github_catalog.snapshot_items si
         join github_catalog.repositories r
             on r.provider_repository_id = si.provider_repository_id
         join github_catalog.current_star_state c
             on c.account_id = $2 and c.repository_id = r.repository_id
         where si.sync_run_id = $1 and not c.starred
         on conflict do nothing",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Re-anchors the account's incremental baseline from this completed
/// enumeration: the watermark moves to the newest observed provider
/// starred-at, guarded so it never retreats. An enumeration that observed
/// no timestamps leaves any existing mark alone rather than inventing one.
async fn reanchor_watermark(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.star_watermarks (account_id, high_water_mark)
         select $2, anchored.newest from (
             select max(si.provider_starred_at) as newest
             from github_catalog.snapshot_items si
             where si.sync_run_id = $1 and si.provider_starred_at is not null
         ) anchored
         where anchored.newest is not null
         on conflict (account_id) do update set
             high_water_mark = greatest(
                 github_catalog.star_watermarks.high_water_mark,
                 excluded.high_water_mark),
             updated_at = now()",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Counts repositories this snapshot saw that were not starred before it.
async fn count_additions(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<u32, crate::database::PersistenceError> {
    // Repositories this snapshot saw but which were not starred before.
    let additions: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.snapshot_items si
         join github_catalog.repositories r
             on r.provider_repository_id = si.provider_repository_id
         where si.sync_run_id = $1 and not exists (
             select 1 from github_catalog.current_star_state c
             where c.account_id = $2 and c.repository_id = r.repository_id and c.starred)",
    )
    .bind(run_id)
    .bind(account_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(u32::try_from(additions).unwrap_or(u32::MAX))
}

/// Appends one starred observation fact per seen repository.
async fn record_star_observations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.star_observations
             (observation_id, account_id, repository_id, starred,
              provider_starred_at, observed_at)
         select gen_random_uuid(), $2, r.repository_id, true,
                si.provider_starred_at, now()
         from github_catalog.snapshot_items si
         join github_catalog.repositories r
             on r.provider_repository_id = si.provider_repository_id
         where si.sync_run_id = $1",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// The authority swap itself: seen repositories become starred, with the
/// earliest established starred-at preserved across confirmations; a re-star
/// takes the fresh provider value because unstars cleared the old one.
async fn promote_seen_repositories(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at,
              last_observed_at, evidence_run_id)
         select $2, r.repository_id, true, si.provider_starred_at, now(), $1
         from github_catalog.snapshot_items si
         join github_catalog.repositories r
             on r.provider_repository_id = si.provider_repository_id
         where si.sync_run_id = $1
         on conflict (account_id, repository_id) do update set
             starred = true,
             starred_at = coalesce(
                 github_catalog.current_star_state.starred_at,
                 excluded.starred_at),
             last_observed_at = now(),
             observed_unstarred_at = null,
             evidence_run_id = excluded.evidence_run_id",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Absence from a complete listing is removal evidence: prior stars absent
/// from this snapshot become evidenced unstars plus append-only observation
/// rows - never silent deletions.
async fn unstar_absent_repositories(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<u32, crate::database::PersistenceError> {
    let absent_repositories: Vec<Uuid> = sqlx::query_scalar(
        "update github_catalog.current_star_state c
         set starred = false,
             observed_unstarred_at = now(),
             evidence_run_id = $1,
             starred_at = null
         where c.account_id = $2 and c.starred and not exists (
             select 1 from github_catalog.snapshot_items si
             join github_catalog.repositories r
                 on r.provider_repository_id = si.provider_repository_id
             where si.sync_run_id = $1 and r.repository_id = c.repository_id)
         returning c.repository_id",
    )
    .bind(run_id)
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?
    .into_iter()
    .collect();
    for repository_id in &absent_repositories {
        sqlx::query(
            "insert into github_catalog.star_observations
                 (observation_id, account_id, repository_id, starred,
                  provider_starred_at, observed_at)
              values (gen_random_uuid(), $1, $2, false, null, now())",
        )
        .bind(account_id)
        .bind(repository_id)
        .execute(&mut **transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
        sqlx::query(
            "insert into github_catalog.reconciliation_repairs
                 (sync_run_id, repository_id, action)
              values ($1, $2, 'unstar_after_drift')
              on conflict do nothing",
        )
        .bind(run_id)
        .bind(repository_id)
        .execute(&mut **transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    }
    let count = u32::try_from(absent_repositories.len()).unwrap_or(u32::MAX);
    Ok(count)
}

/// Clears the dead-weight staging rows of a finished run.
async fn clear_staging(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query("delete from github_catalog.snapshot_items where sync_run_id = $1")
        .bind(run_id)
        .execute(&mut **transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Marks the run completed with its final transition statistics.
async fn complete_run_row(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    additions: u32,
    unstars: u32,
) -> Result<(u32, u32), crate::database::PersistenceError> {
    let row: (i32, i32) = sqlx::query_as(
        "update github_catalog.sync_runs
         set status = 'completed',
             finished_at = now(),
             additions = $2,
             unstars = $3
         where sync_run_id = $1
         returning pages_processed, items_observed",
    )
    .bind(run_id)
    .bind(i32::try_from(additions).unwrap_or(i32::MAX))
    .bind(i32::try_from(unstars).unwrap_or(i32::MAX))
    .fetch_one(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok((
        u32::try_from(row.0).unwrap_or_default(),
        u32::try_from(row.1).unwrap_or_default(),
    ))
}
