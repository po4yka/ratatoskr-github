use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::{error, fmt};

use secrecy::SecretString;
use serde::Serialize;

use crate::CredentialKey;

const ENV_PREFIX: &str = "RATATOSKR__";

/// Process configuration with finite built-in limits.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    /// Operator listener configuration.
    pub admin: AdminConfig,
    /// Host-local authenticated domain listener configuration.
    pub api: ApiConfig,
    /// GitHub provider endpoint configuration.
    pub provider: ProviderConfig,
    /// Platform-owned fleet bus connection and finite worker limits.
    pub bus: BusConfig,
    /// Owned durable storage configuration.
    pub storage: StorageConfig,
    /// Credential encryption configuration.
    pub credentials: CredentialsConfig,
    /// GitHub OAuth application configuration.
    pub github_oauth: GithubOAuthConfig,
    /// Ephemeral retired-source configuration used only by import commands.
    pub legacy: LegacyConfig,
    /// Resource and shutdown limits.
    pub limits: Limits,
}

/// Loopback-only operator listener configuration.
#[derive(Debug, Clone, Serialize)]
pub struct AdminConfig {
    /// Socket address for health, metrics, and version routes.
    pub listen_address: SocketAddr,
}

/// Loopback-only authenticated domain listener configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ApiConfig {
    /// Socket address for Edge-authenticated domain routes.
    pub listen_address: SocketAddr,
}

/// Bounded provider endpoint configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderConfig {
    /// HTTPS provider origin, or a numeric loopback HTTP origin for tests.
    pub base_url: String,
}

/// Finite least-privilege fleet bus configuration.
#[derive(Clone, Serialize)]
pub struct BusConfig {
    /// NATS endpoint; credentials are supplied only by the seed file.
    pub url: String,
    #[serde(skip_serializing)]
    nkey_seed_path: Option<String>,
    /// Overall connection deadline.
    pub connect_timeout_ms: u64,
    /// `JetStream` persistence acknowledgement deadline.
    pub publish_ack_timeout_ms: u64,
    /// Idle worker polling interval.
    pub poll_interval_ms: u64,
    /// Database claim lease duration.
    pub lease_ms: u64,
    /// Maximum rows or deliveries admitted per iteration.
    pub batch_size: u32,
    /// Finite application attempt ceiling.
    pub max_attempts: i32,
    /// Maximum wait for supervised workers to join.
    pub worker_join_timeout_ms: u64,
}

impl BusConfig {
    /// Returns the protected `NKey` seed path required by the serving role.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when no absolute seed path is configured.
    pub fn nkey_seed_path(&self) -> Result<&str, ConfigError> {
        self.nkey_seed_path.as_deref().ok_or_else(|| {
            ConfigError::new(
                "RATATOSKR__BUS__NKEY_SEED_PATH",
                "must be configured for the serving role",
            )
        })
    }

