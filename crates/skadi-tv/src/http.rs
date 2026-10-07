//! The television domain's HTTP surface (SKADI-T-0274).
//!
//! Implements [`HttpModule`](skadi_api::HttpModule) so the daemon merges the TV
//! routes into `/api/v1`. Covers series library management (`/series`), the
//! add-series metadata lookup, and per-series/season/episode monitor toggles.
//! The metadata provider here is the keyless Skyhook [`SeriesMetadataProvider`].
//! Interactive manual search/grab + library-import are follow-ons.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use skadi_api::bulk::{BulkAction, BulkOutcome, BulkReport, BulkRequest};
use skadi_api::{ApiError, HttpModule, LibraryEditionDto, LibraryItemDto, LibraryProvider};
use skadi_core::{
    AcquisitionStatus, AppError, EpisodeId, MediaKind, ProfileId, QualityId, RootFolder, SeasonId,
    SeriesId, TvdbId,
};
use skadi_metadata::{MetadataQuery, SeriesMetadataProvider};
use skadi_store::{BlocklistRepo, ConfigRepo, DomainStateRepo, SettingsRepo, Store};

use crate::metadata::add_series;
use crate::monitor::MonitorMode;
use crate::repo::{SeriesFilter, TvRepo};
use crate::series::Series;

/// The TV domain name in the `domains` table (must match `TelevisionModule::name`).
const DOMAIN_NAME: &str = "television";

/// Max metadata matches returned from `GET /series/lookup` (SKADI-T-0248): an add
/// picker only needs the closest few, and the unified add page stacks three domains.
const MAX_LOOKUP_RESULTS: usize = 10;

/// Shared state for the television HTTP handlers.
#[derive(Clone)]
pub struct TelevisionHttp {
    store: Store,
    provider: Arc<dyn SeriesMetadataProvider>,
    runner: Arc<cloacina::runner::DefaultRunner>,
}

impl TelevisionHttp {
    pub fn new(
        store: Store,
        provider: Arc<dyn SeriesMetadataProvider>,
        runner: Arc<cloacina::runner::DefaultRunner>,
    ) -> Self {
        Self {
            store,
            provider,
            runner,
        }
    }
}

impl HttpModule for TelevisionHttp {
    fn routes(&self) -> Router {
        Router::new()
            .route("/series", get(list_series).post(create_series))
            // Static `/series/lookup` before `/series/{id}`.
            .route("/series/lookup", get(lookup_series))
            // One request for a selection on the wall (SKADI-T-0696).
            .route("/series/bulk", post(bulk_series))
            .route(
                "/series/{id}",
                get(get_series).patch(patch_series).delete(delete_series),
            )
            // Per-item and bulk metadata refresh (SKADI-T-0450) — Sonarr's
            // "Refresh & Scan". Both drive the same `refresh_series_metadata`
            // workflow the scheduled worker uses, so an operator-triggered refresh
            // and a stale-sweep refresh cannot drift apart.
            .route("/series/{id}/refresh", post(refresh_series_route))
            .route("/series/refresh", post(refresh_all_series))
            .route("/series/{id}/seasons/{n}/monitor", post(monitor_season))
            .route("/series/{id}/episodes/{eid}/monitor", post(monitor_episode))
            // Manual acquire, at parity with movies and audiobooks
            // (SKADI-T-0558). `releases` is a GET because it is a read the
            // operator refreshes; the rest change state.
            .route(
                "/series/{id}/episodes/{eid}/releases",
                get(list_episode_releases),
            )
            .route("/series/{id}/episodes/{eid}/acquire", post(acquire_episode))
            .route(
                "/series/{id}/episodes/{eid}/grab",
                post(grab_episode_release),
            )
            .route(
                "/series/{id}/episodes/{eid}/grab-link",
                post(grab_episode_link),
            )
            .route("/series/{id}/episodes/{eid}/reset", post(reset_episode))
            // Video bytes for a player, with Range/seek (SKADI-T-0574).
            .route("/series/{id}/episodes/{eid}/video", get(episode_video))
            .route("/series/{id}/episodes/{eid}/subtitles", get(episode_subtitles))
            .route("/series/{id}/episodes/{eid}/markers", get(episode_markers))
            .route("/series/{id}/episodes/{eid}/subtitles/{n}", get(episode_subtitle))
            // Library import (SKADI-I-0047): namespaced under `/tv/library-import`
            // (movies owns the bare `/library-import`), all merged under `/api/v1`.
            .route("/tv/library-import/scan", post(library_import_scan))
            .route("/tv/library-import/match", post(library_import_match))
            .route(
                "/tv/library-import/series-structure",
                get(library_import_structure),
            )
            .route("/tv/library-import/commit", post(library_import_commit))
            .with_state(self.clone())
    }
}

// --- helpers ---

fn parse_id<T: From<Uuid>>(s: &str, what: &str) -> Result<T, ApiError> {
    Uuid::parse_str(s)
        .map(T::from)
        .map_err(|e| ApiError(AppError::Validation(format!("invalid {what} id: {e}"))))
}

fn repo(http: &TelevisionHttp) -> &dyn TvRepo {
    &http.store
}

/// The single `library.root` from the config plane (SKADI-T-0302), or the
/// registry default when unset. Series live under `<library.root>/television`.
async fn library_root(store: &Store) -> PathBuf {
    store
        .get_config("library.root")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            skadi_config::default_for("library.root")
                .unwrap_or("/data")
                .to_string()
        })
        .into()
}

// --- series ---

#[derive(Deserialize)]
struct ListParams {
    monitored: Option<bool>,
    /// Page size. Absent means every series **with all of its episodes** —
    /// 16.5 MB over a prod-sized library, the worst payload in the API
    /// (SKADI-T-0494).
    limit: Option<i64>,
    /// Rows to skip; ignored without a `limit`.
    offset: Option<i64>,
    /// `summary` trims each episode to what a library list actually reads
    /// (SKADI-T-0494). Anything else, or absent, returns the full shape.
    view: Option<String>,
}

async fn list_series(
    State(http): State<TelevisionHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    // A non-admin's page is cut from the policy-filtered list (SKADI-T-0617),
    // see `list_movies` for why.
    let paged_by_store = member.is_admin();
    let filter = SeriesFilter {
        monitored: params.monitored,
        limit: if paged_by_store { params.limit } else { None },
        offset: if paged_by_store { params.offset } else { None },
    };
    let mut total = repo(&http)
        .count_series(SeriesFilter {
            limit: None,
            offset: None,
            ..filter
        })
        .await?;
    let mut series = repo(&http).list_series(filter).await?;
    if !paged_by_store {
        // Household policy (SKADI-T-0612): what this member may not see is not here.
        series.retain(|s| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Series,
                &s.id.to_string(),
                s.content_rating.as_deref(),
                &s.genres,
            )
        });
        total = series.len() as i64;
        let offset = params.offset.unwrap_or(0).max(0) as usize;
        let limit = params.limit.map_or(usize::MAX, |l| l.max(0) as usize);
        series = series.into_iter().skip(offset).take(limit).collect();
    }
    if params.view.as_deref() == Some("summary") {
        let slim: Vec<SeriesSummaryDto> = series.iter().map(SeriesSummaryDto::from).collect();
        return Ok((
            [("x-total-count", total.to_string())],
            Json(serde_json::to_value(slim).unwrap_or_default()),
        ));
    }
    Ok((
        [("x-total-count", total.to_string())],
        Json(serde_json::to_value(series).unwrap_or_default()),
    ))
}

/// One episode, reduced to the fields a **library list** reads
/// (SKADI-T-0494).
///
/// `/series` was the API's worst payload — 17.5 MB over a prod-sized library,
/// because every episode carried its title, air date, scene numbers, file path,
/// media info and quality, and the TV page loads all of them to render a status
/// badge.
///
/// This is a **projection, not a computation**. An earlier pass deferred the
/// summary shape believing it meant deriving the wall status in SQL, which would
/// have created a second definition of "owned" free to drift from the client's.
/// It does not: `series_lib_status` reads only each episode's `season` and
/// `status`, so sending fewer *fields* keeps that rule byte-for-byte identical
/// and simply stops shipping what it never looks at.
#[derive(serde::Serialize)]
struct EpisodeSummaryDto {
    /// Kept because clients type an episode as having one — dropping it would
    /// make the projection a *different* type rather than a smaller one, and an
    /// existing client would fail to deserialise instead of simply reading less.
    id: String,
    season: u16,
    number: u16,
    monitored: bool,
    /// The status **variant name only** — "Imported", "Missing", "Failed".
    ///
    /// The full enum is a nested object carrying attempts, retry times, file
    /// refs and quality: ~110 bytes per episode, ~2.6 MB across a prod-sized
    /// library, none of it read by a library list. `series_lib_status` calls
    /// `status_label`, which takes the variant name and nothing else — and that
    /// helper already accepts a bare string, so this is the shape it wants.
    ///
    /// The detail page still gets the full enum from `GET /series/{id}`.
    status: String,
}

