//! Shared integration-test harness for Skadi (SKADI-T-0077).
//!
//! [`TestDb`] gives each test a fully isolated database with the skadi-store
//! schema plus the caller's domain migrations already applied, on whichever
//! backend the environment selects:
//!
//! - **Postgres** (when `SKADI_TEST_DATABASE_URL` is set, e.g. the compose
//!   service `postgres://skadi:skadi@127.0.0.1:5433/skadi`): creates a unique
//!   throwaway database `skadi_test_<uuid>`, migrates it, and `DROP DATABASE …
//!   WITH (FORCE)` on teardown. Because each test owns its own database, the
//!   suite runs in parallel with no cross-test state — and the Cloacina runner a
//!   test builds from [`TestDb::url`] lands its hunter schema *inside* that same
//!   database, so it's torn down too.
//! - **SQLite** (the default when the env var is unset): a per-test tempfile,
//!   preserving coverage of the backend Skadi still ships.
//!
//! This sidesteps SQLite's single-writer lock, which flaked the movies HTTP
//! integration tests under parallelism (each builds a Cloacina runner): on
//! Postgres there is no global write lock to contend on.
//!
//! ```ignore
//! use skadi_testsupport::TestDb;
//! // Caller passes both backends' domain migrations; the harness applies the
//! // one matching the selected backend.
//! let db = TestDb::new(
//!     skadi_movies::SQLITE_MIGRATIONS,
//!     skadi_movies::POSTGRES_MIGRATIONS,
//! )
//! .await;
//! let store = db.store.clone();
//! // build a Cloacina runner against db.url(), run the test, …
//! ```

use diesel::RunQueryDsl;
use diesel::connection::Connection;
use diesel::pg::PgConnection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness};
use skadi_store::Store;

/// The environment variable that, when set to a `postgres://` URL, switches the
/// harness from the SQLite default onto a per-test Postgres database created on
/// that server.
pub const TEST_DATABASE_URL_ENV: &str = "SKADI_TEST_DATABASE_URL";

/// A migrated, isolated database for one test. Drop tears it down: the temp
/// SQLite file is removed with the [`tempfile::TempDir`], the throwaway Postgres
/// database is dropped on the server.
pub struct TestDb {
    /// A live store connected to the isolated database.
    pub store: Store,
    /// The connection URL of the isolated database (hand this to
    /// `skadi_hunter::build_runner` to get a runner scoped to this test).
    pub url: String,
    backend: Backend,
}

enum Backend {
    /// Keep the tempdir alive for the test's lifetime; dropping it removes the
    /// SQLite file.
    Sqlite(#[allow(dead_code)] tempfile::TempDir),
    /// Admin URL (a database we are *not* connected to via `store`) + the name
    /// of the throwaway database to drop on teardown.
    Postgres { admin_url: String, db_name: String },
}

impl TestDb {
    /// Create an isolated, migrated database for the active backend.
    ///
    /// `sqlite_migrations` / `pg_migrations` are the caller's domain migration
    /// sets (e.g. `skadi_movies::SQLITE_MIGRATIONS` / `POSTGRES_MIGRATIONS`);
    /// the one matching the selected backend is applied after skadi-store's.
    ///
    /// Must run inside a Tokio runtime (builds a `bb8` pool). Panics on setup
    /// failure — these are tests; a failure to provision is a hard error.
    pub async fn new(
        sqlite_migrations: EmbeddedMigrations,
        pg_migrations: EmbeddedMigrations,
    ) -> Self {
        Self::new_inner(Some(sqlite_migrations), Some(pg_migrations)).await
    }

    /// Like [`TestDb::new`] but applies only skadi-store's own schema — for
    /// tests that touch the store/control-plane tables without any domain
    /// tables (e.g. domain enable/disable, bootstrap).
    pub async fn new_store_only() -> Self {
        Self::new_inner(None, None).await
    }

    async fn new_inner(
        sqlite_migrations: Option<EmbeddedMigrations>,
        pg_migrations: Option<EmbeddedMigrations>,
    ) -> Self {
        match std::env::var(TEST_DATABASE_URL_ENV) {
            Ok(admin_url)
                if admin_url.starts_with("postgres://")
                    || admin_url.starts_with("postgresql://") =>
            {
                Self::new_postgres(&admin_url, pg_migrations).await
            }
            _ => Self::new_sqlite(sqlite_migrations).await,
        }
    }

