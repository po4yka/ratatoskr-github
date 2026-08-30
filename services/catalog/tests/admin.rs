//! Operator-plane state tests.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use ratatoskr_github_catalog_service::{Lifecycle, admin_router};
use tower::ServiceExt as _;

#[tokio::test]
async fn readiness_follows_startup_and_drain() -> Result<(), Box<dyn std::error::Error>> {
    let lifecycle = Lifecycle::starting();
    let app = admin_router(lifecycle.clone());

    assert_response(&app, "/live", StatusCode::OK).await?;
    assert_response(&app, "/ready", StatusCode::SERVICE_UNAVAILABLE).await?;
    lifecycle.mark_ready();
    assert_response(&app, "/ready", StatusCode::OK).await?;
    assert_response(&app, "/metrics", StatusCode::OK).await?;
    assert_response(&app, "/version", StatusCode::OK).await?;
    lifecycle.begin_drain();
    assert_response(&app, "/ready", StatusCode::SERVICE_UNAVAILABLE).await?;
    assert_response(&app, "/live", StatusCode::OK).await?;
    Ok(())
}

#[tokio::test]
async fn listener_only_bus_loss_and_worker_exit_are_never_reported_ready()
-> Result<(), Box<dyn std::error::Error>> {
    let lifecycle = Lifecycle::starting();
    let app = admin_router(lifecycle.clone());
    lifecycle.mark_database_ready();
    lifecycle.mark_serving();
    lifecycle.set_live_workers(7);
    assert_response(&app, "/ready", StatusCode::SERVICE_UNAVAILABLE).await?;
    lifecycle.set_bus_ready(true);
    lifecycle.set_topology_ready(true);
    assert_response(&app, "/ready", StatusCode::OK).await?;
    lifecycle.set_bus_ready(false);
    assert_response(&app, "/ready", StatusCode::SERVICE_UNAVAILABLE).await?;
    lifecycle.set_bus_ready(true);
    lifecycle.set_live_workers(6);
    assert_response(&app, "/ready", StatusCode::SERVICE_UNAVAILABLE).await?;
    lifecycle.record_dead_letter();
    lifecycle.set_live_workers(7);
    assert_response(&app, "/ready", StatusCode::OK).await?;
    Ok(())
}

async fn assert_response(
    app: &axum::Router,
    path: &str,
    expected: StatusCode,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;

    assert_eq!(
        response.status(),
        expected,
        "{path} returned an unexpected status"
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&header::HeaderValue::from_static("no-store"))
    );
    let _body = response.into_body().collect().await?;
    Ok(())
}
