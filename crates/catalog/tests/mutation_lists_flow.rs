//! End-to-end authorized mutations: wiremock provider plus disposable
//! catalog database, exercising authorization enforcement, idempotent
//! star/unstar and list-membership writes, audit-trail convergence, and
//! batched partial success.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    Database, MutationContext, MutationRequest, MutationSource, MutationStatus, RepositoryRef,
    execute_batch, execute_mutation, upsert_repository,
};
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
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
        "insert into github_catalog.github_accounts
             (account_id, owner_ref, status, provider_user_id, granted_scopes)
         values ($1, $2, 'connected', 1, $3)",
    )
    .bind(account_id)
    .bind(owner_ref)
    .bind(granted)
    .execute(database.pool())
    .await?;
    Ok(account_id)
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

/// Seeds one active native list for the account and returns its local id.
async fn seed_list(
    database: &Database,
    account_id: Uuid,
    provider_list_id: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let list_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.star_lists (list_id, account_id, provider_list_id, name)
         values ($1, $2, $3, $4)",
    )
    .bind(list_id)
    .bind(account_id)
    .bind(provider_list_id)
    .bind(format!("list {provider_list_id}"))
    .execute(database.pool())
    .await?;
    Ok(list_id)
}

/// Seeds the membership projection for one (list, repository) pair.
async fn seed_membership(
    database: &Database,
    list_id: Uuid,
    repository_id: Uuid,
    member: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if member {
        sqlx::query(
            "insert into github_catalog.star_list_memberships
                 (list_id, repository_id, member, last_observed_at)
             values ($1, $2, true, now())",
        )
        .bind(list_id)
        .bind(repository_id)
        .execute(database.pool())
        .await?;
    } else {
        sqlx::query(
            "insert into github_catalog.star_list_memberships
                 (list_id, repository_id, member, last_observed_at, observed_removed_at)
             values ($1, $2, false, now(), now())",
        )
        .bind(list_id)
        .bind(repository_id)
        .execute(database.pool())
        .await?;
    }
    Ok(())
}

fn list_add_request(list_id: Uuid, key: &str) -> MutationRequest {
    MutationRequest::ListMemberAdd {
        repository: RepositoryRef {
            provider_repository_id: 990_401,
            owner: "acme".to_owned(),
            name: "widgets".to_owned(),
        },
        list_id,
        idempotency_key: key.to_owned(),
    }
}

fn list_remove_request(list_id: Uuid, key: &str) -> MutationRequest {
    MutationRequest::ListMemberRemove {
        repository: RepositoryRef {
            provider_repository_id: 990_401,
            owner: "acme".to_owned(),
            name: "widgets".to_owned(),
        },
        list_id,
        idempotency_key: key.to_owned(),
    }
}

