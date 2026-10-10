//! The authorized README byte endpoint Knowledge reads (XR-021 CONTRACTS.md S09).
//!
//! The route is mounted only while a reader service secret is configured, and it is not part of
//! Edge's prefix table: it is reachable on the loopback domain listener by a caller that holds the
//! shared bearer secret. The secret is compared in constant time before any lookup, so a caller
//! without it cannot learn which digests exist.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use ratatoskr_github_catalog::Database;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

const ROUTE: &str = "/internal/v1/readme-blobs/{sha256_hex}";
const BEARER_PREFIX: &str = "Bearer ";
const DIGEST_HEADER: &str = "x-content-sha256";
const MARKDOWN: &str = "text/markdown";

#[derive(Clone)]
struct ReadmeState {
    database: Database,
    secret: SecretString,
}

/// Builds the route fragment that serves stored README bytes to the holder of `secret`.
pub(crate) fn router(database: Database, secret: SecretString) -> Router {
    Router::new()
        .route(ROUTE, get(readme_blob))
        .with_state(ReadmeState { database, secret })
}

async fn readme_blob(
    State(state): State<ReadmeState>,
    Path(digest): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !bearer_matches(&headers, &state.secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !is_sha256_hex(&digest) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let stored: Result<Option<Vec<u8>>, sqlx::Error> = sqlx::query_scalar(
        "select bytes from github_catalog.repository_readme_blobs where content_digest = $1",
    )
    .bind(&digest)
    .fetch_optional(state.database.pool())
    .await;
    match stored {
        Ok(Some(bytes)) => {
            let mut response = bytes.into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(MARKDOWN));
            if let Ok(value) = HeaderValue::from_str(&digest) {
                response.headers_mut().insert(DIGEST_HEADER, value);
            }
            response
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Compares the presented bearer token with the configured secret without branching on content.
/// Both sides are hashed first so the comparison is over equal-length values.
fn bearer_matches(headers: &HeaderMap, secret: &SecretString) -> bool {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix(BEARER_PREFIX))
        .unwrap_or_default();
    let presented = Sha256::digest(presented.as_bytes());
    let expected = Sha256::digest(secret.expose_secret().as_bytes());
    let equal: bool = presented.ct_eq(&expected).into();
    // An absent or empty header hashes like an empty secret would; refuse it explicitly.
    equal && !secret.expose_secret().is_empty()
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
