//! The repository action route against a wiremock provider, served in process.
//!
//! The route is driven through its real router (`tower::ServiceExt::oneshot`), so the handler, the
//! provider adapter and the database are the production ones; only the transport socket is skipped.

#![allow(
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "each test binary compiles this shared module and uses a different subset"
)]

use axum::Router;
use http_body_util::BodyExt as _;
use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog_service::{RepositoryApiState, domain_router};
use tower::ServiceExt as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub const FIRST_USER: &str = "018f0000-0000-7000-8000-000000000a01";
pub const SECOND_USER: &str = "018f0000-0000-7000-8000-000000000a02";
pub const README_TEXT: &str = "# Tracked repository\n\nBytes Knowledge reads back.\n";

pub struct Harness {
    pub database: TestDatabase,
    pub provider: MockServer,
    router: Router,
}

impl Harness {
    /// Starts the route against a fresh database and a provider that serves `owner/repository`
    /// (numeric id 42) and, when `readme` is given, its README; otherwise the README is 404.
    pub async fn start(readme: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with(readme, false).await
    }

    /// Like [`Self::start`], plus a connected account of [`FIRST_USER`] with a stored PAT and a
    /// provider that accepts the star mutation, so a `star` action can complete.
    pub async fn start_with_account(
        readme: Option<&str>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with(readme, true).await
    }

    async fn start_with(
        readme: Option<&str>,
        with_account: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let database = TestDatabase::create().await?;
        let credential_key = if with_account {
            Some(connect_account(&database).await?)
        } else {
            None
        };
        let provider = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "id": 42,
                        "full_name": "owner/repository",
                        "description": "A tracked repository.",
                        "language": "Rust",
                        "stargazers_count": 123,
                        "topics": [],
                        "default_branch": "main",
                        "pushed_at": "2026-08-27T10:00:00Z"
                    }))
                    .insert_header("x-ratelimit-limit", "5000")
                    .insert_header("x-ratelimit-remaining", "4999")
                    .insert_header("x-ratelimit-reset", "1787000000"),
            )
            .mount(&provider)
            .await;
        if with_account {
            mount_star_mutation(&provider).await;
        }
        let readme_response = match readme {
            Some(text) => ResponseTemplate::new(200).set_body_string(text),
            None => ResponseTemplate::new(404),
        };
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/readme"))
            .respond_with(readme_response)
            .mount(&provider)
            .await;
        let router = domain_router(RepositoryApiState::new(
            database.database.clone(),
            ReqwestGithubApi::for_base_url(&provider.uri())?,
            credential_key,
        ));
        Ok(Self {
            database,
            provider,
            router,
        })
    }

    /// Posts one action as `user` and returns the status and the JSON answer.
    pub async fn act(
        &self,
        user: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), Box<dyn std::error::Error>> {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/gh/repositories/actions")
            .header("content-type", "application/json")
            .header("x-ratatoskr-user-id", user)
            .body(axum::body::Body::from(body.to_string()))?;
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status().as_u16();
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        ))
    }

    pub async fn count(&self, table: &str) -> Result<i64, Box<dyn std::error::Error>> {
        Ok(
            sqlx::query_scalar(&format!("select count(*) from github_catalog.{table}"))
                .fetch_one(self.database.database.pool())
                .await?,
        )
    }
}

pub const ACCOUNT_ID: &str = "018f0000-0000-7000-8000-000000000a03";
const KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

async fn connect_account(
    database: &TestDatabase,
) -> Result<ratatoskr_github_catalog::CredentialKey, Box<dyn std::error::Error>> {
    let account_id = uuid::Uuid::parse_str(ACCOUNT_ID)?;
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(format!("user:{FIRST_USER}"))
    .execute(database.database.pool())
    .await?;
    let key = ratatoskr_github_catalog::CredentialKey::from_hex(KEY_HEX, "test-key")?;
    ratatoskr_github_catalog::register_pat(
        &database.database,
        account_id,
        secrecy::SecretString::from("synthetic-provider-token"),
        &key,
        &ratatoskr_github_catalog::VerifiedGithubAccount {
            provider_user_id: 7,
            login: "synthetic-owner".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;
    Ok(key)
}

async fn mount_star_mutation(provider: &MockServer) {
    for (needle, body) in [
        (
            "repository(owner:",
            serde_json::json!({
                "data": { "repository": { "id": "gid://repository/42" } },
                "rateLimit": { "remaining": 4998, "resetAt": "2026-08-27T11:00:00Z" }
            }),
        ),
        (
            "addStar",
            serde_json::json!({
                "data": { "addStar": { "starrable": { "viewerHasStarred": true } } },
                "rateLimit": { "remaining": 4997, "resetAt": "2026-08-27T11:00:00Z" }
            }),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(wiremock::matchers::body_string_contains(needle))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(provider)
            .await;
    }
}

pub fn star_request(idempotency_key: &str) -> serde_json::Value {
    let mut request = action_request("star", idempotency_key);
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "account_ref".to_owned(),
            serde_json::json!(format!("github-account:{ACCOUNT_ID}")),
        );
    }
    request
}

pub fn action_request(mode: &str, idempotency_key: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": mode,
        "target": {
            "github_repository_numeric_id": 42,
            "repository_full_name": "owner/repository",
            "canonical_url": "https://github.com/owner/repository"
        },
        "confirmation_evidence_ref": "telegram-confirmation:018f0000-0000-7000-8000-000000000613",
        "idempotency_key": idempotency_key
    })
}
