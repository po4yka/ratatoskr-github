//! Operator command boundary tests, and the provider origin the commands honour (XR-021
//! CONTRACTS.md R4-09).

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{Config, VerifiedGithubAccount, register_pat};
use secrecy::SecretString;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const REPLACEMENT_PAT: &str = "synthetic-replacement-pat-value";
const SOURCE_ID: &str = "archive-2026-08";

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

/// Runs one operator command of the real binary against a disposable database and a provider
/// origin, with `input` on standard input.
fn run_operator_command(
    arguments: &[&str],
    database_url: &str,
    provider_origin: &str,
    input: &str,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args(arguments)
        .env("RATATOSKR__STORAGE__DATABASE_URL", database_url)
        .env("RATATOSKR__PROVIDER__BASE_URL", provider_origin)
        .env("RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX", KEY_HEX)
        .env("RATATOSKR__CREDENTIALS__KEY_VERSION", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("standard input was not piped")?
        .write_all(input.as_bytes())?;
    Ok(child.wait_with_output()?)
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

/// A disposable database holding one imported account that awaits reauthorization.
async fn database_with_imported_account()
-> Result<(TestDatabase, String, Uuid), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'user:018f0000-0000-7000-8000-000000000e01', 'reauthorization_required')",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query(
        "insert into github_catalog.legacy_import_accounts (source_id, legacy_user_id, account_id)
         values ($1, 101, $2)",
    )
    .bind(SOURCE_ID)
    .bind(account_id)
    .execute(database.database.pool())
    .await?;
    let database_url = test_database_url(&database_name)?;
    Ok((database, database_url, account_id))
}

async fn mount_identity(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header(
            "authorization",
            format!("Bearer {REPLACEMENT_PAT}").as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"id": 4242, "login": "verified-login"}"#)
                .insert_header("x-oauth-scopes", "repo, read:user"),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_pat_verifies_the_pat_against_the_configured_provider_origin() -> TestResult {
    let provider = MockServer::start().await;
    mount_identity(&provider).await;
    let (database, database_url, account_id) = database_with_imported_account().await?;

    let output = run_operator_command(
        &["reconnect-pat", &account_id.to_string()],
        &database_url,
        &provider.uri(),
        REPLACEMENT_PAT,
    )?;

    assert!(
        output.status.success(),
        "reconnect-pat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: String = sqlx::query_scalar(
        "select status from github_catalog.github_accounts where account_id = $1",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        status, "connected",
        "the verified PAT reconnects the account"
    );
    provider.verify().await;
    database.cleanup().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shadow_legacy_snapshots_through_the_configured_provider_origin() -> TestResult {
    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(header(
            "authorization",
            format!("Bearer {REPLACEMENT_PAT}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1..)
        .mount(&provider)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header(
            "authorization",
            format!("Bearer {REPLACEMENT_PAT}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":{"viewer":{"lists":{"pageInfo":{"hasNextPage":false,"endCursor":null},"edges":[]}}}}"#,
        ))
        .expect(1..)
        .mount(&provider)
        .await;
    let (database, database_url, account_id) = database_with_imported_account().await?;
    // The account is connected through the library, so this test depends only on the shadow
    // command's own provider origin and not on the reconnect command.
    let configuration = Config::from_environment([
        ("RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX", KEY_HEX),
        ("RATATOSKR__CREDENTIALS__KEY_VERSION", "test-key"),
    ])?;
    register_pat(
        &database.database,
        account_id,
        SecretString::from(REPLACEMENT_PAT.to_owned()),
        &configuration.credentials.encryption_key()?,
        &VerifiedGithubAccount {
            provider_user_id: 4242,
            login: "verified-login".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;

    let output = run_operator_command(
        &["shadow-legacy", "--source-id", SOURCE_ID],
        &database_url,
        &provider.uri(),
        "",
    )?;

    assert!(
        output.status.success(),
        "shadow-legacy failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    provider.verify().await;
    database.cleanup().await?;
    Ok(())
}
