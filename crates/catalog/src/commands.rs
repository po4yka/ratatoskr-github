//! Consumption of this service's own synchronization commands under the
//! platform scheduler command grammar.
//!
//! The platform scheduler publishes `cmd.github.sync.requested.v1`; this
//! module is the catalog's receiving boundary. It validates the published
//! envelope strictly before any effect, claims the command durably through
//! the owned inbox so `JetStream`'s at-least-once redelivery performs no
//! second effect, dispatches the requested scan mode for the named
//! connected account, and escalates an ordering gap found during a
//! commanded incremental scan into an immediate full rescan. Schedule
//! registration stays outside this service entirely: an operator registers
//! schedules through the mechanism platform documents, never through code
//! here.

use serde_json::Value;
use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::identity::IdentityError;
use crate::incremental::{IncrementalScanError, IncrementalScanOutcome, run_incremental_scan};
use crate::provider::{GithubApi, ProviderError};
use crate::rate_limit::{RateLimitLedger, TokenRef};
use crate::snapshot::{FullSnapshotOutcome, SnapshotError, run_full_snapshot};

/// The contract type this catalog consumes its own sync commands as,
/// published by the platform scheduler to `cmd.` plus this name.
pub const SYNC_REQUESTED_TYPE: &str = "github.sync.requested.v1";

/// The scan mode a sync command requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedSyncMode {
    /// Ingest only what is newer than the account's watermark (default).
    Incremental,
    /// Enumerate the complete starred listing as reconciliation authority.
    Full,
}

/// What handling one accepted sync command established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandledSyncCommand {
    /// The identity of the command that was handled.
    pub command_id: Uuid,
    /// The local account the command targeted.
    pub account_id: Uuid,
    /// The mode the command requested.
    pub requested_mode: RequestedSyncMode,
    /// The incremental scan outcome when incremental mode ran - including
    /// a gap detection that triggered the chained rescan below.
    pub incremental: Option<IncrementalScanOutcome>,
    /// The full-snapshot outcome when a full snapshot ran, whether by
    /// direct request, by baseline-less deferral, or as a gap-forced rescan.
    pub full: Option<FullSnapshotOutcome>,
}

/// How the delivery of one command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumedSyncCommand {
    /// The command was claimed and dispatched to completion.
    Handled(HandledSyncCommand),
    /// The command's identity was already claimed; nothing ran again.
    Duplicate,
}

/// Failures of sync-command consumption beyond its outcomes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncCommandError {
    /// The envelope violates the command grammar this service consumes.
    #[error("command envelope is invalid: {0}")]
    Invalid(&'static str),
    /// The command names an account this catalog does not know.
    #[error("command names an account that does not exist")]
    UnknownAccount,
    /// The command names an account that is not currently connected.
    #[error("command names an account that is not connected")]
    AccountNotConnected,
    /// Identity or alias handling failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Persistence failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// The provider exchange failed or was unclassifiable.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The dispatched full-snapshot flow failed.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// The dispatched incremental-scan flow failed.
    #[error(transparent)]
    Incremental(#[from] IncrementalScanError),
}

/// What strict envelope validation extracts for dispatch.
struct ValidatedEnvelope {
    /// The parsed envelope, stored verbatim as the inbox claim's payload.
    raw: Value,
    /// The parseable command identity that keys the inbox claim.
    command_id: Uuid,
    /// The account owner reference the payload names.
    owner_ref: String,
    /// The scan mode the payload requests, defaulting to incremental.
    requested_mode: RequestedSyncMode,
}

/// Validates and consumes one delivered sync command envelope.
///
/// Validation happens entirely before any effect: a rejected envelope
/// leaves no rows and starts no scans. An accepted envelope is claimed in
/// the owned inbox keyed by its command identity, then dispatched; a
/// redelivered identity short-circuits as [`ConsumedSyncCommand::Duplicate`].
///
/// # Errors
///
/// Returns [`SyncCommandError`] for grammar violations, unknown or
/// disconnected accounts, and provider or persistence failures.
pub async fn handle_sync_command<G>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    envelope_json: &str,
) -> Result<ConsumedSyncCommand, SyncCommandError>
where
    G: GithubApi,
{
    let validated = validate_envelope(envelope_json)?;
    let account_id = resolve_account(database, &validated.owner_ref).await?;

    // Claim the delivery before dispatch so redelivery of the same identity
    // can never run a second scan.
    let claimed: Option<Uuid> = sqlx::query_scalar(
        "insert into github_catalog.inbox_events (message_id, subject, payload)
         values ($1, $2, $3)
         on conflict (message_id) do nothing
         returning message_id",
    )
    .bind(validated.command_id)
    .bind(SYNC_REQUESTED_TYPE)
    .bind(&validated.raw)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    if claimed.is_none() {
        return Ok(ConsumedSyncCommand::Duplicate);
    }

    let mut handled = HandledSyncCommand {
        command_id: validated.command_id,
        account_id,
        requested_mode: validated.requested_mode,
        incremental: None,
        full: None,
    };
    match validated.requested_mode {
        RequestedSyncMode::Incremental => {
            let outcome =
                run_incremental_scan(database, gateway, ledger, token, account_id).await?;
            if let IncrementalScanOutcome::GapDetected { .. } = &outcome {
                // A commanded scan that cannot prove coverage converges the
                // account immediately instead of stalling until the next
                // periodic full pass.
                handled.full =
                    Some(run_full_snapshot(database, gateway, ledger, token, account_id).await?);
            }
            if let IncrementalScanOutcome::DeferredToFull(deferred) = &outcome {
                handled.full = Some(deferred.clone());
            }
            handled.incremental = Some(outcome);
        }
        RequestedSyncMode::Full => {
            handled.full =
                Some(run_full_snapshot(database, gateway, ledger, token, account_id).await?);
        }
    }

    sqlx::query(
        "update github_catalog.inbox_events set consumed_at = now()
         where message_id = $1",
    )
    .bind(validated.command_id)
    .execute(database.pool())
    .await
    .map_err(PersistenceError::Query)?;

    Ok(ConsumedSyncCommand::Handled(handled))
}

