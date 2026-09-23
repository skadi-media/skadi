//! Read-only quality-definitions endpoint (SKADI-T-0067).
//!
//! `GET /quality/definitions` returns the built-in [`QualityDefinition`] set
//! (skadi-quality `default_definitions`) — stable ids, names, ordered low→high
//! — so the web UI's profile editor can offer the named qualities to allow and
//! pick a cutoff without hard-coding their UUIDs. The ids here are exactly the
//! ones a stored `profiles` row references (see the daemon's profile resolver).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::routing::get;

use serde::Serialize;
use skadi_quality::default_definitions;

use crate::state::AppState;

/// One quality definition, flattened for the UI. `rank` is the index in the
/// low→high ordering (the same order the array is returned in).
#[derive(Serialize)]
struct QualityDefDto {
    id: String,
    name: String,
    resolution: String,
    rank: usize,
}

/// Routes for the read-only quality registry, merged into the authed router.
pub fn quality_router() -> Router<Arc<AppState>> {
    Router::new().route("/quality/definitions", get(list_definitions))
}

async fn list_definitions() -> Json<Vec<QualityDefDto>> {
    let defs = default_definitions()
        .into_iter()
        .enumerate()
        .map(|(rank, d)| QualityDefDto {
            id: d.id.to_string(),
            name: d.name,
            resolution: format!("{:?}", d.resolution),
            rank,
        })
        .collect();
    Json(defs)
}
