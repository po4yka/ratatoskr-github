//! Durable README-evidence storage against a disposable Catalog database.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use ratatoskr_github_catalog::provider::ProviderRepositoryBody;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AppliedOutcome, RepositoryAnalysisSource, apply_fresh_source, store_readme, upsert_repository,
};
use ratatoskr_github_contracts::ReadmeRevision;
use ratatoskr_identifiers::DigestAlgorithm;

/// README bytes must have their own content-addressed durable boundary before an analysis event
/// may carry only a `BlobRef` to them.
#[tokio::test]
async fn fresh_readme_has_a_durable_content_addressed_store()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let relation: Option<String> =
        sqlx::query_scalar("select to_regclass('github_catalog.repository_readme_blobs')::text")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        relation.as_deref(),
        Some("github_catalog.repository_readme_blobs"),
        "a README must be preserved before any analysis command can reference it"
    );
    database.cleanup().await?;
    Ok(())
}

/// The stored source is addressed by its raw bytes, never a provider URL or an event body.
#[tokio::test]
async fn equal_readme_bytes_converge_on_one_sha256_blob_reference()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let first = store_readme(&database.database, b"# Widget\n").await?;
    let second = store_readme(&database.database, b"# Widget\n").await?;
    assert_eq!(first, second);
    assert_eq!(first.owner_service.as_str(), "ratatoskr-github");
    assert_eq!(first.digest.algorithm, DigestAlgorithm::Sha256);
    assert_eq!(first.length_bytes, 9);
    let stored: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.repository_readme_blobs")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(stored, 1);
    database.cleanup().await?;
    Ok(())
}

/// A replay of the same source revision converges, and recording a source revision queues nothing
/// by itself.
#[tokio::test]
async fn an_equal_source_revision_converges_and_queues_no_request()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository = upsert_repository(&database.database, 300_000_401).await?;
    let readme = store_readme(&database.database, b"# Widget\n").await?;
    let source = RepositoryAnalysisSource {
        readme: ReadmeRevision::Present {
            content_ref: readme,
        },
        readme_etag: Some(r#"W/"readme-v1""#.to_owned()),
    };
    let body = ProviderRepositoryBody {
        provider_repository_id: 300_000_401,
        full_name: "acme/widgets".to_owned(),
        description: Some("A synthetic widget".to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 42,
        topics: vec!["widgets".to_owned()],
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-01T10:00:00Z".to_owned()),
    };
    assert_eq!(
        apply_fresh_source(
            &database.database,
            repository.repository_id,
            &body,
            None,
            &source
        )
        .await?,
        AppliedOutcome::Created
    );
    assert_eq!(
        apply_fresh_source(
            &database.database,
            repository.repository_id,
            &body,
            None,
            &source
        )
        .await?,
        AppliedOutcome::Unchanged
    );
    let revisions: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repository_metadata_revisions where repository_id = $1",
    )
    .bind(repository.repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(revisions, 1, "a replay appends no second revision");
    let queued: i64 = sqlx::query_scalar(
        "select (select count(*) from github_catalog.outbox_events)
              + (select count(*) from github_catalog.repository_analysis_requests)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(queued, 0, "only a watch originates an analysis request");
    database.cleanup().await?;
    Ok(())
}
