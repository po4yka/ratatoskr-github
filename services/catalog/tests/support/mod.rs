use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub(crate) const KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    #[allow(
        dead_code,
        reason = "only content-boundary tests inspect response headers"
    )]
    pub(crate) headers: String,
    pub(crate) body: String,
}

pub(crate) fn configured_command(
    admin_address: SocketAddr,
    api_address: SocketAddr,
    database_url: &str,
    provider_base_url: &str,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"));
    command
        .env(
            "RATATOSKR__ADMIN__LISTEN_ADDRESS",
            admin_address.to_string(),
        )
        .env("RATATOSKR__API__LISTEN_ADDRESS", api_address.to_string())
        .env("RATATOSKR__STORAGE__DATABASE_URL", database_url)
        .env("RATATOSKR__PROVIDER__BASE_URL", provider_base_url)
        .env("RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX", KEY_HEX)
        .env("RATATOSKR__CREDENTIALS__KEY_VERSION", "test-key");
    command
}

pub(crate) fn wait_ready(
    child: &mut Child,
    address: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("process exited before readiness: {status}").into());
        }
        if http_status(address, "/ready").is_ok_and(|status| status == 200) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("readiness did not arrive".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[allow(
    dead_code,
    reason = "the shared support module is compiled independently for each integration test"
)]
pub(crate) fn http_json(
    address: SocketAddr,
    route: &str,
    user_id: &str,
    body: &serde_json::Value,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let body = body.to_string();
    send_request(
        address,
        &format!(
            "POST {route} HTTP/1.1\r\nHost: localhost\r\nx-ratatoskr-user-id: {user_id}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[allow(
    dead_code,
    reason = "only the service-authenticated content-boundary test uses this helper"
)]
pub(crate) fn http_service_json(
    address: SocketAddr,
    route: &str,
    bearer_token: Option<&str>,
    body: &serde_json::Value,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let body = body.to_string();
    let authorization = bearer_token.map_or_else(String::new, |token| {
        format!("Authorization: Bearer {token}\r\n")
    });
    send_request(
        address,
        &format!(
            "POST {route} HTTP/1.1\r\nHost: localhost\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[allow(
    dead_code,
    reason = "this shared module is compiled separately by tests that do not all issue GETs"
)]
pub(crate) fn http_get_json(
    address: SocketAddr,
    route: &str,
    user_id: &str,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    send_request(
        address,
        &format!(
            "GET {route} HTTP/1.1\r\nHost: localhost\r\nx-ratatoskr-user-id: {user_id}\r\nConnection: close\r\n\r\n"
        ),
    )
}

fn send_request(
    address: SocketAddr,
    request: &str,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(200))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("missing HTTP response body")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("missing HTTP status")?
        .parse()?;
    Ok(HttpResponse {
        status,
        headers: head.to_owned(),
        body: body.to_owned(),
    })
}

fn http_status(address: SocketAddr, route: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let response = send_request(
        address,
        &format!("GET {route} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
    )?;
    Ok(response.status)
}

pub(crate) fn stop_process(child: &mut Child) -> Result<(), Box<dyn std::error::Error>> {
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
pub(crate) fn test_database_url(database_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let admin_url = std::env::var("GITHUB_CATALOG_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://github:github@127.0.0.1:5435/github".to_owned());
    let (server, _) = admin_url
        .rsplit_once('/')
        .ok_or("invalid test database URL")?;
    Ok(format!("{server}/{database_name}"))
}
