//! C03 settings-store schema steps: the key registry, env ↔ key mapping, the
//! boot-time env seeding rules and the typed `ConfigView` read contract.
use cucumber::gherkin::Step;
use cucumber::{given, then, when};

use skadi_config::{ConfigError, ConfigView, Mode, REGISTRY, Tier, ValueKind};

use crate::bdd_support::World;

// ---- registry / env mapping --------------------------------------------------

#[then(expr = "the config key {string} is read from the environment variable {string}")]
async fn env_name(_w: &mut World, key: String, env: String) {
    assert_eq!(skadi_config::env_name(&key), env);
    assert_eq!(skadi_config::key_from_env(&env), Some(key.as_str()));
}

#[then("every registered key round-trips through its environment variable name")]
async fn every_key_round_trips(_w: &mut World) {
    for spec in REGISTRY {
        let env = skadi_config::env_name(spec.key);
        assert!(env.starts_with("SKADI_"), "{env} lacks the SKADI_ prefix");
        assert_eq!(
            skadi_config::key_from_env(&env),
            Some(spec.key),
            "{} did not round-trip",
            spec.key
        );
    }
}

#[then(expr = "the environment variable {string} is not a config key")]
async fn not_a_key(_w: &mut World, env: String) {
    assert_eq!(skadi_config::key_from_env(&env), None);
}

#[then(expr = "the pre-table keys are exactly {string}")]
async fn tier0_keys(_w: &mut World, list: String) {
    let want: Vec<&str> = list.split(',').map(str::trim).collect();
    let got: Vec<&str> = REGISTRY
        .iter()
        .filter(|s| s.tier == Tier::Tier0)
        .map(|s| s.key)
        .collect();
    assert_eq!(got, want);
}

#[then(expr = "the key {string} defaults to {string}")]
async fn default_for(_w: &mut World, key: String, value: String) {
    assert_eq!(skadi_config::default_for(&key).unwrap(), value);
}

#[then(expr = "the key {string} is declared as a {word}")]
async fn declared_kind(_w: &mut World, key: String, kind: String) {
    let spec = skadi_config::spec(&key).unwrap_or_else(|| panic!("{key} is not registered"));
    let want = match kind.as_str() {
        "string" => ValueKind::String,
        "bool" => ValueKind::Bool,
        "u16" => ValueKind::U16,
        "u64" => ValueKind::U64,
        "path" => ValueKind::Path,
        other => panic!("unknown kind {other}"),
    };
    assert_eq!(spec.kind, want);
}

#[then(expr = "the key {string} is not registered")]
async fn not_registered(_w: &mut World, key: String) {
    assert!(skadi_config::spec(&key).is_none());
    assert_eq!(
        skadi_config::default_for(&key),
        Err(ConfigError::UnknownKey(key.clone()))
    );
}

/// Keys the daemon's HTTP layer writes into the `config` table (see
/// `crates/skadi-api/src/naming.rs:91-96` / `library.rs:504-511`). Every one of
/// them should be a registered key so it has a declared type + default.
#[then("every config key the daemon's naming endpoint writes is registered")]
async fn naming_keys_registered(_w: &mut World) {
    let written = [
        "naming.movie_folder",
        "naming.movie_file",
        "naming.series_folder",
        "naming.series_file",
        "naming.audiobook_folder",
        "naming.audiobook_file",
        "naming.space",
    ];
    let missing: Vec<&str> = written
        .iter()
        .copied()
        .filter(|k| skadi_config::spec(k).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "PUT /naming/settings writes unregistered config keys {missing:?} — \
         they have no declared type/default and ConfigView::get_string errors UnknownKey until written"
    );
}

// ---- env seeding (`read_env`) — @serial scenarios only ----------------------

#[given(expr = "the environment variable {string} is {string}")]
async fn env_set(w: &mut World, key: String, value: String) {
    w.env_touched.push((key.clone(), std::env::var(&key).ok()));
    // SAFETY: scenarios that touch the environment are tagged `@serial`, so no
    // other scenario reads env concurrently.
    unsafe { std::env::set_var(&key, &value) };
}

