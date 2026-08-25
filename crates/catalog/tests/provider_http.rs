//! HTTP-level provider gateway behavior against a local mock server.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::provider::{
    FetchOutcome, FreshRepository, GithubApi, OwnerName, ProviderRepositoryBody,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO_PATH: &str = "/repos/acme/widgets";
const FRESH_BODY: &str = r#"{
    "id": 300000100,
    "full_name": "acme/widgets",
    "description": "A synthetic widget",
    "language": "Rust",
    "stargazers_count": 42,
    "topics": ["widgets", "rust"],
    "default_branch": "main",
    "pushed_at": "2026-08-01T10:00:00Z"
}"#;

#[tokio::test]
async fn conditional_request_sends_if_none_match_and_short_circuits_on_304()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(REPO_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(FRESH_BODY)
                .insert_header("etag", r#"W/"v1""#),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(REPO_PATH))
        .and(header("if-none-match", r#"W/"v1""#))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;

    let gateway =
        ratatoskr_github_catalog::provider::ReqwestGithubApi::for_base_url(&server.uri())?;

    let first = gateway
        .fetch_repository(None, "acme", "widgets", None)
        .await?
        .outcome;
    assert_eq!(
        first,
        FetchOutcome::Fresh(FreshRepository {
            body: serde_json::from_str::<ProviderRepositoryBody>(FRESH_BODY)?,
            etag: Some(r#"W/"v1""#.to_owned()),
            rename_evidence: None,
        }),
        "the first fetch must be fresh with the payload and its validator"
    );

    let second = gateway
        .fetch_repository(None, "acme", "widgets", Some(r#"W/"v1""#))
        .await?
        .outcome;
    assert_eq!(
        second,
        FetchOutcome::NotModified,
        "a 304 must short-circuit without a payload"
    );

    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn moved_permanently_reports_new_location() -> Result<(), Box<dyn std::error::Error>> {
    use ratatoskr_github_catalog::provider::ReqwestGithubApi;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(REPO_PATH))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", "/repos/new-owner/new-name"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = gateway
        .fetch_repository(None, "acme", "widgets", None)
        .await?;

    assert_eq!(
        outcome.outcome,
        FetchOutcome::MovedPermanently {
            target: OwnerName {
                owner: "new-owner".to_owned(),
                name: "new-name".to_owned(),
            }
        },
        "a permanent move must surface as evidence naming the new location"
    );

    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn mismatched_full_name_reports_rename_evidence_with_payload()
-> Result<(), Box<dyn std::error::Error>> {
    use ratatoskr_github_catalog::provider::{RenameEvidence, ReqwestGithubApi};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(REPO_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(FRESH_BODY.replace("acme/widgets", "renamed-crew/widgets")),
        )
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = gateway
        .fetch_repository(None, "acme", "widgets", None)
        .await?;

    let mut expected_body = serde_json::from_str::<ProviderRepositoryBody>(FRESH_BODY)?;
    expected_body.full_name = "renamed-crew/widgets".to_owned();
    assert_eq!(
        outcome.outcome,
        FetchOutcome::Fresh(FreshRepository {
            body: expected_body,
            etag: None,
            rename_evidence: Some(RenameEvidence {
                observed_as: OwnerName {
                    owner: "renamed-crew".to_owned(),
                    name: "widgets".to_owned(),
                },
            }),
        }),
        "the payload must still be delivered, with the differing full_name as evidence"
    );
    Ok(())
}

fn starred_item(id: i64, name: &str, starred_at: &str) -> String {
    format!(
        r#"{{"starred_at": "{starred_at}", "repo": {{
            "id": {id},
            "full_name": "{name}",
            "description": null,
            "language": "Rust",
            "stargazers_count": 1,
            "topics": [],
            "default_branch": "main",
            "pushed_at": null
        }}}}"#
    )
}

#[tokio::test]
async fn starred_listing_serves_pages_with_rate_headers_and_starred_at()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    let page_bodies = [
        (
            1_u32,
            format!(
                "[{}, {}]",
                starred_item(300_000_001, "acme/alpha", "2026-01-01T00:00:00Z"),
                starred_item(300_000_002, "acme/beta", "2026-02-02T00:00:00Z")
            ),
        ),
        (
            2,
            format!(
                "[{}]",
                starred_item(300_000_003, "acme/gamma", "2026-03-03T00:00:00Z")
            ),
        ),
        (3, "[]".to_owned()),
    ];
    for (page, body) in &page_bodies {
        Mock::given(method("GET"))
            .and(path("/user/starred"))
            .and(query_param("page", page.to_string()))
            .and(header("accept", "application/vnd.github.star+json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body.clone())
                    .insert_header("x-ratelimit-limit", "5000")
                    .insert_header("x-ratelimit-remaining", "4999")
                    .insert_header("x-ratelimit-reset", "1787000000"),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;

    let first = gateway.list_starred(None, 1).await?;
    assert_eq!(first.page.items.len(), 2, "page one must carry both items");
    assert_eq!(first.page.items[0].repo.provider_repository_id, 300_000_001);
    assert_eq!(
        first.page.items[0]
            .repo
            .owner_name()
            .map(|owner_name| owner_name.to_string()),
        Some("acme/alpha".to_owned())
    );
    assert_eq!(
        first.page.items[0].starred_at.as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "the listing must surface the provider starred-at timestamp"
    );
    assert_eq!(
        first.rate_limit.remaining,
        Some(4999),
        "listing replies must carry rate-limit headers for the shared ledger"
    );

    let second = gateway.list_starred(None, 2).await?;
    assert_eq!(second.page.items.len(), 1, "page two must carry its item");

    let third = gateway.list_starred(None, 3).await?;
    assert!(
        third.page.items.is_empty(),
        "an empty page must be representable so enumeration can terminate"
    );

    let received = server.received_requests().await.unwrap_or_default();
    let requested_pages: Vec<String> = received
        .iter()
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find_map(|(key, value)| (key == "page").then(|| value.into_owned()))
        })
        .collect();
    assert_eq!(
        requested_pages,
        ["1", "2", "3"],
        "pages must be requested in ascending order"
    );

    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn newest_first_listing_requests_sort_created_direction_desc()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "1"))
        .and(query_param("sort", "created"))
        .and(query_param("direction", "desc"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!(
                    "[{}]",
                    starred_item(300_000_004, "acme/delta", "2026-04-04T00:00:00Z")
                ))
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4998")
                .insert_header("x-ratelimit-reset", "1787000000"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;

    let reply = gateway.list_starred_newest_first(None, 1).await?;
    assert_eq!(
        reply.page.items.len(),
        1,
        "the newest-first page must carry its item"
    );
    assert_eq!(
        reply.page.items[0].starred_at.as_deref(),
        Some("2026-04-04T00:00:00Z"),
        "the newest-first listing must surface provider starred-at for watermarking"
    );
    assert_eq!(
        reply.rate_limit.remaining,
        Some(4998),
        "listing replies must carry rate-limit headers for the shared ledger"
    );

    let received = server.received_requests().await.unwrap_or_default();
    let requested_pages: Vec<String> = received
        .iter()
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find_map(|(key, value)| (key == "page").then(|| value.into_owned()))
        })
        .collect();
    assert_eq!(
        requested_pages,
        ["1"],
        "the newest-first call must address exactly the requested page"
    );

    server.verify().await;
    Ok(())
}

/// The committed synthetic GraphQL response pinning the star-list wire shape.
const USER_LISTS_PAGE_FIXTURE: &str = include_str!("fixtures/lists/user_lists_page.json");

#[tokio::test]
async fn starred_lists_page_posts_graphql_query_and_normalizes_reply()
-> Result<(), Box<dyn std::error::Error>> {
    use time::format_description::well_known::Rfc3339;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("authorization", "Bearer token-1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(USER_LISTS_PAGE_FIXTURE))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;

    let first = gateway.list_user_lists(Some("token-1"), None).await?;
    assert_eq!(
        first.page.lists.len(),
        2,
        "the fixture's two lists must be normalized into list nodes"
    );
    let rust_crates = &first.page.lists[0];
    assert_eq!(
        rust_crates.provider_list_id, "gid://UserList/5021471",
        "the stable GraphQL node id is the provider list identity"
    );
    assert_eq!(rust_crates.name, "Rust crates");
    assert!(
        !rust_crates.items_truncated,
        "a list whose items fit one page is not truncated"
    );
    assert_eq!(
        rust_crates
            .items
            .iter()
            .map(|item| (item.provider_repository_id, item.full_name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (300_000_101_i64, "acme/alpha"),
            (300_000_102_i64, "acme/beta")
        ],
        "each listed repository carries its numeric id and owner/name"
    );
    assert_eq!(
        first.page.next_cursor.as_deref(),
        Some("MQ"),
        "the lists connection's endCursor becomes the continuation token"
    );
    let expected_reset =
        time::OffsetDateTime::parse("2026-08-25T22:00:00Z", &Rfc3339)?.unix_timestamp();
    assert_eq!(
        first.rate_limit.remaining,
        Some(4998),
        "the GraphQL rateLimit object must map onto the shared ledger shape"
    );
    assert_eq!(
        first.rate_limit.reset_epoch_seconds,
        Some(expected_reset),
        "the rateLimit resetAt maps onto the ledger's reset epoch"
    );

    // The continuation token travels with the next request.
    let _second = gateway.list_user_lists(Some("token-1"), Some("MQ")).await?;
    let received = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        received.len(),
        2,
        "exactly one request per enumeration call"
    );
    let second_request_body = String::from_utf8_lossy(&received[1].body);
    assert!(
        !String::from_utf8_lossy(&received[0].body).contains("MQ"),
        "the first request must not carry a continuation token"
    );
    assert!(
        second_request_body.contains("MQ"),
        "the resumed request must carry the stored continuation token"
    );

    server.verify().await;
    Ok(())
}
