//! A `track` action originates the first repository-analysis request, with real README evidence
//! (XR-021 CONTRACTS.md R2-08 a and b).

mod action_support;
use action_support::{FIRST_USER, Harness, README_TEXT, SECOND_USER, TestResult, action_request};
use sha2::{Digest as _, Sha256};

const TRACK_KEY: &str = "track-origination.018f0000-0000-7000-8000-000000000b01";
const OTHER_TRACK_KEY: &str = "track-origination.018f0000-0000-7000-8000-000000000b02";

#[tokio::test]
async fn track_queues_one_analysis_request_with_the_readme() -> TestResult {
    let harness = Harness::start(Some(README_TEXT)).await?;
    let pool = harness.database.database.pool();

    let (status, first) = harness
        .act(FIRST_USER, &action_request("track", TRACK_KEY))
        .await?;
    assert_eq!(status, 200, "track answer: {first}");
    let owners: Vec<String> = sqlx::query_scalar(
        "select owner_ref from github_catalog.repository_analysis_requests order by created_at",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(owners, vec![format!("user:{FIRST_USER}")]);

    let (state, digest, length): (String, String, i64) = sqlx::query_as(
        "select source_revision #>> '{readme,state}',
                source_revision #>> '{readme,content_ref,digest,hex}',
                (source_revision #>> '{readme,content_ref,length_bytes}')::bigint
         from github_catalog.repository_analysis_requests",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(state, "present");
    let expected = format!("{:x}", Sha256::digest(README_TEXT.as_bytes()));
    assert_eq!(
        digest, expected,
        "the request names the fetched README bytes"
    );
    assert_eq!(length, i64::try_from(README_TEXT.len())?);
    let stored: Vec<u8> = sqlx::query_scalar(
        "select bytes from github_catalog.repository_readme_blobs where content_digest = $1",
    )
    .bind(&digest)
    .fetch_one(pool)
    .await?;
    assert_eq!(stored, README_TEXT.as_bytes(), "the BlobRef resolves");

    let (_status, replay) = harness
        .act(FIRST_USER, &action_request("track", TRACK_KEY))
        .await?;
    assert_eq!(replay, first, "an exact replay returns the recorded answer");
    assert_eq!(harness.count("repository_analysis_requests").await?, 1);
    assert_eq!(harness.count("repository_watches").await?, 1);

    let (status, second) = harness
        .act(SECOND_USER, &action_request("track", OTHER_TRACK_KEY))
        .await?;
    assert_eq!(status, 200, "second track answer: {second}");
    assert_eq!(harness.count("repository_analysis_requests").await?, 2);
    assert_eq!(harness.count("repository_watches").await?, 2);
    let first_user_watches: i64 = sqlx::query_scalar(
        "select count(*) from github_catalog.repository_watches where owner_ref = $1",
    )
    .bind(format!("user:{FIRST_USER}"))
    .fetch_one(pool)
    .await?;
    assert_eq!(
        first_user_watches, 1,
        "the first user keeps exactly one watch"
    );

    harness.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn a_repository_without_a_readme_is_requested_with_the_real_reason() -> TestResult {
    let harness = Harness::start(None).await?;

    let (status, answer) = harness
        .act(FIRST_USER, &action_request("track", TRACK_KEY))
        .await?;
    assert_eq!(status, 200, "track answer: {answer}");

    let (state, reason): (String, Option<String>) = sqlx::query_as(
        "select source_revision #>> '{readme,state}', source_revision #>> '{readme,reason}'
         from github_catalog.repository_analysis_requests",
    )
    .fetch_one(harness.database.database.pool())
    .await?;
    assert_eq!(state, "absent");
    assert_eq!(reason.as_deref(), Some("not_found"));

    harness.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn the_metadata_action_registers_no_watch() -> TestResult {
    let harness = Harness::start(Some(README_TEXT)).await?;

    let (status, answer) = harness
        .act(
            FIRST_USER,
            &action_request(
                "metadata",
                "metadata-origination.018f0000-0000-7000-8000-000000000b03",
            ),
        )
        .await?;
    assert_eq!(status, 200, "metadata answer: {answer}");

    assert_eq!(harness.count("repository_watches").await?, 0);
    assert_eq!(harness.count("repository_analysis_requests").await?, 0);

    harness.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn the_star_action_registers_no_watch() -> TestResult {
    let harness = Harness::start_with_account(Some(README_TEXT)).await?;

    let (status, answer) = harness
        .act(
            FIRST_USER,
            &action_support::star_request("star-origination.018f0000-0000-7000-8000-000000000b04"),
        )
        .await?;
    assert_eq!(status, 200, "star answer: {answer}");
    assert_eq!(answer["provider_star"]["status"], "succeeded");

    assert_eq!(harness.count("repository_watches").await?, 0);
    assert_eq!(harness.count("repository_analysis_requests").await?, 0);

    harness.database.cleanup().await?;
    Ok(())
}
