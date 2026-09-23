//! Unified library view + in-flight activity endpoints (SKADI-T-0055).
//!
//! `GET /library` aggregates a [`LibraryItemDto`] list across every enabled
//! domain. Like [`HttpModule`](crate::http_module::HttpModule), domains
//! contribute via a trait ([`LibraryProvider`]) rather than `skadi-api`
//! depending on them: the daemon registers each domain's provider in
//! [`AppState::library`](crate::state::AppState). `GET /activity` reads the
//! process-global in-flight tracker from `skadi-hunter`.
//!
//! **Deviation from AC (consistent with the Option-A `HttpModule` decision):**
//! the library hook is an async `LibraryProvider` trait in `skadi-api`, not a
//! `library_view` method on `skadi_core::DomainModule` — `DomainModule` is
//! framework- and DB-free, and reading the library is an async DB operation, so
//! it belongs alongside the other domain-contributed HTTP hooks.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use serde::{Deserialize, Serialize};

use skadi_core::{MediaKind, Result};
use skadi_store::DomainStateRepo;

use crate::error::ApiError;
use crate::state::AppState;

/// One edition row in a library item.
#[derive(Clone, Debug, Serialize)]
pub struct LibraryEditionDto {
    pub id: String,
    /// The edition-kind id (string UUID).
    pub kind: String,
    /// Coarse status discriminant (`missing`/`imported`/…).
    pub status_kind: String,
    /// Whether this edition/episode/file is itself monitored (SKADI-T-0594):
    /// `/wanted` drops unmonitored ones — an unmonitored special is not wanted,
    /// however unsatisfied its status is.
    pub monitored: bool,
    /// Human-readable edition-kind name (SKADI-T-0454), e.g. "Theatrical".
    /// `None` when the provider cannot resolve it; clients fall back to `kind`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_name: Option<String>,
    /// Quality id of the imported file, when present.
    pub quality: Option<String>,
    /// Human-readable quality name, e.g. "Bluray-1080p" (SKADI-T-0454).
    ///
    /// The ids are UUIDs, so every client had to resolve them itself against
    /// `/quality/definitions` and the edition-kind registry — or, as the web UI
    /// did, show a UUID. Sending the name alongside the id costs one lookup here
    /// and removes that from every client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_name: Option<String>,
    /// Probed media-info of the imported file (SKADI-T-0238): real
    /// resolution/codec/duration/audio. `None` until the post-import probe runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_info: Option<skadi_core::MediaInfo>,
}

/// One library item, domain-agnostic.
#[derive(Clone, Debug, Serialize)]
pub struct LibraryItemDto {
    /// Media kind (`movie`, `series`, …).
    pub kind: String,
    pub id: String,
    pub title: String,
    pub year: Option<u16>,
    pub monitored: bool,
    pub editions: Vec<LibraryEditionDto>,
}

/// One dated thing on the calendar (SKADI-T-0465).
#[derive(Serialize, Clone, Debug, schemars::JsonSchema)]
pub struct CalendarEntryDto {
    /// Media kind (`movie`, `series`, `audiobook`).
    pub kind: String,
    /// The library item this belongs to, so a client can link to it.
    pub id: String,
    /// What to show: an episode title, a film title, a book title.
    pub title: String,
    /// The parent's name when the entry is a child — a series name for an
    /// episode. `None` for a standalone item.
    pub series: Option<String>,
    /// `S02E05`-style label when the entry is an episode.
    pub episode: Option<String>,
    /// The date this lands on.
    pub date: chrono::NaiveDate,
    /// Whether the file is already in the library — the difference between
    /// "coming up" and "aired but missing", which is the whole reason an
    /// operator looks at this view.
    pub has_file: bool,
    pub monitored: bool,
}

/// What an import list wants applied to the items it adds (SKADI-T-0511).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportListAddOptions {
    /// Whether the item arrives monitored — i.e. whether the sweep may acquire
    /// it. False by default; see `ImportList.add_monitored`.
    pub monitored: bool,
    /// Quality profile to apply, or the domain's default when absent.
    pub profile_id: Option<String>,
    /// Root folder to file it under, or the domain's default when absent.
    pub root_folder: Option<String>,
}

/// A domain's contribution to the unified library view. Registered in
/// [`AppState::library`](crate::state::AppState) by the daemon.
#[async_trait]
pub trait LibraryProvider: Send + Sync {
    /// The domain name (must match the `domains` table key, for the enabled
    /// check).
    fn domain(&self) -> &str;
    /// The media kind this provider yields (for the `?kind=` filter).
    fn kind(&self) -> MediaKind;
    /// Dated items falling in `[from, to]` — the calendar's contribution
    /// (SKADI-T-0465).
    ///
    /// "Dated" means whatever that domain schedules on: a TV episode's air date,
    /// a movie's release date, a book's publication date. The provider decides,
    /// because only it knows which of its dates an operator would expect to see
    /// on a calendar.
    ///
    /// Defaults to **empty** so a domain adopts this incrementally — the same
    /// pattern `items_paged` uses. A calendar that shows two domains out of three
    /// is useful; a compile error in every domain crate the day the route lands
    /// is not.
    async fn calendar(
        &self,
        _from: chrono::NaiveDate,
        _to: chrono::NaiveDate,
    ) -> Result<Vec<CalendarEntryDto>> {
        Ok(Vec::new())
    }

    /// Add an item this domain owns, named by an **external** id, on behalf of an
    /// import list (SKADI-T-0511).
    ///
    /// External id rather than metadata: the provider hands the id to the same
    /// add flow the "add movie" button uses, so a list-added item and a
    /// hand-added one are identical rows. A list that carried its own metadata
    /// would be a second path for the same fact, free to disagree with the first.
    ///
    /// `Ok(false)` means "already present" — not an error, and the common case on
    /// every sync after the first.
    ///
    /// Defaults to **`Err`**, so a domain adopts this incrementally, like
    /// `calendar` and `items_page`. The default is an error rather than
    /// `Ok(false)` on purpose: a list targeting a domain that cannot add would
    /// otherwise report a clean sync that added nothing, forever, with no
    /// indication why.
    async fn add_from_list(
        &self,
        _id_kind: &str,
        _external_id: &str,
        _opts: &ImportListAddOptions,
    ) -> Result<bool> {
        Err(skadi_core::AppError::Internal(format!(
            "domain `{}` cannot add items from an import list yet",
            self.domain()
        )))
    }

