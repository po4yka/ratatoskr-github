//! Legacy shadow-report behavior tests.

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    generate_legacy_shadow_report, legacy_cutover_readiness, upsert_repository,
};
use uuid::Uuid;

#[tokio::test]
async fn shadow_report_classifies_repository_star_and_list_differences()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = Uuid::now_v7();
    let repository = upsert_repository(&database.database, 99).await?;
    sqlx::query(
        "insert into github_catalog.github_accounts
             (account_id, owner_ref, status, provider_user_id)
         values ($1, 'user:018f0000-0000-7000-8000-000000000001',
                 'reauthorization_required', 4242)",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.legacy_import_accounts
             (source_id, legacy_user_id, account_id)
         values ('archive-2026-08', 101, $1)",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.legacy_import_repository_records
             (source_id, legacy_repository_id, account_id, repository_id, starred)
         values ('archive-2026-08', 7, $1, $2, true)",
    )
    .bind(account_id)
    .bind(repository.repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at,
              provider_starred_at_unknown, last_observed_at)
         values ($1, $2, true, null, true, now())",
    )
    .bind(account_id)
    .bind(repository.repository_id)
    .execute(database.database.pool())
    .await?;
    insert_legacy_list_claim(&database.database, account_id, repository.repository_id).await?;

    let report = generate_legacy_shadow_report(&database.database, "archive-2026-08").await?;

    assert_eq!(report.accounts_reauthorization_required, 1);
    assert_eq!(report.provider_star_times_unknown, 1);
    assert_eq!(report.list_claims_missing_from_provider, 1);
    assert!(!report.cutover_reviewable);
    assert!(!report.canonical_json()?.contains("encrypted_token"));
    assert!(!report.concise_text().contains("encrypted_token"));
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn clean_post_reconnect_full_snapshot_is_cutover_reviewable()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = Uuid::now_v7();
    let repository = upsert_repository(&database.database, 99).await?;
    sqlx::query(
        "insert into github_catalog.github_accounts
             (account_id, owner_ref, status, provider_user_id)
         values ($1, 'user:018f0000-0000-7000-8000-000000000001', 'connected', 4242)",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.github_account_credentials
             (account_id, key_version, encrypted_token, nonce)
         values ($1, 'test-key', decode(repeat('ff', 17), 'hex'), decode(repeat('00', 12), 'hex'))",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.legacy_import_accounts
             (source_id, legacy_user_id, account_id)
         values ('archive-2026-08', 101, $1)",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.legacy_import_repository_records
             (source_id, legacy_repository_id, account_id, repository_id, starred)
         values ('archive-2026-08', 7, $1, $2, true)",
    )
    .bind(account_id)
    .bind(repository.repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at,
              provider_starred_at_unknown, last_observed_at)
         values ($1, $2, true, '2026-08-20T00:00:00Z', false, now())",
    )
    .bind(account_id)
    .bind(repository.repository_id)
    .execute(database.database.pool())
    .await?;
    for mode in ["full", "star_lists"] {
        sqlx::query(
            "insert into github_catalog.sync_runs
                 (sync_run_id, account_id, mode, status, finished_at)
             values ($1, $2, $3, 'completed', now())",
        )
        .bind(Uuid::now_v7())
        .bind(account_id)
        .bind(mode)
        .execute(database.database.pool())
        .await?;
    }
    let list_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.star_lists (list_id, account_id, provider_list_id, name)
         values ($1, $2, 'gid://list/archived', 'Archived List')",
    )
    .bind(list_id)
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.star_list_memberships
             (list_id, repository_id, member, last_observed_at)
         values ($1, $2, true, now())",
    )
    .bind(list_id)
    .bind(repository.repository_id)
    .execute(database.database.pool())
    .await?;
    insert_legacy_list_claim(&database.database, account_id, repository.repository_id).await?;

    let report = generate_legacy_shadow_report(&database.database, "archive-2026-08").await?;

    assert!(report.cutover_reviewable);
    assert_eq!(report.provider_star_times_unknown, 0);
    assert_eq!(report.full_snapshots_missing, 0);
    assert_eq!(report.list_snapshots_missing, 0);
    let persisted_digest: String = sqlx::query_scalar(
        "select report_digest from github_catalog.legacy_shadow_reports where report_id = $1",
    )
    .bind(report.report_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(persisted_digest, report.report_digest);
    let readiness = legacy_cutover_readiness(&database.database, "archive-2026-08").await?;
    assert_eq!(readiness.report_id, report.report_id);
    assert_eq!(readiness.report_digest, report.report_digest);
    database.cleanup().await?;
    Ok(())
}

async fn insert_legacy_list_claim(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    repository_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into github_catalog.legacy_list_claims
             (source_id, legacy_repository_id, account_id, repository_id, list_name, observed_at)
         values ('archive-2026-08', 7, $1, $2, 'Archived List', now())",
    )
    .bind(account_id)
    .bind(repository_id)
    .execute(database.pool())
    .await?;
    Ok(())
}