/// A series with slim episodes. Seasons are kept whole: there are tens of them,
/// not thousands, and the page reads their `monitored` flag.
#[derive(serde::Serialize)]
struct SeriesSummaryDto {
    #[serde(flatten)]
    series: serde_json::Value,
    episodes: Vec<EpisodeSummaryDto>,
}

impl From<&Series> for SeriesSummaryDto {
    fn from(s: &Series) -> Self {
        // Serialise the series, then replace its episodes wholesale. Flattening
        // the original keeps every series-level field the page uses (title,
        // poster, year, monitored, …) without restating them here, where they
        // would drift as `Series` gains fields.
        let mut v = serde_json::to_value(s).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.remove("episodes");
        }
        Self {
            series: v,
            episodes: s
                .episodes
                .iter()
                .map(|e| EpisodeSummaryDto {
                    id: e.id.to_string(),
                    season: e.season,
                    number: e.number,
                    monitored: e.monitored,
                    // Serialise the enum, then keep only its discriminant, so
                    // this cannot drift from the real variant names the way a
                    // hand-written match would.
                    status: match serde_json::to_value(&e.status) {
                        Ok(serde_json::Value::String(v)) => v,
                        Ok(serde_json::Value::Object(m)) => {
                            m.keys().next().cloned().unwrap_or_else(|| "Unknown".into())
                        }
                        _ => "Unknown".into(),
                    },
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct LookupParams {
    /// Search term (a series title).
    q: String,
}

#[derive(Serialize)]
struct LookupResult {
    tvdb_id: u64,
    title: String,
    year: Option<u16>,
    /// Poster URL + short synopsis (when the provider's search carries them).
    #[serde(skip_serializing_if = "Option::is_none")]
    poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overview: Option<String>,
}

/// `GET /series/lookup?q=` — free-text series search via Skyhook. Results without
/// a TVDB id are dropped (the add path keys on it).
async fn lookup_series(
    State(http): State<TelevisionHttp>,
    Query(params): Query<LookupParams>,
) -> Result<impl IntoResponse, ApiError> {
    let query = MetadataQuery {
        title: params.q,
        year: None,
        kind: MediaKind::Series,
    };
    let matches = http
        .provider
        .search_series(&query)
        .await
        .map_err(ApiError)?;
    let results: Vec<LookupResult> = matches
        .into_iter()
        .filter_map(|m| {
            m.external_ids.tvdb.map(|t| LookupResult {
                tvdb_id: t.0,
                title: m.title,
                year: m.year,
                poster_url: m.poster_url,
                overview: m.overview,
            })
        })
        // Cap to the closest matches (SKADI-T-0248): the provider returns them in
        // relevance order, and the unified add page can't show every hit per domain.
        .take(MAX_LOOKUP_RESULTS)
        .collect();
    Ok(Json(results))
}

#[derive(Deserialize)]
struct AddSeriesRequest {
    tvdb_id: u64,
    profile: Option<String>,
    /// Monitor mode (default `all`). One of all/future/missing/existing/
    /// firstSeason/lastSeason/pilot/none.
    #[serde(default)]
    monitor: Option<String>,
}

async fn create_series(
    State(http): State<TelevisionHttp>,
    Json(req): Json<AddSeriesRequest>,
) -> Result<Response, ApiError> {
    let profiles = http.store.list_settings("profiles").await?;
    let profile: ProfileId = match &req.profile {
        Some(p) => {
            let id: ProfileId = parse_id(p, "profile")?;
            let known = profiles
                .iter()
                .any(|r| Uuid::parse_str(&r.id).ok() == Some(id.into_uuid()));
            if !known {
                return Err(ApiError(AppError::Validation(format!(
                    "profile {p} is not a registered quality profile"
                ))));
            }
            id
        }
        None => profiles
            .first()
            .and_then(|r| Uuid::parse_str(&r.id).ok())
            .map(ProfileId::from)
            .ok_or_else(|| {
                ApiError(AppError::Validation(
                    "no quality profile registered — create one under settings/profiles first"
                        .into(),
                ))
            })?,
    };

    // Derived root (SKADI-T-0302): series live under `<library.root>/television`.
    let root_folder = RootFolder::for_domain(library_root(&http.store).await, MediaKind::Series);

    let monitor = req
        .monitor
        .as_deref()
        .map_or(MonitorMode::All, MonitorMode::from_str_lossy);

    let series = add_series(
        repo(&http),
        http.provider.as_ref(),
        TvdbId(req.tvdb_id),
        profile,
        root_folder,
        monitor,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(series)).into_response())
}

/// Refresh one series' metadata now (SKADI-T-0450).
///
/// Runs the same `refresh_series_metadata` workflow as the scheduled worker
/// rather than calling `refresh_series` directly: the workflow carries the
/// circuit breaker and the persistence, and since SKADI-T-0447 the refresh also
/// *prunes* episodes — a second path into that is a second place to get the
/// file-retention rule wrong.
async fn refresh_series_route(
    State(http): State<TelevisionHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use cloacina::executor::WorkflowExecutor;
    let id: SeriesId = parse_id(&id, "series")?;
    // 404 before doing any work, so a typo'd id is not reported as a queued refresh.
    repo(&http)
        .get_series(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {id} not found"))))?;
    let ctx = crate::refresh::refresh_context(&id.to_string())?;
    http.runner
        .execute("refresh_series_metadata", ctx)
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("refresh run failed: {e}"))))?;
    Ok(Json(serde_json::json!({ "refreshed": id.to_string() })))
}

/// Refresh every series (Sonarr's library-wide refresh, SKADI-T-0450).
///
/// Serial on purpose: each series is a provider lookup plus one request per
/// season (SKADI-T-0514), so a whole library at once is exactly the burst that
/// gets an API key rate-limited. Reports what it managed, so a partial failure is
/// visible instead of silently leaving half the library stale.
async fn refresh_all_series(
    State(http): State<TelevisionHttp>,
) -> Result<impl IntoResponse, ApiError> {
    use cloacina::executor::WorkflowExecutor;
    let list = repo(&http)
        .list_series(SeriesFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await?;
    let requested = list.len();
    let mut refreshed = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for series in list {
        let Ok(ctx) = crate::refresh::refresh_context(&series.id.to_string()) else {
            failed.push(series.id.to_string());
            continue;
        };
        match http.runner.execute("refresh_series_metadata", ctx).await {
            Ok(_) => refreshed += 1,
            Err(e) => {
                tracing::warn!(series = %series.id, error = %e, "bulk refresh: run failed");
                failed.push(series.id.to_string());
            }
        }
    }
    Ok(Json(serde_json::json!({
        "requested": requested,
        "refreshed": refreshed,
        "failed": failed,
    })))
}

async fn get_series(
    State(http): State<TelevisionHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    let id: SeriesId = parse_id(&id, "series")?;
    let series = repo(&http)
        .get_series(id)
        .await?
        .filter(|s| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Series,
                &s.id.to_string(),
                s.content_rating.as_deref(),
                &s.genres,
            )
        })
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {id} not found"))))?;
    Ok(Json(series))
}

#[derive(Deserialize, Default)]
struct PatchSeries {
    monitored: Option<bool>,
    /// Quality profile id. Mirrors `PatchMovie` (SKADI-T-0607): an old show
    /// that only exists in SD needs a profile that allows it, per series.
    profile: Option<String>,
    /// The item's tags, as settings-record ids (SKADI-T-0560).
    ///
    /// Absent means "leave them alone"; `[]` means "remove them all". A bare
    /// `Vec` would make every PATCH that omits tags silently clear them.
    tags: Option<Vec<String>>,
}

async fn patch_series(
    State(http): State<TelevisionHttp>,
    Path(id): Path<String>,
    Json(req): Json<PatchSeries>,
) -> Result<impl IntoResponse, ApiError> {
    let id: SeriesId = parse_id(&id, "series")?;
    Ok(Json(apply_patch(&http, id, req).await?))
}

