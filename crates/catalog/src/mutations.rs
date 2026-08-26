//! Authorized provider mutations: starring, unstarring, and star-list
//! membership filing.
//!
//! Every mutation carries an authorization context supplied by the calling
//! product flow - the account it acts through, the principal that confirmed
//! it, and the calling source that owns the confirmation UX. This service
//! enforces connection status and granted-scope capabilities before any
//! provider contact, executes repeat-safe provider operations, and records
//! every attempt in one append-only audit trail keyed by idempotency keys so
//! retries converge on one end state and exactly one successful audit record.

use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::mutation_trail::{
    AuditOutcome, failed_outcome, insert_audit_row, paused_outcome, refused_outcome,
    successful_outcome_exists,
};
use crate::provider_mutations::MutationApi;
use crate::rate_limit::{RateLimitLedger, TokenRef};

/// The calling product flow that owns the confirmation UX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationSource {
    /// The Telegram bot flow.
    Telegram,
    /// The web client flow.
    Web,
}

impl MutationSource {
    /// The database representation of the calling source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Web => "web",
        }
    }
}

/// Who authorized a mutation and through which channel.
///
/// Confirmation UX belongs to the calling product; this context is the
/// evidence trail of that confirmation arriving here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    /// The connected account the mutation acts through.
    pub account_id: Uuid,
    /// The principal that confirmed the action, such as `telegram:42`.
    pub principal: String,
    /// The calling product flow.
    pub source: MutationSource,
}

/// A local reference to the repository a mutation targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRef {
    /// The stable GitHub numeric repository id.
    pub provider_repository_id: i64,
    /// The current owner login used to address the provider.
    pub owner: String,
    /// The current repository name used to address the provider.
    pub name: String,
}

/// One requested external write with its replay identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationRequest {
    /// Star the repository on GitHub.
    Star {
        /// The repository target.
        repository: RepositoryRef,
        /// The operation's idempotency key; resubmitting a completed request
        /// with the same key replays the recorded outcome.
        idempotency_key: String,
    },
    /// Remove the repository's star on GitHub.
    Unstar {
        /// The repository target.
        repository: RepositoryRef,
        /// The operation's idempotency key.
        idempotency_key: String,
    },
    /// File the repository into one native star list, preserving every
    /// membership the local authority records elsewhere.
    ListMemberAdd {
        /// The repository target.
        repository: RepositoryRef,
        /// The catalog identity of the target list.
        list_id: Uuid,
        /// The operation's idempotency key.
        idempotency_key: String,
    },
    /// Remove the repository from one native star list, preserving every
    /// other membership.
    ListMemberRemove {
        /// The repository target.
        repository: RepositoryRef,
        /// The catalog identity of the target list.
        list_id: Uuid,
        /// The operation's idempotency key.
        idempotency_key: String,
    },
}

impl MutationRequest {
    /// The idempotency key carried by this request.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        match self {
            Self::Star {
                idempotency_key, ..
            }
            | Self::Unstar {
                idempotency_key, ..
            }
            | Self::ListMemberAdd {
                idempotency_key, ..
            }
            | Self::ListMemberRemove {
                idempotency_key, ..
            } => idempotency_key,
        }
    }

    /// The targeted repository.
    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        match self {
            Self::Star { repository, .. }
            | Self::Unstar { repository, .. }
            | Self::ListMemberAdd { repository, .. }
            | Self::ListMemberRemove { repository, .. } => repository,
        }
    }
}

/// The operation vocabulary of the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationKind {
    Star,
    Unstar,
    ListMemberAdd,
    ListMemberRemove,
}

impl MutationKind {
    /// The database representation of the operation kind.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Star => "star",
            Self::Unstar => "unstar",
            Self::ListMemberAdd => "list_member_add",
            Self::ListMemberRemove => "list_member_remove",
        }
    }
}

/// The mutation capability an operation requires from the acting account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationCapability {
    /// Starring or unstarring on behalf of the account.
    Star,
    /// Writing native star-list membership on behalf of the account.
    ListWrite,
}

