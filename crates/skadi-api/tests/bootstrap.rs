//! Bootstrap integration tests (SKADI-T-0051).
//!
//! Drives `skadi_api::bootstrap` against a real database and asserts both
//! `skadi-store`'s schema and a domain's schema land, and that re-running is a
//! no-op. SQLite always runs; the Postgres counterpart is gated on
//! `SKADI_TEST_DATABASE_URL` (set by `angreal db test`).
//!
//! The "domain" here is a self-contained probe module embedding its own tiny
//! migration set (`test_migrations/`). `skadi-api` sits below the real domain
//! crates in the dependency stack and must not depend on them; the genuine
//! movies schema is covered by the binary-level e2e (SKADI-T-0057).

use std::sync::Arc;

use diesel::Connection;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, embed_migrations};

use skadi_api::{Config, bootstrap};
use skadi_core::{BoxedWorker, DomainModule, MediaKind};

const PROBE_SQLITE: EmbeddedMigrations = embed_migrations!("test_migrations/sqlite");
const PROBE_POSTGRES: EmbeddedMigrations = embed_migrations!("test_migrations/postgres");

/// A minimal domain module contributing a single `bootstrap_probe` table, used
/// to prove bootstrap applies *domain* migrations (not just skadi-store's).
struct ProbeModule;

impl DomainModule for ProbeModule {
    fn name(&self) -> &'static str {
        "probe"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    fn sqlite_migrations(&self) -> EmbeddedMigrations {
        PROBE_SQLITE
    }
    fn postgres_migrations(&self) -> EmbeddedMigrations {
        PROBE_POSTGRES
    }
    fn workers(&self) -> Vec<BoxedWorker> {
        Vec::new()
    }
}

#[derive(QueryableByName)]
struct TableName {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

fn list_sqlite_tables(path: &str) -> Vec<String> {
    let mut conn = SqliteConnection::establish(path).unwrap();
    diesel::sql_query("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .load::<TableName>(&mut conn)
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect()
}

fn sqlite_config(db: &std::path::Path) -> Config {
    Config {
        database_url: format!("sqlite://{}", db.display()),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    }
}

#[tokio::test]
async fn bootstrap_applies_store_and_domain_schema_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("skadi.db");
    let config = sqlite_config(&db);
    let registry: Vec<Arc<dyn DomainModule>> = vec![Arc::new(ProbeModule)];

    bootstrap(&config, &registry).await.expect("bootstrap");

    let tables = list_sqlite_tables(&db.display().to_string());

    // The domain's migration ran.
    assert!(
        tables.iter().any(|t| t == "bootstrap_probe"),
        "domain table absent; have {tables:?}"
    );
    // Migrations bookkeeping table is present.
    assert!(
        tables.iter().any(|t| t == "__diesel_schema_migrations"),
        "migrations bookkeeping table absent; have {tables:?}"
    );
    // skadi-store contributed tables of its own beyond the probe + bookkeeping.
    let store_tables: Vec<&String> = tables
        .iter()
        .filter(|t| *t != "bootstrap_probe" && *t != "__diesel_schema_migrations")
        .collect();
    assert!(
        !store_tables.is_empty(),
        "expected skadi-store tables; have {tables:?}"
    );
}

#[tokio::test]
async fn bootstrap_is_idempotent_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("skadi.db");
    let config = sqlite_config(&db);
    let registry: Vec<Arc<dyn DomainModule>> = vec![Arc::new(ProbeModule)];

    bootstrap(&config, &registry)
        .await
        .expect("first bootstrap");
    let before = list_sqlite_tables(&db.display().to_string());
    bootstrap(&config, &registry)
        .await
        .expect("second bootstrap");
    let after = list_sqlite_tables(&db.display().to_string());

    assert_eq!(before, after, "bootstrap was not idempotent");
}

// --- Postgres path (gated) ---

fn pg_url() -> Option<String> {
    std::env::var("SKADI_TEST_DATABASE_URL").ok()
}

#[tokio::test]
async fn bootstrap_creates_cloacina_db_and_domain_schema_on_postgres() {
    let Some(url) = pg_url() else {
        eprintln!("SKADI_TEST_DATABASE_URL unset — skipping Postgres bootstrap test");
        return;
    };
    let config = Config {
        database_url: url.clone(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let registry: Vec<Arc<dyn DomainModule>> = vec![Arc::new(ProbeModule)];

    // Idempotent across two runs (also proves CREATE DATABASE tolerates an
    // already-existing cloacina database).
    bootstrap(&config, &registry)
        .await
        .expect("first bootstrap");
    bootstrap(&config, &registry)
        .await
        .expect("second bootstrap");

    let mut conn =
        diesel::pg::PgConnection::establish(&url).expect("connect to assert cloacina db");

    #[derive(QueryableByName)]
    struct Exists {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }
    let rows = diesel::sql_query(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = 'cloacina') AS present",
    )
    .load::<Exists>(&mut conn)
    .expect("query pg_database");
    assert!(rows[0].present, "cloacina database was not created");

    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let rows = diesel::sql_query(
        "SELECT count(*) AS n FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'bootstrap_probe'",
    )
    .load::<Count>(&mut conn)
    .expect("query information_schema");
    assert_eq!(rows[0].n, 1, "probe table missing from public schema");
}

/// Default custom formats reach an **existing** install, without resurrecting
/// what the operator removed (SKADI-T-0584).
///
/// The original guard ran the whole preset pass once per database, so the default
/// format set could never grow: an install seeded with the original four would
/// never receive a later default — and the direct-play formats added for the
/// re-grab work are exactly the ones that stop the library re-acquiring an
/// unplayable release.
///
/// The fix keys on ids the database has been *offered*, not ids that are
/// *present*, which is what keeps the two cases apart.
#[tokio::test]
async fn a_new_default_format_reaches_an_existing_install() {
    use skadi_store::{ConfigRepo, ConfigSource, SettingsRepo};

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("skadi.db");
    let config = sqlite_config(&db);
    let registry: Vec<Arc<dyn DomainModule>> = vec![Arc::new(ProbeModule)];

    bootstrap(&config, &registry).await.expect("first boot");
    let store = skadi_store::Store::connect(&config.database_url).expect("store");

    let all: Vec<String> = skadi_quality::default_formats()
        .iter()
        .map(|f| f.id.to_string())
        .collect();
    assert!(
        all.len() >= 2,
        "need at least two defaults to tell these apart"
    );

    // Rewind to an "older install": it has only the first default, and has only
    // ever been offered that one. The rest are new to it.
    for id in &all[1..] {
        store.delete_setting("custom_formats", id).await.unwrap();
    }
    store
        .set_config(
            "bootstrap.default_formats_seen",
            &all[0],
            ConfigSource::Runtime,
        )
        .await
        .unwrap();

    bootstrap(&config, &registry).await.expect("second boot");

    let after: Vec<String> = store
        .list_settings("custom_formats")
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    for id in &all[1..] {
        assert!(
            after.contains(id),
            "a default this install had never been offered must arrive"
        );
    }

    // The other half, and the reason this is a seen-set rather than a diff: a
    // default the operator DELETED must stay deleted across restarts.
    let deleted = all[0].clone();
    store
        .delete_setting("custom_formats", &deleted)
        .await
        .unwrap();
    bootstrap(&config, &registry).await.expect("third boot");
    let final_ids: Vec<String> = store
        .list_settings("custom_formats")
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        !final_ids.contains(&deleted),
        "a format the operator removed must not come back on the next restart"
    );
}
