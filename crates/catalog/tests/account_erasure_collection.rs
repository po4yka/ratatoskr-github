//! Erasure collects repositories, README bytes and tracked-mode residue that only the erased
//! owner reached (XR-021 CONTRACTS.md S12).

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_operation_contracts::AccountErasureOutcome;
use serde_json::json;
use uuid::Uuid;

mod erasure_support;

use erasure_support::{Owner, TestResult, attempt, count, erase, owner, repository, requested};

const README_ONLY_R1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const README_SHARED: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const README_R3: &str = "4444444444444444444444444444444444444444444444444444444444444444";
const OPAQUE_DIGEST: &str = "5555555555555555555555555555555555555555555555555555555555555555";

async fn readme_blob(database: &TestDatabase, digest: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into github_catalog.repository_readme_blobs (content_digest, bytes, length_bytes)
         values ($1, 'x'::bytea, 1)",
    )
    .bind(digest)
    .execute(database.database.pool())
    .await?;
    Ok(())
}

/// Gives a repository an alias, current metadata that references one README digest, a metadata
/// revision and a backup policy.
async fn catalog_entry(
    database: &TestDatabase,
    repository_id: Uuid,
    name: &str,
    readme_digest: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into github_catalog.repository_aliases (alias_id, repository_id, alias_kind, alias_value)
         values ($1, $2, 'owner_name', $3)",
    )
    .bind(Uuid::now_v7())
    .bind(repository_id)
    .bind(name)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.repository_metadata
             (repository_id, stargazers_count, readme_revision, content_hash, fetched_at)
         values ($1, 1, $2, 'hash', now())",
    )
    .bind(repository_id)
    .bind(json!({"state": "present", "content_ref": {"digest": {"algorithm": "sha256", "hex": readme_digest}}}))
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.repository_metadata_revisions
             (revision_id, repository_id, payload, content_hash, observed_at)
         values ($1, $2, '{}'::jsonb, 'hash', now())",
    )
    .bind(Uuid::now_v7())
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.backup_policies (backup_policy_id, repository_id, policy_level)
         values ($1, $2, 'git_mirror')",
    )
    .bind(Uuid::now_v7())
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    Ok(())
}

async fn star(
    database: &TestDatabase,
    owner: &Owner,
    repository_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, now(), now())",
    )
    .bind(owner.account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.star_observations
             (observation_id, account_id, repository_id, starred, provider_starred_at, observed_at)
         values ($1, $2, $3, true, now(), now())",
    )
    .bind(Uuid::now_v7())
    .bind(owner.account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    Ok(())
}

async fn watch_and_request(
    database: &TestDatabase,
    owner: &Owner,
    repository_id: Uuid,
    numeric_id: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let watch_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.repository_watches
             (watch_id, owner_ref, repository_id, trigger_type, downstream_action, last_evaluated_content_hash)
         values ($1, $2, $3, 'metadata_changed', 'repository_analysis', 'hash')",
    )
    .bind(watch_id)
    .bind(&owner.reference)
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.repository_analysis_requests
             (request_id, watch_id, owner_ref, repository_id, github_repository_numeric_id,
              source_revision, repository_attributes, request_payload, attributes_digest_hex,
              idempotency_digest_hex, requested_contract, not_before)
         values ($1, $2, $3, $4, $5, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, $6, $6,
                 'repository_analysis', now())",
    )
    .bind(Uuid::now_v7())
    .bind(watch_id)
    .bind(&owner.reference)
    .bind(repository_id)
    .bind(numeric_id)
    .bind(OPAQUE_DIGEST)
    .execute(database.database.pool())
    .await?;
    Ok(())
}

async fn publication(
    database: &TestDatabase,
    owner: &Owner,
    repository_id: Uuid,
    numeric_id: u64,
    readme_digest: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut payload = serde_json::to_value(requested(owner, repository_id, numeric_id)?)?;
    payload
        .pointer_mut("/source_revision")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("the request has a source revision")?
        .insert(
            "readme".to_owned(),
            json!({
                "state": "present",
                "content_ref": {"digest": {"algorithm": "sha256", "hex": readme_digest}}
            }),
        );
    sqlx::query(
        "insert into github_catalog.repository_analysis_publications
             (repository_id, source_digest, message_id, payload)
         values ($1, $2, $3, $4)",
    )
    .bind(repository_id)
    .bind(OPAQUE_DIGEST)
    .bind(Uuid::now_v7())
    .bind(payload)
    .execute(database.database.pool())
    .await?;
    Ok(())
}