impl MutationCapability {
    /// The granted-scope names that satisfy this capability, mirroring the
    /// legacy deployment's requirements: stars ride the repository-read
    /// scopes, while list writes demand the broader `user` scope.
    const fn accepted_scopes(self) -> &'static [&'static str] {
        match self {
            Self::Star => &["repo", "public_repo"],
            Self::ListWrite => &["user"],
        }
    }

    /// Whether a granted-scope set satisfies this capability.
    fn satisfied_by(self, granted: &[String]) -> bool {
        self.accepted_scopes()
            .iter()
            .any(|accepted| granted.iter().any(|scope| scope == accepted))
    }
}

/// The capability a request demands before any provider contact.
const fn required_capability(request: &MutationRequest) -> MutationCapability {
    match request {
        MutationRequest::Star { .. } | MutationRequest::Unstar { .. } => MutationCapability::Star,
        MutationRequest::ListMemberAdd { .. } | MutationRequest::ListMemberRemove { .. } => {
            MutationCapability::ListWrite
        }
    }
}

/// Why a mutation was refused before any provider contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// No connected account matches the authorization context.
    AccountNotConnected,
    /// The account's granted scopes do not satisfy the capability.
    MissingScope,
    /// A direct request for `auto` was refused; auto is reached through
    /// star effects only.
    AutoNotDirectlyRequestable,
    /// The repository cannot be ignored while the acting account stars it.
    RepositoryCurrentlyStarred,
    /// The repository is deliberately excluded; starring cannot bypass it.
    RepositoryIgnored,
}

/// How one mutation attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationStatus {
    /// The provider confirmed the newly requested state.
    Applied,
    /// The provider reported the requested state already held.
    AlreadyApplied,
    /// Refused by this service before any provider contact.
    Rejected {
        /// The enforcement rule that refused the attempt.
        reason: RefusalReason,
    },
    /// The attempt did not complete: the provider rejected or could not
    /// finish it, or the account's budget paused it before contact.
    Failed {
        /// The classified reason; never carries credential material.
        reason: String,
    },
}

/// The truthful result of one mutation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationOutcome {
    /// The idempotency key of the attempt.
    pub idempotency_key: String,
    /// How the attempt ended.
    pub status: MutationStatus,
}

/// Failure modes of the mutation executor itself: outcomes carry domain
/// truth, errors are reserved for caller-level failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MutationError {
    /// The database refused a read or the audit-trail write.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// The operation-kind label for the request's audit row.
pub(crate) fn kind_label(request: &MutationRequest) -> &'static str {
    kind_of(request)
}

/// The collaborators one mutation execution needs.
pub(crate) struct MutationRuntime<'a, G: MutationApi> {
    pub(crate) database: &'a Database,
    pub(crate) gateway: &'a G,
    pub(crate) ledger: &'a RateLimitLedger,
    pub(crate) token: &'a TokenRef,
    pub(crate) secret: Option<&'a str>,
    pub(crate) context: &'a MutationContext,
}

/// Executes one authorized mutation attempt and reports its truthful outcome.
///
/// Enforcement runs before any provider contact: the acting account must
/// resolve to a connected account whose granted scopes satisfy the request's
/// capability. Every refusal is audited; the provider never sees one.
///
/// # Errors
///
/// Returns [`MutationError`] when the audit trail itself cannot be read or
/// written; domain refusals and failures arrive as [`MutationOutcome`] data.
pub async fn execute_mutation<G: MutationApi>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    secret: Option<&str>,
    context: &MutationContext,
    request: MutationRequest,
) -> Result<MutationOutcome, MutationError> {
    let runtime = MutationRuntime {
        database,
        gateway,
        ledger,
        token,
        secret,
        context,
    };
    let idempotency_key = request.idempotency_key().to_owned();

    // Connection and capability enforcement come first - all before any
    // provider contact.
    if let Some(reason) = authorization_refusal(database, context, &request).await? {
        return refused_outcome(database, context, &request, reason).await;
    }

    // A consumed key replays its recorded success without any provider
    // contact; the response reports already-applied per the replay contract.
    if successful_outcome_exists(database, &idempotency_key).await? {
        return Ok(MutationOutcome {
            idempotency_key,
            status: MutationStatus::AlreadyApplied,
        });
    }

    // Deliberate exclusion outranks starring: an ignored repository cannot be
    // starred through the write path, and no provider request is spent.
    if let Some(reason) = ignored_star_conflict(database, &request).await? {
        return refused_outcome(database, context, &request, reason).await;
    }

    match ledger.acquire(token) {
        Err(crate::rate_limit::AcquireError::RateLimited { retry_at }) => {
            paused_outcome(database, context, &request, retry_at).await
        }
        Ok(()) => dispatch(&runtime, request).await,
    }
}

