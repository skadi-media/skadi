//! C02 store steps: backend selection, embedded migrations, the settings and
//! domains repositories, JSON/timestamp round-trips and SQLite write concurrency.
use chrono::Utc;
use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use diesel::prelude::*;

use skadi_store::{DomainState, DomainStateRepo, SettingsRepo, Store};

use crate::bdd_support::{Db, World};

/// Every table the embedded skadi-store migrations create.
pub const EXPECTED_TABLES: &[&str] = &[
    "domains",
    "credentials",
    "settings",
    "acquisition_history",
    "downloads",
    "config",
    "blocklist",
    "decision_history",
    "download_categories",
    "worker_status",
    "trace_events",
    // Tag membership on library items (SKADI-T-0550).
    "item_tags",
];

#[derive(QueryableByName)]
struct TableName {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

/// User tables present on the active backend.
pub async fn table_names(store: &Store) -> Vec<String> {
    store
        .with_conn(|conn| {
            let rows: Vec<TableName> = conn
                .dispatch(
                    |pg| {
                        diesel::sql_query(
                            "SELECT table_name AS name FROM information_schema.tables \
                             WHERE table_schema = 'public' ORDER BY table_name",
                        )
                        .load(pg)
                    },
                    |sq| {
                        diesel::sql_query(
                            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
                        )
                        .load(sq)
                    },
                )
                .map_err(|e| skadi_core::AppError::Internal(e.to_string()))?;
            Ok(rows.into_iter().map(|r| r.name).collect())
        })
        .await
        .expect("table listing")
}

// ---- backend selection / migrations ----------------------------------------------

#[given("a fresh sqlite database")]
async fn fresh_sqlite(w: &mut World) {
    w.db = Some(Db::sqlite());
}

#[given("a fresh postgres database")]
async fn fresh_postgres(w: &mut World) {
    let admin = std::env::var("SKADI_TEST_DATABASE_URL").expect("SKADI_TEST_DATABASE_URL");
    w.db = Some(Db::postgres(&admin));
}

#[given(expr = "a migrated {word} store")]
async fn migrated(w: &mut World, backend: String) {
    match backend.as_str() {
        "sqlite" => fresh_sqlite(w).await,
        "postgres" => fresh_postgres(w).await,
        other => panic!("unknown backend {other}"),
    }
    let n = w.store().run_migrations().await.expect("migrations");
    w.applied.push(n);
}

#[when("the embedded migrations are applied")]
async fn apply(w: &mut World) {
    let n = w.store().run_migrations().await.expect("migrations");
    w.applied.push(n);
}

#[when("the embedded migrations are reverted")]
async fn revert(w: &mut World) {
    let n = w.store().revert_migrations().await.expect("revert");
    w.applied.push(n);
}

/// Every embedded migration ran — compared against what is *embedded*, not
/// against a literal in the feature file.
///
/// This used to assert a hard-coded count, which broke on every migration added
/// (twice in two days: SKADI-T-0511 and SKADI-T-0495). That failure taught
/// nothing except that someone had not updated the number, and a scenario whose
/// only failure mode is bookkeeping trains you to bump it without reading it.
/// The property is "all of them ran".
#[then("every embedded migration was applied")]
async fn applied_all(w: &mut World) {
    let embedded = std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/generated/migrations-sqlite"),
    )
    .expect("the generated sqlite migration dir exists")
    .filter_map(std::result::Result::ok)
    .filter(|e| e.path().is_dir())
    .count();
    assert!(embedded > 0, "no embedded migrations found on disk");
    assert_eq!(
        w.applied.first().copied(),
        Some(embedded),
        "a fresh database must apply every embedded migration; applied = {:?}, on disk = {embedded}",
        w.applied
    );
}

#[then(expr = "{int} migration(s) were applied")]
async fn applied_n(w: &mut World, n: usize) {
    assert_eq!(
        w.applied.last().copied(),
        Some(n),
        "applied = {:?}",
        w.applied
    );
}

#[then("the second application applied no migrations")]
async fn idempotent(w: &mut World) {
    assert!(w.applied.len() >= 2, "applied = {:?}", w.applied);
    assert_eq!(w.applied[1], 0, "applied = {:?}", w.applied);
}

