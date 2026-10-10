//! GitHub Catalog owner-erasure behavior.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AccountErasureError, Config, CredentialKey, VerifiedGithubAccount, erase_account,
    register_oauth, register_pat,
};
use ratatoskr_identifiers::{Extensions, OperationId, TenantRef};
use ratatoskr_operation_contracts::{AccountErasureOutcome, AccountErasureRequested};
use secrecy::SecretString;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{basic_auth, body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod erasure_support;

#[tokio::test]
async fn github_owner_erasure_revokes_matching_grant_then_removes_all_owner_state()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let tenant = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;
    let owner_ref = tenant.to_string();
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(&owner_ref)
    .execute(database.database.pool())
    .await?;
    let seeded = seed_owner_event_state(&database, &tenant, &owner_ref).await?;

    let configuration = Config::from_environment([
        ("RATATOSKR__GITHUB_OAUTH__CLIENT_ID", "Iv1.configured-app"),
        (
            "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET",
            "synthetic-oauth-client-secret",
        ),
    ])?;
    let oauth_app = configuration.github_oauth.credentials().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configured OAuth app must expose credentials",
        )
    })?;
    let key = CredentialKey::from_hex(
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        "key-2026-08",
    )?;
    register_oauth(
        &database.database,
        account_id,
        SecretString::from("oauth-access-token"),
        &key,
        &VerifiedGithubAccount {
            provider_user_id: 42,
            login: "verified-login".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
        &oauth_app,
    )
    .await?;

    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.configured-app/grant"))
        .and(basic_auth(
            "Iv1.configured-app",
            "synthetic-oauth-client-secret",
        ))
        .and(header("accept", "application/vnd.github+json"))
        .and(body_json(json!({ "access_token": "oauth-access-token" })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };

    let erased = erase_account(
        &database.database,
        &github,
        Some(&key),
        Some(&oauth_app),
        tenant,
        &request,
    )
    .await;

    assert!(erased.is_ok(), "matching OAuth erasure must complete");
    let acknowledgement = erased?;
    assert_eq!(acknowledgement.operation_id, request.operation_id);
    assert_eq!(acknowledgement.outcome, AccountErasureOutcome::Verified);
    let remaining: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.github_accounts where owner_ref = $1",
    )
    .bind(&owner_ref)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        remaining, 0,
        "owner account and its credential must be gone"
    );
    for (table, message_id) in seeded {
        let remaining: i64 = sqlx::query_scalar(&format!(
            "select count(*) from github_catalog.{table} where message_id = $1"
        ))
        .bind(message_id)
        .fetch_one(database.database.pool())
        .await?;
        assert_eq!(remaining, 0, "owner-keyed {table} state must be gone");
    }
    server.verify().await;

    database.cleanup().await?;
    Ok(())
}

/// Seeds one outbox row and one inbox row in the shapes the real producers write.
async fn seed_owner_event_state(
    database: &TestDatabase,
    tenant: &TenantRef,
    owner_ref: &str,
) -> Result<Vec<(&'static str, Uuid)>, Box<dyn std::error::Error>> {
    let owner = erasure_support::Owner {
        tenant: *tenant,
        reference: owner_ref.to_owned(),
        account_id: Uuid::now_v7(),
    };
    let repository_id = erasure_support::repository(database, 953_001).await?;
    let request =
        serde_json::to_value(erasure_support::requested(&owner, repository_id, 953_001)?)?;
    let outbox = erasure_support::insert_event(
        database,
        "outbox_events",
        "evt.knowledge.repository_analysis.requested.v1",
        &request,
    )
    .await?;
    let inbox = erasure_support::insert_event(
        database,
        "inbox_events",
        "github.sync.requested.v1",
        &json!({"tenant_id": owner_ref, "payload": {"account": owner_ref}}),
    )
    .await?;
    Ok(vec![("outbox_events", outbox), ("inbox_events", inbox)])
}

