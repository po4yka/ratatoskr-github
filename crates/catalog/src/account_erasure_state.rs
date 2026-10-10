//! Owner-keyed state removed by account erasure, and the garbage collection of the shared catalog
//! entries only the erased owner reached (XR-021 CONTRACTS.md S12).
//!
//! Repositories are a shared catalog. Erasure deletes what belongs to the owner, then collects a
//! repository, its children and its README bytes only when no remaining owner row references them.

use crate::PersistenceError;
use crate::backup_policy::mark_backup_policy_dirty_in_tx;

type Transaction<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

/// The one predicate that decides which owner an outbox, inbox or publication row belongs to.
///
/// The real producers write three shapes and no other: a contract payload with a top-level
/// `owner` (`RepositoryAnalysisRequested`, `Completed`, `Failed` and the publication copy), a
/// complete envelope with a `tenant_id`, and a stored `github.sync.requested.v1` command envelope
/// whose payload names the `account`. Rows with none of them (the Vault policy command and its
/// acknowledgement) are catalog-wide and untouched. `$1` is the owner reference.
pub(crate) const OWNER_KEYED_PAYLOAD: &str = "(payload ->> 'owner' = $1 \
     or payload ->> 'tenant_id' = $1 \
     or payload #>> '{payload,account}' = $1)";

/// Tables with a foreign key into `repositories` whose rows keep a repository alive.
pub(crate) const OWNER_REFERENCES: &[&str] = &[
    "current_star_state",
    "star_observations",
    "legacy_list_claims",
    "legacy_import_repository_records",
    "star_list_memberships",
    "star_list_membership_observations",
    "mutation_audit",
    "reconciliation_repairs",
    "repository_watches",
    "repository_analysis_requests",
    "repository_analysis_links",
];

/// Tables with a foreign key into `repositories` that hang off a repository and go with it.
pub(crate) const CATALOG_CHILDREN: &[&str] = &[
    "repository_aliases",
    "repository_metadata",
    "repository_metadata_revisions",
    "repository_analysis_publications",
    "backup_policies",
];

const OWNER_ACCOUNTS: &str =
    "select account_id from github_catalog.github_accounts where owner_ref = $1";
const OWNER_SYNC_RUNS: &str = "select sync_run_id from github_catalog.sync_runs \
     where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)";
const OWNER_LISTS: &str = "select list_id from github_catalog.star_lists \
     where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)";

const README_IN_METADATA: &str = "readme_revision #>> '{content_ref,digest,hex}'";
const README_IN_PUBLICATION: &str = "payload #>> '{source_revision,readme,content_ref,digest,hex}'";
const README_IN_REQUEST: &str = "source_revision #>> '{readme,content_ref,digest,hex}'";

/// Removes everything keyed to `owner_ref` and collects what only that owner reached, inside the
/// caller's transaction.
pub(crate) async fn erase_owner_state(
    transaction: &mut Transaction<'_>,
    owner_ref: &str,
) -> Result<(), PersistenceError> {
    capture_candidates(transaction, owner_ref).await?;
    delete_owner_rows(transaction, owner_ref).await?;
    let collected = collect_unreferenced_repositories(transaction).await?;
    collect_unreferenced_readme_bytes(transaction).await?;
    if collected > 0 {
        mark_backup_policy_dirty_in_tx(transaction).await?;
    }
    Ok(())
}

async fn execute(
    transaction: &mut Transaction<'_>,
    sql: &str,
    owner_ref: Option<&str>,
) -> Result<u64, PersistenceError> {
    let query = sqlx::query(sql);
    let query = match owner_ref {
        Some(owner_ref) => query.bind(owner_ref),
        None => query,
    };
    Ok(query
        .execute(&mut **transaction)
        .await
        .map_err(PersistenceError::Query)?
        .rows_affected())
}

