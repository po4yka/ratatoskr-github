//! The star-list snapshot flow: complete enumeration of the account's
//! native lists and memberships under the shared rate budget, resumable
//! cursor checkpoints, and the atomic promotion of a completed enumeration
//! into list authority.
//!
//! List authority lives in `star_lists`, the current membership projection
//! in `star_list_memberships`, and append-only evidence in
//! `star_list_membership_observations`; this module writes them only inside
//! one final transaction, never while pages are still arriving.

use uuid::Uuid;

use crate::database::Database;
use crate::identity::{AliasKind, apply_alias_observation, upsert_repository};
use crate::provider::{GithubApi, UserListsPage};
use crate::rate_limit::{AcquireError, RateLimitLedger, TokenRef};

/// What one star-list snapshot attempt established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StarListSnapshotOutcome {
    /// The whole listing was traversed and its authority was applied.
    Completed {
        /// The run that owns the result.
        run_id: Uuid,
        /// Pages fetched, including the empty terminator page.
        pages_processed: u32,
        /// Membership entries seen across all pages.
        items_observed: u32,
        /// Distinct lists seen across all pages.
        lists_observed: u32,
        /// Memberships newly established under this snapshot's authority.
        additions: u32,
        /// Prior memberships this snapshot evidenced as removed.
        removals: u32,
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

/// Failures of the star-list snapshot flow beyond its outcomes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StarListsError {
    /// Identity or alias handling failed.
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    /// Persistence failed.
    #[error(transparent)]
    Persistence(#[from] crate::database::PersistenceError),
    /// The provider exchange failed or was unclassifiable.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
}

/// One active native list of an account, as promoted authority spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarListSummary {
    /// The local list identity.
    pub list_id: Uuid,
    /// The stable provider list identity (the GraphQL node id).
    pub provider_list_id: String,
    /// The list name as currently spelled upstream.
    pub name: String,
}

/// One current member of a native list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMember {
    /// The member repository's local identity.
    pub repository_id: Uuid,
}

/// Runs a complete native star-list enumeration for one account.
///
/// Pages are fetched in continuation order through the shared budget; each
/// page is durably staged together with its checkpoint before the next page
/// is requested, so an interrupted run can resume without refetching it.
///
/// # Errors
///
/// Returns [`StarListsError`] for provider or persistence failures.
pub async fn run_star_list_snapshot<G>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    account_id: Uuid,
) -> Result<StarListSnapshotOutcome, StarListsError>
where
    G: GithubApi,
{
    let (run_id, mut cursor) = open_or_resume_run(database, account_id).await?;
    loop {
        if let Err(AcquireError::RateLimited { retry_at }) = ledger.acquire(token) {
            return Ok(StarListSnapshotOutcome::Paused { run_id, retry_at });
        }
        let reply = match gateway.list_user_lists(None, cursor.as_deref()).await {
            Ok(reply) => reply,
            Err(error) => {
                crate::snapshot::fail_run(database, run_id, &error.to_string()).await?;
                return Ok(StarListSnapshotOutcome::Failed { run_id });
            }
        };
        ledger.observe(token, &reply.rate_limit);
        // A list holding more items than one page carries makes this
        // enumeration incomplete; incomplete enumerations never become
        // authority, so the run dies before anything from it is staged.
        if let Some(truncated) = reply.page.lists.iter().find(|list| list.items_truncated) {
            crate::snapshot::fail_run(
                database,
                run_id,
                &format!(
                    "truncated list membership: list {} holds more items than one page carries",
                    truncated.provider_list_id
                ),
            )
            .await?;
            return Ok(StarListSnapshotOutcome::Failed { run_id });
        }
        record_page(database, run_id, &reply.page).await?;
        if reply.page.lists.is_empty() {
            let (pages_processed, items_observed, additions, removals) =
                apply_list_authority_and_complete(database, run_id, account_id).await?;
            return Ok(StarListSnapshotOutcome::Completed {
                run_id,
                pages_processed,
                items_observed,
                lists_observed: read_lists_observed(database, run_id).await?,
                additions,
                removals,
            });
        }
        cursor = reply.page.next_cursor;
    }
}

