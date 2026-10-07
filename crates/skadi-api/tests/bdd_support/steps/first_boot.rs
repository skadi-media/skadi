//! First-boot provisioning steps (C06, SKADI-T-0703): the library root and the
//! default-indexer switch a boot sees, and a record of the install that a later
//! boot must leave unchanged. Every scenario here sets env vars: tag `@serial`.
use cucumber::{given, then, when};
use skadi_store::{ConfigRepo, ConfigSource, DomainStateRepo, SettingsRepo, Store};

use crate::bdd_support::{EnvGuard, World};

/// The settings kinds a boot can provision.
const KINDS: &[&str] = &[
    "downloaders",
    "profiles",
    "indexers",
    "custom_formats",
    "notifiers",
];

fn library_root(w: &mut World) -> std::path::PathBuf {
    w.tmp().join("library")
}

#[given("the library root is an empty folder")]
async fn root_empty(w: &mut World) {
    let root = library_root(w);
    std::fs::create_dir_all(&root).expect("library root");
    w.env_guards
        .push(EnvGuard::set("SKADI_LIBRARY_ROOT", root.to_str().unwrap()));
}

#[given("the library root is a folder that does not exist")]
async fn root_missing(w: &mut World) {
    let root = library_root(w);
    w.env_guards
        .push(EnvGuard::set("SKADI_LIBRARY_ROOT", root.to_str().unwrap()));
}

#[given("the cardigann definitions live in a scratch folder")]
async fn definitions_scratch(w: &mut World) {
    let defs = w.tmp().join("definitions");
    w.env_guards.push(EnvGuard::set(
        "SKADI_CARDIGANN_DEFINITIONS_DIR",
        defs.to_str().unwrap(),
    ));
}

#[given("the curated trackers are switched off")]
async fn curated_off(w: &mut World) {
    w.env_guards
        .push(EnvGuard::set("SKADI_DEFAULT_INDEXERS", "false"));
}

async fn put_indexer(w: &mut World, definition: &str, auto: bool) {
    let store = w.store().await;
    store.run_migrations().await.expect("migrations");
    let mut body = serde_json::json!({
        "kind": "cardigann",
        "name": definition,
        "definition_id": definition,
        "settings": {},
    });
    if auto {
        body["default_seeded"] = serde_json::Value::Bool(true);
    }
    store
        .put_setting("indexers", &uuid::Uuid::new_v4().to_string(), &body)
        .await
        .unwrap();
}

#[given(expr = "an auto-seeded indexer for {string}")]
async fn auto_seeded(w: &mut World, definition: String) {
    put_indexer(w, &definition, true).await;
}

#[given(expr = "an operator-added indexer for {string}")]
async fn operator_added(w: &mut World, definition: String) {
    put_indexer(w, &definition, false).await;
}

/// What the definition sync worker does after a refresh: new files land in the
/// definitions folder, then it reconciles the curated trackers (the same call
/// as `skadi_api::definitions::sync_loop`). The new file is the bundled YTS
/// definition under another id, so it is in the curated scope.
#[when(expr = "a definition sync brings the new public tracker {string}")]
async fn definition_sync(w: &mut World, id: String) {
    let defs = w.tmp().join("definitions");
    std::fs::create_dir_all(&defs).unwrap();
    let yts = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../skadi-indexers/bundled/yts.yml"),
    )
    .unwrap();
    let def = yts.replacen("id: yts", &format!("id: {id}"), 1).replacen(
        "name: YTS",
        &format!("name: {id}"),
        1,
    );
    std::fs::write(defs.join(format!("{id}.yml")), def).unwrap();
    let store = w.store().await;
    skadi_api::sync_curated_indexers(&store)
        .await
        .expect("curated sync");
}

#[then(expr = "no indexer for {string} is registered")]
async fn no_indexer(w: &mut World, definition: String) {
    let rows = w.store().await.list_settings("indexers").await.unwrap();
    assert!(
        !rows
            .iter()
            .any(|s| s.body["definition_id"] == definition.as_str()),
        "{definition} is still registered"
    );
}

#[then(expr = "the indexer for {string} is still registered")]
async fn still_registered(w: &mut World, definition: String) {
    let rows = w.store().await.list_settings("indexers").await.unwrap();
    assert!(
        rows.iter()
            .any(|s| s.body["definition_id"] == definition.as_str()),
        "{definition} was removed"
    );
}

