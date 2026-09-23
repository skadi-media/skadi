//! Mode-keyed bootstrap presets (SKADI-T-0102): `mode` in the config table
//! drives one-time, non-clobbering seeding of runtime provider settings.

use skadi_api::seed_mode_presets;
use skadi_store::{ConfigRepo, ConfigSource, DomainStateRepo, SettingsRepo, Store};

fn temp_store_url() -> String {
    let p = skadi_core::unique_temp_path("presets").with_extension("db");
    format!("sqlite://{}", p.display())
}

async fn fresh_store() -> Store {
    let store = Store::connect(&temp_store_url()).unwrap();
    store.run_migrations().await.unwrap();
    store
}

async fn count(store: &Store, kind: &str) -> usize {
    store.list_settings(kind).await.unwrap().len()
}

async fn movies_enabled(store: &Store) -> Option<bool> {
    store.get("movies").await.unwrap().map(|d| d.enabled)
}

/// The built-in default profile set seeded on first boot (SKADI-T-0145).
const DEFAULT_PROFILE_COUNT: usize = 6;

#[tokio::test]
async fn production_mode_seeds_the_default_profiles_only() {
    let store = fresh_store().await;
    // No `mode` key → defaults to production.
    let mode = seed_mode_presets(&store, &["movies"]).await.unwrap();
    assert_eq!(mode, skadi_config::Mode::Production);
    // Even production now ships the default quality-profile set (SKADI-T-0145)…
    assert_eq!(count(&store, "profiles").await, DEFAULT_PROFILE_COUNT);
    // …but nothing else: the operator wires their own root folder / providers.
    assert_eq!(count(&store, "root_folders").await, 0);
    assert_eq!(count(&store, "downloaders").await, 0);
    assert_eq!(movies_enabled(&store).await, None, "domain untouched");

    // The set is the *arr-style ladder, each with a non-empty allowed + cutoff.
    let names: Vec<String> = store
        .list_settings("profiles")
        .await
        .unwrap()
        .iter()
        .map(|p| p.body["name"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "Any",
        "SD",
        "HD-720p",
        "HD-1080p",
        "HD-720p/1080p",
        "Ultra-HD",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    let prof = &store.list_settings("profiles").await.unwrap()[0];
    assert!(!prof.body["allowed"].as_array().unwrap().is_empty());
    assert!(prof.body["cutoff"].is_string());
}

#[tokio::test]
async fn just_go_mode_seeds_profile_and_root_only() {
    let store = fresh_store().await;
    store
        .set_config("mode", "just-go", ConfigSource::Env)
        .await
        .unwrap();

    let mode = seed_mode_presets(&store, &["movies"]).await.unwrap();
    assert_eq!(mode, skadi_config::Mode::JustGo);
    // The default profile set (all modes). No root folder is seeded any more —
    // the library root is the single derived `library.root` (SKADI-T-0302).
    assert_eq!(count(&store, "profiles").await, DEFAULT_PROFILE_COUNT);
    assert_eq!(count(&store, "root_folders").await, 0);
    // just-go leaves the downloader (deliberate register) + domain to the operator.
    assert_eq!(count(&store, "downloaders").await, 0);
    assert_eq!(movies_enabled(&store).await, None);

    // Profiles have non-empty allowed + a string cutoff.
    let prof = &store.list_settings("profiles").await.unwrap()[0];
    assert!(!prof.body["allowed"].as_array().unwrap().is_empty());
    assert!(prof.body["cutoff"].is_string());
}

#[tokio::test]
async fn testing_mode_seeds_a_working_config_idempotently() {
    let store = fresh_store().await;
    store
        .set_config("mode", "testing", ConfigSource::Env)
        .await
        .unwrap();

    let mode = seed_mode_presets(&store, &["movies"]).await.unwrap();
    assert_eq!(mode, skadi_config::Mode::Testing);
    assert_eq!(count(&store, "profiles").await, DEFAULT_PROFILE_COUNT);
    assert_eq!(count(&store, "root_folders").await, 0);
    assert_eq!(count(&store, "downloaders").await, 1, "built-in downloader");
    assert_eq!(count(&store, "indexers").await, 1, "built-in stub indexer");
    assert_eq!(movies_enabled(&store).await, Some(true), "domain enabled");

    let dl = &store.list_settings("downloaders").await.unwrap()[0];
    assert_eq!(dl.body["kind"], "skadi");
    let ix = &store.list_settings("indexers").await.unwrap()[0];
    assert_eq!(
        ix.body["kind"], "stub",
        "testing mode seeds the stub indexer"
    );

    // Second boot is a no-op: the marker prevents re-seeding (no duplicates).
    seed_mode_presets(&store, &["movies"]).await.unwrap();
    assert_eq!(count(&store, "profiles").await, DEFAULT_PROFILE_COUNT);
    assert_eq!(count(&store, "root_folders").await, 0);
    assert_eq!(count(&store, "downloaders").await, 1);
    assert_eq!(count(&store, "indexers").await, 1);
}

#[tokio::test]
async fn presets_never_clobber_existing_operator_settings() {
    let store = fresh_store().await;
    store
        .set_config("mode", "testing", ConfigSource::Env)
        .await
        .unwrap();
    // Operator already has a profile; the preset must not add a second.
    store
        .put_setting(
            "profiles",
            "11111111-1111-1111-1111-111111111111",
            &serde_json::json!({ "name": "mine" }),
        )
        .await
        .unwrap();

    seed_mode_presets(&store, &["movies"]).await.unwrap();
    let profs = store.list_settings("profiles").await.unwrap();
    assert_eq!(profs.len(), 1, "did not add a second profile");
    assert_eq!(profs[0].body["name"], "mine", "kept the operator's profile");
}