/// Executes a submitted batch of mutations independently and reports one
/// truthful outcome per operation in submission order. Any operation's
/// refusal or failure neither prevents nor undoes the others; previously
/// succeeded keys replay their recorded outcome without provider contact.
///
/// # Errors
///
/// Returns [`MutationError`] when the audit trail itself becomes unwritable;
/// domain outcomes arrive as data even inside batches.
pub async fn execute_batch<G: MutationApi>(
    database: &Database,
    gateway: &G,
    ledger: &RateLimitLedger,
    token: &TokenRef,
    secret: Option<&str>,
    context: &MutationContext,
    requests: Vec<MutationRequest>,
) -> Result<Vec<MutationOutcome>, MutationError> {
    let mut outcomes = Vec::with_capacity(requests.len());
    for request in requests {
        outcomes.push(
            execute_mutation(database, gateway, ledger, token, secret, context, request).await?,
        );
    }
    Ok(outcomes)
}

/// Routes one cleared request to its executor.
async fn dispatch<G: MutationApi>(
    runtime: &MutationRuntime<'_, G>,
    request: MutationRequest,
) -> Result<MutationOutcome, MutationError> {
    match request {
        MutationRequest::Star {
            repository,
            idempotency_key,
        } => execute_star_write(runtime, repository, idempotency_key, StarDirection::Add).await,
        MutationRequest::Unstar {
            repository,
            idempotency_key,
        } => execute_star_write(runtime, repository, idempotency_key, StarDirection::Remove).await,
        MutationRequest::ListMemberAdd {
            repository,
            list_id,
            idempotency_key,
        } => {
            crate::list_mutations::execute_list_membership(
                runtime,
                repository,
                list_id,
                idempotency_key,
                crate::list_mutations::MembershipChange::Add,
            )
            .await
        }
        MutationRequest::ListMemberRemove {
            repository,
            list_id,
            idempotency_key,
        } => {
            crate::list_mutations::execute_list_membership(
                runtime,
                repository,
                list_id,
                idempotency_key,
                crate::list_mutations::MembershipChange::Remove,
            )
            .await
        }
    }
}

/// The connection-and-scope enforcement result: a reason when the request
/// must be refused before any provider contact.
async fn authorization_refusal(
    database: &Database,
    context: &MutationContext,
    request: &MutationRequest,
) -> Result<Option<RefusalReason>, PersistenceError> {
    let account = sqlx::query_as::<_, (String, Vec<String>)>(
        "select status, granted_scopes from github_catalog.github_accounts
         where account_id = $1",
    )
    .bind(context.account_id)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?;

    Ok(match account {
        None => Some(RefusalReason::AccountNotConnected),
        Some((status, _granted_scopes)) if status != "connected" => {
            Some(RefusalReason::AccountNotConnected)
        }
        Some((_status, granted_scopes)) => {
            let satisfied = required_capability(request).satisfied_by(&granted_scopes);
            (!satisfied).then_some(RefusalReason::MissingScope)
        }
    })
}

