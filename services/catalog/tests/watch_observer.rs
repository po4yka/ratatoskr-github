//! The periodic observer evaluates repositories that have an enabled watch (XR-021
//! CONTRACTS.md R2-08 c) against a wiremock provider and a disposable database.

use std::time::Duration;

use ratatoskr_github_catalog::provider::{ProviderRepositoryBody, ReqwestGithubApi};
use ratatoskr_github_catalog::rate_limit::{RateLimitHeaders, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    AliasKind, RepositoryAnalysisSource, WatchBaseline, WatchConfig, apply_fresh_source,
    record_alias, register_repository_analysis_watch, upsert_repository,
};
use ratatoskr_github_catalog_service::observer::{
    OBSERVER_TOKEN_LABEL, ObserverPass, WatchObserver,
};
use ratatoskr_github_contracts::{ReadmeAbsenceReason, ReadmeRevision};
use ratatoskr_identifiers::TenantRef;
use time::OffsetDateTime;
use tokio::sync::watch;
use uuid::Uuid;
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000d01";

fn body(provider_id: i64, name: &str, description: &str) -> ProviderRepositoryBody {
    ProviderRepositoryBody {
        provider_repository_id: provider_id,
        full_name: format!("acme/{name}"),
        description: Some(description.to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 5,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-27T00:00:00Z".to_owned()),
    }
}

fn provider_body(provider_id: i64, name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "id": provider_id,
        "full_name": format!("acme/{name}"),
        "description": description,
        "language": "Rust",
        "stargazers_count": 5,
        "topics": [],
        "default_branch": "main",
        "pushed_at": "2026-08-27T00:00:00Z"
    })
}

/// A watched repository the observer already knows at description "v1" with the entity tag `"v1"`.
async fn seed(
    database: &TestDatabase,
    provider_id: i64,
    name: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let repository = upsert_repository(&database.database, provider_id).await?;
    record_alias(
        &database.database,
        repository.repository_id,
        AliasKind::OwnerName,
        &format!("acme/{name}"),
    )
    .await?;
    apply_fresh_source(
        &database.database,
        repository.repository_id,
        &body(provider_id, name, "v1"),
        Some("\"v1\""),
        &RepositoryAnalysisSource {
            readme: ReadmeRevision::Absent {
                reason: ReadmeAbsenceReason::NotFound,
            },
            readme_etag: None,
        },
    )
    .await?;
    register_repository_analysis_watch(
        &database.database,
        TenantRef::parse(OWNER)?,
        repository.repository_id,
        WatchBaseline::Current,
    )
    .await?;
    Ok(repository.repository_id)
}

fn observer(
    server: &MockServer,
    interval: u32,
    batch: u32,
) -> Result<Option<WatchObserver>, Box<dyn std::error::Error>> {
    Ok(WatchObserver::new(
        &WatchConfig {
            observe_interval_seconds: interval,
            batch,
        },
        ReqwestGithubApi::for_base_url(&server.uri())?,
    ))
}

async fn request_owners(
    database: &TestDatabase,
    name: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "select request.owner_ref from github_catalog.repository_analysis_requests request
         join github_catalog.repository_aliases alias on alias.repository_id = request.repository_id
         where alias.alias_value = $1",
    )
    .bind(format!("acme/{name}"))
    .fetch_all(database.database.pool())
    .await?)
}

#[tokio::test]
async fn a_changed_repository_is_requested_and_an_unchanged_one_is_not() -> TestResult {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    seed(&database, 971_001, "alpha").await?;
    seed(&database, 971_002, "beta").await?;
    Mock::given(method("GET"))
        .and(path("/repos/acme/alpha"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(provider_body(971_001, "alpha", "v2"))
                .insert_header("etag", "\"v2\""),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/alpha/readme"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# alpha\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/beta"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;
    let observer = observer(&server, 3600, 10)?.expect("a non-zero interval enables the observer");

    let pass = observer
        .pass(&database.database, OffsetDateTime::now_utc())
        .await?;

    assert_eq!(
        pass,
        ObserverPass {
            observed: 1,
            unchanged: 1,
            failed: 0,
            rate_limited: false,
        }
    );
    assert_eq!(
        request_owners(&database, "alpha").await?,
        vec![OWNER.to_owned()]
    );
    assert!(request_owners(&database, "beta").await?.is_empty());

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_pass_observes_at_most_the_batch_oldest_first() -> TestResult {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    for (index, (name, age_minutes)) in [("old", 30), ("middle", 20), ("new", 10)]
        .into_iter()
        .enumerate()
    {
        let repository_id = seed(&database, 972_000 + i64::try_from(index)?, name).await?;
        sqlx::query(
            "update github_catalog.repository_metadata
             set fetched_at = now() - make_interval(mins => $2) where repository_id = $1",
        )
        .bind(repository_id)
        .bind(age_minutes)
        .execute(database.database.pool())
        .await?;
    }
    Mock::given(method("GET"))
        .and(path_regex("^/repos/acme/[a-z]+$"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;
    let observer = observer(&server, 3600, 2)?.expect("enabled");

    let pass = observer
        .pass(&database.database, OffsetDateTime::now_utc())
        .await?;

    assert_eq!(pass.unchanged, 2);
    let mut requested: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    requested.sort();
    assert_eq!(requested, vec!["/repos/acme/middle", "/repos/acme/old"]);

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn interval_zero_disables_the_observer() -> TestResult {
    let server = MockServer::start().await;
    assert!(observer(&server, 0, 10)?.is_none());
    Ok(())
}

#[tokio::test]
async fn a_withheld_budget_observes_nothing() -> TestResult {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    seed(&database, 973_001, "alpha").await?;
    let observer = observer(&server, 3600, 10)?.expect("enabled");
    observer.ledger().observe(
        &TokenRef::from_label(OBSERVER_TOKEN_LABEL),
        &RateLimitHeaders {
            remaining: Some(0),
            reset_epoch_seconds: Some(OffsetDateTime::now_utc().unix_timestamp() + 3600),
            ..RateLimitHeaders::default()
        },
    );

    let pass = observer
        .pass(&database.database, OffsetDateTime::now_utc())
        .await?;

    assert_eq!(
        pass,
        ObserverPass {
            rate_limited: true,
            ..ObserverPass::default()
        }
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn the_running_observer_passes_every_interval_until_shutdown() -> TestResult {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    seed(&database, 974_001, "alpha").await?;
    Mock::given(method("GET"))
        .and(path("/repos/acme/alpha"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(provider_body(974_001, "alpha", "v2"))
                .insert_header("etag", "\"v2\""),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/alpha/readme"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let observer = observer(&server, 1, 10)?.expect("enabled");
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(observer.run(database.database.clone(), shutdown));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if !request_owners(&database, "alpha").await?.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the observer never queued the request"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop.send(true)?;
    task.await??;

    database.cleanup().await?;
    Ok(())
}
