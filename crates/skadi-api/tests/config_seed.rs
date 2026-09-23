//! Boot config seeding (SKADI-T-0100): `SKADI_*` env → `config` table, env
//! overwrites. In its own test binary so the process-global env mutation can't
//! race other tests.

use skadi_api::{Config, load_config_view, seed_config_from_env, service_fingerprint};
use skadi_store::{ConfigRepo, ConfigSource, Store};

fn temp_store_url() -> String {
    let p = skadi_core::unique_temp_path("cfgseed").with_extension("db");
    format!("sqlite://{}", p.display())
}

#[tokio::test]
#[serial_test::serial(skadi_env)]
async fn env_seeds_config_overwrites_runtime_and_preserves_unset_keys() {
    // This binary has more than one test and they share a process environment, so
    // the env writes below are serialised with the sibling test rather than being
    // safe by virtue of being alone (the "sole test" this comment once claimed).
    unsafe {
        std::env::set_var("SKADI_MODE", "testing");
        std::env::set_var("SKADI_API_TOKEN", "tok-123");
        // bind_addr must NOT come from env, so its runtime value can survive.
        std::env::remove_var("SKADI_BIND_ADDR");
    }

    let store = Store::connect(&temp_store_url()).unwrap();
    store.run_migrations().await.unwrap();

    // Pre-seed runtime values: `mode` is also in env (should be overwritten);
    // `bind_addr` is not in env (should survive).
    store
        .set_config("mode", "production", ConfigSource::Runtime)
        .await
        .unwrap();
    store
        .set_config("bind_addr", "9.9.9.9:1", ConfigSource::Runtime)
        .await
        .unwrap();

    let n = seed_config_from_env(&store).await.unwrap();
    assert!(n >= 2, "seeded at least mode + api_token, got {n}");

    // Env wins: the prior runtime `mode` is overwritten to env's value+source.
    let mode = store.get_config("mode").await.unwrap().unwrap();
    assert_eq!(mode.value, "testing");
    assert_eq!(mode.source, ConfigSource::Env);

    // A key only present in env is seeded.
    let tok = store.get_config("api_token").await.unwrap().unwrap();
    assert_eq!(tok.value, "tok-123");
    assert_eq!(tok.source, ConfigSource::Env);

    // A runtime key absent from env keeps its value + source.
    let bind = store.get_config("bind_addr").await.unwrap().unwrap();
    assert_eq!(bind.value, "9.9.9.9:1");
    assert_eq!(bind.source, ConfigSource::Runtime);

    // A migrated reader (Config::resolve over the loaded view) reads the
    // table-backed fields: bind_addr from the runtime row, api_token from env.
    let view = load_config_view(&store).await.unwrap();
    let mut cfg = Config {
        database_url: "sqlite://x".into(),
        bind_addr: "127.0.0.1:1".parse().unwrap(),
        bearer_token: None,
    };
    cfg.resolve(&view).unwrap();
    assert_eq!(cfg.bind_addr.to_string(), "9.9.9.9:1");
    assert_eq!(cfg.bearer_token.as_deref(), Some("tok-123"));
}

#[tokio::test]
#[serial_test::serial(skadi_env)]
async fn service_fingerprint_reflects_config_changes() {
    let store = Store::connect(&temp_store_url()).unwrap();
    store.run_migrations().await.unwrap();

    let fp0 = service_fingerprint(&store).await.unwrap();

    // SKADI-T-0459: a config key that cannot affect a provider must NOT change
    // the fingerprint. This test used to assert the opposite — writing
    // `bind_addr` bumped it — which is what made the supervisor rebuild every
    // provider and re-apply it to every reloader on any unrelated config write,
    // on a tick that runs every few seconds.
    store
        .set_config("bind_addr", "0.0.0.0:9000", ConfigSource::Runtime)
        .await
        .unwrap();
    assert_eq!(
        service_fingerprint(&store).await.unwrap(),
        fp0,
        "bind_addr cannot change a provider, so it must not force a rebuild"
    );

    // A key the provider set is actually built from does change it.
    store
        .set_config(
            "flaresolverr_url",
            "http://solver:8191",
            ConfigSource::Runtime,
        )
        .await
        .unwrap();
    let fp1 = service_fingerprint(&store).await.unwrap();
    assert_ne!(fp0, fp1, "a provider-relevant config write changes it");

    // Stable when nothing changes.
    assert_eq!(service_fingerprint(&store).await.unwrap(), fp1);
}