/// The one PATCH path, shared by `PATCH /series/{id}` and the bulk
/// monitor/unmonitor (SKADI-T-0696). 404 when the series is not there.
async fn apply_patch(
    http: &TelevisionHttp,
    id: SeriesId,
    req: PatchSeries,
) -> Result<Series, ApiError> {
    let mut series = repo(http)
        .get_series(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {id} not found"))))?;
    let mut changed = false;
    if let Some(m) = req.monitored {
        series.monitored = m;
        changed = true;
    }
    if let Some(p) = req.profile {
        let id: ProfileId = parse_id(&p, "profile")?;
        let profiles = http.store.list_settings("profiles").await?;
        if !profiles
            .iter()
            .any(|r| Uuid::parse_str(&r.id).ok() == Some(id.into_uuid()))
        {
            return Err(ApiError(AppError::Validation(format!(
                "profile {p} is not a registered quality profile"
            ))));
        }
        series.profile = id;
        changed = true;
    }
    if changed {
        repo(http).upsert_series(&series).await?;
    }
    if let Some(tags) = req.tags {
        use skadi_store::ItemTagRepo;
        http.store
            .set_tags("series", &series.id.0.to_string(), &tags)
            .await?;
    }
    Ok(series)
}

/// Delete options (SKADI-T-0316): `?delete_files=true` also removes every imported episode
/// file + prunes the now-empty folders. The web UI sends it behind a confirmation.
#[derive(serde::Deserialize)]
struct DeleteOpts {
    #[serde(default)]
    delete_files: bool,
}

async fn delete_series(
    State(http): State<TelevisionHttp>,
    Path(id): Path<String>,
    axum::extract::Query(opts): axum::extract::Query<DeleteOpts>,
) -> Result<impl IntoResponse, ApiError> {
    let id: SeriesId = parse_id(&id, "series")?;
    if !delete_one(&http, id, opts.delete_files).await? {
        return Err(ApiError(AppError::NotFound(format!(
            "series {id} not found"
        ))));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The one delete path, shared by `DELETE /series/{id}` and the bulk delete
/// (SKADI-T-0696). With `delete_files`, removes the files the episodes record
/// as imported (those exact paths, nothing else) and prunes the emptied
/// folders up to the series' root, never past it. Returns `false` when the
/// series is not there.
async fn delete_one(
    http: &TelevisionHttp,
    id: SeriesId,
    delete_files: bool,
) -> Result<bool, ApiError> {
    let Some(series) = repo(http).get_series(id).await? else {
        return Ok(false);
    };
    // Remove every imported episode file + prune empty folders first (SKADI-T-0316).
    if delete_files {
        let paths: Vec<std::path::PathBuf> = series
            .episodes
            .iter()
            .filter_map(|e| match &e.status {
                skadi_core::AcquisitionStatus::Imported { file, .. } => Some(file.path.clone()),
                _ => None,
            })
            .collect();
        let root = std::path::PathBuf::from(&series.root_folder.path);
        let removed = tokio::task::spawn_blocking(move || {
            skadi_importer::delete_files_and_prune(&paths, &root)
        })
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("delete task panicked: {e}"))))?;
        tracing::info!(series = %id, removed = removed.len(), "deleted episode files on library delete");
    }
    // Take the item's tag membership with it (SKADI-T-0560). Otherwise the rows
    // outlive the item and a later one reusing the id would inherit them — the
    // silent orphaning the vision calls out as something skadi does not do.
    {
        use skadi_store::ItemTagRepo;
        let _ = http.store.clear_item("series", &id.0.to_string()).await;
    }
    repo(http).delete_series(id).await?;
    Ok(true)
}

/// `POST /series/bulk` (SKADI-T-0696): apply one action to many series in one
/// request. Each id goes through the single-item path (`apply_patch`,
/// `start_episode_acquire`, `delete_one`), so a bulk action cannot do what the
/// single route would not.
async fn bulk_series(
    State(http): State<TelevisionHttp>,
    skadi_api::error::ApiJson(req): skadi_api::error::ApiJson<BulkRequest>,
) -> Result<Response, ApiError> {
    let ids: Vec<(String, SeriesId)> = req.parsed_ids()?;
    if req.action == BulkAction::Search && !domain_enabled(&http).await? {
        return Ok(domain_disabled_response());
    }
    let mut report = BulkReport::new(req.action, ids.len());
    for (raw, id) in ids {
        let outcome = match req.action {
            BulkAction::Monitor | BulkAction::Unmonitor => {
                let patch = PatchSeries {
                    monitored: Some(req.action == BulkAction::Monitor),
                    ..PatchSeries::default()
                };
                apply_patch(&http, id, patch)
                    .await
                    .map(|_| BulkOutcome::Done)
            }
            BulkAction::Search => search_series(&http, id).await,
            BulkAction::Delete => delete_one(&http, id, req.delete_files).await.map(|found| {
                if found {
                    BulkOutcome::Done
                } else {
                    BulkOutcome::NotFound
                }
            }),
        };
        report.record(raw, outcome);
    }
    Ok(Json(report).into_response())
}

/// Search one series now: start the manual acquire for each monitored episode
/// that has aired, is not imported and is not already in flight. Unaired and
/// unmonitored episodes are left alone: a search for them finds nothing, or
/// fetches what the operator chose not to want.
async fn search_series(http: &TelevisionHttp, id: SeriesId) -> Result<BulkOutcome, ApiError> {
    let Some(series) = repo(http).get_series(id).await? else {
        return Ok(BulkOutcome::NotFound);
    };
    let today = chrono::Utc::now().date_naive();
    let mut started = 0;
    for episode in &series.episodes {
        if !episode.monitored
            || !episode.has_aired_or_undated(today, false)
            || matches!(episode.status, AcquisitionStatus::Imported { .. })
        {
            continue;
        }
        if start_episode_acquire(http, &series, episode) {
            started += 1;
        }
    }
    Ok(BulkOutcome::Searched(started))
}

#[derive(Deserialize)]
struct MonitorReq {
    monitored: bool,
}

