//! Provider mutation operations: the write-side seam over GitHub's GraphQL
//! API.
//!
//! Star writes use the documented, server-side-idempotent `addStar` and
//! `removeStar` mutations. List-membership writes use
//! `updateUserListsForItem`, present in the public GraphQL schema though
//! undocumented - the same operations the retired monolith ran in
//! production. List identity requires GraphQL node ids, so resolving a
//! repository node id from its owner/name precedes every star or list write.
//!
//! Every reply carries rate-limit accounting for the shared ledger.

use crate::provider::{
    GraphqlRateLimit, ProviderError, ReqwestGithubApi, graphql_rate_limit, rate_headers_from,
};
use crate::rate_limit::RateLimitHeaders;
use serde::Deserialize;
use serde_json::json;

/// The documented, server-side-idempotent star write. Legacy proof:
/// `addStar` on an already-starred repository succeeds.
const ADD_STAR_MUTATION: &str = "mutation($starrableId: ID!) {
  addStar(input: {starrableId: $starrableId}) {
    starrable { ... on Repository { databaseId viewerHasStarred } }
  }
}";

/// The symmetric documented removal; repeating an absent star also succeeds.
const REMOVE_STAR_MUTATION: &str = "mutation($starrableId: ID!) {
  removeStar(input: {starrableId: $starrableId}) {
    starrable { ... on Repository { databaseId viewerHasStarred } }
  }
}";

/// Resolves the repository GraphQL node id a star or list write addresses;
/// REST database ids are not accepted by these mutations.
const REPOSITORY_NODE_ID_QUERY: &str = "query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) { id }
}";

/// The undocumented-but-public-schema list-membership write the legacy
/// deployment used in production. It replaces the item's complete list set,
/// so callers must compute the full desired set before writing.
const SET_ITEM_LISTS_MUTATION: &str = "mutation($itemId: ID!, $listIds: [ID!]!) {
  updateUserListsForItem(input: {itemId: $itemId, listIds: $listIds}) {
    lists { id }
  }
}";

/// The provider seam used by authorized mutations.
pub trait MutationApi {
    /// Resolves the GraphQL node id of one repository by owner/name.
    ///
    /// # Errors
    ///
    /// Returns [`crate::provider::ProviderError`] classifications for caller
    /// handling.
    fn fetch_repository_node_id(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
    ) -> impl std::future::Future<Output = Result<NodeIdReply, crate::provider::ProviderError>> + Send;

    /// Stars one repository addressed by its GraphQL node id.
    ///
    /// # Errors
    ///
    /// Returns [`crate::provider::ProviderError`] classifications for caller
    /// handling.
    fn star_repository(
        &self,
        token: Option<&str>,
        node_id: &str,
    ) -> impl std::future::Future<Output = Result<StarWriteReply, crate::provider::ProviderError>> + Send;

    /// Removes the star from one repository addressed by its GraphQL node id.
    ///
    /// # Errors
    ///
    /// Returns [`crate::provider::ProviderError`] classifications for caller
    /// handling.
    fn unstar_repository(
        &self,
        token: Option<&str>,
        node_id: &str,
    ) -> impl std::future::Future<Output = Result<StarWriteReply, crate::provider::ProviderError>> + Send;

    /// Writes the repository's complete list set addressed by its GraphQL
    /// node id. The replacement semantics make the write naturally
    /// repeat-safe; callers own computing the desired set.
    ///
    /// # Errors
    ///
    /// Returns [`crate::provider::ProviderError`] classifications for caller
    /// handling.
    fn set_repository_lists(
        &self,
        token: Option<&str>,
        item_node_id: &str,
        list_ids: &[String],
    ) -> impl std::future::Future<Output = Result<ListWriteReply, crate::provider::ProviderError>> + Send;
}

/// One resolved repository node id with rate accounting.
#[derive(Debug, Clone)]
pub struct NodeIdReply {
    /// The GraphQL node id of the repository.
    pub node_id: String,
    /// Rate accounting captured from the reply.
    pub rate_limit: RateLimitHeaders,
}

/// The provider-confirmed star state after one star write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarWriteReply {
    /// What the provider reports about the resulting viewer star state.
    pub viewer_has_starred: bool,
    /// Rate accounting captured from the reply.
    pub rate_limit: RateLimitHeaders,
}

/// The provider-confirmed list set after one membership write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListWriteReply {
    /// The provider-confirmed complete list set for the item.
    pub lists: Vec<String>,
    /// Rate accounting captured from the reply.
    pub rate_limit: RateLimitHeaders,
}