#[given(expr = "the operator has a profile named {string}")]
async fn operator_profile(w: &mut World, name: String) {
    let store = w.store().await;
    store.run_migrations().await.expect("migrations");
    store
        .put_setting(
            "profiles",
            &uuid::Uuid::new_v4().to_string(),
            &serde_json::json!({ "name": name }),
        )
        .await
        .unwrap();
}

#[given(expr = "the operator has a download client named {string}")]
async fn operator_downloader(w: &mut World, name: String) {
    let store = w.store().await;
    store.run_migrations().await.expect("migrations");
    store
        .put_setting(
            "downloaders",
            &uuid::Uuid::new_v4().to_string(),
            &serde_json::json!({ "kind": "skadi", "name": name }),
        )
        .await
        .unwrap();
}

#[when(expr = "the operator renames the built-in downloader to {string}")]
async fn rename_downloader(w: &mut World, name: String) {
    let store = w.store().await;
    let mut row = store
        .list_settings("downloaders")
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("a downloader");
    row.body["name"] = serde_json::Value::String(name);
    store
        .put_setting("downloaders", &row.id, &row.body)
        .await
        .unwrap();
}

#[when(expr = "the operator removes the indexer {string}")]
async fn remove_indexer(w: &mut World, definition: String) {
    let store = w.store().await;
    let row = store
        .list_settings("indexers")
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.body["definition_id"] == definition.as_str())
        .unwrap_or_else(|| panic!("no indexer {definition}"));
    assert!(store.delete_setting("indexers", &row.id).await.unwrap());
}

/// Every setting, domain switch and config value, in a stable order.
async fn snapshot(store: &Store, domains: &[String]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for kind in KINDS {
        let mut rows: Vec<(String, serde_json::Value)> = store
            .list_settings(kind)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.id, s.body))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        out.insert((*kind).to_string(), serde_json::json!(rows));
    }
    let mut enabled = Vec::new();
    for d in domains {
        enabled.push((d.clone(), store.get(d).await.unwrap().map(|s| s.enabled)));
    }
    out.insert("domains".into(), serde_json::json!(enabled));
    // Env-seeded keys are the operator's input to the boot, not its output.
    let mut config: Vec<(String, String)> = store
        .list_config()
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.source != ConfigSource::Env)
        .map(|e| (e.key, e.value))
        .collect();
    config.sort();
    out.insert("config".into(), serde_json::json!(config));
    serde_json::Value::Object(out)
}

fn domain_names(w: &World) -> Vec<String> {
    w.modules.iter().map(|m| m.name.to_string()).collect()
}

#[when("the state of the install is recorded")]
async fn record(w: &mut World) {
    let store = w.store().await;
    let snap = snapshot(&store, &domain_names(w)).await;
    w.install_snapshot = Some(snap);
}

#[then("the install is unchanged since it was recorded")]
async fn unchanged(w: &mut World) {
    let store = w.store().await;
    let now = snapshot(&store, &domain_names(w)).await;
    let before = w.install_snapshot.as_ref().expect("a recorded install");
    assert_eq!(
        &now, before,
        "the boot changed the install:\nbefore {before:#}\nafter  {now:#}"
    );
}

#[then(expr = "the domain {string} is enabled")]
async fn domain_enabled(w: &mut World, name: String) {
    let s = w.store().await.get(&name).await.unwrap();
    assert_eq!(s.map(|s| s.enabled), Some(true), "{name}");
}

#[then(expr = "the folder {string} exists under the library root")]
async fn folder_exists(w: &mut World, sub: String) {
    let path = library_root(w).join(&sub);
    assert!(path.is_dir(), "{} is not a folder", path.display());
}

#[then("the built-in downloader downloads under the library root")]
async fn downloader_under_root(w: &mut World) {
    let root = library_root(w);
    let row = w
        .store()
        .await
        .list_settings("downloaders")
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("a downloader");
    let complete = row.body["complete_dir"].as_str().unwrap_or_default();
    assert_eq!(
        complete,
        format!("{}/downloads/complete", root.display()),
        "{}",
        row.body
    );
}

#[then(expr = "the indexer {string} is registered from the curated set")]
async fn default_indexer(w: &mut World, definition: String) {
    let rows = w.store().await.list_settings("indexers").await.unwrap();
    let row = rows
        .iter()
        .find(|s| s.body["definition_id"] == definition.as_str())
        .unwrap_or_else(|| panic!("no indexer {definition}"));
    assert_eq!(row.body["kind"], "cardigann");
    assert_eq!(row.body["default_seeded"], true);
}