    /// The provider's items, optionally pre-filtered by `monitored`.
    ///
    /// `limit`/`offset` are a **query** bound, not a slice of the result: the
    /// point of SKADI-T-0494 is that they reach the database. `/library` used to
    /// await every provider's full item list and then `.skip().take()` the
    /// concatenation, which is why paging shrank the payload and left the latency
    /// alone (427 ms unpaginated, 435 ms at limit=50 over a prod-sized library).
    ///
    /// The default implementation ignores the bound so a provider can adopt this
    /// incrementally; a provider that does must still return the same order.
    async fn items(&self, monitored: Option<bool>) -> Result<Vec<LibraryItemDto>>;

    /// Like [`items`](Self::items) but bounded in the query. Defaults to slicing
    /// [`items`](Self::items), which is the old behaviour — correct, just not
    /// faster.
    async fn items_page(
        &self,
        monitored: Option<bool>,
        limit: Option<usize>,
        offset: usize,
    ) -> Result<Vec<LibraryItemDto>> {
        let all = self.items(monitored).await?;
        Ok(all
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect())
    }

    /// How many items match `monitored`, ignoring paging — the total a client
    /// needs for page controls. Defaults to counting [`items`](Self::items).
    async fn count(&self, monitored: Option<bool>) -> Result<usize> {
        Ok(self.items(monitored).await?.len())
    }
    /// Absolute on-disk folders this domain's items occupy (SKADI-T-0233) — the
    /// immediate-child-of-root directory each item lives under (a movie folder, an
    /// audiobook author folder). Used to compute a root's **unmapped** subfolders. The
    /// default contributes nothing (a domain that can't report occupancy just leaves
    /// its folders looking unmapped).
    async fn occupied_folders(&self) -> Result<Vec<std::path::PathBuf>> {
        Ok(Vec::new())
    }
    /// Genre facet (SKADI-T-0605): every genre this domain's items carry, with
    /// how many items carry it. The default contributes nothing — audiobooks
    /// have no genre field yet, and a domain without one is simply absent from
    /// the facet rather than reported as "no genres".
    async fn genres(&self) -> Result<Vec<GenreCountDto>> {
        Ok(Vec::new())
    }
}

/// One entry of the genre facet: a name and how many items carry it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenreCountDto {
    pub name: String,
    pub count: usize,
}

