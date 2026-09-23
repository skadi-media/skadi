//! `skadi-store` — the single persistence surface for Skadi.
//!
//! One [`Store`] wraps a [`diesel_dualdb::Pool`] for whichever backend the URL
//! selects: SQLite (instant, zero-config first run) or Postgres (serious
//! deployments). Queries are written **once** against a [`DualConnection`] and
//! run on a blocking pool thread via [`Store::with_conn`] (`spawn_blocking`), so
//! the repository traits keep their `async` surface while the bodies are plain
//! synchronous diesel (SKADI-I-0015, diesel-dualdb).
//!
//! Migrations for every backend are embedded in the binary (generated from the
//! logical DDL in `schema/migrations/`) and run unconditionally at startup via
//! [`Store::run_migrations`]. Repository traits are defined in this crate,
//! implemented here and in domain crates over [`Store::with_conn`].

use std::sync::Arc;

use diesel::Connection as _;
use diesel::sqlite::SqliteConnection;
use diesel_dualdb::Pool;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

use skadi_core::{AppError, Result};

mod blocklist;
mod categories;
mod config;
mod credentials;
pub mod crypto;
mod decision_history;
mod downloads;
mod history;
pub mod import_lists;
mod item_tags;
mod repo;
mod schema;
mod settings;
mod trace;
mod worker_status;

pub use blocklist::{BlocklistEntry, BlocklistRepo, NewBlocklistEntry};
pub use categories::{DownloadCategory, DownloadCategoryRepo};
pub use config::{ConfigEntry, ConfigRepo, ConfigSource};
pub use credentials::{CredentialRepo, SealedSecret};
pub use crypto::Cipher;
pub use decision_history::{DecisionEntry, DecisionHistoryRepo};
pub use diesel_dualdb::DualConnection;
pub use downloads::{
    DownloadJob, DownloadJobRepo, DownloadJobStatus, DownloadProgress, NewDownloadJob,
};
pub use history::{HistoryCounts, HistoryEntry, HistoryQuery, HistoryRepo};
pub use import_lists::{ImportList, ImportListExclusion, ImportListRepo};
pub use item_tags::ItemTagRepo;
pub use repo::{DomainState, DomainStateRepo};
pub use settings::{SettingRecord, SettingsRepo};
pub use trace::{TraceEvent, TraceRepo};
pub use worker_status::{WorkerStatus, WorkerStatusRepo};

/// Default database URL when `SKADI_DATABASE_URL` is unset.
pub const DEFAULT_DATABASE_URL: &str = "sqlite://./skadi.db";

/// Embedded SQLite migrations, generated from the logical DDL (SKADI-I-0015).
pub const SQLITE_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-sqlite");
/// Embedded Postgres migrations, generated from the logical DDL (SKADI-I-0015).
pub const POSTGRES_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-postgres");

/// Map a diesel/query error to an [`AppError`], **classified** (SKADI-T-0526).
///
/// Everything used to become `AppError::Internal`, so a caller sending a
/// duplicate id, a query that legitimately matched nothing, and the database
/// being down were one indistinguishable 500. That is wrong three ways: the
/// client cannot tell its own mistake from ours, an outage looks like a bug, and
/// the logs give an operator nothing to triage on.
///
/// Three classes, chosen by what the *caller* should do about it:
/// * **The caller's input is bad** — a unique/foreign-key/not-null/check
///   violation. `Validation`, so it answers 400 rather than 500.
/// * **Nothing matched** — diesel's `NotFound`. `AppError::NotFound` → 404.
///   Reachable only where a query does not use `.optional()`.
/// * **The database is unreachable** — a closed connection. `Network`, so it
///   answers 502 rather than 500: retrying may work, which is not true of a bug.
///
/// Anything else stays `Internal`. Guessing a friendlier class for an error we
/// have not actually characterised would just move the confusion.
pub(crate) fn db_err(e: diesel::result::Error) -> AppError {
    use diesel::result::{DatabaseErrorKind, Error};
    match &e {
        Error::NotFound => AppError::NotFound("no matching row".into()),
        Error::DatabaseError(kind, info) => match kind {
            DatabaseErrorKind::UniqueViolation
            | DatabaseErrorKind::ForeignKeyViolation
            | DatabaseErrorKind::NotNullViolation
            | DatabaseErrorKind::CheckViolation => {
                AppError::Validation(format!("constraint violation: {}", info.message()))
            }
            DatabaseErrorKind::ClosedConnection => {
                AppError::Network(format!("database unreachable: {}", info.message()))
            }
            _ => AppError::Internal(format!("database error: {e}")),
        },
        _ => AppError::Internal(format!("database error: {e}")),
    }
}

