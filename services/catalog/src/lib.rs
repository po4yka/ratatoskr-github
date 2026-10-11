#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Process boundary for Ratatoskr GitHub Catalog.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use axum::Json;
use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;

pub mod bus;
pub mod observer;
mod readme_blobs;
mod relay;
mod repository_action_attempts;
mod repository_api;

pub use repository_api::{RepositoryApiState, domain_router};

const STARTING: u8 = 0;
const READY: u8 = 1;
const DRAINING: u8 = 2;
const FAILED: u8 = 3;

/// A bounded, non-secret operator command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorCommand {
    /// Start the loopback operator server.
    Serve,
    /// Validate process configuration and exit.
    CheckConfig,
    /// Read one replacement PAT from standard input for an account UUID.
    ReconnectPat {
        /// Catalog account awaiting reauthorization.
        account_id: String,
    },
    /// Import one temporary source using a non-secret source label and map path.
    ImportLegacy {
        /// Stable non-secret source label.
        source_id: String,
        /// Local JSON owner-map path.
        owner_map_path: String,
    },
    /// Generate a redacted report after the normal snapshot cycle.
    ShadowLegacy {
        /// Stable non-secret source label.
        source_id: String,
    },
    /// Validate the newest clean report without activating any external route.
    CutoverReadiness {
        /// Stable non-secret source label.
        source_id: String,
    },
}

/// An invalid operator command with no supplied argument values.
#[derive(Debug, thiserror::Error)]
pub enum OperatorCommandError {
    /// An unknown command was requested.
    #[error("operator command is not recognized")]
    UnknownCommand,
    /// A command had an unsupported number of arguments.
    #[error("operator command arguments are invalid")]
    InvalidArguments,
    /// A secret-bearing command-line argument was refused.
    #[error("credential and source connection values must not be command-line arguments")]
    SecretBearingArgument,
}

/// Parses bounded operator commands without retaining program arguments.
///
/// # Errors
///
/// Returns [`OperatorCommandError`] for unsupported arguments or commands.
pub fn parse_operator_command<I>(arguments: I) -> Result<OperatorCommand, OperatorCommandError>
where
    I: IntoIterator<Item = String>,
{
    let mut arguments = arguments.into_iter();
    let _program_name = arguments.next();
    let command = arguments.next();
    let remaining = arguments.collect::<Vec<_>>();
    if remaining.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--pat" | "--token" | "--source-url" | "--database-url"
        )
    }) {
        return Err(OperatorCommandError::SecretBearingArgument);
    }
    match command.as_deref() {
        None => Ok(OperatorCommand::Serve),
        Some("check-config") if remaining.is_empty() => Ok(OperatorCommand::CheckConfig),
        Some("reconnect-pat") if remaining.len() == 1 => Ok(OperatorCommand::ReconnectPat {
            account_id: remaining
                .into_iter()
                .next()
                .ok_or(OperatorCommandError::InvalidArguments)?,
        }),
        Some("import-legacy") => parse_import_legacy(&remaining),
        Some("shadow-legacy") => parse_source_id(&remaining, |source_id| {
            OperatorCommand::ShadowLegacy { source_id }
        }),
        Some("cutover-readiness") => parse_source_id(&remaining, |source_id| {
            OperatorCommand::CutoverReadiness { source_id }
        }),
        Some("check-config" | "reconnect-pat") => Err(OperatorCommandError::InvalidArguments),
        Some(_) => Err(OperatorCommandError::UnknownCommand),
    }
}

fn parse_source_id(
    arguments: &[String],
    command: impl FnOnce(String) -> OperatorCommand,
) -> Result<OperatorCommand, OperatorCommandError> {
    match arguments {
        [flag, source_id] if flag == "--source-id" => Ok(command(source_id.clone())),
        _ => Err(OperatorCommandError::InvalidArguments),
    }
}

fn parse_import_legacy(arguments: &[String]) -> Result<OperatorCommand, OperatorCommandError> {
    if arguments.len() != 4 {
        return Err(OperatorCommandError::InvalidArguments);
    }
    let mut source_id = None;
    let mut owner_map_path = None;
    for pair in arguments.chunks_exact(2) {
        match pair {
            [flag, value] if flag == "--source-id" => source_id = Some(value.clone()),
            [flag, value] if flag == "--owner-map" => owner_map_path = Some(value.clone()),
            _ => return Err(OperatorCommandError::InvalidArguments),
        }
    }
    match (source_id, owner_map_path) {
        (Some(source_id), Some(owner_map_path)) => Ok(OperatorCommand::ImportLegacy {
            source_id,
            owner_map_path,
        }),
        _ => Err(OperatorCommandError::InvalidArguments),
    }
}

