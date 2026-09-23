//! Settings CRUD conveniences + the supervisor's settings-reload semantics.
use std::sync::Arc;

use cucumber::gherkin::Step;
use cucumber::{given, then, when};

use skadi_api::Supervisor;
use skadi_store::{ConfigRepo, ConfigSource};

use crate::bdd_support::{CountingReloader, Sup, World};

#[given(expr = "a stored {word} setting remembered as {string} with body:")]
async fn stored_setting(w: &mut World, step: &Step, kind: String, name: String) {
    let body = step.docstring().cloned().unwrap_or_default();
    let r = w
        .call(
            "POST",
            &format!("/api/v1/settings/{kind}"),
            Some((body, "application/json")),
        )
        .await;
    assert_eq!(r.status, 201, "seeding {kind} failed: {}", r.text);
    let id = r.json["id"].as_str().expect("id").to_string();
    w.ids.insert(name, id);
}

#[given(expr = "the config key {string} is set to {string}")]
#[when(expr = "the config key {string} is set to {string}")]
async fn config_set(w: &mut World, key: String, value: String) {
    let value = w.expand(&value);
    w.store()
        .await
        .set_config(&key, &value, ConfigSource::Runtime)
        .await
        .expect("set_config");
}

#[then(expr = "the config key {string} equals {string}")]
async fn config_equals(w: &mut World, key: String, value: String) {
    let got = w
        .store()
        .await
        .get_config(&key)
        .await
        .expect("get_config")
        .map(|e| e.value);
    assert_eq!(got.as_deref(), Some(value.as_str()));
}

// --- supervisor reload semantics (SKADI-T-0062/0067) -----------------------

#[given(expr = "{int} provider reloaders are registered with the supervisor")]
async fn reloaders(w: &mut World, n: usize) {
    let store = w.store().await;
    let reloaders: Vec<Arc<CountingReloader>> = (0..n)
        .map(|_| Arc::new(CountingReloader::default()))
        .collect();
    let dyn_reloaders: Vec<Arc<dyn skadi_api::ProviderReloader>> = reloaders
        .iter()
        .map(|r| r.clone() as Arc<dyn skadi_api::ProviderReloader>)
        .collect();
    w.reloaders = reloaders;
    w.supervisor = Some(Sup(Supervisor::with_reloaders(
        store,
        Vec::new(),
        dyn_reloaders,
    )));
}

#[when("the supervisor reconciles")]
async fn reconcile(w: &mut World) {
    w.supervisor
        .as_ref()
        .expect("supervisor")
        .0
        .tick()
        .await
        .expect("tick");
}

#[then(expr = "every reloader has been applied {int} time(s)")]
async fn applied_times(w: &mut World, n: usize) {
    for (i, r) in w.reloaders.iter().enumerate() {
        let got = r.applied.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(got, n, "reloader {i} applied {got} times, expected {n}");
    }
}

/// SKADI-T-0466: the daemon re-reads `api_token` on the supervisor's reconcile
/// tick, so a rotation applies without a restart. The BDD harness runs no
/// supervisor, so the tick is driven explicitly here — the refresh lives on
/// `AppState` precisely so it can be, rather than needing a supervisor stood up.
#[when("the daemon re-reads its settings")]
async fn reread_settings(w: &mut World) {
    w.api().await.refresh_api_token().await;
}
