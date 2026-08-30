//! Repository domain API behavior through the real service process.

use std::net::TcpListener;
use std::process::Stdio;

use secrecy::SecretString;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

use support::{
    KEY_HEX, configured_command, http_json, stop_process, test_database_url, wait_ready,
};

const USER_ID: &str = "018f0000-0000-7000-8000-000000000611";
const ACCOUNT_ID: &str = "018f0000-0000-7000-8000-000000000612";

#[tokio::test]
async fn preview_returns_bounded_metadata_without_catalog_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
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
                    "description": "A bounded repository preview.",
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

    let reserved_admin = TcpListener::bind("127.0.0.1:0")?;
    let admin_address = reserved_admin.local_addr()?;
    let reserved_api = TcpListener::bind("127.0.0.1:0")?;
    let api_address = reserved_api.local_addr()?;
    drop(reserved_admin);
    drop(reserved_api);
    let mut child = configured_command(admin_address, api_address, &database_url, &provider.uri())
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;

    let response = http_json(
        api_address,
        "/v1/gh/repositories/preview",
        USER_ID,
        &serde_json::json!({
            "repository_url": "https://github.com/owner/repository"
        }),
    );
    stop_process(&mut child)?;
    let response = response?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    let write_count: i64 = sqlx::query_scalar(
        "select
            (select count(*) from github_catalog.repositories)
          + (select count(*) from github_catalog.repository_metadata)
          + (select count(*) from github_catalog.mutation_audit)",
    )
    .fetch_one(database.database.pool())
    .await?;
    database.cleanup().await?;

    assert_eq!(response.status, 200, "preview body: {}", response.body);
    let body: serde_json::Value = serde_json::from_str(&response.body)?;
    assert_eq!(body["target"]["github_repository_numeric_id"], 42);
    assert_eq!(body["target"]["repository_full_name"], "owner/repository");
    assert_eq!(
        body["target"]["canonical_url"],
        "https://github.com/owner/repository"
    );
    assert_eq!(body["description"], "A bounded repository preview.");
    assert_eq!(body["stargazer_count"], 123);
    assert_eq!(body["primary_language"], "Rust");
    assert_eq!(
        body["available_actions"],
        serde_json::json!(["metadata", "track"])
    );
    assert_eq!(provider_requests.len(), 1);
    assert_eq!(write_count, 0, "preview mutated Catalog state");
    Ok(())
}

#[tokio::test]
async fn preview_refuses_foreign_private_and_subresource_urls_before_disclosure()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 99,
            "full_name": "foreign/private",
            "description": "Must not be disclosed.",
            "language": "Rust",
            "stargazers_count": 999,
            "topics": [],
            "default_branch": "main",
            "pushed_at": "2026-08-27T10:00:00Z"
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
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;

    let mut invalid_responses = Vec::new();
    for repository_url in [
        "https://github.com/owner/repository/issues",
        "https://github.com/owner/repository/pull/1",
        "https://github.com/owner/repository/tree/main",
        "https://github.com/owner/repository?tab=readme",
        "https://github.com/owner/repository#readme",
    ] {
        invalid_responses.push(http_json(
            api_address,
            "/v1/gh/repositories/preview",
            USER_ID,
            &serde_json::json!({ "repository_url": repository_url }),
        )?);
    }
    let foreign = http_json(
        api_address,
        "/v1/gh/repositories/preview",
        USER_ID,
        &serde_json::json!({
            "repository_url": "https://github.com/owner/repository"
        }),
    );
    stop_process(&mut child)?;
    let foreign = foreign?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    database.cleanup().await?;

    for response in invalid_responses {
        assert_eq!(response.status, 400, "invalid response: {}", response.body);
        let body: serde_json::Value = serde_json::from_str(&response.body)?;
        assert_eq!(body["code"], "github.request.invalid");
    }
    assert_eq!(
        provider_requests.len(),
        1,
        "subresource input reached GitHub"
    );
    assert_eq!(foreign.status, 404, "foreign response: {}", foreign.body);
    assert!(
        !foreign.body.contains("Must not be disclosed") && !foreign.body.contains("999"),
        "foreign metadata leaked through the safe error"
    );
    Ok(())
}

