//! `skadi-hunter` — the acquisition orchestrator, built on [Cloacina].
//!
//! The hunter drives an acquirable through **search → decide → snatch → monitor
//! → import → notify** as a durable Cloacina workflow. This crate owns the
//! workflow runner and (in later tasks) the `acquire` workflow itself, the
//! per-run context, and the worker that triggers runs.
//!
//! ## Storage isolation
//!
//! Cloacina manages its own schema (it tracks migrations in the standard
//! `__diesel_schema_migrations` table and creates ~24 unprefixed tables) and
//! owns its own `deadpool-diesel` pool — it cannot share `skadi-store`'s
//! `diesel-async`/bb8 pool. To keep one operational deployment without
//! colliding with `skadi-store`, the hunter runs Cloacina against an
//! *isolated* target derived from `SKADI_DATABASE_URL`:
//!
//! - **Postgres**: a dedicated [`CLOACINA_PG_DATABASE`] database on the **same
//!   PG server**, with [`HUNTER_PG_SCHEMA`] as the schema inside it. (Cloacina
//!   0.6.x forced that database name itself; since 0.11 it honours the URL's
//!   database, so skadi selects it explicitly to keep existing state in place.)
//!   The daemon must ensure the `cloacina` database exists on the PG server at
//!   startup (`CREATE DATABASE` privilege required).
//! - **SQLite**: a sibling database file (`hunter.db`) next to the main one.
//!
//! See [`cloacina_target_for`] for the derivation.
//!
//! [Cloacina]: https://github.com/colliery-io/cloacina

use std::path::Path;

use cloacina::runner::{DefaultRunner, DefaultRunnerConfig};
use skadi_core::{AppError, Result};

pub mod breaker;
pub mod pipeline;
pub mod services;
pub mod state;
pub mod status;
pub mod steps;
pub mod trace;
pub mod tracker;
pub mod trigger;
pub mod worker;
pub mod workflow;

pub use trigger::request_sweep;

pub use worker::{
    AcquireOutcome, AcquireSeed, DEFAULT_HISTORY_RETENTION_DAYS, DEFAULT_RSS_INTERVAL,
    DEFAULT_SWEEP_MAX_CONCURRENT, HunterWorker, STALE_ACQUIRE_GRACE, WantedQuery, feed_title_keys,
    rss_sweep, rss_sweep_detached, start_acquire, start_acquire_detached, start_grab, sweep_once,
    sweep_once_detached,
};

pub use breaker::CircuitBreaker;
pub use pipeline::{
    ReachabilityPolicy, ReleaseExplanation, Scoring, Verdict, decide, decide_with, evaluate,
    explain, floor_for_height, identity_rejection, import, monitor, notify, notify_failed,
    notify_grabbed, reachability_policy, search, seeder_band, set_reachability_policy, snatch,
    tv_episode_rejection, tv_wrong_show_rejection,
};
pub use services::{
    AudiobookScoring, HunterServices, ImporterFactory, ScoringConfig, services_for, set_services,
    try_services_for,
};
pub use state::{
    AcquireState, STATE_KEY, SearchSpec, SearchTrigger, SnatchFailure, SpecQuery, TvScope,
    load_state, store_state,
};
pub use status::{InMemoryStatusSink, StatusSink};
pub use steps::indexer_flags;
pub use tracker::{ActivityFilter, InFlightTracker, RunMeta, tracker};

/// Dedicated Postgres **database** (same server as skadi's) that all Cloacina
/// state lives in. Cloacina 0.6.x silently rewrote every Postgres URL to this
/// database name; 0.11 honours the URL's own database instead, so skadi now
/// selects it explicitly to keep production state where it already is
/// (SKADI-T-0390). `skadi-api`'s bootstrap creates it on demand.
pub const CLOACINA_PG_DATABASE: &str = "cloacina";

/// Dedicated Postgres schema (inside [`CLOACINA_PG_DATABASE`]) Cloacina's
/// workflow tables live in, isolating the hunter from other Cloacina users.
pub const HUNTER_PG_SCHEMA: &str = "cloacina_hunter";

