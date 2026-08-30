//! Derivation, durable publication, and acknowledgment projection for Vault policy intent.

use crate::database::{Database, PersistenceError};
use ratatoskr_backup_contracts::{
    BackupExclusion, BackupExclusionScope, BackupPriorityHint, DesiredBackupPolicy,
    ExclusionExpression, MirrorCadence, PolicyAcknowledged, PolicyOutcome, RepositoryBackupEntry,
};
use ratatoskr_event_envelope::ProducerName;
use ratatoskr_identifiers::{EntityRef, Extensions, WireTimestamp};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{InboxClaimOutcome, InboxDelivery, claim_inbox_delivery};

/// The trailing delay used to coalesce catalog changes into one publication.
pub const POLICY_DEBOUNCE: Duration = Duration::seconds(60);
const ACKNOWLEDGED_SUBJECT: &str = "evt.vault.backup_policy.acknowledged.v1";

/// One catalog entry considered while deriving the mirror policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BackupPolicyInput {
    repository_ref: String,
    included: bool,
    cadence: String,
    priority: String,
    size_hint_bytes: Option<u64>,
    exclusions: Vec<(String, String)>,
}
impl BackupPolicyInput {
    /// Creates a mirror-governed input.
    #[must_use]
    pub fn mirror(
        repository_ref: &str,
        cadence: &str,
        priority: &str,
        size_hint_bytes: Option<u64>,
        exclusions: Vec<(&str, &str)>,
    ) -> Self {
        Self {
            repository_ref: repository_ref.to_owned(),
            included: true,
            cadence: cadence.to_owned(),
            priority: priority.to_owned(),
            size_hint_bytes,
            exclusions: exclusions
                .into_iter()
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .collect(),
        }
    }
    /// Creates an excluded input.
    #[must_use]
    pub fn excluded(repository_ref: &str) -> Self {
        Self {
            repository_ref: repository_ref.to_owned(),
            included: false,
            cadence: "daily".to_owned(),
            priority: "standard".to_owned(),
            size_hint_bytes: None,
            exclusions: Vec::new(),
        }
    }
}

/// A malformed catalog value or durable operation failure.
#[derive(Debug, thiserror::Error)]
pub enum BackupPolicyError {
    /// A contract field could not be formed.
    #[error("backup policy contains an invalid {field}")]
    Invalid {
        /// The invalid contract field.
        field: &'static str,
    },
    /// Catalog persistence rejected the policy operation.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// A typed policy could not be serialized for the outbox.
    #[error("backup policy could not be serialized")]
    Serialization(#[source] serde_json::Error),
}
/// Outcome of one due-publication attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationOutcome {
    /// No dirty policy is due yet.
    Pending,
    /// The latest desired state was already published.
    Unchanged,
    /// One new immutable policy version was written to the outbox.
    Published {
        /// The strictly increasing document version.
        policy_version: u64,
    },
}
/// Result of one Vault acknowledgment delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackOutcome {
    /// The delivery was recorded once.
    Recorded,
    /// The message ID was already consumed.
    Duplicate,
}
/// Operator-visible Vault policy feedback; it is not backup health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFeedback {
    /// The policy version Vault answered.
    pub policy_version: u64,
    /// Whether Vault accepted or rejected it.
    pub outcome: PolicyOutcome,
    /// Vault's prior fully applied version.
    pub last_applied_policy_version: u64,
    /// Machine-actionable reasons serialized by the shared contract.
    pub reasons: serde_json::Value,
}

/// Derives a sorted, contract-valid desired mirror policy.
/// # Errors
/// Returns [`BackupPolicyError`] if catalog intent cannot be represented safely.
pub fn derive_backup_policy(
    policy_version: u64,
    inputs: &[BackupPolicyInput],
) -> Result<DesiredBackupPolicy, BackupPolicyError> {
    let mut repositories = inputs
        .iter()
        .filter(|input| input.included)
        .map(to_entry)
        .collect::<Result<Vec<_>, _>>()?;
    repositories.sort_by_key(|entry| entry.repository_ref.to_wire());
    let policy = DesiredBackupPolicy {
        policy_version,
        producing_service: ProducerName::parse("ratatoskr-github")
            .map_err(|_| invalid("producing_service"))?,
        produced_at: WireTimestamp::now(),
        repositories,
        extensions: Extensions::default(),
    };
    policy.validate().map_err(|_| invalid("policy"))?;
    Ok(policy)
}
/// Marks committed catalog state as needing a trailing-edge policy reconciliation.
/// # Errors
/// Returns [`BackupPolicyError`] when the cursor cannot be updated.
pub async fn mark_backup_policy_dirty(
    database: &Database,
    now: OffsetDateTime,
) -> Result<(), BackupPolicyError> {
    sqlx::query("insert into github_catalog.backup_policy_publication_cursor (scope, dirty_generation, published_generation, not_before) values ('catalog', 1, 0, $1 + interval '60 seconds') on conflict (scope) do update set dirty_generation = github_catalog.backup_policy_publication_cursor.dirty_generation + 1, not_before = excluded.not_before").bind(now).execute(database.pool()).await.map_err(PersistenceError::Query)?;
    Ok(())
}