/// `POST /series/{id}/seasons/{n}/monitor` — toggle a whole season's monitor flag
/// (and every one of its episodes).
async fn monitor_season(
    State(http): State<TelevisionHttp>,
    Path((id, n)): Path<(String, i32)>,
    Json(req): Json<MonitorReq>,
) -> Result<impl IntoResponse, ApiError> {
    let id: SeriesId = parse_id(&id, "series")?;
    let series = repo(&http)
        .get_series(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {id} not found"))))?;
    let number = u16::try_from(n).unwrap_or(0);
    let Some(season) = series.seasons.iter().find(|s| s.number == number) else {
        return Err(ApiError(AppError::NotFound(format!(
            "season {number} not found"
        ))));
    };
    set_season_monitored_cascade(&http, season.id, number, &series, req.monitored).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_season_monitored_cascade(
    http: &TelevisionHttp,
    season_id: SeasonId,
    number: u16,
    series: &Series,
    monitored: bool,
) -> Result<(), ApiError> {
    repo(http)
        .set_season_monitored(season_id, monitored)
        .await?;
    for ep in series.episodes.iter().filter(|e| e.season == number) {
        repo(http).set_episode_monitored(ep.id, monitored).await?;
    }
    Ok(())
}

/// `POST /series/{id}/episodes/{eid}/monitor` — toggle one episode's monitor flag.
async fn monitor_episode(
    State(http): State<TelevisionHttp>,
    Path((_id, eid)): Path<(String, String)>,
    Json(req): Json<MonitorReq>,
) -> Result<impl IntoResponse, ApiError> {
    let eid: EpisodeId = parse_id(&eid, "episode")?;
    if repo(&http).get_episode(eid).await?.is_none() {
        return Err(ApiError(AppError::NotFound(format!(
            "episode {eid} not found"
        ))));
    }
    repo(&http)
        .set_episode_monitored(eid, req.monitored)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- library import (SKADI-I-0047) ---

#[derive(Deserialize)]
struct ScanRequest {
    /// Root folder to scan (as the daemon sees it, e.g. `/media/tv`).
    path: String,
    /// Include files already in the library (SKADI-T-0541). `false` by default:
    /// they clutter the review list and invite an accidental re-import.
    #[serde(default)]
    include_imported: bool,
}

/// One scanned candidate, **parse-only** (no metadata lookup). Keyed by `path`
/// so the UI can merge match results back into the row it scanned. Mirrors the
/// movies scan DTO, but per-episode (each video file is one candidate).
#[derive(Serialize)]
struct ScanCandidateDto {
    path: String,
    display_name: String,
    series_title: Option<String>,
    season: Option<u16>,
    episodes: Vec<u16>,
    absolute: Vec<u16>,
    air_date: Option<String>,
    quality_id: Option<String>,
    quality_name: Option<String>,
    /// Show folder under the scan root (the import UI groups by this).
    folder: Option<String>,
    /// Exact TVDB id + real title/year from the folder's `tvshow.nfo`, when present.
    nfo_tvdb_id: Option<u64>,
    nfo_title: Option<String>,
    nfo_year: Option<u16>,
    /// Display metadata from `tvshow.nfo` to confirm a match at a glance.
    nfo_overview: Option<String>,
    nfo_genres: Vec<String>,
    nfo_status: Option<String>,
    nfo_network: Option<String>,
    nfo_rating: Option<String>,
}

/// `POST /tv/library-import/scan` — walk a root folder and return parsed episode
/// candidates **without any metadata lookups**. Read-only + fast (filesystem +
/// parse only); series matching is the separate, lazy [`library_import_match`].
async fn library_import_scan(
    State(http): State<TelevisionHttp>,
    Json(req): Json<ScanRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let started = std::time::Instant::now();
    // The walk is blocking + can be slow on a high-latency mount; keep it off the
    // async worker (parity with movies).
    let scan_path = req.path.clone();
    let candidates = tokio::task::spawn_blocking(move || {
        crate::import::scan_tv_candidates(std::path::Path::new(&scan_path))
    })
    .await
    .map_err(|e| ApiError(AppError::Internal(format!("scan task panicked: {e}"))))?
    .map_err(ApiError)?;
    // Drop files already in the library (SKADI-T-0541, matching movies'
    // SKADI-T-0320). Matched by **inode**, not path: import places library files
    // as hardlinks to their source (SKADI-T-0424), so the held copy and the one
    // still in the scan directory are the same inode under two names — a path
    // comparison would miss every one of them.
    let total_scanned = candidates.len();
    let candidates = if req.include_imported {
        candidates
    } else {
        let held: Vec<std::path::PathBuf> = repo(&http)
            .list_series(SeriesFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?
            .iter()
            .flat_map(|s| s.episodes.iter())
            .filter_map(|e| e.file.as_ref().map(|f| f.path.clone()))
            .collect();
        let paths: Vec<std::path::PathBuf> = candidates.iter().map(|c| c.path.clone()).collect();
        let keep: Vec<bool> = tokio::task::spawn_blocking(move || {
            let ids = skadi_importer::file_identities(&held);
            paths
                .iter()
                .map(|p| skadi_importer::file_identity(p).is_none_or(|id| !ids.contains(&id)))
                .collect()
        })
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("scan filter panicked: {e}"))))?;
        candidates
            .into_iter()
            .zip(keep)
            .filter_map(|(c, k)| k.then_some(c))
            .collect()
    };
    let hidden = total_scanned - candidates.len();
    let dtos: Vec<ScanCandidateDto> = candidates
        .into_iter()
        .map(|c| ScanCandidateDto {
            path: c.path.to_string_lossy().into_owned(),
            display_name: c.display_name,
            series_title: c.series_title,
            season: c.season,
            episodes: c.episodes,
            absolute: c.absolute,
            air_date: c.air_date,
            quality_id: c.quality_id.map(|q| q.to_string()),
            quality_name: c.quality_name,
            folder: c.folder,
            nfo_tvdb_id: c.nfo_tvdb_id,
            nfo_title: c.nfo_title,
            nfo_year: c.nfo_year,
            nfo_overview: c.nfo_overview,
            nfo_genres: c.nfo_genres,
            nfo_status: c.nfo_status,
            nfo_network: c.nfo_network,
            nfo_rating: c.nfo_rating,
        })
        .collect();
    tracing::info!(
        path = %req.path,
        total = dtos.len(),
        // Logged, not silent (SKADI-T-0541): an operator who scanned 400 files
        // and sees 12 needs to know the rest were filtered, not missed.
        already_imported = hidden,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "tv library-import scan complete (parse-only)"
    );
    Ok(Json(dtos))
}

/// How many series lookups [`library_import_match`] runs concurrently. Bounded so
/// a page doesn't open a burst the provider would rate-limit.
const MATCH_CONCURRENCY: usize = 6;

/// One candidate to match: its stable `path` key plus the parsed series title/year.
#[derive(Deserialize)]
struct MatchItem {
    path: String,
    #[serde(default)]
    series_title: Option<String>,
    #[serde(default)]
    year: Option<u16>,
}

#[derive(Deserialize)]
struct MatchRequest {
    items: Vec<MatchItem>,
}

/// The series proposed for a candidate (the closest provider hit carrying a TVDB id).
#[derive(Clone, Serialize)]
struct ProposedSeries {
    tvdb_id: u64,
    title: String,
    year: Option<u16>,
    poster_url: Option<String>,
}

/// The metadata match resolved for one candidate, keyed by `path`.
#[derive(Serialize)]
struct MatchDto {
    path: String,
    proposed: Option<ProposedSeries>,
    /// `high` (parsed year agrees, or nothing to compare), `low` (mismatch), or `none`.
    confidence: &'static str,
    needs_review: bool,
    already_in_library: bool,
}

/// Resolve a series title to the closest provider hit carrying a TVDB id
/// (best-effort — a provider error just yields no match).
async fn resolve_series(
    provider: &dyn SeriesMetadataProvider,
    title: &str,
    year: Option<u16>,
) -> Option<ProposedSeries> {
    let q = MetadataQuery {
        title: title.to_string(),
        year,
        kind: MediaKind::Series,
    };
    provider.search_series(&q).await.ok().and_then(|matches| {
        // Best score first, then PREFER a result whose year agrees with the
        // parse — taking the provider's first hit is how sequels/reboots land
        // on the wrong series (movies parity, SKADI-T-0328 feedback).
        let mut with_id: Vec<_> = matches
            .into_iter()
            .filter(|m| m.external_ids.tvdb.is_some())
            .collect();
        with_id.sort_by(|a, b| b.score.total_cmp(&a.score));
        let pos = year
            .and_then(|y| with_id.iter().position(|m| m.year == Some(y)))
            .unwrap_or(0);
        (!with_id.is_empty()).then(|| {
            let m = with_id.swap_remove(pos);
            ProposedSeries {
                tvdb_id: m.external_ids.tvdb.map(|t| t.0).unwrap_or_default(),
                title: m.title,
                year: m.year,
                poster_url: m.poster_url,
            }
        })
    })
}

/// Case/punctuation-insensitive title equality for confidence grading: lowercase,
/// alphanumerics only. `"doctor-who"` == `"Doctor Who"`, but != `"Doctor Who (2005)"`.
fn titles_equal(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect::<String>()
    };
    let (na, nb) = (norm(a), norm(b));
    !na.is_empty() && na == nb
}

/// `POST /tv/library-import/match` — resolve series matches for a batch of scanned
/// candidates (one page's worth). Many episode files share one series, so lookups
/// are **cached per `(series_title, year)`** within the batch — one provider call
/// per distinct pair, run under bounded concurrency ([`MATCH_CONCURRENCY`]).
/// Result order matches the request via the `path` key.
async fn library_import_match(
    State(http): State<TelevisionHttp>,
    Json(req): Json<MatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let started = std::time::Instant::now();
    let n = req.items.len();

    // Distinct (title, year) → one lookup each. Keying by title alone let the
    // first-seen year win, proposing the wrong series for same-titled shows with
    // different years in one batch (SKADI-T-0325).
    let mut distinct: std::collections::HashSet<(String, Option<u16>)> =
        std::collections::HashSet::new();
    for item in &req.items {
        if let Some(t) = &item.series_title {
            distinct.insert((t.clone(), item.year));
        }
    }

    let sem = Arc::new(tokio::sync::Semaphore::new(MATCH_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for (title, year) in distinct {
        let provider = http.provider.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("match semaphore not closed");
            let proposed = resolve_series(provider.as_ref(), &title, year).await;
            ((title, year), proposed)
        });
    }
    let mut cache: std::collections::HashMap<(String, Option<u16>), Option<ProposedSeries>> =
        std::collections::HashMap::new();
    while let Some(res) = set.join_next().await {
        if let Ok((key, proposed)) = res {
            cache.insert(key, proposed);
        }
    }

    // Build result rows in request order, running the already-in-library check.
    let mut dtos: Vec<MatchDto> = Vec::with_capacity(n);
    for item in req.items {
        let proposed = item
            .series_title
            .as_ref()
            .and_then(|t| cache.get(&(t.clone(), item.year)).cloned())
            .flatten();
        let already_in_library = match &proposed {
            Some(p) => repo(&http)
                .get_series_by_tvdb(TvdbId(p.tvdb_id))
                .await
                .ok()
                .flatten()
                .is_some(),
            None => false,
        };
        let (confidence, needs_review) = match &proposed {
            None => ("none", true),
            // With no parsed year there's nothing to cross-check, and the search
            // just takes the first ranked hit — only call it "high" when the
            // proposed title actually equals the query title (normalized);
            // otherwise a bare "Doctor Who" folder confidently pre-ticks whichever
            // entry the provider ranks first (SKADI-T-0325).
            Some(p) => match (item.year, p.year) {
                (None, _) => {
                    let same_title = item
                        .series_title
                        .as_deref()
                        .is_some_and(|q| titles_equal(q, &p.title));
                    if same_title {
                        ("high", false)
                    } else {
                        ("low", true)
                    }
                }
                (Some(a), Some(b)) if a == b => ("high", false),
                _ => ("low", true),
            },
        };
        dtos.push(MatchDto {
            path: item.path,
            proposed,
            confidence,
            needs_review,
            already_in_library,
        });
    }

    tracing::info!(
        items = n,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "tv library-import match batch complete"
    );
    Ok(Json(dtos))
}

