//! Least-privilege immutable README boundary for Knowledge.

use std::fmt;
use std::path::Path;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use ratatoskr_github_catalog::provider::README_MAX_BYTES;
use ratatoskr_github_catalog::{ResolveReadmeError, resolve_authorized_readme};
use ratatoskr_identifiers::{BlobRef, RepositoryId, TenantRef};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::repository_api::RepositoryApiState;

const AUTHORIZATION_SCHEME: &str = "Bearer ";
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 256;
const MAX_TOKEN_FILE_BYTES: u64 = 258;
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// Redacted digest of the bearer credential shared with Knowledge.
#[derive(Clone)]
pub struct ServiceBearerToken {
    digest: [u8; 32],
}

/// Failure while loading a file-backed service credential.
#[derive(Debug, thiserror::Error)]
pub enum ServiceBearerTokenError {
    /// The configured secret file could not be inspected or read.
    #[error("the Knowledge service token file is unavailable")]
    Read(#[source] std::io::Error),
    /// The configured path does not resolve to a regular file.
    #[error("the Knowledge service token path is not a regular file")]
    NotRegularFile,
    /// The secret file is readable by users outside its owner/group boundary.
    #[cfg(unix)]
    #[error("the Knowledge service token file permissions are too broad")]
    UnsafePermissions,
    /// The secret is not a bounded opaque bearer value.
    #[error("the Knowledge service token file contains an invalid bearer value")]
    Invalid,
}

impl ServiceBearerToken {
    /// Loads and hashes one bounded bearer credential from a deployment secret file.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceBearerTokenError`] when the path is not a safely permissioned regular
    /// file or its contents are not a bounded opaque token.
    pub fn from_file(path: &Path) -> Result<Self, ServiceBearerTokenError> {
        let metadata = std::fs::metadata(path).map_err(ServiceBearerTokenError::Read)?;
        if !metadata.is_file() {
            return Err(ServiceBearerTokenError::NotRegularFile);
        }
        if metadata.len() > MAX_TOKEN_FILE_BYTES {
            return Err(ServiceBearerTokenError::Invalid);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            if metadata.permissions().mode() & 0o137 != 0 {
                return Err(ServiceBearerTokenError::UnsafePermissions);
            }
        }

        let raw = std::fs::read_to_string(path).map_err(ServiceBearerTokenError::Read)?;
        let token = raw.trim_end_matches(['\r', '\n']);
        if token.len() < MIN_TOKEN_BYTES
            || token.len() > MAX_TOKEN_BYTES
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ServiceBearerTokenError::Invalid);
        }
        Ok(Self {
            digest: Sha256::digest(token.as_bytes()).into(),
        })
    }

    fn matches(&self, supplied: &str) -> bool {
        let supplied_digest: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
        bool::from(self.digest.ct_eq(&supplied_digest))
    }
}

impl fmt::Debug for ServiceBearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ServiceBearerToken")
            .field(&"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveReadmeRequest {
    owner: TenantRef,
    repository_id: RepositoryId,
    content_ref: BlobRef,
}

#[derive(Debug, Clone, Copy)]
enum ContentFault {
    Unauthenticated,
    Forbidden,
    InvalidRequest,
    NotFound,
    TooLarge,
    Integrity,
    Unavailable,
}

/// Builds the separately bound service-authenticated internal router.
pub fn internal_router(state: RepositoryApiState) -> Router {
    Router::new()
        .route(
            "/internal/v1/repository-readmes/resolve",
            post(resolve_readme),
        )
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn(no_store))
}

async fn resolve_readme(
    State(state): State<RepositoryApiState>,
    headers: HeaderMap,
    payload: Result<Json<ResolveReadmeRequest>, JsonRejection>,
) -> Result<Response, ContentFault> {
    authenticate_service(&state, &headers)?;
    let Json(request) = payload.map_err(|_| ContentFault::InvalidRequest)?;
    let bytes = resolve_authorized_readme(
        &state.database,
        &request.owner,
        request.repository_id,
        &request.content_ref,
        README_MAX_BYTES,
    )
    .await
    .map_err(ContentFault::from)?;

    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown"),
    );
    Ok(response)
}

fn authenticate_service(
    state: &RepositoryApiState,
    headers: &HeaderMap,
) -> Result<(), ContentFault> {
    let expected = state
        .knowledge_service_token
        .as_ref()
        .ok_or(ContentFault::Unavailable)?;
    let authorization = headers
        .get(header::AUTHORIZATION)
        .ok_or(ContentFault::Unauthenticated)?;
    let supplied = authorization
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix(AUTHORIZATION_SCHEME))
        .filter(|value| !value.is_empty())
        .ok_or(ContentFault::Forbidden)?;
    if expected.matches(supplied) {
        Ok(())
    } else {
        Err(ContentFault::Forbidden)
    }
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl From<ResolveReadmeError> for ContentFault {
    fn from(error: ResolveReadmeError) -> Self {
        match error {
            ResolveReadmeError::NotFound => Self::NotFound,
            ResolveReadmeError::TooLarge => Self::TooLarge,
            ResolveReadmeError::Integrity | ResolveReadmeError::Contract(_) => Self::Integrity,
            _ => Self::Unavailable,
        }
    }
}

impl IntoResponse for ContentFault {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Integrity => StatusCode::CONFLICT,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
        .into_response()
    }
}
