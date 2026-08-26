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
        "insert into github_catalog.github_accounts (account_id, owner_ref, status)
         values ($1, 'tester', 'connected')",
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

#[tokio::test]
async fn read_surface_reports_active_lists_and_current_members_only()
-> Result<(), Box<dyn std::error::Error>> {
    use ratatoskr_github_catalog::{
        ListMember, StarListSummary, current_list_members, current_star_lists,
    };

    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;
    let other_account = seed_account(&database.database).await?;

    // Authority state: L1 active with B,C members and A demoted; L2
    // tombstoned; the other account owns its own separate list.
    let l1 = seed_list(
        &database.database,
        account_id,
        "gid://UserList/1",
        "current name",
    )
    .await?;
    seed_member(&database.database, l1, 300_000_071).await?;
    seed_member(&database.database, l1, 300_000_072).await?;
    let demoted =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_070).await?;
    sqlx::query(
        "insert into github_catalog.star_list_memberships
             (list_id, repository_id, member, last_observed_at,
              observed_removed_at)
         values ($1, $2, false, now(), now())",
    )
    .bind(l1)
    .bind(demoted.repository_id)
    .execute(database.database.pool())
    .await?;
    let l2 = seed_list(&database.database, account_id, "gid://UserList/2", "gone").await?;
    sqlx::query(
        "update github_catalog.star_lists set status = 'removed', observed_removed_at = now()
         where list_id = $1",
    )
    .bind(l2)
    .execute(database.database.pool())
    .await?;
    let _other_list = seed_list(
        &database.database,
        other_account,
        "gid://UserList/3",
        "other",
    )
    .await?;

    let lists = current_star_lists(&database.database, account_id).await?;
    assert_eq!(
        lists,
        vec![StarListSummary {
            list_id: l1,
            provider_list_id: "gid://UserList/1".to_owned(),
            name: "current name".to_owned(),
        }],
        "only active lists are reported, under their promoted names"
    );

    let members = current_list_members(&database.database, l1).await?;
    let member_ids: Vec<Uuid> = members
        .iter()
        .map(|m: &ListMember| m.repository_id)
        .collect();
    let expected_ids: Vec<Uuid> = sqlx::query_as::<_, (Uuid,)>(
        "select r.repository_id from github_catalog.repositories r
         where r.provider_repository_id in (300000071, 300000072)
         order by r.provider_repository_id",
    )
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .map(|row| row.0)
    .collect();
    assert_eq!(
        member_ids, expected_ids,
        "current members include additions and exclude demoted ones"
    );
    assert!(
        !member_ids.contains(&demoted.repository_id),
        "a demoted membership is not a current member"
    );

    let tombstoned_members = current_list_members(&database.database, l2).await?;
    assert!(
        tombstoned_members.is_empty(),
        "a removed list holds no current members"
    );
    let other_lists = current_star_lists(&database.database, other_account).await?;
    assert_eq!(other_lists.len(), 1, "accounts see only their own lists");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn mid_scan_provider_failure_preserves_prior_list_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    let l1 = seed_list(&database.database, account_id, "gid://UserList/1", "prior").await?;
    seed_member(&database.database, l1, 300_000_070).await?;

    // Page one serves fine with a continuation; page two fails permanently.
    let server = MockServer::start().await;
    mount_page(
        &server,
        false,
        lists_page(
            &[list_edge(
                "gid://UserList/3",
                "second page list",
                false,
                &[item_edge(300_000_078, "acme/paged")],
            )],
            Some("MQ"),
        ),
    )
    .await?;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(r#""after":"MQ""#))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_star_list_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("failure-e2e"),
        account_id,
    )
    .await?;
    let StarListSnapshotOutcome::Failed { run_id } = outcome else {
        return Err(
            format!("a permanent provider failure must fail the run, got {outcome:?}").into(),
        );
    };
    let (status, reason): (String, Option<String>) = sqlx::query_as(
        "select status, failure_reason from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(status, "failed");
    assert!(reason.is_some(), "the run row must name its failure");

    // Prior authority unchanged; no observations for anything this run saw.
    let prior: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_list_memberships
         where list_id = $1 and member",
    )
    .bind(l1)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(prior, 1);
    let new_authority: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_lists where provider_list_id = 'gid://UserList/3'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        new_authority, 0,
        "nothing observed before the failure reaches authority"
    );
    let observation_count: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.star_list_membership_observations")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(observation_count, 0, "failures record no observations");
    let staged: i64 = sqlx::query_scalar("select count(*) from github_catalog.list_snapshot_items")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(staged, 0, "staging clears when a run dies");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn budget_refusal_pauses_then_resume_continues_from_recorded_cursor()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    // Server one: page one answers with an exhausted rate budget so the
    // next acquisition refuses after it is durably staged.
    let server_one = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains(r#""after":null"#))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(lists_page(
                    &[list_edge(
                        "gid://UserList/1",
                        "paused list",
                        false,
                        &[item_edge(300_000_090, "acme/pause")],
                    )],
                    Some("MQ"),
                ))
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "4102444800"),
        )
        .expect(1)
        .mount(&server_one)
        .await;

    let gateway_one = ReqwestGithubApi::for_base_url(&server_one.uri())?;
    let ledger = RateLimitLedger::new();
    let token = TokenRef::from_label("resume-e2e");
    let paused = run_star_list_snapshot(
        &database.database,
        &gateway_one,
        &ledger,
        &token,
        account_id,
    )
    .await?;
    let StarListSnapshotOutcome::Paused { run_id, .. } = paused else {
        return Err(format!("an exhausted budget must pause the scan, got {paused:?}").into());
    };

    // The paused run stays open with its continuation recorded.
    let status: String =
        sqlx::query_scalar("select status from github_catalog.sync_runs where sync_run_id = $1")
            .bind(run_id)
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(status, "running", "a paused run stays open");
    let checkpoint_cursor: Option<String> = sqlx::query_scalar(
        "select graphql_cursor from github_catalog.sync_checkpoints where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(database.database.pool())
    .await?
    .flatten();
    assert_eq!(
        checkpoint_cursor.as_deref(),
        Some("MQ"),
        "the checkpoint records where the walk stopped"
    );

    // Resuming against a fresh budget and server continues from the token
    // without refetching page one.
    let server_two = MockServer::start().await;
    mount_page(&server_two, true, lists_page(&[], None)).await?;
    let gateway_two = ReqwestGithubApi::for_base_url(&server_two.uri())?;
    let resumed = run_star_list_snapshot(
        &database.database,
        &gateway_two,
        &RateLimitLedger::new(),
        &token,
        account_id,
    )
    .await?;
    assert!(
        matches!(resumed, StarListSnapshotOutcome::Completed { .. }),
        "the resumed walk must complete, got {resumed:?}"
    );

    let received = server_two.received_requests().await.unwrap_or_default();
    assert!(
        !received.is_empty(),
        "the resume must have requested its page"
    );
    for request in &received {
        assert!(
            String::from_utf8_lossy(&request.body).contains("MQ"),
            "every resumed request must carry the recorded continuation token"
        );
    }

    // The staged membership from before the pause survived into authority.
    let members: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_list_memberships m
         join github_catalog.star_lists l on l.list_id = m.list_id
         where l.provider_list_id = 'gid://UserList/1' and m.member",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(members, 1, "page-one staging survives the pause");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn truncated_list_fails_run_naming_it_without_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    // Prior authority exists and must survive untouched.
    let l1 = seed_list(&database.database, account_id, "gid://UserList/1", "prior").await?;
    seed_member(&database.database, l1, 300_000_070).await?;

    // The enumerated list reports more items than the page carries.
    let server = MockServer::start().await;
    let first_page = lists_page(
        &[list_edge(
            "gid://UserList/5",
            "too big",
            true,
            &[item_edge(300_000_075, "acme/big")],
        )],
        Some("MQ"),
    );
    mount_page(&server, false, first_page).await?;
    mount_page(&server, true, lists_page(&[], None)).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_star_list_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("truncation-e2e"),
        account_id,
    )
    .await?;
    let StarListSnapshotOutcome::Failed { run_id } = outcome else {
        return Err(format!("a truncated enumeration must fail the run, got {outcome:?}").into());
    };
    let reason: String = sqlx::query_scalar(
        "select failure_reason from github_catalog.sync_runs where sync_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert!(
        reason.contains("gid://UserList/5"),
        "the failure reason must name the truncated list: {reason}"
    );

    // Prior list authority is unchanged and nothing from this run leaked.
    let prior: (String, i64) = sqlx::query_as(
        "select l.status, count(m.repository_id) filter (where m.member)
             from github_catalog.star_lists l
             left join github_catalog.star_list_memberships m on m.list_id = l.list_id
         where l.list_id = $1 group by l.status",
    )
    .bind(l1)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(prior, ("active".to_owned(), 1), "prior authority untouched");
    let new_list_rows: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_lists where provider_list_id = 'gid://UserList/5'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        new_list_rows, 0,
        "nothing from a truncated run reaches authority"
    );
    let observation_count: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.star_list_membership_observations")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        observation_count, 0,
        "a truncated enumeration records no observations"
    );
    let staged: i64 = sqlx::query_scalar("select count(*) from github_catalog.list_snapshot_items")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(staged, 0, "the dead run leaves no staging rows");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn removed_list_tombstones_with_evidence_and_demotes_its_memberships()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    // Prior authority: L1 holds A and B, L2 holds C.
    let l1 = seed_list(&database.database, account_id, "gid://UserList/1", "doomed").await?;
    let l2 = seed_list(
        &database.database,
        account_id,
        "gid://UserList/2",
        "survivor",
    )
    .await?;
    seed_member(&database.database, l1, 300_000_070).await?;
    seed_member(&database.database, l1, 300_000_071).await?;
    let survivor_repo = seed_member(&database.database, l2, 300_000_072).await?;

    // Fresh enumeration contains only L2.
    let server = MockServer::start().await;
    let first_page = lists_page(
        &[list_edge(
            "gid://UserList/2",
            "survivor",
            false,
            &[item_edge(300_000_072, "acme/c")],
        )],
        Some("MQ"),
    );
    mount_page(&server, false, first_page).await?;
    mount_page(&server, true, lists_page(&[], None)).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_star_list_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("tombstone-e2e"),
        account_id,
    )
    .await?;
    let StarListSnapshotOutcome::Completed {
        run_id, removals, ..
    } = outcome
    else {
        return Err(format!("the snapshot must complete, got {outcome:?}").into());
    };
    assert_eq!(
        removals, 2,
        "both memberships of the vanished list are removals"
    );

    // L1 remains as a tombstone with inferred observation time and evidence.
    let doomed: (String, Option<String>, Option<Uuid>) = sqlx::query_as(
        "select status, observed_removed_at::text, evidence_run_id
             from github_catalog.star_lists where list_id = $1",
    )
    .bind(l1)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(doomed.0, "removed");
    assert!(
        doomed.1.is_some(),
        "the tombstone carries an observation time"
    );
    assert_eq!(doomed.2, Some(run_id), "the completing run is the evidence");

    // Its memberships are all non-member with the same evidence.
    let demoted_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.star_list_memberships
         where list_id = $1 and not member
           and observed_removed_at is not null and evidence_run_id = $2",
    )
    .bind(l1)
    .bind(run_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        demoted_count, 2,
        "every membership of a removed list carries the same removal evidence"
    );

    // Nothing was deleted: the survivor list, its member, and the demoted
    // rows all remain queryable history.
    let total_lists: i64 = sqlx::query_scalar("select count(*) from github_catalog.star_lists")
        .fetch_one(database.database.pool())
        .await?;
    assert_eq!(total_lists, 2, "no list row is deleted");
    let survivor_membership: (bool,) = sqlx::query_as(
        "select member from github_catalog.star_list_memberships where repository_id = $1",
    )
    .bind(survivor_repo)
    .fetch_one(database.database.pool())
    .await?;
    assert!(survivor_membership.0, "the untouched list keeps its member");

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn listed_unstarred_repository_remains_member_without_touching_star_state()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let account_id = seed_account(&database.database).await?;

    // Repo X holds explicit unstarred state with removal evidence; repo Y
    // has never been star-observed at all.
    let identity_x =
        ratatoskr_github_catalog::upsert_repository(&database.database, 300_000_080).await?;
    sqlx::query(
        "insert into github_catalog.current_star_state
             (account_id, repository_id, starred, last_observed_at, observed_unstarred_at)
         values ($1, $2, false, now(), '2026-01-05T00:00:00Z')",
    )
    .bind(account_id)
    .bind(identity_x.repository_id)
    .execute(database.database.pool())
    .await?;

    // Both repos are enumerated inside list L.
    let server = MockServer::start().await;
    let first_page = lists_page(
        &[list_edge(
            "gid://UserList/9",
            "mixed list",
            false,
            &[
                item_edge(300_000_080, "acme/unstarred"),
                item_edge(300_000_081, "acme/never-starred"),
            ],
        )],
        Some("MQ"),
    );
    mount_page(&server, false, first_page).await?;
    mount_page(&server, true, lists_page(&[], None)).await?;

    let gateway = ReqwestGithubApi::for_base_url(&server.uri())?;
    let outcome = run_star_list_snapshot(
        &database.database,
        &gateway,
        &RateLimitLedger::new(),
        &TokenRef::from_label("orphan-e2e"),
        account_id,
    )
    .await?;
    assert!(
        matches!(outcome, StarListSnapshotOutcome::Completed { .. }),
        "the snapshot must complete"
    );

    let list_row: Uuid = sqlx::query_scalar(
        "select list_id from github_catalog.star_lists where provider_list_id = 'gid://UserList/9'",
    )
    .fetch_one(database.database.pool())
    .await?;
    let members: Vec<i64> = sqlx::query_as::<_, (i64,)>(
        "select r.provider_repository_id
             from github_catalog.star_list_memberships m
             join github_catalog.repositories r on r.repository_id = m.repository_id
         where m.list_id = $1 and m.member
         order by r.provider_repository_id",
    )
    .bind(list_row)
    .fetch_all(database.database.pool())
    .await?
    .into_iter()
    .map(|row| row.0)
    .collect();
    assert_eq!(
        members,
        vec![300_000_080, 300_000_081],
        "an unstarred repository is representable as a truthful member"
    );

    // X's star state is exactly what it was; Y gains no star state at all;
    // no star observations appeared for either.
    let star_state: (bool, Option<String>) = sqlx::query_as(
        "select starred, observed_unstarred_at::text
             from github_catalog.current_star_state where repository_id = $1",
    )
    .bind(identity_x.repository_id)
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        star_state,
        (false, Some("2026-01-05 00:00:00+00".to_owned())),
        "list membership must not touch star authority"
    );
    let y_star_rows: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.current_star_state c
         join github_catalog.repositories r on r.repository_id = c.repository_id
         where r.provider_repository_id = 300_000_081",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        y_star_rows, 0,
        "a never-starred member gains no star state from a list snapshot"
    );
    let star_observation_count: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.star_observations")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(
        star_observation_count, 0,
        "a list snapshot writes no star observations"
    );

    database.cleanup().await?;
    Ok(())
}