/// Mounts the updateUserListsForItem reply and captures nothing else; body
/// assertions read back through `received_requests()`.
async fn mount_set_lists(server: &MockServer) -> Result<(), Box<dyn std::error::Error>> {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("updateUserListsForItem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "updateUserListsForItem": {
                    "lists": [
                        { "id": "gid://list/a" },
                        { "id": "gid://list/b" },
                        { "id": "gid://list/c" }
                    ]
                }
            },
            "rateLimit": { "remaining": 4_987, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .expect(1)
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn adding_a_list_preserves_the_repositorys_other_live_lists()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_lookup(&server).await?;
    mount_set_lists(&server).await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("filing");
    let account_id = seed_account(&database.database, "filing", &["user"]).await?;
    let repository_id = upsert_repository(&database.database, 990_401)
        .await?
        .repository_id;
    let list_a = seed_list(&database.database, account_id, "gid://list/a").await?;
    let list_b = seed_list(&database.database, account_id, "gid://list/b").await?;
    let list_c = seed_list(&database.database, account_id, "gid://list/c").await?;
    seed_membership(&database.database, list_a, repository_id, true).await?;
    seed_membership(&database.database, list_b, repository_id, true).await?;

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        list_add_request(list_c, "retry-file-1"),
    )
    .await?;

    assert_eq!(outcome.status, MutationStatus::Applied);
    let received = server.received_requests().await.ok_or("no requests")?;
    let write_body = received
        .iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body))
        .find_map(|parsed| {
            parsed.ok().filter(|body| {
                body["query"]
                    .as_str()
                    .is_some_and(|q| q.contains("updateUserListsForItem"))
            })
        })
        .ok_or("the membership write never reached the wire")?;
    let written: Vec<String> = write_body["variables"]["listIds"]
        .as_array()
        .ok_or("listIds must be an array")?
        .iter()
        .map(|value| value.as_str().ok_or("non-string id").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        sorted(written),
        ["gid://list/a", "gid://list/b", "gid://list/c"],
        "the complete desired set must preserve every other live membership"
    );

    let filed: bool = sqlx::query_scalar(
        "select member from github_catalog.star_list_memberships
         where list_id = $1 and repository_id = $2",
    )
    .bind(list_c)
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(filed, "the local projection must record the filing");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn removing_a_list_leaves_remaining_memberships_in_place()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_lookup(&server).await?;
    mount_set_lists(&server).await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("unfiling");
    let account_id = seed_account(&database.database, "unfiling", &["user"]).await?;
    let repository_id = upsert_repository(&database.database, 990_401)
        .await?
        .repository_id;
    let list_a = seed_list(&database.database, account_id, "gid://list/a").await?;
    let list_b = seed_list(&database.database, account_id, "gid://list/b").await?;
    let list_c = seed_list(&database.database, account_id, "gid://list/c").await?;
    seed_membership(&database.database, list_a, repository_id, true).await?;
    seed_membership(&database.database, list_b, repository_id, true).await?;
    seed_membership(&database.database, list_c, repository_id, true).await?;

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        list_remove_request(list_a, "retry-unfile-1"),
    )
    .await?;

    assert_eq!(outcome.status, MutationStatus::Applied);
    let received = server.received_requests().await.ok_or("no requests")?;
    let write_body = received
        .iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body))
        .find_map(|parsed| {
            parsed.ok().filter(|body| {
                body["query"]
                    .as_str()
                    .is_some_and(|q| q.contains("updateUserListsForItem"))
            })
        })
        .ok_or("the membership write never reached the wire")?;
    let written: Vec<String> = write_body["variables"]["listIds"]
        .as_array()
        .ok_or("listIds must be an array")?
        .iter()
        .map(|value| value.as_str().ok_or("non-string id").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        sorted(written),
        ["gid://list/b", "gid://list/c"],
        "removal must touch only the targeted list"
    );
    let removed_evidenced: bool = sqlx::query_scalar(
        "select observed_removed_at is not null from github_catalog.star_list_memberships
         where list_id = $1 and repository_id = $2",
    )
    .bind(list_a)
    .bind(repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        removed_evidenced,
        "the self-inflicted membership removal records its exact evidence time"
    );

    database.cleanup().await?;
    Ok(())
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[tokio::test]
async fn list_write_requires_user_scope_and_is_refused_audited_otherwise()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("listscope");
    let account_id = seed_account(&database.database, "listscope", &["repo"]).await?;

    let context = context_for(account_id);
    let outcome = execute_mutation(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        list_add_request(Uuid::now_v7(), "retry-listscope-1"),
    )
    .await?;

    assert!(matches!(
        outcome.status,
        MutationStatus::Rejected {
            reason: ratatoskr_github_catalog::RefusalReason::MissingScope
        }
    ));
    let received = server.received_requests().await.unwrap_or_default();
    assert!(
        received.is_empty(),
        "a refused list write must never reach the provider"
    );
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key = 'retry-listscope-1' and outcome = 'rejected'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(audited, 1);

    database.cleanup().await?;
    Ok(())
}

