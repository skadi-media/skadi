//! Domain enable/disable endpoint tests (SKADI-T-0054).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config, DomainDescriptor};
use skadi_core::MediaKind;
use skadi_testsupport::TestDb;

/// Build an `AppState` over an isolated, migrated control-plane database
/// (Postgres-default via `SKADI_TEST_DATABASE_URL`, SQLite tempfile otherwise).
/// The returned `TestDb` must be kept alive for the test's duration — dropping
/// it tears the database down.
async fn state() -> (Arc<AppState>, TestDb) {
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let state = AppState::new_with_domains(
        config,
        Some(db.store.clone()),
        vec![
            DomainDescriptor {
                name: "movies".into(),
                kind: MediaKind::Movie,
            },
            DomainDescriptor {
                name: "music".into(),
                kind: MediaKind::Music,
            },
        ],
    );
    (state, db)
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = skadi_api::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

#[tokio::test]
async fn lists_compiled_in_domains_disabled_by_default() {
    let (state, _db) = state().await;
    let (s, body) = call(&state, "GET", "/api/v1/domains", None).await;
    assert_eq!(s, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|d| d["enabled"] == false));
    assert!(
        arr.iter()
            .any(|d| d["name"] == "movies" && d["kind"] == "movie")
    );
}

#[tokio::test]
async fn enable_then_reflect_in_list() {
    let (state, _db) = state().await;
    let (s, dto) = call(
        &state,
        "PUT",
        "/api/v1/domains/movies",
        Some(serde_json::json!({ "enabled": true })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(dto["name"], "movies");
    assert_eq!(dto["enabled"], true);

    let (_, body) = call(&state, "GET", "/api/v1/domains", None).await;
    let movies = body
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "movies")
        .unwrap();
    assert_eq!(movies["enabled"], true);
}

#[tokio::test]
async fn unknown_domain_is_404() {
    let (state, _db) = state().await;
    let (s, _) = call(
        &state,
        "PUT",
        "/api/v1/domains/bogus",
        Some(serde_json::json!({ "enabled": true })),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