/// Parses one delivered envelope against the grammar this service consumes,
/// without touching any state: type equality, UUID identity, tenant shape,
/// presence of every required member, and the payload's account plus mode.
fn validate_envelope(envelope_json: &str) -> Result<ValidatedEnvelope, SyncCommandError> {
    let envelope: Value = serde_json::from_str(envelope_json)
        .map_err(|_| SyncCommandError::Invalid("the envelope is not valid JSON"))?;
    let members = envelope.as_object().ok_or(SyncCommandError::Invalid(
        "the envelope must be a JSON object",
    ))?;

    let command_type = require_str(members, "command_type", "command_type must be a string")?;
    if command_type != SYNC_REQUESTED_TYPE {
        return Err(SyncCommandError::Invalid(
            "command_type is foreign to this service",
        ));
    }
    let command_id = Uuid::parse_str(require_str(
        members,
        "command_id",
        "command_id must be a UUID string",
    )?)
    .map_err(|_| SyncCommandError::Invalid("command_id must be a UUID string"))?;
    let tenant = require_str(members, "tenant_id", "tenant_id must be a string")?;
    let tenant_uuid = tenant
        .strip_prefix("user:")
        .ok_or(SyncCommandError::Invalid(
            "tenant_id must take the form user:<uuid>",
        ))?;
    Uuid::parse_str(tenant_uuid)
        .map_err(|_| SyncCommandError::Invalid("tenant_id must take the form user:<uuid>"))?;
    for (key, message) in [
        (
            "requested_at",
            "requested_at must be a present string member",
        ),
        (
            "operation_id",
            "operation_id must be a present string member",
        ),
        (
            "correlation_id",
            "correlation_id must be a present string member",
        ),
        (
            "idempotency_key",
            "idempotency_key must be a present string member",
        ),
    ] {
        require_str(members, key, message)?;
    }

    let payload = members
        .get("payload")
        .and_then(Value::as_object)
        .ok_or(SyncCommandError::Invalid("payload must be a JSON object"))?;
    let owner_ref = require_str(payload, "account", "payload must name an account")?;
    ensure_owner_ref(owner_ref)?;
    let requested_mode = requested_mode(payload)?;

    Ok(ValidatedEnvelope {
        raw: envelope.clone(),
        command_id,
        owner_ref: owner_ref.to_owned(),
        requested_mode,
    })
}

/// Resolves the payload's owner reference to a connected local account.
async fn resolve_account(database: &Database, owner_ref: &str) -> Result<Uuid, SyncCommandError> {
    let account: (Uuid, String) = sqlx::query_as(
        "select account_id, status from github_catalog.github_accounts
         where owner_ref = $1",
    )
    .bind(owner_ref)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?
    .ok_or(SyncCommandError::UnknownAccount)?;
    if account.1 != "connected" {
        return Err(SyncCommandError::AccountNotConnected);
    }
    Ok(account.0)
}

/// Reads one required string member or explains which one went missing.
fn require_str<'a>(
    members: &'a serde_json::Map<String, Value>,
    key: &str,
    message: &'static str,
) -> Result<&'a str, SyncCommandError> {
    members
        .get(key)
        .and_then(Value::as_str)
        .ok_or(SyncCommandError::Invalid(message))
}

/// Reads the payload's optional mode member against its vocabulary.
fn requested_mode(
    payload: &serde_json::Map<String, Value>,
) -> Result<RequestedSyncMode, SyncCommandError> {
    match payload.get("mode") {
        None | Some(Value::Null) => Ok(RequestedSyncMode::Incremental),
        Some(Value::String(text)) => match text.as_str() {
            "incremental" => Ok(RequestedSyncMode::Incremental),
            "full" => Ok(RequestedSyncMode::Full),
            _ => Err(SyncCommandError::Invalid(
                "payload mode must be incremental or full",
            )),
        },
        Some(_) => Err(SyncCommandError::Invalid(
            "payload mode must be incremental or full",
        )),
    }
}

/// Checks the payload account against the owner-reference grammar the
/// accounts table enforces (`^[a-z][a-z0-9-]{1,63}$`), without a regular
/// expression engine.
fn ensure_owner_ref(value: &str) -> Result<(), SyncCommandError> {
    let mut chars = value.chars();
    let head_ok = matches!(chars.next(), Some(first) if first.is_ascii_lowercase());
    let tail_ok =
        chars.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '-');
    let length_ok = (2..=64).contains(&value.chars().count());
    if head_ok && tail_ok && length_ok {
        return Ok(());
    }
    Err(SyncCommandError::Invalid(
        "payload account does not match the owner reference grammar",
    ))
}