fn repo_ref(provider_repository_id: i64, owner: &str, name: &str) -> RepositoryRef {
    RepositoryRef {
        provider_repository_id,
        owner: owner.to_owned(),
        name: name.to_owned(),
    }
}

/// Mounts a successful node-id reply for one repository's owner login.
async fn mount_node_id_for(
    server: &MockServer,
    owner: &str,
    node_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let needle = format!(r#""owner":"{owner}""#);
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(needle))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "repository": { "id": node_id } },
            "rateLimit": { "remaining": 4_980, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .mount(server)
        .await;
    Ok(())
}

/// Mounts a star-direction mutation reply keyed by the unique starrable id.
async fn mount_star_for(
    server: &MockServer,
    document_marker: &str,
    node_id: &str,
    status: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let needle = format!(r#""starrableId":"{node_id}""#);
    let response = if status == 200 {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "addStar": {
                    "starrable": { "databaseId": 1, "viewerHasStarred": true }
                }
            },
            "rateLimit": { "remaining": 4_979, "resetAt": "2026-08-26T10:00:00Z" }
        }))
    } else {
        ResponseTemplate::new(status)
    };
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(document_marker))
        .and(body_string_contains(needle))
        .respond_with(response)
        .mount(server)
        .await;
    Ok(())
}

