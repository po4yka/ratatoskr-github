//! Account erasure finds the real producers' payload shapes (XR-021 CONTRACTS.md S12).

use std::collections::BTreeSet;

use ratatoskr_github_catalog::test_support::{TestDatabase, erasure_table_classification};
use ratatoskr_github_contracts::{
    AnalysisFailureCode, RepositoryAnalysisCompleted, RepositoryAnalysisFailed,
};
use ratatoskr_identifiers::{EntityRef, Extensions, WireTimestamp};
use ratatoskr_operation_contracts::AccountErasureOutcome;
use serde_json::json;
use uuid::Uuid;

mod erasure_support;

use erasure_support::{
    Owner, TestResult, attempt, count, erase, insert_event, owner, repository, requested,
};

/// Seeds the rows the real producers write for one owner and returns the message ids keyed to
/// the owner in the outbox and in the inbox.
async fn seed_events(
    database: &TestDatabase,
    owner: &Owner,
    repository_id: Uuid,
    numeric_id: u64,
) -> Result<(Vec<Uuid>, Vec<Uuid>), Box<dyn std::error::Error>> {
    let request = requested(owner, repository_id, numeric_id)?;
    let bare = serde_json::to_value(&request)?;
    let envelope = json!({
        "event_id": Uuid::now_v7().to_string(),
        "event_type": "knowledge.repository_analysis.requested.v1",
        "occurred_at": "2026-10-01T00:00:00Z",
        "producer": "ratatoskr-github",
        "aggregate_id": format!("repository:{repository_id}"),
        "correlation_id": format!("repository_analysis:{}", request.request_id),
        "tenant_id": owner.reference,
        "schema_version": 1,
        "payload": bare,
    });
    let subject = "evt.knowledge.repository_analysis.requested.v1";
    let outbox = vec![
        insert_event(database, "outbox_events", subject, &bare).await?,
        insert_event(database, "outbox_events", subject, &envelope).await?,
    ];
    let completed = serde_json::to_value(RepositoryAnalysisCompleted {
        owner: owner.tenant,
        repository_id: request.repository_id,
        github_repository_numeric_id: numeric_id,
        request_id: request.request_id,
        source_revision: request.source_revision.clone(),
        analysis_result_ref: EntityRef::parse(&format!("analysis:{}", Uuid::now_v7()))?,
        completed_at: WireTimestamp::now(),
        extensions: Extensions::new(),
    })?;
    let failed = serde_json::to_value(RepositoryAnalysisFailed {
        owner: owner.tenant,
        repository_id: request.repository_id,
        github_repository_numeric_id: numeric_id,
        request_id: request.request_id,
        source_revision: request.source_revision.clone(),
        failure_code: AnalysisFailureCode::SourceUnavailable,
        retryable: true,
        failed_at: WireTimestamp::now(),
        extensions: Extensions::new(),
    })?;
    let sync_command = json!({
        "command_id": Uuid::now_v7().to_string(),
        "command_type": "github.sync.requested.v1",
        "requested_at": "2026-10-01T00:00:00Z",
        "operation_id": Uuid::now_v7().to_string(),
        "tenant_id": owner.reference,
        "correlation_id": "sched/github-sync/occurrence",
        "idempotency_key": Uuid::now_v7().to_string(),
        "payload": { "account": owner.reference },
    });
    let inbox = vec![
        insert_event(
            database,
            "inbox_events",
            "evt.knowledge.repository_analysis.completed.v1",
            &completed,
        )
        .await?,
        insert_event(
            database,
            "inbox_events",
            "evt.knowledge.repository_analysis.failed.v1",
            &failed,
        )
        .await?,
        insert_event(
            database,
            "inbox_events",
            "github.sync.requested.v1",
            &sync_command,
        )
        .await?,
    ];
    sqlx::query(
        "insert into github_catalog.repository_analysis_publications
             (repository_id, source_digest, message_id, payload)
         values ($1, $2, $3, $4)",
    )
    .bind(repository_id)
    .bind(format!(
        "{:0>64x}",
        u128::from_be_bytes(Uuid::now_v7().into_bytes())
    ))
    .bind(Uuid::now_v7())
    .bind(&bare)
    .execute(database.database.pool())
    .await?;
    Ok((outbox, inbox))
}

