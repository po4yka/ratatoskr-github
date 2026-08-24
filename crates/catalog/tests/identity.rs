//! Repository identity and alias behavior against a disposable catalog database.

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AliasKind, apply_alias_observation, record_alias, resolve_alias, upsert_repository,
};

async fn active_owner_name_aliases(
    database: &TestDatabase,
    value: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repository_aliases
         where alias_kind = 'owner_name' and alias_value = $1 and status = 'active'",
    )
    .bind(value)
    .fetch_one(database.database.pool())
    .await?;
    Ok(count)
}

async fn count_repositories(
    database: &TestDatabase,
    provider_repository_id: i64,
) -> Result<i64, Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repositories
         where provider_repository_id = $1",
    )
    .bind(provider_repository_id)
    .fetch_one(database.database.pool())
    .await?;
    Ok(count)
}

#[tokio::test]
async fn upsert_repository_creates_one_record_per_provider_id()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;

    let identity = upsert_repository(&database.database, 3_000_000_001).await?;
    assert_ne!(
        identity.repository_id,
        uuid::Uuid::nil(),
        "a created repository carries a fresh internal identifier"
    );
    let stored: (uuid::Uuid, i64) = sqlx::query_as(
        "select repository_id, provider_repository_id from github_catalog.repositories",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        stored.0, identity.repository_id,
        "the row stores the internal id"
    );
    assert_eq!(stored.1, 3_000_000_001);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn upsert_repository_reuses_identity_for_known_provider_id()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;

    let first = upsert_repository(&database.database, 3_000_000_002).await?;
    let second = upsert_repository(&database.database, 3_000_000_002).await?;
    assert_eq!(
        first.repository_id, second.repository_id,
        "a known provider ID must resolve to the identical internal identity"
    );
    assert_eq!(count_repositories(&database, 3_000_000_002).await?, 1);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn resolve_alias_finds_recorded_owner_name_and_unknown_resolves_to_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let identity = upsert_repository(&database.database, 3_000_000_003).await?;

    record_alias(
        &database.database,
        identity.repository_id,
        AliasKind::OwnerName,
        "acme/widgets",
    )
    .await?;

    let resolved = resolve_alias(&database.database, AliasKind::OwnerName, "acme/widgets")
        .await?
        .expect("the recorded owner/name alias must resolve");
    assert_eq!(resolved, identity.repository_id);

    let unknown =
        resolve_alias(&database.database, AliasKind::OwnerName, "acme/never-seen").await?;
    assert!(
        unknown.is_none(),
        "an unrecorded alias must resolve to no repository"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn rename_evidence_redirects_old_alias_to_same_repository()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let seeded = upsert_repository(&database.database, 3_000_000_004).await?;
    record_alias(
        &database.database,
        seeded.repository_id,
        AliasKind::OwnerName,
        "acme/widgets",
    )
    .await?;

    let renamed = apply_alias_observation(
        &database.database,
        3_000_000_004,
        AliasKind::OwnerName,
        Some("acme/widgets"),
        "acme/gadgets",
    )
    .await?;

    assert_eq!(
        renamed.repository_id, seeded.repository_id,
        "a rename must not create a new logical repository"
    );
    let via_new = resolve_alias(&database.database, AliasKind::OwnerName, "acme/gadgets")
        .await?
        .expect("the new owner/name must be the live alias");
    assert_eq!(via_new, seeded.repository_id);
    let via_old = resolve_alias(&database.database, AliasKind::OwnerName, "acme/widgets")
        .await?
        .expect("the superseded owner/name must still redirect to the repository");
    assert_eq!(via_old, seeded.repository_id);
    assert_eq!(
        active_owner_name_aliases(&database, "acme/widgets").await?,
        0,
        "the superseded alias must no longer be live"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn transfer_keeps_single_identity_across_owners() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let seeded = upsert_repository(&database.database, 3_000_000_005).await?;
    record_alias(
        &database.database,
        seeded.repository_id,
        AliasKind::OwnerName,
        "legacy-corp/tools",
    )
    .await?;

    let transferred = apply_alias_observation(
        &database.database,
        3_000_000_005,
        AliasKind::OwnerName,
        Some("legacy-corp/tools"),
        "acme/tools",
    )
    .await?;

    assert_eq!(
        transferred.repository_id, seeded.repository_id,
        "a transfer keeps the single logical identity behind the provider ID"
    );
    assert_eq!(count_repositories(&database, 3_000_000_005).await?, 1);
    let via_new = resolve_alias(&database.database, AliasKind::OwnerName, "acme/tools")
        .await?
        .expect("the post-transfer owner/name resolves");
    let via_old = resolve_alias(
        &database.database,
        AliasKind::OwnerName,
        "legacy-corp/tools",
    )
    .await?
    .expect("the pre-transfer owner/name still redirects");
    assert_eq!(via_new, seeded.repository_id);
    assert_eq!(via_old, seeded.repository_id);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn live_owner_name_is_globally_unique() -> Result<(), Box<dyn std::error::Error>> {
    use ratatoskr_github_catalog::IdentityError;

    let database = TestDatabase::create().await?;
    let holder = upsert_repository(&database.database, 3_000_000_006).await?;
    record_alias(
        &database.database,
        holder.repository_id,
        AliasKind::OwnerName,
        "acme/widgets",
    )
    .await?;

    let challenger = upsert_repository(&database.database, 3_000_000_007).await?;
    let conflict = record_alias(
        &database.database,
        challenger.repository_id,
        AliasKind::OwnerName,
        "acme/widgets",
    )
    .await;
    assert!(
        matches!(conflict, Err(IdentityError::LiveAliasConflict)),
        "claiming another repository's live alias must be a typed conflict, got {conflict:?}"
    );
    assert_eq!(
        resolve_alias(&database.database, AliasKind::OwnerName, "acme/widgets")
            .await?
            .expect("the original live alias must keep resolving"),
        holder.repository_id,
        "the rejected claim must not change alias ownership"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn released_name_may_be_claimed_while_history_still_redirects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let original = upsert_repository(&database.database, 3_000_000_008).await?;
    record_alias(
        &database.database,
        original.repository_id,
        AliasKind::OwnerName,
        "acme/shared",
    )
    .await?;
    apply_alias_observation(
        &database.database,
        3_000_000_008,
        AliasKind::OwnerName,
        Some("acme/shared"),
        "acme/moved",
    )
    .await?;

    let newcomer = upsert_repository(&database.database, 3_000_000_009).await?;
    record_alias(
        &database.database,
        newcomer.repository_id,
        AliasKind::OwnerName,
        "acme/shared",
    )
    .await?;

    let live_holder = resolve_alias(&database.database, AliasKind::OwnerName, "acme/shared")
        .await?
        .expect("the claimed name resolves to its new holder");
    assert_eq!(
        live_holder, newcomer.repository_id,
        "the newly recorded alias is live and preferred"
    );
    let redirected = resolve_alias(&database.database, AliasKind::OwnerName, "acme/moved")
        .await?
        .expect("the original repository still resolves through its history");
    assert_eq!(redirected, original.repository_id);

    database.cleanup().await?;
    Ok(())
}
