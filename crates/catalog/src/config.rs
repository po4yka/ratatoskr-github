use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
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
    /// Private service-to-service listener configuration.
    pub internal_api: InternalApiConfig,
    /// GitHub provider endpoint configuration.
    pub provider: ProviderConfig,
    /// Owned durable storage configuration.
    pub storage: StorageConfig,
    /// Credential encryption configuration.
    pub credentials: CredentialsConfig,
    /// GitHub OAuth application configuration.
    pub github_oauth: GithubOAuthConfig,
    /// Service-to-service authentication configuration.
    pub service_auth: ServiceAuthConfig,
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

/// Private authenticated listener used only between fleet services.
#[derive(Debug, Clone, Serialize)]
pub struct InternalApiConfig {
    /// Socket address for service-authenticated internal routes.
    pub listen_address: SocketAddr,
}

/// File-backed credentials for internal service callers.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ServiceAuthConfig {
    /// Absolute path to the bearer token shared only with Knowledge.
    pub knowledge_token_file: Option<PathBuf>,
}

/// Bounded provider endpoint configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderConfig {
    /// HTTPS provider origin, or a numeric loopback HTTP origin for tests.
    pub base_url: String,
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
        let value = self.encryption_key_hex.as_deref().ok_or_else(|| {
            ConfigError::new(
                "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
                "must be configured for credential registration",
            )
        })?;
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
        match (&self.encryption_key_hex, &self.key_version) {
            (None, None) => Ok(()),
            (Some(_), Some(_)) => self.encryption_key().map(|_| ()),
            (None, Some(_)) => Err(ConfigError::new(
                "RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
                "must be configured with RATATOSKR__CREDENTIALS__KEY_VERSION",
            )),
            (Some(_), None) => Err(ConfigError::new(
                "RATATOSKR__CREDENTIALS__KEY_VERSION",
                "must be configured with RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX",
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
        if self
            .service_auth
            .knowledge_token_file
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(ConfigError::new(
                "RATATOSKR__SERVICE_AUTH__KNOWLEDGE_TOKEN_FILE",
                "must be an absolute secret-file path",
            ));
        }
        if self.api.listen_address == self.admin.listen_address {
            return Err(ConfigError::new(
                "RATATOSKR__API__LISTEN_ADDRESS",
                "must differ from the operator listener",
            ));
        }
        if self.internal_api.listen_address == self.admin.listen_address
            || self.internal_api.listen_address == self.api.listen_address
        {
            return Err(ConfigError::new(
                "RATATOSKR__INTERNAL_API__LISTEN_ADDRESS",
                "must differ from the operator and domain listeners",
            ));
        }
        validate_provider_base_url("RATATOSKR__PROVIDER__BASE_URL", &self.provider.base_url)
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
        "RATATOSKR__INTERNAL_API__LISTEN_ADDRESS" => {
            let address = value
                .parse::<SocketAddr>()
                .map_err(|_| ConfigError::new(key, "must be a socket address"))?;
            if address.port() == 0 || !is_private_listener(address.ip()) {
                return Err(ConfigError::new(
                    key,
                    "must be a private, loopback, or container-wildcard address with a port",
                ));
            }
            config.internal_api.listen_address = address;
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
        "RATATOSKR__CREDENTIALS__KEY_VERSION" => {
            config.credentials.key_version = Some(value.to_owned());
        }
        "RATATOSKR__GITHUB_OAUTH__CLIENT_ID" => {
            config.github_oauth.client_id = Some(value.to_owned());
        }
        "RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET" => {
            config.github_oauth.client_secret = Some(SecretString::from(value.to_owned()));
        }
        "RATATOSKR__SERVICE_AUTH__KNOWLEDGE_TOKEN_FILE" => {
            config.service_auth.knowledge_token_file = Some(PathBuf::from(value));
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

fn is_private_listener(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_unspecified()
                || address.is_loopback()
                || address.is_private()
                || address.is_link_local()
        }
        IpAddr::V6(address) => {
            address.is_unspecified()
                || address.is_loopback()
                || address.is_unique_local()
                || address.is_unicast_link_local()
        }
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
            internal_api: InternalApiConfig {
                listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8093),
            },
            provider: ProviderConfig {
                base_url: "https://api.github.com".to_owned(),
            },
            storage: StorageConfig {
                database_url: "postgres://github:github@127.0.0.1:5435/github".to_owned(),
            },
            credentials: CredentialsConfig {
                encryption_key_hex: None,
                key_version: None,
            },
            github_oauth: GithubOAuthConfig {
                client_id: None,
                client_secret: None,
            },
            service_auth: ServiceAuthConfig::default(),
            legacy: LegacyConfig {
                source_database_url: None,
            },
            limits: Limits {
                database_connections: 8,
                database_acquire_timeout_ms: 5_000,
                shutdown_timeout_ms: 10_000,
            },
        }
    }
}
