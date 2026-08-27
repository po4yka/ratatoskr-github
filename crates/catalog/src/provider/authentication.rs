//! Replacement-PAT identity verification.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use super::{ProviderError, ReqwestGithubApi};
use crate::OAuthAppCredentials;

/// Identity and scopes verified from a replacement GitHub credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    /// Stable numeric GitHub user identity.
    pub provider_user_id: i64,
    /// Current GitHub login.
    pub login: String,
    /// Scopes GitHub reported for the credential.
    pub granted_scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthenticatedUserWire {
    id: i64,
    login: String,
}

impl ReqwestGithubApi {
    /// Revokes one user's grant for the configured OAuth application.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when GitHub does not confirm revocation.
    pub async fn revoke_oauth_grant(
        &self,
        oauth_app: &OAuthAppCredentials,
        access_token: &SecretString,
    ) -> Result<(), ProviderError> {
        let response = self
            .http
            .delete(format!(
                "{}/applications/{}/grant",
                self.base_url, oauth_app.client_id
            ))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .basic_auth(
                &oauth_app.client_id,
                Some(oauth_app.client_secret().expose_secret()),
            )
            .json(&serde_json::json!({ "access_token": access_token.expose_secret() }))
            .send()
            .await
            .map_err(ProviderError::Transport)?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(ProviderError::UnexpectedStatus {
                status: response.status().as_u16(),
            })
        }
    }

    /// Verifies a replacement PAT and reads the provider identity and scopes.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Unauthorized`] when GitHub rejects the PAT.
    pub async fn authenticate_pat(&self, token: &str) -> Result<AuthenticatedUser, ProviderError> {
        let response = self
            .http
            .get(format!("{}/user", self.base_url))
            .bearer_auth(token)
            .send()
            .await
            .map_err(ProviderError::Transport)?;
        match response.status() {
            reqwest::StatusCode::OK => {
                let scopes = response
                    .headers()
                    .get("x-oauth-scopes")
                    .and_then(|value| value.to_str().ok())
                    .map(|value| {
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|scope| !scope.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let body: AuthenticatedUserWire =
                    response.json().await.map_err(ProviderError::Transport)?;
                if body.id <= 0 || body.login.is_empty() {
                    return Err(ProviderError::UnexpectedStatus {
                        status: reqwest::StatusCode::OK.as_u16(),
                    });
                }
                Ok(AuthenticatedUser {
                    provider_user_id: body.id,
                    login: body.login,
                    granted_scopes: scopes,
                })
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(ProviderError::Unauthorized)
            }
            status => Err(ProviderError::UnexpectedStatus {
                status: status.as_u16(),
            }),
        }
    }
}
