//! Test-connection endpoint tests (SKADI-T-0061).
//!
//! `POST /settings/{kind}/{id}/test` builds the stored provider and runs its
//! `test()`. Verified against a wiremock fake-Torznab: a live endpoint with the
//! right key → `{ok:true}`; a dead URL → `{ok:false, error}`; missing setting →
//! 404; non-provider kind → 404.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wiremock::matchers::{method, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use skadi_api::{AppState, Config};
use skadi_testsupport::TestDb;

const CAPS_XML: &str = r#"<?xml version="1.0"?>
<caps>
  <searching>
    <search available="yes" supportedParams="q" />
    <movie-search available="yes" supportedParams="q,imdbid,tmdbid" />
  </searching>
  <categories>
    <category id="2000" name="Movies" />
  </categories>
</caps>"#;

async fn state_with_store() -> (Arc<AppState>, TestDb) {
    // Postgres-default isolated DB (SQLite fallback). Caller keeps the `TestDb`
    // guard alive for the test's duration (SKADI-T-0077).
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    (AppState::new(config, Some(db.store.clone())), db)
}

async fn call(
    state: &Arc<AppState>,
    method_s: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let builder = Request::builder().method(method_s).uri(uri);
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
async fn torznab_test_passes_against_live_fake_and_fails_against_dead_url() {
    let (state, _db) = state_with_store().await;

    // A fake Torznab that answers t=caps.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("t", "caps"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CAPS_XML))
        .mount(&server)
        .await;

    // Create the indexer setting pointing at the fake (secret sealed via T-0059).
    let (s, created) = call(
        &state,
        "POST",
        "/api/v1/settings/indexers",
        Some(serde_json::json!({
            "kind": "torznab",
            "name": "fake",
            "base_url": server.uri(),
            "categories": [2000],
            "api_key": "k"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_string();

    // Live endpoint → ok:true.
    let (s, body) = call(
        &state,
        "POST",
        &format!("/api/v1/settings/indexers/{id}/test"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["ok"], true, "expected ok:true, got {body}");

    // Re-point the setting at a dead URL → ok:false with an error message.
    call(
        &state,
        "PUT",
        &format!("/api/v1/settings/indexers/{id}"),
        Some(serde_json::json!({
            "kind": "torznab",
            "name": "fake",
            "base_url": "http://127.0.0.1:1",
            "categories": [2000]
        })),
    )
    .await;
    let (s, body) = call(
        &state,
        "POST",
        &format!("/api/v1/settings/indexers/{id}/test"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "test ran (failure is in the body)");
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "expected an error message: {body}"
    );
}

#[tokio::test]
async fn missing_setting_and_non_provider_kind_are_404() {
    let (state, _db) = state_with_store().await;
    let (s, _) = call(
        &state,
        "POST",
        &format!("/api/v1/settings/indexers/{}/test", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // profiles is a settings kind but not a testable provider.
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/settings/profiles",
        Some(serde_json::json!({ "name": "p" })),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (s, _) = call(
        &state,
        "POST",
        &format!("/api/v1/settings/profiles/{id}/test"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// SKADI-T-0501: the test button delivers a real event now, so a notifier whose
/// receiver cannot be reached must report failure. It used to return `ok: true`
/// without contacting anything, which made the button useless at the one moment
/// an operator relies on it.
#[tokio::test]
async fn webhook_test_reports_failure_when_the_receiver_cannot_be_reached() {
    let (state, _db) = state_with_store().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/settings/notifiers",
        Some(serde_json::json!({
            "kind": "webhook",
            "name": "hook",
            // Nothing listens here: 127.0.0.1:1 refuses immediately, which keeps
            // the test fast and offline.
            "url": "http://127.0.0.1:1/hook",
            "channels": ["imported"]
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (s, body) = call(
        &state,
        "POST",
        &format!("/api/v1/settings/notifiers/{id}/test"),
        None,
    )
    .await;
    // The endpoint itself succeeds; the *result* it reports is the failure.
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["ok"], false, "body: {body}");
    assert!(
        body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "the failure should say what went wrong: {body}"
    );
}
