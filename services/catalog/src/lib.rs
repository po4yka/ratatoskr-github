#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Process boundary for Ratatoskr GitHub Catalog.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;

mod bus;
mod repository_action_attempts;
mod repository_api;
mod runtime;

pub use bus::{BusError, COMMAND_STREAM, CONSUMERS, ConsumerSpec, EVENT_STREAM, FleetBus};
pub use repository_api::{RepositoryApiState, domain_router};
pub use runtime::{RuntimeError, run_fleet_bus_runtime};

const STARTING: u8 = 0;
const READY: u8 = 1;
const DRAINING: u8 = 2;
const DATABASE_READY: u8 = 1;
const BUS_READY: u8 = 2;
const TOPOLOGY_READY: u8 = 4;
const ALL_DEPENDENCIES_READY: u8 = DATABASE_READY | BUS_READY | TOPOLOGY_READY;
const EXPECTED_WORKERS: u8 = 7;

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
    /// Requeue one exact unpublished dead-lettered outbox identity.
    RequeueDeadLetter {
        /// Existing outbox message UUID.
        message_id: String,
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
        Some("requeue-dead-letter") => parse_message_id(&remaining),
        Some("check-config" | "reconnect-pat") => Err(OperatorCommandError::InvalidArguments),
        Some(_) => Err(OperatorCommandError::UnknownCommand),
    }
}

fn parse_message_id(arguments: &[String]) -> Result<OperatorCommand, OperatorCommandError> {
    match arguments {
        [flag, message_id] if flag == "--message-id" => Ok(OperatorCommand::RequeueDeadLetter {
            message_id: message_id.clone(),
        }),
        _ => Err(OperatorCommandError::InvalidArguments),
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

/// Shared process lifecycle used by readiness checks.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: Arc<AtomicU8>,
    dependencies: Arc<AtomicU8>,
    workers: Arc<AtomicU8>,
    retries: Arc<AtomicU64>,
    duplicates: Arc<AtomicU64>,
    rejections: Arc<AtomicU64>,
    dead_letters: Arc<AtomicU64>,
}

impl Lifecycle {
    /// Creates a starting lifecycle.
    #[must_use]
    pub fn starting() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(STARTING)),
            dependencies: Arc::new(AtomicU8::new(0)),
            workers: Arc::new(AtomicU8::new(0)),
            retries: Arc::new(AtomicU64::new(0)),
            duplicates: Arc::new(AtomicU64::new(0)),
            rejections: Arc::new(AtomicU64::new(0)),
            dead_letters: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Marks storage startup complete.
    pub fn mark_ready(&self) {
        self.dependencies
            .store(ALL_DEPENDENCIES_READY, Ordering::Release);
        self.workers.store(EXPECTED_WORKERS, Ordering::Release);
        self.state.store(READY, Ordering::Release);
    }

    /// Marks storage usable without claiming the other serving dependencies.
    pub fn mark_database_ready(&self) {
        self.dependencies.fetch_or(DATABASE_READY, Ordering::AcqRel);
    }

    /// Marks the NATS connection usable or unavailable.
    pub fn set_bus_ready(&self, ready: bool) {
        if ready {
            self.dependencies.fetch_or(BUS_READY, Ordering::AcqRel);
        } else {
            self.dependencies.fetch_and(!BUS_READY, Ordering::AcqRel);
        }
    }

    /// Marks all four exact fixed durables verified or drifted.
    pub fn set_topology_ready(&self, ready: bool) {
        if ready {
            self.dependencies.fetch_or(TOPOLOGY_READY, Ordering::AcqRel);
        } else {
            self.dependencies
                .fetch_and(!TOPOLOGY_READY, Ordering::AcqRel);
        }
    }

    /// Marks the serving supervisor active after worker creation.
    pub fn mark_serving(&self) {
        self.state.store(READY, Ordering::Release);
    }

    /// Records the current live supervised-worker count.
    pub fn set_live_workers(&self, count: u8) {
        self.workers.store(count, Ordering::Release);
    }

    /// Increments one bounded retry counter.
    pub fn record_retry(&self) {
        self.retries.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments one absorbed duplicate counter.
    pub fn record_duplicate(&self) {
        self.duplicates.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments one terminal rejection counter.
    pub fn record_rejection(&self) {
        self.rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments one dead-letter counter without changing readiness.
    pub fn record_dead_letter(&self) {
        self.dead_letters.fetch_add(1, Ordering::Relaxed);
    }

    /// Starts drain and makes readiness fail.
    pub fn begin_drain(&self) {
        self.state.store(DRAINING, Ordering::Release);
    }

    /// Reports whether startup has completed and drain has not begun.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == READY
            && self.dependencies.load(Ordering::Acquire) == ALL_DEPENDENCIES_READY
            && self.workers.load(Ordering::Acquire) == EXPECTED_WORKERS
    }

    fn metrics(&self) -> String {
        format!(
            "# TYPE github_catalog_process_info gauge\ngithub_catalog_process_info 1\n# TYPE github_catalog_bus_retries_total counter\ngithub_catalog_bus_retries_total {}\n# TYPE github_catalog_bus_duplicates_total counter\ngithub_catalog_bus_duplicates_total {}\n# TYPE github_catalog_bus_rejections_total counter\ngithub_catalog_bus_rejections_total {}\n# TYPE github_catalog_outbox_dead_letters_total counter\ngithub_catalog_outbox_dead_letters_total {}\n# TYPE github_catalog_bus_workers gauge\ngithub_catalog_bus_workers {}\n",
            self.retries.load(Ordering::Relaxed),
            self.duplicates.load(Ordering::Relaxed),
            self.rejections.load(Ordering::Relaxed),
            self.dead_letters.load(Ordering::Relaxed),
            self.workers.load(Ordering::Relaxed),
        )
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

async fn ready(axum::extract::State(lifecycle): axum::extract::State<Lifecycle>) -> StatusCode {
    if lifecycle.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn metrics(axum::extract::State(lifecycle): axum::extract::State<Lifecycle>) -> String {
    lifecycle.metrics()
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