/// Opens a fresh star-list run, or resumes the account's newest interrupted
/// one from its recorded continuation token, so an interrupted scan never
/// refetches a completed page.
async fn open_or_resume_run(
    database: &Database,
    account_id: Uuid,
) -> Result<(Uuid, Option<String>), crate::database::PersistenceError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select sync_run_id from github_catalog.sync_runs
         where account_id = $1 and mode = 'star_lists' and status = 'running'
         order by started_at desc, sync_run_id desc
         limit 1",
    )
    .bind(account_id)
    .fetch_optional(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    if let Some(run_id) = existing {
        let cursor: Option<String> = sqlx::query_scalar(
            "select graphql_cursor from github_catalog.sync_checkpoints
             where sync_run_id = $1
             order by recorded_at desc
             limit 1",
        )
        .bind(run_id)
        .fetch_optional(database.pool())
        .await
        .map_err(crate::database::PersistenceError::Query)?
        .flatten();
        return Ok((run_id, cursor));
    }

    let run_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.sync_runs (sync_run_id, account_id, mode, status)
         values ($1, $2, 'star_lists', 'running')",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok((run_id, None))
}

/// Durably records one processed page: identity upserts for every listed
/// repository, staged membership rows, the continuation-token checkpoint,
/// and the running statistics - all observable only after they are all true.
async fn record_page(
    database: &Database,
    run_id: Uuid,
    page: &UserListsPage,
) -> Result<(), StarListsError> {
    for list in &page.lists {
        for item in &list.items {
            upsert_repository(database, item.provider_repository_id).await?;
            if let Some((owner, name)) = item.full_name.split_once('/').filter(|(owner, name)| {
                !owner.is_empty() && !name.is_empty() && !name.contains('/')
            }) {
                apply_alias_observation(
                    database,
                    item.provider_repository_id,
                    AliasKind::OwnerName,
                    None,
                    item.full_name.as_str(),
                )
                .await?;
                let _ = (owner, name);
            }
        }
    }

    let distinct_lists = page
        .lists
        .iter()
        .map(|list| list.provider_list_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    let base_position: i64 = sqlx::query_scalar(
        "select coalesce(max(position), -1) + 1 from github_catalog.list_snapshot_items
         where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    let mut position = base_position;
    for list in &page.lists {
        for item in &list.items {
            sqlx::query(
                "insert into github_catalog.list_snapshot_items
                     (sync_run_id, position, provider_list_id, list_name,
                      provider_repository_id)
                 values ($1, $2, $3, $4, $5)",
            )
            .bind(run_id)
            .bind(position)
            .bind(&list.provider_list_id)
            .bind(&list.name)
            .bind(item.provider_repository_id)
            .execute(&mut *transaction)
            .await
            .map_err(crate::database::PersistenceError::Query)?;
            position += 1;
        }
    }
    // The ordinal column stays satisfied even though resume is cursor-based;
    // null marks the first page.
    sqlx::query(
        "insert into github_catalog.sync_checkpoints
             (checkpoint_id, sync_run_id, next_page, graphql_cursor)
         values ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(run_id)
    .bind(base_position + 1)
    .bind(page.next_cursor.as_deref())
    .execute(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    sqlx::query(
        "update github_catalog.sync_runs
         set pages_processed = pages_processed + 1,
             items_observed = items_observed + $2,
             lists_observed = lists_observed + $3
         where sync_run_id = $1",
    )
    .bind(run_id)
    .bind(
        i32::try_from(
            page.lists
                .iter()
                .map(|list| list.items.len())
                .sum::<usize>(),
        )
        .unwrap_or(i32::MAX),
    )
    .bind(i32::try_from(distinct_lists).unwrap_or(i32::MAX))
    .execute(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    transaction
        .commit()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Reads back the accumulated distinct-list statistic of a run.
async fn read_lists_observed(
    database: &Database,
    run_id: Uuid,
) -> Result<u32, crate::database::PersistenceError> {
    let lists_observed: i32 = sqlx::query_scalar(
        "select lists_observed from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(u32::try_from(lists_observed).unwrap_or_default())
}

/// Promotes the completed enumeration to be the sole list authority and
/// marks the run finished, all inside one transaction: lists are upserted
/// with their current names, every seen membership becomes an observation,
/// staged pairs become members, locally-member-but-absent pairs become
/// evidenced removals, absent lists are tombstoned, staging is cleared, and
/// statistics are finalized. Readers see either the whole previous authority
/// or the whole new one.
async fn apply_list_authority_and_complete(
    database: &Database,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(u32, u32, u32, u32), crate::database::PersistenceError> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(crate::database::PersistenceError::Query)?;

    upsert_lists_from_staging(&mut transaction, run_id, account_id).await?;
    let additions = count_member_additions(&mut transaction, run_id, account_id).await?;
    record_membership_observations(&mut transaction, run_id).await?;
    promote_seen_pairs(&mut transaction, run_id, account_id).await?;
    let removals = demote_absent_pairs(&mut transaction, run_id, account_id).await?;
    tombstone_absent_lists(&mut transaction, run_id, account_id).await?;
    sqlx::query("delete from github_catalog.list_snapshot_items where sync_run_id = $1")
        .bind(run_id)
        .execute(&mut *transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    let row: (i32, i32) = sqlx::query_as(
        "update github_catalog.sync_runs
         set status = 'completed', finished_at = now(), additions = $2, removals = $3
         where sync_run_id = $1
         returning pages_processed, items_observed",
    )
    .bind(run_id)
    .bind(i32::try_from(additions).unwrap_or(i32::MAX))
    .bind(i32::try_from(removals).unwrap_or(i32::MAX))
    .fetch_one(&mut *transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;

    transaction
        .commit()
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    Ok((
        u32::try_from(row.0).unwrap_or_default(),
        u32::try_from(row.1).unwrap_or_default(),
        additions,
        removals,
    ))
}

/// Upserts list identity from the staged enumeration: new lists appear,
/// renames propagate, and a previously removed list seen again is
/// reactivated with its removal evidence cleared.
async fn upsert_lists_from_staging(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.star_lists (list_id, account_id, provider_list_id, name)
         select gen_random_uuid(), $2, si.provider_list_id, max(si.list_name)
         from github_catalog.list_snapshot_items si
         where si.sync_run_id = $1
         group by si.provider_list_id
         on conflict (account_id, provider_list_id) do update set
             name = excluded.name,
             status = 'active',
             observed_removed_at = null,
             evidence_run_id = null",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Counts distinct staged pairs that were not members before this snapshot;
/// captured before promotion flips the very state it describes.
async fn count_member_additions(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<u32, crate::database::PersistenceError> {
    let additions: i64 = sqlx::query_scalar(
        "select count(*) from (
             select distinct l.list_id, r.repository_id
             from github_catalog.list_snapshot_items si
             join github_catalog.repositories r
                 on r.provider_repository_id = si.provider_repository_id
             join github_catalog.star_lists l
                 on l.account_id = $2 and l.provider_list_id = si.provider_list_id
             where si.sync_run_id = $1 and not exists (
                 select 1 from github_catalog.star_list_memberships m
                 where m.list_id = l.list_id
                   and m.repository_id = r.repository_id
                   and m.member)
         ) added",
    )
    .bind(run_id)
    .bind(account_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(u32::try_from(additions).unwrap_or(u32::MAX))
}

/// Appends one member observation fact per seen membership.
async fn record_membership_observations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.star_list_membership_observations
             (observation_id, list_id, repository_id, member, observed_at,
              evidence_run_id)
         select gen_random_uuid(), l.list_id, r.repository_id, true, now(), $1
         from github_catalog.list_snapshot_items si
         join github_catalog.repositories r
             on r.provider_repository_id = si.provider_repository_id
         join github_catalog.star_lists l
             on l.provider_list_id = si.provider_list_id
         where si.sync_run_id = $1",
    )
    .bind(run_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// The authority swap itself: seen pairs become members, clearing stale
/// removal evidence; continuing members keep no invented timestamps because
/// the provider supplies none.
async fn promote_seen_pairs(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "insert into github_catalog.star_list_memberships
             (list_id, repository_id, member, last_observed_at, evidence_run_id)
         select l.list_id, r.repository_id, true, now(), $1
         from github_catalog.list_snapshot_items si
         join github_catalog.repositories r
             on r.provider_repository_id = si.provider_repository_id
         join github_catalog.star_lists l
             on l.account_id = $2 and l.provider_list_id = si.provider_list_id
         where si.sync_run_id = $1
         on conflict (list_id, repository_id) do update set
             member = true,
             last_observed_at = now(),
             observed_removed_at = null,
             evidence_run_id = excluded.evidence_run_id",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Absence from a complete enumeration is removal evidence: prior member
/// pairs absent from this snapshot become evidenced non-members plus
/// append-only observation rows - never silent deletions.
async fn demote_absent_pairs(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<u32, crate::database::PersistenceError> {
    let demoted: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "update github_catalog.star_list_memberships m
         set member = false, observed_removed_at = now(), evidence_run_id = $1
         from github_catalog.star_lists l
         where l.account_id = $2 and m.list_id = l.list_id and m.member
           and not exists (
               select 1 from github_catalog.list_snapshot_items si
               join github_catalog.repositories r
                   on r.provider_repository_id = si.provider_repository_id
               where si.sync_run_id = $1
                 and si.provider_list_id = l.provider_list_id
                 and r.repository_id = m.repository_id)
         returning m.list_id, m.repository_id",
    )
    .bind(run_id)
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    for (list_id, repository_id) in &demoted {
        sqlx::query(
            "insert into github_catalog.star_list_membership_observations
                 (observation_id, list_id, repository_id, member, observed_at,
                  evidence_run_id)
              values (gen_random_uuid(), $1, $2, false, now(), $3)",
        )
        .bind(list_id)
        .bind(repository_id)
        .bind(run_id)
        .execute(&mut **transaction)
        .await
        .map_err(crate::database::PersistenceError::Query)?;
    }
    Ok(u32::try_from(demoted.len()).unwrap_or(u32::MAX))
}

/// A list absent from a complete enumeration is removed upstream: it is
/// tombstoned with an inferred observation time and the establishing run as
/// evidence, never deleted. Its memberships were already demoted above, so
/// history stays fully explainable.
async fn tombstone_absent_lists(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    account_id: Uuid,
) -> Result<(), crate::database::PersistenceError> {
    sqlx::query(
        "update github_catalog.star_lists l
         set status = 'removed', observed_removed_at = now(), evidence_run_id = $1
         where l.account_id = $2 and l.status = 'active' and not exists (
             select 1 from github_catalog.list_snapshot_items si
             where si.sync_run_id = $1 and si.provider_list_id = l.provider_list_id)",
    )
    .bind(run_id)
    .bind(account_id)
    .execute(&mut **transaction)
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(())
}

/// Reads the account's active lists; tombstoned ones are excluded.
///
/// # Errors
///
/// Returns [`StarListsError`] when persistence fails.
pub async fn current_star_lists(
    database: &Database,
    account_id: Uuid,
) -> Result<Vec<StarListSummary>, StarListsError> {
    let lists: Vec<(Uuid, String, String)> = sqlx::query_as(
        "select list_id, provider_list_id, name from github_catalog.star_lists
         where account_id = $1 and status = 'active'
         order by provider_list_id",
    )
    .bind(account_id)
    .fetch_all(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(lists
        .into_iter()
        .map(|(list_id, provider_list_id, name)| StarListSummary {
            list_id,
            provider_list_id,
            name,
        })
        .collect())
}

/// Reads a list's current members; demoted memberships are excluded.
///
/// # Errors
///
/// Returns [`StarListsError`] when persistence fails.
pub async fn current_list_members(
    database: &Database,
    list_id: Uuid,
) -> Result<Vec<ListMember>, StarListsError> {
    let members: Vec<(Uuid,)> = sqlx::query_as(
        "select repository_id from github_catalog.star_list_memberships
         where list_id = $1 and member
         order by repository_id",
    )
    .bind(list_id)
    .fetch_all(database.pool())
    .await
    .map_err(crate::database::PersistenceError::Query)?;
    Ok(members
        .into_iter()
        .map(|(repository_id,)| ListMember { repository_id })
        .collect())
}
