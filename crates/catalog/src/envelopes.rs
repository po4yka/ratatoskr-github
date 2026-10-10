//! Complete bus envelopes for the two producers' outbox rows (XR-021 CONTRACTS.md S02 and S09).
//!
//! An outbox row stores the whole canonical envelope, never a bare payload, and its `message_id`
//! is the envelope's `event_id` or `command_id` so the relay can use it as the `Nats-Msg-Id`.

use ratatoskr_backup_contracts::{DesiredBackupPolicy, VaultBackupPolicyApplyRequested};
use ratatoskr_event_envelope::{
    CommandEnvelope, CommandPayload as _, EnvelopeSchemaVersion, EventEnvelope, EventPayload as _,
    ProducerName,
};
use ratatoskr_github_contracts::RepositoryAnalysisRequested;
use ratatoskr_identifiers::{CommandId, EntityRef, EventId, Extensions, WireTimestamp};
use uuid::Uuid;

/// Class-prefixed outbox subject of the repository analysis request fact.
pub(crate) const ANALYSIS_REQUESTED_SUBJECT: &str =
    "evt.knowledge.repository_analysis.requested.v1";
/// Class-prefixed outbox subject of the Vault policy command.
pub(crate) const BACKUP_POLICY_APPLY_SUBJECT: &str = "cmd.vault.backup_policy.apply_requested.v1";

const PRODUCER: &str = "ratatoskr-github";

/// A contract value that could not be formed into an envelope.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EnvelopeBuildError {
    /// An identity or reference violated the published identifier contract.
    #[error("an envelope identity violates the published contract")]
    Identity,
    /// The envelope could not be serialized for the outbox.
    #[error("the envelope could not be serialized")]
    Encode(#[source] serde_json::Error),
}

/// Wraps a repository analysis request in the event envelope stored in the outbox.
pub(crate) fn analysis_requested_envelope(
    event_id: Uuid,
    request: &RepositoryAnalysisRequested,
) -> Result<serde_json::Value, EnvelopeBuildError> {
    let mut envelope = EventEnvelope {
        event_id: EventId(event_id),
        event_type: RepositoryAnalysisRequested::event_type(),
        occurred_at: WireTimestamp::now(),
        producer: ProducerName::parse(PRODUCER).map_err(|_| EnvelopeBuildError::Identity)?,
        aggregate_id: EntityRef::parse(&format!("repository:{}", request.repository_id))
            .map_err(|_| EnvelopeBuildError::Identity)?,
        correlation_id: EntityRef::parse(&format!("repository_analysis:{}", request.request_id))
            .map_err(|_| EnvelopeBuildError::Identity)?,
        causation_id: None,
        tenant_id: Some(request.owner),
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload: serde_json::Map::new(),
        extensions: Extensions::default(),
    };
    envelope
        .set_payload(request)
        .map_err(|_| EnvelopeBuildError::Identity)?;
    serde_json::to_value(&envelope).map_err(EnvelopeBuildError::Encode)
}

/// Wraps one desired policy version in the catalog-wide command envelope stored in the outbox.
pub(crate) fn backup_policy_apply_envelope(
    command_id: Uuid,
    policy: DesiredBackupPolicy,
) -> Result<serde_json::Value, EnvelopeBuildError> {
    let version_ref = EntityRef::parse(&format!("backup_policy:{}", policy.policy_version))
        .map_err(|_| EnvelopeBuildError::Identity)?;
    let mut envelope = CommandEnvelope {
        command_id: CommandId(command_id),
        command_type: VaultBackupPolicyApplyRequested::command_type(),
        issued_at: WireTimestamp::now(),
        producer: ProducerName::parse(PRODUCER).map_err(|_| EnvelopeBuildError::Identity)?,
        aggregate_id: version_ref.clone(),
        correlation_id: version_ref,
        causation_id: None,
        tenant_id: None,
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload: serde_json::Map::new(),
        extensions: Extensions::default(),
    };
    envelope
        .set_payload(&VaultBackupPolicyApplyRequested {
            policy,
            extensions: Extensions::default(),
        })
        .map_err(|_| EnvelopeBuildError::Identity)?;
    serde_json::to_value(&envelope).map_err(EnvelopeBuildError::Encode)
}