/// Whether the write path would bypass a deliberate exclusion.
async fn ignored_star_conflict(
    database: &Database,
    request: &MutationRequest,
) -> Result<Option<RefusalReason>, PersistenceError> {
    if !matches!(request, MutationRequest::Star { .. }) {
        return Ok(None);
    }
    let ignored = sqlx::query_scalar::<_, bool>(
        "select coalesce(mode = 'ignored', false) from github_catalog.repositories
         where provider_repository_id = $1",
    )
    .bind(request.repository().provider_repository_id)
    .fetch_optional(database.pool())
    .await
    .map_err(PersistenceError::Query)?
    .unwrap_or(false);
    Ok(ignored.then_some(RefusalReason::RepositoryIgnored))
}

/// Which direction one star write travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StarDirection {
    Add,
    Remove,
}

impl StarDirection {
    /// The audit vocabulary for the direction.
    const fn kind(self) -> MutationKind {
        match self {
            Self::Add => MutationKind::Star,
            Self::Remove => MutationKind::Unstar,
        }
    }
}

/// The shared star-write path: resolve the node id, apply the documented
/// mutation in the chosen direction, then converge projection, mode, and
/// audit in one transaction.
async fn execute_star_write<G: MutationApi>(
    runtime: &MutationRuntime<'_, G>,
    repository: RepositoryRef,
    idempotency_key: String,
    direction: StarDirection,
) -> Result<MutationOutcome, MutationError> {
    let node = match resolve_node(runtime, &repository, direction.kind(), &idempotency_key).await? {
        NodeResolution::Resolved(node) => node,
        NodeResolution::Failed(failed) => return Ok(failed),
    };
    let write = match direction {
        StarDirection::Add => runtime.gateway.star_repository(runtime.secret, &node).await,
        StarDirection::Remove => {
            runtime
                .gateway
                .unstar_repository(runtime.secret, &node)
                .await
        }
    };
    let confirmation = match write {
        Ok(reply) => {
            runtime.ledger.observe(runtime.token, &reply.rate_limit);
            reply
        }
        Err(error) => {
            return failed_outcome(
                runtime.database,
                runtime.context,
                direction.kind().as_str(),
                repository.provider_repository_id,
                &idempotency_key,
                error.to_string(),
            )
            .await;
        }
    };
    let state_holds = match direction {
        StarDirection::Add => confirmation.viewer_has_starred,
        StarDirection::Remove => !confirmation.viewer_has_starred,
    };
    if !state_holds {
        // The provider denied reaching the requested state; nothing local
        // may claim success.
        return failed_outcome(
            runtime.database,
            runtime.context,
            direction.kind().as_str(),
            repository.provider_repository_id,
            &idempotency_key,
            "the provider did not confirm the requested star state".to_owned(),
        )
        .await;
    }

    let mut transaction = runtime
        .database
        .pool()
        .begin()
        .await
        .map_err(PersistenceError::Query)?;
    let repository_id = crate::identity::upsert_repository_in_tx(
        &mut transaction,
        repository.provider_repository_id,
    )
    .await?;

    // Outcome truthfulness is relative to known local state: locally held
    // plus confirming reply is already-applied with untouched timestamps.
    let prior_starred: Option<bool> = sqlx::query_scalar(
        "select starred from github_catalog.current_star_state
         where account_id = $1 and repository_id = $2",
    )
    .bind(runtime.context.account_id)
    .bind(repository_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    let newly_applied = match direction {
        StarDirection::Add => !prior_starred.unwrap_or(false),
        StarDirection::Remove => prior_starred.unwrap_or(false),
    };

    apply_star_projection(
        &mut transaction,
        runtime.context.account_id,
        repository_id,
        direction,
    )
    .await?;
    if newly_applied {
        crate::backup_policy::mark_backup_policy_dirty_in_tx(&mut transaction)
            .await
            .map_err(MutationError::from)?;
    }

    let outcome_label = if newly_applied {
        AuditOutcome::Applied
    } else {
        AuditOutcome::AlreadyApplied
    };
    let inserted = insert_audit_row(
        &mut transaction,
        runtime.context,
        repository_id,
        direction.kind().as_str(),
        &idempotency_key,
        outcome_label,
        serde_json::json!({ "via": "mutation" }),
    )
    .await?;
    finish_with_replay_guard(transaction, inserted, idempotency_key, newly_applied).await
}

/// Converges the star projection and mode governance for one direction.
/// Starring promotes only unclassified entries to auto; unstarring releases
/// auto back to unclassified. Explicit modes are never overridden.
async fn apply_star_projection(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    repository_id: Uuid,
    direction: StarDirection,
) -> Result<(), PersistenceError> {
    match direction {
        StarDirection::Add => {
            sqlx::query(
                "insert into github_catalog.current_star_state
                     (account_id, repository_id, starred, starred_at, last_observed_at)
                 values ($1, $2, true, now(), now())
                 on conflict (account_id, repository_id) do update set
                     starred = true,
                     starred_at = coalesce(github_catalog.current_star_state.starred_at, now()),
                     last_observed_at = now(),
                     observed_unstarred_at = null",
            )
            .bind(account_id)
            .bind(repository_id)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
            sqlx::query(
                "update github_catalog.repositories
                 set mode = 'auto', updated_at = now()
                 where repository_id = $1 and mode is null",
            )
            .bind(repository_id)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
        }
        StarDirection::Remove => {
            sqlx::query(
                "insert into github_catalog.current_star_state
                     (account_id, repository_id, starred, observed_unstarred_at, last_observed_at)
                 values ($1, $2, false, now(), now())
                 on conflict (account_id, repository_id) do update set
                     starred = false,
                     starred_at = null,
                     observed_unstarred_at = now(),
                     last_observed_at = now()",
            )
            .bind(account_id)
            .bind(repository_id)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
            sqlx::query(
                "update github_catalog.repositories
                 set mode = null, updated_at = now()
                 where repository_id = $1 and mode = 'auto'",
            )
            .bind(repository_id)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
        }
    }
    Ok(())
}

/// The node-id resolution result: an id to write against, or an already
/// audited truthful outcome replacing the whole operation.
pub(crate) enum NodeResolution {
    Resolved(String),
    Failed(MutationOutcome),
}

/// Resolves the repository's GraphQL node id, observing rate accounting.
/// A provider failure is audited here and surfaced as
/// [`NodeResolution::Failed`].
pub(crate) async fn resolve_node<G: MutationApi>(
    runtime: &MutationRuntime<'_, G>,
    repository: &RepositoryRef,
    kind: MutationKind,
    idempotency_key: &str,
) -> Result<NodeResolution, MutationError> {
    match runtime
        .gateway
        .fetch_repository_node_id(runtime.secret, &repository.owner, &repository.name)
        .await
    {
        Ok(reply) => {
            runtime.ledger.observe(runtime.token, &reply.rate_limit);
            Ok(NodeResolution::Resolved(reply.node_id))
        }
        Err(error) => Ok(NodeResolution::Failed(
            failed_outcome(
                runtime.database,
                runtime.context,
                kind.as_str(),
                repository.provider_repository_id,
                idempotency_key,
                error.to_string(),
            )
            .await?,
        )),
    }
}

/// The direction of one list-membership change.
pub(crate) async fn finish_with_replay_guard(
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    inserted_rows: u64,
    idempotency_key: String,
    newly_applied: bool,
) -> Result<MutationOutcome, MutationError> {
    if inserted_rows == 0 {
        transaction
            .rollback()
            .await
            .map_err(PersistenceError::Query)?;
        return Ok(MutationOutcome {
            idempotency_key,
            status: MutationStatus::AlreadyApplied,
        });
    }
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(MutationOutcome {
        idempotency_key,
        status: if newly_applied {
            MutationStatus::Applied
        } else {
            MutationStatus::AlreadyApplied
        },
    })
}

/// The operation-kind label for the request's audit row.
fn kind_of(request: &MutationRequest) -> &'static str {
    match request {
        MutationRequest::Star { .. } => MutationKind::Star.as_str(),
        MutationRequest::Unstar { .. } => MutationKind::Unstar.as_str(),
        MutationRequest::ListMemberAdd { .. } => MutationKind::ListMemberAdd.as_str(),
        MutationRequest::ListMemberRemove { .. } => MutationKind::ListMemberRemove.as_str(),
    }
}