    /// The backend name for diagnostics: `"sqlite"` or `"postgres"`.
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Sqlite(_) => "sqlite",
            Backend::Postgres { .. } => "postgres",
        }
    }

    /// The isolated database URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    async fn new_sqlite(domain_migrations: Option<EmbeddedMigrations>) -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("skadi.db");
        let url = format!("sqlite://{}", path.display());

        let store = Store::connect(&url).expect("connect sqlite store");
        store
            .run_migrations()
            .await
            .expect("skadi-store migrations");

        if let Some(domain_migrations) = domain_migrations {
            let path_str = path.display().to_string();
            tokio::task::spawn_blocking(move || {
                let mut conn = SqliteConnection::establish(&path_str).expect("open sqlite db");
                conn.run_pending_migrations(domain_migrations)
                    .expect("domain sqlite migrations");
            })
            .await
            .expect("sqlite migration task");
        }

        // Fresh store for the test (the migration connection above is dropped).
        let store = Store::connect(&url).expect("reconnect sqlite store");
        Self {
            store,
            url,
            backend: Backend::Sqlite(dir),
        }
    }

    async fn new_postgres(admin_url: &str, domain_migrations: Option<EmbeddedMigrations>) -> Self {
        let db_name = format!("skadi_test_{}", uuid::Uuid::new_v4().simple());
        let test_url = with_database(admin_url, &db_name);

        // CREATE DATABASE cannot run in a transaction; diesel executes raw
        // statements in autocommit, so this is fine on the admin connection.
        // We also ensure the shared `cloacina` database exists: on Postgres the
        // Cloacina runner (built by tests from `url()`) always connects to a
        // dedicated `cloacina` database, isolating its tables in the
        // `cloacina_hunter` schema. It's shared across tests and never dropped
        // here — tests that build a runner must not depend on it being empty.
        {
            let admin = admin_url.to_string();
            let name = db_name.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn = PgConnection::establish(&admin)
                    .expect("connect to admin postgres for CREATE DATABASE");
                create_database(&mut conn, &name);
                create_database(&mut conn, "cloacina");
            })
            .await
            .expect("create-database task");
        }

        let store = Store::connect(&test_url).expect("connect pg store");
        store
            .run_migrations()
            .await
            .expect("skadi-store migrations");

        if let Some(domain_migrations) = domain_migrations {
            let url = test_url.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn =
                    PgConnection::establish(&url).expect("connect to test db for migrations");
                conn.run_pending_migrations(domain_migrations)
                    .expect("domain postgres migrations");
            })
            .await
            .expect("pg migration task");
        }

        let store = Store::connect(&test_url).expect("reconnect pg store");
        Self {
            store,
            url: test_url,
            backend: Backend::Postgres {
                admin_url: admin_url.to_string(),
                db_name,
            },
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // SQLite teardown is the TempDir's own Drop (removes the file). For
        // Postgres we must drop the throwaway database from the admin
        // connection. The hazard: the store's bb8 pool keeps idle connections
        // to this database and re-establishes them after a plain terminate, so
        // a naive DROP races and intermittently fails. Sequence it: first
        // revoke new connections, then terminate the existing ones, then drop
        // (retrying while the pool's stragglers wind down).
        if let Backend::Postgres { admin_url, db_name } = &self.backend {
            // Best-effort: never panic in Drop (a panic mid-unwind aborts).
            let Ok(mut conn) = PgConnection::establish(admin_url) else {
                return;
            };
            let _ = diesel::sql_query(format!(
                "UPDATE pg_database SET datallowconn = false WHERE datname = '{db_name}'"
            ))
            .execute(&mut conn);
            for attempt in 0u64..20 {
                let _ = diesel::sql_query(format!(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = '{db_name}' AND pid <> pg_backend_pid()"
                ))
                .execute(&mut conn);
                match diesel::sql_query(format!("DROP DATABASE IF EXISTS {db_name}"))
                    .execute(&mut conn)
                {
                    Ok(_) => return,
                    Err(e) if e.to_string().contains("being accessed by other users") => {
                        std::thread::sleep(std::time::Duration::from_millis(25 * (attempt + 1)));
                    }
                    Err(_) => return, // unexpected; leave it for the next `db down`
                }
            }
        }
    }
}

/// `CREATE DATABASE name`, idempotent and tolerant of parallel creators.
///
/// Two hazards under parallelism: the database may already exist (a shared one
/// like `cloacina`), and concurrent `CREATE DATABASE` statements contend on the
/// `template1` source and error rather than block ("source database … is being
/// accessed by other users"). Treat "already exists" as success and retry the
/// template contention a few times with a short backoff.
fn create_database(conn: &mut PgConnection, name: &str) {
    const ATTEMPTS: u32 = 10;
    for attempt in 0..ATTEMPTS {
        match diesel::sql_query(format!("CREATE DATABASE {name}")).execute(conn) {
            Ok(_) => return,
            // Already present — either the friendly message, or the unique-index
            // violation when two creators of the *same* name race past their
            // existence checks (the shared `cloacina` database).
            Err(e)
                if e.to_string().contains("already exists")
                    || e.to_string().contains("pg_database_datname_index") =>
            {
                return;
            }
            Err(e)
                if e.to_string().contains("being accessed by other users")
                    && attempt + 1 < ATTEMPTS =>
            {
                // Backoff scaled by attempt; another creator is cloning template1.
                std::thread::sleep(std::time::Duration::from_millis(
                    50 * u64::from(attempt + 1),
                ));
            }
            Err(e) => panic!("CREATE DATABASE {name}: {e} (does the role have CREATEDB?)"),
        }
    }
}

/// Rewrite a `postgres://…/<db>[?query]` URL to point at database `db_name`,
/// preserving credentials, host, port, and any query string.
fn with_database(url: &str, db_name: &str) -> String {
    // Split off a query string if present.
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    // Replace the path component (everything after the last '/').
    let rebuilt = match base.rsplit_once('/') {
        Some((prefix, _db)) => format!("{prefix}/{db_name}"),
        None => format!("{base}/{db_name}"),
    };
    match query {
        Some(q) => format!("{rebuilt}?{q}"),
        None => rebuilt,
    }
}

#[cfg(test)]
mod tests {
    use super::with_database;

    #[test]
    fn rewrites_database_name() {
        assert_eq!(
            with_database(
                "postgres://skadi:skadi@127.0.0.1:5433/skadi",
                "skadi_test_abc"
            ),
            "postgres://skadi:skadi@127.0.0.1:5433/skadi_test_abc"
        );
    }

    #[test]
    fn preserves_query_string() {
        assert_eq!(
            with_database(
                "postgres://u:p@host:5432/skadi?sslmode=disable",
                "skadi_test_x"
            ),
            "postgres://u:p@host:5432/skadi_test_x?sslmode=disable"
        );
    }
}
