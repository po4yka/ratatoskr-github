//! Replacement-PAT identity verification.

use serde::Deserialize;

use super::{ProviderError, ReqwestGithubApi};

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