#[derive(Deserialize)]
struct CommitItem {
    /// Existing file path from the scan (the import *source*; never modified on failure).
    path: String,
    tvdb_id: u64,
    /// Detected quality id from the scan; falls back to the lowest when absent.
    #[serde(default)]
    quality_id: Option<String>,
    /// Manual mapping (SKADI-T-0324): when both set, place the file at this exact
    /// (season, episode) instead of re-parsing the filename.
    #[serde(default)]
    season: Option<u16>,
    #[serde(default)]
    episode: Option<u16>,
}

#[derive(Deserialize)]
struct CommitRequest {
    /// Quality profile to assign added-on-import series. Optional: an omitted
    /// profile binds the first registered one (mirrors `create_series`).
    #[serde(default)]
    profile: Option<String>,
    items: Vec<CommitItem>,
}

#[derive(Serialize)]
struct CommitResult {
    imported: usize,
    skipped: usize,
    /// Of `imported`: hardlinked to their canonical path (source dropped — adoption move).
    linked: usize,
    /// Of `imported`: already at their canonical path — nothing to move.
    in_place: usize,
    /// Files left in place (no matching TVDB episode, or already imported) — benign.
    unmatched: Vec<String>,
    errors: Vec<String>,
}

#[derive(Deserialize)]
struct StructureParams {
    tvdb: u64,
}

#[derive(Serialize)]
struct SeasonStructDto {
    number: u16,
    episode_count: u16,
}

#[derive(Serialize)]
struct EpisodeStructDto {
    season: u16,
    number: u16,
    title: Option<String>,
}

#[derive(Serialize)]
struct StructureDto {
    seasons: Vec<SeasonStructDto>,
    episodes: Vec<EpisodeStructDto>,
}

/// `GET /tv/library-import/series-structure?tvdb=` — a show's season/episode tree
/// straight from the metadata provider (no DB write), so the import UI can render
/// the full Season → Episode tree and populate manual pickers (SKADI-T-0324).
async fn library_import_structure(
    State(http): State<TelevisionHttp>,
    Query(params): Query<StructureParams>,
) -> Result<impl IntoResponse, ApiError> {
    let meta = http
        .provider
        .lookup_series(TvdbId(params.tvdb))
        .await
        .map_err(ApiError)?;
    let seasons = meta
        .seasons
        .iter()
        .map(|s| SeasonStructDto {
            number: s.number,
            episode_count: s.episode_count,
        })
        .collect();
    let episodes = meta
        .episodes
        .iter()
        .map(|e| EpisodeStructDto {
            season: e.season,
            number: e.number,
            title: e.title.clone(),
        })
        .collect();
    Ok(Json(StructureDto { seasons, episodes }))
}

/// `POST /tv/library-import/commit` — restructure the confirmed episode files into
/// the canonical layout under the TV root folder (hardlink; in-place no-op when
/// already canonical) and mark each matching `Episode` `Imported`. Series not yet
/// in the library are added-on-import. Idempotent; non-destructive (a source is
/// only dropped after a fresh link, never on a failure path).
async fn library_import_commit(
    State(http): State<TelevisionHttp>,
    Json(req): Json<CommitRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Resolve the upgrade profile exactly like `create_series`: an explicit id must
    // be registered; omitted binds the first registered profile.
    let profiles = http.store.list_settings("profiles").await?;
    let profile: ProfileId = match &req.profile {
        Some(p) if !p.trim().is_empty() => {
            let id: ProfileId = parse_id(p, "profile")?;
            if !profiles
                .iter()
                .any(|r| Uuid::parse_str(&r.id).ok() == Some(id.into_uuid()))
            {
                return Err(ApiError(AppError::Validation(format!(
                    "profile {p} is not a registered quality profile"
                ))));
            }
            id
        }
        _ => profiles
            .first()
            .and_then(|r| Uuid::parse_str(&r.id).ok())
            .map(ProfileId::from)
            .ok_or_else(|| {
                ApiError(AppError::Validation(
                    "no quality profile registered — create one under settings/profiles first"
                        .into(),
                ))
            })?,
    };

    // Series adopt into `<library.root>/television` (SKADI-T-0302).
    let root = RootFolder::for_domain(library_root(&http.store).await, MediaKind::Series);

    // Operator-configured naming (else the built-in default), like the importer factory.
    let naming = match http.store.list_config().await {
        Ok(entries) => {
            let view =
                skadi_config::ConfigView::from_pairs(entries.into_iter().map(|e| (e.key, e.value)));
            crate::naming::SeriesNaming::from_view(&view)
        }
        Err(_) => crate::naming::SeriesNaming::default(),
    };

    // Validate client quality ids against the known definitions — a garbage or
    // unknown UUID would otherwise be recorded verbatim into episode statuses
    // (SKADI-T-0327). Absent/invalid → rejected; the commit re-derives quality
    // from the filename when the client sends none.
    let known: std::collections::HashSet<skadi_core::QualityId> =
        skadi_quality::default_definitions()
            .iter()
            .map(|d| d.id)
            .collect();
    let mut targets: Vec<crate::import::CommitTarget> = Vec::with_capacity(req.items.len());
    for it in req.items {
        let quality_id = match it.quality_id.as_deref() {
            None => None,
            Some(s) => {
                let id = Uuid::parse_str(s).ok().map(QualityId::from);
                match id {
                    Some(q) if known.contains(&q) => Some(q),
                    _ => {
                        return Err(ApiError::from(AppError::Validation(format!(
                            "unknown quality id {s:?} for {}",
                            it.path
                        ))));
                    }
                }
            }
        };
        targets.push(crate::import::CommitTarget {
            path: PathBuf::from(it.path),
            tvdb_id: it.tvdb_id,
            quality_id,
            season: it.season,
            episode: it.episode,
        });
    }

    let n_items = targets.len();
    tracing::info!(items = n_items, "tv library-import commit: starting");

    let out = crate::import::commit_items(
        repo(&http),
        http.provider.as_ref(),
        targets,
        profile,
        root,
        naming,
    )
    .await;

    tracing::info!(
        items = n_items,
        imported = out.imported,
        skipped = out.skipped,
        linked = out.linked,
        in_place = out.in_place,
        unmatched = out.unmatched.len(),
        errors = out.errors.len(),
        "tv library-import commit: done"
    );
    Ok(Json(CommitResult {
        imported: out.imported,
        skipped: out.skipped,
        linked: out.linked,
        in_place: out.in_place,
        unmatched: out.unmatched,
        errors: out.errors,
    }))
}

// --- library provider ---

/// The television domain's contribution to the unified `/library` view.
#[derive(Clone)]
pub struct TelevisionLibrary {
    store: Store,
    /// Needed only by `add_from_list` (SKADI-T-0564), so it is optional: every
    /// existing construction site builds this for the read-side methods and has
    /// no provider to hand. Absent means the add hook reports that it is not
    /// configured, rather than the domain silently not being addable.
    provider: Option<Arc<dyn SeriesMetadataProvider>>,
}

impl TelevisionLibrary {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            provider: None,
        }
    }

    /// Attach the metadata provider that `add_from_list` needs (SKADI-T-0564).
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn SeriesMetadataProvider>) -> Self {
        self.provider = Some(provider);
        self
    }
}

fn status_kind_str(status: &AcquisitionStatus) -> &'static str {
    match status {
        AcquisitionStatus::Missing => "missing",
        AcquisitionStatus::Searching { .. } => "searching",
        AcquisitionStatus::Snatched { .. } => "snatched",
        AcquisitionStatus::Downloading { .. } => "downloading",
        AcquisitionStatus::Imported { .. } => "imported",
        AcquisitionStatus::Cutoff => "cutoff",
        AcquisitionStatus::Failed { .. } => "failed",
    }
}