#[tokio::test]
async fn pat_erasure_does_not_call_github_oauth_grant_revocation()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let tenant = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;
    let owner_ref = tenant.to_string();
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(&owner_ref)
    .execute(database.database.pool())
    .await?;

    let key = CredentialKey::from_hex(
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        "key-2026-08",
    )?;
    register_pat(
        &database.database,
        account_id,
        SecretString::from("personal-access-token"),
        &key,
        &VerifiedGithubAccount {
            provider_user_id: 43,
            login: "pat-login".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
    )
    .await?;

    let server = MockServer::start().await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };

    let acknowledgement = erase_account(
        &database.database,
        &github,
        Some(&key),
        None,
        tenant,
        &request,
    )
    .await?;

    assert_eq!(
        acknowledgement.outcome,
        AccountErasureOutcome::IncompleteExternalGrantRevocation,
        "a PAT has no OAuth application grant to revoke"
    );
    let remaining: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.github_accounts where owner_ref = $1",
    )
    .bind(&owner_ref)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(remaining, 0, "a PAT account must still be erased locally");
    server.verify().await;

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn failed_oauth_grant_revocation_reports_incomplete_after_local_erasure()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let tenant = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;
    let owner_ref = tenant.to_string();
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(&owner_ref)
    .execute(database.database.pool())
    .await?;

    let configuration = Config::from_environment([
        ("RATATOSKR__GITHUB_OAUTH__CLIENT_ID", "Iv1.configured-app"),
        (
            "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET",
            "synthetic-oauth-client-secret",
        ),
    ])?;
    let oauth_app = configuration
        .github_oauth
        .credentials()
        .ok_or_else(|| std::io::Error::other("OAuth configuration must be complete"))?;
    let key = CredentialKey::from_hex(
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        "key-2026-08",
    )?;
    register_oauth(
        &database.database,
        account_id,
        SecretString::from("oauth-access-token"),
        &key,
        &VerifiedGithubAccount {
            provider_user_id: 44,
            login: "failed-revocation-login".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
        &oauth_app,
    )
    .await?;

    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.configured-app/grant"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };

    let acknowledgement = erase_account(
        &database.database,
        &github,
        Some(&key),
        Some(&oauth_app),
        tenant,
        &request,
    )
    .await?;

    assert_eq!(
        acknowledgement.outcome,
        AccountErasureOutcome::IncompleteExternalGrantRevocation,
        "provider refusal must remain visible after local erasure"
    );
    let remaining: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.github_accounts where owner_ref = $1",
    )
    .bind(&owner_ref)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(remaining, 0, "a refused grant must not retain local state");
    server.verify().await;

    database.cleanup().await?;
    Ok(())
}

const OAUTH_KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

struct OAuthFixture {
    database: TestDatabase,
    tenant: TenantRef,
    owner_ref: String,
    key: CredentialKey,
    app: ratatoskr_github_catalog::OAuthAppCredentials,
}

async fn oauth_fixture(provider_user_id: i64) -> Result<OAuthFixture, Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let tenant = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;
    let owner_ref = tenant.to_string();
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(&owner_ref)
    .execute(database.database.pool())
    .await?;
    let configuration = Config::from_environment([
        ("RATATOSKR__GITHUB_OAUTH__CLIENT_ID", "Iv1.configured-app"),
        (
            "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET",
            "synthetic-oauth-client-secret",
        ),
    ])?;
    let app = configuration
        .github_oauth
        .credentials()
        .ok_or_else(|| std::io::Error::other("OAuth configuration must be complete"))?;
    let key = CredentialKey::from_hex(OAUTH_KEY_HEX, "key-2026-08")?;
    register_oauth(
        &database.database,
        account_id,
        SecretString::from("oauth-access-token"),
        &key,
        &VerifiedGithubAccount {
            provider_user_id,
            login: format!("login-{provider_user_id}"),
            granted_scopes: vec!["repo".to_owned()],
        },
        &app,
    )
    .await?;
    Ok(OAuthFixture {
        database,
        tenant,
        owner_ref,
        key,
        app,
    })
}

