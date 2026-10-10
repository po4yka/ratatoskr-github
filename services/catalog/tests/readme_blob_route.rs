//! The authorized README byte endpoint Knowledge reads (XR-021 CONTRACTS.md S09).

use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt as _;
use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog_service::{RepositoryApiState, domain_router};
use secrecy::SecretString;
use tower::ServiceExt as _;

const SECRET: &str = "synthetic-reader-service-secret";
const README: &[u8] = b"# Synthetic README\n\nBytes the endpoint must return untouched.\n";

struct Reply {
    status: u16,
    digest_header: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

async fn fixture(
    secret_configured: bool,
) -> Result<(TestDatabase, axum::Router, String), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let digest: String = sqlx::query_scalar("select encode(sha256($1::bytea), 'hex')")
        .bind(README)
        .fetch_one(database.database.pool())
        .await?;
    sqlx::query(
        "insert into github_catalog.repository_readme_blobs (content_digest, bytes, length_bytes)
         values ($1, $2, $3)",
    )
    .bind(&digest)
    .bind(README)
    .bind(i64::try_from(README.len())?)
    .execute(database.database.pool())
    .await?;
    let mut state = RepositoryApiState::new(
        database.database.clone(),
        ReqwestGithubApi::for_base_url("http://127.0.0.1:9")?,
        None,
    );
    if secret_configured {
        state = state.with_reader_service_secret(SecretString::from(SECRET));
    }
    Ok((database, domain_router(state), digest))
}

async fn get(
    router: &axum::Router,
    digest: &str,
    authorization: Option<&str>,
) -> Result<Reply, Box<dyn std::error::Error>> {
    let mut request = Request::builder()
        .method("GET")
        .uri(format!("/internal/v1/readme-blobs/{digest}"));
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    let response = router.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let digest_header = response
        .headers()
        .get("x-content-sha256")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.into_body().collect().await?.to_bytes().to_vec();
    Ok(Reply {
        status,
        digest_header,
        content_type,
        body,
    })
}

#[tokio::test]
async fn a_stored_readme_is_served_with_its_digest_to_the_bearer()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, router, digest) = fixture(true).await?;

    let reply = get(&router, &digest, Some(&format!("Bearer {SECRET}"))).await?;

    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, README);
    assert_eq!(reply.digest_header.as_deref(), Some(digest.as_str()));
    assert_eq!(reply.content_type.as_deref(), Some("text/markdown"));
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_missing_or_wrong_bearer_is_refused_before_any_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, router, digest) = fixture(true).await?;
    let unknown = "0".repeat(64);

    let missing = get(&router, &digest, None).await?;
    let wrong = get(&router, &digest, Some("Bearer not-the-secret")).await?;
    let wrong_scheme = get(&router, &digest, Some(&format!("Basic {SECRET}"))).await?;
    let unknown_without_bearer = get(&router, &unknown, None).await?;

    assert_eq!(missing.status, 401);
    assert_eq!(wrong.status, 401);
    assert_eq!(wrong_scheme.status, 401);
    assert_eq!(
        unknown_without_bearer.status, 401,
        "an unauthenticated caller must not learn which digests exist"
    );
    assert!(missing.body.is_empty() || missing.body != README);
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn an_unknown_or_malformed_digest_is_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let (database, router, _digest) = fixture(true).await?;
    let bearer = format!("Bearer {SECRET}");

    let unknown = get(&router, &"0".repeat(64), Some(&bearer)).await?;
    let malformed = get(&router, "NOT-A-DIGEST", Some(&bearer)).await?;

    assert_eq!(unknown.status, 404);
    assert_eq!(malformed.status, 404);
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn the_route_is_absent_when_no_secret_is_configured() -> Result<(), Box<dyn std::error::Error>>
{
    let (database, router, digest) = fixture(false).await?;

    let with_bearer = get(&router, &digest, Some(&format!("Bearer {SECRET}"))).await?;
    let without = get(&router, &digest, None).await?;

    assert_eq!(with_bearer.status, 404);
    assert_eq!(without.status, 404);
    assert!(with_bearer.digest_header.is_none());
    database.cleanup().await?;
    Ok(())
}
