//! Current GitHub Catalog schema integration behavior.

use ratatoskr_github_catalog::test_support::TestDatabase;
use sqlx::Row as _;
use uuid::Uuid;

/// A demoted list membership and a removed list each carry their removal
/// evidence; neither state is representable without it.
async fn assert_list_authority_carries_removal_evidence(
    database: &TestDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    let list_owner = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'list-holder', 'connected')",
    )
    .bind(list_owner)
    .execute(database.database.pool())
    .await?;
    let member_repo =
        ratatoskr_github_catalog::upsert_repository(&database.database, 990_201_i64).await?;
    let list_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.star_lists (list_id, account_id, provider_list_id, name)
         values ($1, $2, 'gid://list/one', 'reading list')",
    )
    .bind(list_id)
    .bind(list_owner)
    .execute(database.database.pool())
    .await?;
    let demotion_without_evidence = sqlx::query(
        "insert into github_catalog.star_list_memberships
             (list_id, repository_id, member, last_observed_at)
         values ($1, $2, false, now())",
    )
    .bind(list_id)
    .bind(member_repo.repository_id)
    .execute(database.database.pool())
    .await;
    assert!(
        demotion_without_evidence.is_err(),
        "a non-member projection must record observed_removed_at"
    );
    let tombstone_without_evidence = sqlx::query(
        "insert into github_catalog.star_lists
             (list_id, account_id, provider_list_id, name, status)
         values ($1, $2, 'gid://list/two', 'gone list', 'removed')",
    )
    .bind(Uuid::now_v7())
    .bind(list_owner)
    .execute(database.database.pool())
    .await;
    assert!(
        tombstone_without_evidence.is_err(),
        "a removed list must record observed_removed_at"
    );
    Ok(())
}

#[tokio::test]
async fn owned_schema_applies_twice_without_cross_schema_objects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    database.database.apply_schema().await?;

    let rows = sqlx::query(
        "select table_name from information_schema.tables
         where table_schema = 'github_catalog' order by table_name",
    )
    .fetch_all(database.database.pool())
    .await?;
    let tables = rows
        .into_iter()
        .map(|row| row.try_get::<String, _>("table_name"))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        tables,
        [
            "backup_policies",
            "backup_policy_feedback",
            "backup_policy_publication_cursor",
            "backup_policy_publications",
            "current_star_state",
            "github_accounts",
            "inbox_events",
            "list_snapshot_items",
            "mutation_audit",
            "outbox_events",
            "reconciliation_repairs",
            "repositories",
            "repository_aliases",
            "repository_metadata",
            "repository_metadata_revisions",
            "repository_watches",
            "snapshot_items",
            "star_list_membership_observations",
            "star_list_memberships",
            "star_lists",
            "star_observations",
            "star_watermarks",
            "sync_checkpoints",
            "sync_runs",
        ]
    );

    let cross_schema_count: i64 = sqlx::query_scalar(
        "select count(*) from information_schema.tables
         where table_schema not in ('github_catalog', 'information_schema', 'pg_catalog')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(cross_schema_count, 0);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn placeholder_tables_carry_the_decided_identity_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;

    // Stable GitHub numeric identity: two rows cannot share one provider ID.
    let inserted = sqlx::query(
        "insert into github_catalog.repositories
             (repository_id, provider_repository_id)
         values ($1, $2)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(1_i64)
    .execute(database.database.pool())
    .await?;
    assert_eq!(inserted.rows_affected(), 1);
    let duplicate_provider_id = sqlx::query(
        "insert into github_catalog.repositories
             (repository_id, provider_repository_id)
         values ($1, $2)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(1_i64)
    .execute(database.database.pool())
    .await;
    assert!(
        duplicate_provider_id.is_err(),
        "provider repository id must be unique"
    );

    // An unstarred projection carries its removal evidence.
    let unstarred_without_evidence = sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, last_observed_at)
         values ($1, $2, false, now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::now_v7())
    .execute(database.database.pool())
    .await;
    assert!(
        unstarred_without_evidence.is_err(),
        "an unstarred state must record observed_unstarred_at"
    );

    // A starred projection carries the provider starred-at that established it.
    let starred_without_starred_at = sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, last_observed_at)
         values ($1, $2, true, now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::now_v7())
    .execute(database.database.pool())
    .await;
    assert!(
        starred_without_starred_at.is_err(),
        "a starred state must carry its establishing starred-at timestamp"
    );

    assert_list_authority_carries_removal_evidence(&database).await?;

    // One live holder per alias value: a second repository claiming an active
    // owner/name is rejected, while superseded history stays out of the way.
    let holder = uuid::Uuid::now_v7();
    let challenger = uuid::Uuid::now_v7();
    for (repository_id, provider_id) in [(holder, 990_001_i64), (challenger, 990_002_i64)] {
        sqlx::query(
            "insert into github_catalog.repositories
                 (repository_id, provider_repository_id)
             values ($1, $2)",
        )
        .bind(repository_id)
        .bind(provider_id)
        .execute(database.database.pool())
        .await?;
    }
    let first_alias = sqlx::query(
        "insert into github_catalog.repository_aliases
             (alias_id, repository_id, alias_kind, alias_value)
         values ($1, $2, 'owner_name', 'acme/widgets')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(holder)
    .execute(database.database.pool())
    .await?;
    assert_eq!(first_alias.rows_affected(), 1);
    let conflicting_live_alias = sqlx::query(
        "insert into github_catalog.repository_aliases
             (alias_id, repository_id, alias_kind, alias_value)
         values ($1, $2, 'owner_name', 'acme/widgets')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(challenger)
    .execute(database.database.pool())
    .await;
    assert!(
        conflicting_live_alias.is_err(),
        "a live owner/name alias must have exactly one holding repository"
    );

    database.cleanup().await?;
    Ok(())
}

async fn assert_granted_scopes_default_empty(
    database: &TestDatabase,
    account_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'mutator', 'connected')",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    let granted_scopes: Vec<String> = sqlx::query_scalar(
        "select granted_scopes from github_catalog.github_accounts where account_id = $1",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        granted_scopes.is_empty(),
        "an account starts with no granted scopes"
    );
    Ok(())
}