    fn validate(&self, shutdown_timeout_ms: u64) -> Result<(), ConfigError> {
        for (key, value, maximum) in [
            (
                "RATATOSKR__BUS__CONNECT_TIMEOUT_MS",
                self.connect_timeout_ms,
                30_000,
            ),
            (
                "RATATOSKR__BUS__PUBLISH_ACK_TIMEOUT_MS",
                self.publish_ack_timeout_ms,
                30_000,
            ),
            (
                "RATATOSKR__BUS__POLL_INTERVAL_MS",
                self.poll_interval_ms,
                60_000,
            ),
            ("RATATOSKR__BUS__LEASE_MS", self.lease_ms, 600_000),
            (
                "RATATOSKR__BUS__WORKER_JOIN_TIMEOUT_MS",
                self.worker_join_timeout_ms,
                120_000,
            ),
        ] {
            if value == 0 || value > maximum {
                return Err(ConfigError::new(key, "must be within its finite bound"));
            }
        }
        if !(1..=256).contains(&self.batch_size) {
            return Err(ConfigError::new(
                "RATATOSKR__BUS__BATCH_SIZE",
                "must be between 1 and 256",
            ));
        }
        if !(1..=100).contains(&self.max_attempts) {
            return Err(ConfigError::new(
                "RATATOSKR__BUS__MAX_ATTEMPTS",
                "must be between 1 and 100",
            ));
        }
        if self.worker_join_timeout_ms >= shutdown_timeout_ms || shutdown_timeout_ms > 125_000 {
            return Err(ConfigError::new(
                "RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS",
                "must exceed worker join timeout and be at most 125000",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for BusConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BusConfig")
            .field("url", &self.url)
            .field(
                "nkey_seed_path",
                &self.nkey_seed_path.as_ref().map(|_| "[REDACTED]"),
            )
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("publish_ack_timeout_ms", &self.publish_ack_timeout_ms)
            .field("poll_interval_ms", &self.poll_interval_ms)
            .field("lease_ms", &self.lease_ms)
            .field("batch_size", &self.batch_size)
            .field("max_attempts", &self.max_attempts)
            .field("worker_join_timeout_ms", &self.worker_join_timeout_ms)
            .finish()
    }
}

/// `PostgreSQL` storage locations owned by this service.
#[derive(Clone, Serialize)]
pub struct StorageConfig {
    /// Catalog `PostgreSQL` connection URL.
    #[serde(skip_serializing)]
    pub database_url: String,
}

impl fmt::Debug for StorageConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StorageConfig")
            .field("database_url", &"[REDACTED]")
            .finish()
    }
}

/// Encryption configuration for locally stored GitHub credentials.
#[derive(Clone, Serialize)]
pub struct CredentialsConfig {
    #[serde(skip_serializing)]
    encryption_key_hex: Option<String>,
    #[serde(skip_serializing)]
    encryption_key_path: Option<String>,
    /// Non-secret label of the configured encryption key.
    pub key_version: Option<String>,
}

impl CredentialsConfig {
    /// Returns a parsed key for a credential registration operation.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when registration encryption is not configured.
    pub fn encryption_key(&self) -> Result<CredentialKey, ConfigError> {
        let from_file;
        let value = if let Some(value) = self.encryption_key_hex.as_deref() {
            value
        } else if let Some(path) = self.encryption_key_path.as_deref() {
            from_file = std::fs::read_to_string(path).map_err(|_| {
                ConfigError::new(
                    "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_PATH",
                    "must name a readable protected key file",
                )
            })?;
            from_file.trim()
        } else {
            return Err(ConfigError::new(
                "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_PATH",
                "must be configured for credential registration",
            ));
        };
        let version = self.key_version.as_deref().ok_or_else(|| {
            ConfigError::new(
                "RATATOSKR__CREDENTIALS__KEY_VERSION",
                "must be configured for credential registration",
            )
        })?;
        CredentialKey::from_hex(value, version).map_err(|_| {
            ConfigError::new(
                "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
                "must be a 32-byte hexadecimal AES key",
            )
        })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.encryption_key_hex.is_some() && self.encryption_key_path.is_some() {
            return Err(ConfigError::new(
                "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_PATH",
                "must not be combined with RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
            ));
        }
        match (
            self.encryption_key_hex.is_some() || self.encryption_key_path.is_some(),
            &self.key_version,
        ) {
            (false, None) => Ok(()),
            (true, Some(_)) => self.encryption_key().map(|_| ()),
            (false, Some(_)) => Err(ConfigError::new(
                "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
                "must be configured with RATATOSKR__CREDENTIALS__KEY_VERSION",
            )),
            (true, None) => Err(ConfigError::new(
                "RATATOSKR__CREDENTIALS__KEY_VERSION",
                "must be configured with the credential encryption key",
            )),
        }
    }
}

impl fmt::Debug for CredentialsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialsConfig")
            .field(
                "encryption_key_hex",
                &self.encryption_key_hex.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "encryption_key_path",
                &self.encryption_key_path.as_ref().map(|_| "[REDACTED]"),
            )
            .field("key_version", &self.key_version)
            .finish()
    }
}

/// Service-local GitHub OAuth application configuration.
#[derive(Clone, Serialize)]
pub struct GithubOAuthConfig {
    /// Non-secret GitHub OAuth application client identifier.
    pub client_id: Option<String>,
    #[serde(skip_serializing)]
    client_secret: Option<SecretString>,
}