#[given(expr = "the environment variable {string} is unset")]
async fn env_unset(w: &mut World, key: String) {
    w.env_touched.push((key.clone(), std::env::var(&key).ok()));
    // SAFETY: see `env_set`.
    unsafe { std::env::remove_var(&key) };
}

#[when("the boot seeder collects the environment")]
async fn read_env(w: &mut World) {
    match skadi_config::read_env() {
        Ok(seeded) => w.seeded = seeded,
        Err(e) => w.seed_error = Some(e.to_string()),
    }
    if let Some(dir) = w.secret_dir.take() {
        let _ = std::fs::remove_dir_all(dir);
    }
    // Restore whatever this scenario touched so later scenarios see the
    // original process environment.
    for (key, prior) in w.env_touched.drain(..) {
        // SAFETY: `@serial` scenario; see `env_set`.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }
    }
}

#[then(expr = "the seeder collected {string} as {string}")]
async fn seeded_has(w: &mut World, key: String, value: String) {
    let got = w
        .seeded
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.as_str());
    assert_eq!(got, Some(value.as_str()), "seeded = {:?}", w.seeded);
}

#[then(expr = "the seeder did not collect {string}")]
async fn seeded_lacks(w: &mut World, key: String) {
    assert!(
        !w.seeded.iter().any(|(k, _)| *k == key),
        "{key} was seeded: {:?}",
        w.seeded
    );
}

// ---- typed view -------------------------------------------------------------

#[given("an empty config table")]
async fn empty_view(w: &mut World) {
    w.view = Some(ConfigView::default());
}

#[given("a config table holding:")]
async fn view_from_table(w: &mut World, step: &Step) {
    let table = step.table().expect("a | key | value | table");
    let pairs = table
        .rows
        .iter()
        .skip(1)
        .map(|r| (r[0].trim().to_string(), r[1].trim().to_string()));
    w.view = Some(ConfigView::from_pairs(pairs));
}

#[when(expr = "the {word} value of {string} is read")]
async fn read_typed(w: &mut World, kind: String, key: String) {
    let view = w.view.as_ref().expect("a config table");
    w.last = Some(match kind.as_str() {
        "string" => view.get_string(&key),
        "optional" => view.get_opt_string(&key).map(|o| format!("{o:?}")),
        "bool" => view.get_bool(&key).map(|b| b.to_string()),
        "u16" => view.get_u16(&key).map(|n| n.to_string()),
        "u64" => view.get_u64(&key).map(|n| n.to_string()),
        "path" => view.get_path(&key).map(|p| {
            p.map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".into())
        }),
        "mode" => view.mode().map(|m| format!("{m:?}")),
        other => panic!("unknown read kind {other}"),
    });
}

#[then(expr = "the value is {string}")]
async fn value_is(w: &mut World, want: String) {
    match w.last.as_ref().expect("a read") {
        Ok(v) => assert_eq!(v, &want),
        Err(e) => panic!("expected {want:?}, got error {e}"),
    }
}

#[then(expr = "the read fails with an unknown-key error for {string}")]
async fn unknown_key(w: &mut World, key: String) {
    assert_eq!(
        w.last.as_ref().expect("a read"),
        &Err(ConfigError::UnknownKey(key))
    );
}

#[then(expr = "the read fails with a parse error naming {string}")]
async fn parse_error(w: &mut World, key: String) {
    match w.last.as_ref().expect("a read") {
        Err(ConfigError::Parse { key: k, .. }) => assert_eq!(k, &key),
        other => panic!("expected a parse error for {key}, got {other:?}"),
    }
}

#[then(expr = "the mode string {string} parses as {word}")]
async fn mode_parses(_w: &mut World, s: String, want: String) {
    let got: Mode = s.parse().unwrap_or_else(|e| panic!("{s:?}: {e}"));
    assert_eq!(format!("{got:?}"), want);
}