/// Count genres over a list of per-item genre lists, most frequent first and
/// then by name, so a chip row reads the same on every client. Pure.
#[must_use]
pub fn genre_counts<'a, I>(items: I) -> Vec<GenreCountDto>
where
    I: IntoIterator<Item = &'a [String]>,
{
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for genres in items {
        // A film listing "Drama" twice is one drama film.
        let mut seen = std::collections::HashSet::new();
        for g in genres {
            let g = g.trim();
            if !g.is_empty() && seen.insert(g.to_string()) {
                *counts.entry(g.to_string()).or_insert(0) += 1;
            }
        }
    }
    let mut out: Vec<GenreCountDto> = counts
        .into_iter()
        .map(|(name, count)| GenreCountDto { name, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    out
}

/// The display name of a video quality id, e.g. `"Bluray-1080p"` (SKADI-T-0454).
///
/// Looked up in the built-in ladder, which is where every video quality id comes
/// from. `None` for an id the ladder does not know — including the deliberate
/// "not assessed" sentinel (SKADI-T-0412), which has no name to show.
#[must_use]
pub fn quality_display_name(id: &str) -> Option<String> {
    let parsed = uuid::Uuid::parse_str(id).ok()?;
    skadi_quality::default_definitions()
        .into_iter()
        .find(|d| d.id.into_uuid() == parsed)
        .map(|d| d.name)
}

/// Of a root's on-disk immediate child directories (`children`), those not occupied by
/// any library item (`occupied`, absolute paths). Pure (SKADI-T-0233): compared by full
/// path, returned sorted, de-duplicated.
#[must_use]
pub fn unmapped_subfolders(
    children: &[std::path::PathBuf],
    occupied: &std::collections::HashSet<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = children
        .iter()
        .filter(|c| !occupied.contains(*c))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Lowercase media-kind string (`"movie"`), matching the wire convention.
fn kind_str(kind: MediaKind) -> String {
    format!("{kind:?}").to_lowercase()
}

#[derive(Deserialize)]
struct LibraryParams {
    kind: Option<String>,
    monitored: Option<bool>,
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

/// Routes for the library + activity + history views, merged into the authed
/// API router.
pub fn library_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/library", get(library))
        .route("/wanted", get(wanted))
        .route("/library/genres", get(genres))
        .route("/search-all", post(search_all))
        .route("/activity", get(activity))
        .route("/traces", get(traces))
        .route("/history", get(history))
        .route("/history/counts", get(history_counts))
        .route("/history/{id}", get(history_detail))
        .route(
            "/history/{id}/blocklist-and-search",
            post(history_blocklist_and_search),
        )
        .route("/decisions", get(decisions))
        .route("/downloads", get(downloads))
        .route("/downloads/worker", get(worker_status))
        .route("/downloads/vpn", get(vpn_status))
        .route(
            "/downloads/settings",
            get(download_settings).put(set_download_settings),
        )
        .route("/downloads/pause-all", post(pause_all))
        .route("/downloads/resume-all", post(resume_all))
        .route("/downloads/{id}", delete(remove_download))
        .route("/downloads/{id}/pause", post(pause_download))
        .route("/downloads/{id}/resume", post(resume_download))
        .route("/downloads/categories", get(list_categories))
        .route(
            "/downloads/categories/{name}",
            put(upsert_category).delete(delete_category),
        )
        .route("/downloads/import", post(manual_import))
        .route("/downloads/import/preview", post(manual_import_preview))
}

async fn library(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LibraryParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let store = state.store.as_ref().ok_or_else(|| {
        ApiError(skadi_core::AppError::Internal(
            "store not configured".into(),
        ))
    })?;

    // A single-domain, unfiltered request can page in the database
    // (SKADI-T-0494). Cross-domain paging still has to aggregate first — the
    // order is defined over the union — and `?q=` filters on a field the
    // providers do not query on, so both fall back to the old path rather than
    // returning a wrong page. Those are the cases worth splitting next.
    let can_push_down = params.q.is_none() && params.kind.is_some();

    let mut items: Vec<LibraryItemDto> = Vec::new();
    let mut total: usize = 0;
    for provider in &state.library {
        // `?kind=` selects a single domain's kind.
        if let Some(k) = &params.kind
            && &kind_str(provider.kind()) != k
        {
            continue;
        }
        // Only enabled domains contribute.
        let enabled = store
            .get(provider.domain())
            .await?
            .map(|s| s.enabled)
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        if can_push_down {
            total += provider.count(params.monitored).await?;
            items.extend(
                provider
                    .items_page(params.monitored, params.limit, params.offset.unwrap_or(0))
                    .await?,
            );
        } else {
            items.extend(provider.items(params.monitored).await?);
        }
    }

    // `?q=` is a case-insensitive title substring filter.
    if let Some(q) = params.q.as_ref().map(|q| q.to_lowercase()) {
        items.retain(|i| i.title.to_lowercase().contains(&q));
    }

    let page: Vec<LibraryItemDto> = if can_push_down {
        // Already bounded by the query.
        items
    } else {
        // Cross-domain or text-filtered: aggregate, then slice. The total is the
        // filtered set, which is what a client needs to page over.
        total = items.len();
        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(usize::MAX);
        items.into_iter().skip(offset).take(limit).collect()
    };

    // Every list endpoint should say how many rows exist, so a client can render
    // page controls; `/history` was the only one that did (SKADI-T-0494).
    Ok(([("x-total-count", total.to_string())], Json(page)))
}

/// An edition is **satisfied** (not wanted) once it's imported at or above cutoff.
/// Everything else — missing, searching, snatched, downloading, failed — is still
/// "wanted" (the daemon hasn't put a good file on disk yet).
const SATISFIED_STATUS: [&str; 2] = ["imported", "cutoff"];

/// Counts over the wanted set (computed before pagination).
#[derive(Serialize, Default)]
struct WantedSummary {
    /// Number of wanted items (with ≥1 unsatisfied edition).
    items: usize,
    /// Total unsatisfied editions across those items.
    editions: usize,
    /// Unsatisfied-edition counts keyed by status (`missing`/`failed`/…) — the
    /// "why it's still wanted" breakdown.
    by_status: std::collections::BTreeMap<String, usize>,
}

/// `GET /wanted` response: a summary plus the wanted items (editions filtered to
/// the unsatisfied ones).
#[derive(Serialize)]
struct WantedResponse {
    summary: WantedSummary,
    items: Vec<LibraryItemDto>,
}

/// `GET /wanted?kind=&q=&limit=&offset=` — the acquisition backlog: monitored items
/// that still need a file (SKADI-T-0191). Mirrors `GET /library` aggregation but
/// keeps only monitored items, drops satisfied editions, and adds a `by_status`
/// summary so the UI can show "N missing, M failed-retrying" at a glance.
///
/// Scope (v0): "wanted" = no good file yet. Cutoff-unmet *upgrades* (an imported
/// file below the format/quality cutoff) are a separate axis the per-edition status
/// doesn't carry here; surfacing them is a follow-up.
/// `GET /library/genres?kind=movie|series` — the genre facet (SKADI-T-0605):
/// `[{name, count}]`, most frequent first. Without `kind`, every domain's
/// genres are merged (a name counts once per item across domains).
async fn genres(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LibraryParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let mut merged: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for provider in &state.library {
        if let Some(k) = &params.kind
            && &kind_str(provider.kind()) != k
        {
            continue;
        }
        for g in provider.genres().await.map_err(ApiError)? {
            *merged.entry(g.name).or_insert(0) += g.count;
        }
    }
    let mut out: Vec<GenreCountDto> = merged
        .into_iter()
        .map(|(name, count)| GenreCountDto { name, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(out))
}

async fn wanted(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LibraryParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let store = state.store.as_ref().ok_or_else(|| {
        ApiError(skadi_core::AppError::Internal(
            "store not configured".into(),
        ))
    })?;

    let mut items: Vec<LibraryItemDto> = Vec::new();
    for provider in &state.library {
        if let Some(k) = &params.kind
            && &kind_str(provider.kind()) != k
        {
            continue;
        }
        let enabled = store
            .get(provider.domain())
            .await?
            .map(|s| s.enabled)
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        // Only monitored items are "wanted".
        for mut item in provider.items(Some(true)).await? {
            item.editions
                .retain(|e| e.monitored && !SATISFIED_STATUS.contains(&e.status_kind.as_str()));
            if !item.editions.is_empty() {
                items.push(item);
            }
        }
    }

    if let Some(q) = params.q.as_ref().map(|q| q.to_lowercase()) {
        items.retain(|i| i.title.to_lowercase().contains(&q));
    }

    // Summary over the whole wanted set, before pagination.
    let mut summary = WantedSummary {
        items: items.len(),
        ..Default::default()
    };
    for it in &items {
        for e in &it.editions {
            summary.editions += 1;
            *summary.by_status.entry(e.status_kind.clone()).or_default() += 1;
        }
    }

    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(usize::MAX);
    let page: Vec<LibraryItemDto> = items.into_iter().skip(offset).take(limit).collect();

    // The total is `summary.items`, computed over the whole wanted set before
    // paging — the header just surfaces it where a paged client looks
    // (SKADI-T-0468).
    Ok((
        [("x-total-count", summary.items.to_string())],
        Json(WantedResponse {
            summary,
            items: page,
        }),
    ))
}

/// `POST /search-all` — manually trigger an immediate sweep of all wanted items
/// across every running domain worker, without waiting for the scheduled timer
/// (the Radarr "Search All Missing" control, SKADI-T-0193).
///
/// Fire-and-forget: it pokes the process-global sweep trigger and returns
/// `202 Accepted` straight away — the actual search/grab happens asynchronously in
/// the hunter workers (watch `/activity` for progress). Idempotent under rapid
/// repeats: overlapping pokes collapse into one follow-up sweep.
async fn search_all() -> impl IntoResponse {
    skadi_hunter::request_sweep();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "sweep requested" })),
    )
}

async fn activity() -> impl IntoResponse {
    // In-memory, no DB call; empty when idle.
    Json(skadi_hunter::tracker().snapshot())
}

