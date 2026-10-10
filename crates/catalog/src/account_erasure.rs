//! Typed GitHub Catalog account-erasure boundary.

use std::fmt::Write as _;

use ratatoskr_identifiers::{Extensions, TenantRef};
use ratatoskr_operation_contracts::{
    AccountErasureAcknowledged, AccountErasureOutcome, AccountErasureRequested,
};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::account_erasure_state::erase_owner_state;
use crate::provider::ReqwestGithubApi;
use crate::{CredentialKey, Database, OAuthAppCredentials, PersistenceError, load_active_oauth};

/// A failure while erasing one owner's GitHub Catalog data.
#[derive(Debug, thiserror::Error)]
pub enum AccountErasureError {
    /// Removing locally owned account state failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// An erasure operation identity was already used for a different owner.
    #[error("the erasure operation id was already used for a different owner")]
    OperationOwnerMismatch,
    /// A recorded erasure outcome is not one this build understands.
    #[error("the recorded erasure outcome is not recognized")]
    UnknownRecordedOutcome,
}

/// Erases one tenant's GitHub Catalog state for a published erasure command.
///
/// The answer is recorded in the same transaction as the deletion, and a redelivery of the same
/// operation returns the recorded outcome without contacting the provider or recomputing it from
/// state that no longer exists.
///
/// # Errors
///
/// Returns [`AccountErasureError`] when the owner cannot be erased, or when the operation identity
/// was already recorded for a different owner.
pub async fn erase_account(
    database: &Database,
    github: &ReqwestGithubApi,
    credential_key: Option<&CredentialKey>,
    oauth_app: Option<&OAuthAppCredentials>,
    tenant: TenantRef,
    request: &AccountErasureRequested,
) -> Result<AccountErasureAcknowledged, AccountErasureError> {
    let owner_ref = tenant.to_string();
    let owner_digest = owner_digest(&owner_ref);
    let operation_id = request.operation_id.0;
    if let Some(recorded) = recorded_operation(database, operation_id).await? {
        return replay(request, &owner_digest, &recorded);
    }

    let outcome = revoke_grants(database, github, credential_key, oauth_app, &owner_ref).await?;

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    erase_owner_state(&mut transaction, &owner_ref).await?;
    let inserted = sqlx::query(
        "insert into github_catalog.account_erasure_operations (operation_id, owner_digest, outcome)
         values ($1, $2, $3) on conflict (operation_id) do nothing",
    )
    .bind(operation_id)
    .bind(&owner_digest)
    .bind(outcome_label(outcome))
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if inserted == 0 {
        // A concurrent delivery of the same operation committed first. Its deletion is the same
        // deletion, so this one rolls back and converges on the recorded answer.
        transaction
            .rollback()
            .await
            .map_err(PersistenceError::Query)?;
        let recorded = recorded_operation(database, operation_id)
            .await?
            .ok_or(PersistenceError::Query(sqlx::Error::RowNotFound))?;
        return replay(request, &owner_digest, &recorded);
    }
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;

    Ok(acknowledgement(request, outcome))
}

/// One answered erasure: the owner digest it was answered for and the outcome label.
struct RecordedOperation {
    owner_digest: String,
    outcome: String,
}

fn owner_digest(owner_ref: &str) -> String {
    Sha256::digest(owner_ref.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // Writing to a String cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

const fn outcome_label(outcome: AccountErasureOutcome) -> &'static str {
    match outcome {
        AccountErasureOutcome::Verified => "verified",
        _ => "incomplete_external_grant_revocation",
    }
}

fn acknowledgement(
    request: &AccountErasureRequested,
    outcome: AccountErasureOutcome,
) -> AccountErasureAcknowledged {
    AccountErasureAcknowledged {
        operation_id: request.operation_id,
        outcome,
        extensions: Extensions::default(),
    }
}

async fn recorded_operation(
    database: &Database,
    operation_id: Uuid,
) -> Result<Option<RecordedOperation>, PersistenceError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "select owner_digest, outcome from github_catalog.account_erasure_operations
         where operation_id = $1",
    )
    .bind(operation_id)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(row.map(|(owner_digest, outcome)| RecordedOperation {
        owner_digest,
        outcome,
    }))
}

fn replay(
    request: &AccountErasureRequested,
    owner_digest: &str,
    recorded: &RecordedOperation,
) -> Result<AccountErasureAcknowledged, AccountErasureError> {
    if recorded.owner_digest != owner_digest {
        return Err(AccountErasureError::OperationOwnerMismatch);
    }
    let outcome = match recorded.outcome.as_str() {
        "verified" => AccountErasureOutcome::Verified,
        "incomplete_external_grant_revocation" => {
            AccountErasureOutcome::IncompleteExternalGrantRevocation
        }
        _ => return Err(AccountErasureError::UnknownRecordedOutcome),
    };
    Ok(acknowledgement(request, outcome))
}

/// Revokes every OAuth grant the owner's stored credentials hold. A credential that cannot be
/// revoked, or whose grant cannot be confirmed, makes the outcome incomplete; it never stops the
/// local erasure.
async fn revoke_grants(
    database: &Database,
    github: &ReqwestGithubApi,
    credential_key: Option<&CredentialKey>,
    oauth_app: Option<&OAuthAppCredentials>,
    owner_ref: &str,
) -> Result<AccountErasureOutcome, PersistenceError> {
    let credentials: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "select credential.account_id, credential.credential_kind, credential.oauth_client_id
         from github_catalog.github_account_credentials credential
         join github_catalog.github_accounts account on account.account_id = credential.account_id
         where account.owner_ref = $1",
    )
    .bind(owner_ref)
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
    Ok(outcome)
}
