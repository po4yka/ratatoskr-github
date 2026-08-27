//! End-to-end star-list snapshots: wiremock GraphQL provider plus disposable
//! catalog database, exercising complete enumeration of native lists and
//! memberships, the atomic promotion into list authority, evidenced
//! membership observations, tombstoned lists, truncation refusal, and the
//! read surface.

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{StarListSnapshotOutcome, run_star_list_snapshot};
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Seeds one connected account row and returns its id.
async fn seed_account(
    database: &ratatoskr_github_catalog::Database,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.github_accounts
             (account_id, owner_ref, status, provider_user_id)
         values ($1, 'tester', 'connected', 1)",
    )
    .bind(account_id)
    .execute(database.pool())
    .await?;
    Ok(account_id)
}

/// Builds one item edge for a repository node.
fn item_edge(database_id: i64, name_with_owner: &str) -> serde_json::Value {
    json!({
        "node": {
            "__typename": "Repository",
            "databaseId": database_id,
            "nameWithOwner": name_with_owner,
        }
    })
}

/// Builds one list edge with its inline items.
fn list_edge(
    gid: &str,
    name: &str,
    truncated: bool,
    items: &[serde_json::Value],
) -> serde_json::Value {
    json!({
        "node": {
            "id": gid,
            "name": name,
            "items": {
                "pageInfo": {"hasNextPage": truncated},
                "edges": items,
            }
        }
    })
}

/// Wraps list edges into one full GraphQL enumeration page.
fn lists_page(edges: &[serde_json::Value], end_cursor: Option<&str>) -> String {
    json!({
        "data": {
            "viewer": {
                "lists": {
                    "pageInfo": {"hasNextPage": true, "endCursor": end_cursor},
                    "edges": edges,
                }
            },
            "rateLimit": {
                "cost": 1,
                "remaining": 4998,
                "resetAt": "2026-08-25T22:00:00Z",
            },
        }
    })
    .to_string()
}

/// Mounts one enumeration page matched by whether it carries a continuation.
async fn mount_page(
    server: &MockServer,
    carries_cursor: bool,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = ResponseTemplate::new(200).set_body_string(body);
    let continuation = if carries_cursor {
        r#""after":"MQ""#
    } else {
        r#""after":null"#
    };
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(continuation))
        .respond_with(response)
        .up_to_n_times(2)
        .mount(server)
        .await;
    Ok(())
}

/// Seeds one native list row and returns its id.
async fn seed_list(
    database: &ratatoskr_github_catalog::Database,
    account_id: Uuid,
    provider_list_id: &str,
    name: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let list_id = Uuid::now_v7();
    sqlx::query(
        "insert into github_catalog.star_lists (list_id, account_id, provider_list_id, name)
         values ($1, $2, $3, $4)",
    )
    .bind(list_id)
    .bind(account_id)
    .bind(provider_list_id)
    .bind(name)
    .execute(database.pool())
    .await?;
    Ok(list_id)
}

/// Seeds one membership projection row.
async fn seed_member(
    database: &ratatoskr_github_catalog::Database,
    list_id: Uuid,
    provider_repository_id: i64,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let identity =
        ratatoskr_github_catalog::upsert_repository(database, provider_repository_id).await?;
    sqlx::query(
        "insert into github_catalog.star_list_memberships
             (list_id, repository_id, member, last_observed_at)
         values ($1, $2, true, now())",
    )
    .bind(list_id)
    .bind(identity.repository_id)
    .execute(database.pool())
    .await?;
    Ok(identity.repository_id)
}

