//! Legacy `PostgreSQL` source boundary tests.

use ratatoskr_github_catalog::test_support::TestDatabase;
use ratatoskr_github_catalog::{
    LegacyImportRequest, LegacyIntegration, LegacyOwnerMap, LegacyRepository, LegacySnapshot,
    LegacySource, import_legacy_snapshot,
};

#[tokio::test]
async fn source_preflight_accepts_only_the_archived_allow_list()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    sqlx::raw_sql(
        "create table public.repositories (
             id integer primary key,
             github_id bigint not null,
             owner text not null,
             name text not null,
             user_id bigint not null,
             is_starred boolean not null,
             last_synced_at timestamptz,
             list_names jsonb not null default '[]'::jsonb
         );
         create table public.user_github_integrations (
             id integer primary key,
             user_id bigint not null,
             encrypted_token bytea,
             token_scopes text,
             github_login text,
             github_user_id bigint,
             status text not null
         );
         insert into public.repositories
             (id, github_id, owner, name, user_id, is_starred, last_synced_at, list_names)
         values
             (7, 99, 'legacy-owner', 'legacy-repository', 101, true,
              '2026-08-19T12:34:56Z', '[\"Archived List\"]'::jsonb);
         insert into public.user_github_integrations
             (id, user_id, token_scopes, github_login, github_user_id, status)
         values (1, 101, 'repo', 'legacy-login', 4242, 'active');",
    )
    .execute(database.database.pool())
    .await?;
    let source = LegacySource::from_pool(database.database.pool().clone());

    let snapshot = source.read_snapshot().await?;

    assert_eq!(snapshot.repositories.len(), 1);
    assert_eq!(snapshot.repositories[0].provider_repository_id, 99);
    assert_eq!(snapshot.repositories[0].list_names, ["Archived List"]);
    assert_eq!(snapshot.integrations.len(), 1);
    assert_eq!(snapshot.integrations[0].provider_user_id, Some(4242));
    assert_eq!(
        snapshot.integrations[0].granted_scopes.as_deref(),
        Some("repo")
    );
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn source_preflight_rejects_a_required_column_with_the_wrong_type()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    sqlx::raw_sql(
        "create table public.repositories (
             id integer primary key,
             github_id bigint not null,
             owner bigint not null,
             name text not null,
             user_id bigint not null,
             is_starred boolean not null,
             last_synced_at timestamptz,
             list_names jsonb not null default '[]'::jsonb
         );
         create table public.user_github_integrations (
             id integer primary key,
             user_id bigint not null,
             token_scopes text,
             github_login text,
             github_user_id bigint,
             status text not null
         );",
    )
    .execute(database.database.pool())
    .await?;
    let source = LegacySource::from_pool(database.database.pool().clone());

    assert!(source.preflight().await.is_err());

    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn unmapped_or_duplicate_owner_mapping_leaves_the_target_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let duplicate = r#"[
        {"legacy_user_id": 101, "owner_ref": "user:018f0000-0000-7000-8000-000000000001"},
        {"legacy_user_id": 101, "owner_ref": "user:018f0000-0000-7000-8000-000000000002"}
    ]"#;
    let valid = r#"[
        {"legacy_user_id": 101, "owner_ref": "user:018f0000-0000-7000-8000-000000000001"}
    ]"#;

    assert!(LegacyOwnerMap::from_json(duplicate).is_err());
    let owners = LegacyOwnerMap::from_json(valid)?;
    assert!(owners.owner_for(999).is_err());
    let account_count: i64 =
        sqlx::query_scalar("select count(*) from github_catalog.github_accounts")
            .fetch_one(database.database.pool())
            .await?;
    assert_eq!(account_count, 0);
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn imports_repository_star_observation_and_list_claim_without_fabricating_provider_values()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let source = LegacySource::from_pool(database.database.pool().clone());
    sqlx::raw_sql(
        "create table public.repositories (
             id integer primary key, github_id bigint not null, owner text not null, name text not null,
             user_id bigint not null, is_starred boolean not null, last_synced_at timestamptz,
             list_names jsonb not null
         );
         create table public.user_github_integrations (
             id integer primary key, user_id bigint not null, encrypted_token bytea,
             token_scopes text, github_login text, github_user_id bigint, status text not null
         );
         insert into public.repositories values
             (7, 99, 'legacy-owner', 'legacy-repository', 101, true,
              '2026-08-19T12:34:56Z', '[\"Archived List\"]'::jsonb);
         insert into public.user_github_integrations
             (id, user_id, token_scopes, github_login, github_user_id, status)
         values (1, 101, 'repo', 'legacy-login', 4242, 'active');",
    )
    .execute(database.database.pool())
    .await?;
    let snapshot = source.read_snapshot().await?;
    let owners = LegacyOwnerMap::from_json(
        r#"[{"legacy_user_id":101,"owner_ref":"user:018f0000-0000-7000-8000-000000000001"}]"#,
    )?;

    import_legacy_snapshot(
        &database.database,
        LegacyImportRequest {
            source_id: "archive-2026-08".to_owned(),
            owner_map: owners,
            snapshot,
        },
    )
    .await?;

    let imported: (i64, String, Option<time::OffsetDateTime>) = sqlx::query_as(
        "select r.provider_repository_id, a.status, c.starred_at
         from github_catalog.current_star_state c
         join github_catalog.repositories r on r.repository_id = c.repository_id
         join github_catalog.github_accounts a on a.account_id = c.account_id",
    )
    .fetch_one(database.database.pool())
    .await?;
    let claim: String = sqlx::query_scalar(
        "select list_name from github_catalog.legacy_list_claims where source_id = 'archive-2026-08'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(imported.0, 99);
    assert_eq!(imported.1, "reauthorization_required");
    assert_eq!(imported.2, None);
    assert_eq!(claim, "Archived List");
    database.cleanup().await?;
    Ok(())
}

