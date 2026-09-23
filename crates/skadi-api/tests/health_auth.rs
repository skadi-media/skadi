//! Handler + middleware tests for the skadi-api skeleton (SKADI-T-0050).
//!
//! Exercises the assembled router with `tower::ServiceExt::oneshot`: the health
//! endpoint, and the bearer-auth middleware across no-token / wrong-token /
//! right-token / open-mode.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config, bearer_auth};

fn config(token: Option<&str>) -> Config {
    Config {
        database_url: "sqlite://:memory:".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: token.map(|t| t.to_string()),
    }
}

/// A router mirroring `skadi_api::router` but with an extra protected `/api/v1/protected`
/// route returning 200, so we can prove the middleware lets authorized requests
/// through to a real handler. (The production `router()` only has `/health` so
/// far; later tasks add protected routes.)
fn test_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/health", get(skadi_api::health::health))
        .route("/protected", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            bearer_auth,
        ))
        .with_state(state);
    Router::new().nest("/api/v1", api)
}

async fn get_status(router: Router, uri: &str, token: Option<&str>) -> StatusCode {
    let mut req = Request::builder().uri(uri);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let res = router
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    res.status()
}

#[tokio::test]
async fn health_returns_ok_body() {
    let state = AppState::new(config(Some("secret")), None);
    let res = skadi_api::router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn health_is_reachable_without_a_token() {
    let state = AppState::new(config(Some("secret")), None);
    let status = get_status(test_router(state), "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn protected_route_rejects_missing_token() {
    let state = AppState::new(config(Some("secret")), None);
    let status = get_status(test_router(state), "/api/v1/protected", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn protected_route_rejects_wrong_token() {
    let state = AppState::new(config(Some("secret")), None);
    let status = get_status(test_router(state), "/api/v1/protected", Some("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn protected_route_accepts_right_token() {
    let state = AppState::new(config(Some("secret")), None);
    let status = get_status(test_router(state), "/api/v1/protected", Some("secret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn open_mode_lets_requests_through() {
    let state = AppState::new(config(None), None);
    let status = get_status(test_router(state), "/api/v1/protected", None).await;
    assert_eq!(status, StatusCode::OK);
}
