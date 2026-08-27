//! Credential-bound provider gateway with deliberately redacted debug output.

use secrecy::{ExposeSecret as _, SecretString};

use super::{
    GatewayReply, GithubApi, ListingReply, ProviderError, ReadmeReply, ReqwestGithubApi,
    UserListsReply,
};

/// A provider gateway bound to one redacting credential for sync operations.
pub struct AuthenticatedGithubApi {
    pub(super) gateway: ReqwestGithubApi,
    pub(super) credential: SecretString,
}

impl std::fmt::Debug for AuthenticatedGithubApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthenticatedGithubApi([REDACTED])")
    }
}

impl GithubApi for AuthenticatedGithubApi {
    fn fetch_repository(
        &self,
        _token: Option<&str>,
        owner: &str,
        name: &str,
        etag: Option<&str>,
    ) -> impl std::future::Future<Output = Result<GatewayReply, ProviderError>> + Send {
        self.gateway
            .fetch_repository(Some(self.credential.expose_secret()), owner, name, etag)
    }

    fn fetch_readme(
        &self,
        _token: Option<&str>,
        owner: &str,
        name: &str,
        etag: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ReadmeReply, ProviderError>> + Send {
        self.gateway
            .fetch_readme(Some(self.credential.expose_secret()), owner, name, etag)
    }

    fn list_starred(
        &self,
        _token: Option<&str>,
        page: u32,
    ) -> impl std::future::Future<Output = Result<ListingReply, ProviderError>> + Send {
        self.gateway
            .list_starred(Some(self.credential.expose_secret()), page)
    }

    fn list_starred_newest_first(
        &self,
        _token: Option<&str>,
        page: u32,
    ) -> impl std::future::Future<Output = Result<ListingReply, ProviderError>> + Send {
        self.gateway
            .list_starred_newest_first(Some(self.credential.expose_secret()), page)
    }

    fn list_user_lists(
        &self,
        _token: Option<&str>,
        cursor: Option<&str>,
    ) -> impl std::future::Future<Output = Result<UserListsReply, ProviderError>> + Send {
        self.gateway
            .list_user_lists(Some(self.credential.expose_secret()), cursor)
    }
}
