//! Account re-registration and credential secrecy behavior.

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    CredentialKey, VerifiedGithubAccount, load_active_pat, register_pat,
};
use secrecy::{ExposeSecret as _, SecretString};
use uuid::Uuid;

#[test]
fn credential_key_debug_redacts_key_material() -> Result<(), Box<dyn std::error::Error>> {
    let key = CredentialKey::from_hex(
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "key-2026-08",
    )?;

    let rendered = format!("{key:?}");

    assert!(!rendered.contains("255"));
    assert!(rendered.contains("key-2026-08"));
    Ok(())
}

#[tokio::test]
async fn imported_account_requires_reauthorization_and_has_no_credential()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'imported-owner', 'reauthorization_required')",
    )
    .bind(account_id)
    .execute(database.database.pool())
    .await?;

    let status: String = sqlx::query_scalar(
        "select status from github_catalog.github_accounts where account_id = $1",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    let credential_table: Option<String> =
        sqlx::query_scalar("select to_regclass('github_catalog.github_account_credentials')::text")
            .fetch_one(database.database.pool())
            .await?;

    assert_eq!(status, "reauthorization_required");
    assert_eq!(
        credential_table.as_deref(),
        Some("github_catalog.github_account_credentials"),
        "the current schema must provide credential storage without putting one on an imported account"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn valid_pat_reconnects_only_the_matching_imported_account()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let matching_account = Uuid::now_v7();
    let other_account = Uuid::now_v7();
    for (account_id, owner_ref) in [
        (matching_account, "imported-owner"),
        (other_account, "other-imported-owner"),
    ] {
        sqlx::query(
            "insert into github_catalog.github_accounts (account_id, owner_ref, status)
             values ($1, $2, 'reauthorization_required')",
        )
        .bind(account_id)
        .bind(owner_ref)
        .execute(database.database.pool())
        .await?;
    }

    let key = CredentialKey::from_hex(
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        "key-2026-08",
    )?;
    register_pat(
        &database.database,
        matching_account,
        SecretString::from("replacement-pat-value"),
        &key,
        &VerifiedGithubAccount {
            provider_user_id: 42,
            login: "verified-login".to_owned(),
            granted_scopes: vec!["repo".to_owned(), "read:user".to_owned()],
        },
    )
    .await?;

    let matching: (String, Option<i64>, Option<String>, Vec<String>) = sqlx::query_as(
        "select status, provider_user_id, provider_login, granted_scopes
         from github_catalog.github_accounts where account_id = $1",
    )
    .bind(matching_account)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(matching.0, "connected");
    assert_eq!(matching.1, Some(42));
    assert_eq!(matching.2.as_deref(), Some("verified-login"));
    assert_eq!(matching.3, ["repo", "read:user"]);

    let other_status: String = sqlx::query_scalar(
        "select status from github_catalog.github_accounts where account_id = $1",
    )
    .bind(other_account)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(other_status, "reauthorization_required");

    let stored: (String, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "select key_version, encrypted_token, nonce
         from github_catalog.github_account_credentials where account_id = $1",
    )
    .bind(matching_account)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(stored.0, "key-2026-08");
    assert_eq!(stored.2.len(), 12);
    assert_ne!(stored.1, b"replacement-pat-value");
    let loaded = load_active_pat(&database.database, matching_account, &key).await?;
    assert_eq!(loaded.expose_secret(), "replacement-pat-value");

    database.cleanup().await?;
    Ok(())
}