impl GithubOAuthConfig {
    /// Returns the configured OAuth application credentials when the feature is enabled.
    #[must_use]
    pub fn credentials(&self) -> Option<OAuthAppCredentials> {
        match (&self.client_id, &self.client_secret) {
            (Some(client_id), Some(client_secret)) => Some(OAuthAppCredentials {
                client_id: client_id.clone(),
                client_secret: client_secret.clone(),
            }),
            (None | Some(_), None) | (None, Some(_)) => None,
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        match (&self.client_id, &self.client_secret) {
            (None, None) => Ok(()),
            (Some(client_id), Some(_)) if valid_oauth_client_id(client_id) => Ok(()),
            (Some(_), Some(_)) => Err(ConfigError::new(
                "RATATOSKR__GITHUB_OAUTH__CLIENT_ID",
                "must use only ASCII letters, digits, dots, underscores, or hyphens",
            )),
            (Some(_), None) => Err(ConfigError::new(
                "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET",
                "must be configured with RATATOSKR__GITHUB_OAUTH__CLIENT_ID",
            )),
            (None, Some(_)) => Err(ConfigError::new(
                "RATATOSKR__GITHUB_OAUTH__CLIENT_ID",
                "must be configured with RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET",
            )),
        }
    }
}

impl fmt::Debug for GithubOAuthConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GithubOAuthConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Redacting OAuth application credentials ready for a provider request.
#[derive(Clone)]
pub struct OAuthAppCredentials {
    /// Non-secret GitHub OAuth application client identifier.
    pub client_id: String,
    client_secret: SecretString,
}

impl OAuthAppCredentials {
    /// Returns the provider-authentication secret without serializing it.
    #[must_use]
    pub fn client_secret(&self) -> &SecretString {
        &self.client_secret
    }
}

impl fmt::Debug for OAuthAppCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthAppCredentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .finish()
    }
}

/// Temporary source configuration for one legacy import invocation.
#[derive(Clone, Serialize)]
pub struct LegacyConfig {
    #[serde(skip_serializing)]
    source_database_url: Option<String>,
}

impl LegacyConfig {
    /// Returns the temporary source URL required by a legacy import command.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when no isolated source was configured.
    pub fn source_database_url(&self) -> Result<&str, ConfigError> {
        self.source_database_url.as_deref().ok_or_else(|| {
            ConfigError::new(
                "RATATOSKR__LEGACY__SOURCE_DATABASE_URL",
                "must be configured for legacy import",
            )
        })
    }
}

