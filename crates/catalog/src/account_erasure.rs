//! Typed GitHub Catalog account-erasure boundary.

use ratatoskr_identifiers::{Extensions, TenantRef};
use ratatoskr_operation_contracts::{
    AccountErasureAcknowledged, AccountErasureOutcome, AccountErasureRequested,
};
use uuid::Uuid;

use crate::provider::ReqwestGithubApi;
use crate::{CredentialKey, Database, OAuthAppCredentials, PersistenceError, load_active_oauth};

/// A failure while erasing one owner's GitHub Catalog data.
#[derive(Debug, thiserror::Error)]
pub enum AccountErasureError {
    /// Removing locally owned account state failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// Erases one tenant's GitHub Catalog state for a published erasure command.
///
/// # Errors
///
/// Returns [`AccountErasureError`] when the owner cannot be erased.
pub async fn erase_account(
    database: &Database,
    github: &ReqwestGithubApi,
    credential_key: Option<&CredentialKey>,
    oauth_app: Option<&OAuthAppCredentials>,
    tenant: TenantRef,
    request: &AccountErasureRequested,
) -> Result<AccountErasureAcknowledged, AccountErasureError> {
    let owner_ref = tenant.to_string();
    let credentials: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "select credential.account_id, credential.credential_kind, credential.oauth_client_id
         from github_catalog.github_account_credentials credential
         join github_catalog.github_accounts account on account.account_id = credential.account_id
         where account.owner_ref = $1",
    )
    .bind(&owner_ref)
    .fetch_all(database.pool())
    .await
    .map_err(PersistenceError::Query)?;

    let mut outcome = AccountErasureOutcome::Verified;
    for (account_id, credential_kind, credential_client_id) in credentials {
        let matching_app = oauth_app.filter(|app| {
            credential_kind == "oauth"
                && credential_client_id.as_deref() == Some(app.client_id.as_str())
        });
        let Some(app) = matching_app else {
            outcome = AccountErasureOutcome::IncompleteExternalGrantRevocation;
            continue;
        };
        let Some(key) = credential_key else {
            outcome = AccountErasureOutcome::IncompleteExternalGrantRevocation;
            continue;
        };
        let Ok(access_token) = load_active_oauth(database, account_id, key, &app.client_id).await
        else {
            outcome = AccountErasureOutcome::IncompleteExternalGrantRevocation;
            continue;
        };
        if github.revoke_oauth_grant(app, &access_token).await.is_err() {
            outcome = AccountErasureOutcome::IncompleteExternalGrantRevocation;
        }
    }

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    delete_owner_state(&mut transaction, &owner_ref).await?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;

    Ok(AccountErasureAcknowledged {
        operation_id: request.operation_id,
        outcome,
        extensions: Extensions::default(),
    })
}

async fn delete_owner_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner_ref: &str,
) -> Result<(), PersistenceError> {
    for statement in [
        "delete from github_catalog.outbox_events where owner_ref = $1",
        "delete from github_catalog.inbox_events where owner_ref = $1",
        "delete from github_catalog.repository_analysis_links where owner_ref = $1",
        "delete from github_catalog.repository_analysis_requests where owner_ref = $1",
        "delete from github_catalog.repository_watches where owner_ref = $1",
        "delete from github_catalog.mutation_audit where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.star_list_membership_observations where list_id in (select list_id from github_catalog.star_lists where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1))",
        "delete from github_catalog.star_list_memberships where list_id in (select list_id from github_catalog.star_lists where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1))",
        "delete from github_catalog.star_lists where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.current_star_state where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.star_observations where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.legacy_list_claims where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.star_watermarks where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.reconciliation_repairs where sync_run_id in (select sync_run_id from github_catalog.sync_runs where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1))",
        "delete from github_catalog.snapshot_items where sync_run_id in (select sync_run_id from github_catalog.sync_runs where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1))",
        "delete from github_catalog.list_snapshot_items where sync_run_id in (select sync_run_id from github_catalog.sync_runs where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1))",
        "delete from github_catalog.sync_checkpoints where sync_run_id in (select sync_run_id from github_catalog.sync_runs where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1))",
        "delete from github_catalog.sync_runs where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.legacy_import_repository_records where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.legacy_import_accounts where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.github_account_credentials where account_id in (select account_id from github_catalog.github_accounts where owner_ref = $1)",
        "delete from github_catalog.github_accounts where owner_ref = $1",
    ] {
        sqlx::query(statement)
            .bind(owner_ref)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
    }
    Ok(())
}
