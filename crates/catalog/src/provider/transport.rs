//! The bounded HTTP client behind the provider gateway (XR-021 CONTRACTS.md R2-08 d).

use std::time::Duration;

use super::{ProviderError, ReqwestGithubApi};

/// Longest wait to establish a provider connection; fixed, not configurable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default longest wait for one whole provider exchange.
const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

fn client(total: Duration) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(total)
        .build()
        .map_err(ProviderError::Transport)
}

impl ReqwestGithubApi {
    /// Builds a gateway against an injectable API base URL. Every request is bounded by a
    /// connect timeout of [`CONNECT_TIMEOUT`] and a total timeout of [`DEFAULT_TOTAL_TIMEOUT`];
    /// a request that exceeds its bound fails as [`ProviderError::Transport`].
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Transport`] when the HTTP client cannot be
    /// built.
    pub fn for_base_url(base_url: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            http: client(DEFAULT_TOTAL_TIMEOUT)?,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Rebuilds the gateway with another total timeout for every request.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Transport`] when the HTTP client cannot be built.
    pub fn with_timeout(mut self, total: Duration) -> Result<Self, ProviderError> {
        self.http = client(total)?;
        Ok(self)
    }
}
