#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Ratatoskr GitHub Catalog service process.

use std::future::IntoFuture as _;
use std::io::Read as _;
use std::io::Write as _;
use std::time::Duration;

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::{
    Config, CredentialError, Database, LegacyImportError, LegacyImportRequest, LegacyOwnerMap,
    LegacyShadowError, LegacySource, LegacySourceError, VerifiedGithubAccount,
    generate_legacy_shadow_report, import_legacy_snapshot, init_telemetry,
    legacy_cutover_readiness, legacy_shadow_account_ids, load_active_pat, register_pat,
    run_full_snapshot, run_star_list_snapshot,
};
use ratatoskr_github_catalog_service::{
    Lifecycle, OperatorCommand, OperatorCommandError, admin_router, parse_operator_command,
};
use secrecy::{ExposeSecret as _, SecretString};
use uuid::Uuid;

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
    /// A command was refused before performing work.
    #[error("operator command is invalid: {0}")]
    Command(#[from] OperatorCommandError),
    /// The account identifier was malformed.
    #[error("operator account identifier is invalid")]
    AccountIdentifier,
    /// Standard input could not provide a replacement PAT.
    #[error("replacement PAT input was unavailable")]
    PatInput(#[source] std::io::Error),
    /// Standard input did not contain a PAT.
    #[error("replacement PAT input was empty")]
    EmptyPatInput,
    /// Provider credential verification failed without exposing provider details.
    #[error("provider credential verification failed")]
    Provider(#[source] ratatoskr_github_catalog::provider::ProviderError),
    /// Credential registration could not complete.
    #[error("credential registration failed: {0}")]
    Credential(#[from] CredentialError),
    /// The owner-map file could not be read.
    #[error("legacy owner map could not be read")]
    OwnerMapRead(#[source] std::io::Error),
    /// The owner-map document was invalid.
    #[error("legacy owner map was refused")]
    OwnerMap(#[source] ratatoskr_github_catalog::LegacyOwnerMapError),
    /// The isolated legacy source could not be used.
    #[error("legacy source operation failed")]
    LegacySource(#[source] LegacySourceError),
    /// The catalog import could not complete.
    #[error("legacy import failed")]
    LegacyImport(#[source] LegacyImportError),
    /// The shadow report could not complete.
    #[error("legacy shadow report failed")]
    LegacyShadow(#[source] LegacyShadowError),
    /// A full star snapshot could not run for a reauthorized account.
    #[error("legacy shadow star snapshot failed")]
    ShadowStarSnapshot(#[source] ratatoskr_github_catalog::SnapshotError),
    /// A native-list snapshot could not run for a reauthorized account.
    #[error("legacy shadow list snapshot failed")]
    ShadowListSnapshot(#[source] ratatoskr_github_catalog::StarListsError),
    /// Operator output could not be written atomically.
    #[error("operator output could not be written")]
    Output(#[source] std::io::Error),
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
    let command = parse_operator_command(std::env::args())?;
    let config = Config::load()?;
    match command {
        OperatorCommand::CheckConfig => Ok(()),
        OperatorCommand::ReconnectPat { account_id } => {
            register_replacement_pat(&config, &account_id).await
        }
        OperatorCommand::ImportLegacy {
            source_id,
            owner_map_path,
        } => import_legacy(&config, &source_id, &owner_map_path).await,
        OperatorCommand::ShadowLegacy { source_id } => shadow_legacy(&config, &source_id).await,
        OperatorCommand::CutoverReadiness { source_id } => {
            cutover_readiness(&config, &source_id).await
        }
        OperatorCommand::Serve => serve(config).await,
    }
}

async fn cutover_readiness(config: &Config, source_id: &str) -> Result<(), ProcessError> {
    let database = connect_database(config).await?;
    let readiness = legacy_cutover_readiness(&database, source_id)
        .await
        .map_err(ProcessError::LegacyShadow);
    database.close().await;
    let readiness = readiness?;
    write_operator_output(&format!(
        "{{\"report_id\":\"{}\",\"report_digest\":\"{}\",\"cutover_reviewable\":true}}",
        readiness.report_id, readiness.report_digest,
    ))
}

async fn import_legacy(
    config: &Config,
    source_id: &str,
    owner_map_path: &str,
) -> Result<(), ProcessError> {
    let map_document =
        std::fs::read_to_string(owner_map_path).map_err(ProcessError::OwnerMapRead)?;
    let owner_map = LegacyOwnerMap::from_json(&map_document).map_err(ProcessError::OwnerMap)?;
    let source = LegacySource::connect(
        config.legacy.source_database_url()?,
        config.limits.database_connections,
        Duration::from_millis(config.limits.database_acquire_timeout_ms),
    )
    .await
    .map_err(ProcessError::LegacySource)?;
    let snapshot = source
        .read_snapshot()
        .await
        .map_err(ProcessError::LegacySource);
    source.close().await;
    let snapshot = snapshot?;
    let database = connect_database(config).await?;
    let result = import_legacy_snapshot(
        &database,
        LegacyImportRequest {
            source_id: source_id.to_owned(),
            owner_map,
            snapshot,
        },
    )
    .await
    .map_err(ProcessError::LegacyImport);
    database.close().await;
    let result = result?;
    write_operator_output(&format!(
        "{{\"import_run_id\":\"{}\",\"accounts_imported\":{},\"repositories_imported\":{},\"star_claims_imported\":{},\"list_claims_imported\":{}}}",
        result.import_run_id,
        result.accounts_imported,
        result.repositories_imported,
        result.star_claims_imported,
        result.list_claims_imported,
    ))
}

async fn shadow_legacy(config: &Config, source_id: &str) -> Result<(), ProcessError> {
    let database = connect_database(config).await?;
    let accounts = legacy_shadow_account_ids(&database, source_id)
        .await
        .map_err(ProcessError::LegacyShadow)?;
    if !accounts.is_empty() {
        let key = config.credentials.encryption_key()?;
        let ledger = RateLimitLedger::new();
        for account_id in accounts {
            let credential = load_active_pat(&database, account_id, &key)
                .await
                .map_err(ProcessError::Credential)?;
            let gateway = ReqwestGithubApi::for_base_url("https://api.github.com")
                .map_err(ProcessError::Provider)?
                .authenticated(credential);
            let token = TokenRef::from_label(account_id.to_string());
            let _full = run_full_snapshot(&database, &gateway, &ledger, &token, account_id)
                .await
                .map_err(ProcessError::ShadowStarSnapshot)?;
            let _lists = run_star_list_snapshot(&database, &gateway, &ledger, &token, account_id)
                .await
                .map_err(ProcessError::ShadowListSnapshot)?;
        }
    }
    let report = generate_legacy_shadow_report(&database, source_id)
        .await
        .map_err(ProcessError::LegacyShadow);
    database.close().await;
    let report = report?;
    let body = report
        .canonical_json()
        .map_err(ProcessError::LegacyShadow)?;
    write_operator_output(&body)
}

fn write_operator_output(body: &str) -> Result<(), ProcessError> {
    let mut stdout = std::io::stdout().lock();
    let output = format!("{body}\n");
    stdout
        .write_all(output.as_bytes())
        .map_err(ProcessError::Output)
}

async fn serve(config: Config) -> Result<(), ProcessError> {
    init_telemetry()?;
    let database = connect_database(&config).await?;

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

async fn register_replacement_pat(config: &Config, account_id: &str) -> Result<(), ProcessError> {
    let account_id = Uuid::parse_str(account_id).map_err(|_| ProcessError::AccountIdentifier)?;
    let key = config.credentials.encryption_key()?;
    let pat = read_replacement_pat()?;
    let database = connect_database(config).await?;
    let result = async {
        let gateway = ReqwestGithubApi::for_base_url("https://api.github.com")
            .map_err(ProcessError::Provider)?;
        let authenticated = gateway
            .authenticate_pat(pat.expose_secret())
            .await
            .map_err(ProcessError::Provider)?;
        register_pat(
            &database,
            account_id,
            pat,
            &key,
            &VerifiedGithubAccount {
                provider_user_id: authenticated.provider_user_id,
                login: authenticated.login,
                granted_scopes: authenticated.granted_scopes,
            },
        )
        .await
        .map_err(ProcessError::Credential)
    }
    .await;
    database.close().await;
    result
}

async fn connect_database(config: &Config) -> Result<Database, ProcessError> {
    let database = Database::connect(
        &config.storage.database_url,
        config.limits.database_connections,
        Duration::from_millis(config.limits.database_acquire_timeout_ms),
    )
    .await?;
    if let Err(error) = database.apply_schema().await {
        database.close().await;
        return Err(ProcessError::Database(error));
    }
    Ok(database)
}

fn read_replacement_pat() -> Result<SecretString, ProcessError> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(ProcessError::PatInput)?;
    let value = input.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() {
        return Err(ProcessError::EmptyPatInput);
    }
    Ok(SecretString::from(value))
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