/// The isolated Cloacina connection target derived from a skadi database URL:
/// the connection URL plus, for Postgres, the dedicated schema.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct CloacinaTarget {
    /// Connection URL Cloacina's `DefaultRunner` is built against.
    pub url: String,
    /// Postgres schema for isolation; `None` for SQLite (uses a separate file).
    pub schema: Option<String>,
}

/// Derive Cloacina's isolated storage target from skadi's database URL.
///
/// - `postgres://…` / `postgresql://…` → same server/credentials, database
///   [`CLOACINA_PG_DATABASE`], schema [`HUNTER_PG_SCHEMA`]. The user's database
///   is untouched.
/// - `sqlite://<path>` → a sibling `hunter.db` in the same directory, no schema.
///
/// Any other scheme is a [`AppError::Config`].
pub fn cloacina_target_for(skadi_url: &str) -> Result<CloacinaTarget> {
    if skadi_url.starts_with("postgres://") || skadi_url.starts_with("postgresql://") {
        let mut url = url::Url::parse(skadi_url)
            .map_err(|e| AppError::Config(format!("invalid database URL {skadi_url:?}: {e}")))?;
        url.set_path(CLOACINA_PG_DATABASE);
        return Ok(CloacinaTarget {
            url: url.to_string(),
            schema: Some(HUNTER_PG_SCHEMA.to_string()),
        });
    }
    if let Some(path) = skadi_url.strip_prefix("sqlite://") {
        // A URL with no directory component is rejected rather than resolved
        // (SKADI-T-0547). This used to fall back to a bare relative `hunter.db`,
        // which resolves against the process's working directory — so the runner
        // database landed wherever the daemon happened to be started from, and a
        // different place next time. Worse, every process started from the same
        // directory shared one file: that is how SKADI-T-0542's "database is
        // locked" happened, three directories deep from any error mentioning a
        // path.
        //
        // Production always passes an absolute path, so this rejects only the
        // cases that were already broken, and says why instead of guessing.
        let dir = Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| {
                AppError::Config(format!(
                    "database URL {skadi_url:?} has no directory component; the Cloacina \
                     runner database is placed alongside it as <dir>/hunter.db, so an \
                     absolute or directory-qualified path is required"
                ))
            })?;
        return Ok(CloacinaTarget {
            url: format!("sqlite://{}", dir.join("hunter.db").display()),
            schema: None,
        });
    }
    Err(AppError::Config(format!(
        "unsupported database URL scheme for hunter: {skadi_url:?} (expected sqlite:// or postgres://)"
    )))
}

/// The `DefaultRunnerConfig` the hunter uses: Cloacina's own cron scheduling is
/// **off** (the wanted/upgrade sweep is driven by `HunterWorker`, not Cloacina
/// cron), crash recovery is **on**.
fn runner_config() -> Result<DefaultRunnerConfig> {
    DefaultRunnerConfig::builder()
        .enable_cron_scheduling(false)
        .enable_recovery(true)
        .build()
        .map_err(|e| AppError::Config(format!("invalid cloacina runner config: {e}")))
}

/// Build a Cloacina [`DefaultRunner`] against the isolated target derived from
/// `skadi_url`. Runs Cloacina's migrations and spawns its background services;
/// call [`DefaultRunner::shutdown`] for a clean teardown.
pub async fn build_runner(skadi_url: &str) -> Result<DefaultRunner> {
    let target = cloacina_target_for(skadi_url)?;
    build_runner_for(&target).await
}