/// One persistent history row on the wire (SKADI-T-0082).
#[derive(Serialize)]
struct HistoryDto {
    id: String,
    /// RFC 3339 timestamp.
    at: String,
    kind: String,
    acquirable_ref: String,
    label: String,
    /// `grabbed` / `imported` / `failed`.
    event: String,
    detail: Option<String>,
    /// Structured failure reason code (e.g. `import_failed`), for `failed` events.
    reason_code: Option<String>,
}

#[derive(Deserialize)]
struct HistoryParams {
    limit: Option<i64>,
    offset: Option<i64>,
    /// `grabbed` / `imported` / `failed`.
    event: Option<String>,
    /// Domain kind (`movie` / `audiobook`).
    kind: Option<String>,
    /// Scope to one acquirable ref.
    acquirable: Option<String>,
    /// Structured failure reason code (e.g. `import_failed`).
    reason_code: Option<String>,
    /// RFC 3339 inclusive lower time bound.
    since: Option<String>,
    /// RFC 3339 exclusive upper time bound.
    until: Option<String>,
}

/// Parse an RFC 3339 timestamp query param into UTC, surfacing a 400 on garbage.
fn parse_ts(s: &str) -> std::result::Result<chrono::DateTime<chrono::Utc>, ApiError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .map_err(|e| {
            ApiError(skadi_core::AppError::Validation(format!(
                "bad timestamp {s:?}: {e}"
            )))
        })
}

/// How long without a heartbeat before the download worker is considered down
/// (SKADI-T-0288). Generous vs the worker's tick (seconds) so a brief hiccup
/// doesn't flap the status.
const WORKER_STALE_SECS: i64 = 45;

/// The worker-liveness payload.
#[derive(Serialize)]
struct WorkerStatusDto {
    /// `true` when the worker heartbeated within [`WORKER_STALE_SECS`].
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// Seconds since the last heartbeat (`None` if never seen).
    #[serde(skip_serializing_if = "Option::is_none")]
    age_secs: Option<i64>,
    stale_after_secs: i64,
    /// Free / total bytes on the filesystem backing the download dir — an always-on
    /// stat for the control panel (`None` if the path is unset/unreadable).
    #[serde(skip_serializing_if = "Option::is_none")]
    free_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_bytes: Option<u64>,
}

/// Free/total bytes on the filesystem backing the worker's download dir.
async fn download_dir_space(store: &skadi_store::Store) -> (Option<u64>, Option<u64>) {
    let dir = crate::load_config_view(store)
        .await
        .ok()
        .and_then(|v| v.get_string("worker.download_dir").ok())
        .filter(|s| !s.trim().is_empty());
    match dir.and_then(|d| crate::diagnostics::disk_space(std::path::Path::new(&d))) {
        Some((free, total)) => (Some(free), Some(total)),
        None => (None, None),
    }
}

/// `GET /downloads/worker` — built-in torrent worker liveness from its heartbeat
/// (SKADI-T-0288). Reflects the **worker process**, independent of whether a
/// downloader is registered; a never-seen worker reports `running: false`.
async fn worker_status(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::WorkerStatusRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let window = chrono::Duration::seconds(WORKER_STALE_SECS);
    let (free_bytes, total_bytes) = download_dir_space(store).await;
    let dto = match store.latest_worker_status().await? {
        Some(s) => {
            let age = chrono::Utc::now()
                .signed_duration_since(s.last_seen_at)
                .num_seconds();
            WorkerStatusDto {
                running: s.is_fresh(window),
                worker_id: Some(s.worker_id),
                last_seen_at: Some(s.last_seen_at),
                version: Some(s.version),
                age_secs: Some(age),
                stale_after_secs: WORKER_STALE_SECS,
                free_bytes,
                total_bytes,
            }
        }
        None => WorkerStatusDto {
            running: false,
            worker_id: None,
            last_seen_at: None,
            version: None,
            age_secs: None,
            stale_after_secs: WORKER_STALE_SECS,
            free_bytes,
            total_bytes,
        },
    };
    Ok(Json(dto))
}

/// `GET /downloads/vpn` — gluetun tunnel state + exit IP/location (SKADI-T-0292).
/// Read-only: the VPN is always-on, with gluetun's firewall as the structural
/// kill-switch — there's deliberately no control to disable it.
/// The hot-reloadable worker settings (SKADI-T-0291): bandwidth caps, the active-
/// download cap, and seed policy. Persisted to the shared config plane; the worker
/// re-reads + applies them within a tick — no restart. `0` ⇒ unlimited on an axis.
#[derive(Serialize, Deserialize)]
struct DownloadSettingsDto {
    /// Max simultaneous active downloads (`0` ⇒ unlimited).
    max_active: u64,
    /// Global download cap, bytes/sec (`0` ⇒ unlimited).
    down_limit_bps: u64,
    /// Global upload cap, bytes/sec (`0` ⇒ unlimited).
    up_limit_bps: u64,
    /// Stop seeding once uploaded/downloaded ≥ ratio (`0` ⇒ no ratio limit).
    seed_ratio: f64,
    /// Stop seeding after this many minutes (`0` ⇒ no time limit).
    seed_time_mins: u64,
    /// On hitting a seed limit: `stop` (keep data) or `remove` (drop the torrent).
    seed_action: String,
}

/// `GET /downloads/settings` — the current worker settings from the config plane.
async fn download_settings(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let view = crate::load_config_view(store).await?;
    let dto = DownloadSettingsDto {
        max_active: view.get_u64("worker.max_active").unwrap_or(0),
        down_limit_bps: view.get_u64("worker.down_limit_bps").unwrap_or(0),
        up_limit_bps: view.get_u64("worker.up_limit_bps").unwrap_or(0),
        seed_ratio: view
            .get_string("worker.seed_ratio")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0.0),
        seed_time_mins: view.get_u64("worker.seed_time_mins").unwrap_or(0),
        seed_action: view
            .get_string("worker.seed_action")
            .unwrap_or_else(|_| "stop".into()),
    };
    Ok(Json(dto))
}

