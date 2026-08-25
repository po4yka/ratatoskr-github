//! GitHub REST provider access behind a testable seam.
//!
//! Provider response types stay inside this adapter boundary and never leak
//! into domain state unnormalized.

use serde::Deserialize;

use crate::rate_limit::RateLimitHeaders;

/// Fixed page size for starred-listing enumeration.
const STARRED_PAGE_SIZE: u32 = 100;

/// Owner and name of a repository as GitHub spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerName {
    /// The repository owner login.
    pub owner: String,
    /// The repository name.
    pub name: String,
}

impl std::fmt::Display for OwnerName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Rename evidence observed from a provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEvidence {
    /// The owner/name the provider reports for the requested alias.
    pub observed_as: OwnerName,
}

/// One fresh GitHub repository payload with its validator.
#[derive(Debug, Clone, PartialEq)]
pub struct FreshRepository {
    /// The normalized payload body.
    pub body: ProviderRepositoryBody,
    /// The validator to present on the next conditional refresh, if any.
    pub etag: Option<String>,
    /// Rename evidence when the payload declares a different `owner/name`
    /// than the alias that was requested.
    pub rename_evidence: Option<RenameEvidence>,
}

/// One provider reply: the fetch outcome plus the response's rate-limit
/// headers for shared budget accounting.
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayReply {
    /// What the fetch established.
    pub outcome: FetchOutcome,
    /// Rate-limit headers from the same response.
    pub rate_limit: RateLimitHeaders,
}

/// The observable outcomes of one repository fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchOutcome {
    /// The provider returned a fresh payload.
    Fresh(FreshRepository),
    /// The provider confirmed the stored validator without a body.
    NotModified,
    /// The provider permanently moved the alias to another `owner/name`.
    MovedPermanently {
        /// The target `owner/name` from the move location.
        target: OwnerName,
    },
}

/// One entry of the starred-repository listing: when the star was made and
/// the repository payload itself.
#[derive(Debug, Clone, PartialEq)]
pub struct StarredItem {
    /// The provider `starred_at` timestamp as supplied by the listing.
    pub starred_at: Option<String>,
    /// The normalized repository payload.
    pub repo: ProviderRepositoryBody,
}

/// One page of the starred-repository listing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StarredPage {
    /// The entries of this page; empty pages terminate enumeration.
    pub items: Vec<StarredItem>,
}

/// One starred-listing page reply with its rate-limit headers.
#[derive(Debug, Clone, PartialEq)]
pub struct ListingReply {
    /// The decoded page.
    pub page: StarredPage,
    /// Rate-limit headers from the same response.
    pub rate_limit: RateLimitHeaders,
}

/// Provider access failures, classified for callers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// The alias does not exist or is not visible.
    #[error("the repository was not found")]
    NotFound,
    /// The token lacks access to the repository.
    #[error("access to the repository was denied")]
    Unauthorized,
    /// The transport or protocol failed before a classification was possible.
    #[error("the provider exchange failed: {0}")]
    Transport(#[source] reqwest::Error),
    /// The provider answered outside every classified status family.
    #[error("the provider answered with an unexpected status: {status}")]
    UnexpectedStatus {
        /// The raw HTTP status code.
        status: u16,
    },
}

/// A normalized GitHub repository payload.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderRepositoryBody {
    /// GitHub's stable numeric repository ID.
    #[serde(rename = "id")]
    pub provider_repository_id: i64,
    /// The `owner/name` the payload declares.
    #[serde(rename = "full_name")]
    pub full_name: String,
    /// The short human description, if any.
    pub description: Option<String>,
    /// The primary language, if any.
    pub language: Option<String>,
    /// The stargazer count.
    #[serde(rename = "stargazers_count")]
    pub stargazers: i64,
    /// The topic list, empty when absent.
    #[serde(default)]
    pub topics: Vec<String>,
    /// The default branch name, if any.
    #[serde(rename = "default_branch")]
    pub default_branch: Option<String>,
    /// The last push time as an RFC 3339 string, if any.
    #[serde(rename = "pushed_at")]
    pub pushed_at: Option<String>,
}

