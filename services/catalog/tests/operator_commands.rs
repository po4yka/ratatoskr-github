//! Operator command boundary tests.

use std::io::Write as _;
use std::process::{Command, Stdio};

use ratatoskr_github_catalog_service::{OperatorCommand, parse_operator_command};
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[test]
fn dead_letter_requeue_accepts_only_one_exact_message_identity() {
    let message_id = Uuid::now_v7();
    assert_eq!(
        parse_operator_command([
            "catalog".to_owned(),
            "requeue-dead-letter".to_owned(),
            "--message-id".to_owned(),
            message_id.to_string(),
        ])
        .expect("exact command"),
        OperatorCommand::RequeueDeadLetter {
            message_id: message_id.to_string(),
        }
    );
    assert!(
        parse_operator_command([
            "catalog".to_owned(),
            "requeue-dead-letter".to_owned(),
            "--message-id".to_owned(),
            message_id.to_string(),
            "extra".to_owned(),
        ])
        .is_err()
    );
}

#[test]
fn legacy_commands_reject_secret_bearing_arguments_and_unapproved_activation()
-> Result<(), Box<dyn std::error::Error>> {
    let supplied_token = "synthetic-pat-must-not-be-accepted";
    let output = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args(["check-config", "--pat", supplied_token])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(
        !output.status.success(),
        "token-like CLI input was accepted"
    );
    assert!(
        !stderr.contains(supplied_token),
        "diagnostic echoed token input"
    );
    let source_url = "postgres://synthetic-source-must-not-be-accepted";
    let output = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args(["import-legacy", "--source-url", source_url])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        !output.status.success(),
        "source URL CLI input was accepted"
    );
    assert!(!stderr.contains(source_url), "diagnostic echoed source URL");
    let unapproved_activation = "synthetic-unapproved-activation";
    let output = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args([
            "activate-legacy-cutover",
            "--approval",
            unapproved_activation,
        ])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "cutover activation was accepted");
    assert!(
        !stderr.contains(unapproved_activation),
        "diagnostic echoed unapproved activation material"
    );
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only database location is not process configuration"
)]
fn test_database_url(database_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let admin_url = std::env::var("GITHUB_CATALOG_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://github:github@127.0.0.1:5435/github".to_owned());
    let (server, _) = admin_url
        .rsplit_once('/')
        .ok_or("invalid test database URL")?;
    Ok(format!("{server}/{database_name}"))
}

#[tokio::test]
async fn reconnect_pat_uses_the_configured_provider_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'user:018f0000-0000-7000-8000-000000000901', 'reauthorization_required')",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;

    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", "Bearer synthetic-reconnect-pat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": 901, "login": "fixture-owner"}))
                .insert_header("x-oauth-scopes", "repo"),
        )
        .expect(1)
        .mount(&provider)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args(["reconnect-pat", &account_id.to_string()])
        .env("RATATOSKR__STORAGE__DATABASE_URL", database_url)
        .env("RATATOSKR__PROVIDER__BASE_URL", provider.uri())
        .env("RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX", KEY_HEX)
        .env("RATATOSKR__CREDENTIALS__KEY_VERSION", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing child stdin")?
        .write_all(b"synthetic-reconnect-pat\n")?;
    let output = child.wait_with_output()?;

    assert!(
        output.status.success(),
        "reconnect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: String = sqlx::query_scalar(
        "select status from github_catalog.github_accounts where account_id = $1",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(status, "connected");
    provider.verify().await;
    database.cleanup().await?;
    Ok(())
}