/// `PUT /downloads/settings` — persist worker settings (`source = runtime`); the
/// worker hot-applies them within a tick (SKADI-T-0291). `seed_action` is clamped
/// to the two valid values so a bad client can't wedge the worker's parse.
async fn set_download_settings(
    State(state): State<Arc<AppState>>,
    Json(dto): Json<DownloadSettingsDto>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{ConfigRepo, ConfigSource};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let action = if dto.seed_action.trim().eq_ignore_ascii_case("remove") {
        "remove"
    } else {
        "stop"
    };
    let pairs: [(&str, String); 6] = [
        ("worker.max_active", dto.max_active.to_string()),
        ("worker.down_limit_bps", dto.down_limit_bps.to_string()),
        ("worker.up_limit_bps", dto.up_limit_bps.to_string()),
        ("worker.seed_ratio", format!("{}", dto.seed_ratio)),
        ("worker.seed_time_mins", dto.seed_time_mins.to_string()),
        ("worker.seed_action", action.to_string()),
    ];
    for (k, v) in pairs {
        store.set_config(k, &v, ConfigSource::Runtime).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn vpn_status() -> impl IntoResponse {
    Json(crate::vpn::status().await)
}

/// `GET /history?event=&kind=&acquirable=&since=&until=&limit=&offset=` — the
/// persistent acquisition history, newest first, with server-side filters
/// (SKADI-T-0194). Default 50 rows, capped at 500. The total number of matching
/// rows (ignoring paging) is returned in the `X-Total-Count` header for paging.
async fn history(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{HistoryQuery, HistoryRepo};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let limit = params.limit.unwrap_or(50).clamp(1, 500);
    let offset = params.offset.unwrap_or(0).max(0);
    let query = HistoryQuery {
        event: params.event,
        kind: params.kind,
        acquirable_ref: params.acquirable,
        reason_code: params.reason_code,
        since: params.since.as_deref().map(parse_ts).transpose()?,
        until: params.until.as_deref().map(parse_ts).transpose()?,
    };
    let total = store.count_history(&query).await?;
    let rows = store.list_history_filtered(&query, limit, offset).await?;
    let dtos: Vec<HistoryDto> = rows.into_iter().map(history_dto).collect();
    Ok(([("x-total-count", total.to_string())], Json(dtos)))
}

/// One hunter trace event on the wire (SKADI-T-0323).
#[derive(Serialize)]
struct TraceDto {
    id: String,
    /// RFC 3339 timestamp.
    at: String,
    run_id: Option<String>,
    kind: String,
    acquirable_ref: String,
    /// Pipeline stage: `searching` / `deciding` / `grabbing` / `downloading` / `importing`.
    stage: String,
    /// Machine event label, e.g. `candidates_found`, `decision`, `snatched`.
    event: String,
    /// Human one-line summary.
    message: String,
    detail: Option<String>,
}

#[derive(Deserialize)]
struct TraceParams {
    limit: Option<i64>,
    offset: Option<i64>,
    /// Scope to one acquirable ref (per-item trace view).
    acquirable: Option<String>,
}

/// `GET /traces` — the hunter's structured per-step trace stream, newest first
/// (SKADI-T-0323). Default 100 rows, capped at 500. With `?acquirable=` scopes to
/// one item. The richer companion to `/history` (coarse outcomes).
async fn traces(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TraceParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::TraceRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let offset = params.offset.unwrap_or(0).max(0);
    // Scoped to one acquirable the rows are the whole set; otherwise count in SQL
    // so the total does not cost a full load (SKADI-T-0468).
    let (rows, total) = match &params.acquirable {
        Some(aref) => {
            let rows = store.traces_for(aref).await?;
            let total = rows.len() as i64;
            (rows, total)
        }
        None => (
            store.list_traces(limit, offset).await?,
            store.count_traces().await?,
        ),
    };
    let dtos: Vec<TraceDto> = rows.into_iter().map(trace_dto).collect();
    Ok(([("x-total-count", total.to_string())], Json(dtos)))
}

fn trace_dto(e: skadi_store::TraceEvent) -> TraceDto {
    TraceDto {
        id: e.id,
        at: e.at.to_rfc3339(),
        run_id: e.run_id,
        kind: e.kind,
        acquirable_ref: e.acquirable_ref,
        stage: e.stage,
        event: e.event,
        message: e.message,
        detail: e.detail,
    }
}

fn history_dto(e: skadi_store::HistoryEntry) -> HistoryDto {
    HistoryDto {
        id: e.id,
        at: e.at.to_rfc3339(),
        kind: e.kind,
        acquirable_ref: e.acquirable_ref,
        label: e.label,
        event: e.event,
        detail: e.detail,
        reason_code: e.reason_code,
    }
}

/// `GET /history/{id}` — one history event (the per-event detail read,
/// SKADI-T-0197). 404 when absent.
async fn history_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::HistoryRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let entry = store.get_history(&id).await?.ok_or_else(|| {
        ApiError(skadi_core::AppError::NotFound(format!(
            "history {id} not found"
        )))
    })?;
    Ok(Json(history_dto(entry)))
}

/// `POST /history/{id}/blocklist-and-search` (SKADI-T-0197) — the Activity "this
/// release was bad, find another" action: blocklist the release that was grabbed
/// for this item (looked up via the latest `decision_history.release_key` for its
/// acquirable), then poke the sweep so the next pass re-decides over a fresh
/// candidate set. Returns what it did. If no grabbed release is on record (e.g. the
/// item never got past search), it still kicks the sweep.
async fn history_blocklist_and_search(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{BlocklistRepo, DecisionHistoryRepo, HistoryRepo, NewBlocklistEntry};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;

    let entry = store.get_history(&id).await?.ok_or_else(|| {
        ApiError(skadi_core::AppError::NotFound(format!(
            "history {id} not found"
        )))
    })?;

    // The grabbed release for this item = the most recent decision's release_key.
    let latest = store
        .decisions_for(&entry.acquirable_ref)
        .await?
        .into_iter()
        .find(|d| d.release_key.is_some());
    let blocklisted_key = if let Some(decision) = latest {
        let release_key = decision.release_key.clone().expect("filtered to Some");
        store
            .block(&NewBlocklistEntry {
                release_key: release_key.clone(),
                title: decision.title,
                acquirable_ref: Some(entry.acquirable_ref.clone()),
                indexer: None,
                reason: Some("blocklist-and-search".into()),
                expires_at: None,
            })
            .await?;
        Some(release_key)
    } else {
        None
    };

    // Kick a sweep so the item is re-decided without the now-blocked release.
    skadi_hunter::request_sweep();

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "blocklisted": blocklisted_key.is_some(),
            "release_key": blocklisted_key,
            "sweep_requested": true,
        })),
    ))
}