async fn assert_repository_mode_vocabulary(
    database: &TestDatabase,
    repository_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    // Unclassified is the absence of a mode; the vocabulary holds three names.
    let tracked = sqlx::query(
        "update github_catalog.repositories set mode = 'tracked'
         where repository_id = $1",
    )
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    assert_eq!(tracked.rows_affected(), 1);
    let unknown_mode = sqlx::query(
        "update github_catalog.repositories set mode = 'pinned'
         where repository_id = $1",
    )
    .bind(repository_id)
    .execute(database.database.pool())
    .await;
    assert!(
        unknown_mode.is_err(),
        "repository modes admit only auto, tracked, and ignored"
    );
    Ok(())
}

async fn assert_mutation_audit_constraints(
    database: &TestDatabase,
    account_id: Uuid,
    repository_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    // Every accepted audit row carries who, what, when, and how it ended.
    let audit_row = |operation_kind: &'static str, outcome: &'static str| {
        sqlx::query(
            "insert into github_catalog.mutation_audit
                 (audit_id, idempotency_key, account_id, repository_id,
                  operation_kind, principal, source, outcome)
             values ($1, $2, $3, $4, $5, 'telegram:42', 'telegram', $6)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(format!("{operation_kind}:{outcome}"))
        .bind(account_id)
        .bind(repository_id)
        .bind(operation_kind)
        .bind(outcome)
        .execute(database.database.pool())
    };
    assert_eq!(
        audit_row("star", "applied").await?.rows_affected(),
        1,
        "a well-formed mutation audit row is accepted"
    );
    assert!(
        audit_row("delete_repository", "applied").await.is_err(),
        "audit rows name a known operation kind"
    );
    assert!(
        audit_row("star", "pending_review").await.is_err(),
        "audit rows name a known outcome"
    );
    let bad_source = sqlx::query(
        "insert into github_catalog.mutation_audit
             (audit_id, idempotency_key, account_id, repository_id,
              operation_kind, principal, source, outcome)
         values ($1, $2, $3, $4, 'star', 'web:7', 'cli', 'applied')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind("bad-source")
    .bind(account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await;
    assert!(
        bad_source.is_err(),
        "audit rows name a known calling source"
    );
    Ok(())
}

async fn assert_successful_idempotency_keys_are_unique(
    database: &TestDatabase,
    account_id: Uuid,
    repository_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    // Only successful outcomes claim the idempotency key: a replayed success
    // collides, while a failure leaves the key free for a later retry.
    let replayed_key = sqlx::query(
        "insert into github_catalog.mutation_audit
             (audit_id, idempotency_key, account_id, repository_id,
              operation_kind, principal, source, outcome)
         values ($1, 'replayed-key', $2, $3, 'star', 'web:7', 'web', 'already_applied')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    assert_eq!(replayed_key.rows_affected(), 1);
    let duplicate_success_key = sqlx::query(
        "insert into github_catalog.mutation_audit
             (audit_id, idempotency_key, account_id, repository_id,
              operation_kind, principal, source, outcome)
         values ($1, 'replayed-key', $2, $3, 'star', 'web:7', 'web', 'applied')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await;
    assert!(
        duplicate_success_key.is_err(),
        "one successful audit record per idempotency key"
    );
    let failed_then_applied = async {
        let failed = sqlx::query(
            "insert into github_catalog.mutation_audit
                 (audit_id, idempotency_key, account_id, repository_id,
                  operation_kind, principal, source, outcome)
             values ($1, 'recovered-key', $2, $3, 'star', 'web:7', 'web', 'failed')",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(account_id)
        .bind(repository_id)
        .execute(database.database.pool())
        .await?;
        let applied = sqlx::query(
            "insert into github_catalog.mutation_audit
                 (audit_id, idempotency_key, account_id, repository_id,
                  operation_kind, principal, source, outcome)
             values ($1, 'recovered-key', $2, $3, 'star', 'web:7', 'web', 'applied')",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(account_id)
        .bind(repository_id)
        .execute(database.database.pool())
        .await?;
        Ok::<_, sqlx::Error>((failed.rows_affected(), applied.rows_affected()))
    }
    .await;
    assert!(
        matches!(failed_then_applied, Ok((1, 1))),
        "a failed attempt does not consume the idempotency key"
    );
    Ok(())
}

/// Mode vocabulary and mutation-audit rules decided for item 7 live in the
/// schema itself: the mode vocabulary cannot drift, every attempt shares one
/// append-only audit trail, and only successful outcomes own an idempotency
/// key so failed attempts never consume theirs.
#[tokio::test]
async fn mutation_audit_and_mode_columns_carry_the_decided_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = Uuid::now_v7();
    assert_granted_scopes_default_empty(&database, account_id).await?;

    let identity =
        ratatoskr_github_catalog::upsert_repository(&database.database, 990_301_i64).await?;
    assert_repository_mode_vocabulary(&database, identity.repository_id).await?;
    assert_mutation_audit_constraints(&database, account_id, identity.repository_id).await?;
    assert_successful_idempotency_keys_are_unique(&database, account_id, identity.repository_id)
        .await?;

    database.cleanup().await?;
    Ok(())
}

/// A drift repair names a known action; the (run, repository) pair admits
/// one row so repeated reconciliation cannot duplicate a recorded repair.
#[tokio::test]
async fn reconciliation_repairs_carry_named_actions_once_per_run()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'repairer', 'connected')",
    )
    .bind(uuid::Uuid::now_v7())
    .execute(database.database.pool())
    .await?;
    for (provider_id, name) in [(990_101_i64, "acme/drifted"), (990_102_i64, "acme/other")] {
        let identity =
            ratatoskr_github_catalog::upsert_repository(&database.database, provider_id).await?;
        sqlx::query(
            "insert into github_catalog.repository_aliases
                 (alias_id, repository_id, alias_kind, alias_value)
             values ($1, $2, 'owner_name', $3)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(identity.repository_id)
        .bind(name)
        .execute(database.database.pool())
        .await?;
    }
    let run_id = uuid::Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.sync_runs (sync_run_id, account_id, mode, status)
         values ($1,
                 (select account_id from github_catalog.github_accounts where owner_ref = 'repairer'),
                 'full', 'running')",
    )
    .bind(run_id)
    .execute(database.database.pool())
    .await?;
    let drifted: Uuid = sqlx::query_scalar(
        "select repository_id from github_catalog.repository_aliases where alias_value = 'acme/drifted'",
    )
    .fetch_one(database.database.pool())
    .await?;
    let other: Uuid = sqlx::query_scalar(
        "select repository_id from github_catalog.repository_aliases where alias_value = 'acme/other'",
    )
    .fetch_one(database.database.pool())
    .await?;

    let first_repair = sqlx::query(
        "insert into github_catalog.reconciliation_repairs
             (sync_run_id, repository_id, action)
         values ($1, $2, 'unstar_after_drift')",
    )
    .bind(run_id)
    .bind(drifted)
    .execute(database.database.pool())
    .await?;
    assert_eq!(first_repair.rows_affected(), 1);
    let duplicate_repair = sqlx::query(
        "insert into github_catalog.reconciliation_repairs
             (sync_run_id, repository_id, action)
         values ($1, $2, 'unstar_after_drift')",
    )
    .bind(run_id)
    .bind(drifted)
    .execute(database.database.pool())
    .await;
    assert!(
        duplicate_repair.is_err(),
        "one drifted repository admits exactly one repair per completing run"
    );
    let unknown_action = sqlx::query(
        "insert into github_catalog.reconciliation_repairs
             (sync_run_id, repository_id, action)
         values ($1, $2, 'unstar_because_full')",
    )
    .bind(run_id)
    .bind(other)
    .execute(database.database.pool())
    .await;
    assert!(
        unknown_action.is_err(),
        "a drift repair must name a known action"
    );

    database.cleanup().await?;
    Ok(())
}
