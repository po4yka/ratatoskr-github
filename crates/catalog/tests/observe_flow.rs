//! End-to-end observe flow: wiremock provider plus disposable catalog
//! database, exercising budget, conditional requests, and persistence.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{ObserveOutcome, observe_repository};
use ratatoskr_identifiers::TenantRef;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO_PATH: &str = "/repos/acme/widgets";
const FRESH_BODY: &str = r#"{
    "id": 300000300,
    "full_name": "acme/widgets",
    "description": "A synthetic widget",
    "language": "Rust",
    "stargazers_count": 42,
    "topics": ["widgets"],
    "default_branch": "main",
    "pushed_at": "2026-08-01T10:00:00Z"
}"#;

fn rate_headers(remaining: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", remaining)
        .insert_header("x-ratelimit-reset", "1787000000")
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the single wiremock scenario proves metadata, README acquisition, and replay together"
)]
async fn observe_repository_end_to_end_via_wiremock() -> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(REPO_PATH))
        .respond_with(
            rate_headers("4999")
                .set_body_string(FRESH_BODY)
                .insert_header("etag", r#"W/"v1""#),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/readme"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("# Widgets\n")
                .insert_header("etag", r#"W/"readme-v1""#),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(REPO_PATH))
        .and(header("if-none-match", r#"W/"v1""#))
        .respond_with(
            ResponseTemplate::new(304)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4998")
                .insert_header("x-ratelimit-reset", "1787000000"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("account-e2e");

    let first = observe_repository(
        &database.database,
        &gateway,
        &ledger,
        &token,
        TenantRef::parse("user:018f0000-0000-7000-8000-000000000005")?,
        "acme",
        "widgets",
    )
    .await?;
    let ObserveOutcome::Observed { repository_id } = first else {
        return Err(format!("the first observe must apply fresh state, got {first:?}").into());
    };

    let alias = ratatoskr_github_catalog::resolve_alias(
        &database.database,
        ratatoskr_github_catalog::AliasKind::OwnerName,
        "acme/widgets",
    )
    .await?;
    assert_eq!(alias.as_ref(), Some(&repository_id));
    let revisions: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repository_metadata_revisions where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(revisions, 1);
    let request_payload: serde_json::Value = sqlx::query_scalar(
        "select payload from github_catalog.outbox_events
         where subject = 'knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    let request: ratatoskr_github_contracts::RepositoryAnalysisRequested =
        serde_json::from_value(request_payload.clone())?;
    assert_eq!(request.repository_id.to_string(), repository_id.to_string());
    assert!(
        request_payload.to_string().contains("content_ref"),
        "the command carries a BlobRef, not README bytes"
    );
    assert!(
        !request_payload.to_string().contains("Widgets"),
        "README body bytes must stay inside the Catalog evidence boundary"
    );
    assert_eq!(
        ledger.remaining(&token),
        Some(4999),
        "the shared ledger must ingest the response rate headers"
    );

    let second = observe_repository(
        &database.database,
        &gateway,
        &ledger,
        &token,
        TenantRef::parse("user:018f0000-0000-7000-8000-000000000005")?,
        "acme",
        "widgets",
    )
    .await?;
    assert_eq!(
        second,
        ObserveOutcome::NotModified { repository_id },
        "the second observe must short-circuit on 304"
    );
    let revisions_after: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repository_metadata_revisions where repository_id = $1",
    )
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(revisions_after, 1);
    let requests_after_redelivery: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.outbox_events
         where subject = 'knowledge.repository_analysis.requested.v1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(requests_after_redelivery, 1);
    assert_eq!(ledger.remaining(&token), Some(4998));

    server.verify().await;
    database.cleanup().await?;
    Ok(())
}
