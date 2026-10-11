//! A stalled provider cannot hang account erasure (XR-021 CONTRACTS.md R2-08 d).

use std::time::{Duration, Instant};

use ratatoskr_github_catalog::provider::ReqwestGithubApi;
use ratatoskr_operation_contracts::AccountErasureOutcome;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod revocation_support;
use revocation_support::{TestResult, fixture};

#[tokio::test]
async fn erasure_with_a_stalled_revocation_is_bounded_and_incomplete() -> TestResult {
    let fixture = fixture().await?;
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.configured-app/grant"))
        .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(4)))
        .mount(&server)
        .await;
    let github =
        ReqwestGithubApi::for_base_url(&server.uri())?.with_timeout(Duration::from_millis(400))?;

    let started = Instant::now();
    let outcome = fixture.erase(&github).await?;

    assert!(
        started.elapsed() < Duration::from_secs(3),
        "erasure waited for the stalled provider: {:?}",
        started.elapsed()
    );
    assert_eq!(
        outcome,
        AccountErasureOutcome::IncompleteExternalGrantRevocation,
        "an unconfirmed revocation stays visible; local erasure still completes"
    );
    assert_eq!(fixture.accounts_left().await?, 0);

    fixture.database.cleanup().await?;
    Ok(())
}