async fn revoke_answering(
    status: u16,
    expected_calls: u64,
) -> Result<MockServer, wiremock::MockServer> {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.configured-app/grant"))
        .respond_with(ResponseTemplate::new(status))
        .expect(expected_calls)
        .mount(&server)
        .await;
    Ok(server)
}

#[tokio::test]
async fn redelivered_erasure_returns_the_recorded_outcome() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = oauth_fixture(61).await?;
    let server = revoke_answering(500, 1).await.map_err(|_| "mock server")?;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };

    let first = erase_account(
        &fixture.database.database,
        &github,
        Some(&fixture.key),
        Some(&fixture.app),
        fixture.tenant,
        &request,
    )
    .await?;
    let second = erase_account(
        &fixture.database.database,
        &github,
        Some(&fixture.key),
        Some(&fixture.app),
        fixture.tenant,
        &request,
    )
    .await?;

    assert_eq!(
        first.outcome,
        AccountErasureOutcome::IncompleteExternalGrantRevocation
    );
    assert_eq!(
        second.outcome, first.outcome,
        "a redelivery must return the recorded outcome, not recompute it from surviving state"
    );
    assert_eq!(second.operation_id, request.operation_id);
    server.verify().await;
    let ledger: Vec<(String,)> =
        sqlx::query_as("select outcome from github_catalog.account_erasure_operations")
            .fetch_all(fixture.database.database.pool())
            .await?;
    assert_eq!(
        ledger,
        vec![("incomplete_external_grant_revocation".to_owned(),)]
    );
    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_reused_operation_id_for_a_different_owner_is_a_mismatch()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = oauth_fixture(62).await?;
    let server = MockServer::start().await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };
    erase_account(
        &fixture.database.database,
        &github,
        None,
        None,
        fixture.tenant,
        &request,
    )
    .await?;
    let stranger = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;

    let reused = erase_account(
        &fixture.database.database,
        &github,
        None,
        None,
        stranger,
        &request,
    )
    .await;

    assert!(
        matches!(reused, Err(AccountErasureError::OperationOwnerMismatch)),
        "a reused operation id must be refused, got {reused:?}"
    );
    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn revoke_treats_404_as_already_revoked() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = oauth_fixture(63).await?;
    let server = revoke_answering(404, 1).await.map_err(|_| "mock server")?;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };

    let acknowledgement = erase_account(
        &fixture.database.database,
        &github,
        Some(&fixture.key),
        Some(&fixture.app),
        fixture.tenant,
        &request,
    )
    .await?;

    assert_eq!(acknowledgement.outcome, AccountErasureOutcome::Verified);
    server.verify().await;
    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn ledger_row_commits_atomically_with_owner_data() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = oauth_fixture(64).await?;
    sqlx::query(
        "create function github_catalog.fail_after_ledger() returns trigger language plpgsql as
         $$ begin raise exception 'injected failure after the ledger insert'; end $$",
    )
    .execute(fixture.database.database.pool())
    .await?;
    sqlx::query(
        "create trigger fail_after_ledger after insert on github_catalog.account_erasure_operations
         for each row execute function github_catalog.fail_after_ledger()",
    )
    .execute(fixture.database.database.pool())
    .await?;
    let server = revoke_answering(204, 1).await.map_err(|_| "mock server")?;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };

    let result = erase_account(
        &fixture.database.database,
        &github,
        Some(&fixture.key),
        Some(&fixture.app),
        fixture.tenant,
        &request,
    )
    .await;

    assert!(
        result.is_err(),
        "the injected failure must fail the erasure"
    );
    let accounts: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.github_accounts where owner_ref = $1",
    )
    .bind(&fixture.owner_ref)
    .fetch_one(fixture.database.database.pool())
    .await?;
    let ledger: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.account_erasure_operations")
            .fetch_one(fixture.database.database.pool())
            .await?;
    assert_eq!(
        accounts, 1,
        "the owner data deletion must roll back with the ledger"
    );
    assert_eq!(ledger, 0, "the ledger row must roll back with the deletion");
    fixture.database.cleanup().await?;
    Ok(())
}
