//! Static web-UI serving + SPA fallback tests (SKADI-T-0065).
//!
//! These run against the production [`skadi_api::router`], which mounts the
//! assets fallback (see `crate::assets`). They assert the fallback never
//! shadows the API and that unknown client-side routes get a UI document — for
//! both feature configurations:
//!
//!   * default (no `embed-ui`): the built-in placeholder is served;
//!   * `--features embed-ui`: the embedded `index.html` shell is served.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config};

fn config() -> Config {
    Config {
        database_url: "sqlite://:memory:".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        // Open mode — matches the UI's no-token assumption.
        bearer_token: None,
    }
}

async fn get(uri: &str) -> (StatusCode, String, Option<String>) {
    let state = AppState::new(config(), None);
    let res = skadi_api::router(state)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let ctype = res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body =
        String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    (status, body, ctype)
}

/// The API route must win over the static fallback.
#[tokio::test]
async fn api_health_is_not_shadowed_by_fallback() {
    let (status, body, _) = get("/api/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

/// A nonexistent API path is an honest 404, NOT the SPA shell.
#[tokio::test]
async fn unknown_api_path_is_404_not_spa() {
    let (status, body, _) = get("/api/v1/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !body.contains("<html") && !body.to_lowercase().contains("<!doctype"),
        "API 404 must not return an HTML document, got: {body}"
    );
}

/// The root path serves an HTML document (placeholder or embedded shell).
#[tokio::test]
async fn root_serves_html() {
    let (status, _body, ctype) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ctype.as_deref(), Some("text/html"));
}

/// An unknown non-API path is the SPA fallback: it serves the HTML shell rather
/// than 404, so client-side routing can resolve it.
#[tokio::test]
async fn unknown_client_route_serves_html_shell() {
    let (status, _body, ctype) = get("/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ctype.as_deref(), Some("text/html"));
}

/// Token injection (SKADI-T-0068): the served `index.html` carries the
/// configured bearer token in its meta tag, and never the raw placeholder, so
/// the bundled UI authenticates with no manual setup. Only meaningful when the
/// real bundle is embedded.
#[cfg(feature = "embed-ui")]
#[tokio::test]
async fn index_html_has_token_injected() {
    let cfg = Config {
        database_url: "sqlite://:memory:".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: Some("s3cr3t-token".into()),
    };
    let state = AppState::new(cfg, None);
    let res = skadi_api::router(state)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body =
        String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(body.contains("s3cr3t-token"), "token should be injected");
    assert!(
        !body.contains("__SKADI_API_TOKEN__"),
        "placeholder must be replaced"
    );
}

/// In open mode the placeholder is replaced with an empty string (UI treats that
/// as "no token") — the raw placeholder never leaks to the client.
#[cfg(feature = "embed-ui")]
#[tokio::test]
async fn index_html_open_mode_has_no_placeholder() {
    let (_status, body, _) = get("/").await;
    assert!(
        !body.contains("__SKADI_API_TOKEN__"),
        "placeholder must be replaced even in open mode"
    );
}
