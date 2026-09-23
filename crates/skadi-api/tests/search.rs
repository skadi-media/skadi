//! Manual/interactive search endpoint tests (SKADI-T-0203).
//!
//! Uses the built-in **Stub** indexer (no secret, canned open-movie releases that
//! match by title) so the test exercises `GET /search` end-to-end through
//! `build_providers` without standing up a real Torznab/Prowlarr server.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config};
use skadi_store::SettingsRepo;
use skadi_testsupport::TestDb;

/// Returns the app state, the TestDb guard, and the seeded indexer's id.
async fn state_with_stub_indexer() -> (Arc<AppState>, TestDb, String) {
    let db = TestDb::new_store_only().await;
    // Seed a built-in Stub indexer config (needs no secret).
    let ix_id = uuid::Uuid::new_v4().to_string();
    db.store
        .put_setting(
            "indexers",
            &ix_id,
            &serde_json::json!({ "kind": "stub", "name": "built-in" }),
        )
        .await
        .unwrap();
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let state = AppState::new_full(config, Some(db.store.clone()), vec![], vec![]);
    (state, db, ix_id)
}

async fn get(state: &Arc<AppState>, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = skadi_api::router(state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn search_returns_candidate_releases_from_the_indexers() {
    let (state, _db, _ix) = state_with_stub_indexer().await;

    // The stub matches "Sintel" → a 1080p BluRay magnet release.
    let (s, body) = get(&state, "/api/v1/search?q=Sintel").await;
    assert_eq!(s, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one candidate for Sintel: {body}");
    let r = &arr[0];
    assert!(
        r["title"].as_str().unwrap().contains("Sintel"),
        "{}",
        r["title"]
    );
    assert_eq!(r["protocol"], "magnet");
    assert_eq!(r["resolution"], "1080p");
    assert!(r["indexer"].is_string());
    assert!(r["size"].is_number());
}

#[tokio::test]
async fn search_for_unknown_title_is_empty() {
    let (state, _db, _ix) = state_with_stub_indexer().await;
    let (s, body) = get(&state, "/api/v1/search?q=NoSuchMovieXYZ").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn search_requires_a_non_empty_query() {
    let (state, _db, _ix) = state_with_stub_indexer().await;
    let (s, _) = get(&state, "/api/v1/search?q=").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn indexer_categories_lists_the_named_newznab_tree() {
    let (state, _db, _ix) = state_with_stub_indexer().await;
    let (s, body) = get(&state, "/api/v1/indexers/categories").await;
    assert_eq!(s, StatusCode::OK);
    let cats = body.as_array().unwrap();
    let movies = cats.iter().find(|c| c["id"] == 2000).expect("movies");
    assert_eq!(movies["name"], "Movies");
    let audio = cats.iter().find(|c| c["id"] == 3000).expect("audio");
    assert!(
        audio["sub_categories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == 3030 && s["name"] == "Audio/Audiobook"),
        "audiobook category exposed for the picker"
    );
}

#[tokio::test]
async fn indexer_health_reflects_a_search() {
    let (state, _db, ix_id) = state_with_stub_indexer().await;

    // Drive a search so the HealthTracked decorator records a success for this id.
    let (s, _) = get(&state, "/api/v1/search?q=Sintel").await;
    assert_eq!(s, StatusCode::OK);

    let (s, body) = get(&state, "/api/v1/indexers/health").await;
    assert_eq!(s, StatusCode::OK);
    let entry = body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["indexer"] == serde_json::json!(ix_id))
        .expect("our indexer appears in the health snapshot");
    assert_eq!(entry["healthy"], true);
    assert!(entry["success_count"].as_u64().unwrap() >= 1);
    assert_eq!(entry["consecutive_failures"], 0);
}