#[test]
fn checked_in_legacy_fixture_manifest_has_no_credential_fields_or_values()
-> Result<(), Box<dyn std::error::Error>> {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/legacy-import-manifest.json"))?;
    assert_no_secret_named_fields(&manifest);
    Ok(())
}

#[tokio::test]
async fn repeating_the_same_fixture_import_is_idempotent() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::create().await?;
    let snapshot = LegacySnapshot {
        repositories: vec![LegacyRepository {
            legacy_repository_id: 7,
            provider_repository_id: 99,
            owner: "example-owner".to_owned(),
            name: "example-repository".to_owned(),
            legacy_user_id: 101,
            starred: true,
            observed_at: None,
            list_names: vec!["Archived List".to_owned()],
        }],
        integrations: vec![LegacyIntegration {
            legacy_user_id: 101,
            granted_scopes: Some("repo".to_owned()),
            login: Some("example-login".to_owned()),
            provider_user_id: Some(4242),
            status: "active".to_owned(),
        }],
    };
    let owner_map = || {
        LegacyOwnerMap::from_json(
            r#"[{"legacy_user_id":101,"owner_ref":"user:018f0000-0000-7000-8000-000000000001"}]"#,
        )
    };
    let first = import_legacy_snapshot(
        &database.database,
        LegacyImportRequest {
            source_id: "synthetic-legacy-fixture".to_owned(),
            owner_map: owner_map()?,
            snapshot: snapshot.clone(),
        },
    )
    .await?;
    let second = import_legacy_snapshot(
        &database.database,
        LegacyImportRequest {
            source_id: "synthetic-legacy-fixture".to_owned(),
            owner_map: owner_map()?,
            snapshot,
        },
    )
    .await?;
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "select
             (select count(*) from github_catalog.repositories),
             (select count(*) from github_catalog.star_observations),
             (select count(*) from github_catalog.legacy_list_claims),
             (select count(*) from github_catalog.github_account_credentials)",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(first.repositories_imported, 1);
    assert_eq!(second.repositories_imported, 0);
    assert_eq!(second.star_claims_imported, 0);
    assert_eq!(second.list_claims_imported, 0);
    assert_eq!(counts, (1, 1, 1, 0));
    database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn failed_import_leaves_only_redacted_failure_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::create().await?;
    let request = LegacyImportRequest {
        source_id: "failed-import-fixture".to_owned(),
        owner_map: LegacyOwnerMap::from_json(
            r#"[{"legacy_user_id":101,"owner_ref":"user:018f0000-0000-7000-8000-000000000001"}]"#,
        )?,
        snapshot: LegacySnapshot {
            repositories: vec![LegacyRepository {
                legacy_repository_id: 7,
                provider_repository_id: 99,
                owner: "example-owner".to_owned(),
                name: "example-repository".to_owned(),
                legacy_user_id: 101,
                starred: true,
                observed_at: None,
                list_names: vec![String::new()],
            }],
            integrations: vec![],
        },
    };

    assert!(
        import_legacy_snapshot(&database.database, request)
            .await
            .is_err()
    );

    let failure: (String, String, Option<time::OffsetDateTime>) = sqlx::query_as(
        "select status, failure_code, finished_at
         from github_catalog.legacy_import_runs
         where source_id = 'failed-import-fixture'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(failure.0, "failed");
    assert_eq!(failure.1, "conflicting_source_data");
    assert!(failure.2.is_some());
    let account_count: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.github_accounts where owner_ref = 'user:018f0000-0000-7000-8000-000000000001'",
    )
    .fetch_one(database.database.pool())
    .await?;
    assert_eq!(
        account_count, 0,
        "the failed attempt must roll target writes back"
    );

    database.cleanup().await?;
    Ok(())
}

fn assert_no_secret_named_fields(value: &serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                assert_no_secret_named_fields(value);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let normalized = key.to_ascii_lowercase();
                assert!(
                    !["token", "credential", "password", "secret", "encrypted"]
                        .iter()
                        .any(|needle| normalized.contains(needle)),
                    "fixture field name must not carry secret material"
                );
                assert_no_secret_named_fields(value);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}