impl fmt::Debug for LegacyConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyConfig")
            .field(
                "source_database_url",
                &self.source_database_url.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
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

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.credentials.validate()?;
        self.github_oauth.validate()?;
        self.bus.validate(self.limits.shutdown_timeout_ms)?;
        if self.api.listen_address == self.admin.listen_address {
            return Err(ConfigError::new(
                "RATATOSKR__API__LISTEN_ADDRESS",
                "must differ from the operator listener",
            ));
        }
        validate_provider_base_url("RATATOSKR__PROVIDER__BASE_URL", &self.provider.base_url)
    }

    /// Validates dependencies required only by the long-running serving role.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when bus identity or credential encryption is incomplete.
    pub fn validate_for_serving(&self) -> Result<(), ConfigError> {
        let seed_path = self.bus.nkey_seed_path()?;
        std::fs::File::open(seed_path).map_err(|_| {
            ConfigError::new(
                "RATATOSKR__BUS__NKEY_SEED_PATH",
                "must name a readable protected seed file",
            )
        })?;
        self.credentials.encryption_key().map(|_| ())
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
    if apply_bus_entry(&mut config.bus, key, value)? {
        return Ok(());
    }

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
        "RATATOSKR__API__LISTEN_ADDRESS" => {
            let address = value
                .parse::<SocketAddr>()
                .map_err(|_| ConfigError::new(key, "must be a socket address"))?;
            if !address.ip().is_loopback() || address.port() == 0 {
                return Err(ConfigError::new(
                    key,
                    "must be a loopback address with a port",
                ));
            }
            config.api.listen_address = address;
        }
        "RATATOSKR__PROVIDER__BASE_URL" => {
            validate_provider_base_url(key, value)?;
            value
                .trim_end_matches('/')
                .clone_into(&mut config.provider.base_url);
        }
        "RATATOSKR__STORAGE__DATABASE_URL" => {
            value
                .parse::<sqlx::postgres::PgConnectOptions>()
                .map_err(|_| ConfigError::new(key, "must be a PostgreSQL connection URL"))?;
            value.clone_into(&mut config.storage.database_url);
        }
        "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX" => {
            config.credentials.encryption_key_hex = Some(value.to_owned());
        }
        "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_PATH" => {
            if !std::path::Path::new(value).is_absolute() {
                return Err(ConfigError::new(
                    key,
                    "must be an absolute protected file path",
                ));
            }
            config.credentials.encryption_key_path = Some(value.to_owned());
        }
        "RATATOSKR__CREDENTIALS__KEY_VERSION" => {
            config.credentials.key_version = Some(value.to_owned());
        }
        "RATATOSKR__GITHUB_OAUTH__CLIENT_ID" => {
            config.github_oauth.client_id = Some(value.to_owned());
        }
        "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET" => {
            config.github_oauth.client_secret = Some(SecretString::from(value.to_owned()));
        }
        "RATATOSKR__LEGACY__SOURCE_DATABASE_URL" => {
            value
                .parse::<sqlx::postgres::PgConnectOptions>()
                .map_err(|_| ConfigError::new(key, "must be a PostgreSQL connection URL"))?;
            config.legacy.source_database_url = Some(value.to_owned());
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

fn apply_bus_entry(config: &mut BusConfig, key: &str, value: &str) -> Result<bool, ConfigError> {
    match key {
        "RATATOSKR__BUS__URL" => {
            validate_bus_url(key, value)?;
            value.clone_into(&mut config.url);
        }
        "RATATOSKR__BUS__NKEY_SEED_PATH" => {
            if !std::path::Path::new(value).is_absolute() {
                return Err(ConfigError::new(
                    key,
                    "must be an absolute protected file path",
                ));
            }
            config.nkey_seed_path = Some(value.to_owned());
        }
        "RATATOSKR__BUS__CONNECT_TIMEOUT_MS" => {
            config.connect_timeout_ms = parse_positive(key, value)?;
        }
        "RATATOSKR__BUS__PUBLISH_ACK_TIMEOUT_MS" => {
            config.publish_ack_timeout_ms = parse_positive(key, value)?;
        }
        "RATATOSKR__BUS__POLL_INTERVAL_MS" => {
            config.poll_interval_ms = parse_positive(key, value)?;
        }
        "RATATOSKR__BUS__LEASE_MS" => config.lease_ms = parse_positive(key, value)?,
        "RATATOSKR__BUS__BATCH_SIZE" => config.batch_size = parse_positive(key, value)?,
        "RATATOSKR__BUS__MAX_ATTEMPTS" => config.max_attempts = parse_positive(key, value)?,
        "RATATOSKR__BUS__WORKER_JOIN_TIMEOUT_MS" => {
            config.worker_join_timeout_ms = parse_positive(key, value)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn validate_bus_url(key: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| ConfigError::new(key, "must be a loopback NATS origin"))?;
    let valid = parsed.scheme() == "nats"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.path() == ""
        && parsed.port().is_some()
        && parsed
            .host_str()
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
    if valid {
        Ok(())
    } else {
        Err(ConfigError::new(key, "must be a loopback NATS origin"))
    }
}

fn validate_provider_base_url(key: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| ConfigError::new(key, "must be a bounded provider origin"))?;
    let origin_only = parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.path() == "/";
    let permitted_scheme = match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => parsed
            .host_str()
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|address| address.is_loopback()),
        _ => false,
    };
    if origin_only && permitted_scheme {
        Ok(())
    } else {
        Err(ConfigError::new(
            key,
            "must be an HTTPS origin or numeric loopback HTTP origin",
        ))
    }
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

fn valid_oauth_client_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl Default for Config {
    fn default() -> Self {
        Self {
            admin: AdminConfig {
                listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9095),
            },
            api: ApiConfig {
                listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8092),
            },
            provider: ProviderConfig {
                base_url: "https://api.github.com".to_owned(),
            },
            bus: BusConfig {
                url: "nats://127.0.0.1:4222".to_owned(),
                nkey_seed_path: None,
                connect_timeout_ms: 5_000,
                publish_ack_timeout_ms: 5_000,
                poll_interval_ms: 250,
                lease_ms: 30_000,
                batch_size: 16,
                max_attempts: 10,
                worker_join_timeout_ms: 120_000,
            },
            storage: StorageConfig {
                database_url: "postgres://github:github@127.0.0.1:5435/github".to_owned(),
            },
            credentials: CredentialsConfig {
                encryption_key_hex: None,
                encryption_key_path: None,
                key_version: None,
            },
            github_oauth: GithubOAuthConfig {
                client_id: None,
                client_secret: None,
            },
            legacy: LegacyConfig {
                source_database_url: None,
            },
            limits: Limits {
                database_connections: 8,
                database_acquire_timeout_ms: 5_000,
                shutdown_timeout_ms: 125_000,
            },
        }
    }
}