#[then("the revert and the re-apply moved the same number of migrations")]
async fn revert_symmetry(w: &mut World) {
    let n = w.applied.len();
    assert!(n >= 3, "applied = {:?}", w.applied);
    assert!(w.applied[n - 2] >= 1);
    assert_eq!(
        w.applied[n - 2],
        w.applied[n - 1],
        "applied = {:?}",
        w.applied
    );
}

#[then("every skadi-store table exists")]
async fn tables_exist(w: &mut World) {
    let names = table_names(&w.store()).await;
    for t in EXPECTED_TABLES {
        assert!(
            names.contains(&t.to_string()),
            "missing table {t}; have {names:?}"
        );
    }
}

#[then("no skadi-store table remains")]
async fn tables_gone(w: &mut World) {
    let names = table_names(&w.store()).await;
    for t in EXPECTED_TABLES {
        assert!(
            !names.contains(&t.to_string()),
            "table {t} survived the revert"
        );
    }
}

#[then(expr = "the store reports backend {string}")]
async fn backend_is(w: &mut World, name: String) {
    assert_eq!(w.store().backend_name(), name);
}

#[then(expr = "connecting to {string} is rejected as a configuration error")]
async fn bad_scheme(_w: &mut World, url: String) {
    match Store::connect(&url) {
        Err(skadi_core::AppError::Config(msg)) => {
            assert!(msg.contains("unsupported"), "{msg}");
        }
        other => panic!("expected Config error, got {:?}", other.map(|_| "store")),
    }
}

#[then(expr = "connecting to {string} selects backend {string} without touching the network")]
async fn lazy_connect(_w: &mut World, url: String, backend: String) {
    let store = Store::connect(&url).expect("lazy connect");
    assert_eq!(store.backend_name(), backend);
    assert_eq!(store.url(), url);
}

// ---- settings repo -----------------------------------------------------------------

#[when(expr = "the {word} setting {string} is stored with body:")]
async fn put_setting(w: &mut World, step: &Step, kind: String, id: String) {
    let body: serde_json::Value =
        serde_json::from_str(&step.docstring().cloned().unwrap_or_default()).expect("JSON");
    let rec = w
        .store()
        .put_setting(&kind, &id, &body)
        .await
        .expect("put_setting");
    w.stamps
        .insert(format!("{kind}/{id}.created"), rec.created_at);
    w.stamps
        .insert(format!("{kind}/{id}.updated"), rec.updated_at);
}

#[when(expr = "the {word} setting {string} is stored again with body:")]
async fn put_again(w: &mut World, step: &Step, kind: String, id: String) {
    // A visible gap so `updated_at` provably moves on both backends.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let body: serde_json::Value =
        serde_json::from_str(&step.docstring().cloned().unwrap_or_default()).expect("JSON");
    let before_created = w.stamps[&format!("{kind}/{id}.created")];
    let before_updated = w.stamps[&format!("{kind}/{id}.updated")];
    let rec = w
        .store()
        .put_setting(&kind, &id, &body)
        .await
        .expect("put_setting");
    assert_eq!(rec.created_at, before_created, "created_at is preserved");
    assert!(rec.updated_at > before_updated, "updated_at is stamped");
    w.stamps
        .insert(format!("{kind}/{id}.updated"), rec.updated_at);
}

#[then(expr = "the {word} kind holds exactly {int} setting(s)")]
async fn kind_count(w: &mut World, kind: String, n: usize) {
    assert_eq!(w.store().list_settings(&kind).await.unwrap().len(), n);
}