/// Map a connection-pool checkout error to an [`AppError`] (SKADI-T-0526).
///
/// `Network`, not `Internal`: failing to check out a connection means the
/// database is unreachable or the pool is exhausted. Both are conditions where
/// retrying is reasonable and where the fix is operational, not a code change —
/// 502 says that; 500 says "we have a bug".
pub(crate) fn pool_err(e: impl std::fmt::Display) -> AppError {
    AppError::Network(format!("connection pool error: {e}"))
}

/// A backend-agnostic handle to the database.
///
/// Cheap to [`Clone`] (the pool is reference-counted), so it can be shared
/// freely into the daemon context and request handlers.
#[derive(Clone)]
pub struct Store {
    pool: Pool,
    url: Arc<str>,
    /// Optional credential cipher, derived from `SKADI_SECRET_KEY` at connect
    /// time. `None` ⇒ secrets are stored as plaintext.
    cipher: Option<crypto::Cipher>,
}

impl Store {
    /// Build a [`Store`] from `SKADI_DATABASE_URL` (defaulting to
    /// [`DEFAULT_DATABASE_URL`]).
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("SKADI_DATABASE_URL")
            .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
        Self::connect(&url)
    }

    /// Build a [`Store`] from an explicit connection URL, selecting the backend
    /// by scheme. The pool is built **lazily** (`connect_lazy`), so this does
    /// not fail just because the database is not yet reachable — connection
    /// errors surface on first use.
    pub fn connect(url: &str) -> Result<Self> {
        // Bound the pool and cap how long a checkout may wait: a contended pool
        // must surface a fast error, never block a handler forever (SKADI-T-0081).
        const POOL_MAX: u32 = 16;
        let pool_timeout = std::time::Duration::from_secs(30);

        if !(url.starts_with("postgres://")
            || url.starts_with("postgresql://")
            || url.starts_with("sqlite://"))
        {
            return Err(AppError::Config(format!(
                "unsupported SKADI_DATABASE_URL scheme: {url:?} (expected sqlite:// or postgres://)"
            )));
        }

        let pool = Pool::builder()
            .max_size(POOL_MAX)
            // Open connections on demand and keep none idle by default. r2d2's
            // default (`min_idle = None`) eagerly maintains `max_size` *idle*
            // connections — far too many when many `Store`s exist at once (e.g.
            // parallel integration tests each spawning a daemon), exhausting the
            // server's connection slots. `Some(0)` restores the lazy behaviour
            // the async pool had.
            .min_idle(Some(0))
            .connection_timeout(pool_timeout)
            .connect_lazy(url)
            .map_err(|e| AppError::Config(format!("build connection pool for {url:?}: {e}")))?;

        // The "no secret key" warning is NOT emitted here (SKADI-T-0520). This
        // runs on every `Store::connect`, and the daemon connects five times at
        // boot, so the operator saw the same line five times; the downloader
        // worker also connects and never touches a credential, so it warned about
        // a key it has no use for. Repeated warnings about a non-problem are how
        // real warnings get skimmed past.
        //
        // `warn_plaintext_credentials` emits it once per process, on the first
        // credential operation that would have used the cipher — which is exactly
        // "the process that needs the key", and only once it actually does.
        let cipher = crypto::Cipher::from_env();
        Ok(Store {
            pool,
            url: Arc::from(url),
            cipher,
        })
    }

    /// Warn — at most once per process — that credentials are being handled
    /// without `SKADI_SECRET_KEY` (SKADI-T-0520).
    ///
    /// Called from the credential seal/unseal paths rather than from
    /// `Store::connect`, so a process that never touches a credential never
    /// mentions the key, and one that does says it once however many `Store`s it
    /// opened.
    pub(crate) fn warn_plaintext_credentials() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            tracing::warn!(
                "{} not set — credentials are stored as plaintext",
                crypto::SECRET_KEY_ENV
            );
        });
    }

    /// The backend name, for logging and diagnostics.
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        if self.url.starts_with("postgres") {
            "postgres"
        } else {
            "sqlite"
        }
    }

    /// The configured database URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Run a synchronous diesel closure on a pooled [`DualConnection`], off the
    /// async runtime via `spawn_blocking`. **This is the seam** (SKADI-I-0015):
    /// the repository traits stay `async`, but each body is plain synchronous
    /// diesel over the dual-backend connection. Domain crates implement their
    /// own repositories the same way.
    pub async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut DualConnection) -> Result<T> + Send + 'static,
    {
        let pool = self.pool.clone();
        let sqlite = self.url.starts_with("sqlite://");
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(pool_err)?;
            if sqlite {
                prepare_sqlite(&mut conn);
            }
            f(&mut conn)
        })
        .await
        .map_err(|e| AppError::Internal(format!("database task panicked: {e}")))?
    }

    /// Apply all pending embedded migrations for the active backend.
    ///
    /// Idempotent: already-applied migrations are skipped. Returns the number
    /// of migrations newly applied. Migrations are inherently synchronous
    /// (`diesel_migrations`), so they run on a fresh synchronous connection
    /// inside `spawn_blocking`.
    pub async fn run_migrations(&self) -> Result<usize> {
        let url = self.url.to_string();
        let is_pg = url.starts_with("postgres");
        tokio::task::spawn_blocking(move || {
            if is_pg {
                run_postgres_migrations(&url)
            } else {
                let path = url.strip_prefix("sqlite://").unwrap_or(&url);
                run_sqlite_migrations(path)
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("migration task panicked: {e}")))?
    }

    /// Revert all applied migrations for the active backend, returning the
    /// number reverted. Primarily for tests and tooling.
    pub async fn revert_migrations(&self) -> Result<usize> {
        let url = self.url.to_string();
        let is_pg = url.starts_with("postgres");
        tokio::task::spawn_blocking(move || {
            if is_pg {
                revert_postgres_migrations(&url)
            } else {
                let path = url.strip_prefix("sqlite://").unwrap_or(&url);
                revert_sqlite_migrations(path)
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("migration task panicked: {e}")))?
    }
}

fn run_sqlite_migrations(path: &str) -> Result<usize> {
    let mut conn = SqliteConnection::establish(path)
        .map_err(|e| AppError::Config(format!("cannot open sqlite database {path:?}: {e}")))?;
    let applied = conn
        .run_pending_migrations(SQLITE_MIGRATIONS)
        .map_err(|e| AppError::Internal(format!("sqlite migrations failed: {e}")))?;
    Ok(applied.len())
}

fn run_postgres_migrations(url: &str) -> Result<usize> {
    let mut conn = diesel::PgConnection::establish(url)
        .map_err(|e| AppError::Config(format!("cannot connect to postgres: {e}")))?;
    let applied = conn
        .run_pending_migrations(POSTGRES_MIGRATIONS)
        .map_err(|e| AppError::Internal(format!("postgres migrations failed: {e}")))?;
    Ok(applied.len())
}

fn revert_sqlite_migrations(path: &str) -> Result<usize> {
    let mut conn = SqliteConnection::establish(path)
        .map_err(|e| AppError::Config(format!("cannot open sqlite database {path:?}: {e}")))?;
    let reverted = conn
        .revert_all_migrations(SQLITE_MIGRATIONS)
        .map_err(|e| AppError::Internal(format!("sqlite migration revert failed: {e}")))?;
    Ok(reverted.len())
}

fn revert_postgres_migrations(url: &str) -> Result<usize> {
    let mut conn = diesel::PgConnection::establish(url)
        .map_err(|e| AppError::Config(format!("cannot connect to postgres: {e}")))?;
    let reverted = conn
        .revert_all_migrations(POSTGRES_MIGRATIONS)
        .map_err(|e| AppError::Internal(format!("postgres migration revert failed: {e}")))?;
    Ok(reverted.len())
}

/// Make a checked-out SQLite connection tolerate concurrency (SKADI-T-0518).
///
/// SQLite's default is to fail a write *immediately* with `SQLITE_BUSY` when
/// another connection holds the write lock, which surfaced as
/// "database error: database is locked" whenever two pooled connections wrote at
/// once. `busy_timeout` makes the loser wait and retry instead; WAL lets readers
/// run while a writer holds the lock. Both are cheap and idempotent, so applying
/// them on checkout is safe even when the connection is reused.
///
/// Postgres needs neither, so callers apply this only for `sqlite://` stores.
/// Failures are ignored deliberately: a pragma that will not apply must not take
/// down the query the caller actually asked for.
fn prepare_sqlite(conn: &mut DualConnection) {
    use diesel::connection::SimpleConnection as _;
    let _ = conn.batch_execute("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_url_selects_sqlite_backend() {
        let store = Store::connect("sqlite://:memory:").unwrap();
        assert_eq!(store.backend_name(), "sqlite");
    }

    #[tokio::test]
    async fn postgres_url_selects_postgres_backend() {
        let store = Store::connect("postgres://user:pw@localhost/skadi").unwrap();
        assert_eq!(store.backend_name(), "postgres");
        let store = Store::connect("postgresql://localhost/skadi").unwrap();
        assert_eq!(store.backend_name(), "postgres");
    }

    #[test]
    fn unknown_scheme_is_a_config_error() {
        let result = Store::connect("mysql://localhost/skadi");
        assert!(matches!(result, Err(AppError::Config(_))));
    }

    #[tokio::test]
    async fn from_env_defaults_to_sqlite() {
        if std::env::var("SKADI_DATABASE_URL").is_err() {
            let store = Store::from_env().unwrap();
            assert_eq!(store.backend_name(), "sqlite");
        }
    }

    #[tokio::test]
    async fn migrations_apply_once_then_are_idempotent() {
        let path = skadi_core::unique_temp_path("migtest").with_extension("db");
        let url = format!("sqlite://{}", path.display());
        let store = Store::connect(&url).unwrap();

        let first = store.run_migrations().await.unwrap();
        assert!(first >= 1, "embedded migrations should apply on a fresh db");

        let second = store.run_migrations().await.unwrap();
        assert_eq!(second, 0, "re-running migrations should be a no-op");
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod err_tests {
    use super::*;
    use diesel::result::{DatabaseErrorKind, Error};

    /// A minimal `DatabaseErrorInformation` so the classifier can be tested
    /// without a live connection (SKADI-T-0526).
    #[derive(Debug)]
    struct Info(String);
    impl diesel::result::DatabaseErrorInformation for Info {
        fn message(&self) -> &str {
            &self.0
        }
        fn details(&self) -> Option<&str> {
            None
        }
        fn hint(&self) -> Option<&str> {
            None
        }
        fn table_name(&self) -> Option<&str> {
            None
        }
        fn column_name(&self) -> Option<&str> {
            None
        }
        fn constraint_name(&self) -> Option<&str> {
            None
        }
        fn statement_position(&self) -> Option<i32> {
            None
        }
    }

    fn db(kind: DatabaseErrorKind) -> AppError {
        db_err(Error::DatabaseError(
            kind,
            Box::new(Info("duplicate key".to_string())),
        ))
    }

    #[test]
    fn constraint_violations_are_the_callers_fault() {
        // 400, not 500: the client sent something the schema rejects.
        for kind in [
            DatabaseErrorKind::UniqueViolation,
            DatabaseErrorKind::ForeignKeyViolation,
            DatabaseErrorKind::NotNullViolation,
            DatabaseErrorKind::CheckViolation,
        ] {
            assert!(
                matches!(db(kind), AppError::Validation(_)),
                "{kind:?} should be a validation error"
            );
        }
    }

    #[test]
    fn a_closed_connection_is_an_outage_not_a_bug() {
        // 502, not 500: retrying may work, which is not true of a bug.
        assert!(matches!(
            db(DatabaseErrorKind::ClosedConnection),
            AppError::Network(_)
        ));
        assert!(matches!(pool_err("timed out"), AppError::Network(_)));
    }

    #[test]
    fn no_rows_is_not_found() {
        assert!(matches!(db_err(Error::NotFound), AppError::NotFound(_)));
    }

    #[test]
    fn anything_uncharacterised_stays_internal() {
        // Deliberate: guessing a friendlier class for an error we have not
        // actually characterised would just move the confusion.
        assert!(matches!(
            db_err(Error::RollbackTransaction),
            AppError::Internal(_)
        ));
    }
}
