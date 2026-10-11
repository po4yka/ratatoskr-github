//! Confirmed OAuth grant revocation during account erasure (XR-021 CONTRACTS.md R2-08 e).

use std::time::Duration;

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_operation_contracts::AccountErasureOutcome;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod revocation_support;
use revocation_support::{TestResult, fixture, grant_answers, probe_answers};

#[tokio::test]
async fn revoke_204_is_verified() -> TestResult {
    let fixture = fixture().await?;
    let server = MockServer::start().await;
    grant_answers(&server, 204).await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;

    assert_eq!(
        fixture.erase(&github).await?,
        AccountErasureOutcome::Verified
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .all(|request| request.url.path() != "/user"),
        "a 204 needs no probe"
    );

    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn revoke_404_with_a_dead_token_is_verified() -> TestResult {
    let fixture = fixture().await?;
    let server = MockServer::start().await;
    grant_answers(&server, 404).await;
    probe_answers(&server, 401).await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;

    assert_eq!(
        fixture.erase(&github).await?,
        AccountErasureOutcome::Verified
    );

    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn revoke_404_with_a_live_token_is_incomplete() -> TestResult {
    let fixture = fixture().await?;
    let server = MockServer::start().await;
    grant_answers(&server, 404).await;
    probe_answers(&server, 200).await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;

    assert_eq!(
        fixture.erase(&github).await?,
        AccountErasureOutcome::IncompleteExternalGrantRevocation
    );

    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn revoke_404_with_a_probe_transport_failure_is_incomplete() -> TestResult {
    let fixture = fixture().await?;
    let server = MockServer::start().await;
    grant_answers(&server, 404).await;
    // The probe path is not mounted for a transport failure: the stalled answer exceeds the bound.
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_secs(4)))
        .mount(&server)
        .await;
    let github =
        ReqwestGithubApi::for_base_url(&server.uri())?.with_timeout(Duration::from_millis(400))?;

    assert_eq!(
        fixture.erase(&github).await?,
        AccountErasureOutcome::IncompleteExternalGrantRevocation
    );

    fixture.database.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn revoke_422_is_incomplete() -> TestResult {
    let fixture = fixture().await?;
    let server = MockServer::start().await;
    grant_answers(&server, 422).await;
    let github = ReqwestGithubApi::for_base_url(&server.uri())?;

    assert_eq!(
        fixture.erase(&github).await?,
        AccountErasureOutcome::IncompleteExternalGrantRevocation
    );

    fixture.database.cleanup().await?;
    Ok(())
}
