//! Settings CRUD endpoint tests (SKADI-T-0053).
//!
//! Drives the assembled router (open mode, so no token needed) through the full
//! lifecycle for a couple of kinds, plus the 404 paths. All five kinds share one
//! generic handler, so exercising two of them covers the logic.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config};
use skadi_testsupport::TestDb;

async fn state_with_store() -> (Arc<AppState>, TestDb) {
    // Postgres-default isolated DB (SQLite fallback). Caller keeps the `TestDb`
    // guard alive for the test's duration (SKADI-T-0077).
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None, // open mode → no auth needed in tests
    };
    (AppState::new(config, Some(db.store.clone())), db)
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
async fn full_crud_lifecycle_for_indexers() {
    let (state, _db) = state_with_store().await;

    // Empty list.
    let (s, body) = call(&state, "GET", "/api/v1/settings/indexers", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);

    // Create. The secret (`api_key`) is stripped from the stored body and
    // sealed in the credential store (SKADI-T-0059).
    let doc = serde_json::json!({ "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "abc" });
    let stored = serde_json::json!({ "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000] });
    let (s, created) = call(
        &state,
        "POST",
        "/api/v1/settings/indexers",
        Some(doc.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["body"], stored, "secret stripped from body");
    assert_eq!(created["has_secret"], true);

    // Fetch by id — still no secret in the response.
    let (s, fetched) = call(
        &state,
        "GET",
        &format!("/api/v1/settings/indexers/{id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(fetched["body"], stored);
    assert_eq!(fetched["has_secret"], true);

    // Update with a new secret value.
    let doc2 = serde_json::json!({ "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "xyz" });
    let (s, updated) = call(
        &state,
        "PUT",
        &format!("/api/v1/settings/indexers/{id}"),
        Some(doc2.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(updated["body"], stored);
    assert_eq!(updated["has_secret"], true);

    // List now has one.
    let (s, body) = call(&state, "GET", "/api/v1/settings/indexers", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Delete → 204, then 404.
    let (s, _) = call(
        &state,
        "DELETE",
        &format!("/api/v1/settings/indexers/{id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &state,
        "DELETE",
        &format!("/api/v1/settings/indexers/{id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn kinds_are_isolated() {
    let (state, _db) = state_with_store().await;
    call(
        &state,
        "POST",
        "/api/v1/settings/downloaders",
        Some(serde_json::json!({ "kind": "skadi", "name": "built-in" })),
    )
    .await;
    // The indexers list is unaffected by a downloaders row.
    let (s, body) = call(&state, "GET", "/api/v1/settings/indexers", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
    let (s, body) = call(&state, "GET", "/api/v1/settings/downloaders", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn unknown_kind_is_404() {
    let (state, _db) = state_with_store().await;
    let (s, _) = call(&state, "GET", "/api/v1/settings/bogus", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fetch_and_update_missing_id_is_404() {
    let (state, _db) = state_with_store().await;
    let (s, _) = call(&state, "GET", "/api/v1/settings/profiles/nope", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &state,
        "PUT",
        "/api/v1/settings/profiles/nope",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// --- secret handling (SKADI-T-0059) ---

use skadi_store::CredentialRepo;

#[tokio::test]
async fn secret_lifecycle_create_update_delete() {
    let (state, _db) = state_with_store().await;
    let store = state.store.clone().unwrap();

    // Create with secret → credential present, body stripped.
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/settings/downloaders",
        Some(serde_json::json!({ "kind": "skadi", "name": "built-in", "password": "hunter2" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["has_secret"], true);
    assert_eq!(
        store
            .get_secret("downloaders", &id)
            .await
            .unwrap()
            .as_deref(),
        Some("hunter2")
    );

    // Update WITHOUT the secret field → credential untouched.
    let (s, updated) = call(
        &state,
        "PUT",
        &format!("/api/v1/settings/downloaders/{id}"),
        Some(serde_json::json!({ "kind": "skadi", "name": "renamed" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(updated["has_secret"], true);
    assert_eq!(
        store
            .get_secret("downloaders", &id)
            .await
            .unwrap()
            .as_deref(),
        Some("hunter2"),
        "absent secret field keeps the stored credential"
    );

    // Update WITH a new secret → credential replaced.
    call(
        &state,
        "PUT",
        &format!("/api/v1/settings/downloaders/{id}"),
        Some(serde_json::json!({ "kind": "skadi", "name": "renamed", "password": "newpw" })),
    )
    .await;
    assert_eq!(
        store
            .get_secret("downloaders", &id)
            .await
            .unwrap()
            .as_deref(),
        Some("newpw")
    );

    // Empty-string secret → 400 Validation, credential unchanged.
    let (s, _) = call(
        &state,
        "PUT",
        &format!("/api/v1/settings/downloaders/{id}"),
        Some(serde_json::json!({ "kind": "skadi", "name": "built-in", "password": "" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        store
            .get_secret("downloaders", &id)
            .await
            .unwrap()
            .as_deref(),
        Some("newpw")
    );

    // Delete → credential cascades.
    let (s, _) = call(
        &state,
        "DELETE",
        &format!("/api/v1/settings/downloaders/{id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(
        store
            .get_secret("downloaders", &id)
            .await
            .unwrap()
            .is_none(),
        "credential removed with the setting"
    );
}

#[tokio::test]
async fn non_secret_kinds_have_no_marker() {
    let (state, _db) = state_with_store().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/settings/profiles",
        Some(serde_json::json!({ "name": "default" })),
    )
    .await;
    assert!(
        created.get("has_secret").is_none(),
        "profiles carry no has_secret marker: {created}"
    );
}

#[tokio::test]
async fn create_without_secret_reports_has_secret_false() {
    let (state, _db) = state_with_store().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/settings/indexers",
        Some(serde_json::json!({ "kind": "torznab", "name": "open-indexer", "base_url": "http://127.0.0.1:1", "categories": [2000] })),
    )
    .await;
    assert_eq!(created["has_secret"], false);
}
