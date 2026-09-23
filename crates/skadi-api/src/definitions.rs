//! Cardigann definition catalog endpoints + the scheduled upstream sync
//! (SKADI-I-0036 / T-0261 / T-0263).
//!
//! Backs the add-tracker picker + the refresh control:
//! - `GET  /indexers/definitions`        — the catalog (summaries) for the picker
//! - `GET  /indexers/definitions/{id}`   — one definition's summary
//! - `POST /indexers/definitions/refresh`— re-sync the library from upstream now
//!
//! …and [`sync_loop`] is the daemon's background worker that keeps the library
//! fresh on a schedule (mirroring Prowlarr's daily definition refresh).
//!
//! The store is constructed per request from the configured definitions dir; the
//! parse is cheap and reload is rare.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio_util::sync::CancellationToken;

use skadi_core::AppError;
use skadi_http::HttpClient;
use skadi_indexers::definitions::{DEFAULT_REVISION, DefinitionStore};
use skadi_store::Store;

use crate::error::ApiError;
use crate::state::AppState;

/// Timeout for the (network) refresh; loads ignore it.
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);

/// Lower bound on the auto-sync interval, so a misconfig can't hammer upstream.
const MIN_SYNC_INTERVAL: u64 = 300;

/// Routes for the cardigann definition catalog, merged into the authed router.
pub fn definitions_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/indexers/definitions", get(list))
        .route("/indexers/definitions/refresh", post(refresh))
        .route("/indexers/definitions/{id}", get(detail))
}

/// The configured definitions directory (default if config is unavailable).
async fn defs_dir(state: &AppState) -> String {
    let Some(store) = state.store.as_ref() else {
        return "./definitions".into();
    };
    crate::load_config_view(store)
        .await
        .ok()
        .and_then(|v| v.get_string("cardigann_definitions_dir").ok())
        .unwrap_or_else(|| "./definitions".into())
}

fn store_for(dir: String) -> Result<DefinitionStore, ApiError> {
    let http = HttpClient::new(SYNC_TIMEOUT).map_err(ApiError::from)?;
    Ok(DefinitionStore::new(dir, http))
}

async fn list(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let catalog = store_for(defs_dir(&state).await)?.load()?;
    let entries: Vec<_> = catalog.list().into_iter().cloned().collect();
    Ok(Json(entries))
}

async fn detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let catalog = store_for(defs_dir(&state).await)?.load()?;
    let entry = catalog
        .entry(&id)
        .cloned()
        .ok_or_else(|| ApiError(AppError::NotFound(format!("unknown definition: {id}"))))?;
    Ok(Json(entry))
}

async fn refresh(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let revision = match state.store.as_ref() {
        Some(s) => configured_revision(s).await,
        None => DEFAULT_REVISION.into(),
    };
    let report = store_for(defs_dir(&state).await)?
        .refresh(&revision)
        .await?;
    Ok(Json(report))
}

/// The pinned upstream revision to sync (`cardigann_definitions_revision`,
/// default `master`).
async fn configured_revision(store: &Store) -> String {
    crate::load_config_view(store)
        .await
        .ok()
        .and_then(|v| v.get_string("cardigann_definitions_revision").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REVISION.into())
}

/// The daemon's background definition-sync worker (SKADI-T-0263): seeds the
/// bundled set on first run, then re-syncs the Prowlarr/Indexers library on a
/// schedule. `enabled` + `revision` are re-read each cycle so config changes take
/// effect without a restart; a failed sync keeps the current set.
pub async fn sync_loop(store: Store, cancel: CancellationToken) {
    let view = crate::load_config_view(&store).await.ok();
    let dir = view
        .as_ref()
        .and_then(|v| v.get_string("cardigann_definitions_dir").ok())
        .unwrap_or_else(|| "./definitions".into());
    let interval_secs = view
        .as_ref()
        .and_then(|v| v.get_u64("cardigann_sync_interval_secs").ok())
        .unwrap_or(86_400)
        .max(MIN_SYNC_INTERVAL);

    let Ok(http) = HttpClient::new(SYNC_TIMEOUT) else {
        tracing::warn!("definition sync: could not build http client; disabled");
        return;
    };
    let def_store = DefinitionStore::new(&dir, http);
    // Always ensure the dir is populated (bundled seed) so search works offline,
    // even before the first online sync.
    if let Err(e) = def_store.load() {
        tracing::warn!(error = %e, "definition sync: initial load failed");
    }

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    tracing::info!(
        interval_secs,
        "cardigann definition auto-sync worker started"
    );
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::debug!("definition sync worker cancelled");
                break;
            }
            _ = ticker.tick() => {
                // Re-read the toggle + revision so config edits apply live.
                let v = crate::load_config_view(&store).await.ok();
                let enabled = v
                    .as_ref()
                    .and_then(|v| v.get_bool("cardigann_sync_enabled").ok())
                    .unwrap_or(true);
                if !enabled {
                    continue;
                }
                let revision = v
                    .as_ref()
                    .and_then(|v| v.get_string("cardigann_definitions_revision").ok())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_REVISION.into());
                match def_store.refresh(&revision).await {
                    Ok(r) => {
                        tracing::info!(
                            written = r.written,
                            unparseable = r.unparseable,
                            revision = %r.revision,
                            "cardigann definitions synced from upstream",
                        );
                        // Register any newly-synced public indexers (+ ABB) so the
                        // live set grows as upstream adds trackers — no restart. The
                        // supervisor's next tick reloads providers off the new
                        // settings (SKADI-I-0042; fixes the bootstrap-before-sync
                        // timing that left only the bundled set registered).
                        if let Err(e) = crate::bootstrap::ensure_default_providers(&store).await {
                            tracing::warn!(error = %e, "default-provider seeding after sync failed");
                        }
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "cardigann definition sync failed; keeping current set",
                    ),
                }
            }
        }
    }
}
