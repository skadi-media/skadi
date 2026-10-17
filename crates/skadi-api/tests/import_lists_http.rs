//! The import-list HTTP surface the list editor uses (SKADI-T-0733): a form
//! body creates a list, an edit keeps the sync history, and an exclusion needs
//! only the item.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use skadi_api::{AppState, Config};
use skadi_store::ImportListRepo;
use skadi_testsupport::TestDb;

async fn state() -> (Arc<AppState>, TestDb) {
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let state = AppState::new_full(config, Some(db.store.clone()), Vec::new(), Vec::new());
    (state, db)
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let req = match body {
        Some(b) => req.body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = skadi_api::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_form_body_creates_a_list_with_safe_defaults() {
    let (state, _db) = state().await;
    let (s, created) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({
            "name": "Bond",
            "kind": "tmdb_collection",
            "target_domain": "movie",
            "settings": { "list_id": "645" },
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert!(!created["id"].as_str().unwrap().is_empty());
    assert_eq!(created["enabled"], true);
    assert_eq!(created["add_monitored"], false);
    assert_eq!(created["interval_minutes"], 720);
    assert!(created["last_synced_at"].is_null());

    let (_, all) = call(&state, "GET", "/api/v1/importlists", None).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["settings"]["list_id"], "645");
}

#[tokio::test]
async fn a_list_without_a_name_is_refused() {
    let (state, _db) = state().await;
    let (s, body) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({ "name": "  ", "kind": "tmdb_list", "target_domain": "movie" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn an_edit_keeps_the_sync_history() {
    let (state, db) = state().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({ "name": "Bond", "kind": "tmdb_collection", "target_domain": "movie" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let synced = chrono::Utc::now();
    db.store
        .record_sync(&id, synced, Some("TMDB said no"))
        .await
        .unwrap();

    // The form sends what it edits, not the history.
    let (s, edited) = call(
        &state,
        "PUT",
        &format!("/api/v1/importlists/{id}"),
        Some(json!({
            "name": "007",
            "kind": "tmdb_collection",
            "target_domain": "movie",
            "add_monitored": true,
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{edited}");
    let stored = db.store.get_import_list(&id).await.unwrap().unwrap();
    assert_eq!(stored.name, "007");
    assert!(stored.add_monitored);
    assert_eq!(stored.last_error.as_deref(), Some("TMDB said no"));
    assert!(stored.last_synced_at.is_some());
    assert_eq!(
        stored.created_at.timestamp(),
        chrono::DateTime::parse_from_rfc3339(created["created_at"].as_str().unwrap())
            .unwrap()
            .timestamp()
    );
}

#[tokio::test]
async fn an_exclusion_needs_only_the_item() {
    let (state, _db) = state().await;
    let (s, e) = call(
        &state,
        "POST",
        "/api/v1/importlists/exclusions",
        Some(json!({
            "target_domain": "movie",
            "id_kind": "tmdb",
            "external_id": " 603 ",
            "title": "The Matrix",
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(e["external_id"], "603");
    let (_, all) = call(&state, "GET", "/api/v1/importlists/exclusions", None).await;
    assert_eq!(all[0]["title"], "The Matrix");

    let (s, _) = call(
        &state,
        "POST",
        "/api/v1/importlists/exclusions",
        Some(json!({ "target_domain": "movie", "id_kind": "tmdb", "external_id": "" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_failed_sync_is_stored_in_the_words_the_caller_saw() {
    let (state, db) = state().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({ "name": "Bond", "kind": "tmdb_collection", "target_domain": "movie" })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let (s, body) = call(
        &state,
        "POST",
        &format!("/api/v1/importlists/{id}/sync"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let stored = db.store.get_import_list(&id).await.unwrap().unwrap();
    assert_eq!(stored.last_error.as_deref(), body["message"].as_str());
    assert!(!stored.last_error.unwrap().starts_with("Validation error"));
}

#[tokio::test]
async fn an_audiobook_list_starts_monitored_and_a_film_list_does_not() {
    let (state, _db) = state().await;
    let (_, books) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({ "name": "To listen", "kind": "stub", "target_domain": "audiobook" })),
    )
    .await;
    assert_eq!(books["add_monitored"], true);
    let (_, films) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({ "name": "Bond", "kind": "tmdb_collection", "target_domain": "movie" })),
    )
    .await;
    assert_eq!(films["add_monitored"], false);
    // A body that says so wins.
    let (_, off) = call(
        &state,
        "POST",
        "/api/v1/importlists",
        Some(json!({ "name": "Later", "kind": "stub", "target_domain": "audiobook", "add_monitored": false })),
    )
    .await;
    assert_eq!(off["add_monitored"], false);
}