/// Reads the full membership projection as
/// (provider list id, provider repository id, member) triples in order.
async fn projection_dump(
    database: &ratatoskr_github_catalog::Database,
) -> Result<Vec<(String, i64, bool)>, Box<dyn std::error::Error>> {
    let rows: Vec<(String, i64, bool)> = sqlx::query_as(
        "select l.provider_list_id, r.provider_repository_id, m.member
             from github_catalog.star_list_memberships m
             join github_catalog.star_lists l on l.list_id = m.list_id
             join github_catalog.repositories r on r.repository_id = m.repository_id
             order by l.provider_list_id, r.provider_repository_id",
    )
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .collect();
    Ok(rows)
}
/// Reads every observation bound to a run as
/// (provider list id, provider repository id, member) triples in order.
async fn observations_for_run(
    database: &ratatoskr_github_catalog::Database,
    run_id: Uuid,
) -> Result<Vec<(String, i64, bool)>, Box<dyn std::error::Error>> {
    let rows: Vec<(String, i64, bool)> = sqlx::query_as(
        "select l.provider_list_id, r.provider_repository_id, o.member
             from github_catalog.star_list_membership_observations o
             join github_catalog.star_lists l on l.list_id = o.list_id
             join github_catalog.repositories r on r.repository_id = o.repository_id
         where o.evidence_run_id = $1
         order by l.provider_list_id, r.provider_repository_id, o.member",
    )
    .bind(run_id)
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .collect();
    Ok(rows)
}

/// Seeds prior list authority for the swap fixture: L1 holds A and B,
/// L2 holds A. Returns L1's id and A's repository id.
async fn seed_prior_swap_authority(
    database: &TestDatabase,
    account_id: Uuid,
) -> Result<(Uuid, Uuid), Box<dyn std::error::Error>> {
    let l1 = seed_list(
        &database.database,
        account_id,
        "gid://UserList/1",
        "old name",
    )
    .await?;
    let l2 = seed_list(&database.database, account_id, "gid://UserList/2", "steady").await?;
    let repo_a = seed_member(&database.database, l1, 300_000_070).await?;
    seed_member(&database.database, l1, 300_000_071).await?;
    seed_member(&database.database, l2, 300_000_070).await?;
    Ok((l1, repo_a))
}

#[tokio::test]
async fn completed_swap_applies_diff_records_observations_and_repeats_inertly()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    // Prior authority: L1 holds A and B; L2 holds A.
    let (l1, repo_a) = seed_prior_swap_authority(&database, account_id).await?;

    // Fresh enumeration: L1 renamed holding B and newcomer C; L2 unchanged.
    let server = MockServer::start().await;
    let first_page = lists_page(
        &[
            list_edge(
                "gid://UserList/1",
                "new name",
                false,
                &[
                    item_edge(300_000_071, "acme/beta"),
                    item_edge(300_000_072, "acme/gamma"),
                ],
            ),
            list_edge(
                "gid://UserList/2",
                "steady",
                false,
                &[item_edge(300_000_070, "acme/alpha")],
            ),
        ],
        Some("MQ"),
    );
    mount_page(&server, false, first_page).await?;
    mount_page(&server, true, lists_page(&[], None)).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let token = TokenRef::from_label("swap-e2e");
    let run = || async {
        run_star_list_snapshot(
            &database.database,
            &gateway,
            &RateLimitLedger::new(),
            &token,
            account_id,
        )
        .await
    };

    let first = run().await?;
    let StarListSnapshotOutcome::Completed {
        run_id: first_run,
        additions,
        removals,
        ..
    } = first
    else {
        return Err(format!("the first snapshot must complete, got {first:?}").into());
    };
    assert_eq!(
        (additions, removals),
        (1, 1),
        "exactly C is added and exactly A is removed"
    );

    // The rename propagated and both lists stay active.
    let l1_row: (String, String) =
        sqlx::query_as("select name, status from github_catalog.star_lists where list_id = $1")
            .bind(l1)
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(l1_row, ("new name".to_owned(), "active".to_owned()));

    // A was demoted with removal evidence bound to the completing run.
    let demoted: (bool, Option<String>, Option<Uuid>) = sqlx::query_as(
        "select member, observed_removed_at::text, evidence_run_id
             from github_catalog.star_list_memberships
          where list_id = $1 and repository_id = $2",
    )
    .bind(l1)
    .bind(repo_a)
    .fetch_one(database.database.pool())
    .await?;
    assert!(!demoted.0, "A must be a non-member of its abandoned list");
    assert!(
        demoted.1.is_some(),
        "the demotion carries an observation time"
    );
    assert_eq!(
        demoted.2,
        Some(first_run),
        "the completing run is the evidence"
    );

    // The diff observations cover every seen membership plus the removal.
    assert_eq!(
        observations_for_run(&database.database, first_run).await?,
        vec![
            ("gid://UserList/1".to_owned(), 300_000_070, false),
            ("gid://UserList/1".to_owned(), 300_000_071, true),
            ("gid://UserList/1".to_owned(), 300_000_072, true),
            ("gid://UserList/2".to_owned(), 300_000_070, true),
        ],
        "every seen membership plus every removal is append-only evidence"
    );

    // Repeating over identical upstream records zero transitions and leaves
    // the projection byte-identical; only fresh confirmations appear.
    assert_repeated_run_is_inert(&database, &gateway, &token, account_id).await?;

    database.cleanup().await?;
    Ok(())
}