#[then(expr = "the {word} setting {string} has body:")]
async fn setting_body(w: &mut World, step: &Step, kind: String, id: String) {
    let want: serde_json::Value =
        serde_json::from_str(&step.docstring().cloned().unwrap_or_default()).expect("JSON");
    let rec = w
        .store()
        .get_setting(&kind, &id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{kind}/{id} missing"));
    assert_eq!(rec.body, want);
}

#[then(expr = "the {word} settings list in id order {string}")]
async fn list_order(w: &mut World, kind: String, ids: String) {
    let want: Vec<&str> = ids.split(',').map(str::trim).collect();
    let got: Vec<String> = w
        .store()
        .list_settings(&kind)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(got, want);
}

#[when(expr = "the {word} setting {string} is deleted")]
async fn delete_setting(w: &mut World, kind: String, id: String) {
    let existed = w.store().delete_setting(&kind, &id).await.unwrap();
    w.notes.push(format!("deleted:{existed}"));
}

#[then(expr = "the delete reported {word}")]
async fn delete_reported(w: &mut World, existed: String) {
    assert_eq!(
        w.notes.last().map(String::as_str),
        Some(format!("deleted:{existed}").as_str())
    );
}

#[then(expr = "the stored timestamps of {word} {string} are UTC and at least microsecond-precise")]
async fn timestamp_precision(w: &mut World, kind: String, id: String) {
    let rec = w
        .store()
        .get_setting(&kind, &id)
        .await
        .unwrap()
        .expect("row");
    let created = w.stamps[&format!("{kind}/{id}.created")];
    // Postgres stores timestamptz at microsecond precision; SQLite keeps the
    // RFC3339 text with nanoseconds. Both must agree to the microsecond.
    assert_eq!(
        rec.created_at.timestamp_micros(),
        created.timestamp_micros(),
        "created_at drifted beyond microseconds on {}",
        w.db.as_ref().unwrap().backend
    );
    assert!(rec.updated_at >= rec.created_at);
    assert!((Utc::now() - rec.created_at).num_seconds() < 60);
}

// ---- domains repo --------------------------------------------------------------------

#[when(expr = "the domain {string} is registered with settings:")]
async fn upsert_domain(w: &mut World, step: &Step, name: String) {
    let settings: serde_json::Value =
        serde_json::from_str(&step.docstring().cloned().unwrap_or_default()).expect("JSON");
    let mut d = DomainState::new(&name);
    d.settings = settings;
    w.store().upsert(&d).await.expect("upsert");
}

#[when(expr = "the domain {string} is {word}")]
async fn toggle_domain(w: &mut World, name: String, what: String) {
    let enabled = match what.as_str() {
        "enabled" => true,
        "disabled" => false,
        other => panic!("expected enabled|disabled, got {other}"),
    };
    let s = w
        .store()
        .set_enabled(&name, enabled)
        .await
        .expect("set_enabled");
    assert_eq!(s.enabled, enabled);
    assert_eq!(
        s.enabled_at.is_some(),
        enabled,
        "enabled_at stamped only while enabled"
    );
}

#[then(expr = "the domain {string} is {word} with its settings intact")]
async fn domain_state(w: &mut World, name: String, what: String) {
    let s = w.store().get(&name).await.unwrap().expect("domain row");
    assert_eq!(s.enabled, what == "enabled");
    assert_eq!(s.enabled_at.is_some(), what == "enabled");
    assert_eq!(s.settings["root"], serde_json::json!("/movies"));
}

#[then(expr = "the domains list in name order {string}")]
async fn domains_order(w: &mut World, names: String) {
    let want: Vec<&str> = names.split(',').map(str::trim).collect();
    let got: Vec<String> = w
        .store()
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(got, want);
}

// ---- concurrency ----------------------------------------------------------------------

#[when(expr = "{int} writers upsert distinct config keys at the same time")]
async fn concurrent_writes(w: &mut World, n: usize) {
    use skadi_store::{ConfigRepo, ConfigSource};
    let store = w.store();
    let mut handles = Vec::new();
    for i in 0..n {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.set_config(
                &format!("bdd.key{i}"),
                &i.to_string(),
                ConfigSource::Runtime,
            )
            .await
            .map(|_| ())
        }));
    }
    let mut errors = Vec::new();
    for h in handles {
        if let Err(e) = h.await.expect("task") {
            errors.push(e.to_string());
        }
    }
    w.last_err = (!errors.is_empty()).then(|| errors.join(" | "));
}

#[then(expr = "every write succeeded and {int} keys are stored")]
async fn all_written(w: &mut World, n: usize) {
    use skadi_store::ConfigRepo;
    assert_eq!(w.last_err, None, "some concurrent writes failed");
    let keys = w
        .store()
        .list_config()
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.key.starts_with("bdd.key"))
        .count();
    assert_eq!(keys, n);
}