/// Marks the policy dirty inside the caller's catalog transaction.
pub(crate) async fn mark_backup_policy_dirty_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), PersistenceError> {
    sqlx::query(
        "insert into github_catalog.backup_policy_publication_cursor
             (scope, dirty_generation, published_generation, not_before)
         values ('catalog', 1, 0, now() + interval '60 seconds')
         on conflict (scope) do update set
             dirty_generation = github_catalog.backup_policy_publication_cursor.dirty_generation + 1,
             not_before = excluded.not_before",
    )
    .execute(&mut **tx)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}
/// Publishes the complete current desired state once the durable deadline has elapsed.
/// # Errors
/// Returns [`BackupPolicyError`] when derivation or atomic persistence fails.
pub async fn publish_due_backup_policy(
    database: &Database,
    now: OffsetDateTime,
) -> Result<PublicationOutcome, BackupPolicyError> {
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    sqlx::query("insert into github_catalog.backup_policy_publication_cursor (scope) values ('catalog') on conflict do nothing").execute(&mut *tx).await.map_err(PersistenceError::Query)?;
    let cursor: (i64,i64,i64,Option<String>,Option<OffsetDateTime>) = sqlx::query_as("select dirty_generation, published_generation, last_policy_version, last_fingerprint, not_before from github_catalog.backup_policy_publication_cursor where scope = 'catalog' for update").fetch_one(&mut *tx).await.map_err(PersistenceError::Query)?;
    if cursor.0 == cursor.1 || cursor.4.is_some_and(|deadline| deadline > now) {
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(PublicationOutcome::Pending);
    }
    let inputs = load_inputs(&mut tx).await?;
    let fingerprint = serde_json::to_string(&inputs).map_err(BackupPolicyError::Serialization)?;
    if cursor.3.as_deref() == Some(&fingerprint) {
        settle(&mut tx, cursor.0, cursor.2, &fingerprint).await?;
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(PublicationOutcome::Unchanged);
    }
    let version = u64::try_from(cursor.2).map_err(|_| invalid("policy_version"))? + 1;
    let policy = derive_backup_policy(version, &inputs)?;
    let payload = serde_json::to_value(&policy).map_err(BackupPolicyError::Serialization)?;
    sqlx::query("insert into github_catalog.backup_policy_publications (policy_version, fingerprint, document) values ($1,$2,$3)").bind(i64::try_from(version).map_err(|_|invalid("policy_version"))?).bind(&fingerprint).bind(&payload).execute(&mut *tx).await.map_err(PersistenceError::Query)?;
    let message_id = Uuid::now_v7();
    let envelope = crate::outbox::policy_command_bytes(message_id, &policy)
        .map_err(BackupPolicyError::Serialization)?;
    crate::outbox::insert(
        &mut tx,
        message_id,
        crate::outbox::POLICY_SUBJECT,
        &envelope,
        "vault-policy:catalog",
        None,
    )
    .await?;
    settle(
        &mut tx,
        cursor.0,
        i64::try_from(version).map_err(|_| invalid("policy_version"))?,
        &fingerprint,
    )
    .await?;
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(PublicationOutcome::Published {
        policy_version: version,
    })
}
/// Records one shared-contract acknowledgment through the idempotent inbox.
/// # Errors
/// Returns [`BackupPolicyError`] when the inbox or feedback projection cannot commit.
pub async fn record_backup_policy_acknowledgment(
    database: &Database,
    message_id: Uuid,
    ack: &PolicyAcknowledged,
) -> Result<FeedbackOutcome, BackupPolicyError> {
    let envelope = serde_json::to_vec(ack).map_err(BackupPolicyError::Serialization)?;
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let inserted=sqlx::query("insert into github_catalog.inbox_events (message_id,subject,envelope,stream_name,consumer_name,stream_sequence,delivery_count,state) values ($1,$2,$3,'domain',$2,(select coalesce(max(stream_sequence),0)+1 from github_catalog.inbox_events),1,'processing') on conflict do nothing").bind(message_id).bind(ACKNOWLEDGED_SUBJECT).bind(&envelope).execute(&mut *tx).await.map_err(PersistenceError::Query)?.rows_affected();
    if inserted == 0 {
        tx.commit().await.map_err(PersistenceError::Query)?;
        return Ok(FeedbackOutcome::Duplicate);
    }
    let outcome = match ack.outcome {
        PolicyOutcome::Accepted => "accepted",
        PolicyOutcome::Rejected => "rejected",
        _ => return Err(invalid("outcome")),
    };
    sqlx::query("insert into github_catalog.backup_policy_feedback (message_id,acknowledged_policy_version,outcome,last_applied_policy_version,reasons) values ($1,$2,$3,$4,$5)").bind(message_id).bind(i64::try_from(ack.acknowledged_policy_version).map_err(|_|invalid("acknowledged_policy_version"))?).bind(outcome).bind(i64::try_from(ack.last_applied_policy_version).map_err(|_|invalid("last_applied_policy_version"))?).bind(serde_json::to_value(&ack.reasons).map_err(BackupPolicyError::Serialization)?).execute(&mut *tx).await.map_err(PersistenceError::Query)?;
    sqlx::query("update github_catalog.inbox_events set state='consumed',terminal_outcome='acknowledged',consumed_at=now() where message_id=$1")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(PersistenceError::Query)?;
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(FeedbackOutcome::Recorded)
}

