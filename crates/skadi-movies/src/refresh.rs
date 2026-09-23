//! Periodic movie-metadata refresh (SKADI-T-0041).
//!
//! Once a movie is added its TMDB-sourced fields are otherwise frozen. This
//! module keeps them fresh on a schedule, as a **Cloacina workflow** (so a run
//! is crash-resumable + retried like `acquire`) guarded by a per-provider
//! **circuit breaker** (so a failing provider isn't hammered).
//!
//! Shape mirrors the hunter (SKADI-I-0006): a process-global
//! [`RefreshServices`] (the non-serializable capabilities the task needs), a
//! `#[workflow]` whose single task reaches them, and a [`MovieRefreshWorker`]
//! that enumerates stale movies and runs the workflow once per movie.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use cloacina::Context;
use cloacina::executor::WorkflowExecutor;
use cloacina::runner::DefaultRunner;
use cloacina::workflow;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use skadi_core::module::BoxFuture;
use skadi_core::{AppError, Worker};
use skadi_hunter::CircuitBreaker;
use skadi_metadata::MetadataProvider;

use crate::MoviesRepo;
use crate::repo::MovieFilter;

/// Context key carrying the movie id for a refresh run.
const MOVIE_ID_KEY: &str = "movie_id";

/// The non-serializable capabilities the refresh task reaches via the global.
pub struct RefreshServices {
    pub provider: Arc<dyn MetadataProvider>,
    pub repo: Arc<dyn MoviesRepo>,
    /// Per-provider breaker; keyed by `provider.name()`.
    pub breaker: Arc<CircuitBreaker>,
}

static REFRESH: RwLock<Option<Arc<RefreshServices>>> = RwLock::new(None);

/// Install the global refresh services (called once at worker start; tests reset).
pub fn set_refresh_services(services: Arc<RefreshServices>) {
    *REFRESH.write().unwrap() = Some(services);
}

/// The installed global refresh services.
///
/// # Panics
/// If [`set_refresh_services`] hasn't run — the task ran before the worker
/// initialised the registry, a programming error.
#[must_use]
pub fn refresh_services() -> Arc<RefreshServices> {
    REFRESH
        .read()
        .unwrap()
        .clone()
        .expect("RefreshServices not initialized; call set_refresh_services() at worker start")
}

/// Clear the global (for tests).
pub fn reset_refresh_services() {
    *REFRESH.write().unwrap() = None;
}

/// Build the per-run context carrying `movie_id`.
pub(crate) fn refresh_context(movie_id: &str) -> skadi_core::Result<Context<Value>> {
    let mut ctx = Context::new();
    ctx.insert(MOVIE_ID_KEY, json!(movie_id))
        .map_err(|e| AppError::Internal(format!("refresh context: {e}")))?;
    Ok(ctx)
}

/// The `refresh_movie_metadata` workflow: one task that refreshes a single
/// movie's metadata through the active provider, behind the circuit breaker.
#[workflow(
    name = "refresh_movie_metadata",
    description = "Refresh one movie's metadata from the configured provider, behind a circuit breaker."
)]
pub mod refresh_movie_metadata {
    use chrono::Utc;
    use cloacina::{Context, TaskError, task};

    use crate::refresh::{MOVIE_ID_KEY, refresh_services};
    use crate::refresh_movie;
    use skadi_core::MovieId;

    fn te(message: impl Into<String>) -> TaskError {
        TaskError::ExecutionFailed {
            message: message.into(),
            task_id: "fetch_and_update".to_string(),
            timestamp: Utc::now(),
        }
    }

    // `retry_attempts = 1`: a single attempt per run. The circuit breaker + the
    // worker's interval ticks are the retry mechanism; in-run Cloacina retries
    // (default 3) would fight the breaker (each retry is another provider hit
    // that re-trips it) and muddy the per-run failure accounting.
    #[task(id = "fetch_and_update", retry_attempts = 1)]
    pub async fn fetch_and_update(
        context: &mut Context<serde_json::Value>,
    ) -> std::result::Result<(), TaskError> {
        let svc = refresh_services();
        let movie_id = context
            .get(MOVIE_ID_KEY)
            .and_then(|v| v.as_str())
            .ok_or_else(|| te("refresh context missing movie_id"))?
            .to_string();

        // Circuit breaker: don't hammer a failing provider. Returning an error
        // lets Cloacina back off + retry per its policy.
        let key = svc.provider.name().to_string();
        if !svc.breaker.allow(&key) {
            return Err(te(format!("metadata provider {key:?} circuit is open")));
        }

        let id = uuid::Uuid::parse_str(&movie_id)
            .map(MovieId::from)
            .map_err(|e| te(format!("bad movie id {movie_id:?}: {e}")))?;
        let movie = match svc.repo.get_movie(id).await {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(()), // deleted between enumeration and run — nothing to do
            Err(e) => return Err(te(format!("load movie: {e}"))),
        };
        let Some(tmdb) = movie.external_ids.tmdb.clone() else {
            return Ok(()); // no TMDB id → nothing to refresh
        };

        match refresh_movie(svc.provider.as_ref(), tmdb, Some(movie), None).await {
            Ok(updated) => {
                svc.repo
                    .upsert_movie(&updated)
                    .await
                    .map_err(|e| te(format!("persist refreshed movie: {e}")))?;
                svc.breaker.record_success(&key);
                Ok(())
            }
            Err(e) => {
                svc.breaker.record_failure(&key);
                Err(te(format!("provider refresh failed: {e}")))
            }
        }
    }
}

/// Daemon-side worker that refreshes stale movie metadata on an interval
/// (SKADI-T-0041). Shares the domain's Cloacina runner (`Arc`) with the hunter
/// worker; does **not** shut the runner down on cancel (the hunter worker owns
/// that, SKADI-T-0056).
pub struct MovieRefreshWorker {
    services: Arc<RefreshServices>,
    runner: Arc<DefaultRunner>,
    /// How often to look for stale movies.
    interval: Duration,
    /// A movie is refreshed when its `last_metadata_refresh` is older than this
    /// (or never set).
    staleness: Duration,
}

impl MovieRefreshWorker {
    #[must_use]
    pub fn new(
        services: Arc<RefreshServices>,
        runner: Arc<DefaultRunner>,
        interval: Duration,
        staleness: Duration,
    ) -> Self {
        Self {
            services,
            runner,
            interval,
            staleness,
        }
    }

    /// Enumerate stale movies and run the refresh workflow once per movie.
    /// Best-effort: individual run failures are logged, not propagated.
    async fn refresh_stale(&self) {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(self.staleness).unwrap_or(chrono::Duration::days(7));
        let movies = match self
            .services
            .repo
            .list_movies(MovieFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "metadata refresh: listing movies failed");
                return;
            }
        };
        for movie in movies {
            let stale = movie.last_metadata_refresh.is_none_or(|t| t < cutoff);
            if !stale {
                continue;
            }
            let ctx = match refresh_context(&movie.id.to_string()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "metadata refresh: context build failed");
                    continue;
                }
            };
            if let Err(e) = self.runner.execute("refresh_movie_metadata", ctx).await {
                tracing::warn!(movie = %movie.id, error = %e, "metadata refresh run failed");
            }
        }
    }
}

impl Worker for MovieRefreshWorker {
    fn name(&self) -> &str {
        "movies-metadata-refresh"
    }

    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            set_refresh_services(self.services.clone());
            let mut ticker = tokio::time::interval(self.interval);
            ticker.tick().await; // skip the immediate first tick
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => self.refresh_stale().await,
                }
            }
            // The runner is shared with the hunter worker, which shuts it down.
        })
    }
}