/// Reruns the enumeration over identical fixtures and pins inertness: zero
/// transitions, a byte-identical projection, and no duplicated removal.
async fn assert_repeated_run_is_inert(
    database: &TestDatabase,
    gateway: &ReqwestGithubApi,
    token: &TokenRef,
    account_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let projection_before = projection_dump(&database.database).await?;
    let second = run_star_list_snapshot(
        &database.database,
        gateway,
        &RateLimitLedger::new(),
        token,
        account_id,
    )
    .await?;
    let StarListSnapshotOutcome::Completed {
        additions,
        removals,
        ..
    } = second
    else {
        return Err(format!("the second snapshot must complete, got {second:?}").into());
    };
    assert_eq!(
        (additions, removals),
        (0, 0),
        "converged state admits no transitions"
    );
    assert_eq!(
        projection_before,
        projection_dump(&database.database).await?,
        "repeated application leaves the projection byte-identical"
    );
    let removal_rows_total: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_list_membership_observations where not member",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        removal_rows_total, 1,
        "the same drift must never be recorded twice as a removal"
    );
    Ok(())
}

#[tokio::test]
async fn list_snapshot_enumerates_all_pages_stages_memberships_and_completes_run()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    let server = MockServer::start().await;
    let first_page = lists_page(
        &[
            list_edge(
                "gid://UserList/5021471",
                "Rust crates",
                false,
                &[item_edge(300_000_101, "acme/alpha")],
            ),
            list_edge(
                "gid://UserList/5021472",
                "Read later",
                false,
                &[
                    item_edge(300_000_102, "acme/beta"),
                    item_edge(300_000_103, "acme/gamma"),
                ],
            ),
        ],
        Some("MQ"),
    );
    let final_page = lists_page(&[], None);
    mount_page(&server, false, first_page).await?;
    mount_page(&server, true, final_page).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let token = TokenRef::from_label("list-snapshot-e2e");

    let outcome = run_star_list_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;

    let StarListSnapshotOutcome::Completed {
        pages_processed,
        items_observed,
        ..
    } = outcome
    else {
        return Err(format!("the snapshot must complete its traversal, got {outcome:?}").into());
    };
    assert_eq!(
        (pages_processed, items_observed),
        (2, 3),
        "both pages count and every staged membership is observed"
    );

    // Every listed repository exists under stable numeric identity with its
    // owner/name alias observed.
    let repository_count: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.repositories")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        repository_count, 3,
        "every listed repository must be known before any authority decision"
    );
    let aliased: Option<Uuid> = sqlx::query_scalar(
        "select repository_id from github_catalog.repository_aliases
         where alias_kind = 'owner_name' and alias_value = 'acme/alpha' and status = 'active'",
    )
    .fetch_optional(database.database.pool())
    .await?;
    assert!(
        aliased.is_some(),
        "the owner/name seen through the listing must be observed as an alias"
    );

    // Exactly one completed star_lists run records the walk.
    let runs: Vec<(String, String)> =
        sqlx::query_as("select mode, status from github_catalog.sync_runs where account_id = $1")
            .bind(account_id)
            .fetch_all(database.database.pool())
            .await?;
    assert_eq!(
        runs,
        vec![("star_lists".to_owned(), "completed".to_owned())],
        "the walk must be recorded as one completed star_lists-mode run"
    );

    // Staging is cleared once the run reaches its terminal state.
    let staged: i64 = sqlx::query_scalar("select count(*) from github_catalog.list_snapshot_items")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(staged, 0, "completed runs leave no staging rows behind");

    database.cleanup().await?;
    Ok(())
}