/// Records one exact Vault acknowledgment delivery under a resumable inbox lease.
///
/// The projection and terminal inbox state commit together. A broker acknowledgement may only be
/// sent after this function returns successfully.
///
/// # Errors
///
/// Returns [`BackupPolicyError`] when the delivery is already processing or persistence fails.
pub async fn record_backup_policy_acknowledgment_delivery(
    database: &Database,
    delivery: &InboxDelivery<'_>,
    ack: &PolicyAcknowledged,
    now: OffsetDateTime,
    lease_duration: Duration,
) -> Result<FeedbackOutcome, BackupPolicyError> {
    let outcome = match ack.outcome {
        PolicyOutcome::Accepted => "accepted",
        PolicyOutcome::Rejected => "rejected",
        _ => return Err(invalid("outcome")),
    };
    let lease_owner = match claim_inbox_delivery(database, delivery, now, lease_duration).await? {
        InboxClaimOutcome::Claimed { lease_owner } => lease_owner,
        InboxClaimOutcome::TerminalDuplicate => return Ok(FeedbackOutcome::Duplicate),
        InboxClaimOutcome::Busy => {
            return Err(PersistenceError::Query(sqlx::Error::Protocol(
                "inbox delivery is already processing".to_owned(),
            ))
            .into());
        }
    };
    let mut tx = database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into github_catalog.backup_policy_feedback
             (message_id,acknowledged_policy_version,outcome,last_applied_policy_version,reasons)
         values ($1,$2,$3,$4,$5) on conflict (message_id) do nothing",
    )
    .bind(delivery.message_id)
    .bind(
        i64::try_from(ack.acknowledged_policy_version)
            .map_err(|_| invalid("acknowledged_policy_version"))?,
    )
    .bind(outcome)
    .bind(
        i64::try_from(ack.last_applied_policy_version)
            .map_err(|_| invalid("last_applied_policy_version"))?,
    )
    .bind(serde_json::to_value(&ack.reasons).map_err(BackupPolicyError::Serialization)?)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?;
    let changed = sqlx::query(
        "update github_catalog.inbox_events
         set state='consumed',terminal_outcome='acknowledged',consumed_at=$3,
             lease_owner=null,lease_expires_at=null,failure_code=null
         where message_id=$1 and lease_owner=$2 and state='processing'",
    )
    .bind(delivery.message_id)
    .bind(lease_owner)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(PersistenceError::Query)?
    .rows_affected();
    if changed != 1 {
        return Err(PersistenceError::Query(sqlx::Error::Protocol(
            "inbox lease is not owned".to_owned(),
        ))
        .into());
    }
    tx.commit().await.map_err(PersistenceError::Query)?;
    Ok(FeedbackOutcome::Recorded)
}
/// Returns the latest decision Vault reported, without converting it into backup health.
/// # Errors
/// Returns [`BackupPolicyError`] when the projection cannot be queried.
pub async fn latest_backup_policy_feedback(
    database: &Database,
) -> Result<Option<PolicyFeedback>, BackupPolicyError> {
    let row:Option<(i64,String,i64,serde_json::Value)>=sqlx::query_as("select acknowledged_policy_version,outcome,last_applied_policy_version,reasons from github_catalog.backup_policy_feedback order by received_at desc,message_id desc limit 1").fetch_optional(database.pool()).await.map_err(PersistenceError::Query)?;
    row.map(|(v, o, p, r)| {
        Ok(PolicyFeedback {
            policy_version: u64::try_from(v).map_err(|_| invalid("acknowledged_policy_version"))?,
            outcome: if o == "accepted" {
                PolicyOutcome::Accepted
            } else {
                PolicyOutcome::Rejected
            },
            last_applied_policy_version: u64::try_from(p)
                .map_err(|_| invalid("last_applied_policy_version"))?,
            reasons: r,
        })
    })
    .transpose()
}

