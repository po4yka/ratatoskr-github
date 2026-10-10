//! Shared seeding and erasure helpers for the account-erasure integration tests.

#![allow(
    dead_code,
    unreachable_pub,
    missing_docs,
    reason = "each erasure test binary compiles this shared module and uses a different subset"
)]

use ratatoskr_github_catalog::erase_account;
use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_contracts::{
    ReadmeAbsenceReason, ReadmeRevision, RepositoryAnalysisAttributes, RepositoryAnalysisContract,
    RepositoryAnalysisRequested, RepositoryAnalysisRevision, RepositoryFullName,
};
use ratatoskr_identifiers::{
    ContentDigest, DigestAlgorithm, DigestHex, Extensions, OperationId,
    RepositoryAnalysisRequestId, RepositoryId, TenantRef,
};
use ratatoskr_operation_contracts::{AccountErasureOutcome, AccountErasureRequested};
use uuid::Uuid;

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub struct Owner {
    pub tenant: TenantRef,
    pub reference: String,
    pub account_id: Uuid,
}

pub async fn owner(database: &TestDatabase) -> Result<Owner, Box<dyn std::error::Error>> {
    let tenant = TenantRef::parse(&format!("user:{}", Uuid::now_v7()))?;
    let reference = tenant.to_string();
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, $2, 'reauthorization_required')",
    )
    .bind(account_id)
    .bind(&reference)
    .execute(database.database.pool())
    .await?;
    Ok(Owner {
        tenant,
        reference,
        account_id,
    })
}

pub async fn repository(
    database: &TestDatabase,
    provider_id: i64,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let repository_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.repositories (repository_id, provider_repository_id, mode)
         values ($1, $2, 'auto')",
    )
    .bind(repository_id)
    .bind(provider_id)
    .execute(database.database.pool())
    .await?;
    Ok(repository_id)
}

pub fn digest(hex: &str) -> Result<ContentDigest, Box<dyn std::error::Error>> {
    Ok(ContentDigest {
        algorithm: DigestAlgorithm::Sha256,
        hex: DigestHex::parse(hex)?,
    })
}

pub fn requested(
    owner: &Owner,
    repository_id: Uuid,
    numeric_id: u64,
) -> Result<RepositoryAnalysisRequested, Box<dyn std::error::Error>> {
    Ok(RepositoryAnalysisRequested {
        owner: owner.tenant,
        repository_id: RepositoryId::parse(&repository_id.to_string())?,
        github_repository_numeric_id: numeric_id,
        request_id: RepositoryAnalysisRequestId::new_v7(),
        source_revision: RepositoryAnalysisRevision {
            attributes_digest: digest(DIGEST_A)?,
            readme: ReadmeRevision::Absent {
                reason: ReadmeAbsenceReason::NotFound,
            },
        },
        repository_attributes: RepositoryAnalysisAttributes {
            repository_full_name: RepositoryFullName::parse("acme/erased")?,
            description: None,
            primary_language: None,
        },
        requested_contract: RepositoryAnalysisContract::RepositoryAnalysis,
        idempotency_key: digest(DIGEST_A)?,
        extensions: Extensions::new(),
    })
}

pub async fn insert_event(
    database: &TestDatabase,
    table: &str,
    subject: &str,
    payload: &serde_json::Value,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let message_id = Uuid::now_v7();
    sqlx::query(&format!(
        "insert into github_catalog.{table} (message_id, subject, payload) values ($1, $2, $3)"
    ))
    .bind(message_id)
    .bind(subject)
    .bind(payload)
    .execute(database.database.pool())
    .await?;
    Ok(message_id)
}

pub async fn attempt(
    database: &TestDatabase,
    owner: &Owner,
    numeric_id: i64,
    mode: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "insert into github_catalog.repository_action_attempts
             (owner_ref, idempotency_key, request_fingerprint, mode, github_repository_numeric_id,
              repository_full_name, canonical_url, confirmation_evidence_ref)
         values ($1, $2, '{}'::jsonb, $3, $4, 'acme/erased', 'https://github.com/acme/erased', 'telegram-confirmation:x')",
    )
    .bind(&owner.reference)
    .bind(format!("key.{}", Uuid::now_v7()))
    .bind(mode)
    .bind(numeric_id)
    .execute(database.database.pool())
    .await?;
    Ok(())
}

pub async fn count(database: &TestDatabase, sql: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(sql)
        .fetch_one(database.database.pool())
        .await?)
}

pub async fn erase(
    database: &TestDatabase,
    owner: &Owner,
) -> Result<AccountErasureOutcome, Box<dyn std::error::Error>> {
    let github = ReqwestGithubApi::for_base_url("http://127.0.0.1:9")?;
    let request = AccountErasureRequested {
        operation_id: OperationId::new_v7(),
        extensions: Extensions::default(),
    };
    Ok(erase_account(
        &database.database,
        &github,
        None,
        None,
        owner.tenant,
        &request,
    )
    .await?
    .outcome)
}
