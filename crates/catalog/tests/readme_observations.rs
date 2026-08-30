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
use ratatoskr_identifiers::{DigestAlgorithm, TenantRef};

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

/// The source revision and typed request are committed together and replays cannot create a
/// second command or spendable source revision.
#[tokio::test]
async fn source_revision_creates_one_contract_valid_analysis_outbox_command()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let repository = upsert_repository(&database.database, 300_000_401).await?;
    let readme = store_readme(&database.database, b"# Widget\n").await?;
    let source = RepositoryAnalysisSource {
        owner: TenantRef::parse("user:018f0000-0000-7000-8000-000000000005")?,
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
    let envelope_bytes: Vec<u8> = sqlx::query_scalar(
        "select envelope from github_catalog.outbox_events where subject = 'evt.knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    let envelope = ratatoskr_event_envelope::EventEnvelope::from_json(&envelope_bytes)?;
    assert_eq!(
        envelope.event_type.to_wire(),
        "knowledge.repository_analysis.requested.v1"
    );
    assert_eq!(
        envelope.event_id.to_string(),
        envelope_bytes_message_id(&database).await?
    );
    let request =
        envelope.payload_as::<ratatoskr_github_contracts::RepositoryAnalysisRequested>()?;
    assert_eq!(
        request.repository_id.to_string(),
        repository.repository_id.to_string()
    );
    let publications: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.repository_analysis_publications")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(publications, 1);
    database.cleanup().await?;
    Ok(())
}

async fn envelope_bytes_message_id(database: &TestDatabase) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "select message_id::text from github_catalog.outbox_events
         where subject = 'evt.knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await
}
