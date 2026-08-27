//! GitHub Catalog owner-erasure behavior.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    Config, CredentialKey, VerifiedGithubAccount, erase_account, register_oauth, register_pat,
};
use ratatoskr_identifiers::{Extensions, OperationId, TenantRef};
use ratatoskr_operation_contracts::{AccountErasureOutcome, AccountErasureRequested};
use secrecy::SecretString;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{basic_auth, body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    insert_owner_event_state(&database, &owner_ref).await?;

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
    assert_owner_event_state_absent(&database, &owner_ref).await?;
    server.verify().await;

    database.cleanup().await?;
    Ok(())
}

async fn insert_owner_event_state(
    database: &TestDatabase,
    owner_ref: &str,
) -> Result<(), sqlx::Error> {
    for table in ["outbox_events", "inbox_events"] {
        let statement = format!(
            "insert into github_catalog.{table} (message_id, subject, payload)
             values ($1, 'github.sync.requested.v1', jsonb_build_object('account', $2))"
        );
        sqlx::query(&statement)
            .bind(Uuid::now_v7())
            .bind(owner_ref)
            .execute(database.database.pool())
            .await?;
    }
    Ok(())
}

async fn assert_owner_event_state_absent(
    database: &TestDatabase,
    owner_ref: &str,
) -> Result<(), sqlx::Error> {
    for table in ["outbox_events", "inbox_events"] {
        let statement =
            format!("select count(*) from github_catalog.{table} where payload ->> 'account' = $1");
        let remaining: i64 = sqlx::query_scalar(&statement)
            .bind(owner_ref)
            .fetch_one(database.database.pool())
            .await?;
        assert_eq!(remaining, 0, "owner-keyed {table} state must be gone");
    }
    Ok(())
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
