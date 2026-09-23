//! Health & diagnostics fixtures: the library root on disk, worker heartbeats,
//! and the environment knobs some endpoints read (`@serial` scenarios only).
use cucumber::{given, then};

use skadi_store::{ConfigRepo, ConfigSource, WorkerStatusRepo};

use crate::bdd_support::World;

#[given("the library root points at an existing writable directory")]
async fn library_root_exists(w: &mut World) {
    let dir = w.tmp().join("library");
    std::fs::create_dir_all(&dir).expect("mkdir");
    w.store()
        .await
        .set_config(
            "library.root",
            &dir.display().to_string(),
            ConfigSource::Runtime,
        )
        .await
        .expect("set library.root");
}

#[given(expr = "the library root contains the folders {string}")]
async fn library_root_folders(w: &mut World, list: String) {
    let dir = w.tmp().join("library");
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        std::fs::create_dir_all(dir.join(name)).expect("mkdir child");
    }
}

#[given(expr = "the library root points at the missing path {string}")]
async fn library_root_missing(w: &mut World, path: String) {
    w.store()
        .await
        .set_config("library.root", &path, ConfigSource::Runtime)
        .await
        .expect("set library.root");
}

#[given(expr = "the download worker {string} heartbeated just now")]
async fn worker_heartbeat(w: &mut World, id: String) {
    w.store()
        .await
        .heartbeat_worker(&id, "0.0.0-test")
        .await
        .expect("heartbeat");
}

/// Process-global env knob — only for scenarios tagged `@serial`.
#[given(expr = "the environment variable {string} is {string}")]
async fn env_set(w: &mut World, key: String, value: String) {
    let value = value.replace("<tmp>", &w.tmp().display().to_string());
    // SAFETY: scenarios that touch the environment are tagged `@serial`, so no
    // other scenario reads env concurrently.
    unsafe { std::env::set_var(&key, &value) };
    w.notes.push(format!("env:{key}"));
}

#[given(expr = "the environment variable {string} is unset")]
async fn env_unset(_w: &mut World, key: String) {
    // SAFETY: see `env_set`.
    unsafe { std::env::remove_var(&key) };
}

#[given(expr = "a published APK {string} with manifest version {string} in the scratch apk folder")]
async fn published_apk(w: &mut World, file: String, version: String) {
    let dir = w.tmp().join("apk");
    std::fs::create_dir_all(&dir).expect("mkdir apk");
    std::fs::write(dir.join(&file), b"not-really-an-apk").expect("apk bytes");
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::json!({ "file": file, "version_name": version }).to_string(),
    )
    .expect("manifest");
}

#[then(expr = "the health check named {string} has status {string}")]
async fn check_status(w: &mut World, name: String, status: String) {
    let r = w.reply();
    let check = r
        .json
        .as_array()
        .and_then(|a| a.iter().find(|c| c["name"] == name))
        .unwrap_or_else(|| panic!("no health check named {name:?} in {}", r.text));
    assert_eq!(check["status"], status, "check: {check}");
}

#[then(expr = "there is no health check named {string}")]
async fn no_check(w: &mut World, name: String) {
    let r = w.reply();
    assert!(
        r.json
            .as_array()
            .is_some_and(|a| !a.iter().any(|c| c["name"] == name)),
        "unexpected check {name:?} in {}",
        r.text
    );
}

#[then(expr = "some health check name starts with {string}")]
async fn check_prefix(w: &mut World, prefix: String) {
    let r = w.reply();
    assert!(
        r.json.as_array().is_some_and(|a| a
            .iter()
            .any(|c| c["name"].as_str().is_some_and(|n| n.starts_with(&prefix)))),
        "no health check starting with {prefix:?}; got {}",
        r.text
    );
}
