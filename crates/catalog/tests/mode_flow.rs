//! End-to-end repository-mode transitions: disposable catalog database
//! exercising the validation matrix, audited transitions, and idempotent
//! replay of mode requests.

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    Database, MutationContext, MutationOutcome, MutationSource, MutationStatus, RequestedMode,
    SetModeRequest, set_repository_mode, upsert_repository,
};
use uuid::Uuid;

fn context_for(account_id: Uuid) -> MutationContext {
    MutationContext {
        account_id,
        principal: "web:7".to_owned(),
        source: MutationSource::Web,
    }
}

fn track_request(provider_repository_id: i64, key: &str) -> SetModeRequest {
    SetModeRequest {
        provider_repository_id,
        mode: RequestedMode::Tracked,
        idempotency_key: key.to_owned(),
    }
}

async fn seed_account(database: &Database) -> Result<Uuid, Box<dyn std::error::Error>> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'classifier', 'connected')",
    )
    .bind(account_id)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

async fn current_mode(
    database: &Database,
    provider_repository_id: i64,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mode: Option<String> = sqlx::query_scalar(
        "select mode from github_catalog.repositories where provider_repository_id = $1",
    )
    .bind(provider_repository_id)
    .fetch_one(database.pool())
    .await?;
    Ok(mode)
}

