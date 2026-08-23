#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Domain library for the Ratatoskr GitHub Catalog bounded context.
//!
//! The foundation owns process configuration, telemetry bootstrap, and
//! application of the first-version `github_catalog` schema. Account
//! credentials, synchronization, and provider access arrive with later
//! implementation plan items.

mod config;
mod database;
mod telemetry;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use config::{AdminConfig, Config, ConfigError, Limits, StorageConfig};
pub use database::{Database, PersistenceError};
pub use telemetry::{TelemetryError, init_telemetry};