/// Records, before any owner row is deleted, every repository the owner reached and every README
/// digest those rows name. Both temporary tables disappear with the transaction.
async fn capture_candidates(
    transaction: &mut Transaction<'_>,
    owner_ref: &str,
) -> Result<(), PersistenceError> {
    execute(
        transaction,
        "create temporary table erasure_candidates (repository_id uuid primary key) on commit drop",
        None,
    )
    .await?;
    let reached = [
        format!("select repository_id from github_catalog.current_star_state where account_id in ({OWNER_ACCOUNTS})"),
        format!("select repository_id from github_catalog.star_observations where account_id in ({OWNER_ACCOUNTS})"),
        format!("select repository_id from github_catalog.legacy_list_claims where account_id in ({OWNER_ACCOUNTS})"),
        format!("select repository_id from github_catalog.legacy_import_repository_records where account_id in ({OWNER_ACCOUNTS})"),
        format!("select repository_id from github_catalog.star_list_memberships where list_id in ({OWNER_LISTS})"),
        format!("select repository_id from github_catalog.star_list_membership_observations where list_id in ({OWNER_LISTS})"),
        format!("select repository_id from github_catalog.mutation_audit where account_id in ({OWNER_ACCOUNTS})"),
        format!("select repository_id from github_catalog.reconciliation_repairs where sync_run_id in ({OWNER_SYNC_RUNS})"),
        "select repository_id from github_catalog.repository_watches where owner_ref = $1".to_owned(),
        "select repository_id from github_catalog.repository_analysis_requests where owner_ref = $1".to_owned(),
        "select repository_id from github_catalog.repository_analysis_links where owner_ref = $1".to_owned(),
        format!("select repository_id from github_catalog.repository_analysis_publications where {OWNER_KEYED_PAYLOAD}"),
        "select repository.repository_id from github_catalog.repositories repository \
         join github_catalog.repository_action_attempts attempt \
           on attempt.github_repository_numeric_id = repository.provider_repository_id \
         where attempt.owner_ref = $1"
            .to_owned(),
    ];
    for select in reached {
        execute(
            transaction,
            &format!(
                "insert into erasure_candidates (repository_id) {select} on conflict do nothing"
            ),
            Some(owner_ref),
        )
        .await?;
    }
    execute(
        transaction,
        "create temporary table erasure_readme_digests (digest text primary key) on commit drop",
        None,
    )
    .await?;
    // A superset of what will be deleted: a digest that a surviving row still names is kept by the
    // reference check after the deletions.
    for select in [
        format!(
            "select {README_IN_METADATA} from github_catalog.repository_metadata \
             where repository_id in (select repository_id from erasure_candidates)"
        ),
        format!(
            "select {README_IN_PUBLICATION} from github_catalog.repository_analysis_publications \
             where repository_id in (select repository_id from erasure_candidates) \
                or {OWNER_KEYED_PAYLOAD}"
        ),
        format!(
            "select {README_IN_REQUEST} from github_catalog.repository_analysis_requests \
             where owner_ref = $1"
        ),
    ] {
        execute(
            transaction,
            &format!(
                "insert into erasure_readme_digests (digest) \
                 select digest from ({select}) as named(digest) where digest is not null \
                 on conflict do nothing"
            ),
            Some(owner_ref),
        )
        .await?;
    }
    Ok(())
}

/// Deletes the owner's rows in foreign-key order.
async fn delete_owner_rows(
    transaction: &mut Transaction<'_>,
    owner_ref: &str,
) -> Result<(), PersistenceError> {
    let statements = [
        format!("delete from github_catalog.outbox_events where {OWNER_KEYED_PAYLOAD}"),
        format!("delete from github_catalog.inbox_events where {OWNER_KEYED_PAYLOAD}"),
        format!(
            "delete from github_catalog.repository_analysis_publications where {OWNER_KEYED_PAYLOAD}"
        ),
        "delete from github_catalog.repository_action_attempts where owner_ref = $1".to_owned(),
        "delete from github_catalog.repository_analysis_links where owner_ref = $1".to_owned(),
        "delete from github_catalog.repository_analysis_requests where owner_ref = $1".to_owned(),
        "delete from github_catalog.repository_watches where owner_ref = $1".to_owned(),
        format!("delete from github_catalog.mutation_audit where account_id in ({OWNER_ACCOUNTS})"),
        format!(
            "delete from github_catalog.star_list_membership_observations where list_id in ({OWNER_LISTS})"
        ),
        format!(
            "delete from github_catalog.star_list_memberships where list_id in ({OWNER_LISTS})"
        ),
        format!("delete from github_catalog.star_lists where account_id in ({OWNER_ACCOUNTS})"),
        format!(
            "delete from github_catalog.current_star_state where account_id in ({OWNER_ACCOUNTS})"
        ),
        format!(
            "delete from github_catalog.star_observations where account_id in ({OWNER_ACCOUNTS})"
        ),
        format!(
            "delete from github_catalog.legacy_list_claims where account_id in ({OWNER_ACCOUNTS})"
        ),
        format!(
            "delete from github_catalog.star_watermarks where account_id in ({OWNER_ACCOUNTS})"
        ),
        format!(
            "delete from github_catalog.reconciliation_repairs where sync_run_id in ({OWNER_SYNC_RUNS})"
        ),
        format!(
            "delete from github_catalog.snapshot_items where sync_run_id in ({OWNER_SYNC_RUNS})"
        ),
        format!(
            "delete from github_catalog.list_snapshot_items where sync_run_id in ({OWNER_SYNC_RUNS})"
        ),
        format!(
            "delete from github_catalog.sync_checkpoints where sync_run_id in ({OWNER_SYNC_RUNS})"
        ),
        format!("delete from github_catalog.sync_runs where account_id in ({OWNER_ACCOUNTS})"),
        format!(
            "delete from github_catalog.legacy_import_repository_records where account_id in ({OWNER_ACCOUNTS})"
        ),
        format!(
            "delete from github_catalog.legacy_import_accounts where account_id in ({OWNER_ACCOUNTS})"
        ),
        format!(
            "delete from github_catalog.github_account_credentials where account_id in ({OWNER_ACCOUNTS})"
        ),
        "delete from github_catalog.github_accounts where owner_ref = $1".to_owned(),
    ];
    for statement in statements {
        execute(transaction, &statement, Some(owner_ref)).await?;
    }
    Ok(())
}