fn series_to_dto(series: Series) -> LibraryItemDto {
    // Each episode is an acquirable; surface them as the item's "editions" so the
    // unified library wall can show per-episode status.
    let editions = series
        .episodes
        .iter()
        .map(|e| LibraryEditionDto {
            id: e.id.to_string(),
            kind: format!("S{:02}E{:02}", e.season, e.number),
            monitored: e.monitored,
            kind_name: None,
            status_kind: status_kind_str(&e.status).to_string(),
            quality: e.quality.map(|q| q.to_string()),
            quality_name: e
                .quality
                .and_then(|q| skadi_api::quality_display_name(&q.to_string())),
            // Episodes persist probed media info since SKADI-T-0451; this was
            // hard-coded `None` because there was nothing to read.
            media_info: e.media_info.clone(),
        })
        .collect();
    LibraryItemDto {
        kind: "series".into(),
        id: series.id.to_string(),
        title: series.title,
        year: series.year,
        monitored: series.monitored,
        editions,
    }
}

#[async_trait]
impl LibraryProvider for TelevisionLibrary {
    fn domain(&self) -> &str {
        DOMAIN_NAME
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Series
    }

    /// Add a series by TVDB id on behalf of an import list (SKADI-T-0564).
    ///
    /// Goes through `add_series` — the same function `POST /series` calls — so a
    /// list-added series and a hand-added one are the same rows built the same
    /// way.
    ///
    /// **`MonitorMode::None` unless the list opted in.** Movies achieve this by
    /// adding then flipping `monitored`; TV has a first-class way to say it, and
    /// using it matters more here: a monitored series marks *every* episode
    /// monitored, so a list of thirty shows would put thousands of episodes into
    /// the wanted sweep on its first run.
    async fn add_from_list(
        &self,
        id_kind: &str,
        external_id: &str,
        opts: &skadi_api::library::ImportListAddOptions,
    ) -> skadi_core::Result<bool> {
        if !id_kind.eq_ignore_ascii_case("tvdb") {
            return Err(AppError::Validation(format!(
                "series can be added by tvdb id, not `{id_kind}`"
            )));
        }
        let provider = self.provider.as_ref().ok_or_else(|| {
            AppError::Internal("television library has no metadata provider configured".into())
        })?;
        let tvdb: u64 = external_id
            .parse()
            .map_err(|_| AppError::Validation(format!("`{external_id}` is not a TVDB id")))?;
        let tvdb = TvdbId(tvdb);

        // Already present is the ordinary case on every sync after the first.
        if (&self.store as &dyn TvRepo)
            .get_series_by_tvdb(tvdb.clone())
            .await?
            .is_some()
        {
            return Ok(false);
        }

        let profile = match &opts.profile_id {
            Some(p) => Uuid::parse_str(p)
                .map(ProfileId::from)
                .map_err(|_| AppError::Validation(format!("`{p}` is not a profile id")))?,
            None => (&self.store as &dyn SettingsRepo)
                .list_settings("profiles")
                .await?
                .first()
                .and_then(|r| Uuid::parse_str(&r.id).ok())
                .map(ProfileId::from)
                .ok_or_else(|| {
                    AppError::Validation(
                        "no quality profile registered — create one under settings/profiles first"
                            .into(),
                    )
                })?,
        };
        // Derived, not chosen (SKADI-T-0302), as the HTTP add does.
        let root_folder =
            RootFolder::for_domain(library_root(&self.store).await, MediaKind::Series);
        let monitor = if opts.monitored {
            MonitorMode::All
        } else {
            MonitorMode::None
        };

        add_series(
            &self.store,
            provider.as_ref(),
            tvdb,
            profile,
            root_folder,
            monitor,
        )
        .await?;
        Ok(true)
    }
    async fn items(&self, monitored: Option<bool>) -> skadi_core::Result<Vec<LibraryItemDto>> {
        let series = (&self.store as &dyn TvRepo)
            .list_series(SeriesFilter {
                monitored,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(series.into_iter().map(series_to_dto).collect())
    }

    async fn genres(&self) -> skadi_core::Result<Vec<skadi_api::library::GenreCountDto>> {
        let series = (&self.store as &dyn TvRepo)
            .list_series(SeriesFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(skadi_api::library::genre_counts(
            series.iter().map(|s| s.genres.as_slice()),
        ))
    }

    /// Episodes whose air date falls in the window (SKADI-T-0465).
    ///
    /// Television is the domain the calendar exists for: an episode has a date
    /// that is genuinely a schedule, and the useful question is "what aired that
    /// I still do not have", which is why `has_file` is on the entry.
    ///
    /// Every series is listed unfiltered, because a series being unmonitored
    /// does not mean its already-owned episodes should vanish from the calendar
    /// — `monitored` is carried per entry so a client can grey them instead.
    async fn calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> skadi_core::Result<Vec<skadi_api::library::CalendarEntryDto>> {
        let series = (&self.store as &dyn TvRepo)
            .list_series(SeriesFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        let mut out = Vec::new();
        for s in series {
            for ep in &s.episodes {
                // An episode with no air date is not schedulable — it is the
                // undated case SKADI-T-0403 stopped searching for, and it has no
                // place on a calendar either.
                let Some(date) = ep.air_date else { continue };
                if date < from || date > to {
                    continue;
                }
                out.push(skadi_api::library::CalendarEntryDto {
                    kind: "series".to_string(),
                    id: s.id.0.to_string(),
                    title: ep
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("Episode {}", ep.number)),
                    series: Some(s.title.clone()),
                    episode: Some(format!("S{:02}E{:02}", ep.season, ep.number)),
                    date,
                    has_file: ep.file.is_some(),
                    monitored: ep.monitored,
                });
            }
        }
        Ok(out)
    }

    /// The bound reaches diesel (SKADI-T-0494). It matters more here than for
    /// movies: `list_series` loads every season and episode of each series it
    /// returns, so an unbounded call materialises all 24k episodes.
    async fn items_page(
        &self,
        monitored: Option<bool>,
        limit: Option<usize>,
        offset: usize,
    ) -> skadi_core::Result<Vec<LibraryItemDto>> {
        let series = (&self.store as &dyn TvRepo)
            .list_series(SeriesFilter {
                monitored,
                limit: limit.map(|l| l as i64),
                offset: Some(offset as i64),
            })
            .await?;
        Ok(series.into_iter().map(series_to_dto).collect())
    }

    async fn count(&self, monitored: Option<bool>) -> skadi_core::Result<usize> {
        let n = (&self.store as &dyn TvRepo)
            .count_series(SeriesFilter {
                monitored,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(n.max(0) as usize)
    }

    async fn occupied_folders(&self) -> skadi_core::Result<Vec<std::path::PathBuf>> {
        // Each series occupies `<root>/<Series Folder>` — the immediate child of
        // its root (derived through the naming engine).
        let series = (&self.store as &dyn TvRepo)
            .list_series(SeriesFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(series
            .into_iter()
            .filter_map(|s| {
                let naming = crate::naming::EpisodeNaming {
                    series_title: &s.title,
                    year: s.year,
                    tmdb: s.external_ids.tmdb.as_ref().map(|t| t.0),
                    season: 1,
                    episodes: &[1],
                    series_type: s.series_type,
                    ..Default::default()
                };
                let p = crate::naming::SeriesNaming::default().path(
                    &s.root_folder.path,
                    &naming,
                    std::path::Path::new("x.mkv"),
                );
                let rel = p.strip_prefix(&s.root_folder.path).ok()?;
                let first = rel.components().next()?;
                Some(s.root_folder.path.join(first))
            })
            .collect())
    }
}

// --- manual acquire (SKADI-T-0558) -----------------------------------------
//
// Television had the full automated pipeline — wanted query, matcher, status
// sink, hunter worker — but none of the manual surface movies and audiobooks
// both expose. The gap mattered most exactly where automation struggles: a
// release the decision engine keeps rejecting, or an episode whose numbering the
// matcher will not accept. For those, an operator looks at the candidates and
// picks one; for TV there was nothing to look at.

/// Fetch an episode and confirm it belongs to `series_id`.
///
/// The ownership check is not ceremony: episode ids are opaque, so without it
/// `/series/{a}/episodes/{b}` would happily act on an episode of a different
/// series, and the operator would see the effect on a page they were not
/// looking at.
async fn episode_of(
    http: &TelevisionHttp,
    series_id: SeriesId,
    episode_id: EpisodeId,
) -> Result<crate::episode::Episode, ApiError> {
    let ep = (&http.store as &dyn TvRepo)
        .get_episode(episode_id)
        .await?
        .ok_or_else(|| {
            ApiError(AppError::NotFound(format!(
                "episode {episode_id} not found"
            )))
        })?;
    if ep.series_id != series_id {
        return Err(ApiError(AppError::NotFound(format!(
            "episode {episode_id} does not belong to series {series_id}"
        ))));
    }
    Ok(ep)
}

/// Whether the television domain is enabled — manual acquisition is refused
/// while it is off, matching movies.
async fn domain_enabled(http: &TelevisionHttp) -> Result<bool, ApiError> {
    Ok(http
        .store
        .get(DOMAIN_NAME)
        .await?
        .map(|s| s.enabled)
        .unwrap_or(false))
}

fn domain_disabled_response() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "domain_disabled",
            "message": "the television domain is disabled; enable it before acquiring"
        })),
    )
        .into_response()
}

/// `POST /series/{id}/episodes/{eid}/acquire` — run the pipeline for one episode.
async fn acquire_episode(
    State(http): State<TelevisionHttp>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let series_id: SeriesId = parse_id(&id, "series")?;
    let episode_id: EpisodeId = parse_id(&eid, "episode")?;
    if !domain_enabled(&http).await? {
        return Ok(domain_disabled_response());
    }
    let series = (&http.store as &dyn TvRepo)
        .get_series(series_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {series_id} not found"))))?;
    let episode = episode_of(&http, series_id, episode_id).await?;
    if !start_episode_acquire(&http, &series, &episode) {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this episode is already in progress"
            })),
        )
            .into_response());
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "run_id": eid, "accepted": true })),
    )
        .into_response())
}

