use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::{error, fmt};

use serde::Serialize;

const ENV_PREFIX: &str = "RATATOSKR__";

/// Process configuration with finite built-in limits.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    /// Operator listener configuration.
    pub admin: AdminConfig,
    /// Owned durable storage configuration.
    pub storage: StorageConfig,
    /// Resource and shutdown limits.
    pub limits: Limits,
}

/// Loopback-only operator listener configuration.
#[derive(Debug, Clone, Serialize)]
pub struct AdminConfig {
    /// Socket address for health, metrics, and version routes.
    pub listen_address: SocketAddr,
}

/// `PostgreSQL` storage locations owned by this service.
#[derive(Debug, Clone, Serialize)]
pub struct StorageConfig {
    /// Catalog `PostgreSQL` connection URL.
    #[serde(skip_serializing)]
    pub database_url: String,
}

/// Finite limits used by the process foundation.
#[derive(Debug, Clone, Serialize)]
pub struct Limits {
    /// Maximum database connections.
    pub database_connections: u32,
    /// Maximum wait for a database connection.
    pub database_acquire_timeout_ms: u64,
    /// Maximum graceful shutdown duration.
    pub shutdown_timeout_ms: u64,
}

/// Configuration loading failure that never includes a supplied value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    key: String,
    rule: &'static str,
}

impl Config {
    /// Loads the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for an unrecognized prefixed key, non-Unicode
    /// value, or invalid value.
    pub fn load() -> Result<Self, ConfigError> {
        let mut entries = Vec::new();
        for (key, value) in std::env::vars_os() {
            let Some(key) = key.to_str() else {
                continue;
            };
            if !key.starts_with(ENV_PREFIX) {
                continue;
            }
            let Some(value) = value.to_str() else {
                return Err(ConfigError::new(key, "must contain Unicode text"));
            };
            entries.push((key.to_owned(), value.to_owned()));
        }

        Self::from_environment(entries)
    }

    /// Loads configuration from prefixed environment entries.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for an unrecognized key or invalid value.
    pub fn from_environment<I, K, V>(entries: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut config = Self::default();
        for (key, value) in entries {
            let key = key.as_ref();
            if !key.starts_with(ENV_PREFIX) {
                continue;
            }
            apply_entry(&mut config, key, value.as_ref())?;
        }

        Ok(config)
    }
}

impl ConfigError {
    fn new(key: &str, rule: &'static str) -> Self {
        Self {
            key: key.to_owned(),
            rule,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration key {} {}", self.key, self.rule)
    }
}

impl error::Error for ConfigError {}

fn apply_entry(config: &mut Config, key: &str, value: &str) -> Result<(), ConfigError> {
    match key {
        "RATATOSKR__ADMIN__LISTEN_ADDRESS" => {
            let address = value
                .parse::<SocketAddr>()
                .map_err(|_| ConfigError::new(key, "must be a socket address"))?;
            if !address.ip().is_loopback() || address.port() == 0 {
                return Err(ConfigError::new(
                    key,
                    "must be a loopback address with a port",
                ));
            }
            config.admin.listen_address = address;
        }
        "RATATOSKR__STORAGE__DATABASE_URL" => {
            value
                .parse::<sqlx::postgres::PgConnectOptions>()
                .map_err(|_| ConfigError::new(key, "must be a PostgreSQL connection URL"))?;
            value.clone_into(&mut config.storage.database_url);
        }
        "RATATOSKR__LIMITS__DATABASE_CONNECTIONS" => {
            config.limits.database_connections = parse_positive(key, value)?;
        }
        "RATATOSKR__LIMITS__DATABASE_ACQUIRE_TIMEOUT_MS" => {
            config.limits.database_acquire_timeout_ms = parse_positive(key, value)?;
        }
        "RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS" => {
            config.limits.shutdown_timeout_ms = parse_positive(key, value)?;
        }
        _ => return Err(ConfigError::new(key, "is not recognized")),
    }
    Ok(())
}

fn parse_positive<T>(key: &str, value: &str) -> Result<T, ConfigError>
where
    T: std::str::FromStr + Default + PartialOrd,
{
    let parsed = value
        .parse::<T>()
        .map_err(|_| ConfigError::new(key, "must be a positive integer"))?;
    if parsed <= T::default() {
        return Err(ConfigError::new(key, "must be a positive integer"));
    }
    Ok(parsed)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            admin: AdminConfig {
                listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9095),
            },
            storage: StorageConfig {
                database_url: "postgres://github:github@127.0.0.1:5435/github".to_owned(),
            },
            limits: Limits {
                database_connections: 8,
                database_acquire_timeout_ms: 5_000,
                shutdown_timeout_ms: 10_000,
            },
        }
    }
}
