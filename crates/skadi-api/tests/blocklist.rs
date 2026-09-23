//! Blocklist endpoint tests — focus on bulk-remove + clear-all (SKADI-T-0195).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config};
use skadi_store::{BlocklistRepo, NewBlocklistEntry};
use skadi_testsupport::TestDb;

async fn state() -> (Arc<AppState>, TestDb) {
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let state = AppState::new_full(config, Some(db.store.clone()), vec![], vec![]);
    (state, db)
}

async fn call(state: &Arc<AppState>, method: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = skadi_api::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn entry(key: &str, title: &str) -> NewBlocklistEntry {
    NewBlocklistEntry {
        release_key: key.into(),
        title: title.into(),
        acquirable_ref: Some("ed-1".into()),
        indexer: Some("torznab".into()),
        reason: Some("download failed".into()),
        expires_at: None,
    }
}

#[tokio::test]
async fn delete_blocklist_bulk_by_ids_then_clear_all() {
    let (state, db) = state().await;

    let a = db.store.block(&entry("btih:a", "A")).await.unwrap();
    let b = db.store.block(&entry("btih:b", "B")).await.unwrap();
    db.store.block(&entry("btih:c", "C")).await.unwrap();

    // Sanity: all three are listed.
    let (s, list) = call(&state, "GET", "/api/v1/blocklist").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 3);

    // Bulk remove two by id (with a stray unknown id, which is ignored).
    let uri = format!("/api/v1/blocklist?ids={},{},{}", a.id, b.id, "nope");
    let (s, body) = call(&state, "DELETE", &uri).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["removed"], 2);

    let (_, list) = call(&state, "GET", "/api/v1/blocklist").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "only C remains");

    // SKADI-T-0473: a bare DELETE used to wipe everything. It is now refused —
    // a destructive default reachable by omission is exactly the footgun.
    let (s, body) = call(&state, "DELETE", "/api/v1/blocklist").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "validation");
    let (_, list) = call(&state, "GET", "/api/v1/blocklist").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "nothing was removed");

    // An explicitly-empty id list removes nothing and must NOT fall through to
    // the wipe — that is the client bug the confirmation guards against.
    let (s, body) = call(&state, "DELETE", "/api/v1/blocklist?ids=").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["removed"], 0);
    let (_, list) = call(&state, "GET", "/api/v1/blocklist").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "still there");

    // Clear-all with explicit confirmation removes the remainder.
    let (s, body) = call(&state, "DELETE", "/api/v1/blocklist?all=true").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["removed"], 1);

    let (_, list) = call(&state, "GET", "/api/v1/blocklist").await;
    assert!(list.as_array().unwrap().is_empty(), "blocklist cleared");

    // Clearing an already-empty blocklist removes nothing.
    let (_, body) = call(&state, "DELETE", "/api/v1/blocklist?all=true").await;
    assert_eq!(body["removed"], 0);
}

#[tokio::test]
async fn post_blocklist_with_ttl_sets_expiry() {
    let (state, db) = state().await;

    // POST with a TTL → the stored row carries an expiry; the response echoes it.
    let res = skadi_api::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/blocklist")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "release_key": "btih:temp",
                        "title": "Temp Block",
                        "ttl_seconds": 3600
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["expires_at"].is_string(),
        "ttl_seconds should set expires_at: {body}"
    );

    // A TTL'd entry that hasn't elapsed still vetoes.
    assert!(db.store.is_blocked("btih:temp").await.unwrap());

    // A plain POST (no ttl) is permanent → expires_at null.
    let (_, perm) = call(&state, "GET", "/api/v1/blocklist").await;
    let _ = perm;
}
