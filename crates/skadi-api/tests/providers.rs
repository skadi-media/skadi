//! Provider factory tests (SKADI-T-0060).
//!
//! Seeds real settings rows + credentials in a temp SQLite store and asserts
//! `build_providers` constructs the right live set, skipping bad rows without
//! sinking the rest.

use skadi_api::build_providers;
use skadi_store::{CredentialRepo, SettingsRepo, Store};
use skadi_testsupport::TestDb;

async fn temp_store() -> (Store, TestDb) {
    // Postgres-default isolated DB (SQLite fallback). Caller keeps the `TestDb`
    // guard alive for the test's duration (SKADI-T-0077).
    let db = TestDb::new_store_only().await;
    (db.store.clone(), db)
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::test]
async fn builds_all_three_provider_kinds() {
    let (store, _db) = temp_store().await;

    // Indexer (torznab) + its api key.
    let ix = uuid();
    store
        .put_setting(
            "indexers",
            &ix,
            &serde_json::json!({
                "kind": "torznab",
                "name": "geek",
                "base_url": "https://api.example.org",
                "categories": [2000]
            }),
        )
        .await
        .unwrap();
    store.set_secret("indexers", &ix, "apikey").await.unwrap();

    // Downloader: the built-in DB-queue client, which needs no endpoint and no
    // secret (SKADI-T-0517 removed the only kind that did).
    let dl = uuid();
    store
        .put_setting(
            "downloaders",
            &dl,
            &serde_json::json!({
                "kind": "skadi",
                "name": "built-in"
            }),
        )
        .await
        .unwrap();

    // Notifier (webhook), no secret (optional).
    let nf = uuid();
    store
        .put_setting(
            "notifiers",
            &nf,
            &serde_json::json!({
                "kind": "webhook",
                "name": "ping",
                "url": "https://example.com/hook",
                "channels": ["imported"]
            }),
        )
        .await
        .unwrap();

    let set = build_providers(&store).await.unwrap();
    assert_eq!(set.indexers.len(), 1);
    assert_eq!(set.downloaders.len(), 1);
    assert_eq!(set.notifiers.len(), 1);
    assert_eq!(set.len(), 3);

    // Provider identity matches the settings row id.
    assert_eq!(set.indexers[0].id().to_string(), ix);
    assert_eq!(set.downloaders[0].id().to_string(), dl);
    assert_eq!(set.notifiers[0].id().to_string(), nf);
}

#[tokio::test]
async fn empty_settings_is_an_empty_set() {
    let (store, _db) = temp_store().await;
    let set = build_providers(&store).await.unwrap();
    assert!(set.is_empty());
}

#[tokio::test]
async fn bad_rows_are_skipped_not_fatal() {
    let (store, _db) = temp_store().await;

    // A malformed indexer row (unknown kind).
    store
        .put_setting(
            "indexers",
            &uuid(),
            &serde_json::json!({ "kind": "prowlarr", "name": "nope" }),
        )
        .await
        .unwrap();

    // A torznab row with NO credential — required, so skipped.
    store
        .put_setting(
            "indexers",
            &uuid(),
            &serde_json::json!({
                "kind": "torznab",
                "name": "no-key",
                "base_url": "https://x",
                "categories": [2000]
            }),
        )
        .await
        .unwrap();

    // A torznab row that fails validation (empty categories).
    let bad = uuid();
    store
        .put_setting(
            "indexers",
            &bad,
            &serde_json::json!({
                "kind": "torznab",
                "name": "no-cats",
                "base_url": "https://x",
                "categories": []
            }),
        )
        .await
        .unwrap();
    store.set_secret("indexers", &bad, "k").await.unwrap();

    // And one GOOD row.
    let good = uuid();
    store
        .put_setting(
            "indexers",
            &good,
            &serde_json::json!({
                "kind": "torznab",
                "name": "good",
                "base_url": "https://good.example.org",
                "categories": [2000]
            }),
        )
        .await
        .unwrap();
    store.set_secret("indexers", &good, "k").await.unwrap();

    let set = build_providers(&store).await.unwrap();
    assert_eq!(set.indexers.len(), 1, "only the good row builds");
    assert_eq!(set.indexers[0].id().to_string(), good);
}
