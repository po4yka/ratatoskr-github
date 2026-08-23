#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Ratatoskr GitHub Catalog service process.

use std::future::IntoFuture as _;
use std::time::Duration;

use ratatoskr_github_catalog::{Config, Database, init_telemetry};
use ratatoskr_github_catalog_service::{Lifecycle, admin_router};

/// A startup or runtime failure of the service process.
#[derive(Debug, thiserror::Error)]
enum ProcessError {
    /// Configuration was refused.
    #[error("configuration is invalid: {0}")]
    Config(#[from] ratatoskr_github_catalog::ConfigError),
    /// Telemetry bootstrap failed.
    #[error("telemetry failed: {0}")]
    Telemetry(#[from] ratatoskr_github_catalog::TelemetryError),
    /// The database was unreachable or refused the schema.
    #[error("database failed: {0}")]
    Database(#[from] ratatoskr_github_catalog::PersistenceError),
    /// The operator listener could not be bound.
    #[error("the operator listener could not be bound")]
    Bind(#[source] std::io::Error),
    /// The operator server failed while serving.
    #[error("the operator server failed")]
    Serve(#[source] std::io::Error),
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("ratatoskr-github-catalog: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), ProcessError> {
    let config = Config::load()?;
    if std::env::args().nth(1).as_deref() == Some("check-config") {
        return Ok(());
    }

    init_telemetry()?;
    let database = Database::connect(
        &config.storage.database_url,
        config.limits.database_connections,
        Duration::from_millis(config.limits.database_acquire_timeout_ms),
    )
    .await?;
    database.apply_schema().await?;

    let lifecycle = Lifecycle::starting();
    let listener = tokio::net::TcpListener::bind(config.admin.listen_address)
        .await
        .map_err(ProcessError::Bind)?;
    lifecycle.mark_ready();
    serve_admin(
        listener,
        lifecycle,
        database,
        Duration::from_millis(config.limits.shutdown_timeout_ms),
    )
    .await
}

async fn serve_admin(
    listener: tokio::net::TcpListener,
    lifecycle: Lifecycle,
    database: Database,
    shutdown_timeout: Duration,
) -> Result<(), ProcessError> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, admin_router(lifecycle))
        .with_graceful_shutdown(async move {
            let _ignored = shutdown_rx.await;
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => {
            database.close().await;
            result.map_err(ProcessError::Serve)?;
        }
        result = shutdown_signal() => {
            result.map_err(ProcessError::Serve)?;
            let _ignored = shutdown_tx.send(());
            if tokio::time::timeout(shutdown_timeout, &mut server).await.is_err() {
                database.close().await;
                return Err(ProcessError::Serve(std::io::Error::other(
                    "the operator server did not stop within the shutdown bound",
                )));
            }
            database.close().await;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}
