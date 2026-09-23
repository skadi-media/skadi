//! Domain module registration.
//!
//! A [`DomainModule`] is a media-type plugin (movies, tv, music, ...) compiled
//! into the binary. The daemon registers every module unconditionally at
//! startup and consults the runtime enable/disable state to decide which
//! modules' [`Worker`]s to spawn.
//!
//! Per the foundation design decision (2026-05-26), this trait is deliberately
//! framework-free: it carries identity, migrations, and background workers
//! only. HTTP routing (axum) and CLI wiring (clap) live in separate
//! `HttpModule` / `CliModule` traits defined in `skadi-api` / `skadi-cli`,
//! where those dependencies already exist — keeping `skadi-core` dependency-light.

use std::future::Future;
use std::pin::Pin;

use diesel_migrations::EmbeddedMigrations;
use tokio_util::sync::CancellationToken;

use crate::status::MediaKind;

/// A boxed, `Send` future returned by a [`Worker`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A long-running background task owned by a domain (hunters, scanners, refresh
/// loops). Spawned when the domain is enabled and cancelled when disabled.
pub trait Worker: Send + Sync {
    /// Human-readable name, for logging.
    fn name(&self) -> &str;

    /// Run until the supplied [`CancellationToken`] is cancelled.
    ///
    /// Implementations should select on `cancel.cancelled()` and return
    /// promptly once it fires.
    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()>;
}

/// A type-erased [`Worker`].
pub type BoxedWorker = Box<dyn Worker>;

/// A media-type plugin compiled into the binary.
///
/// ## Migrations
///
/// Each module ships its schema as Diesel [`EmbeddedMigrations`] — one set per
/// backend, since the SQL dialects diverge (`TEXT`/`INTEGER` vs
/// `UUID`/`JSONB`/`TIMESTAMPTZ`). The daemon's bootstrap selects the set for the
/// configured backend and applies it. This replaced an earlier hand-rolled
/// `Migration` struct (SKADI-T-0051): real `diesel_migrations` embedding makes a
/// domain crate's `migrations/<backend>/` directory the single source of truth,
/// identical to how `skadi-store` ships its own schema.
pub trait DomainModule: Send + Sync {
    /// Stable machine name, e.g. `"movies"`.
    fn name(&self) -> &'static str;

    /// The kind of media this module manages.
    fn kind(&self) -> MediaKind;

    /// The module's embedded SQLite migrations. Run unconditionally at startup
    /// regardless of enable state (see SKADI-I-0002).
    ///
    /// Returned by value: [`EmbeddedMigrations`] is a thin `&'static`-backed
    /// handle, and `diesel_migrations`' `MigrationSource` is implemented for it
    /// by value (not for `&EmbeddedMigrations`). Implementations return their
    /// `const` migration set directly, which materializes a fresh handle.
    fn sqlite_migrations(&self) -> EmbeddedMigrations;

    /// The module's embedded Postgres migrations. Run unconditionally at startup
    /// regardless of enable state. See [`Self::sqlite_migrations`].
    fn postgres_migrations(&self) -> EmbeddedMigrations;

    /// Background workers to spawn while this domain is enabled.
    ///
    /// (The daemon will pass a context argument once `DaemonContext` exists in
    /// the daemon initiative; for now workers capture what they need at
    /// construction.)
    fn workers(&self) -> Vec<BoxedWorker>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel_migrations::embed_migrations;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FlagWorker {
        ran: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
    }

    impl Worker for FlagWorker {
        fn name(&self) -> &str {
            "flag-worker"
        }

        fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
            Box::pin(async move {
                self.ran.store(true, Ordering::SeqCst);
                cancel.cancelled().await;
                self.stopped.store(true, Ordering::SeqCst);
            })
        }
    }

    struct DummyModule;

    // Tiny real embedded migrations so the reshaped trait stays exercised.
    const SQLITE_TEST_MIGRATIONS: EmbeddedMigrations = embed_migrations!("test_migrations/sqlite");
    const POSTGRES_TEST_MIGRATIONS: EmbeddedMigrations =
        embed_migrations!("test_migrations/postgres");

    impl DomainModule for DummyModule {
        fn name(&self) -> &'static str {
            "dummy"
        }
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        fn sqlite_migrations(&self) -> EmbeddedMigrations {
            SQLITE_TEST_MIGRATIONS
        }
        fn postgres_migrations(&self) -> EmbeddedMigrations {
            POSTGRES_TEST_MIGRATIONS
        }
        fn workers(&self) -> Vec<BoxedWorker> {
            vec![Box::new(FlagWorker {
                ran: Arc::new(AtomicBool::new(false)),
                stopped: Arc::new(AtomicBool::new(false)),
            })]
        }
    }

    #[test]
    fn module_exposes_identity_and_migrations() {
        let m = DummyModule;
        assert_eq!(m.name(), "dummy");
        assert_eq!(m.kind(), MediaKind::Movie);
        // The migration accessors return the embedded sets by value (the actual
        // application is exercised by bootstrap in skadi-api); here we just
        // confirm both backends are wired and callable.
        let _sqlite = m.sqlite_migrations();
        let _postgres = m.postgres_migrations();
        assert_eq!(m.workers().len(), 1);
    }

    #[tokio::test]
    async fn worker_runs_then_stops_on_cancel() {
        let ran = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker: BoxedWorker = Box::new(FlagWorker {
            ran: ran.clone(),
            stopped: stopped.clone(),
        });

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(worker.run(cancel.clone()));

        // Let it start, then cancel and await clean shutdown.
        tokio::task::yield_now().await;
        cancel.cancel();
        handle.await.unwrap();

        assert!(ran.load(Ordering::SeqCst));
        assert!(stopped.load(Ordering::SeqCst));
    }
}