impl MutationApi for ReqwestGithubApi {
    async fn fetch_repository_node_id(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
    ) -> Result<NodeIdReply, ProviderError> {
        let response = self
            .post_graphql_body(
                token,
                &json!({
                    "query": REPOSITORY_NODE_ID_QUERY,
                    "variables": { "owner": owner, "name": name }
                }),
            )
            .await?;
        let header_rate = rate_headers_from(response.headers());
        let envelope: NodeIdEnvelope = match response.status() {
            reqwest::StatusCode::OK => response.json().await.map_err(ProviderError::Transport)?,
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
        Ok(NodeIdReply {
            node_id: envelope.data.repository.id,
            rate_limit: graphql_rate_limit(header_rate, envelope.rate_limit.as_ref()),
        })
    }

    async fn star_repository(
        &self,
        token: Option<&str>,
        node_id: &str,
    ) -> Result<StarWriteReply, ProviderError> {
        star_write(self, token, node_id, ADD_STAR_MUTATION).await
    }

    async fn unstar_repository(
        &self,
        token: Option<&str>,
        node_id: &str,
    ) -> Result<StarWriteReply, ProviderError> {
        star_write(self, token, node_id, REMOVE_STAR_MUTATION).await
    }

    async fn set_repository_lists(
        &self,
        token: Option<&str>,
        item_node_id: &str,
        list_ids: &[String],
    ) -> Result<ListWriteReply, ProviderError> {
        let response = self
            .post_graphql_body(
                token,
                &json!({
                    "query": SET_ITEM_LISTS_MUTATION,
                    "variables": { "itemId": item_node_id, "listIds": list_ids }
                }),
            )
            .await?;
        let header_rate = rate_headers_from(response.headers());
        let envelope: UpdateListsEnvelope = match response.status() {
            reqwest::StatusCode::OK => response.json().await.map_err(ProviderError::Transport)?,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(ProviderError::Unauthorized);
            }
            status => {
                return Err(ProviderError::UnexpectedStatus {
                    status: status.as_u16(),
                });
            }
        };
        let Some(payload) = envelope.data.update_user_lists_for_item else {
            return Err(ProviderError::UnexpectedStatus { status: 200 });
        };
        Ok(ListWriteReply {
            lists: payload.lists.into_iter().map(|list| list.id).collect(),
            rate_limit: graphql_rate_limit(header_rate, envelope.rate_limit.as_ref()),
        })
    }
}

/// The wire shape of a list-membership write reply.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateListsEnvelope {
    data: UpdateListsData,
    rate_limit: Option<GraphqlRateLimit>,
}

/// The `data` member of a list-membership write reply.
#[derive(Debug, Deserialize)]
struct UpdateListsData {
    #[serde(rename = "updateUserListsForItem")]
    update_user_lists_for_item: Option<UpdatedItemLists>,
}

/// One confirmed item-lists payload.
#[derive(Debug, Deserialize)]
struct UpdatedItemLists {
    lists: Vec<UpdatedListRef>,
}

/// One confirmed list reference.
#[derive(Debug, Deserialize)]
struct UpdatedListRef {
    id: String,
}

/// The wire shape both documented star mutations share.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutationEnvelope {
    data: MutationData,
    rate_limit: Option<GraphqlRateLimit>,
}

/// The `data` member carrying whichever mutation was sent.
#[derive(Debug, Deserialize)]
struct MutationData {
    #[serde(rename = "addStar")]
    add_star: Option<StarMutationPayload>,
    #[serde(rename = "removeStar")]
    remove_star: Option<StarMutationPayload>,
}

/// One mutation payload wrapping its confirmed starrable.
#[derive(Debug, Deserialize)]
struct StarMutationPayload {
    starrable: StarrableConfirmation,
}

/// One confirmed starrable payload.
#[derive(Debug, Deserialize)]
struct StarrableConfirmation {
    #[serde(rename = "viewerHasStarred")]
    viewer_has_starred: bool,
}

impl MutationEnvelope {
    /// The confirmed resulting viewer state for the sent mutation. A reply
    /// without the confirmation payload classifies as an unclassifiable
    /// 200, mirroring how strict parsing treats malformed provider bodies.
    fn confirmation(&self, selector: MutationSelector) -> Option<bool> {
        let payload = match selector {
            MutationSelector::AddStar => self.data.add_star.as_ref()?,
            MutationSelector::RemoveStar => self.data.remove_star.as_ref()?,
        };
        Some(payload.starrable.viewer_has_starred)
    }
}

/// Which mutation direction a document carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationSelector {
    AddStar,
    RemoveStar,
}

fn selector_of(document: &str) -> MutationSelector {
    if document == REMOVE_STAR_MUTATION {
        MutationSelector::RemoveStar
    } else {
        MutationSelector::AddStar
    }
}

/// The wire shape of a repository node-id resolution reply.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeIdEnvelope {
    data: RepositoryIdData,
    rate_limit: Option<GraphqlRateLimit>,
}

/// The `data` member of a node-id resolution reply.
#[derive(Debug, Deserialize)]
struct RepositoryIdData {
    repository: RepositoryIdNode,
}

/// One resolved repository node.
#[derive(Debug, Deserialize)]
struct RepositoryIdNode {
    id: String,
}

