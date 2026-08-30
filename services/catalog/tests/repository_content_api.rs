//! Service-authenticated immutable README resolution through the real Catalog process.

use std::net::TcpListener;
use std::process::Stdio;

use ratatoskr_github_catalog::provider::ProviderRepositoryBody;
use ratatoskr_github_catalog::{
    RepositoryAnalysisSource, apply_fresh_source, store_readme, upsert_repository,
};
use ratatoskr_github_contracts::ReadmeRevision;
use ratatoskr_identifiers::TenantRef;

mod support;

use support::{configured_command, http_service_json, stop_process, test_database_url, wait_ready};

const AUTHORIZED_OWNER: &str = "user:018f0000-0000-7000-8000-000000000701";
const FOREIGN_OWNER: &str = "user:018f0000-0000-7000-8000-000000000702";
const SERVICE_TOKEN: &str = "synthetic-knowledge-service-token-with-adequate-entropy";
const RESOLVE_ROUTE: &str = "/internal/v1/repository-readmes/resolve";
const MAX_README_BYTES: usize = 1_048_576;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one process scenario proves the complete authorization and integrity boundary"
)]
async fn knowledge_reads_only_authorized_digest_verified_readme()
-> Result<(), Box<dyn std::error::Error>> {
    let database = ratatoskr_github_catalog::test_support::TestDatabase::create().await?;
    let database_name: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.database.pool())
        .await?;
    let database_url = test_database_url(&database_name)?;
    let repository = upsert_repository(&database.database, 300_000_701).await?;
    let readme_bytes = b"# Immutable evidence\n";
    let readme = store_readme(&database.database, readme_bytes).await?;
    let owner = TenantRef::parse(AUTHORIZED_OWNER)?;
    let source = RepositoryAnalysisSource {
        owner,
        readme: ReadmeRevision::Present {
            content_ref: readme.clone(),
        },
        readme_etag: Some(r#"W/"readme-v1""#.to_owned()),
    };
    apply_fresh_source(
        &database.database,
        repository.repository_id,
        &repository_body(300_000_701, "acme/immutable"),
        None,
        &source,
    )
    .await?;

    let reserved_admin = TcpListener::bind("127.0.0.1:0")?;
    let admin_address = reserved_admin.local_addr()?;
    let reserved_api = TcpListener::bind("127.0.0.1:0")?;
    let api_address = reserved_api.local_addr()?;
    let reserved_internal = TcpListener::bind("0.0.0.0:0")?;
    let internal_bind_address = reserved_internal.local_addr()?;
    let internal_address =
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, internal_bind_address.port()));
    drop(reserved_admin);
    drop(reserved_api);
    drop(reserved_internal);
    let token_file = std::env::temp_dir().join(format!(
        "ratatoskr-github-knowledge-token-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::write(&token_file, format!("{SERVICE_TOKEN}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600))?;
    }
    let loaded_token =
        ratatoskr_github_catalog_service::ServiceBearerToken::from_file(&token_file)?;
    assert!(
        !format!("{loaded_token:?}").contains(SERVICE_TOKEN),
        "service credential escaped through Debug"
    );
    let mut command = configured_command(
        admin_address,
        api_address,
        &database_url,
        "https://api.github.com",
    );
    command.env("RATATOSKR__SERVICE_AUTH__KNOWLEDGE_TOKEN_FILE", &token_file);
    command.env(
        "RATATOSKR__INTERNAL_API__LISTEN_ADDRESS",
        internal_bind_address.to_string(),
    );
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_ready(&mut child, admin_address)?;

    let request = serde_json::json!({
        "owner": AUTHORIZED_OWNER,
        "repository_id": repository.repository_id,
        "content_ref": readme,
    });
    let edge_route = http_service_json(api_address, RESOLVE_ROUTE, Some(SERVICE_TOKEN), &request)?;
    assert_eq!(edge_route.status, 404, "resolver leaked onto the Edge API");
    let authorized = http_service_json(
        internal_address,
        RESOLVE_ROUTE,
        Some(SERVICE_TOKEN),
        &request,
    )?;
    assert_eq!(authorized.status, 200, "authorized: {}", authorized.body);
    assert_eq!(authorized.body.as_bytes(), readme_bytes);
    assert!(
        authorized
            .headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("content-type: text/markdown")),
        "the immutable media type was not returned: {}",
        authorized.headers
    );

    let missing_auth = http_service_json(internal_address, RESOLVE_ROUTE, None, &request)?;
    let wrong_auth = http_service_json(
        internal_address,
        RESOLVE_ROUTE,
        Some("wrong-service-token"),
        &request,
    )?;
    assert_eq!(missing_auth.status, 401);
    assert_eq!(wrong_auth.status, 403);

    let foreign_owner = request_with(&request, |value| {
        value["owner"] = serde_json::json!(FOREIGN_OWNER);
    });
    let foreign_repository = request_with(&request, |value| {
        value["repository_id"] = serde_json::json!(uuid::Uuid::now_v7());
    });
    let arbitrary_url = request_with(&request, |value| {
        value["url"] = serde_json::json!("file:///etc/passwd");
    });
    let wrong_digest = request_with(&request, |value| {
        value["content_ref"]["digest"]["hex"] = serde_json::json!("0".repeat(64));
    });
    let wrong_media = request_with(&request, |value| {
        value["content_ref"]["media_type"] = serde_json::json!("text/plain");
    });
    let wrong_length = request_with(&request, |value| {
        value["content_ref"]["length_bytes"] = serde_json::json!(readme_bytes.len() + 1);
    });
    for denied in [
        foreign_owner,
        foreign_repository,
        arbitrary_url,
        wrong_digest,
        wrong_media,
        wrong_length,
    ] {
        let response = http_service_json(
            internal_address,
            RESOLVE_ROUTE,
            Some(SERVICE_TOKEN),
            &denied,
        )?;
        assert!(
            matches!(response.status, 400 | 404),
            "unscoped or inexact lookup escaped: {} {}",
            response.status,
            response.body
        );
        assert!(!response.body.contains("Immutable evidence"));
    }

    let oversized_bytes = vec![b'x'; MAX_README_BYTES + 1];
    let oversized = store_readme(&database.database, &oversized_bytes).await?;
    let oversized_source = RepositoryAnalysisSource {
        owner,
        readme: ReadmeRevision::Present {
            content_ref: oversized.clone(),
        },
        readme_etag: Some(r#"W/"readme-oversized""#.to_owned()),
    };
    apply_fresh_source(
        &database.database,
        repository.repository_id,
        &repository_body(300_000_701, "acme/immutable"),
        None,
        &oversized_source,
    )
    .await?;
    let oversized_request = serde_json::json!({
        "owner": AUTHORIZED_OWNER,
        "repository_id": repository.repository_id,
        "content_ref": oversized,
    });
    let oversized_response = http_service_json(
        internal_address,
        RESOLVE_ROUTE,
        Some(SERVICE_TOKEN),
        &oversized_request,
    )?;
    assert_eq!(oversized_response.status, 413);
    assert!(!oversized_response.body.contains(&"x".repeat(128)));

    sqlx::query("delete from github_catalog.repository_readme_blobs where content_digest = $1")
        .bind(
            request["content_ref"]["digest"]["hex"]
                .as_str()
                .ok_or("digest is not text")?,
        )
        .execute(database.database.pool())
        .await?;
    let missing = http_service_json(
        internal_address,
        RESOLVE_ROUTE,
        Some(SERVICE_TOKEN),
        &request,
    )?;
    assert_eq!(missing.status, 404);

    sqlx::query(
        "insert into github_catalog.repository_readme_blobs
             (content_digest, bytes, media_type, length_bytes)
         values ($1, $2, 'text/markdown', $3)",
    )
    .bind(
        request["content_ref"]["digest"]["hex"]
            .as_str()
            .ok_or("digest is not text")?,
    )
    .bind(vec![b'!'; readme_bytes.len()])
    .bind(i64::try_from(readme_bytes.len())?)
    .execute(database.database.pool())
    .await?;
    let corrupt = http_service_json(
        internal_address,
        RESOLVE_ROUTE,
        Some(SERVICE_TOKEN),
        &request,
    )?;
    stop_process(&mut child)?;
    std::fs::remove_file(token_file)?;
    database.cleanup().await?;

    assert_eq!(corrupt.status, 409);
    assert!(!corrupt.body.contains(&"!".repeat(readme_bytes.len())));
    Ok(())
}

fn repository_body(provider_repository_id: i64, full_name: &str) -> ProviderRepositoryBody {
    ProviderRepositoryBody {
        provider_repository_id,
        full_name: full_name.to_owned(),
        description: Some("Synthetic immutable source".to_owned()),
        language: Some("Rust".to_owned()),
        stargazers: 1,
        topics: Vec::new(),
        default_branch: Some("main".to_owned()),
        pushed_at: Some("2026-08-30T00:00:00Z".to_owned()),
    }
}

fn request_with(
    request: &serde_json::Value,
    update: impl FnOnce(&mut serde_json::Value),
) -> serde_json::Value {
    let mut changed = request.clone();
    update(&mut changed);
    changed
}