/// The manual acquire for one episode, shared by `POST …/acquire` and the bulk
/// search (SKADI-T-0696). Returns `false` (and starts nothing) when a fresh run
/// is already working the episode.
fn start_episode_acquire(
    http: &TelevisionHttp,
    series: &Series,
    episode: &crate::episode::Episode,
) -> bool {
    let episode_id = episode.id;
    // In-flight guard, with the same staleness escape hatch movies uses
    // (SKADI-T-0112): a daemon crash leaves an episode wedged in a non-terminal
    // state with no live run, and without the grace window a manual acquire
    // could never recover it — the operator would be stuck with no lever at all,
    // which is the situation this whole ticket exists to remove.
    let in_flight = matches!(
        episode.status,
        AcquisitionStatus::Searching { .. }
            | AcquisitionStatus::Snatched { .. }
            | AcquisitionStatus::Downloading { .. }
    );
    let fresh = (chrono::Utc::now() - episode.updated_at)
        < chrono::Duration::seconds(skadi_hunter::STALE_ACQUIRE_GRACE.as_secs() as i64);
    if in_flight && fresh {
        return false;
    }
    if in_flight {
        tracing::warn!(
            episode = %episode_id,
            "episode was wedged in a non-terminal acquire state past the recovery window; re-acquiring"
        );
    }

    let seed = crate::wanted::episode_seed(series, episode);
    let runner = http.runner.clone();
    tokio::spawn(async move {
        if let Err(e) = skadi_hunter::start_acquire(&runner, seed).await {
            tracing::warn!(error = %e, "manual episode acquire failed to start");
        }
    });
    true
}

/// `POST /series/{id}/episodes/{eid}/reset` — force a wedged episode back to
/// `Missing` (SKADI-T-0112's recovery path, now available for TV).
///
/// Acts on **this episode only**, even when the wedged download was a season
/// pack. Resetting the siblings a pack covered would be a surprise: the operator
/// asked about one episode, and the others may have imported successfully from
/// the same download.
async fn reset_episode(
    State(http): State<TelevisionHttp>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let series_id: SeriesId = parse_id(&id, "series")?;
    let episode_id: EpisodeId = parse_id(&eid, "episode")?;
    let episode = episode_of(&http, series_id, episode_id).await?;
    let previous = format!("{:?}", episode.status);
    (&http.store as &dyn TvRepo)
        .set_episode_status(episode_id, AcquisitionStatus::Missing)
        .await?;
    tracing::info!(episode = %episode_id, %previous, "episode reset to Missing");
    Ok(Json(serde_json::json!({
        "reset": true,
        "previous": previous,
    }))
    .into_response())
}

/// `GET /series/{id}/episodes/{eid}/releases` — candidate releases with the same
/// accept/reject verdicts movies show.
async fn list_episode_releases(
    State(http): State<TelevisionHttp>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let series_id: SeriesId = parse_id(&id, "series")?;
    let episode_id: EpisodeId = parse_id(&eid, "episode")?;
    let series = (&http.store as &dyn TvRepo)
        .get_series(series_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {series_id} not found"))))?;
    let episode = episode_of(&http, series_id, episode_id).await?;

    let svc = skadi_hunter::try_services_for(MediaKind::Series).ok_or_else(|| {
        ApiError(AppError::Config(
            "providers not initialised yet; try again once the daemon has reconciled".into(),
        ))
    })?;

    let seed = crate::wanted::episode_seed(&series, &episode);
    let mut state = skadi_hunter::AcquireState::new(seed.acquirable, seed.request, seed.profile);
    // The operator asked for this list by hand, so indexers with automatic
    // search off are still consulted (SKADI-T-0539).
    state.request.trigger = skadi_hunter::SearchTrigger::Interactive;
    skadi_hunter::search(&mut state, &svc.indexers)
        .await
        .map_err(ApiError)?;

    let blocklisted = http.store.blocked_keys().await.unwrap_or_default();
    // Upgrade semantics when the episode is already imported: a candidate no
    // better than the held file shows as "not an upgrade" rather than accepted.
    let (current_quality, current_format_score) = match &episode.status {
        AcquisitionStatus::Imported { quality, score, .. } => (Some(*quality), Some(*score)),
        _ => (None, None),
    };
    // Mirror the sweep (SKADI-T-0584): a held file that does not play changes
    // what decide would do, so the preview must know about it too.
    let current_unplayable = episode
        .media_info
        .as_ref()
        .is_some_and(skadi_core::is_broken);
    // The item's own profile, not the daemon-wide active one (SKADI-T-0607):
    // the sweep judges each item on its profile (SKADI-T-0537) and this list
    // must agree with it — an HD-only show was showing SD rips as "accepted".
    let profile = skadi_hunter::services::resolve_profile_by_id(
        &http.store,
        series.profile,
        &svc.scoring.definitions,
    )
    .await
    .unwrap_or_else(|| svc.scoring.profile.clone());
    let scoring = skadi_hunter::Scoring {
        definitions: &svc.scoring.definitions,
        profile: &profile,
        formats: &svc.scoring.formats,
        min_seeders: svc.scoring.min_seeders,
        indexer_flags: &skadi_hunter::indexer_flags(&svc.indexers),
        blocklisted: &blocklisted,
        audiobook: None,
        current_quality,
        current_format_score,
        current_unplayable,
    };
    let now = chrono::Utc::now();
    let wanted_titles = state.request.titles.clone();
    let mut candidates: Vec<EpisodeReleaseCandidate> = state
        .candidates
        .iter()
        .map(|r| {
            let (mut verdict, _) = skadi_hunter::evaluate(r, &scoring);
            // The episode identity gate (SKADI-T-0587): `evaluate` judges the
            // profile axis only, and a bare-title search returns other episodes
            // of the same show, which must not read as accepted.
            if verdict.accepted
                && let Some(scope) = state.request.tv
                && let Some(why) = skadi_hunter::tv_episode_rejection(&r.title, scope)
            {
                verdict = skadi_hunter::Verdict {
                    accepted: false,
                    reason: why.to_string(),
                };
            }
            // Wrong-show gate (SKADI-T-0607): a revival or spin-off sharing the
            // title as a prefix ("… The Return S01E01") is not this show's S01E01.
            if verdict.accepted
                && let Some(why) = skadi_hunter::identity_rejection(
                    skadi_core::MediaKind::Series,
                    &wanted_titles,
                    state.request.year,
                    state.request.tv,
                    r,
                )
            {
                verdict = skadi_hunter::Verdict {
                    accepted: false,
                    reason: why,
                };
            }
            let rel = skadi_quality::title_relevance(&wanted_titles, &r.title);
            EpisodeReleaseCandidate {
                relevance: rel.coverage * 0.75 + rel.precision * 0.25,
                quality: skadi_quality::to_quality(&r.parsed, &svc.scoring.definitions)
                    .and_then(|q| {
                        svc.scoring
                            .definitions
                            .iter()
                            .find(|d| d.id == q.id)
                            .map(|d| d.name.clone())
                    })
                    .unwrap_or_else(|| "Unknown".into()),
                age_days: (now - r.published).num_days(),
                protocol: match r.fetch {
                    skadi_indexers::ReleaseFetch::NzbUrl(_) => "usenet",
                    _ => "torrent",
                },
                // A season pack that covers this episode is a legitimate choice
                // and is listed. Flagged so the UI can say what grabbing it
                // implies — it will satisfy the whole season, not just this row.
                season_pack: r.parsed.full_season,
                accepted: verdict.accepted,
                reason: verdict.reason,
                release_key: skadi_indexers::release_key(r),
                release: r.clone(),
            }
        })
        .collect();
    // Sorted, not filtered: the manual list exists so the operator can reach a
    // candidate the gate rejects, so hiding low-relevance ones would defeat it.
    candidates.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.release_key.cmp(&b.release_key))
    });
    Ok(Json(candidates).into_response())
}