#[then(expr = "the mode string {string} is rejected")]
async fn mode_rejected(_w: &mut World, s: String) {
    assert!(s.parse::<Mode>().is_err());
}

// ---- parity gaps ------------------------------------------------------------

/// Sonarr/Radarr validate a settings value against its declared type/range when
/// it is saved (the form shows the error). skadi-config declares `ValueKind` but
/// offers no write-side validator: the table accepts any string and the parse
/// error only surfaces on the consumer's next typed read.
#[then(expr = "the schema rejects {string} as a value for {string} before it is stored")]
async fn write_side_validation(_w: &mut World, value: String, key: String) {
    // SKADI-T-0524: values used to be parsed only on the *read* path, so a typo
    // went into the table happily and surfaced later as a startup failure or a
    // silent fallback — far from the operator who typed it.
    let err = skadi_config::validate_write(&key, &value)
        .expect_err("a value of the wrong type must be rejected on write");
    let text = err.to_string();
    assert!(
        text.contains(&key) && text.contains(&value),
        "the rejection must name the key and the offending value: {text}"
    );
    // The valid value for the same key still passes, so the check is a type test
    // and not a blanket refusal.
    assert!(skadi_config::validate_write(&key, "16881").is_ok());
}

/// Sonarr's download-client / port settings are range-checked against each
/// other (port range lo <= hi) — SKADI-T-0524.
#[then("the schema rejects a worker port range whose low end is above its high end")]
async fn cross_key_validation(_w: &mut World) {
    // Each value is individually valid; only together are they wrong, which is
    // exactly what per-key validation cannot catch. Left unchecked the worker
    // simply cannot bind.
    let bad = ConfigView::from_pairs([
        ("worker.port_lo".to_string(), "17000".to_string()),
        ("worker.port_hi".to_string(), "16000".to_string()),
    ]);
    assert!(skadi_config::validate_write("worker.port_lo", "17000").is_ok());
    assert!(skadi_config::validate_write("worker.port_hi", "16000").is_ok());
    let err = skadi_config::validate_cross_key(&bad)
        .expect_err("an inverted port range must be rejected");
    assert!(err.to_string().contains("port range"), "{err}");

    // The right way round is accepted.
    let good = ConfigView::from_pairs([
        ("worker.port_lo".to_string(), "16000".to_string()),
        ("worker.port_hi".to_string(), "17000".to_string()),
    ]);
    assert!(skadi_config::validate_cross_key(&good).is_ok());
}

// ---- secrets from files (C37, SKADI-T-0702) ---------------------------------

#[given(expr = "the secret file {string} holds the line {string}")]
async fn secret_file(w: &mut World, name: String, value: String) {
    let dir = w
        .secret_dir
        .get_or_insert_with(|| {
            let d = std::env::temp_dir().join(format!(
                "skadi-config-bdd-secrets-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            d
        })
        .clone();
    std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
}

#[given(expr = "the environment variable {string} names the secret file {string}")]
async fn env_names_secret_file(w: &mut World, key: String, name: String) {
    let dir = w
        .secret_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("skadi-config-bdd-no-secrets"));
    let path = dir.join(name).display().to_string();
    env_set(w, key, path).await;
}

#[then(expr = "the boot seeder fails, naming {string}")]
async fn seeder_fails(w: &mut World, needle: String) {
    let err = w
        .seed_error
        .as_deref()
        .expect("read_env should have failed");
    assert!(
        err.contains(&needle),
        "error {err:?} does not name {needle:?}"
    );
}

#[then(expr = "the error does not contain {string}")]
async fn error_lacks(w: &mut World, needle: String) {
    let err = w.seed_error.as_deref().unwrap_or_default();
    assert!(!err.contains(&needle), "error {err:?} leaks {needle:?}");
}
