//! Live repository API gate against disposable `PostgreSQL` and fake GitHub.

use std::net::TcpListener;
use std::process::Stdio;

use secrecy::SecretString;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

use support::{
    KEY_HEX, configured_command, http_get_json, http_json, stop_process, test_database_url,
    wait_ready,
};

const USER_ID: &str = "018f0000-0000-7000-8000-000000000711";
const ACCOUNT_ID: &str = "018f0000-0000-7000-8000-000000000712";

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the one live-process scenario proves readiness, preview, confirmation, and partial truth together"
)]
async fn real_service_serves_preview_and_partial_action_against_fake_provider()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let account_id = Uuid::parse_str(ACCOUNT_ID)?;
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(format!("user:{USER_ID}"))
    .execute(database.database.pool())
    .await?;
    let key = ratatoskr_github_catalog::CredentialKey::from_hex(KEY_HEX, "test-key")?;
    ratatoskr_github_catalog::register_pat(
        &database.database,
        account_id,
        SecretString::from("synthetic-live-provider-token"),
        &key,
        &ratatoskr_github_catalog::VerifiedGithubAccount {
            provider_user_id: 71,
            login: "synthetic-live-owner".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;
    sqlx::raw_sql(
        "create function github_catalog.fail_live_desired_policy() returns trigger language plpgsql as
         $$ begin raise exception 'synthetic live desired policy failure'; end $$;
         create trigger fail_live_desired_policy before insert or update
         on github_catalog.backup_policy_publication_cursor
         for each statement execute function github_catalog.fail_live_desired_policy();",
    )
    .execute(database.database.pool())
    .await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;

    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "id": 42,
                    "full_name": "owner/repository",
                    "description": "Live repository preview.",
                    "language": "Rust",
                    "stargazers_count": 321,
                    "topics": [],
                    "default_branch": "main",
                    "pushed_at": "2026-08-27T10:00:00Z"
                }))
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4999")
                .insert_header("x-ratelimit-reset", "1788000000"),
        )
        .mount(&provider)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("repository(owner:"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "repository": { "id": "gid://repository/42" } },
            "rateLimit": { "remaining": 4998, "resetAt": "2026-08-27T11:00:00Z" }
        })))
        .mount(&provider)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("addStar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "addStar": { "starrable": { "viewerHasStarred": true } } },
            "rateLimit": { "remaining": 4997, "resetAt": "2026-08-27T11:00:00Z" }
        })))
        .mount(&provider)
        .await;

    let reserved_admin = TcpListener::bind("127.0.0.1:0")?;
    let admin_address = reserved_admin.local_addr()?;
    let reserved_api = TcpListener::bind("127.0.0.1:0")?;
    let api_address = reserved_api.local_addr()?;
    drop(reserved_admin);
    drop(reserved_api);
    let mut child = configured_command(admin_address, api_address, &database_url, &provider.uri())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;
    let capabilities = http_get_json(api_address, "/v1/capabilities", USER_ID)?;
    let preview = http_json(
        api_address,
        "/v1/gh/repositories/preview",
        USER_ID,
        &serde_json::json!({
            "repository_url": "https://github.com/owner/repository"
        }),
    )?;
    let action = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &serde_json::json!({
            "mode": "star",
            "target": {
                "github_repository_numeric_id": 42,
                "repository_full_name": "owner/repository",
                "canonical_url": "https://github.com/owner/repository"
            },
            "account_ref": format!("github-account:{ACCOUNT_ID}"),
            "confirmation_evidence_ref":
                "telegram-confirmation:018f0000-0000-7000-8000-000000000713",
            "idempotency_key": "live-star.018f0000-0000-7000-8000-000000000714"
        }),
    );
    stop_process(&mut child)?;
    let action = action?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    database.cleanup().await?;

    assert_eq!(
        capabilities.status, 200,
        "capabilities: {}",
        capabilities.body
    );
    let capabilities: serde_json::Value = serde_json::from_str(&capabilities.body)?;
    assert_eq!(capabilities["repository_preview"], true);
    assert_eq!(
        capabilities["repository_actions"],
        serde_json::json!(["metadata", "track", "star"])
    );
    assert_eq!(preview.status, 200, "preview: {}", preview.body);
    let preview: serde_json::Value = serde_json::from_str(&preview.body)?;
    assert_eq!(preview["description"], "Live repository preview.");
    assert_eq!(preview["stargazer_count"], 321);
    assert_eq!(preview["primary_language"], "Rust");
    assert_eq!(
        preview["available_actions"],
        serde_json::json!(["metadata", "track", "star"])
    );
    assert_eq!(action.status, 200, "action: {}", action.body);
    let action: serde_json::Value = serde_json::from_str(&action.body)?;
    assert_eq!(action["aggregate"], "partial");
    assert_eq!(action["provider_star"]["status"], "succeeded");
    assert_eq!(action["desired_backup"]["status"], "failed");
    assert_eq!(
        action["desired_backup"]["reason"],
        "policy_publication_failed"
    );
    assert_eq!(provider_requests.len(), 4);
    assert!(
        provider_requests
            .iter()
            .all(|request| { !String::from_utf8_lossy(&request.body).contains("removeStar") })
    );
    Ok(())
}
