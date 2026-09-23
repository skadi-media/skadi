//! Cross-backend conformance harness.
//!
//! Runs the same migration and repository assertions against every available
//! backend, so SQLite and Postgres are proven to behave identically. SQLite
//! always runs (temp file); Postgres runs when `SKADI_TEST_DATABASE_URL` is set
//! (e.g. the `docker compose` postgres service) and is skipped — not failed —
//! otherwise, so the default local `cargo test` needs no database.
//!
//! Bring up the Postgres backend with:
//! ```text
//! docker compose up -d postgres
//! export SKADI_TEST_DATABASE_URL=postgres://skadi:skadi@127.0.0.1:5433/skadi
//! ```

use skadi_store::{ConfigRepo, ConfigSource, DomainState, DomainStateRepo, Store};

fn unique_sqlite_url() -> String {
    let path = skadi_core::unique_temp_path("xbtest").with_extension("db");
    format!("sqlite://{}", path.display())
}

/// `(label, url)` for each backend under test.
fn backends() -> Vec<(&'static str, String)> {
    let mut backends = vec![("sqlite", unique_sqlite_url())];
    match std::env::var("SKADI_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => backends.push(("postgres", url)),
        _ => eprintln!("SKADI_TEST_DATABASE_URL unset — skipping Postgres backend"),
    }
    backends
}

/// Migration round-trip (up → down → up) leaving a clean, empty schema.
async fn migrate_clean(store: &Store, label: &str) {
    store.run_migrations().await.unwrap();
    let reverted = store.revert_migrations().await.unwrap();
    assert!(reverted >= 1, "{label}: revert should undo >= 1 migration");
    let reapplied = store.run_migrations().await.unwrap();
    assert_eq!(
        reapplied, reverted,
        "{label}: re-applying should match the reverted count"
    );
}

/// The behavioral contract `DomainStateRepo` must satisfy on every backend.
async fn assert_domain_state_contract(store: &Store, label: &str) {
    assert!(
        store.list().await.unwrap().is_empty(),
        "{label}: starts empty"
    );

    let mut movies = DomainState::new("movies");
    movies.settings = serde_json::json!({ "root": "/movies", "quality": 1080 });
    store.upsert(&movies).await.unwrap();
    assert_eq!(
        store.get("movies").await.unwrap(),
        Some(movies.clone()),
        "{label}: round-trips settings JSON"
    );

    assert_eq!(store.get("music").await.unwrap(), None, "{label}: missing");

    let enabled = store.set_enabled("movies", true).await.unwrap();
    assert!(enabled.enabled, "{label}: enabled flag set");
    assert!(enabled.enabled_at.is_some(), "{label}: enabled_at stamped");
    assert_eq!(enabled.settings, movies.settings, "{label}: settings kept");

    // Creating-on-enable for an absent domain.
    store.set_enabled("books", true).await.unwrap();

    let all = store.list().await.unwrap();
    assert_eq!(all.len(), 2, "{label}: two domains");
    assert_eq!(all[0].name, "books", "{label}: ordered by name");
    assert_eq!(all[1].name, "movies");

    let disabled = store.set_enabled("movies", false).await.unwrap();
    assert!(!disabled.enabled, "{label}: disabled");
    assert!(disabled.enabled_at.is_none(), "{label}: enabled_at cleared");
}

/// The behavioral contract `ConfigRepo` must satisfy on every backend
/// (SKADI-T-0099).
async fn assert_config_contract(store: &Store, label: &str) {
    assert!(
        store.list_config().await.unwrap().is_empty(),
        "{label}: config starts empty"
    );
    assert_eq!(
        store.get_config("mode").await.unwrap(),
        None,
        "{label}: missing key is None"
    );

    // Insert (source=env).
    let e = store
        .set_config("mode", "testing", ConfigSource::Env)
        .await
        .unwrap();
    assert_eq!(e.key, "mode");
    assert_eq!(e.value, "testing");
    assert_eq!(e.source, ConfigSource::Env);
    let fp1 = store.config_fingerprint().await.unwrap();

    // Fingerprint is stable across a re-read (no write).
    assert_eq!(
        store.config_fingerprint().await.unwrap(),
        fp1,
        "{label}: fingerprint stable without a write"
    );

    // Upsert same key (source=runtime) overwrites value + source and bumps fp.
    let e2 = store
        .set_config("mode", "production", ConfigSource::Runtime)
        .await
        .unwrap();
    assert_eq!(e2.value, "production");
    assert_eq!(e2.source, ConfigSource::Runtime);
    assert_eq!(
        store.list_config().await.unwrap().len(),
        1,
        "{label}: upsert, not insert"
    );
    assert_ne!(
        store.config_fingerprint().await.unwrap(),
        fp1,
        "{label}: fingerprint changes on a value write"
    );

    // A second key; list is ordered by key.
    store
        .set_config("bind_addr", "0.0.0.0:9000", ConfigSource::Env)
        .await
        .unwrap();
    let all = store.list_config().await.unwrap();
    assert_eq!(
        all.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        vec!["bind_addr", "mode"],
        "{label}: list ordered by key"
    );

    // Delete.
    assert!(
        store.delete_config("mode").await.unwrap(),
        "{label}: deleted"
    );
    assert!(
        !store.delete_config("mode").await.unwrap(),
        "{label}: second delete is a no-op"
    );
    assert_eq!(store.get_config("mode").await.unwrap(), None);
}

#[tokio::test]
async fn backends_behave_identically() {
    for (label, url) in backends() {
        let store = Store::connect(&url).expect("connect");
        // up/down/up leaves an empty schema, which the repo contract then assumes.
        migrate_clean(&store, label).await;
        assert_domain_state_contract(&store, label).await;
        assert_config_contract(&store, label).await;
    }
}