/// Deletes each candidate that no remaining owner row references, with its catalog children, and
/// returns how many repositories were removed.
async fn collect_unreferenced_repositories(
    transaction: &mut Transaction<'_>,
) -> Result<u64, PersistenceError> {
    // Besides the foreign-keyed owner tables, another owner's action attempt (keyed by the numeric
    // provider id) or analysis publication (a catalog child that carries its owner in the
    // payload) also keeps the repository alive; the erased owner's own were deleted already.
    let mut conditions: Vec<String> = OWNER_REFERENCES
        .iter()
        .map(|table| {
            format!(
                "not exists (select 1 from github_catalog.{table} reference \
                 where reference.repository_id = candidate.repository_id)"
            )
        })
        .collect();
    conditions.push(
        "not exists (select 1 from github_catalog.repository_action_attempts attempt \
         join github_catalog.repositories repository \
           on repository.provider_repository_id = attempt.github_repository_numeric_id \
         where repository.repository_id = candidate.repository_id)"
            .to_owned(),
    );
    conditions.push(
        "not exists (select 1 from github_catalog.repository_analysis_publications publication \
         where publication.repository_id = candidate.repository_id)"
            .to_owned(),
    );
    let unreferenced = conditions.join(" and ");
    execute(
        transaction,
        &format!(
            "create temporary table erasure_doomed on commit drop as \
             select candidate.repository_id from erasure_candidates candidate where {unreferenced}"
        ),
        None,
    )
    .await?;
    // A redirect from a surviving alias to one being deleted would block the delete.
    execute(
        transaction,
        "update github_catalog.repository_aliases survivor set redirect_to = null \
         where survivor.repository_id not in (select repository_id from erasure_doomed) \
           and survivor.redirect_to in (select alias_id from github_catalog.repository_aliases \
                                         where repository_id in (select repository_id from erasure_doomed))",
        None,
    )
    .await?;
    for table in CATALOG_CHILDREN {
        execute(
            transaction,
            &format!(
                "delete from github_catalog.{table} \
                 where repository_id in (select repository_id from erasure_doomed)"
            ),
            None,
        )
        .await?;
    }
    execute(
        transaction,
        "delete from github_catalog.repositories \
         where repository_id in (select repository_id from erasure_doomed)",
        None,
    )
    .await
}

/// Deletes README bytes whose digest was named only by rows that are now gone.
async fn collect_unreferenced_readme_bytes(
    transaction: &mut Transaction<'_>,
) -> Result<(), PersistenceError> {
    execute(
        transaction,
        &format!(
            "delete from github_catalog.repository_readme_blobs blob \
             where blob.content_digest in (select digest from erasure_readme_digests) \
               and not exists (select 1 from github_catalog.repository_metadata \
                                where {README_IN_METADATA} = blob.content_digest) \
               and not exists (select 1 from github_catalog.repository_analysis_publications \
                                where {README_IN_PUBLICATION} = blob.content_digest) \
               and not exists (select 1 from github_catalog.repository_analysis_requests \
                                where {README_IN_REQUEST} = blob.content_digest)"
        ),
        None,
    )
    .await?;
    Ok(())
}
