#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Process boundary for Ratatoskr GitHub Catalog.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;

const STARTING: u8 = 0;
const READY: u8 = 1;
const DRAINING: u8 = 2;

/// Shared process lifecycle used by readiness checks.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: Arc<AtomicU8>,
}

impl Lifecycle {
    /// Creates a starting lifecycle.
    #[must_use]
    pub fn starting() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(STARTING)),
        }
    }

    /// Marks storage startup complete.
    pub fn mark_ready(&self) {
        self.state.store(READY, Ordering::Release);
    }

    /// Starts drain and makes readiness fail.
    pub fn begin_drain(&self) {
        self.state.store(DRAINING, Ordering::Release);
    }

    /// Reports whether startup has completed and drain has not begun.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == READY
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

async fn metrics() -> &'static str {
    "# TYPE github_catalog_process_info gauge\ngithub_catalog_process_info 1\n"
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
