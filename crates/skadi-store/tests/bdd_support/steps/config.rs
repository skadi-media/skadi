//! C03 storage steps: the `config` key/value table — upsert, provenance
//! (`env` vs `runtime`), ordering and the change fingerprint the supervisor
//! polls.
use cucumber::{given, then, when};

use skadi_store::{ConfigRepo, ConfigSource};

use crate::bdd_support::World;

fn source(s: &str) -> ConfigSource {
    match s {
        "env" => ConfigSource::Env,
        "runtime" => ConfigSource::Runtime,
        other => panic!("unknown source {other}"),
    }
}

#[given(expr = "the config key {string} is set to {string} from {word}")]
#[when(expr = "the config key {string} is set to {string} from {word}")]
async fn set(w: &mut World, key: String, value: String, src: String) {
    let e = w
        .store()
        .set_config(&key, &value, source(&src))
        .await
        .expect("set_config");
    assert_eq!(e.key, key);
    assert_eq!(e.value, value);
    assert_eq!(e.source, source(&src));
}

#[when(expr = "the config key {string} is deleted")]
async fn delete(w: &mut World, key: String) {
    let existed = w.store().delete_config(&key).await.expect("delete_config");
    w.notes.push(format!("deleted:{existed}"));
}

#[then(expr = "the config key {string} reads {string} from {word}")]
async fn reads(w: &mut World, key: String, value: String, src: String) {
    let e = w
        .store()
        .get_config(&key)
        .await
        .expect("get_config")
        .unwrap_or_else(|| panic!("{key} missing"));
    assert_eq!(e.value, value);
    assert_eq!(e.source, source(&src));
}

#[then(expr = "the config key {string} is absent")]
async fn absent(w: &mut World, key: String) {
    assert_eq!(w.store().get_config(&key).await.unwrap(), None);
}

#[then(expr = "the config table holds {int} row(s) in key order {string}")]
async fn rows(w: &mut World, n: usize, keys: String) {
    let all = w.store().list_config().await.unwrap();
    assert_eq!(all.len(), n);
    let want: Vec<&str> = keys.split(',').map(str::trim).collect();
    let got: Vec<&str> = all.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(got, want);
}

#[when("the config fingerprint is taken")]
#[given("the config fingerprint is taken")]
async fn fingerprint(w: &mut World) {
    let fp = w.store().config_fingerprint().await.expect("fingerprint");
    w.fingerprints.push(fp);
}

#[then("the fingerprint is unchanged")]
async fn unchanged(w: &mut World) {
    let n = w.fingerprints.len();
    assert!(n >= 2, "need two fingerprints");
    assert_eq!(w.fingerprints[n - 2], w.fingerprints[n - 1]);
}

#[then("the fingerprint changed")]
async fn changed(w: &mut World) {
    let n = w.fingerprints.len();
    assert!(n >= 2, "need two fingerprints");
    assert_ne!(w.fingerprints[n - 2], w.fingerprints[n - 1]);
}

/// Re-writing the *same* value must not look like a change: today
/// `set_config` re-stamps `updated_at` and the fingerprint hashes it
/// (`crates/skadi-store/src/config.rs:619-630`), so `PUT /downloads/settings`
/// (six unconditional writes) or a no-op save rebuilds every provider set.
#[when(expr = "the config key {string} is re-written with the same value {string}")]
async fn rewrite_same(w: &mut World, key: String, value: String) {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let before = w.store().get_config(&key).await.unwrap().expect("row");
    assert_eq!(before.value, value, "precondition: value already stored");
    w.store()
        .set_config(&key, &value, ConfigSource::Runtime)
        .await
        .expect("set_config");
}
