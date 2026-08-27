//! Credential registration primitives.

use std::fmt;

use aes_gcm::aead::{Aead as _, Generate as _, Payload, consts::U12};
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use secrecy::{ExposeSecret as _, SecretString};
use uuid::Uuid;

use crate::Database;

/// Verified GitHub identity and scopes returned by provider authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGithubAccount {
    /// Stable numeric GitHub user identity.
    pub provider_user_id: i64,
    /// Current GitHub login.
    pub login: String,
    /// Scopes granted to the replacement credential.
    pub granted_scopes: Vec<String>,
}

/// Versioned encryption material for a catalog credential.
#[derive(Clone)]
pub struct CredentialKey {
    bytes: [u8; 32],
    version: String,
}

impl fmt::Debug for CredentialKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialKey")
            .field("version", &self.version)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

impl CredentialKey {
    /// Parses one configured AES-256 key and its non-secret version label.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::InvalidKey`] when the key is not 32 bytes of hex.
    pub fn from_hex(value: &str, version: &str) -> Result<Self, CredentialError> {
        if value.len() != 64
            || version.is_empty()
            || version.len() > 64
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(CredentialError::InvalidKey);
        }

        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let [high, low] =
                <&[u8; 2]>::try_from(pair).map_err(|_| CredentialError::InvalidKey)?;
            let high = hex_nibble(*high).ok_or(CredentialError::InvalidKey)?;
            let low = hex_nibble(*low).ok_or(CredentialError::InvalidKey)?;
            *bytes.get_mut(index).ok_or(CredentialError::InvalidKey)? = (high << 4) | low;
        }

        Ok(Self {
            bytes,
            version: version.to_owned(),
        })
    }
}

/// Credential registration failure with no secret material.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// Configured key material was malformed.
    #[error("credential encryption key is invalid")]
    InvalidKey,
    /// The account does not exist or is not awaiting reauthorization.
    #[error("account is not awaiting reauthorization")]
    AccountNotAwaitingReauthorization,
    /// Encrypting the supplied credential failed.
    #[error("credential encryption failed")]
    Encryption,
    /// No active credential exists for the requested connected account.
    #[error("active account credential is unavailable")]
    Unavailable,
    /// Decrypting stored credential material failed.
    #[error("stored account credential could not be decrypted")]
    Decryption,
    /// Persisting the credential or account state failed.
    #[error(transparent)]
    Persistence(#[from] crate::PersistenceError),
}

/// Loads one active credential for an authenticated provider call.
///
/// The plaintext remains a redacting secret value and is never logged or
/// serialized by this function.
///
/// # Errors
///
/// Returns [`CredentialError`] when no active credential exists or its
/// authenticated encryption cannot be verified.
pub async fn load_active_pat(
    database: &Database,
    account_id: Uuid,
    key: &CredentialKey,
) -> Result<SecretString, CredentialError> {
    let stored: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "select credential.encrypted_token, credential.nonce
         from github_catalog.github_account_credentials credential
         join github_catalog.github_accounts account on account.account_id = credential.account_id
         where credential.account_id = $1 and account.status = 'connected'",
    )
    .bind(account_id)
    .fetch_optional(database.pool())
    .await
    .map_err(crate::PersistenceError::Query)?;
    let (ciphertext, nonce) = stored.ok_or(CredentialError::Unavailable)?;
    let nonce =
        Nonce::<U12>::try_from(nonce.as_slice()).map_err(|_| CredentialError::Decryption)?;
    let cipher = Aes256Gcm::new_from_slice(&key.bytes).map_err(|_| CredentialError::Decryption)?;
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &ciphertext,
                aad: account_id.as_bytes(),
            },
        )
        .map_err(|_| CredentialError::Decryption)?;
    let plaintext = String::from_utf8(plaintext).map_err(|_| CredentialError::Decryption)?;
    Ok(SecretString::from(plaintext))
}

/// Registers an already provider-verified replacement PAT for one account.
///
/// # Errors
///
/// Returns [`CredentialError`] when registration cannot complete.
pub async fn register_pat(
    database: &Database,
    account_id: Uuid,
    pat: SecretString,
    key: &CredentialKey,
    verified: &VerifiedGithubAccount,
) -> Result<(), CredentialError> {
    if verified.provider_user_id <= 0 || verified.login.is_empty() {
        return Err(CredentialError::AccountNotAwaitingReauthorization);
    }
    let nonce = nonce();
    let cipher = Aes256Gcm::new_from_slice(&key.bytes).map_err(|_| CredentialError::Encryption)?;
    let encrypted_token = cipher
        .encrypt(
            &Nonce::<U12>::try_from(nonce.as_slice()).map_err(|_| CredentialError::Encryption)?,
            Payload {
                msg: pat.expose_secret().as_bytes(),
                aad: account_id.as_bytes(),
            },
        )
        .map_err(|_| CredentialError::Encryption)?;

    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(crate::PersistenceError::Query)?;
    let updated: Option<Uuid> = sqlx::query_scalar(
        "update github_catalog.github_accounts
         set status = 'connected', provider_user_id = $2, provider_login = $3,
             granted_scopes = $4
         where account_id = $1 and status = 'reauthorization_required'
         returning account_id",
    )
    .bind(account_id)
    .bind(verified.provider_user_id)
    .bind(&verified.login)
    .bind(&verified.granted_scopes)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(crate::PersistenceError::Query)?;
    if updated.is_none() {
        return Err(CredentialError::AccountNotAwaitingReauthorization);
    }
    sqlx::query(
        "insert into github_catalog.github_account_credentials
             (account_id, key_version, encrypted_token, nonce)
         values ($1, $2, $3, $4)
         on conflict (account_id) do update set
             key_version = excluded.key_version,
             encrypted_token = excluded.encrypted_token,
             nonce = excluded.nonce,
             updated_at = now()",
    )
    .bind(account_id)
    .bind(&key.version)
    .bind(encrypted_token)
    .bind(nonce.to_vec())
    .execute(&mut *transaction)
    .await
    .map_err(crate::PersistenceError::Query)?;
    transaction
        .commit()
        .await
        .map_err(crate::PersistenceError::Query)?;
    Ok(())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn nonce() -> Nonce<U12> {
    Nonce::<U12>::generate()
}
