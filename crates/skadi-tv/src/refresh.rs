//! Periodic series-metadata refresh (SKADI-T-0266 worker half).
//!
//! Keeps each series' Skyhook-sourced fields + episode list fresh on a schedule,
//! as a crash-resumable **Cloacina workflow** guarded by a per-provider circuit
//! breaker. Mirrors `skadi-movies`'s `refresh.rs`. The provider here is a
//! [`SeriesMetadataProvider`] (keyless Skyhook), not the generic `MetadataProvider`.

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
use skadi_metadata::SeriesMetadataProvider;

use crate::repo::{SeriesFilter, TvRepo};

const SERIES_ID_KEY: &str = "series_id";

/// The non-serializable capabilities the refresh task reaches via the global.
pub struct RefreshServices {
    pub provider: Arc<dyn SeriesMetadataProvider>,
    pub repo: Arc<dyn TvRepo>,
    /// Per-provider breaker; keyed by a fixed name (the provider trait has none).
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
/// If [`set_refresh_services`] hasn't run.
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

pub(crate) fn refresh_context(series_id: &str) -> skadi_core::Result<Context<Value>> {
    let mut ctx = Context::new();
    ctx.insert(SERIES_ID_KEY, json!(series_id))
        .map_err(|e| AppError::Internal(format!("refresh context: {e}")))?;
    Ok(ctx)
}

/// The `refresh_series_metadata` workflow: one task that re-syncs a single
/// series through the active provider, behind the circuit breaker.
#[workflow(
    name = "refresh_series_metadata",
    description = "Refresh one series' metadata + episodes from Skyhook, behind a circuit breaker."
)]
pub mod refresh_series_metadata {
    use chrono::Utc;
    use cloacina::{Context, TaskError, task};

    use crate::refresh::{SERIES_ID_KEY, refresh_services};
    use crate::refresh_series;
    use skadi_core::{AppError, SeriesId};

    fn te(message: impl Into<String>) -> TaskError {
        TaskError::ExecutionFailed {
            message: message.into(),
            task_id: "fetch_series_and_update".to_string(),
            timestamp: Utc::now(),
        }
    }

    #[task(id = "fetch_series_and_update", retry_attempts = 1)]
    pub async fn fetch_and_update(
        context: &mut Context<serde_json::Value>,
    ) -> std::result::Result<(), TaskError> {
        let svc = refresh_services();
        let series_id = context
            .get(SERIES_ID_KEY)
            .and_then(|v| v.as_str())
            .ok_or_else(|| te("refresh context missing series_id"))?
            .to_string();

        let key = "skyhook".to_string();
        if !svc.breaker.allow(&key) {
            return Err(te(format!("metadata provider {key:?} circuit is open")));
        }

        let id = uuid::Uuid::parse_str(&series_id)
            .map(SeriesId::from)
            .map_err(|e| te(format!("bad series id {series_id:?}: {e}")))?;

        match refresh_series(svc.repo.as_ref(), svc.provider.as_ref(), id).await {
            Ok(_) => {
                svc.breaker.record_success(&key);
                Ok(())
            }
            // A series deleted between enumeration and run is not a provider failure.
            Err(AppError::Validation(_)) => Ok(()),
            Err(e) => {
                svc.breaker.record_failure(&key);
                Err(te(format!("provider refresh failed: {e}")))
            }
        }
    }
}

/// Daemon-side worker that refreshes stale series metadata on an interval. Shares
/// the domain's Cloacina runner with the hunter worker (does not shut it down).
pub struct SeriesRefreshWorker {
    services: Arc<RefreshServices>,
    runner: Arc<DefaultRunner>,
    interval: Duration,
    staleness: Duration,
}

impl SeriesRefreshWorker {
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

    async fn refresh_stale(&self) {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(self.staleness).unwrap_or(chrono::Duration::days(7));
        let series = match self
            .services
            .repo
            .list_series(SeriesFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "metadata refresh: listing series failed");
                return;
            }
        };
        for s in series {
            let stale = s.last_metadata_refresh.is_none_or(|t| t < cutoff);
            if !stale {
                continue;
            }
            let ctx = match refresh_context(&s.id.to_string()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "metadata refresh: context build failed");
                    continue;
                }
            };
            if let Err(e) = self.runner.execute("refresh_series_metadata", ctx).await {
                tracing::warn!(series = %s.id, error = %e, "series metadata refresh run failed");
            }
        }
    }
}

impl Worker for SeriesRefreshWorker {
    fn name(&self) -> &str {
        "television-metadata-refresh"
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
        })
    }
}
