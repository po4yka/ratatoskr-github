//! The shipped operator example loads through the service's own configuration loader and enables
//! the bus and the reader route (XR-021 CONTRACTS.md R2-10), so it cannot rot.

use std::path::Path;

use axum::body::Body;
use axum::http::Request;
use ratatoskr_github_catalog::Config;
use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog_service::{RepositoryApiState, domain_router};
use tower::ServiceExt as _;

const EXAMPLE: &str = "deploy/systemd/github.conf.example";
const VALID_KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn example_text() -> Result<String, Box<dyn std::error::Error>> {
    Ok(std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(EXAMPLE),
    )?)
}

/// The assignments of the example, comments and blank lines dropped.
fn assignments(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[tokio::test]
async fn the_shipped_example_loads_and_enables_the_bus_and_the_reader_route()
-> Result<(), Box<dyn std::error::Error>> {
    let text = example_text()?;
    let seed = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("example-github.nkey");
    std::fs::write(&seed, "SUAEXAMPLESEEDNOTASECRET")?;

    let entries: Vec<(String, String)> = assignments(&text)
        .into_iter()
        .map(|(key, value)| match key.as_str() {
            // Absolute secret paths and placeholder secrets get working stand-ins.
            "RATATOSKR__BUS__NKEY_SEED_PATH" => (key, seed.display().to_string()),
            "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX" => (key, VALID_KEY_HEX.to_owned()),
            _ => (key, value),
        })
        .collect();
    let configuration =
        Config::from_environment(entries.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;

    assert_eq!(
        configuration.admin.listen_address.to_string(),
        "127.0.0.1:9095"
    );
    assert_eq!(
        configuration.api.listen_address.to_string(),
        "127.0.0.1:8092"
    );
    assert_eq!(
        configuration.bus.url.as_deref(),
        Some("nats://127.0.0.1:4222")
    );
    assert_eq!(configuration.bus.nkey_seed_path(), Some(seed.as_path()));
    assert!(
        text.contains("RATATOSKR__BUS__NKEY_SEED_PATH=/etc/ratatoskr/github.nkey"),
        "the seed is installed at /etc/ratatoskr/github.nkey"
    );
    assert!(
        configuration.internal.reader_service_secret().is_some(),
        "the README byte route is mounted only with the reader secret"
    );
    assert!(
        text.contains("RATATOSKR__INGEST__GITHUB_README_SERVICE_SECRET"),
        "the example says which Knowledge key must equal the reader secret"
    );
    assert!(configuration.credentials.encryption_key().is_ok());
    assert_eq!(configuration.watch.observe_interval_seconds, 3600);
    assert_eq!(configuration.watch.batch, 10);
    assert_eq!(configuration.provider.timeout_ms, 30_000);
    assert_eq!(
        configuration.provider.base_url, "https://api.github.com",
        "an example that leaves the origin unset targets GitHub"
    );
    assert!(
        text.contains("# RATATOSKR__PROVIDER__BASE_URL=https://api.github.com"),
        "the example names the provider origin key and its default"
    );
    for key in [
        "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
        "RATATOSKR__CREDENTIALS__KEY_VERSION",
        "RATATOSKR__INTERNAL__READER_SERVICE_SECRET",
        "RATATOSKR__BUS__URL",
        "RATATOSKR__WATCH__OBSERVE_INTERVAL_SECONDS",
        "RATATOSKR__WATCH__BATCH",
    ] {
        assert!(
            entries.iter().any(|(name, _)| name == key),
            "the example must set {key}"
        );
    }

    // The loaded configuration really mounts the reader route.
    let database = TestDatabase::create().await?;
    let secret = configuration
        .internal
        .reader_service_secret()
        .ok_or("reader secret")?
        .clone();
    let router = domain_router(
        RepositoryApiState::new(
            database.database.clone(),
            ReqwestGithubApi::for_base_url("http://127.0.0.1:9")?,
            None,
        )
        .with_reader_service_secret(secret),
    );
    let digest = "0".repeat(64);
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/internal/v1/readme-blobs/{digest}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(
        response.status().as_u16(),
        401,
        "mounted, and bearer-protected"
    );

    database.cleanup().await?;
    let _removed = std::fs::remove_file(&seed);
    Ok(())
}

const ROLE_FILE: &str = "deploy/postgres/role.sql";

/// XR-021 CONTRACTS.md R3-11: the target runs one `PostgreSQL` database, `ratatoskr`, on port 5432,
/// and the service connects to it as its own role, which a checked-in file creates.
#[test]
fn the_example_names_the_shared_database_and_the_role_file_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let text = example_text()?;
    let url = assignments(&text)
        .into_iter()
        .find(|(key, _)| key == "RATATOSKR__STORAGE__DATABASE_URL")
        .map(|(_, value)| value)
        .ok_or("the example must set RATATOSKR__STORAGE__DATABASE_URL")?;
    assert_eq!(
        url, "postgres://ratatoskr_github:CHANGE-ME@127.0.0.1:5432/ratatoskr",
        "the example names the shared database and this service's own role"
    );

    let role_file = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(ROLE_FILE),
    )?;
    assert!(
        role_file.contains("create role ratatoskr_github"),
        "the role file creates the role the example names"
    );
    assert!(
        role_file.contains("grant connect, create on database ratatoskr to ratatoskr_github"),
        "the role may connect to the shared database and create its schema"
    );
    assert!(
        role_file.contains("nosuperuser nocreatedb nocreaterole noreplication nobypassrls"),
        "the role carries no elevated attribute"
    );
    Ok(())
}
