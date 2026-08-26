//! End-to-end authorized mutations: wiremock provider plus disposable
//! catalog database, exercising authorization enforcement, idempotent
//! star/unstar and list-membership writes, audit-trail convergence, and
//! batched partial success.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    Database, MutationContext, MutationRequest, MutationSource, MutationStatus, RepositoryRef,
    execute_mutation, upsert_repository,
};
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Seeds one connected account row with the given granted scopes and returns
/// its id.
async fn seed_account(
    database: &Database,
    owner_ref: &str,
    scopes: &[&str],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let account_id = Uuid::now_v7();
    let granted: Vec<String> = scopes.iter().map(|scope| (*scope).to_owned()).collect();
    sqlx::query(
        "insert into github_catalog.github_accounts (account_id, owner_ref, status, granted_scopes)
         values ($1, $2, 'connected', $3)",
    )
    .bind(account_id)
    .bind(owner_ref)
    .bind(granted)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

fn star_request(key: &str) -> MutationRequest {
    MutationRequest::Star {
        repository: RepositoryRef {
            provider_repository_id: 990_401,
            owner: "acme".to_owned(),
            name: "widgets".to_owned(),
        },
        idempotency_key: key.to_owned(),
    }
}

fn context_for(account_id: Uuid) -> MutationContext {
    MutationContext {
        account_id,
        principal: "telegram:42".to_owned(),
        source: MutationSource::Telegram,
    }
}

/// Mounts the node-id resolution reply served exactly once.
async fn mount_node_id_lookup(server: &MockServer) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("repository(owner"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "repository": { "id": "gid://repository/990401" } },
            "rateLimit": { "remaining": 4_991, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts the addStar reply served exactly once.
async fn mount_add_star(
    server: &MockServer,
    viewer_has_starred: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("addStar"))
        .and(header("authorization", "Bearer token-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "addStar": {
                    "starrable": { "databaseId": 990_401, "viewerHasStarred": viewer_has_starred }
                }
            },
            "rateLimit": { "remaining": 4_990, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn star_without_required_scopes_is_refused_and_audited()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("scopeless");
    let account_id = seed_account(&database.database, "scopeless", &[]).await?;

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        None,
        &context,
        star_request("retry-scopeless-1"),
    )
    .await?;

    assert!(
        matches!(
            outcome.status,
            MutationStatus::Rejected {
                reason: ratatoskr_github_catalog::RefusalReason::MissingScope
            }
        ),
        "an account without star scopes must be refused, got {:?}",
        outcome.status
    );
    let received = server.received_requests().await.unwrap_or_default();
    assert!(
        received.is_empty(),
        "a refused mutation must never reach the provider, got {} requests",
        received.len()
    );
    let audited: (i64, Option<String>) = sqlx::query_as(
        "select count(*), max(detail->>'reason') from github_catalog.mutation_audit
         where idempotency_key = 'retry-scopeless-1' and outcome = 'rejected'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        audited.0, 1,
        "the refusal must leave exactly one audit record"
    );
    assert_eq!(
        audited.1.as_deref(),
        Some("missing_scope"),
        "the audit detail must name the missing capability"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn mutation_for_unconnected_account_is_refused_without_provider_call()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("unconnected");

    let context = context_for(Uuid::now_v7());
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        None,
        &context,
        star_request("retry-unconnected-1"),
    )
    .await?;

    assert!(
        matches!(
            outcome.status,
            MutationStatus::Rejected {
                reason: ratatoskr_github_catalog::RefusalReason::AccountNotConnected
            }
        ),
        "an unknown account must be refused as not connected, got {:?}",
        outcome.status
    );
    let received = server.received_requests().await.unwrap_or_default();
    assert!(
        received.is_empty(),
        "a refused mutation must never reach the provider, got {} requests",
        received.len()
    );
    let audited: (i64,) = sqlx::query_as(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'retry-unconnected-1' and outcome = 'rejected'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        audited.0, 1,
        "the refusal must leave exactly one audit record"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn successful_star_sets_local_star_state_and_records_one_audit_entry()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_lookup(&server).await?;
    mount_add_star(&server, true).await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("starring");
    let account_id = seed_account(&database.database, "starring", &["public_repo"]).await?;
    upsert_repository(&database.database, 990_401).await?;

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        star_request("retry-star-1"),
    )
    .await?;

    assert_eq!(
        outcome.status,
        MutationStatus::Applied,
        "a newly reached star must report applied"
    );
    let received = server
        .received_requests()
        .await
        .ok_or("requests were not recorded")?;
    assert_eq!(
        received.len(),
        2,
        "one node-id resolution plus one addStar must be the whole wire cost"
    );

    let state: (bool, Option<String>) = sqlx::query_as(
        "select starred, starred_at::text from github_catalog.current_star_state
         where account_id = $1 and repository_id = (
             select repository_id from github_catalog.repositories
             where provider_repository_id = 990_401)",
    )
    .bind(account_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(state.0, "the local projection must carry the star");
    assert!(
        state.1.is_some(),
        "a mutation-established star records its action time as establishment"
    );

    let mode: Option<String> = sqlx::query_scalar(
        "select mode from github_catalog.repositories where provider_repository_id = 990_401",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        mode.as_deref(),
        Some("auto"),
        "staring an unclassified repository promotes it to auto"
    );

    let audited: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'retry-star-1' and outcome = 'applied'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(audited, 1, "exactly one successful audit entry");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn retrying_completed_star_with_same_key_short_circuits_to_already_applied()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_lookup(&server).await?;
    mount_add_star(&server, true).await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("replaying");
    let account_id = seed_account(&database.database, "replaying", &["repo"]).await?;
    upsert_repository(&database.database, 990_401).await?;
    let context = context_for(account_id);

    let first = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        star_request("retry-replay-1"),
    )
    .await?;
    assert_eq!(first.status, MutationStatus::Applied);

    let second = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        star_request("retry-replay-1"),
    )
    .await?;

    assert_eq!(
        second.status,
        MutationStatus::AlreadyApplied,
        "the replay must return the recorded outcome"
    );
    let received = server
        .received_requests()
        .await
        .ok_or("requests were not recorded")?;
    assert_eq!(
        received.len(),
        2,
        "a replay must not contact the provider again"
    );
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'retry-replay-1'
           and outcome in ('applied', 'already_applied')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(audited, 1, "exactly one successful audit entry for the key");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn starring_already_starred_repository_reports_already_applied_without_touching_timestamps()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_lookup(&server).await?;
    mount_add_star(&server, true).await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("confirming");
    let account_id = seed_account(&database.database, "confirming", &["public_repo"]).await?;
    let repository_id = upsert_repository(&database.database, 990_401)
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

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        star_request("retry-confirm-1"),
    )
    .await?;

    assert_eq!(
        outcome.status,
        MutationStatus::AlreadyApplied,
        "locally held star plus provider confirmation reports already-applied"
    );
    let established: Option<String> = sqlx::query_scalar(
        "select starred_at::text from github_catalog.current_star_state
         where account_id = $1 and repository_id = $2",
    )
    .bind(account_id)
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        established
            .as_deref()
            .is_some_and(|value| value.starts_with("2020-01-01")),
        "establishment timestamps must survive confirmations, got {established:?}"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn failed_star_attempt_does_not_consume_its_idempotency_key()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    // Two attempts each pay one node-id lookup; the failing addStar mock
    // answers exactly once (up_to_n_times gates routing), so the retry falls
    // through to the success mock mounted after it.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("repository(owner"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "repository": { "id": "gid://repository/990401" } },
            "rateLimit": { "remaining": 4_991, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("addStar"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("addStar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "addStar": {
                    "starrable": { "databaseId": 990_401, "viewerHasStarred": true }
                }
            },
            "rateLimit": { "remaining": 4_989, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .mount(&server)
        .await;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("retrying");
    let account_id = seed_account(&database.database, "retrying", &["public_repo"]).await?;
    upsert_repository(&database.database, 990_401).await?;
    let context = context_for(account_id);

    let failed = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        star_request("retry-failed-1"),
    )
    .await?;
    let MutationStatus::Failed { reason } = failed.status else {
        return Err(format!(
            "a provider failure must report failed, got {:?}",
            failed.status
        )
        .into());
    };
    assert!(
        !reason.to_lowercase().contains("token"),
        "failure reasons must not carry credential material"
    );

    let retried = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        star_request("retry-failed-1"),
    )
    .await?;

    assert_eq!(
        retried.status,
        MutationStatus::Applied,
        "the retry after failure must execute and apply"
    );
    let outcomes: Vec<String> = sqlx::query_scalar(
        "select outcome from github_catalog.mutation_audit
         where idempotency_key = 'retry-failed-1' order by created_at",
    )
    .fetch_all(database.database.pool())
    .await?;
    assert_eq!(
        outcomes,
        ["failed", "applied"],
        "the failure stays audited while exactly one success owns the key"
    );

    database.cleanup().await?;
    Ok(())
}