/// Executes one documented star-direction mutation and reads the provider's
/// confirmation of the resulting viewer state.
async fn star_write(
    gateway: &ReqwestGithubApi,
    token: Option<&str>,
    node_id: &str,
    document: &'static str,
) -> Result<StarWriteReply, ProviderError> {
    let response = gateway
        .post_graphql_body(
            token,
            &json!({
                "query": document,
                "variables": { "starrableId": node_id }
            }),
        )
        .await?;
    let header_rate = rate_headers_from(response.headers());
    let envelope: MutationEnvelope = match response.status() {
        reqwest::StatusCode::OK => response.json().await.map_err(ProviderError::Transport)?,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            return Err(ProviderError::Unauthorized);
        }
        status => {
            return Err(ProviderError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }
    };
    let Some(viewer_has_starred) = envelope.confirmation(selector_of(document)) else {
        // A GraphQL-level error reply carries no usable confirmation
        // payload; it cannot be distinguished from a malformed body here.
        return Err(ProviderError::UnexpectedStatus { status: 200 });
    };
    Ok(StarWriteReply {
        viewer_has_starred,
        rate_limit: graphql_rate_limit(header_rate, envelope.rate_limit.as_ref()),
    })
}

#[cfg(test)]
mod tests {
    use super::{ADD_STAR_MUTATION, MutationApi, REMOVE_STAR_MUTATION};
    use crate::provider::{ProviderError, ReqwestGithubApi};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn star_mutation_sends_documented_add_star_operation_and_reports_provider_confirmation()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("authorization", "Bearer token-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "addStar": {
                        "starrable": { "databaseId": 990_401, "viewerHasStarred": true }
                    }
                },
                "rateLimit": { "remaining": 4_990, "resetAt": "2026-08-26T10:00:00Z" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;

        let reply = gateway
            .star_repository(Some("token-value"), "gid://repository/1")
            .await;

        let reply =
            reply.map_err(|error: ProviderError| format!("expected success, got {error}"))?;
        assert!(
            reply.viewer_has_starred,
            "the provider confirmation must carry the resulting star state"
        );
        assert_eq!(
            reply.rate_limit.remaining,
            Some(4_990),
            "the in-body rate accounting must reach the shared ledger shape"
        );

        let received = server
            .received_requests()
            .await
            .ok_or("requests were not recorded")?;
        assert_eq!(received.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&received[0].body)?;
        assert_eq!(
            body["query"].as_str(),
            Some(ADD_STAR_MUTATION),
            "the wire document must be exactly the documented addStar mutation"
        );
        assert_eq!(body["variables"]["starrableId"], "gid://repository/1");
        Ok(())
    }

    #[tokio::test]
    async fn unstar_mutation_sends_documented_remove_star_operation()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "removeStar": {
                        "starrable": { "databaseId": 990_401, "viewerHasStarred": false }
                    }
                },
                "rateLimit": { "remaining": 4_989, "resetAt": "2026-08-26T10:00:00Z" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;

        let reply = gateway
            .unstar_repository(Some("token-value"), "gid://repository/1")
            .await;

        let reply =
            reply.map_err(|error: ProviderError| format!("expected success, got {error}"))?;
        assert!(!reply.viewer_has_starred);
        assert_eq!(reply.rate_limit.remaining, Some(4_989));

        let received = server
            .received_requests()
            .await
            .ok_or("requests were not recorded")?;
        let body: serde_json::Value = serde_json::from_slice(&received[0].body)?;
        assert_eq!(
            body["query"].as_str(),
            Some(REMOVE_STAR_MUTATION),
            "the wire document must be exactly the documented removeStar mutation"
        );
        Ok(())
    }
}

#[cfg(test)]
mod set_lists_tests {
    use super::{MutationApi, SET_ITEM_LISTS_MUTATION};
    use crate::provider::ReqwestGithubApi;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn set_lists_mutation_sends_update_user_lists_for_item_with_complete_desired_set()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "updateUserListsForItem": {
                        "lists": [
                            { "id": "gid://list/1" },
                            { "id": "gid://list/2" }
                        ]
                    }
                },
                "rateLimit": { "remaining": 4_988, "resetAt": "2026-08-26T10:00:00Z" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;

        let reply = gateway
            .set_repository_lists(
                Some("token-value"),
                "gid://repository/1",
                &["gid://list/1".to_owned(), "gid://list/2".to_owned()],
            )
            .await
            .map_err(|e| format!("expected success, got {e}"))?;

        assert_eq!(
            reply.lists,
            ["gid://list/1", "gid://list/2"],
            "the confirmed complete set must come back"
        );
        assert_eq!(reply.rate_limit.remaining, Some(4_988));
        let received = server.received_requests().await.ok_or("no requests")?;
        let body: serde_json::Value = serde_json::from_slice(&received[0].body)?;
        assert_eq!(
            body["query"].as_str(),
            Some(SET_ITEM_LISTS_MUTATION),
            "the wire document must be exactly the legacy-proven replacement write"
        );
        assert_eq!(body["variables"]["itemId"], "gid://repository/1");
        assert_eq!(
            body["variables"]["listIds"],
            serde_json::json!(["gid://list/1", "gid://list/2"]),
            "the complete desired set must travel as the listIds variable"
        );
        Ok(())
    }
}
