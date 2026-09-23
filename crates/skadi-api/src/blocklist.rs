//! Blocklist CRUD endpoints (SKADI-T-0115).
//!
//! Daemon-level view/manage of the failed-release blocklist that the acquire
//! pipeline writes to on download/import failure (see
//! [`BlocklistRepo`](skadi_store::BlocklistRepo)). `GET /blocklist` lists entries
//! (optionally scoped to one acquirable via `?acquirable=`), `POST /blocklist`
//! manually blocks a release, and `DELETE /blocklist/{id}` clears one. Cross-
//! domain — it only needs the [`Store`] in [`AppState`].

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use serde::{Deserialize, Serialize};

use skadi_core::AppError;
use skadi_store::{BlocklistEntry, BlocklistRepo, NewBlocklistEntry, Store};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
struct BlocklistDto {
    id: String,
    release_key: String,
    title: String,
    acquirable_ref: Option<String>,
    indexer: Option<String>,
    reason: Option<String>,
    at: String,
    /// RFC 3339 expiry, when the block is temporary (SKADI-T-0198).
    expires_at: Option<String>,
}

impl From<BlocklistEntry> for BlocklistDto {
    fn from(e: BlocklistEntry) -> Self {
        BlocklistDto {
            id: e.id,
            release_key: e.release_key,
            title: e.title,
            acquirable_ref: e.acquirable_ref,
            indexer: e.indexer,
            reason: e.reason,
            at: e.at.to_rfc3339(),
            expires_at: e.expires_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Deserialize)]
struct ListQuery {
    /// Scope the list to one acquirable ref.
    acquirable: Option<String>,
    /// Page size (SKADI-T-0494). Applied in memory: the blocklist is small and
    /// already answers in ~2 ms, so this is payload relief (326 KB unbounded on a
    /// prod-sized install), not a scan fix.
    limit: Option<usize>,
    /// Rows to skip; ignored without a `limit`.
    offset: Option<usize>,
}

#[derive(Deserialize)]
struct BlockRequest {
    release_key: String,
    title: String,
    acquirable_ref: Option<String>,
    indexer: Option<String>,
    reason: Option<String>,
    /// Optional time-to-live in seconds (SKADI-T-0198): the block auto-expires
    /// after this long. Absent ⇒ a permanent block.
    ttl_seconds: Option<i64>,
}

/// Routes for the blocklist, merged into the authed API router.
pub fn blocklist_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/blocklist", get(list).post(add).delete(clear))
        .route("/blocklist/{id}", delete(remove))
}

#[derive(Deserialize)]
struct ClearQuery {
    /// Comma-separated entry ids to remove.
    ids: Option<String>,
    /// Explicit confirmation for the wipe-everything case (SKADI-T-0473).
    all: Option<bool>,
}

/// `DELETE /blocklist` — bulk-remove by `?ids=a,b,c`, or clear the whole blocklist
/// with `?all=true` (SKADI-T-0195, SKADI-T-0473). Returns `{ "removed": N }`.
///
/// A bare `DELETE /blocklist` used to wipe the entire blocklist. That is a
/// destructive default reachable by omission — a client that meant to send
/// `?ids=` and computed an empty list, or a curl with a typo'd parameter name,
/// silently discarded every block the operator had accumulated, and a blocklist
/// entry records a judgement (this release is bad) that cannot be reconstructed.
/// Sonarr requires explicit ids for the same reason.
///
/// So the wipe now needs `?all=true`, and asking for neither is a validation
/// error rather than the most destructive available interpretation.
async fn clear(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ClearQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let removed = match (q.ids, q.all.unwrap_or(false)) {
        (Some(raw), _) => {
            let ids: Vec<String> = raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            // An explicitly-empty `ids=` removes nothing. It must NOT fall through
            // to the wipe — that is precisely the client bug this guards against.
            store.unblock_many(&ids).await?
        }
        (None, true) => store.clear_blocklist().await?,
        (None, false) => {
            return Err(ApiError(AppError::Validation(
                "refusing to clear the whole blocklist without confirmation: \
                 pass ?ids=a,b,c to remove specific entries, or ?all=true to clear everything"
                    .into(),
            )));
        }
    };
    Ok(Json(serde_json::json!({ "removed": removed })))
}

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let (entries, total) = match q.acquirable {
        // One acquirable's blocks: naturally a handful, and the query is already
        // indexed on the ref, so this path still pages in memory.
        Some(acq) => {
            let all = store.list_blocklist_for(&acq).await?;
            let total = all.len() as i64;
            let page = all
                .into_iter()
                .skip(q.offset.unwrap_or(0))
                .take(q.limit.unwrap_or(usize::MAX))
                .collect::<Vec<_>>();
            (page, total)
        }
        // The whole blocklist is unbounded and only grows, so the bound reaches
        // the database rather than trimming a fully-loaded Vec (SKADI-T-0494).
        None => {
            let total = store.count_blocklist().await?;
            let page = store
                .list_blocklist_page(q.limit.map(|l| l as i64), q.offset.unwrap_or(0) as i64)
                .await?;
            (page, total)
        }
    };
    let out: Vec<BlocklistDto> = entries.into_iter().map(BlocklistDto::from).collect();
    Ok(([("x-total-count", total.to_string())], Json(out)))
}

async fn add(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BlockRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.release_key.trim().is_empty() {
        return Err(ApiError(AppError::Validation(
            "release_key must not be empty".into(),
        )));
    }
    let expires_at = body
        .ttl_seconds
        .filter(|s| *s > 0)
        .map(|s| chrono::Utc::now() + chrono::Duration::seconds(s));
    let entry = store(&state)?
        .block(&NewBlocklistEntry {
            release_key: body.release_key,
            title: body.title,
            acquirable_ref: body.acquirable_ref,
            indexer: body.indexer,
            reason: body.reason.or_else(|| Some("manual".into())),
            expires_at,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(BlocklistDto::from(entry))))
}

async fn remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let removed = store(&state)?.unblock(&id).await?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(AppError::NotFound(format!(
            "blocklist entry {id} not found"
        ))))
    }
}