/// Mounts an unstar mutation reply keyed by the unique starrable id.
async fn mount_unstar_for(
    server: &MockServer,
    node_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let needle = format!(r#""starrableId":"{node_id}""#);
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("removeStar"))
        .and(body_string_contains(needle))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "removeStar": {
                    "starrable": { "databaseId": 3, "viewerHasStarred": false }
                }
            },
            "rateLimit": { "remaining": 4_978, "resetAt": "2026-08-26T10:00:00Z" }
        })))
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn one_failing_operation_strands_nothing_in_a_batch() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_for(&server, "acme-alpha", "gid://repository/alpha").await?;
    mount_node_id_for(&server, "acme-beta", "gid://repository/beta").await?;
    // beta's star write fails at the provider; alpha and gamma succeed.
    mount_node_id_for(&server, "acme-gamma", "gid://repository/gamma").await?;
    mount_star_for(&server, "addStar", "gid://repository/alpha", 200).await?;
    mount_star_for(&server, "addStar", "gid://repository/beta", 502).await?;
    mount_unstar_for(&server, "gid://repository/gamma").await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("batching");
    let account_id = seed_account(&database.database, "batching", &["repo", "user"]).await?;
    let alpha = upsert_repository(&database.database, 990_501)
        .await?
        .repository_id;
    upsert_repository(&database.database, 990_502).await?;
    let gamma = upsert_repository(&database.database, 990_503)
        .await?
        .repository_id;
    // Gamma is genuinely starred locally, so its batched unstar is a real
    // transition rather than an already-held end state.
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, '2020-02-02T00:00:00Z', now())",
    )
    .bind(account_id)
    .bind(gamma)
    .execute(database.database.pool())
    .await?;
    let _ = alpha;

    let context = context_for(account_id);
    let outcomes = execute_batch(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        vec![
            MutationRequest::Star {
                repository: repo_ref(990_501, "acme-alpha", "alpha"),
                idempotency_key: "batch-1".to_owned(),
            },
            MutationRequest::Star {
                repository: repo_ref(990_502, "acme-beta", "beta"),
                idempotency_key: "batch-2".to_owned(),
            },
            MutationRequest::Unstar {
                repository: repo_ref(990_503, "acme-gamma", "gamma"),
                idempotency_key: "batch-3".to_owned(),
            },
        ],
    )
    .await?;

    assert_eq!(outcomes.len(), 3, "one outcome per submitted operation");
    assert_eq!(outcomes[0].status, MutationStatus::Applied);
    assert!(
        matches!(outcomes[1].status, MutationStatus::Failed { .. }),
        "the middle failure must be reported truthfully, got {:?}",
        outcomes[1].status
    );
    assert_eq!(outcomes[2].status, MutationStatus::Applied);

    let audited: Vec<(String, String)> = sqlx::query_as(
        "select idempotency_key, outcome from github_catalog.mutation_audit
         where idempotency_key like 'batch-%' order by audit_id",
    )
    .fetch_all(database.database.pool())
    .await?;
    assert_eq!(
        audited,
        [
            ("batch-1".to_owned(), "applied".to_owned()),
            ("batch-2".to_owned(), "failed".to_owned()),
            ("batch-3".to_owned(), "applied".to_owned()),
        ],
        "each operation carries its own audit entry"
    );

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn resubmitting_batch_retries_only_incomplete_operations()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let server = MockServer::start().await;
    mount_node_id_for(&server, "acme-alpha", "gid://repository/alpha").await?;
    mount_node_id_for(&server, "acme-beta", "gid://repository/beta").await?;
    mount_node_id_for(&server, "acme-gamma", "gid://repository/gamma").await?;
    mount_star_for(&server, "addStar", "gid://repository/alpha", 200).await?;
    // The first submission finds beta unavailable - exactly once, so the
    // resubmission falls through to the success mock mounted after it.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("addStar"))
        .and(body_string_contains(
            r#""starrableId":"gid://repository/beta""#,
        ))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_unstar_for(&server, "gid://repository/gamma").await?;
    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("resubmitting");
    let account_id = seed_account(&database.database, "resubmitting", &["repo", "user"]).await?;
    for provider_id in [990_501_i64, 990_502, 990_503] {
        upsert_repository(&database.database, provider_id).await?;
    }
    let gamma = upsert_repository(&database.database, 990_503)
        .await?
        .repository_id;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, starred_at, last_observed_at)
         values ($1, $2, true, '2020-03-03T00:00:00Z', now())",
    )
    .bind(account_id)
    .bind(gamma)
    .execute(database.database.pool())
    .await?;

    let context = context_for(account_id);
    let batch = || {
        vec![
            MutationRequest::Star {
                repository: repo_ref(990_501, "acme-alpha", "alpha"),
                idempotency_key: "rebatch-1".to_owned(),
            },
            MutationRequest::Star {
                repository: repo_ref(990_502, "acme-beta", "beta"),
                idempotency_key: "rebatch-2".to_owned(),
            },
            MutationRequest::Unstar {
                repository: repo_ref(990_503, "acme-gamma", "gamma"),
                idempotency_key: "rebatch-3".to_owned(),
            },
        ]
    };
    let first = execute_batch(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        batch(),
    )
    .await?;
    assert!(matches!(first[1].status, MutationStatus::Failed { .. }));

    // The provider recovered; only beta may reach the wire this time.
    mount_node_id_for(&server, "acme-beta-retry-guard", "unused").await?;
    mount_star_for(&server, "addStar", "gid://repository/beta", 200).await?;
    let requests_before = server.received_requests().await.ok_or("no requests")?.len();

    let second = execute_batch(
        &database.database,
        &gateway,
        &ledger,
        &token,
        Some("token-value"),
        &context,
        batch(),
    )
    .await?;

    assert_eq!(second[0].status, MutationStatus::AlreadyApplied);
    assert_eq!(second[1].status, MutationStatus::Applied);
    assert_eq!(second[2].status, MutationStatus::AlreadyApplied);
    let requests_after = server.received_requests().await.ok_or("no requests")?.len();
    let new_requests = requests_after - requests_before;
    assert!(
        (2..=3).contains(&new_requests),
        "only the incomplete operation may hit the wire (node lookup plus write), got {new_requests}"
    );
    let successes: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.mutation_audit
         where idempotency_key in ('rebatch-1', 'rebatch-3')
           and outcome in ('applied', 'already_applied')",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        successes, 2,
        "no duplicate successful entries appear for already-succeeded keys"
    );

    database.cleanup().await?;
    Ok(())
}
