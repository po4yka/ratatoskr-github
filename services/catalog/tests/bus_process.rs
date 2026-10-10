//! The real process supervises its bus: losing the broker ends the process with a failure status
//! instead of logging and staying ready (XR-021 CONTRACTS.md S02 rule 5).

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ratatoskr_github_catalog::test_support::TestDatabase;

mod bus_support;
use bus_support::{BROKER, GITHUB_DURABLES, TestResult, broker_url, provision, proxy_to};

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

fn http_status(address: SocketAddr, path: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100))?;
    stream.set_read_timeout(Some(Duration::from_millis(300)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("missing HTTP status")?
        .parse()?)
}

#[tokio::test]
async fn the_process_exits_with_a_failure_when_the_bus_connection_is_lost() -> TestResult {
    let _broker = BROKER.lock().await;
    let url = broker_url();
    let client = async_nats::connect(&url).await?;
    provision(&client, &GITHUB_DURABLES).await?;
    let target: SocketAddr = url.trim_start_matches("nats://").parse()?;
    let proxy = proxy_to(target).await?;
    let database = TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let reserved_admin = TcpListener::bind("127.0.0.1:0")?;
    let admin_address = reserved_admin.local_addr()?;
    let reserved_api = TcpListener::bind("127.0.0.1:0")?;
    let api_address = reserved_api.local_addr()?;
    drop(reserved_admin);
    drop(reserved_api);

    let mut child = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .env(
            "RATATOSKR__ADMIN__LISTEN_ADDRESS",
            admin_address.to_string(),
        )
        .env("RATATOSKR__API__LISTEN_ADDRESS", api_address.to_string())
        .env(
            "RATATOSKR__STORAGE__DATABASE_URL",
            test_database_url(&database_name)?,
        )
        .env("RATATOSKR__BUS__URL", format!("nats://{}", proxy.address))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let ready_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("the process exited before readiness: {status}").into());
        }
        if http_status(admin_address, "/ready").is_ok_and(|status| status == 200) {
            break;
        }
        if Instant::now() >= ready_deadline {
            child.kill()?;
            return Err("readiness did not arrive with the bus configured".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    proxy.sever.send(true)?;

    let exit_deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= exit_deadline {
            child.kill()?;
            return Err("the process kept running after the bus was lost".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        !status.success(),
        "a lost bus must end the process with a failure status, got {status}"
    );
    database.cleanup().await?;
    Ok(())
}