async fn present(
    database: &TestDatabase,
    sql: &str,
    repository_id: Uuid,
) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(sql)
        .bind(repository_id)
        .fetch_one(database.database.pool())
        .await?)
}

#[tokio::test]
async fn repositories_only_the_erased_owner_referenced_are_collected_with_readme_bytes()
-> TestResult {
    let database = TestDatabase::create().await?;
    let erased = owner(&database).await?;
    let other = owner(&database).await?;
    let r1 = repository(&database, 952_001).await?;
    let r2 = repository(&database, 952_002).await?;
    let r3 = repository(&database, 952_003).await?;
    for digest in [README_ONLY_R1, README_SHARED, README_R3] {
        readme_blob(&database, digest).await?;
    }
    catalog_entry(&database, r1, "acme/only-erased", README_ONLY_R1).await?;
    catalog_entry(&database, r2, "acme/shared", README_SHARED).await?;
    catalog_entry(&database, r3, "acme/untouched", README_R3).await?;
    // R1 is reached by the erased owner through every kind of reference.
    star(&database, &erased, r1).await?;
    watch_and_request(&database, &erased, r1, 952_001).await?;
    attempt(&database, &erased, 952_001, "track").await?;
    // The publication names a digest that R2's metadata also references.
    publication(&database, &erased, r1, 952_001, README_SHARED).await?;
    // R2 is starred by both owners; R3 is never touched by the erased owner.
    star(&database, &erased, r2).await?;
    star(&database, &other, r2).await?;

    let outcome = erase(&database, &erased).await?;

    assert_eq!(outcome, AccountErasureOutcome::Verified);
    for (label, sql) in [
        (
            "repository",
            "select count(*) from github_catalog.repositories where repository_id = $1",
        ),
        (
            "alias",
            "select count(*) from github_catalog.repository_aliases where repository_id = $1",
        ),
        (
            "metadata",
            "select count(*) from github_catalog.repository_metadata where repository_id = $1",
        ),
        (
            "revisions",
            "select count(*) from github_catalog.repository_metadata_revisions where repository_id = $1",
        ),
        (
            "publications",
            "select count(*) from github_catalog.repository_analysis_publications where repository_id = $1",
        ),
        (
            "backup policy",
            "select count(*) from github_catalog.backup_policies where repository_id = $1",
        ),
    ] {
        assert_eq!(
            present(&database, sql, r1).await?,
            0,
            "R1 {label} must be collected"
        );
        let expected = i64::from(label != "publications");
        assert_eq!(
            present(&database, sql, r2).await?,
            expected,
            "R2 {label} must remain"
        );
        assert_eq!(
            present(&database, sql, r3).await?,
            expected,
            "R3 {label} must remain"
        );
    }
    let remaining_readmes: Vec<String> = sqlx::query_scalar(
        "select content_digest from github_catalog.repository_readme_blobs order by content_digest",
    )
    .fetch_all(database.database.pool())
    .await?;
    assert_eq!(
        remaining_readmes,
        vec![README_SHARED.to_owned(), README_R3.to_owned()],
        "bytes only R1 referenced are gone; bytes R2 (also named by R1's publication) and R3 \
         reference remain"
    );
    let other_stars = count(
        &database,
        "select count(*) from github_catalog.current_star_state",
    )
    .await?;
    assert_eq!(other_stars, 1, "the other owner's star remains");
    let dirty: (i64, i64) = sqlx::query_as(
        "select dirty_generation, published_generation
         from github_catalog.backup_policy_publication_cursor where scope = 'catalog'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        dirty.0 > dirty.1,
        "collecting a repository must dirty the backup policy"
    );
    database.cleanup().await?;
    Ok(())
}
