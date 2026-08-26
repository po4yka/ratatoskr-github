//! Native star-list membership mutation execution.

use uuid::Uuid;

use crate::database::{Database, PersistenceError};
use crate::mutation_trail::{AuditOutcome, failed_outcome, insert_audit_row};
use crate::mutations::{
    MutationError, MutationKind, MutationOutcome, MutationRuntime, NodeResolution, RepositoryRef,
    finish_with_replay_guard, resolve_node,
};
use crate::provider_mutations::MutationApi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MembershipChange {
    Add,
    Remove,
}

impl MembershipChange {
    /// The audit vocabulary for the direction.
    const fn kind(self) -> MutationKind {
        match self {
            Self::Add => MutationKind::ListMemberAdd,
            Self::Remove => MutationKind::ListMemberRemove,
        }
    }
}

/// The list-membership execution path: compute the complete desired set from
/// the local membership authority, resolve the node id, write the full set
/// through the replacement-semantics mutation, then converge the local
/// projection and audit in one transaction.
pub(crate) async fn execute_list_membership<G: MutationApi>(
    runtime: &MutationRuntime<'_, G>,
    repository: RepositoryRef,
    list_id: Uuid,
    idempotency_key: String,
    change: MembershipChange,
) -> Result<MutationOutcome, MutationError> {
    let kind = change.kind();

    // The target must be a live list of the acting account.
    let target_provider_id: Option<String> = sqlx::query_scalar(
        "select provider_list_id from github_catalog.star_lists
         where list_id = $1 and account_id = $2 and status = 'active'",
    )
    .bind(list_id)
    .bind(runtime.context.account_id)
    .fetch_optional(runtime.database.pool())
    .await
    .map_err(PersistenceError::Query)?;
    let Some(target_provider_id) = target_provider_id else {
        return failed_outcome(
            runtime.database,
            runtime.context,
            kind.as_str(),
            repository.provider_repository_id,
            &idempotency_key,
            "the target list does not exist for the acting account".to_owned(),
        )
        .await;
    };

    let mut desired = live_member_list_ids(
        runtime.database,
        runtime.context.account_id,
        repository.provider_repository_id,
    )
    .await?;
    match change {
        MembershipChange::Add => {
            if !desired.iter().any(|id| id == &target_provider_id) {
                desired.push(target_provider_id);
            }
        }
        MembershipChange::Remove => desired.retain(|id| id != &target_provider_id),
    }
    desired.sort();
    desired.dedup();

    let node = match resolve_node(runtime, &repository, kind, &idempotency_key).await? {
        NodeResolution::Resolved(node) => node,
        NodeResolution::Failed(failed) => return Ok(failed),
    };
    let write = runtime
        .gateway
        .set_repository_lists(runtime.secret, &node, &desired)
        .await;
    if let Err(error) = write {
        return failed_outcome(
            runtime.database,
            runtime.context,
            kind.as_str(),
            repository.provider_repository_id,
            &idempotency_key,
            error.to_string(),
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
    converge_membership_projection(&mut transaction, list_id, repository_id, change).await?;

    let inserted = insert_audit_row(
        &mut transaction,
        runtime.context,
        repository_id,
        kind.as_str(),
        &idempotency_key,
        AuditOutcome::Applied,
        serde_json::json!({ "via": "mutation", "desired_list_ids": desired }),
    )
    .await?;
    finish_with_replay_guard(transaction, inserted, idempotency_key, true).await
}

/// The provider list ids the local authority currently records as members.
async fn live_member_list_ids(
    database: &Database,
    account_id: Uuid,
    provider_repository_id: i64,
) -> Result<Vec<String>, PersistenceError> {
    sqlx::query_scalar(
        "select sl.provider_list_id from github_catalog.star_lists sl
         join github_catalog.star_list_memberships m on m.list_id = sl.list_id
         join github_catalog.repositories r on r.repository_id = m.repository_id
         where sl.account_id = $1 and r.provider_repository_id = $2
           and sl.status = 'active' and m.member",
    )
    .bind(account_id)
    .bind(provider_repository_id)
    .fetch_all(database.pool())
    .await
    .map_err(PersistenceError::Query)
}

/// Converges the local membership projection with what was written upstream.
async fn converge_membership_projection(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    list_id: Uuid,
    repository_id: Uuid,
    change: MembershipChange,
) -> Result<(), PersistenceError> {
    match change {
        MembershipChange::Add => {
            sqlx::query(
                "insert into github_catalog.star_list_memberships
                     (list_id, repository_id, member, last_observed_at)
                 values ($1, $2, true, now())
                 on conflict (list_id, repository_id) do update set
                     member = true,
                     last_observed_at = now(),
                     observed_removed_at = null",
            )
            .bind(list_id)
            .bind(repository_id)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
        }
        MembershipChange::Remove => {
            sqlx::query(
                "insert into github_catalog.star_list_memberships
                     (list_id, repository_id, member, last_observed_at, observed_removed_at)
                 values ($1, $2, false, now(), now())
                 on conflict (list_id, repository_id) do update set
                     member = false,
                     last_observed_at = now(),
                     observed_removed_at = now()",
            )
            .bind(list_id)
            .bind(repository_id)
            .execute(&mut **transaction)
            .await
            .map_err(PersistenceError::Query)?;
        }
    }
    Ok(())
}