fn invalid(field: &'static str) -> BackupPolicyError {
    BackupPolicyError::Invalid { field }
}
fn to_entry(input: &BackupPolicyInput) -> Result<RepositoryBackupEntry, BackupPolicyError> {
    let repository_ref =
        EntityRef::parse(&input.repository_ref).map_err(|_| invalid("repository_ref"))?;
    let mirror_cadence = match input.cadence.as_str() {
        "eager" => MirrorCadence::Eager,
        "daily" => MirrorCadence::Daily,
        "weekly" => MirrorCadence::Weekly,
        _ => return Err(invalid("mirror_cadence")),
    };
    let priority_hint = match input.priority.as_str() {
        "critical" => BackupPriorityHint::Critical,
        "standard" => BackupPriorityHint::Standard,
        "bulk" => BackupPriorityHint::Bulk,
        _ => return Err(invalid("priority_hint")),
    };
    let exclusions = input
        .exclusions
        .iter()
        .map(|(scope, text)| {
            Ok(BackupExclusion {
                scope: match scope.as_str() {
                    "refs_matching" => BackupExclusionScope::RefsMatching,
                    "paths_matching" => BackupExclusionScope::PathsMatching,
                    _ => return Err(invalid("exclusion_scope")),
                },
                expression: ExclusionExpression::parse(text).map_err(|_| invalid("exclusion"))?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RepositoryBackupEntry {
        repository_ref,
        mirror_cadence,
        priority_hint,
        size_hint_bytes: input.size_hint_bytes,
        exclusions,
    })
}
async fn load_inputs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<Vec<BackupPolicyInput>, BackupPolicyError> {
    let rows:Vec<(Uuid,String,String,Option<i64>,serde_json::Value)>=sqlx::query_as("select r.repository_id,coalesce(p.mirror_cadence,'daily'),coalesce(p.priority_hint,'standard'),p.size_hint_bytes,coalesce(p.exclusions,'[]'::jsonb) from github_catalog.repositories r left join github_catalog.backup_policies p on p.repository_id=r.repository_id where r.mode in ('auto','tracked') and coalesce(p.policy_level,'git_mirror') in ('git_mirror','git_mirror_with_lfs','complete_archive') order by r.repository_id").fetch_all(&mut **tx).await.map_err(PersistenceError::Query)?;
    rows.into_iter()
        .map(|(id, cadence, priority, size, exclusions)| {
            let exclusions: Vec<RawExclusion> =
                serde_json::from_value(exclusions).map_err(|_| invalid("exclusions"))?;
            Ok(BackupPolicyInput::mirror(
                &format!("repository:{id}"),
                &cadence,
                &priority,
                size.map(|n| u64::try_from(n).map_err(|_| invalid("size_hint_bytes")))
                    .transpose()?,
                exclusions
                    .iter()
                    .map(|item| (item.scope.as_str(), item.expression.as_str()))
                    .collect(),
            ))
        })
        .collect()
}
#[derive(Debug, serde::Deserialize)]
struct RawExclusion {
    scope: String,
    expression: String,
}
async fn settle(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    generation: i64,
    version: i64,
    fingerprint: &str,
) -> Result<(), BackupPolicyError> {
    sqlx::query("update github_catalog.backup_policy_publication_cursor set published_generation=$1,last_policy_version=$2,last_fingerprint=$3,not_before=null where scope='catalog'").bind(generation).bind(version).bind(fingerprint).execute(&mut **tx).await.map_err(PersistenceError::Query)?;
    Ok(())
}
