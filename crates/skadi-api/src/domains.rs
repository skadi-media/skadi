//! Domain enable/disable endpoints (SKADI-T-0054).
//!
//! `GET /domains` lists the compiled-in domains joined with their runtime
//! enable state; `PUT /domains/{name}` toggles a domain (the supervisor picks
//! the change up on its next reconcile tick). Domain-agnostic — it only needs
//! the registry descriptors in [`AppState`] and the
//! [`DomainStateRepo`](skadi_store::DomainStateRepo).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, put};
use serde::{Deserialize, Serialize};

use skadi_core::AppError;
use skadi_store::{DomainStateRepo, Store};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
struct DomainDto {
    name: String,
    kind: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct SetEnabled {
    enabled: bool,
}

/// Routes for domain enable/disable, merged into the authed API router.
pub fn domains_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/domains", get(list))
        .route("/domains/{name}", put(set_enabled))
}

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

fn kind_str(kind: skadi_core::MediaKind) -> String {
    format!("{kind:?}").to_lowercase()
}

async fn list(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let mut out = Vec::with_capacity(state.domains.len());
    for d in &state.domains {
        let enabled = store
            .get(&d.name)
            .await?
            .map(|s| s.enabled)
            .unwrap_or(false);
        out.push(DomainDto {
            name: d.name.clone(),
            kind: kind_str(d.kind),
            enabled,
        });
    }
    Ok(Json(out))
}

async fn set_enabled(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    crate::error::ApiJson(body): crate::error::ApiJson<SetEnabled>,
) -> Result<impl IntoResponse, ApiError> {
    let descriptor = state
        .domains
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| ApiError(AppError::NotFound(format!("unknown domain: {name}"))))?;
    let result = store(&state)?.set_enabled(&name, body.enabled).await?;
    Ok(Json(DomainDto {
        name: result.name,
        kind: kind_str(descriptor.kind),
        enabled: result.enabled,
    }))
}