/// `GET /history/counts` — whole-table per-event totals for dashboard badges
/// (SKADI-T-0194).
async fn history_counts(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::HistoryRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let c = store.history_counts().await?;
    Ok(Json(serde_json::json!({
        "total": c.total,
        "grabbed": c.grabbed,
        "imported": c.imported,
        "failed": c.failed,
    })))
}

/// One persisted decision on the wire (SKADI-T-0187): why a release was grabbed.
#[derive(Serialize)]
struct DecisionDto {
    id: String,
    /// RFC 3339 timestamp.
    at: String,
    kind: String,
    acquirable_ref: String,
    /// The chosen release title.
    title: String,
    /// Classified quality name, when known.
    quality: Option<String>,
    /// `Accept` / `Upgrade` / … when classified.
    decision: Option<String>,
    /// Aggregate custom-format score.
    format_score: i32,
    /// Stable blocklist identity of the grabbed release (grab→import correlation;
    /// powers blocklist-and-search). `None` for pre-T-0196 rows.
    release_key: Option<String>,
    /// The full `ReleaseExplanation` (matched formats, rank, reason) as JSON.
    explanation: serde_json::Value,
}

/// `GET /decisions?limit=&offset=&acquirable=` — the persisted decision history
/// (why each release was grabbed), newest first. With `acquirable`, scope to one
/// acquirable. Default 50 rows, capped at 500.
async fn decisions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DecisionParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DecisionHistoryRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    // As for /traces (SKADI-T-0468): scoped means the rows are the whole set,
    // otherwise count in SQL rather than loading everything to size it.
    let (rows, total) = if let Some(aref) = params.acquirable {
        let rows = store.decisions_for(&aref).await?;
        let total = rows.len() as i64;
        (rows, total)
    } else {
        let limit = params.limit.unwrap_or(50).clamp(1, 500);
        let offset = params.offset.unwrap_or(0).max(0);
        (
            store.list_decisions(limit, offset).await?,
            store.count_decisions().await?,
        )
    };
    let dtos: Vec<DecisionDto> = rows
        .into_iter()
        .map(|e| DecisionDto {
            id: e.id,
            at: e.at.to_rfc3339(),
            kind: e.kind,
            acquirable_ref: e.acquirable_ref,
            title: e.title,
            quality: e.quality,
            decision: e.decision,
            format_score: e.format_score,
            release_key: e.release_key,
            // Parse the stored JSON back so the client gets a structured object,
            // not a string; fall back to null if it's somehow unparseable.
            explanation: serde_json::from_str(&e.explanation).unwrap_or(serde_json::Value::Null),
        })
        .collect();
    Ok(([("x-total-count", total.to_string())], Json(dtos)))
}

#[derive(Deserialize)]
struct DecisionParams {
    limit: Option<i64>,
    offset: Option<i64>,
    acquirable: Option<String>,
}

/// One active download job on the wire (SKADI-T-0166): live torrent metrics from
/// the built-in worker. `percent` / `ratio` are computed; speeds are bytes/sec.
#[derive(Serialize)]
struct DownloadDto {
    id: String,
    acquirable_ref: String,
    info_hash: Option<String>,
    /// `queued` / `downloading`.
    status: String,
    progress_bytes: i64,
    total_bytes: i64,
    /// 0–100, computed from progress / total.
    percent: f64,
    down_speed_bps: Option<i64>,
    up_speed_bps: Option<i64>,
    uploaded_bytes: Option<i64>,
    /// Seed ratio = uploaded / downloaded (None until there's downloaded data).
    ratio: Option<f64>,
    /// Connected (live) peers, and total peers seen this session. librqbit does
    /// not classify seeders vs leechers.
    peers: Option<i32>,
    peers_seen: Option<i32>,
    eta_seconds: Option<i64>,
    error: Option<String>,
}

