//! A disposable owner with one OAuth credential, shared by the revocation tests.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each test binary compiles this shared module and uses a different subset"
)]

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    Config, CredentialKey, OAuthAppCredentials, VerifiedGithubAccount, erase_account,
    register_oauth,
};
use ratatoskr_identifiers::{Extensions, OperationId, TenantRef};
use ratatoskr_operation_contracts::{AccountErasureOutcome, AccountErasureRequested};
use secrecy::SecretString;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub const ACCESS_TOKEN: &str = "oauth-access-token";
const KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

pub struct Fixture {
    pub database: TestDatabase,
    pub tenant: TenantRef,
    pub key: CredentialKey,
    pub app: OAuthAppCredentials,
}

pub async fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let tenant = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(tenant.to_string())
    .execute(database.database.pool())
    .await?;
    let app = Config::from_environment([
        ("RATATOSKR__GITHUB_OAUTH__CLIENT_ID", "Iv1.configured-app"),
        (
            "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET",
            "synthetic-oauth-client-secret",
        ),
    ])?
    .github_oauth
    .credentials()
    .ok_or("the OAuth configuration must be complete")?;
    let key = CredentialKey::from_hex(KEY_HEX, "key-2026-08")?;
    register_oauth(
        &database.database,
        account_id,
        SecretString::from(ACCESS_TOKEN),
        &key,
        &VerifiedGithubAccount {
            provider_user_id: 45,
            login: "revocation-login".to_owned(),
            granted_scopes: vec!["repo".to_owned()],
        },
        &app,
    )
    .await?;
    Ok(Fixture {
        database,
        tenant,
        key,
        app,
    })
}

impl Fixture {
    pub async fn erase(
        &self,
        github: &ReqwestGithubApi,
    ) -> Result<AccountErasureOutcome, Box<dyn std::error::Error>> {
        let request = AccountErasureRequested {
            operation_id: OperationId::new_v7(),
            extensions: Extensions::default(),
        };
        Ok(erase_account(
            &self.database.database,
            github,
            Some(&self.key),
            Some(&self.app),
            self.tenant,
            &request,
        )
        .await?
        .outcome)
    }

    pub async fn accounts_left(&self) -> Result<i64, Box<dyn std::error::Error>> {
        Ok(sqlx::query_scalar(
            "select count(*) from github_catalog.github_accounts where owner_ref = $1",
        )
        .bind(self.tenant.to_string())
        .fetch_one(self.database.database.pool())
        .await?)
    }
}

pub async fn grant_answers(server: &MockServer, status: u16) {
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.configured-app/grant"))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

pub async fn probe_answers(server: &MockServer, status: u16) {
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header(
            "authorization",
            format!("Bearer {ACCESS_TOKEN}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}
