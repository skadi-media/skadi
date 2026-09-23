//! Periodic audiobook-metadata refresh (SKADI-T-0135).
//!
//! Once a book is added its Audnexus-sourced fields are otherwise frozen. This
//! keeps them fresh on a schedule, as a **Cloacina workflow** (crash-resumable +
//! retried like `acquire`) guarded by a per-provider **circuit breaker** (so a
//! failing provider isn't hammered). Shape mirrors `skadi_movies::refresh`
//! (SKADI-T-0041): a process-global [`RefreshServices`], a `#[workflow]` whose
//! single task reaches them, and a [`BookRefreshWorker`] that enumerates stale
//! books and runs the workflow once per book.
//!
//! Refresh preserves user-controlled fields (it passes the existing `Book` to
//! [`refresh_book`](crate::refresh_book), which only overwrites the
//! Audnexus-sourced descriptive fields). Books without an ASIN are skipped —
//! there's nothing to look up.

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

use crate::repo::{AudiobooksRepo, BookFilter};

/// Default cadence for the refresh sweep.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// A book is refreshed when `last_metadata_refresh` is older than this.
pub const DEFAULT_REFRESH_STALENESS: Duration = Duration::from_secs(7 * 24 * 3600);

/// Context key carrying the book id for a refresh run.
const BOOK_ID_KEY: &str = "book_id";

/// The non-serializable capabilities the refresh task reaches via the global.
pub struct RefreshServices {
    pub provider: Arc<dyn MetadataProvider>,
    pub repo: Arc<dyn AudiobooksRepo>,
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

/// Build the per-run context carrying `book_id`.
pub(crate) fn refresh_context(book_id: &str) -> skadi_core::Result<Context<Value>> {
    let mut ctx = Context::new();
    ctx.insert(BOOK_ID_KEY, json!(book_id))
        .map_err(|e| AppError::Internal(format!("refresh context: {e}")))?;
    Ok(ctx)
}

/// The `refresh_book_metadata` workflow: one task that refreshes a single book's
/// metadata through the active provider, behind the circuit breaker.
#[workflow(
    name = "refresh_book_metadata",
    description = "Refresh one audiobook's metadata from Audnexus, behind a circuit breaker."
)]
pub mod refresh_book_metadata {
    use chrono::Utc;
    use cloacina::{Context, TaskError, task};

    use crate::refresh::{BOOK_ID_KEY, refresh_services};
    use crate::refresh_book;
    use skadi_core::BookId;

    fn te(message: impl Into<String>) -> TaskError {
        TaskError::ExecutionFailed {
            message: message.into(),
            // Task ids are global in Cloacina's inventory registry, so this must
            // not collide with the movies refresh task (`fetch_and_update`).
            task_id: "fetch_and_update_book".to_string(),
            timestamp: Utc::now(),
        }
    }

    // `retry_attempts = 1`: a single attempt per run. The circuit breaker + the
    // worker's interval ticks are the retry mechanism; in-run Cloacina retries
    // would fight the breaker (each retry is another provider hit that re-trips
    // it) and muddy the per-run failure accounting.
    #[task(id = "fetch_and_update_book", retry_attempts = 1)]
    pub async fn fetch_and_update(
        context: &mut Context<serde_json::Value>,
    ) -> std::result::Result<(), TaskError> {
        let svc = refresh_services();
        let book_id = context
            .get(BOOK_ID_KEY)
            .and_then(|v| v.as_str())
            .ok_or_else(|| te("refresh context missing book_id"))?
            .to_string();

        // Circuit breaker: don't hammer a failing provider.
        let key = svc.provider.name().to_string();
        if !svc.breaker.allow(&key) {
            return Err(te(format!("metadata provider {key:?} circuit is open")));
        }

        let id = uuid::Uuid::parse_str(&book_id)
            .map(BookId::from)
            .map_err(|e| te(format!("bad book id {book_id:?}: {e}")))?;
        let book = match svc.repo.get_book(id).await {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()), // deleted between enumeration and run
            Err(e) => return Err(te(format!("load book: {e}"))),
        };
        let Some(asin) = book.external_ids.asin.clone() else {
            return Ok(()); // no ASIN → nothing to refresh
        };

        match refresh_book(
            svc.repo.as_ref(),
            svc.provider.as_ref(),
            asin,
            Some(book),
            None,
        )
        .await
        {
            Ok(updated) => {
                svc.repo
                    .upsert_book(&updated)
                    .await
                    .map_err(|e| te(format!("persist refreshed book: {e}")))?;
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

/// Daemon-side worker that refreshes stale book metadata on an interval. Shares
/// the domain's Cloacina runner (`Arc`) with the hunter worker; does **not** shut
/// the runner down on cancel (the hunter worker owns that).
pub struct BookRefreshWorker {
    services: Arc<RefreshServices>,
    runner: Arc<DefaultRunner>,
    interval: Duration,
    staleness: Duration,
}

impl BookRefreshWorker {
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

    /// Enumerate stale books and run the refresh workflow once per book.
    /// Best-effort: individual run failures are logged, not propagated.
    async fn refresh_stale(&self) {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(self.staleness).unwrap_or(chrono::Duration::days(7));
        let books = match self
            .services
            .repo
            .list_books(BookFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "metadata refresh: listing books failed");
                return;
            }
        };
        for book in books {
            // Nothing to look up without an ASIN.
            if book.external_ids.asin.is_none() {
                continue;
            }
            let stale = book.last_metadata_refresh.is_none_or(|t| t < cutoff);
            if !stale {
                continue;
            }
            let ctx = match refresh_context(&book.id.to_string()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "metadata refresh: context build failed");
                    continue;
                }
            };
            if let Err(e) = self.runner.execute("refresh_book_metadata", ctx).await {
                tracing::warn!(book = %book.id, error = %e, "metadata refresh run failed");
            }
        }
    }
}

impl Worker for BookRefreshWorker {
    fn name(&self) -> &str {
        "audiobooks-metadata-refresh"
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
