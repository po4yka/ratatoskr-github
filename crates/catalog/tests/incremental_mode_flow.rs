//! End-to-end incremental scan classification, isolated from the large
//! watermark suite so that both test sources stay within the file-size gate.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{IncrementalScanOutcome, run_incremental_scan, upsert_repository};
use uuid::Uuid;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn seed_account(database: &ratatoskr_github_catalog::Database) -> Result<Uuid, sqlx::Error> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'mode-classifier', 'connected')",
    )
    .bind(account_id)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

async fn seed_watermark(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into github_catalog.star_watermarks (account_id, high_water_mark)
         values ($1, '2026-05-01T00:00:00Z'::timestamptz)",
    )
    .bind(account_id)
    .execute(database.pool())
    .await?;
    Ok(())
}

fn starred_item(id: i64, name: &str, starred_at: &str) -> String {
    format!(
        r#"{{"starred_at": "{starred_at}", "repo": {{
            "id": {id}, "full_name": "{name}", "description": null,
            "language": "Rust", "stargazers_count": 1, "topics": [],
            "default_branch": "main", "pushed_at": null
        }}}}"#
    )
}

async fn mount_newest_first_page(
    server: &MockServer,
    page: u32,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", page.to_string()))
        .and(query_param("sort", "created"))
        .and(query_param("direction", "desc"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "4999")
                .insert_header("x-ratelimit-reset", "1787000000"),
        )
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn first_star_observation_promotes_unclassified_to_auto_but_never_overrides_explicit_modes()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    seed_watermark(&database.database, account_id).await?;
    let ignored_id = upsert_repository(&database.database, 300_000_011)
        .await?
        .repository_id;
    sqlx::query("update github_catalog.repositories set mode = 'ignored' where repository_id = $1")
        .bind(ignored_id)
        .execute(database.database.pool())
        .await?;

    let server = MockServer::start().await;
    mount_newest_first_page(
        &server,
        1,
        format!(
            "[{}, {}]",
            starred_item(300_000_010, "acme/newest", "2026-07-03T00:00:00Z"),
            starred_item(300_000_011, "acme/middle", "2026-06-02T00:00:00Z")
        ),
    )
    .await?;
    mount_newest_first_page(&server, 2, "[]".to_owned()).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_incremental_scan(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("mode-promotion"),
        account_id,
    )
    .await?;
    assert!(matches!(outcome, IncrementalScanOutcome::Completed { .. }));

    let modes: Vec<(i64, Option<String>)> = sqlx::query_as(
        "select provider_repository_id, mode from github_catalog.repositories
         order by provider_repository_id",
    )
    .fetch_all(database.database.pool())
    .await?;
    assert_eq!(
        modes,
        [
            (300_000_010, Some("auto".to_owned())),
            (300_000_011, Some("ignored".to_owned())),
        ]
    );

    database.cleanup().await?;
    Ok(())
}