/// What the outbox relay reports: its health and the counters behind `/metrics`.
#[derive(Debug)]
struct PublisherHealth {
    healthy: AtomicBool,
    published: AtomicU64,
    failed: AtomicU64,
    undeliverable_payload_too_large: AtomicU64,
}

/// Shared process lifecycle used by readiness checks.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: Arc<AtomicU8>,
    publisher: Arc<PublisherHealth>,
}

impl Lifecycle {
    /// Creates a starting lifecycle.
    #[must_use]
    pub fn starting() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(STARTING)),
            publisher: Arc::new(PublisherHealth {
                healthy: AtomicBool::new(true),
                published: AtomicU64::new(0),
                failed: AtomicU64::new(0),
                undeliverable_payload_too_large: AtomicU64::new(0),
            }),
        }
    }

    /// Sets the `bus_publish` readiness check fed by the outbox relay (XR-021 CONTRACTS.md
    /// R2-03). An unhealthy publisher only flips readiness; it never stops the relay.
    pub fn set_publisher_healthy(&self, healthy: bool) {
        self.publisher.healthy.store(healthy, Ordering::Release);
    }

    /// Counts one relay publication attempt by its outcome.
    pub fn record_publication(&self, published: bool) {
        let counter = if published {
            &self.publisher.published
        } else {
            &self.publisher.failed
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one outbox row classed undeliverable because it exceeds the broker payload limit.
    pub fn record_undeliverable_payload_too_large(&self) {
        self.publisher
            .undeliverable_payload_too_large
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Marks storage startup complete.
    pub fn mark_ready(&self) {
        self.state.store(READY, Ordering::Release);
    }

    /// Starts drain and makes readiness fail.
    pub fn begin_drain(&self) {
        self.state.store(DRAINING, Ordering::Release);
    }

    /// Makes readiness fail because a supervised dependency stopped before an orderly shutdown.
    pub fn mark_failed(&self) {
        self.state.store(FAILED, Ordering::Release);
    }

    /// Reports whether startup has completed, drain has not begun, and the relay can publish.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == READY && self.publisher_healthy()
    }

    fn publisher_healthy(&self) -> bool {
        self.publisher.healthy.load(Ordering::Acquire)
    }
}

/// Builds the loopback operator router.
pub fn admin_router(lifecycle: Lifecycle) -> Router {
    Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/version", get(version))
        .with_state(lifecycle)
        .layer(middleware::from_fn(no_store))
}

async fn live() -> StatusCode {
    StatusCode::OK
}

/// The readiness answer: the status and the checks that are failing, so an operator sees the
/// failing publisher instead of a bare 503.
async fn ready(
    axum::extract::State(lifecycle): axum::extract::State<Lifecycle>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut failing = Vec::new();
    if !lifecycle.publisher_healthy() {
        failing.push("bus_publish");
    }
    let status = if lifecycle.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(serde_json::json!({ "ready": status == StatusCode::OK, "failing": failing })),
    )
}

async fn metrics(axum::extract::State(lifecycle): axum::extract::State<Lifecycle>) -> String {
    let publisher = &lifecycle.publisher;
    format!(
        "# TYPE github_catalog_process_info gauge\n\
         github_catalog_process_info 1\n\
         # TYPE github_catalog_outbox_publications_total counter\n\
         github_catalog_outbox_publications_total{{outcome=\"published\"}} {}\n\
         github_catalog_outbox_publications_total{{outcome=\"failed\"}} {}\n\
         # TYPE github_catalog_outbox_undeliverable_total counter\n\
         github_catalog_outbox_undeliverable_total{{class=\"payload_too_large\"}} {}\n",
        publisher.published.load(Ordering::Relaxed),
        publisher.failed.load(Ordering::Relaxed),
        publisher
            .undeliverable_payload_too_large
            .load(Ordering::Relaxed),
    )
}

async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