async fn message_ids(
    database: &TestDatabase,
    table: &str,
) -> Result<BTreeSet<Uuid>, Box<dyn std::error::Error>> {
    let rows: Vec<Uuid> =
        sqlx::query_scalar(&format!("select message_id from github_catalog.{table}"))
            .fetch_all(database.database.pool())
            .await?;
    Ok(rows.into_iter().collect())
}

#[tokio::test]
async fn erasure_removes_every_owner_keyed_row_of_the_real_shapes() -> TestResult {
    let database = TestDatabase::create().await?;
    let erased = owner(&database).await?;
    let other = owner(&database).await?;
    let repository_id = repository(&database, 951_001).await?;
    let (_erased_outbox, _erased_inbox) =
        seed_events(&database, &erased, repository_id, 951_001).await?;
    let (other_outbox, other_inbox) =
        seed_events(&database, &other, repository_id, 951_001).await?;
    let catalog_wide_outbox = insert_event(
        &database,
        "outbox_events",
        "cmd.vault.backup_policy.apply_requested.v1",
        &json!({"command_type": "vault.backup_policy.apply_requested.v1", "payload": {}}),
    )
    .await?;
    let catalog_wide_inbox = insert_event(
        &database,
        "inbox_events",
        "evt.vault.backup_policy.acknowledged.v1",
        &json!({"acknowledged_policy_version": 1, "outcome": "accepted"}),
    )
    .await?;
    for mode in ["metadata", "track"] {
        attempt(&database, &erased, 951_001, mode).await?;
    }
    attempt(&database, &other, 951_001, "metadata").await?;

    let outcome = erase(&database, &erased).await?;

    assert_eq!(outcome, AccountErasureOutcome::Verified);
    let expected_outbox: BTreeSet<Uuid> = other_outbox
        .iter()
        .copied()
        .chain([catalog_wide_outbox])
        .collect();
    let expected_inbox: BTreeSet<Uuid> = other_inbox
        .iter()
        .copied()
        .chain([catalog_wide_inbox])
        .collect();
    assert_eq!(
        message_ids(&database, "outbox_events").await?,
        expected_outbox
    );
    assert_eq!(
        message_ids(&database, "inbox_events").await?,
        expected_inbox
    );
    let remaining_attempts: Vec<String> =
        sqlx::query_scalar("select owner_ref from github_catalog.repository_action_attempts")
            .fetch_all(database.database.pool())
            .await?;
    assert_eq!(remaining_attempts, vec![other.reference.clone()]);
    let publications: Vec<String> = sqlx::query_scalar(
        "select payload ->> 'owner' from github_catalog.repository_analysis_publications",
    )
    .fetch_all(database.database.pool())
    .await?;
    assert_eq!(publications, vec![other.reference.clone()]);
    assert_eq!(
        count(
            &database,
            "select count(*) from github_catalog.github_accounts"
        )
        .await?,
        1
    );
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn every_foreign_key_into_repositories_is_classified() -> TestResult {
    let database = TestDatabase::create().await?;
    let referencing: Vec<String> = sqlx::query_scalar(
        "select distinct c.conrelid::regclass::text
         from pg_constraint c
         where c.contype = 'f' and c.confrelid = 'github_catalog.repositories'::regclass
         order by 1",
    )
    .fetch_all(database.database.pool())
    .await?;
    let classification = erasure_table_classification();
    let classified: BTreeSet<String> = classification
        .owner_references
        .iter()
        .chain(classification.catalog_children)
        .map(|table| format!("github_catalog.{table}"))
        .collect();
    let referencing: BTreeSet<String> = referencing.into_iter().collect();

    assert_eq!(
        referencing, classified,
        "every foreign key into repositories must be classified exactly once"
    );
    assert!(
        classification
            .owner_references
            .iter()
            .all(|table| !classification.catalog_children.contains(table)),
        "a table is either an owner reference or a catalog child"
    );
    database.cleanup().await?;
    Ok(())
}