#[tokio::test]
async fn action_refuses_missing_confirmation_foreign_account_and_unsupported_mode_without_provider_calls()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let provider = MockServer::start().await;
    let reserved_admin = TcpListener::bind("127.0.0.1:0")?;
    let admin_address = reserved_admin.local_addr()?;
    let reserved_api = TcpListener::bind("127.0.0.1:0")?;
    let api_address = reserved_api.local_addr()?;
    drop(reserved_admin);
    drop(reserved_api);
    let mut child = configured_command(admin_address, api_address, &database_url, &provider.uri())
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;

    let valid_star = serde_json::json!({
        "mode": "star",
        "target": {
            "github_repository_numeric_id": 42,
            "repository_full_name": "owner/repository",
            "canonical_url": "https://github.com/owner/repository"
        },
        "account_ref": "github-account:018f0000-0000-7000-8000-000000000612",
        "confirmation_evidence_ref": "telegram-confirmation:018f0000-0000-7000-8000-000000000613",
        "idempotency_key": "telegram-github-action.018f0000-0000-7000-8000-000000000614"
    });
    let mut missing_confirmation = valid_star.clone();
    missing_confirmation
        .as_object_mut()
        .ok_or("action fixture is not an object")?
        .remove("confirmation_evidence_ref");
    let mut unsupported_mode = valid_star.clone();
    unsupported_mode["mode"] = serde_json::json!("mirror");

    let missing = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &missing_confirmation,
    )?;
    let unsupported = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &unsupported_mode,
    )?;
    let foreign = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &valid_star,
    );
    stop_process(&mut child)?;
    let foreign = foreign?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    database.cleanup().await?;

    assert_eq!(
        missing.status, 400,
        "missing confirmation: {}",
        missing.body
    );
    assert_eq!(
        unsupported.status, 400,
        "unsupported mode: {}",
        unsupported.body
    );
    assert_eq!(foreign.status, 200, "foreign account: {}", foreign.body);
    let result: serde_json::Value = serde_json::from_str(&foreign.body)?;
    assert_eq!(result["aggregate"], "failed");
    assert_eq!(result["metadata"]["status"], "skipped");
    assert_eq!(result["provider_star"]["status"], "refused");
    assert_eq!(result["provider_star"]["reason"], "not_authorized");
    assert_eq!(result["desired_backup"]["status"], "skipped");
    assert!(
        provider_requests.is_empty(),
        "a refused action reached GitHub"
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the process scenario proves both safe modes and their database/provider effects together"
)]
async fn metadata_and_track_never_call_provider_star() -> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42,
            "full_name": "owner/repository",
            "description": "A repository action.",
            "language": "Rust",
            "stargazers_count": 123,
            "topics": [],
            "default_branch": "main",
            "pushed_at": "2026-08-27T10:00:00Z"
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
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;

    let metadata = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &action_request(
            "metadata",
            "metadata-action.018f0000-0000-7000-8000-000000000615",
        ),
    )?;
    let track = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &action_request("track", "track-action.018f0000-0000-7000-8000-000000000616"),
    );
    stop_process(&mut child)?;
    let track = track?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    let mode: Option<String> = sqlx::query_scalar(
        "select mode from github_catalog.repositories where provider_repository_id = 42",
    )
    .fetch_optional(database.database.pool())
    .await?
    .flatten();
    let metadata_rows: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.repository_metadata")
            .fetch_one(database.database.pool())
            .await?;
    let dirty_generation: i64 = sqlx::query_scalar(
        "select dirty_generation from github_catalog.backup_policy_publication_cursor
         where scope = 'catalog'",
    )
    .fetch_optional(database.database.pool())
    .await?
    .unwrap_or_default();
    database.cleanup().await?;

    assert_eq!(metadata.status, 200, "metadata result: {}", metadata.body);
    assert_eq!(track.status, 200, "track result: {}", track.body);
    let metadata_result: serde_json::Value = serde_json::from_str(&metadata.body)?;
    let track_result: serde_json::Value = serde_json::from_str(&track.body)?;
    assert_eq!(metadata_result["metadata"]["status"], "succeeded");
    assert_eq!(metadata_result["provider_star"]["status"], "skipped");
    assert_eq!(metadata_result["desired_backup"]["status"], "skipped");
    assert!(
        matches!(
            track_result["metadata"]["status"].as_str(),
            Some("succeeded" | "already_applied")
        ),
        "track did not report metadata truth: {track_result}"
    );
    assert_eq!(track_result["provider_star"]["status"], "skipped");
    assert!(
        matches!(
            track_result["desired_backup"]["status"].as_str(),
            Some("accepted" | "already_applied")
        ),
        "track did not report desired-policy acceptance: {track_result}"
    );
    assert_eq!(mode.as_deref(), Some("tracked"));
    assert_eq!(metadata_rows, 1);
    assert!(dirty_generation > 0);
    assert_eq!(
        provider_requests.len(),
        2,
        "each action needs one metadata GET"
    );
    assert!(
        provider_requests
            .iter()
            .all(|request| request.method.as_str() == "GET"),
        "metadata/track invoked a provider mutation"
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the injected post-provider fault and no-compensation evidence form one scenario"
)]
async fn provider_star_success_survives_later_persistence_failure_without_unstar()
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
        SecretString::from("synthetic-provider-token"),
        &key,
        &ratatoskr_github_catalog::VerifiedGithubAccount {
            provider_user_id: 7,
            login: "synthetic-owner".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;
    sqlx::raw_sql(
        "create function github_catalog.fail_desired_policy() returns trigger language plpgsql as
         $$ begin raise exception 'synthetic desired policy failure'; end $$;
         create trigger fail_desired_policy before insert or update
         on github_catalog.backup_policy_publication_cursor
         for each statement execute function github_catalog.fail_desired_policy();",
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42,
            "full_name": "owner/repository",
            "description": "A repository action.",
            "language": "Rust",
            "stargazers_count": 123,
            "topics": [],
            "default_branch": "main",
            "pushed_at": "2026-08-27T10:00:00Z"
        })))
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
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;
    let response = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &star_action_request("star-partial.018f0000-0000-7000-8000-000000000617"),
    );
    stop_process(&mut child)?;
    let response = response?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    database.cleanup().await?;

    assert_eq!(response.status, 200, "star response: {}", response.body);
    let result: serde_json::Value = serde_json::from_str(&response.body)?;
    assert_eq!(result["aggregate"], "partial");
    assert!(
        matches!(
            result["metadata"]["status"].as_str(),
            Some("succeeded" | "already_applied")
        ),
        "metadata truth was lost: {result}"
    );
    assert_eq!(result["provider_star"]["status"], "succeeded");
    assert_eq!(result["desired_backup"]["status"], "failed");
    assert_eq!(
        result["desired_backup"]["reason"],
        "policy_publication_failed"
    );
    assert_eq!(provider_requests.len(), 3);
    for request in provider_requests {
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains("removeStar"),
            "provider success was compensated"
        );
    }
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "provider refusal, dependent skip, and no-policy-write evidence form one scenario"
)]
async fn provider_refusal_skips_dependent_policy_without_fabricated_success()
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
        SecretString::from("synthetic-provider-token"),
        &key,
        &ratatoskr_github_catalog::VerifiedGithubAccount {
            provider_user_id: 7,
            login: "synthetic-owner".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42,
            "full_name": "owner/repository",
            "description": "A repository action.",
            "language": "Rust",
            "stargazers_count": 123,
            "topics": [],
            "default_branch": "main",
            "pushed_at": "2026-08-27T10:00:00Z"
        })))
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
            "data": { "addStar": { "starrable": { "viewerHasStarred": false } } },
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
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;
    let response = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &star_action_request("star-refused.018f0000-0000-7000-8000-000000000618"),
    );
    stop_process(&mut child)?;
    let response = response?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    let dirty_generation: i64 = sqlx::query_scalar(
        "select dirty_generation from github_catalog.backup_policy_publication_cursor
         where scope = 'catalog'",
    )
    .fetch_optional(database.database.pool())
    .await?
    .unwrap_or_default();
    database.cleanup().await?;

    assert_eq!(response.status, 200, "star response: {}", response.body);
    let result: serde_json::Value = serde_json::from_str(&response.body)?;
    assert_eq!(result["aggregate"], "partial");
    assert!(matches!(
        result["metadata"]["status"].as_str(),
        Some("succeeded" | "already_applied")
    ));
    assert_eq!(result["provider_star"]["status"], "failed");
    assert_eq!(result["provider_star"]["reason"], "provider_unavailable");
    assert_eq!(result["desired_backup"]["status"], "skipped");
    assert_eq!(result["desired_backup"]["reason"], "prerequisite_failed");
    assert_eq!(
        dirty_generation, 0,
        "provider failure accepted backup policy"
    );
    assert_eq!(provider_requests.len(), 3);
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "exact replay and conflicting reuse must share one real process and provider ledger"
)]
async fn exact_action_replay_returns_recorded_truth_and_conflicting_reuse_is_refused()
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
        SecretString::from("synthetic-provider-token"),
        &key,
        &ratatoskr_github_catalog::VerifiedGithubAccount {
            provider_user_id: 7,
            login: "synthetic-owner".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42,
            "full_name": "owner/repository",
            "description": "A repository action.",
            "language": "Rust",
            "stargazers_count": 123,
            "topics": [],
            "default_branch": "main",
            "pushed_at": "2026-08-27T10:00:00Z"
        })))
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
        .await?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;
    let idempotency_key = "star-replay.018f0000-0000-7000-8000-000000000619";
    let request = star_action_request(idempotency_key);
    let first = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &request,
    )?;
    let replay = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &request,
    )?;
    let mut conflict = action_request("track", idempotency_key);
    conflict["confirmation_evidence_ref"] =
        serde_json::json!("telegram-confirmation:018f0000-0000-7000-8000-000000000620");
    let conflict = http_json(
        api_address,
        "/v1/gh/repositories/actions",
        USER_ID,
        &conflict,
    );
    stop_process(&mut child)?;
    let conflict = conflict?;
    let provider_requests = provider.received_requests().await.unwrap_or_default();
    database.cleanup().await?;

    assert_eq!(first.status, 200, "first action: {}", first.body);
    assert_eq!(replay.status, 200, "replay: {}", replay.body);
    assert_eq!(first.body, replay.body, "replay rewrote recorded truth");
    assert_eq!(conflict.status, 409, "conflict: {}", conflict.body);
    let conflict_body: serde_json::Value = serde_json::from_str(&conflict.body)?;
    assert_eq!(conflict_body["code"], "github.action.idempotency_conflict");
    assert_eq!(
        provider_requests.len(),
        3,
        "replay or conflict repeated provider work"
    );
    Ok(())
}

fn action_request(mode: &str, idempotency_key: &str) -> serde_json::Value {
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

fn star_action_request(idempotency_key: &str) -> serde_json::Value {
    let mut request = action_request("star", idempotency_key);
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "account_ref".to_owned(),
            serde_json::json!(format!("github-account:{ACCOUNT_ID}")),
        );
    }
    request
}
