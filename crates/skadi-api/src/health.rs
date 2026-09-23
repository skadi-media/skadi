//! The `/api/v1/health` endpoints — an unauthenticated liveness probe and a
//! readiness probe.
//!
//! Liveness answers as soon as the process is serving. Readiness (SKADI-T-0475)
//! additionally requires that the database answers and that the supervisor has
//! completed a provider reconcile, so a deploy gate can tell "the listener is up"
//! apart from "the daemon can actually do its job".

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
use skadi_store::ConfigRepo;

use crate::AppState;

/// Health response body.
#[derive(Debug, Serialize)]
pub struct Health {
    /// Always `"ok"` when the process is serving.
    pub status: &'static str,
    /// The crate version, for quick deployment sanity checks.
    pub version: &'static str,
}

/// `GET /api/v1/health` handler. Skipped by the auth middleware.
pub async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Readiness response body: what was checked, and what failed if anything did.
#[derive(Debug, Serialize)]
pub struct Readiness {
    /// `"ready"` or `"not_ready"`.
    pub status: &'static str,
    pub version: &'static str,
    /// The database answered a query.
    pub database: bool,
    /// The supervisor has completed at least one provider reconcile.
    pub providers: bool,
    /// Human-readable reason when not ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `GET /api/v1/health/ready` — the probe a deploy gate should poll.
///
/// Liveness (`/health`) says the listener is up, which it is well before
/// migrations have run or any provider exists. Worse, bare `/health` (without
/// the `/api/v1` prefix) is served by the SPA fallback, so it returns 200 the
/// instant the socket binds — which is what made `angreal deploy up` report a
/// healthy stack immediately (SKADI-T-0475).
///
/// Returns 503 with the failing check named, so a gate that times out says why.
/// Unauthenticated, like liveness: a probe has no token.
pub async fn readiness(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut detail: Vec<String> = Vec::new();

    let database = match state.store.as_ref() {
        None => {
            detail.push("store not configured".into());
            false
        }
        // A single-row lookup: proves the pool hands out a connection and the
        // schema is present, without scanning anything.
        Some(store) => match store.get_config("mode").await {
            Ok(_) => true,
            Err(e) => {
                detail.push(format!("database: {e}"));
                false
            }
        },
    };

    let providers = state.providers_ready.load(Ordering::Relaxed);
    if !providers {
        detail.push("providers: the supervisor has not completed a reconcile yet".into());
    }

    let ready = database && providers;
    let body = Json(Readiness {
        status: if ready { "ready" } else { "not_ready" },
        version: env!("CARGO_PKG_VERSION"),
        database,
        providers,
        detail: (!detail.is_empty()).then(|| detail.join("; ")),
    });
    let code = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, body)
}