/// Build a runner against an already-derived [`CloacinaTarget`].
pub async fn build_runner_for(target: &CloacinaTarget) -> Result<DefaultRunner> {
    let config = runner_config()?;
    let mut builder = DefaultRunner::builder()
        .database_url(&target.url)
        .with_config(config);
    if let Some(schema) = &target.schema {
        builder = builder.schema(schema);
    }
    builder
        .build()
        .await
        .map_err(|e| AppError::Internal(format!("building cloacina runner: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_postgres_schema_target() {
        let t = cloacina_target_for("postgres://user:pw@localhost/skadi").unwrap();
        assert_eq!(t.url, "postgres://user:pw@localhost/cloacina");
        // Query params (sslmode etc.) survive the database swap.
        let t = cloacina_target_for("postgresql://u:p@db:5432/skadi?sslmode=require").unwrap();
        assert_eq!(t.url, "postgresql://u:p@db:5432/cloacina?sslmode=require");
        assert_eq!(t.schema.as_deref(), Some(HUNTER_PG_SCHEMA));
    }

    #[test]
    fn derives_sqlite_sibling_file() {
        // Relative path with a directory component.
        let t = cloacina_target_for("sqlite://./data/skadi.db").unwrap();
        assert_eq!(t.url, "sqlite://./data/hunter.db");
        assert!(t.schema.is_none());

        // Absolute path keeps its directory.
        let t = cloacina_target_for("sqlite:///var/lib/skadi/skadi.db").unwrap();
        assert_eq!(t.url, "sqlite:///var/lib/skadi/hunter.db");
    }

    #[test]
    fn rejects_a_url_with_no_directory_component() {
        // Both of these used to derive a bare relative `hunter.db` in whatever
        // directory the process was started from, silently, and every process
        // started from that directory shared the one file (SKADI-T-0547).
        for url in ["sqlite://skadi.db", "sqlite://:memory:"] {
            let err = cloacina_target_for(url).unwrap_err();
            let AppError::Config(msg) = &err else {
                panic!("{url}: expected a Config error, got {err:?}");
            };
            // The message has to name the requirement, not just refuse: whoever
            // hits this is looking at a URL that seems perfectly reasonable.
            assert!(
                msg.contains("no directory component") && msg.contains("hunter.db"),
                "{url}: message does not explain the requirement: {msg}"
            );
        }
    }

    #[test]
    fn the_default_database_url_still_derives_a_target() {
        // `sqlite://./skadi.db` — the directory is `.`, which is a directory
        // component, so the default must keep working. A stricter check that
        // also rejected relative directories would break every local run.
        let t = cloacina_target_for(skadi_store::DEFAULT_DATABASE_URL).unwrap();
        assert_eq!(t.url, "sqlite://./hunter.db");
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = cloacina_target_for("mysql://nope").unwrap_err();
        assert!(matches!(err, AppError::Config(_)));
    }
}

/// Smoke workflow proving the runner wiring: a single no-op task. Defined only
/// in tests so it doesn't ship in the library's workflow registry. Cloacina
/// registers it via `inventory` at load time, so it's reachable by name in the
/// test binary without an explicit `use`.
#[cfg(test)]
mod smoke_defs {
    use cloacina::workflow;

    #[workflow(name = "smoke", description = "no-op smoke workflow")]
    pub mod smoke {
        use cloacina::{Context, TaskError, task};

        #[task(id = "noop")]
        pub async fn noop(_context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod runner_tests {
    use super::*;
    use cloacina::Context;
    use cloacina::executor::WorkflowExecutor;

    #[tokio::test]
    async fn builds_runner_runs_migrations_and_executes_smoke_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let skadi_db = dir.path().join("skadi.db");
        let skadi_url = format!("sqlite://{}", skadi_db.display());

        // Derivation lands Cloacina in a sibling hunter.db, not skadi.db.
        let target = cloacina_target_for(&skadi_url).unwrap();
        assert!(target.url.ends_with("hunter.db"));
        assert_ne!(target.url, skadi_url);

        let runner = build_runner_for(&target).await.unwrap();

        // Cloacina created its own database file (migrations ran).
        assert!(dir.path().join("hunter.db").exists());

        let result = runner
            .execute("smoke", Context::<serde_json::Value>::new())
            .await
            .unwrap();
        assert!(
            matches!(result.status, cloacina::WorkflowStatus::Completed),
            "unexpected status: {:?}",
            result.status
        );

        runner.shutdown().await.unwrap();
    }
}