#[tokio::test]
async fn explicit_track_request_records_transition_and_sets_tracked()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    upsert_repository(&database.database, 990_601).await?;

    let outcome = set_repository_mode(
        &database.database,
        &context_for(account_id),
        track_request(990_601, "mode-track-1"),
    )
    .await?;

    assert_eq!(outcome.status, MutationStatus::Applied);
    assert_eq!(
        current_mode(&database.database, 990_601).await?.as_deref(),
        Some("tracked")
    );
    let audit: (String, String, String, String) = sqlx::query_as(
        "select operation_kind, principal, source, detail->>'to'
         from github_catalog.mutation_audit where idempotency_key = 'mode-track-1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(audit.0, "mode_set");
    assert_eq!(audit.1, "web:7");
    assert_eq!(audit.2, "web");
    assert_eq!(audit.3, "tracked");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn direct_auto_request_is_refused_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    upsert_repository(&database.database, 990_602).await?;

    let outcome: MutationOutcome = set_repository_mode(
        &database.database,
        &context_for(account_id),
        SetModeRequest {
            provider_repository_id: 990_602,
            mode: RequestedMode::Auto,
            idempotency_key: "mode-auto-1".to_owned(),
        },
    )
    .await?;

    assert!(matches!(
        outcome.status,
        MutationStatus::Rejected {
            reason: ratatoskr_github_catalog::RefusalReason::AutoNotDirectlyRequestable
        }
    ));
    assert_eq!(
        current_mode(&database.database, 990_602).await?,
        None,
        "a refused request must not classify the repository"
    );
    let applied_or_already: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'mode-auto-1'
           and outcome in ('applied', 'already_applied')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        applied_or_already, 0,
        "no record may claim a transition happened"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn ignoring_a_starred_repository_is_refused_without_state_change()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let repository_id = upsert_repository(&database.database, 990_603)
        .await?
        .repository_id;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, '2020-01-01T00:00:00Z', now())",
    )
    .bind(account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query("update github_catalog.repositories set mode = 'auto' where repository_id = $1")
        .bind(repository_id)
        .execute(database.database.pool())
        .await?;

    let outcome = set_repository_mode(
        &database.database,
        &context_for(account_id),
        SetModeRequest {
            provider_repository_id: 990_603,
            mode: RequestedMode::Ignored,
            idempotency_key: "mode-ignore-starred-1".to_owned(),
        },
    )
    .await?;

    assert!(matches!(
        outcome.status,
        MutationStatus::Rejected {
            reason: ratatoskr_github_catalog::RefusalReason::RepositoryCurrentlyStarred
        }
    ));
    assert_eq!(
        current_mode(&database.database, 990_603).await?.as_deref(),
        Some("auto"),
        "the refusal must leave the prior classification standing"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn starring_an_ignored_repository_is_refused_without_provider_call()
-> Result<(), Box<dyn std::error::Error>> {
    use ratatoskr_github_catalog::provider::ReqwestGithubApi;
    use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
    use ratatoskr_github_catalog::{MutationRequest, RepositoryRef, execute_mutation};
    use wiremock::MockServer;

    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("ignorer");
    // A connected, fully scoped account: only the ignore rule may refuse.
    let account_id = {
        let id = Uuid::now_v7();
        let granted: Vec<String> = vec!["repo".to_owned()];
        sqlx::query(
            "insert into github_catalog.github_accounts (account_id, owner_ref, status, granted_scopes)
             values ($1, 'ignorer', 'connected', $2)",
        )
        .bind(id)
        .bind(&granted)
        .execute(database.database.pool())
        .await?;
        id
    };
    upsert_repository(&database.database, 990_604).await?;
    sqlx::query("update github_catalog.repositories set mode = 'ignored' where provider_repository_id = 990_604")
        .execute(database.database.pool())
        .await?;

    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context_for(account_id),
        MutationRequest::Star {
            repository: RepositoryRef {
                provider_repository_id: 990_604,
                owner: "acme".to_owned(),
                name: "excluded".to_owned(),
            },
            idempotency_key: "star-ignored-1".to_owned(),
        },
    )
    .await?;

    assert!(matches!(
        outcome.status,
        MutationStatus::Rejected {
            reason: ratatoskr_github_catalog::RefusalReason::RepositoryIgnored
        }
    ));
    let received = server.received_requests().await.unwrap_or_default();
    assert!(
        received.is_empty(),
        "an ignored repository must never be starred upstream"
    );
    assert_eq!(
        current_mode(&database.database, 990_604).await?.as_deref(),
        Some("ignored")
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn re_requesting_current_mode_succeeds_as_no_op_with_single_record()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    upsert_repository(&database.database, 990_605).await?;
    set_repository_mode(
        &database.database,
        &context_for(account_id),
        track_request(990_605, "mode-first-1"),
    )
    .await?;

    let again = set_repository_mode(
        &database.database,
        &context_for(account_id),
        track_request(990_605, "mode-second-1"),
    )
    .await?;

    assert_eq!(again.status, MutationStatus::AlreadyApplied);
    let successful: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key in ('mode-first-1', 'mode-second-1')
           and outcome in ('applied', 'already_applied')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(successful, 2, "each distinct key records once");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn retrying_mode_request_with_same_key_yields_one_record()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    upsert_repository(&database.database, 990_606).await?;

    let first = set_repository_mode(
        &database.database,
        &context_for(account_id),
        track_request(990_606, "mode-retry-1"),
    )
    .await?;
    let second = set_repository_mode(
        &database.database,
        &context_for(account_id),
        track_request(990_606, "mode-retry-1"),
    )
    .await?;

    assert_eq!(first.status, MutationStatus::Applied);
    assert_eq!(second.status, MutationStatus::AlreadyApplied);
    let records: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'mode-retry-1'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(records, 1, "the replay must add no second record");

    database.cleanup().await?;
    Ok(())
}

/// Ignoring requires an unstarred state; over an unstarred repository the
/// transition lands and is audited with both classifications.
#[tokio::test]
async fn ignoring_an_unstarred_repository_lands_and_is_audited()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    upsert_repository(&database.database, 990_607).await?;

    let outcome = set_repository_mode(
        &database.database,
        &context_for(account_id),
        SetModeRequest {
            provider_repository_id: 990_607,
            mode: RequestedMode::Ignored,
            idempotency_key: "mode-ignore-1".to_owned(),
        },
    )
    .await?;

    assert_eq!(outcome.status, MutationStatus::Applied);
    let detail: (String, String) = sqlx::query_as(
        "select detail->>'from', detail->>'to' from github_catalog.mutation_audit
         where idempotency_key = 'mode-ignore-1' and outcome = 'applied'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(detail.0, "unclassified");
    assert_eq!(detail.1, "ignored");

    database.cleanup().await?;
    Ok(())
}
