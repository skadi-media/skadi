//! Manual/interactive indexer search (SKADI-T-0203).
//!
//! `GET /search?q=&kind=` runs a **free-text** search across the live indexers
//! (built from stored config) and returns the candidate [`Release`] list — the
//! Prowlarr/*arr "Search" tab: type a query, see what the indexers have, before
//! (or instead of) tying it to a library item. The per-library-item interactive
//! search (`GET …/releases` + grab) is the domain-scoped sibling; this is the
//! domain-agnostic one.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};

use skadi_core::{AppError, ExternalIds, MediaKind};
use skadi_indexers::{Release, ReleaseFetch};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
struct SearchParams {
    /// Free-text query (title). Required.
    q: String,
    /// `movie` (default) or `audiobook` — selects which indexers + categories.
    kind: Option<String>,
}

/// One candidate release on the wire — enough for a manual-search/grab screen.
#[derive(Serialize)]
struct ReleaseDto {
    title: String,
    size: u64,
    seeders: Option<u32>,
    /// RFC 3339 publish time.
    published: String,
    /// Source indexer id.
    indexer: String,
    /// `magnet` / `torrent` / `nzb`.
    protocol: String,
    // Parsed quality bits (best-effort, from the release title).
    year: Option<u16>,
    resolution: Option<String>,
    source: Option<String>,
    codec: Option<String>,
}

fn protocol_of(fetch: &ReleaseFetch) -> &'static str {
    match fetch {
        ReleaseFetch::Magnet(_) => "magnet",
        ReleaseFetch::TorrentUrl(_) => "torrent",
        ReleaseFetch::NzbUrl(_) => "nzb",
    }
}

fn release_dto(r: Release) -> ReleaseDto {
    ReleaseDto {
        title: r.title,
        size: r.size,
        seeders: r.seeders,
        published: r.published.to_rfc3339(),
        indexer: r.indexer.to_string(),
        protocol: protocol_of(&r.fetch).to_string(),
        year: r.parsed.year,
        resolution: r.parsed.resolution,
        source: r.parsed.source,
        codec: r.parsed.codec,
    }
}

/// Routes for manual search + indexer health, merged into the authed API router.
pub fn search_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/search", get(search))
        .route("/indexers/health", get(indexer_health))
        .route("/indexers/categories", get(indexer_categories))
}

/// `GET /indexers/categories` — the standard Newznab category tree (named), so the
/// config UI can pick categories by name instead of raw numbers (SKADI-T-0206).
async fn indexer_categories() -> impl IntoResponse {
    Json(skadi_indexers::standard_categories())
}

/// `GET /indexers/health` — per-indexer health telemetry (last success/failure,
/// counts, consecutive-failure streak, last error) from the in-memory registry
/// the [`HealthTracked`](skadi_indexers::HealthTracked) decorator feeds
/// (SKADI-T-0204). In-memory: empty after a restart until searches repopulate it.
async fn indexer_health() -> impl IntoResponse {
    Json(skadi_indexers::indexer_health().snapshot())
}

/// `GET /search?q=&kind=` — free-text search across the configured indexers,
/// newest-quality-agnostic raw candidates (deduped cross-indexer by title+fetch).
async fn search(
    State(state): State<Arc<AppState>>,
    crate::error::ApiQuery(params): crate::error::ApiQuery<SearchParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    if params.q.trim().is_empty() {
        return Err(ApiError(AppError::Validation("q must not be empty".into())));
    }
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))?;
    let kind = match params.kind.as_deref() {
        Some("audiobook") => MediaKind::Audiobook,
        _ => MediaKind::Movie,
    };

    // Build the live indexers from stored config (manual search is infrequent).
    let providers = crate::providers::build_providers(store).await?;

    let spec = skadi_hunter::SearchSpec {
        kind,
        // The operator typed this query, so indexers with automatic search off
        // are still consulted (SKADI-T-0539).
        trigger: skadi_hunter::SearchTrigger::Interactive,
        titles: vec![params.q.clone()],
        year: None,
        external_ids: ExternalIds::default(),
        categories: vec![],
        tv: None,
        series: None,
        // No item, so no tag scoping: a free-text search consults every indexer
        // (SKADI-T-0556). Passing `Some(vec![])` here would silently skip every
        // *tagged* indexer, which is exactly the tracker an operator reaches for
        // when hand-searching.
        tags: None,
    };
    let query = spec.query();

    let mut found: Vec<Release> = Vec::new();
    for ix in &providers.indexers {
        if !ix.supports(kind) {
            continue;
        }
        match ix.search(&query).await {
            Ok(rels) => found.extend(rels),
            Err(e) => tracing::warn!(error = %e, "manual search: an indexer failed (skipped)"),
        }
    }

    // De-dup cross-indexer duplicates by (title, fetch), like the hunter's search.
    found.sort_by(|a, b| {
        a.title
            .cmp(&b.title)
            .then_with(|| format!("{:?}", a.fetch).cmp(&format!("{:?}", b.fetch)))
    });
    found.dedup_by(|a, b| a.title == b.title && a.fetch == b.fetch);

    let dtos: Vec<ReleaseDto> = found.into_iter().map(release_dto).collect();
    Ok(Json(dtos))
}