/// One candidate release for an episode.
#[derive(Serialize)]
struct EpisodeReleaseCandidate {
    relevance: f32,
    quality: String,
    age_days: i64,
    protocol: &'static str,
    /// Whether this release is a whole-season pack (SKADI-T-0558). Grabbing one
    /// satisfies every episode in the season, not only the one asked about.
    season_pack: bool,
    accepted: bool,
    reason: String,
    release_key: String,
    release: skadi_indexers::Release,
}

#[derive(Deserialize)]
struct EpisodeGrabRequest {
    release: skadi_indexers::Release,
}

/// `POST /series/{id}/episodes/{eid}/grab` — grab a candidate the operator picked.
async fn grab_episode_release(
    State(http): State<TelevisionHttp>,
    Path((id, eid)): Path<(String, String)>,
    Json(req): Json<EpisodeGrabRequest>,
) -> Result<Response, ApiError> {
    let series_id: SeriesId = parse_id(&id, "series")?;
    let episode_id: EpisodeId = parse_id(&eid, "episode")?;
    launch_episode_grab(&http, series_id, episode_id, req.release).await
}

#[derive(Deserialize)]
struct EpisodeGrabLinkRequest {
    link: String,
    #[serde(default)]
    title: Option<String>,
}

/// `POST /series/{id}/episodes/{eid}/grab-link` — paste a magnet or `.torrent`
/// URL for this episode (closes SKADI-T-0296).
async fn grab_episode_link(
    State(http): State<TelevisionHttp>,
    Path((id, eid)): Path<(String, String)>,
    Json(req): Json<EpisodeGrabLinkRequest>,
) -> Result<Response, ApiError> {
    let series_id: SeriesId = parse_id(&id, "series")?;
    let episode_id: EpisodeId = parse_id(&eid, "episode")?;
    // Parsed with `parse_tv`, not `parse`: the movie parser leaves `SxxEyy` in
    // the work title, so a pasted episode link would carry a title nothing
    // matches (SKADI-T-0549).
    let release = skadi_indexers::release_from_link(
        &req.link,
        req.title.as_deref(),
        skadi_quality::parser::parse_tv,
    )
    .ok_or_else(|| {
        ApiError(AppError::Validation(
            "not a valid magnet or .torrent URL (expected magnet:?xt=urn:btih:… or http(s)://…)"
                .into(),
        ))
    })?;
    launch_episode_grab(&http, series_id, episode_id, release).await
}

/// Shared by `grab` and `grab-link`: validate, honour the domain + in-flight
/// guards, then start the acquire workflow at the **snatch step**.
async fn launch_episode_grab(
    http: &TelevisionHttp,
    series_id: SeriesId,
    episode_id: EpisodeId,
    release: skadi_indexers::Release,
) -> Result<Response, ApiError> {
    if !domain_enabled(http).await? {
        return Ok(domain_disabled_response());
    }
    let series = (&http.store as &dyn TvRepo)
        .get_series(series_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("series {series_id} not found"))))?;
    let episode = episode_of(http, series_id, episode_id).await?;

    let in_flight = matches!(
        episode.status,
        AcquisitionStatus::Searching { .. }
            | AcquisitionStatus::Snatched { .. }
            | AcquisitionStatus::Downloading { .. }
    );
    let fresh = (chrono::Utc::now() - episode.updated_at)
        < chrono::Duration::seconds(skadi_hunter::STALE_ACQUIRE_GRACE.as_secs() as i64);
    if in_flight && fresh {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this episode is already in progress"
            })),
        )
            .into_response());
    }

    let seed = crate::wanted::episode_seed(&series, &episode);
    let runner = http.runner.clone();
    let (acquirable, request, profile) = (seed.acquirable, seed.request, seed.profile);
    tokio::spawn(async move {
        if let Err(e) =
            skadi_hunter::start_grab(&runner, acquirable, request, profile, release).await
        {
            tracing::warn!(error = %e, "manual episode grab failed to start");
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "grabbed": true })),
    )
        .into_response())
}

/// `GET /series/{id}/episodes/{eid}/video` — the imported file's bytes, with
/// single-range support so a player can seek (SKADI-T-0574).
///
/// Direct play: skadi has no encoder, so the client decodes what is on disk.
/// Measured over this library that is ~98 % viable — HEVC/H.264 with AAC, AC3 or
/// E-AC3, all of which ExoPlayer handles. The residue is DTS.
///
/// Ownership is checked by `episode_of`, for the reason its own docs give:
/// episode ids are opaque, so without it `/series/{a}/episodes/{b}/video` would
/// serve a file from a different series.
async fn episode_video(
    State(http): State<TelevisionHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let path = episode_video_path(&http, member, &id, &eid).await?;
    skadi_api::ranged::serve_file_range(&path, &headers, "video").await
}

/// The imported file of episode `eid` of series `id`, after the same checks
/// the video route makes.
async fn episode_video_path(
    http: &TelevisionHttp,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    id: &str,
    eid: &str,
) -> Result<std::path::PathBuf, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    let series_id: SeriesId = parse_id(id, "series")?;
    let episode_id: EpisodeId = parse_id(eid, "episode")?;
    if !member.is_admin() {
        let s = repo(http).get_series(series_id).await?.filter(|s| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Series,
                &s.id.to_string(),
                s.content_rating.as_deref(),
                &s.genres,
            )
        });
        if s.is_none() {
            return Err(ApiError(AppError::NotFound(format!(
                "series {series_id} not found"
            ))));
        }
    }
    let ep = episode_of(http, series_id, episode_id).await?;
    match &ep.status {
        AcquisitionStatus::Imported { file, .. } => Ok(file.path.clone()),
        _ => Err(ApiError(AppError::NotFound(
            "episode is not imported — no video to play yet".into(),
        ))),
    }
}

/// `GET /series/{id}/episodes/{eid}/markers` — where the intro and credits are,
/// from the file's named chapters (SKADI-T-0666). All `null` when the file has
/// no such chapters, or `ffprobe` is unavailable.
async fn episode_markers(
    State(http): State<TelevisionHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Json<skadi_media_probe::markers::SkipMarkers>, ApiError> {
    let path = episode_video_path(&http, member, &id, &eid).await?;
    let markers = tokio::task::spawn_blocking(move || {
        let chapters = skadi_media_probe::ffprobe::chapters(&path);
        skadi_media_probe::markers::skip_markers(&chapters, 0.0)
    })
    .await
    .map_err(|e| ApiError(AppError::Internal(format!("marker probe panicked: {e}"))))?;
    Ok(Json(markers))
}

/// `GET /series/{id}/episodes/{eid}/subtitles` — the subtitle files beside the
/// episode's video (SKADI-T-0663).
async fn episode_subtitles(
    State(http): State<TelevisionHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Json<Vec<skadi_importer::subtitles::SubtitleInfo>>, ApiError> {
    let path = episode_video_path(&http, member, &id, &eid).await?;
    let list = tokio::task::spawn_blocking(move || skadi_importer::subtitles::listing(&path))
        .await
        .map_err(|e| {
            ApiError(AppError::Internal(format!(
                "subtitle listing panicked: {e}"
            )))
        })?;
    Ok(Json(list))
}

#[derive(Debug, Deserialize)]
struct SubtitleQuery {
    /// `vtt` converts to WebVTT, for browsers.
    format: Option<String>,
}

/// `GET /series/{id}/episodes/{eid}/subtitles/{n}` — subtitle `n` as UTF-8
/// text; `?format=vtt` converts it (SKADI-T-0663).
async fn episode_subtitle(
    State(http): State<TelevisionHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid, n)): Path<(String, String, usize)>,
    Query(q): Query<SubtitleQuery>,
) -> Result<Response, ApiError> {
    let path = episode_video_path(&http, member, &id, &eid).await?;
    let webvtt = q.format.as_deref() == Some("vtt");
    let found =
        tokio::task::spawn_blocking(move || skadi_importer::subtitles::body(&path, n, webvtt))
            .await
            .map_err(|e| ApiError(AppError::Internal(format!("subtitle read panicked: {e}"))))?;
    let (ct, text) =
        found.ok_or_else(|| ApiError(AppError::NotFound(format!("no subtitle {n}"))))?;
    Ok(([(axum::http::header::CONTENT_TYPE, ct)], text).into_response())
}
