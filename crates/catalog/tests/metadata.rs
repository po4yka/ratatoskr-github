//! Metadata projection and bounded revision history against a disposable
//! catalog database.

use ratatoskr_github_catalog::provider::ProviderRepositoryBody;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{AppliedOutcome, REVISION_HISTORY_LIMIT, apply_fresh_body};
use sqlx::Row as _;
use uuid::Uuid;

fn body(provider_repository_id: i64, full_name: &str, stargazers: i64) -> ProviderRepositoryBody {
    ProviderRepositoryBody {
        provider_repository_id,
        full_name: full_name.to_owned(),
        description: Some("A synthetic widget".to_owned()),
        language: Some("Rust".to_owned()),
        stargazers,
        topics: vec!["widgets".to_owned()],
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-01T10:00:00Z".to_owned()),
    }
}

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

async fn revision_count(database: &TestDatabase, repository_id: Uuid) -> TestResult<i64> {
    Ok(sqlx::query_scalar(
        "select count(*) from github_catalog.repository_metadata_revisions
         where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?)
}

async fn projection_row(
    database: &TestDatabase,
    repository_id: Uuid,
) -> TestResult<(Option<String>, Option<String>, i64)> {
    let row = sqlx::query(
        "select language, default_branch, stargazers_count
         from github_catalog.repository_metadata where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    Ok((
        row.try_get("language")?,
        row.try_get("default_branch")?,
        row.try_get("stargazers_count")?,
    ))
}

#[tokio::test]
async fn first_metadata_observation_creates_projection_and_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_200).await?;

    let outcome = apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_200, "acme/widgets", 42),
        None,
    )
    .await?;
    assert_eq!(outcome, AppliedOutcome::Created);

    let (language, branch, stars) = projection_row(&database, repository.repository_id).await?;
    assert_eq!(language.as_deref(), Some("Rust"));
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(stars, 42);
    assert_eq!(
        revision_count(&database, repository.repository_id).await?,
        1
    );

    database.cleanup().await?;
    Ok(())
}

/// Cross-service source identity must use the published SHA-256 shape rather than `PostgreSQL`'s
/// legacy MD5 helper, so a later README reference can be bound to the same immutable revision.
#[tokio::test]
async fn fresh_metadata_uses_sha256_revision_identity() -> TestResult<()> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_299).await?;

    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_299, "acme/widgets", 42),
        None,
    )
    .await?;

    let content_hash: String = sqlx::query_scalar(
        "select content_hash from github_catalog.repository_metadata where repository_id = $1",
    )
    .bind(repository.repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        content_hash.len(),
        64,
        "the immutable source identity must be a full SHA-256 hex digest"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn not_modified_preserves_previous_metadata() -> Result<(), Box<dyn std::error::Error>> {
    use ratatoskr_github_catalog::apply_not_modified;

    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_201).await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_201, "acme/widgets", 42),
        Some(r#"W/"v1""#),
    )
    .await?;

    // Pin fetch bookkeeping into the past so the cheap refresh must move it.
    sqlx::query(
        "update github_catalog.repository_metadata set fetched_at = '2020-01-01T00:00:00Z'",
    )
    .execute(database.database.pool())
    .await?;

    apply_not_modified(&database.database, repository.repository_id).await?;

    let (language, _branch, stars) = projection_row(&database, repository.repository_id).await?;
    assert_eq!(
        language.as_deref(),
        Some("Rust"),
        "projection values must not change"
    );
    assert_eq!(stars, 42, "projection values must not change");
    assert_eq!(
        revision_count(&database, repository.repository_id).await?,
        1
    );

    let fetched_after_2020: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repository_metadata
         where repository_id = $1 and fetched_at > '2020-06-01T00:00:00Z'",
    )
    .bind(repository.repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        fetched_after_2020, 1,
        "a not-modified refresh must still advance fetch bookkeeping"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn changed_metadata_updates_projection_and_appends_one_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_202).await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_202, "acme/widgets", 42),
        None,
    )
    .await?;

    let outcome = apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_202, "acme/widgets", 43),
        None,
    )
    .await?;
    assert_eq!(outcome, AppliedOutcome::Updated);

    let (_language, _branch, stars) = projection_row(&database, repository.repository_id).await?;
    assert_eq!(stars, 43);
    assert_eq!(
        revision_count(&database, repository.repository_id).await?,
        2
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn unchanged_payload_does_not_append_revision() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_203).await?;
    apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_203, "acme/widgets", 42),
        None,
    )
    .await?;

    let outcome = apply_fresh_body(
        &database.database,
        repository.repository_id,
        &body(300_000_203, "acme/widgets", 42),
        Some(r#"W/"v2""#),
    )
    .await?;
    assert_eq!(
        outcome,
        AppliedOutcome::Unchanged,
        "an identical body must be recognized regardless of validator churn"
    );
    assert_eq!(
        revision_count(&database, repository.repository_id).await?,
        1
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn revision_history_is_bounded_to_ten_most_recent() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::create().await?;
    let repository =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_204).await?;

    for stars in 0..11 {
        apply_fresh_body(
            &database.database,
            repository.repository_id,
            &body(300_000_204, "acme/widgets", stars),
            None,
        )
        .await?;
    }

    assert_eq!(
        i64::from(u32::try_from(REVISION_HISTORY_LIMIT)?),
        revision_count(&database, repository.repository_id).await?
    );

    let ordered_stars: Vec<i64> = sqlx::query(
        "select payload->>'stargazers' as stars
         from github_catalog.repository_metadata_revisions
         where repository_id = $1
         order by observed_at asc, revision_id asc",
    )
    .bind(repository.repository_id)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .map(|row| row.try_get::<String, _>("stars"))
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .map(|s| s.parse::<i64>())
    .collect::<Result<Vec<_>, _>>()?;
    let expected: Vec<i64> = (1..11).collect();
    assert_eq!(
        ordered_stars, expected,
        "exactly the most recent window stays, oldest to newest"
    );

    database.cleanup().await?;
    Ok(())
}