/// Mounts the removeStar reply served exactly once.
async fn mount_remove_star(
    server: &MockServer,
    viewer_has_starred: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("removeStar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "removeStar": {
                    "starrable": { "databaseId": 990_401, "viewerHasStarred": viewer_has_starred }
                }
            },
            "rateLimit": { "remaining": 4_989, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

fn unstar_request(key: &str) -> MutationRequest {
    MutationRequest::Unstar {
        repository: RepositoryRef {
            provider_repository_id: 990_401,
            owner: "acme".to_owned(),
            name: "widgets".to_owned(),
        },
        idempotency_key: key.to_owned(),
    }
}

#[tokio::test]
async fn unstar_follows_the_same_idempotent_contract_as_star()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_lookup(&server).await?;
    mount_remove_star(&server, false).await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("unstarring");
    let account_id = seed_account(&database.database, "unstarring", &["repo"]).await?;
    let repository_id = upsert_repository(&database.database, 990_401)
        .await?
        .repository_id;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, '2020-06-01T00:00:00Z', now())",
    )
    .bind(account_id)
    .bind(repository_id)
    .execute(database.database.pool())
    .await?;
    sqlx::query("update github_catalog.repositories set mode = 'auto' where repository_id = $1")
        .bind(repository_id)
        .execute(database.database.pool())
        .await?;

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        unstar_request("retry-unstar-1"),
    )
    .await?;

    assert_eq!(outcome.status, MutationStatus::Applied);
    let state: (bool, Option<String>, Option<String>) = sqlx::query_as(
        "select starred, starred_at::text, observed_unstarred_at::text
         from github_catalog.current_star_state
         where account_id = $1 and repository_id = $2",
    )
    .bind(account_id)
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(!state.0, "the projection must carry the removal");
    assert!(
        state.1.is_none(),
        "an unstarred state carries no establishment timestamp"
    );
    assert!(
        state.2.is_some(),
        "the self-inflicted removal records its exact evidence time"
    );
    let mode: Option<String> =
        sqlx::query_scalar("select mode from github_catalog.repositories where repository_id = $1")
            .bind(repository_id)
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        mode, None,
        "releasing the star releases auto governance to unclassified"
    );

    // The replay contract holds for unstar too: no further provider calls.
    let replayed = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        unstar_request("retry-unstar-1"),
    )
    .await?;
    assert_eq!(replayed.status, MutationStatus::AlreadyApplied);
    let received = server
        .received_requests()
        .await
        .ok_or("requests were not recorded")?;
    assert_eq!(
        received.len(),
        2,
        "a replay must not contact the provider again"
    );
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'retry-unstar-1'
           and outcome in ('applied', 'already_applied')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(audited, 1, "exactly one successful audit entry for the key");

    database.cleanup().await?;
    Ok(())
}