impl ProviderRepositoryBody {
    /// The `owner/name` declared by the payload, split at the separator.
    #[must_use]
    pub fn owner_name(&self) -> Option<OwnerName> {
        let (owner, name) = self.full_name.split_once('/')?;
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(OwnerName {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// The provider seam used by catalog flows.
pub trait GithubApi {
    /// Fetches one repository conditionally.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] classifications for caller handling.
    fn fetch_repository(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        etag: Option<&str>,
    ) -> impl std::future::Future<Output = Result<GatewayReply, ProviderError>> + Send;

    /// Fetches one page of the authenticated account's starred-repository
    /// listing. Pages are numbered from one; an empty page terminates
    /// enumeration.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] classifications for caller handling.
    fn list_starred(
        &self,
        token: Option<&str>,
        page: u32,
    ) -> impl std::future::Future<Output = Result<ListingReply, ProviderError>> + Send;

    /// Fetches one page of the authenticated account's starred-repository
    /// listing ordered newest-first by star creation time, the ordering an
    /// incremental scan needs to bound its window. Pages are numbered from
    /// one; an empty page terminates enumeration.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] classifications for caller handling.
    fn list_starred_newest_first(
        &self,
        token: Option<&str>,
        page: u32,
    ) -> impl std::future::Future<Output = Result<ListingReply, ProviderError>> + Send;
}

/// Reads rate-limit headers off a response, tolerating absent values.
#[must_use]
fn rate_headers_from(headers: &reqwest::header::HeaderMap) -> RateLimitHeaders {
    let parse_i64 = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
    };
    RateLimitHeaders {
        limit: parse_i64("x-ratelimit-limit"),
        remaining: parse_i64("x-ratelimit-remaining"),
        reset_epoch_seconds: parse_i64("x-ratelimit-reset"),
        retry_after_seconds: headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok()),
    }
}

/// The reqwest-backed gateway to the GitHub REST API.
///
/// Redirect following stays disabled on purpose: a permanent move must
/// surface as [`FetchOutcome::MovedPermanently`] evidence instead of being
/// followed silently.
#[derive(Debug, Clone)]
pub struct ReqwestGithubApi {
    http: reqwest::Client,
    base_url: String,
}

impl ReqwestGithubApi {
    /// Builds a gateway against an injectable API base URL.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Transport`] when the HTTP client cannot be
    /// built.
    pub fn for_base_url(base_url: &str) -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(ProviderError::Transport)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }
}

/// Parses an owner/name out of a move target location.
///
/// Accepts absolute URLs and root-relative paths, ignoring any segments
/// beyond the repository name.
#[must_use]
fn parse_moved_location(location: &str) -> Option<OwnerName> {
    const MARKER: &str = "/repos/";
    let Some((_, after)) = location.split_once(MARKER) else {
        // Without the marker only an exact `owner/name` path is accepted.
        let trimmed = location.trim_start_matches('/');
        let (owner, rest) = trimmed.split_once('/')?;
        let name = rest.split(['?', '#']).next()?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        return Some(OwnerName {
            owner: owner.to_owned(),
            name: name.to_owned(),
        });
    };
    let tail = after.split(['?', '#']).next()?;
    let (owner, name_path) = tail.split_once('/')?;
    let name = name_path.split('/').next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(OwnerName {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// The wire format of one starred-listing entry: the star timestamp beside
/// the repository payload.
#[derive(Debug, Deserialize)]
struct StarredItemWire {
    starred_at: Option<String>,
    repo: ProviderRepositoryBody,
}

impl GithubApi for ReqwestGithubApi {
    async fn fetch_repository(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        etag: Option<&str>,
    ) -> Result<GatewayReply, ProviderError> {
        let url = format!("{}/repos/{owner}/{name}", self.base_url);
        let mut request = self.http.get(url);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let response = request.send().await.map_err(ProviderError::Transport)?;
        let rate_limit = rate_headers_from(response.headers());
        let outcome = match response.status() {
            reqwest::StatusCode::OK => {
                let etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body: ProviderRepositoryBody =
                    response.json().await.map_err(ProviderError::Transport)?;
                let rename_evidence = match body.owner_name() {
                    Some(observed_as) if observed_as.owner != owner || observed_as.name != name => {
                        Some(RenameEvidence { observed_as })
                    }
                    _ => None,
                };
                FetchOutcome::Fresh(FreshRepository {
                    body,
                    etag,
                    rename_evidence,
                })
            }
            reqwest::StatusCode::NOT_MODIFIED => FetchOutcome::NotModified,
            reqwest::StatusCode::MOVED_PERMANENTLY => {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(ProviderError::UnexpectedStatus {
                        status: reqwest::StatusCode::MOVED_PERMANENTLY.as_u16(),
                    })?;
                let target =
                    parse_moved_location(location).ok_or(ProviderError::UnexpectedStatus {
                        status: reqwest::StatusCode::MOVED_PERMANENTLY.as_u16(),
                    })?;
                FetchOutcome::MovedPermanently { target }
            }
            reqwest::StatusCode::NOT_FOUND => return Err(ProviderError::NotFound),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(ProviderError::Unauthorized);
            }
            status => {
                return Err(ProviderError::UnexpectedStatus {
                    status: status.as_u16(),
                });
            }
        };
        Ok(GatewayReply {
            outcome,
            rate_limit,
        })
    }

    async fn list_starred(
        &self,
        token: Option<&str>,
        page: u32,
    ) -> Result<ListingReply, ProviderError> {
        self.list_starred_with(token, page, &[]).await
    }

    async fn list_starred_newest_first(
        &self,
        token: Option<&str>,
        page: u32,
    ) -> Result<ListingReply, ProviderError> {
        self.list_starred_with(token, page, &[("sort", "created"), ("direction", "desc")])
            .await
    }
}

impl ReqwestGithubApi {
    /// Fetches one starred-listing page with an explicit ordering; an empty
    /// ordering leaves the provider default untouched.
    async fn list_starred_with(
        &self,
        token: Option<&str>,
        page: u32,
        ordering: &[(&'static str, &'static str)],
    ) -> Result<ListingReply, ProviderError> {
        let url = format!("{}/user/starred", self.base_url);
        let mut request = self
            .http
            .get(url)
            .query(&[("page", page), ("per_page", STARRED_PAGE_SIZE)]);
        if !ordering.is_empty() {
            request = request.query(ordering);
        }
        request = request.header("Accept", "application/vnd.github.star+json");
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(ProviderError::Transport)?;
        let rate_limit = rate_headers_from(response.headers());
        let items = match response.status() {
            reqwest::StatusCode::OK => {
                let wire: Vec<StarredItemWire> =
                    response.json().await.map_err(ProviderError::Transport)?;
                wire.into_iter()
                    .map(|entry| StarredItem {
                        starred_at: entry.starred_at,
                        repo: entry.repo,
                    })
                    .collect()
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(ProviderError::Unauthorized);
            }
            status => {
                return Err(ProviderError::UnexpectedStatus {
                    status: status.as_u16(),
                });
            }
        };
        Ok(ListingReply {
            page: StarredPage { items },
            rate_limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_moved_location;

    #[test]
    fn parses_relative_and_absolute_move_locations() {
        let relative = parse_moved_location("/repos/new-owner/new-name");
        assert_eq!(
            relative.map(|o| o.to_string()).as_deref(),
            Some("new-owner/new-name")
        );

        let absolute = parse_moved_location("https://api.github.com/repos/other/repo/issues");
        assert_eq!(
            absolute.map(|o| o.to_string()).as_deref(),
            Some("other/repo")
        );
    }

    #[test]
    fn rejects_locations_without_an_owner_name_split() {
        assert!(parse_moved_location("/just/a/path").is_none());
        assert!(parse_moved_location("").is_none());
    }
}
