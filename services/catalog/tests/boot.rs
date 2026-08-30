//! Real process startup test.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[tokio::test]
async fn configured_process_serves_distinct_operator_and_domain_listeners_and_stops_on_sigterm()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let seed_path = std::env::temp_dir().join(format!("github-test-{}.nkey", uuid::Uuid::now_v7()));
    let seed = nkeys::KeyPair::new_user().seed()?;
    std::fs::write(&seed_path, seed)?;
    let nats_url = test_nats_url();
    provision_bus(&nats_url).await?;
    let reserved_admin = TcpListener::bind("127.0.0.1:0")?;
    let admin_address = reserved_admin.local_addr()?;
    let reserved_api = TcpListener::bind("127.0.0.1:0")?;
    let api_address = reserved_api.local_addr()?;

    let check = configured_command(
        admin_address,
        api_address,
        &database_url,
        &nats_url,
        &seed_path,
    )
    .arg("check-config")
    .status()?;
    assert!(check.success());
    drop(reserved_admin);
    drop(reserved_api);

    let mut child = configured_command(
        admin_address,
        api_address,
        &database_url,
        &nats_url,
        &seed_path,
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()?;
    let result = exercise_process(
        &mut child,
        admin_address,
        api_address,
        &nats_url,
        &database.database,
    )
    .await;
    stop_process(&mut child)?;

    database.cleanup().await?;
    std::fs::remove_file(seed_path)?;
    result
}

fn configured_command(
    admin_address: SocketAddr,
    api_address: SocketAddr,
    database_url: &str,
    nats_url: &str,
    seed_path: &std::path::Path,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"));
    command
        .env(
            "RATATOSKR__ADMIN__LISTEN_ADDRESS",
            admin_address.to_string(),
        )
        .env("RATATOSKR__API__LISTEN_ADDRESS", api_address.to_string())
        .env("RATATOSKR__STORAGE__DATABASE_URL", database_url)
        .env("RATATOSKR__BUS__URL", nats_url)
        .env(
            "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .env("RATATOSKR__CREDENTIALS__KEY_VERSION", "test-key")
        .env("RATATOSKR__BUS__NKEY_SEED_PATH", seed_path);
    command
}

async fn provision_bus(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let context = async_nats::jetstream::new(async_nats::connect(url).await?);
    for (name, subjects) in [
        ("ratatoskr_commands", vec!["cmd.>".to_owned()]),
        ("ratatoskr_events", vec!["evt.>".to_owned()]),
    ] {
        context
            .get_or_create_stream(async_nats::jetstream::stream::Config {
                name: name.to_owned(),
                subjects,
                ..async_nats::jetstream::stream::Config::default()
            })
            .await?;
    }
    for spec in ratatoskr_github_catalog_service::CONSUMERS {
        context
            .get_stream(spec.stream)
            .await?
            .get_or_create_consumer(
                spec.durable,
                async_nats::jetstream::consumer::pull::Config {
                    durable_name: Some(spec.durable.to_owned()),
                    filter_subject: spec.subject.to_owned(),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_mins(2),
                    max_deliver: 10,
                    ..async_nats::jetstream::consumer::pull::Config::default()
                },
            )
            .await?;
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only broker location is not process configuration"
)]
fn test_nats_url() -> String {
    std::env::var("GITHUB_CATALOG_TEST_NATS_URL")
        .unwrap_or_else(|_| "nats://127.0.0.1:14227".to_owned())
}

async fn exercise_process(
    child: &mut Child,
    admin_address: SocketAddr,
    api_address: SocketAddr,
    nats_url: &str,
    database: &ratatoskr_github_catalog::Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("process exited before readiness: {status}").into());
        }
        if http_status(admin_address, "/ready").is_ok_and(|status| status == 200) {
            break;
        }
        if Instant::now() >= deadline {
            return Err("readiness did not arrive".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(http_status(admin_address, "/live")?, 200);
    assert_eq!(http_status(admin_address, "/metrics")?, 200);
    assert_eq!(http_status(admin_address, "/version")?, 200);
    assert_eq!(http_status(admin_address, "/unrelated")?, 404);
    assert_eq!(
        http_status(api_address, "/unrelated")?,
        404,
        "the separately configured domain listener must accept HTTP"
    );
    exercise_fixed_consumers(nats_url, database).await?;
    Ok(())
}

async fn exercise_fixed_consumers(
    nats_url: &str,
    database: &ratatoskr_github_catalog::Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = async_nats::connect(nats_url).await?;
    let context = async_nats::jetstream::new(client);
    for spec in ratatoskr_github_catalog_service::CONSUMERS {
        let identity_name = if spec.subject.starts_with("cmd.") {
            "command_id"
        } else {
            "event_id"
        };
        let identity = uuid::Uuid::now_v7();
        let payload = format!("{{\"{identity_name}\":\"{identity}\",\"malformed\":true}}");
        context.publish(spec.subject, payload.into()).await?.await?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rejected: i64 = sqlx::query_scalar(
            "select count(*) from github_catalog.inbox_events where state='rejected'",
        )
        .fetch_one(database.pool())
        .await?;
        if rejected == 4 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("only {rejected} fixed consumers committed rejection").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for spec in ratatoskr_github_catalog_service::CONSUMERS {
        let consumer: async_nats::jetstream::consumer::PullConsumer = context
            .get_consumer_from_stream(spec.durable, spec.stream)
            .await?;
        assert_eq!(consumer.cached_info().num_ack_pending, 0);
    }
    Ok(())
}

fn http_status(address: SocketAddr, path: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100))?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("missing HTTP status")?
        .parse()?;
    Ok(status)
}

fn stop_process(child: &mut Child) -> Result<(), Box<dyn std::error::Error>> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let signal = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()?;
    if !signal.success() {
        return Err("could not signal process".into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            child.kill()?;
            return Err("process did not stop within the shutdown bound".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only database location is not process configuration"
)]
fn test_database_url(database_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let admin_url = std::env::var("GITHUB_CATALOG_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://github:github@127.0.0.1:5435/github".to_owned());
    let (server, _) = admin_url
        .rsplit_once('/')
        .ok_or("invalid test database URL")?;
    Ok(format!("{server}/{database_name}"))
}
