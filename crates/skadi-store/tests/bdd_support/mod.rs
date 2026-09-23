//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
//!
//! Each scenario gets its own isolated database: a SQLite tempfile by default,
//! or — for scenarios tagged `@postgres` when `SKADI_TEST_DATABASE_URL` is set —
//! a throwaway `skadi_bdd_<uuid>` database on that server, dropped when the
//! world is dropped.
pub mod steps;

use std::path::PathBuf;

use skadi_store::Store;

/// `Store` has no `Debug`; the world needs one.
pub struct Db {
    pub store: Store,
    pub url: String,
    pub backend: &'static str,
    /// SQLite: the tempfile removed on drop.
    sqlite_path: Option<PathBuf>,
    /// Postgres: admin URL + throwaway database name dropped on drop.
    pg: Option<(String, String)>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Db({})", self.backend)
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Some(p) = &self.sqlite_path {
            let _ = std::fs::remove_file(p);
        }
        if let Some((admin, name)) = &self.pg {
            use diesel::{Connection, RunQueryDsl};
            if let Ok(mut conn) = diesel::PgConnection::establish(admin) {
                let _ = diesel::sql_query(format!("DROP DATABASE \"{name}\" WITH (FORCE)"))
                    .execute(&mut conn);
            }
        }
    }
}

impl Db {
    pub fn sqlite() -> Self {
        let path = skadi_core::unique_temp_path("store-bdd").with_extension("db");
        let url = format!("sqlite://{}", path.display());
        Db {
            store: Store::connect(&url).expect("connect sqlite"),
            url,
            backend: "sqlite",
            sqlite_path: Some(path),
            pg: None,
        }
    }

    /// A throwaway database on the `SKADI_TEST_DATABASE_URL` server.
    pub fn postgres(admin_url: &str) -> Self {
        use diesel::{Connection, RunQueryDsl};
        let name = format!("skadi_bdd_{}", uuid::Uuid::new_v4().simple());
        let mut conn = diesel::PgConnection::establish(admin_url).expect("connect postgres");
        diesel::sql_query(format!("CREATE DATABASE \"{name}\""))
            .execute(&mut conn)
            .expect("create throwaway database");
        let url = swap_database(admin_url, &name);
        Db {
            store: Store::connect(&url).expect("connect postgres"),
            url,
            backend: "postgres",
            sqlite_path: None,
            pg: Some((admin_url.to_string(), name)),
        }
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub db: Option<Db>,
    /// Extra stores opened against the same database (e.g. under another key).
    pub other: Option<Db>,
    /// Migration counts observed, in order.
    pub applied: Vec<usize>,
    /// The last operation's error text, if it failed.
    pub last_err: Option<String>,
    /// The last fingerprint(s) observed, in order.
    pub fingerprints: Vec<u64>,
    /// Remembered ids by name.
    pub ids: std::collections::HashMap<String, String>,
    /// Remembered timestamps by name.
    pub stamps: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>,
    /// The last secret read back (`None` = absent).
    pub secret: Option<Option<String>>,
    /// Env vars this scenario set, restored afterwards (`@serial` only).
    pub env_touched: Vec<(String, Option<String>)>,
    /// The last count returned by a purge/trim.
    pub purged: Option<usize>,
}

impl World {
    pub fn store(&self) -> Store {
        self.db
            .as_ref()
            .expect("a migrated store (Given …)")
            .store
            .clone()
    }

    pub fn restore_env(&mut self) {
        for (key, prior) in self.env_touched.drain(..) {
            // SAFETY: `@serial` scenario; nothing else reads env concurrently.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }
}

/// `postgres://u:p@host:port/db?x=y` → the same URL pointing at `name`.
fn swap_database(admin_url: &str, name: &str) -> String {
    let (scheme, rest) = admin_url.split_once("://").expect("scheme");
    let (authority, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let query = tail
        .split_once('?')
        .map(|(_, q)| format!("?{q}"))
        .unwrap_or_default();
    format!("{scheme}://{authority}/{name}{query}")
}