/// `GET /downloads` — every torrent **under active management**: downloading,
/// queued, paused, and **seeding** (completed-but-still-uploading) — with live
/// metrics, so the Downloaders page is a real torrent client, not just an
/// in-flight list (SKADI-T-0169). Failed jobs live in `/history`.
/// Paging for `/downloads` (SKADI-T-0494).
///
/// Applied **after** the dedup-by-info-hash and the stable sort, not in the
/// query: the order this endpoint promises is defined over the deduplicated set,
/// so a SQL `LIMIT` would hand back a different — and wrong — page. That makes
/// this payload relief rather than a scan fix, which is honest for an endpoint
/// that already answers in ~23 ms; the total still reflects the whole set.
#[derive(Deserialize, Default)]
struct DownloadsParams {
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn downloads(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DownloadsParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{DownloadJobRepo, DownloadJobStatus};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    // The status filter reaches the database (SKADI-T-0494). This used to load
    // every row — including the removed and failed ones that accumulate forever —
    // and discard most of them here.
    //
    // The *bound* deliberately stays in the handler: the dedup below collapses
    // rows by `info_hash` across the whole set, so a LIMIT pushed down would hand
    // this a page it cannot correctly deduplicate.
    let managed = store
        .list_downloads_with_status(&[
            DownloadJobStatus::Queued,
            DownloadJobStatus::Downloading,
            DownloadJobStatus::Paused,
            DownloadJobStatus::Stalled,
            DownloadJobStatus::Completed,
        ])
        .await?
        .into_iter();
    // One entry per real torrent: the same torrent can have several rows (legacy
    // re-acquires before enqueue-dedup, SKADI-T-0167). Collapse by info_hash,
    // keeping the freshest row; rows without a resolved hash stay individual.
    let mut by_hash: std::collections::HashMap<String, skadi_store::DownloadJob> =
        std::collections::HashMap::new();
    let mut no_hash: Vec<skadi_store::DownloadJob> = Vec::new();
    for j in managed {
        match j.info_hash.clone() {
            Some(h) => {
                by_hash
                    .entry(h)
                    .and_modify(|cur| {
                        if j.updated_at > cur.updated_at {
                            *cur = j.clone();
                        }
                    })
                    .or_insert(j);
            }
            None => no_hash.push(j),
        }
    }
    let mut jobs: Vec<skadi_store::DownloadJob> =
        no_hash.into_iter().chain(by_hash.into_values()).collect();
    // Deterministic, stable order so the UI doesn't reshuffle every poll
    // (SKADI-T-0170): active (downloading / paused / queued) before seeding, then
    // by a stable identity (info_hash, else id) — NOT by a changing field.
    let rank = |s: DownloadJobStatus| match s {
        DownloadJobStatus::Downloading => 0u8,
        DownloadJobStatus::Stalled => 1,
        DownloadJobStatus::Paused => 2,
        DownloadJobStatus::Queued => 3,
        _ => 4,
    };
    jobs.sort_by(|a, b| {
        rank(a.status).cmp(&rank(b.status)).then_with(|| {
            a.info_hash
                .as_deref()
                .unwrap_or(&a.id)
                .cmp(b.info_hash.as_deref().unwrap_or(&b.id))
        })
    });
    let total = jobs.len();
    let jobs: Vec<skadi_store::DownloadJob> = jobs
        .into_iter()
        .skip(params.offset.unwrap_or(0))
        .take(params.limit.unwrap_or(usize::MAX))
        .collect();
    let dtos: Vec<DownloadDto> = jobs
        .into_iter()
        .map(|j| {
            let percent = if j.total_bytes > 0 {
                (j.progress_bytes as f64 / j.total_bytes as f64 * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            let ratio = j
                .uploaded_bytes
                .filter(|_| j.progress_bytes > 0)
                .map(|up| up as f64 / j.progress_bytes as f64);
            // A completed-but-still-managed torrent is "seeding" to the UI.
            let status = match j.status {
                DownloadJobStatus::Completed => "seeding",
                other => other.as_str(),
            }
            .to_string();
            DownloadDto {
                id: j.id,
                acquirable_ref: j.acquirable_ref,
                info_hash: j.info_hash,
                status,
                progress_bytes: j.progress_bytes,
                total_bytes: j.total_bytes,
                percent,
                down_speed_bps: j.down_speed_bps,
                up_speed_bps: j.up_speed_bps,
                uploaded_bytes: j.uploaded_bytes,
                ratio,
                peers: j.peers,
                peers_seen: j.peers_seen,
                eta_seconds: j.eta_seconds,
                error: j.error,
            }
        })
        .collect();
    Ok(([("x-total-count", total.to_string())], Json(dtos)))
}

#[derive(Deserialize)]
struct RemoveParams {
    /// `true` also deletes the downloaded files (drop data); default keeps them.
    delete_data: Option<bool>,
}

/// `DELETE /downloads/{id}?delete_data=` — remove a download (SKADI-T-0168). Flags
/// the row `remove_requested`; the worker then tells librqbit to forget (keep
/// files) or delete (drop files) and marks it `removed`. Also clears a wedged job.
async fn remove_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<RemoveParams>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DownloadJobRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    store
        .request_remove(&id, params.delete_data.unwrap_or(false))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /downloads/{id}/pause` — pause an active download (SKADI-T-0168).
async fn pause_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DownloadJobRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    store.request_pause(&id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /downloads/{id}/resume` — resume a paused download (SKADI-T-0168).
async fn resume_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DownloadJobRepo;
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    store.resume(&id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /downloads/pause-all` — pause every in-flight transfer (back-pressure
/// relief for the whole queue). Idempotent; already-paused/seeding rows are left.
async fn pause_all(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{DownloadJobRepo, DownloadJobStatus};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    for j in store.list_downloads().await? {
        if matches!(
            j.status,
            DownloadJobStatus::Downloading | DownloadJobStatus::Queued | DownloadJobStatus::Stalled
        ) {
            let _ = store.request_pause(&j.id).await;
        }
    }
    Ok(StatusCode::ACCEPTED)
}

/// `POST /downloads/resume-all` — resume every paused transfer.
async fn resume_all(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{DownloadJobRepo, DownloadJobStatus};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    for j in store.list_downloads().await? {
        if matches!(j.status, DownloadJobStatus::Paused) {
            let _ = store.resume(&j.id).await;
        }
    }
    Ok(StatusCode::ACCEPTED)
}

/// A download category (SKADI-T-0215): a save-path + optional per-category seed
/// overrides. `null` override fields fall back to the worker's global settings.
#[derive(Serialize, Deserialize)]
struct CategoryDto {
    name: String,
    save_path: Option<String>,
    seed_ratio: Option<f64>,
    seed_time_mins: Option<i64>,
    seed_action: Option<String>,
}

impl From<skadi_store::DownloadCategory> for CategoryDto {
    fn from(c: skadi_store::DownloadCategory) -> Self {
        CategoryDto {
            name: c.name,
            save_path: c.save_path,
            seed_ratio: c.seed_ratio,
            seed_time_mins: c.seed_time_mins,
            seed_action: c.seed_action,
        }
    }
}

fn store_of(state: &Arc<AppState>) -> std::result::Result<&skadi_store::Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))
}

/// `POST /downloads/import` request: import files from an operator-supplied path into
/// a chosen acquirable (SKADI-T-0222, the *arr "Manual Import" surface).
#[derive(Deserialize)]
struct ManualImportRequest {
    /// A file or directory the operator points at (scanned recursively).
    path: String,
    /// Media kind (`movie`, `audiobook`, …) selecting the domain's importer.
    kind: String,
    /// The target acquirable (e.g. an encoded movie-edition id) the files satisfy.
    acquirable_ref: String,
    /// Optional category tag carried on the synthetic handle.
    category: Option<String>,
}

/// `POST /downloads/import` response — the import outcome counts + details.
#[derive(Serialize)]
struct ManualImportResponse {
    imported: Vec<String>,
    rejected: Vec<(String, String)>,
    replaced: Vec<String>,
    failed: Vec<(String, String)>,
}

/// `POST /downloads/import` — scan an operator path and import its files into the
/// given acquirable via the domain's importer (SKADI-T-0222).
/// Shared by `manual_import` + `manual_import_preview`: resolve the domain importer
/// for `body.kind`/`acquirable_ref` and scan `body.path` into a `CompletedDownload`.
async fn resolve_manual_import(
    state: &Arc<AppState>,
    body: &ManualImportRequest,
) -> std::result::Result<
    (
        std::sync::Arc<dyn skadi_importer::Importer>,
        skadi_importer::CompletedDownload,
    ),
    ApiError,
> {
    let kind = state
        .library
        .iter()
        .map(|p| p.kind())
        .find(|k| kind_str(*k) == body.kind.to_lowercase())
        .ok_or_else(|| {
            ApiError(skadi_core::AppError::Validation(format!(
                "unknown or unregistered media kind {:?}",
                body.kind
            )))
        })?;
    let svc = skadi_hunter::try_services_for(kind).ok_or_else(|| {
        ApiError(skadi_core::AppError::Internal(format!(
            "hunter services not initialized for {kind:?}"
        )))
    })?;
    let acquirable = skadi_importer::AcquirableRef(body.acquirable_ref.clone());
    let importer = if let Some(factory) = svc.importer_factory.as_ref() {
        factory.for_acquirable(&acquirable).await?
    } else {
        svc.importer.clone()
    };

    let files = skadi_importer::scan_files(std::path::Path::new(&body.path)).map_err(|e| {
        ApiError(skadi_core::AppError::Validation(format!(
            "cannot scan {:?}: {e}",
            body.path
        )))
    })?;
    if files.is_empty() {
        return Err(ApiError(skadi_core::AppError::Validation(format!(
            "no files found under {:?}",
            body.path
        ))));
    }
    let category = body
        .category
        .clone()
        .unwrap_or_else(|| "manual".to_string());
    let completed = skadi_importer::CompletedDownload {
        handle: skadi_downloaders::DownloadHandle {
            native_id: "manual-import".to_string(),
            category: category.clone(),
        },
        files,
        category,
    };
    Ok((importer, completed))
}

async fn manual_import(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManualImportRequest>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let (importer, completed) = resolve_manual_import(&state, &body).await?;
    let outcome = importer.import(completed).await?;

    let path_str = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    Ok(Json(ManualImportResponse {
        imported: outcome
            .imported
            .into_iter()
            .map(|f| path_str(f.file.path))
            .collect(),
        rejected: outcome
            .rejected
            .into_iter()
            .map(|(p, r)| (path_str(p), r))
            .collect(),
        replaced: outcome.replaced.into_iter().map(path_str).collect(),
        failed: outcome
            .failed
            .into_iter()
            .map(|(p, r)| (path_str(p), r))
            .collect(),
    }))
}

/// `POST /downloads/import/preview` response: a dry-run plan (no files touched).
#[derive(Serialize)]
struct ImportPlanResponse {
    /// `(acquirable_ref, dest, action)` where action is `place` or `replace`.
    would_import: Vec<(String, String, String)>,
    would_reject: Vec<(String, String)>,
    would_replace: Vec<String>,
}

/// `POST /downloads/import/preview` — dry-run a manual import (SKADI-T-0224): the same
/// matching + collision/space/sample decisions, returning the plan without placing.
async fn manual_import_preview(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManualImportRequest>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let (importer, completed) = resolve_manual_import(&state, &body).await?;
    let plan = importer.preview(completed).await?;

    let path_str = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    let action_str = |a: skadi_importer::PlannedAction| match a {
        skadi_importer::PlannedAction::Place => "place".to_string(),
        skadi_importer::PlannedAction::Replace => "replace".to_string(),
    };
    Ok(Json(ImportPlanResponse {
        would_import: plan
            .would_import
            .into_iter()
            .map(|p| (p.acquirable.0, path_str(p.dest), action_str(p.action)))
            .collect(),
        would_reject: plan
            .would_reject
            .into_iter()
            .map(|(p, r)| (path_str(p), r))
            .collect(),
        would_replace: plan.would_replace.into_iter().map(path_str).collect(),
    }))
}

/// `GET /downloads/categories` — list all download categories.
async fn list_categories(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DownloadCategoryRepo;
    let cats = store_of(&state)?.list_categories().await?;
    Ok(Json(
        cats.into_iter().map(CategoryDto::from).collect::<Vec<_>>(),
    ))
}

/// `PUT /downloads/categories/{name}` — create or replace a category. The path name
/// is authoritative (the body's `name`, if any, is ignored).
async fn upsert_category(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<CategoryDto>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DownloadCategoryRepo;
    let cat = skadi_store::DownloadCategory {
        name,
        save_path: body.save_path,
        seed_ratio: body.seed_ratio,
        seed_time_mins: body.seed_time_mins,
        seed_action: body.seed_action,
    };
    let stored = store_of(&state)?.upsert_category(&cat).await?;
    Ok(Json(CategoryDto::from(stored)))
}

/// `DELETE /downloads/categories/{name}` — remove a category (404 if absent).
async fn delete_category(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::DownloadCategoryRepo;
    if store_of(&state)?.delete_category(&name).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(skadi_core::AppError::NotFound(format!(
            "no category {name:?}"
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::unmapped_subfolders;
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[test]
    fn unmapped_subfolders_excludes_occupied_and_sorts() {
        let children: Vec<PathBuf> = ["/m/The_Matrix_(1999)", "/m/Loose_Files", "/m/Akira_(1988)"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let occupied: HashSet<PathBuf> = ["/m/The_Matrix_(1999)", "/m/Akira_(1988)"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let got = unmapped_subfolders(&children, &occupied);
        assert_eq!(got, vec![PathBuf::from("/m/Loose_Files")]);

        // Everything occupied → nothing unmapped.
        assert!(unmapped_subfolders(&children[..1], &occupied).is_empty());
        // Nothing occupied → all unmapped, sorted.
        let all = unmapped_subfolders(&children, &HashSet::new());
        assert_eq!(
            all,
            vec![
                PathBuf::from("/m/Akira_(1988)"),
                PathBuf::from("/m/Loose_Files"),
                PathBuf::from("/m/The_Matrix_(1999)"),
            ]
        );
    }
}

#[cfg(test)]
mod genre_facet_tests {
    use super::{GenreCountDto, genre_counts};

    #[test]
    fn counts_once_per_item_most_common_first_then_by_name() {
        let items: Vec<Vec<String>> = vec![
            vec!["Drama".into(), "Drama".into(), "Crime".into()],
            vec!["Crime".into(), " Thriller ".into()],
            vec![],
            vec!["Drama".into(), "".into()],
        ];
        let got = genre_counts(items.iter().map(Vec::as_slice));
        assert_eq!(
            got,
            vec![
                GenreCountDto {
                    name: "Crime".into(),
                    count: 2
                },
                GenreCountDto {
                    name: "Drama".into(),
                    count: 2
                },
                GenreCountDto {
                    name: "Thriller".into(),
                    count: 1
                },
            ]
        );
    }
}
