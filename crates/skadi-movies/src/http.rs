//! The movies domain's HTTP surface (SKADI-T-0054).
//!
//! Implements [`HttpModule`](skadi_api::HttpModule) so the daemon can merge the
//! movies routes into `/api/v1` without `skadi-api` depending on this crate.
//! Covers movie library management (`/movies`), the manual acquire trigger
//! (`/movies/{id}/editions/{eid}/acquire`), and the `edition_kinds` registry
//! CRUD (`/edition-kinds`) that was deferred here from SKADI-T-0053 (it lives in
//! `skadi-movies`, not `skadi-store`).
//!
//! All handlers are stated on [`MoviesHttp`], a cheap-to-clone bundle of the
//! store, a metadata provider, and the module's Cloacina runner. The daemon
//! constructs one via [`MoviesHttp::new`] and hands `routes()` to the API.

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

use chrono::Utc;
use skadi_api::bulk::{BulkAction, BulkOutcome, BulkReport, BulkRequest};
use skadi_api::{ApiError, HttpModule, LibraryEditionDto, LibraryItemDto, LibraryProvider};

use skadi_core::{
    AcquisitionStatus, AppError, EditionKindId, ExternalIds, IndexerId, MediaKind, MovieEditionId,
    MovieId, ProfileId, QualityId, RootFolder, TmdbId,
};
use skadi_hunter::{AcquireSeed, SearchSpec, start_acquire};
use skadi_indexers::{Category, Release, ReleaseFetch};
use skadi_metadata::{ExternalId, MetadataProvider, MetadataQuery};
use skadi_store::{BlocklistRepo, ConfigRepo, DomainStateRepo, SettingsRepo, Store};

use crate::edition::{EditionKind, MovieEdition};
use crate::import;
use crate::metadata::add_movie;
use crate::movie::Movie;
use crate::repo::{MovieFilter, MoviesRepo};

/// The movies domain name in the `domains` table (must match
/// [`MoviesModule::name`](crate::module::MoviesModule)).
const DOMAIN_NAME: &str = "movies";

/// Default newznab/torznab category for movies, used to seed the manual-acquire
/// search spec.
const MOVIE_CATEGORY: u32 = 2000;

/// Max metadata matches returned from `GET /movies/lookup` (SKADI-T-0248). An add
/// picker only needs the closest few; the unified add page stacks three domains,
/// so an uncapped per-domain list floods it.
const MAX_LOOKUP_RESULTS: usize = 10;

/// Shared state for the movies HTTP handlers.
#[derive(Clone)]
pub struct MoviesHttp {
    store: Store,
    provider: Arc<dyn MetadataProvider>,
    runner: Arc<cloacina::runner::DefaultRunner>,
}

impl MoviesHttp {
    /// Bundle the pieces the movies routes need. The daemon passes the store,
    /// the configured TMDB provider, and the module's runner
    /// ([`MoviesModule::runner`](crate::module::MoviesModule::runner)).
    pub fn new(
        store: Store,
        provider: Arc<dyn MetadataProvider>,
        runner: Arc<cloacina::runner::DefaultRunner>,
    ) -> Self {
        Self {
            store,
            provider,
            runner,
        }
    }
}

impl HttpModule for MoviesHttp {
    fn routes(&self) -> Router {
        Router::new()
            .route("/movies", get(list_movies).post(create_movie))
            // Static `/movies/lookup` registered before `/movies/{id}` — the
            // metadata search the "add movie" UI uses (SKADI-T-0069).
            .route("/movies/lookup", get(lookup_movies))
            // One request for a selection on the wall (SKADI-T-0696).
            .route("/movies/bulk", post(bulk_movies))
            .route(
                "/movies/{id}",
                get(get_movie).patch(patch_movie).delete(delete_movie),
            )
            // Per-item and bulk metadata refresh (SKADI-T-0450) — Radarr's
            // "Refresh & Scan". Both drive the same `refresh_movie_metadata`
            // workflow the scheduled worker uses, so an operator-triggered refresh
            // and a stale-sweep refresh cannot drift apart.
            .route("/movies/{id}/refresh", post(refresh_movie_route))
            .route("/movies/refresh", post(refresh_all_movies))
            // Library media scan (SKADI-T-0583): how far the background profiler
            // has got, and what it found that will not direct-play.
            .route("/movies/scan", get(scan_status))
            .route("/movies/streaming-report", get(streaming_report))
            .route("/movies/{id}/editions/{eid}/acquire", post(acquire_edition))
            // Video bytes for a player, with Range/seek (SKADI-T-0574).
            .route("/movies/{id}/editions/{eid}/video", get(edition_video))
            .route("/movies/{id}/editions/{eid}/subtitles", get(edition_subtitles))
            .route("/movies/{id}/editions/{eid}/subtitles/{n}", get(edition_subtitle))
            // Interactive search + manual grab (SKADI-T-0114): list scored
            // candidate releases for an edition, and grab a chosen one.
            .route(
                "/movies/{id}/editions/{eid}/releases",
                get(list_releases),
            )
            .route("/movies/{id}/editions/{eid}/grab", post(grab_release))
            // Manual acquisition (SKADI-I-0043): paste a magnet or .torrent URL for
            // this edition; synthesized into a release + fed the same grab pipeline.
            .route(
                "/movies/{id}/editions/{eid}/grab-link",
                post(grab_link),
            )
            // Test-a-title / explain-decision (SKADI-T-0184): explain how a
            // release title would be judged by the live movies profile + custom
            // formats, with no search or grab. Static path before `/movies/{id}`
            // wildcards is irrelevant here (distinct prefix), but kept grouped.
            .route("/movies/quality/test", post(test_release))
            // Recover a wedged edition: force its status back to `Missing` so it
            // can be re-acquired (SKADI-T-0112). The operator escape hatch when
            // an acquire run was lost (e.g. a daemon crash) and the edition is
            // stuck non-terminal.
            .route("/movies/{id}/editions/{eid}/reset", post(reset_edition))
            // Library import (SKADI-T-0074): scan a source path + commit
            // matched items by restructuring (hardlink) into the root folder.
            .route("/library-import/scan", post(library_import_scan))
            .route("/library-import/match", post(library_import_match))
            .route("/library-import/commit", post(library_import_commit))
            .route("/edition-kinds", get(list_kinds).post(create_kind))
            .route(
                "/edition-kinds/{id}",
                get(get_kind).put(update_kind).delete(delete_kind),
            )
            .with_state(self.clone())
    }
}

// --- helpers ---

fn parse_id<T: From<Uuid>>(s: &str, what: &str) -> Result<T, ApiError> {
    Uuid::parse_str(s)
        .map(T::from)
        .map_err(|e| ApiError(AppError::Validation(format!("invalid {what} id: {e}"))))
}

fn repo(http: &MoviesHttp) -> &dyn MoviesRepo {
    &http.store
}

// --- movies ---

#[derive(Deserialize)]
struct ListParams {
    monitored: Option<bool>,
    /// Page size. Absent means every row, which is what this endpoint used to do
    /// unconditionally — 2.8 MB over a prod-sized library (SKADI-T-0494).
    limit: Option<i64>,
    /// Rows to skip; ignored without a `limit`.
    offset: Option<i64>,
    /// Comma-separated tag ids; a movie matches if it carries **any** of them
    /// (SKADI-T-0550). Comma-separated because that is the shape a tag label can
    /// never contain — SKADI-T-0464 refuses a comma in a label precisely so this
    /// stays unambiguous.
    tags: Option<String>,
}

async fn list_movies(
    State(http): State<MoviesHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    // A non-admin's page is cut from the *policy-filtered* list, so the store
    // is asked for everything and paged here (SKADI-T-0617): filtering the
    // store's page instead handed a kid a short first page, which the app
    // reads as "that was the last one" — everything added later never showed.
    let paged_by_store = member.is_admin();
    let filter = MovieFilter {
        monitored: params.monitored,
        limit: if paged_by_store { params.limit } else { None },
        offset: if paged_by_store { params.offset } else { None },
    };
    // The total ignores paging, so a client can size its page controls.
    let mut total = repo(&http)
        .count_movies(MovieFilter {
            limit: None,
            offset: None,
            ..filter
        })
        .await?;
    let mut movies = repo(&http).list_movies(filter).await?;
    if !paged_by_store {
        // Household policy (SKADI-T-0612): what this member may not see is not here.
        movies.retain(|m| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Movie,
                &m.id.to_string(),
                m.content_rating.as_deref(),
                &m.genres,
            )
        });
        total = movies.len() as i64;
        let offset = params.offset.unwrap_or(0).max(0) as usize;
        let limit = params.limit.map_or(usize::MAX, |l| l.max(0) as usize);
        movies = movies.into_iter().skip(offset).take(limit).collect();
    }
    // Tag filtering is applied after the query rather than joined into it
    // (SKADI-T-0550): `item_tags` lives in the shared store and movies in the
    // domain schema, so there is no cross-schema join to push down. Bounded by
    // the page the caller already asked for.
    if let Some(raw) = params.tags.as_deref() {
        use skadi_store::ItemTagRepo;
        let ids: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        // `?tags=` with nothing usable in it is a caller mistake. Returning the
        // unfiltered list would look like the filter worked and quietly showed
        // everything, so refuse instead.
        if ids.is_empty() {
            return Err(ApiError(AppError::Validation(
                "tags= was given but contained no tag ids".into(),
            )));
        }
        let matching: std::collections::HashSet<String> = http
            .store
            .items_with_any_tag("movie", &ids)
            .await?
            .into_iter()
            .collect();
        movies.retain(|m| matching.contains(&m.id.0.to_string()));
    }
    Ok(([("x-total-count", total.to_string())], Json(movies)))
}

#[derive(Deserialize)]
struct LookupParams {
    q: String,
    year: Option<u16>,
}

/// One metadata search result the "add movie" UI can act on (SKADI-T-0069).
#[derive(Serialize)]
struct LookupResult {
    tmdb_id: u64,
    title: String,
    year: Option<u16>,
    score: f32,
    /// Poster URL + short synopsis (when the provider's search carries them), so
    /// the add page can show artwork + a description without a second lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overview: Option<String>,
}

/// `GET /movies/lookup?q=&year=` — free-text metadata search via the configured
/// provider. Results without a TMDB id are dropped (the create path keys on it).
async fn lookup_movies(
    State(http): State<MoviesHttp>,
    Query(params): Query<LookupParams>,
) -> Result<impl IntoResponse, ApiError> {
    let query = skadi_metadata::MetadataQuery {
        title: params.q,
        year: params.year,
        kind: MediaKind::Movie,
    };
    let matches = http.provider.search(&query).await.map_err(ApiError)?;
    let mut results: Vec<LookupResult> = matches
        .into_iter()
        .filter_map(|m| {
            m.external_ids.tmdb.map(|t| LookupResult {
                tmdb_id: t.0,
                title: m.title,
                year: m.year,
                score: m.score,
                poster_url: m.poster_url,
                overview: m.overview,
            })
        })
        .collect();
    // Rank by relevance and cap the list (SKADI-T-0248): an add-search picker only
    // needs the closest matches — returning every provider hit floods the unified
    // add page (movies + TV + audiobooks all at once).
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    results.truncate(MAX_LOOKUP_RESULTS);
    Ok(Json(results))
}

#[derive(Deserialize)]
struct AddMovieRequest {
    tmdb_id: u64,
    /// Quality profile id. Omitted → the first registered profile. When given,
    /// it must reference a registered `profiles` settings row (SKADI-I-0012).
    profile: Option<String>,
    /// Kick off the Theatrical edition's acquire run right away (default).
    /// Needs the movies domain enabled; otherwise the add still succeeds and
    /// the response carries `search_started: false`.
    #[serde(default = "default_search")]
    search: bool,
}

fn default_search() -> bool {
    true
}

/// The single `library.root` from the config plane (SKADI-T-0302), or the
/// registry default when unset. skadi owns the layout; each domain root is
/// `<library.root>/<kind subfolder>` — no operator root picker.
pub(crate) async fn library_root(store: &Store) -> PathBuf {
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

async fn create_movie(
    State(http): State<MoviesHttp>,
    Json(req): Json<AddMovieRequest>,
) -> Result<Response, ApiError> {
    // Resolve the profile: explicit ids must reference a registered profiles
    // row (typos become 422s, not movies bound to nonexistent config).
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

    // The root is derived, not chosen (SKADI-T-0302): movies live under
    // `<library.root>/movie`. `req.root_folder` is ignored (kept for wire compat).
    let root_folder = RootFolder::for_domain(library_root(&http.store).await, MediaKind::Movie);

    match add_movie(
        repo(&http),
        http.provider.as_ref(),
        TmdbId(req.tmdb_id),
        profile,
        root_folder,
    )
    .await
    {
        Ok(movie) => {
            let search_started = if req.search {
                maybe_start_search(&http, &movie).await?
            } else {
                false
            };
            let mut body = serde_json::to_value(&movie)
                .map_err(|e| ApiError(AppError::Internal(format!("serializing movie: {e}"))))?;
            body["search_started"] = serde_json::json!(search_started);
            Ok((StatusCode::CREATED, Json(body)).into_response())
        }
        // add_movie returns Validation for a duplicate tmdb id; surface that as
        // 409 Conflict rather than the generic 400 the mapper would give.
        Err(AppError::Validation(msg)) if msg.contains("already exists") => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "duplicate", "message": msg })),
        )
            .into_response()),
        Err(e) => Err(ApiError(e)),
    }
}

/// Search-on-add (SKADI-I-0012): fire the acquire run for the new movie's
/// Theatrical edition when the movies domain is enabled. Fire-and-forget like
/// the manual acquire endpoint; returns whether a run was started.
async fn maybe_start_search(http: &MoviesHttp, movie: &Movie) -> Result<bool, ApiError> {
    let enabled = http
        .store
        .get(DOMAIN_NAME)
        .await?
        .map(|s| s.enabled)
        .unwrap_or(false);
    let Some(edition) = movie.editions.first() else {
        return Ok(false);
    };
    if !enabled {
        return Ok(false);
    }
    let seed = acquire_seed(movie, edition);
    let runner = http.runner.clone();
    tokio::spawn(async move {
        if let Err(e) = start_acquire(&runner, seed).await {
            tracing::warn!(error = %e, "search-on-add acquire run failed to start");
        }
    });
    Ok(true)
}

/// Refresh one movie's metadata now (SKADI-T-0450).
///
/// Runs the same `refresh_movie_metadata` workflow as the scheduled worker rather
/// than calling `refresh_movie` directly: the workflow carries the circuit breaker
/// and the persistence, so a second path into it would be a second set of bugs.
async fn refresh_movie_route(
    State(http): State<MoviesHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id: MovieId = parse_id(&id, "movie")?;
    // 404 before doing any work, so a typo'd id is not reported as a queued refresh.
    repo(&http)
        .get_movie(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {id} not found"))))?;
    use cloacina::executor::WorkflowExecutor;
    let ctx = crate::refresh::refresh_context(&id.to_string())?;
    http.runner
        .execute("refresh_movie_metadata", ctx)
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("refresh run failed: {e}"))))?;
    Ok(Json(serde_json::json!({ "refreshed": id.to_string() })))
}

/// Refresh every movie (Radarr's library-wide refresh, SKADI-T-0450).
///
/// Deliberately **not** parallel and deliberately not fire-and-forget-per-item:
/// a library-wide refresh is one provider request per movie, and a whole library
/// at once is exactly the burst that gets an API key rate-limited. Runs serially
/// and reports what it managed, so a partial failure is visible instead of
/// silently leaving half the library stale.
async fn refresh_all_movies(State(http): State<MoviesHttp>) -> Result<impl IntoResponse, ApiError> {
    use cloacina::executor::WorkflowExecutor;
    let movies = repo(&http)
        .list_movies(MovieFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await?;
    let requested = movies.len();
    let mut refreshed = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for movie in movies {
        let Ok(ctx) = crate::refresh::refresh_context(&movie.id.to_string()) else {
            failed.push(movie.id.to_string());
            continue;
        };
        match http.runner.execute("refresh_movie_metadata", ctx).await {
            Ok(_) => refreshed += 1,
            Err(e) => {
                tracing::warn!(movie = %movie.id, error = %e, "bulk refresh: run failed");
                failed.push(movie.id.to_string());
            }
        }
    }
    Ok(Json(serde_json::json!({
        "requested": requested,
        "refreshed": refreshed,
        "failed": failed,
    })))
}

/// `GET /movies/scan` — background media-scan progress (SKADI-T-0583).
async fn scan_status(State(http): State<MoviesHttp>) -> Result<impl IntoResponse, ApiError> {
    use skadi_library_scan::ScanSource as _;
    let source = crate::scan::MovieScanSource::new(http.store.clone());
    let p = source.progress().await?;
    Ok(Json(serde_json::json!({
        "scanned": p.scanned,
        "total": p.total,
        "remaining": p.remaining(),
        "complete": p.complete(),
    })))
}

/// One file that will not direct-play cleanly.
#[derive(serde::Serialize)]
struct RepackCandidate {
    movie_id: String,
    edition_id: String,
    title: String,
    year: Option<u16>,
    file_path: Option<String>,
    container: Option<String>,
    size_bytes: Option<u64>,
    overall_bitrate_kbps: Option<u32>,
    /// Worst remedy across this file's issues — what a *local* fix would cost,
    /// kept as diagnosis even though skadi re-acquires rather than repacks.
    remedy: skadi_core::Remedy,
    severity: skadi_core::Severity,
    /// Whether the sweep will re-acquire this file on its own (SKADI-T-0584).
    ///
    /// True for `Broken` only. A big-but-playable remux is reported so the
    /// operator knows, and left alone — spending a download to fix buffering is
    /// their call, not the daemon's. Also requires the item's profile to allow
    /// upgrades, which is per-item and not resolved here.
    will_regrab: bool,
    issues: Vec<skadi_core::StreamingIssue>,
}

/// `GET /movies/streaming-report` — what to repack, and why (SKADI-T-0583).
///
/// Built from stored probe results, so it is a database read rather than a walk
/// of the library: the scan worker does the expensive part once, in the
/// background, and this stays fast enough to hit from a UI.
async fn streaming_report(State(http): State<MoviesHttp>) -> Result<impl IntoResponse, ApiError> {
    let movies = repo(&http)
        .list_movies(MovieFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await?;

    let mut candidates: Vec<RepackCandidate> = Vec::new();
    let mut assessed = 0usize;
    for movie in &movies {
        for e in &movie.editions {
            let Some(info) = e.media_info.as_ref() else {
                continue;
            };
            assessed += 1;
            let issues = skadi_core::assess(info);
            if issues.is_empty() {
                continue;
            }
            candidates.push(RepackCandidate {
                movie_id: movie.id.to_string(),
                edition_id: e.id.to_string(),
                title: movie.title.clone(),
                year: movie.year,
                file_path: e
                    .file
                    .as_ref()
                    .map(|f| f.path.to_string_lossy().into_owned()),
                container: info.container.clone(),
                size_bytes: info.size_bytes,
                overall_bitrate_kbps: info.overall_bitrate_kbps,
                // The most expensive remedy wins: a file needing both a remux
                // and a re-encode is a re-encode job, and grouping it under
                // "remux" would understate the work.
                remedy: issues
                    .iter()
                    .map(skadi_core::StreamingIssue::remedy)
                    .max()
                    .unwrap_or(skadi_core::Remedy::Remux),
                severity: issues
                    .iter()
                    .map(skadi_core::StreamingIssue::severity)
                    .max()
                    .unwrap_or(skadi_core::Severity::Annoying),
                will_regrab: skadi_core::is_broken(info),
                issues,
            });
        }
    }
    // Worst first, then biggest — the order someone works down the list in.
    candidates.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.size_bytes.cmp(&a.size_bytes))
    });

    // Counts by remedy, so the answer to "what should I repack" leads with how
    // much of it is cheap.
    let mut by_remedy: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for c in &candidates {
        *by_remedy
            .entry(format!("{:?}", c.remedy).to_lowercase())
            .or_default() += 1;
    }

    let will_regrab = candidates.iter().filter(|c| c.will_regrab).count();
    // Bytes tied up in files the sweep will replace — the number that says
    // whether this is a quiet background job or a weekend of downloading.
    let regrab_bytes: u64 = candidates
        .iter()
        .filter(|c| c.will_regrab)
        .filter_map(|c| c.size_bytes)
        .sum();
    Ok(Json(serde_json::json!({
        "assessed": assessed,
        "flagged": candidates.len(),
        "will_regrab": will_regrab,
        "regrab_bytes": regrab_bytes,
        "by_remedy": by_remedy,
        "candidates": candidates,
    })))
}

async fn get_movie(
    State(http): State<MoviesHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    let id: MovieId = parse_id(&id, "movie")?;
    let movie = repo(&http)
        .get_movie(id)
        .await?
        .filter(|m| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Movie,
                &m.id.to_string(),
                m.content_rating.as_deref(),
                &m.genres,
            )
        })
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {id} not found"))))?;
    Ok(Json(movie))
}

#[derive(Deserialize, Default)]
struct PatchMovie {
    monitored: Option<bool>,
    profile: Option<String>,
    /// Accepted for wire compatibility and **ignored** (SKADI-T-0162), exactly as
    /// `create_movie` already ignores it.
    #[allow(dead_code)]
    root_folder: Option<String>,
    /// The item's tags, as settings-record ids (SKADI-T-0550).
    ///
    /// Absent means "leave them alone"; `[]` means "remove them all". Those are
    /// different edits, which is why this is `Option<Vec<_>>` and not `Vec<_>` —
    /// a bare vec would make every PATCH that omits tags silently clear them.
    tags: Option<Vec<String>>,
}

async fn patch_movie(
    State(http): State<MoviesHttp>,
    Path(id): Path<String>,
    Json(patch): Json<PatchMovie>,
) -> Result<impl IntoResponse, ApiError> {
    let id: MovieId = parse_id(&id, "movie")?;
    Ok(Json(apply_patch(&http, id, patch).await?))
}

/// The one PATCH path, shared by `PATCH /movies/{id}` and the bulk
/// monitor/unmonitor (SKADI-T-0696). 404 when the movie is not there.
async fn apply_patch(http: &MoviesHttp, id: MovieId, patch: PatchMovie) -> Result<Movie, ApiError> {
    let mut movie = repo(http)
        .get_movie(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {id} not found"))))?;
    if let Some(m) = patch.monitored {
        movie.monitored = m;
    }
    if let Some(p) = patch.profile {
        movie.profile = parse_id(&p, "profile")?;
    }
    // `root_folder` is NOT applied (SKADI-T-0162). Skadi owns one mounted
    // filesystem and derives each domain's folder from the media kind
    // (`<library.root>/movie`, SKADI-T-0302), so there is no root to choose —
    // and honouring an arbitrary one here was the last way left to misfile a
    // movie into the audiobooks tree, which is exactly what this ticket names.
    // `create_movie` already ignored it; this was the inconsistency.
    //
    // Silently ignored rather than rejected: the field is still in the wire
    // shape, and an old client sending it should not start failing.
    repo(http).upsert_movie(&movie).await?;
    if let Some(tags) = patch.tags {
        use skadi_store::ItemTagRepo;
        http.store
            .set_tags("movie", &movie.id.0.to_string(), &tags)
            .await?;
    }
    Ok(movie)
}

/// Delete options (SKADI-T-0316): `?delete_files=true` also removes the imported files +
/// prunes the now-empty folders. The web UI sends it behind a confirmation.
#[derive(serde::Deserialize)]
struct DeleteOpts {
    #[serde(default)]
    delete_files: bool,
}

async fn delete_movie(
    State(http): State<MoviesHttp>,
    Path(id): Path<String>,
    axum::extract::Query(opts): axum::extract::Query<DeleteOpts>,
) -> Result<impl IntoResponse, ApiError> {
    let id: MovieId = parse_id(&id, "movie")?;
    // 404 if it isn't there, so DELETE is honest about what it removed.
    if !delete_one(&http, id, opts.delete_files).await? {
        return Err(ApiError(AppError::NotFound(format!(
            "movie {id} not found"
        ))));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The one delete path, shared by `DELETE /movies/{id}` and the bulk delete
/// (SKADI-T-0696). With `delete_files`, removes the files the movie's editions
/// record as imported (those exact paths, nothing else) and prunes the emptied
/// folders up to the movie's root, never past it. Returns `false` when the
/// movie is not there.
async fn delete_one(http: &MoviesHttp, id: MovieId, delete_files: bool) -> Result<bool, ApiError> {
    let Some(movie) = repo(http).get_movie(id).await? else {
        return Ok(false);
    };
    // Remove the imported files + prune empty folders first (SKADI-T-0316), before the rows go.
    if delete_files {
        let paths: Vec<std::path::PathBuf> = movie
            .editions
            .iter()
            .filter_map(|e| match &e.status {
                skadi_core::AcquisitionStatus::Imported { file, .. } => Some(file.path.clone()),
                _ => None,
            })
            .collect();
        let root = std::path::PathBuf::from(&movie.root_folder.path);
        let removed = tokio::task::spawn_blocking(move || {
            skadi_importer::delete_files_and_prune(&paths, &root)
        })
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("delete task panicked: {e}"))))?;
        tracing::info!(movie = %id, removed = removed.len(), "deleted movie files on library delete");
    }
    // Take the item's tag membership with it (SKADI-T-0560). Otherwise the rows
    // outlive the item and a later one reusing the id would inherit them — the
    // silent orphaning the vision calls out as something skadi does not do.
    {
        use skadi_store::ItemTagRepo;
        let _ = http.store.clear_item("movie", &id.0.to_string()).await;
    }
    repo(http).delete_movie(id).await?;
    Ok(true)
}

/// `POST /movies/bulk` (SKADI-T-0696): apply one action to many movies in one
/// request. Each id goes through the single-item path (`apply_patch`,
/// `start_edition_acquire`, `delete_one`), so a bulk action cannot do what the
/// single route would not.
async fn bulk_movies(
    State(http): State<MoviesHttp>,
    skadi_api::error::ApiJson(req): skadi_api::error::ApiJson<BulkRequest>,
) -> Result<Response, ApiError> {
    let ids: Vec<(String, MovieId)> = req.parsed_ids()?;
    if req.action == BulkAction::Search && !domain_enabled(&http).await? {
        return Ok(domain_disabled_response());
    }
    let mut report = BulkReport::new(req.action, ids.len());
    for (raw, id) in ids {
        let outcome = match req.action {
            BulkAction::Monitor | BulkAction::Unmonitor => {
                let patch = PatchMovie {
                    monitored: Some(req.action == BulkAction::Monitor),
                    ..PatchMovie::default()
                };
                apply_patch(&http, id, patch)
                    .await
                    .map(|_| BulkOutcome::Done)
            }
            BulkAction::Search => search_movie(&http, id).await,
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

/// Search one movie now: start the manual acquire for each edition that is not
/// imported and not already in flight.
async fn search_movie(http: &MoviesHttp, id: MovieId) -> Result<BulkOutcome, ApiError> {
    let Some(movie) = repo(http).get_movie(id).await? else {
        return Ok(BulkOutcome::NotFound);
    };
    let mut started = 0;
    for edition in &movie.editions {
        if matches!(edition.status, AcquisitionStatus::Imported { .. }) {
            continue;
        }
        if start_edition_acquire(http, &movie, edition) {
            started += 1;
        }
    }
    Ok(BulkOutcome::Searched(started))
}

// --- manual acquire ---

/// Build the acquire seed for one (movie, edition) — the single definition of
/// what a movie search looks like, shared by the manual acquire endpoint and
/// search-on-add.
fn acquire_seed(movie: &Movie, edition: &MovieEdition) -> AcquireSeed {
    AcquireSeed {
        acquirable: edition.acquirable_ref(),
        request: SearchSpec {
            // Sweep-driven; the interactive paths override this just before
            // searching (SKADI-T-0539).
            trigger: skadi_hunter::SearchTrigger::Automatic,
            kind: MediaKind::Movie,
            titles: vec![movie.title.clone()],
            year: movie.year,
            external_ids: ExternalIds {
                tmdb: movie.external_ids.tmdb.clone(),
                // Pass the imdb id too (parity with the sweep): indexers that
                // support id search match on imdb (SKADI-T-0069 follow-up).
                imdb: movie.external_ids.imdb.clone(),
                ..Default::default()
            },
            categories: vec![Category(MOVIE_CATEGORY)],
            tv: None,
            series: None,
            // Filled in by `steps::search` from the item's tags
            // (SKADI-T-0556) — a database read, so it cannot happen in these
            // pure seed builders.
            tags: None,
        },
        profile: movie.profile,
        // Manual/interactive acquire is a first-acquisition path; upgrades flow
        // through the sweep's `upgradable()` (SKADI-T-0182 / SKADI-T-0186).
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    }
}

async fn acquire_edition(
    State(http): State<MoviesHttp>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let movie_id: MovieId = parse_id(&id, "movie")?;
    let edition_id: MovieEditionId = parse_id(&eid, "edition")?;

    // Manual acquire requires the movies domain to be enabled.
    if !domain_enabled(&http).await? {
        return Ok(domain_disabled_response());
    }

    let movie = repo(&http)
        .get_movie(movie_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {movie_id} not found"))))?;
    let edition = repo(&http).get_edition(edition_id).await?.ok_or_else(|| {
        ApiError(AppError::NotFound(format!(
            "edition {edition_id} not found"
        )))
    })?;
    if edition.movie_id != movie_id {
        return Err(ApiError(AppError::NotFound(format!(
            "edition {edition_id} does not belong to movie {movie_id}"
        ))));
    }
    if !start_edition_acquire(&http, &movie, &edition) {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this edition is already in progress"
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

/// Whether the movies domain is enabled; manual acquire is refused while off.
async fn domain_enabled(http: &MoviesHttp) -> Result<bool, ApiError> {
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
            "message": "the movies domain is disabled; enable it before acquiring"
        })),
    )
        .into_response()
}

/// The manual acquire for one edition, shared by `POST …/acquire` and the bulk
/// search (SKADI-T-0696). Returns `false` (and starts nothing) when a fresh run
/// is already working the edition.
fn start_edition_acquire(http: &MoviesHttp, movie: &Movie, edition: &MovieEdition) -> bool {
    let edition_id = edition.id;
    // In-flight guard (SKADI-I-0012 Flow 3): an edition already being worked
    // (searching / snatched / downloading) must not get a second concurrent
    // run — that would double-snatch and double-download. Full sweep-side
    // dedup is SKADI-T-0039; this closes the manual-trigger path.
    //
    // BUT only while the run is *fresh*. A daemon crash leaves an edition wedged
    // in a non-terminal state with no live run (the in-memory tracker is cleared
    // on restart, and Cloacina's stale-claim recovery is fixed at ~60-90s and was
    // unreliable in practice — SKADI-T-0112). If the edition has been stuck well
    // past Cloacina's recovery window, treat the in-flight state as stale and let
    // a fresh acquire recover it instead of wedging forever. `POST …/reset`
    // forces immediate recovery.
    let in_flight = matches!(
        edition.status,
        AcquisitionStatus::Searching { .. }
            | AcquisitionStatus::Snatched { .. }
            | AcquisitionStatus::Downloading { .. }
    );
    let fresh = (Utc::now() - edition.updated_at)
        < chrono::Duration::seconds(skadi_hunter::STALE_ACQUIRE_GRACE.as_secs() as i64);
    if in_flight && fresh {
        return false;
    }
    if in_flight {
        tracing::warn!(
            edition = %edition_id,
            "edition was wedged in a non-terminal acquire state past the recovery window; re-acquiring"
        );
    }

    let seed = acquire_seed(movie, edition);

    // Fire-and-forget: the acquire workflow runs to completion on its own task,
    // and the edition's status is persisted by the pipeline's status sink. We
    // return 202 immediately with the edition id as the correlation reference
    // (cloacina's own run id isn't surfaced by `start_acquire` in v0).
    let runner = http.runner.clone();
    tokio::spawn(async move {
        if let Err(e) = start_acquire(&runner, seed).await {
            tracing::warn!(error = %e, "manual acquire run failed to start");
        }
    });
    true
}

/// Force a wedged edition back to `Missing` so it can be re-acquired
/// (SKADI-T-0112). Idempotent — resetting an already-terminal edition just
/// clears it to `Missing`. The operator's clean recovery path when an acquire
/// run is lost (no more manual DB surgery).
async fn reset_edition(
    State(http): State<MoviesHttp>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let movie_id: MovieId = parse_id(&id, "movie")?;
    let edition_id: MovieEditionId = parse_id(&eid, "edition")?;

    let edition = repo(&http).get_edition(edition_id).await?.ok_or_else(|| {
        ApiError(AppError::NotFound(format!(
            "edition {edition_id} not found"
        )))
    })?;
    if edition.movie_id != movie_id {
        return Err(ApiError(AppError::NotFound(format!(
            "edition {edition_id} does not belong to movie {movie_id}"
        ))));
    }

    let previous = format!("{:?}", edition.status);
    repo(&http)
        .set_edition_status(edition_id, AcquisitionStatus::Missing)
        .await?;
    tracing::info!(edition = %edition_id, %previous, "edition reset to Missing");

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "edition": eid,
            "status": "Missing",
            "previous": previous,
        })),
    )
        .into_response())
}

/// One candidate release for the interactive-search UI (SKADI-T-0114): the raw
/// [`Release`](skadi_indexers::Release) (echoed back to `grab`) plus derived
/// display fields and the profile verdict.
#[derive(Serialize)]
struct ReleaseCandidate {
    /// Echo this object back to `POST …/grab` to grab it.
    release: skadi_indexers::Release,
    /// Canonical blocklist identity, so the UI can block/unblock this release
    /// without recomputing it (SKADI-T-0115/T-0117).
    release_key: String,
    quality: String,
    age_days: i64,
    protocol: &'static str,
    accepted: bool,
    reason: String,
    /// How well the title matches what was asked for, 0.0-1.0 (SKADI-T-0181).
    ///
    /// The manual list shows *every* sibling by design, so for a series search the
    /// operator saw books 1-8 in arbitrary order with no hint which one matched.
    /// Candidates are sorted by this, so the likely one is at the top — the
    /// operator still picks, they just are not made to guess.
    relevance: f32,
}

/// Body of `POST /movies/quality/test` (SKADI-T-0184): a release title and an
/// optional size (bytes) for size-range rules.
#[derive(Deserialize)]
struct TestReleaseRequest {
    title: String,
    #[serde(default)]
    size: Option<u64>,
}

/// `POST /movies/quality/test` — explain how `title` would be judged against the
/// movies domain's **live** profile + custom-format registry, without a search or
/// grab (SKADI-T-0184). The Radarr "test a custom format" affordance, plus a
/// debugging window into the decision engine. Returns the full
/// [`ReleaseExplanation`](skadi_hunter::ReleaseExplanation).
async fn test_release(
    State(_http): State<MoviesHttp>,
    Json(req): Json<TestReleaseRequest>,
) -> Result<Response, ApiError> {
    if req.title.trim().is_empty() {
        return Err(ApiError(AppError::Validation(
            "title must not be empty".into(),
        )));
    }
    let svc = skadi_hunter::try_services_for(MediaKind::Movie).ok_or_else(|| {
        ApiError(AppError::Config(
            "providers not initialised yet; try again once the daemon has reconciled".into(),
        ))
    })?;
    // Test-a-title is about quality + custom-format scoring, not the blocklist or
    // seeders, so use an empty blocklist and leave seeders unset.
    let no_block = std::collections::HashSet::new();
    let scoring = skadi_hunter::Scoring {
        definitions: &svc.scoring.definitions,
        profile: &svc.scoring.profile,
        formats: &svc.scoring.formats,
        min_seeders: 0,
        // Per-indexer priority + seeder floor (SKADI-T-0539).
        indexer_flags: &skadi_hunter::indexer_flags(&svc.indexers),
        blocklisted: &no_block,
        audiobook: None,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let release = Release {
        indexer: IndexerId::new(),
        title: req.title.clone(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:test".into()),
        size: req.size.unwrap_or(0),
        published: Utc::now(),
        seeders: None,
        categories: Vec::new(),
        parsed: skadi_quality::parse(&req.title),
    };
    Ok(Json(skadi_hunter::explain(&release, &scoring)).into_response())
}

/// `GET /movies/{id}/editions/{eid}/releases` — run the live indexer search for
/// the edition and return each candidate scored against the active profile
/// (SKADI-T-0114). Read-only; does not snatch.
async fn list_releases(
    State(http): State<MoviesHttp>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let movie_id: MovieId = parse_id(&id, "movie")?;
    let edition_id: MovieEditionId = parse_id(&eid, "edition")?;
    let movie = repo(&http)
        .get_movie(movie_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {movie_id} not found"))))?;
    let edition = repo(&http).get_edition(edition_id).await?.ok_or_else(|| {
        ApiError(AppError::NotFound(format!(
            "edition {edition_id} not found"
        )))
    })?;
    if edition.movie_id != movie_id {
        return Err(ApiError(AppError::NotFound(format!(
            "edition {edition_id} does not belong to movie {movie_id}"
        ))));
    }

    let svc = skadi_hunter::try_services_for(skadi_core::MediaKind::Movie).ok_or_else(|| {
        ApiError(AppError::Config(
            "providers not initialised yet; try again once the daemon has reconciled".into(),
        ))
    })?;

    let seed = acquire_seed(&movie, &edition);
    let mut state = skadi_hunter::AcquireState::new(seed.acquirable, seed.request, seed.profile);
    {
        // The operator asked for this list by hand, so indexers with automatic
        // search off are still consulted (SKADI-T-0539).
        state.request.trigger = skadi_hunter::SearchTrigger::Interactive;
        skadi_hunter::search(&mut state, &svc.indexers)
    }
    .await
    .map_err(ApiError)?;

    // Reflect the blocklist in the verdicts (SKADI-T-0115): a blocklisted
    // candidate shows as rejected "blocklisted" rather than being hidden.
    let blocklisted = http.store.blocked_keys().await.unwrap_or_default();
    // If the edition is already imported, reflect upgrade semantics in the
    // verdicts (SKADI-T-0182/0186): a candidate no better than the held file (on
    // quality or format score) shows as "not an upgrade" rather than accepted.
    let (current_quality, current_format_score) = match &edition.status {
        skadi_core::AcquisitionStatus::Imported { quality, score, .. } => {
            (Some(*quality), Some(*score))
        }
        _ => (None, None),
    };
    // Mirror the sweep (SKADI-T-0584): if the held file does not play, the
    // preview must show what the sweep would really decide, not what it would
    // decide about a file that plays.
    let current_unplayable = edition
        .media_info
        .as_ref()
        .is_some_and(skadi_core::is_broken);
    // The item's own profile, not the daemon-wide active one (SKADI-T-0607):
    // the sweep judges each item on its profile (SKADI-T-0537) and this list
    // must agree with it — an HD-only show was showing SD rips as "accepted".
    let profile = skadi_hunter::services::resolve_profile_by_id(
        &http.store,
        movie.profile,
        &svc.scoring.definitions,
    )
    .await
    .unwrap_or_else(|| svc.scoring.profile.clone());
    let scoring = skadi_hunter::Scoring {
        definitions: &svc.scoring.definitions,
        profile: &profile,
        formats: &svc.scoring.formats,
        min_seeders: svc.scoring.min_seeders,
        // Per-indexer priority + seeder floor (SKADI-T-0539).
        indexer_flags: &skadi_hunter::indexer_flags(&svc.indexers),
        blocklisted: &blocklisted,
        audiobook: None,
        current_quality,
        current_format_score,
        current_unplayable,
    };
    let now = Utc::now();
    let wanted_titles = state.request.titles.clone();
    let mut candidates: Vec<ReleaseCandidate> = state
        .candidates
        .iter()
        .map(|r| {
            let (mut verdict, _) = skadi_hunter::evaluate(r, &scoring);
            // Identity gates (SKADI-T-0607): `evaluate` is the profile axis only;
            // a release for another film must not read as accepted here when
            // `decide` would never take it.
            if verdict.accepted
                && let Some(why) = skadi_hunter::identity_rejection(
                    skadi_core::MediaKind::Movie,
                    &wanted_titles,
                    state.request.year,
                    None,
                    r,
                )
            {
                verdict = skadi_hunter::Verdict {
                    accepted: false,
                    reason: why,
                };
            }
            let rel = skadi_quality::title_relevance(&wanted_titles, &r.title);
            ReleaseCandidate {
                // Coverage first, then precision: coverage answers "is this the
                // thing I asked for?", precision "how much else is in the name?".
                // A partial match on the right title beats a tight match on the
                // wrong one.
                relevance: rel.coverage * 0.75 + rel.precision * 0.25,
                quality: quality_name(r, &svc.scoring.definitions),
                age_days: (now - r.published).num_days(),
                protocol: match r.fetch {
                    skadi_indexers::ReleaseFetch::NzbUrl(_) => "usenet",
                    _ => "torrent",
                },
                accepted: verdict.accepted,
                reason: verdict.reason,
                release_key: skadi_indexers::release_key(r),
                release: r.clone(),
            }
        })
        .collect();
    // Most relevant first (SKADI-T-0181). Sorted, not filtered: the manual list
    // exists precisely so the operator can reach a candidate the gate would
    // reject, so hiding low-relevance ones would defeat it.
    candidates.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Stable tiebreak so the list does not reshuffle between refreshes.
            .then_with(|| a.release_key.cmp(&b.release_key))
    });

    Ok(Json(candidates).into_response())
}

/// Human quality name for a release, via the quality definitions.
fn quality_name(r: &skadi_indexers::Release, defs: &[skadi_quality::QualityDefinition]) -> String {
    skadi_quality::to_quality(&r.parsed, defs)
        .and_then(|q| defs.iter().find(|d| d.id == q.id).map(|d| d.name.clone()))
        .unwrap_or_else(|| "Unknown".into())
}

#[derive(Deserialize)]
struct GrabRequest {
    /// The chosen release, echoed from a `GET …/releases` candidate.
    release: skadi_indexers::Release,
}

/// `POST /movies/{id}/editions/{eid}/grab` — grab a specific release the
/// operator picked (SKADI-T-0114). Starts the acquire workflow at the snatch
/// step (search/decide are skipped). Needs the movies domain enabled and
/// honours the in-flight guard, like the auto-acquire trigger.
async fn grab_release(
    State(http): State<MoviesHttp>,
    Path((id, eid)): Path<(String, String)>,
    Json(req): Json<GrabRequest>,
) -> Result<Response, ApiError> {
    let movie_id: MovieId = parse_id(&id, "movie")?;
    let edition_id: MovieEditionId = parse_id(&eid, "edition")?;
    launch_grab(&http, movie_id, edition_id, req.release).await
}

/// Body of `POST …/grab-link` (SKADI-I-0043): an operator-pasted magnet or
/// `.torrent` URL for this edition, with an optional display title.
#[derive(Deserialize)]
struct GrabLinkRequest {
    link: String,
    #[serde(default)]
    title: Option<String>,
}

/// `POST /movies/{id}/editions/{eid}/grab-link` — manual acquisition: synthesize a
/// release from the pasted magnet or `.torrent` URL (movies title parser) and grab
/// it through the same snatch-step pipeline, so it downloads + imports tracked
/// against the edition.
async fn grab_link(
    State(http): State<MoviesHttp>,
    Path((id, eid)): Path<(String, String)>,
    Json(req): Json<GrabLinkRequest>,
) -> Result<Response, ApiError> {
    let movie_id: MovieId = parse_id(&id, "movie")?;
    let edition_id: MovieEditionId = parse_id(&eid, "edition")?;
    let release = skadi_indexers::release_from_link(
        &req.link,
        req.title.as_deref(),
        skadi_quality::parser::parse,
    )
    .ok_or_else(|| {
        ApiError(AppError::Validation(
            "not a valid magnet or .torrent URL (expected magnet:?xt=urn:btih:… or http(s)://…)"
                .into(),
        ))
    })?;
    launch_grab(&http, movie_id, edition_id, release).await
}

/// Shared by `grab_release` (operator-chosen candidate) and `grab_magnet` (pasted
/// magnet): validate the edition + domain, honour the in-flight guard, then start
/// the acquire workflow at the **snatch step** (search/decide skipped) with
/// `release`. SKADI-T-0114 / SKADI-I-0043.
async fn launch_grab(
    http: &MoviesHttp,
    movie_id: MovieId,
    edition_id: MovieEditionId,
    release: Release,
) -> Result<Response, ApiError> {
    let enabled = http
        .store
        .get(DOMAIN_NAME)
        .await?
        .map(|s| s.enabled)
        .unwrap_or(false);
    if !enabled {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "domain_disabled",
                "message": "the movies domain is disabled; enable it before grabbing"
            })),
        )
            .into_response());
    }

    let movie = repo(http)
        .get_movie(movie_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {movie_id} not found"))))?;
    let edition = repo(http).get_edition(edition_id).await?.ok_or_else(|| {
        ApiError(AppError::NotFound(format!(
            "edition {edition_id} not found"
        )))
    })?;
    if edition.movie_id != movie_id {
        return Err(ApiError(AppError::NotFound(format!(
            "edition {edition_id} does not belong to movie {movie_id}"
        ))));
    }
    // Same fresh in-flight guard as auto-acquire (SKADI-T-0112).
    let in_flight = matches!(
        edition.status,
        AcquisitionStatus::Searching { .. }
            | AcquisitionStatus::Snatched { .. }
            | AcquisitionStatus::Downloading { .. }
    );
    let fresh = (Utc::now() - edition.updated_at)
        < chrono::Duration::seconds(skadi_hunter::STALE_ACQUIRE_GRACE.as_secs() as i64);
    if in_flight && fresh {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this edition is already in progress"
            })),
        )
            .into_response());
    }

    let seed = acquire_seed(&movie, &edition);
    let runner = http.runner.clone();
    tokio::spawn(async move {
        if let Err(e) = skadi_hunter::start_grab(
            &runner,
            seed.acquirable,
            seed.request,
            seed.profile,
            release,
        )
        .await
        {
            tracing::warn!(error = %e, "manual grab run failed to start");
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "edition": edition_id.to_string(), "accepted": true })),
    )
        .into_response())
}

// --- library import (SKADI-T-0074) ---

#[derive(Deserialize)]
struct ScanRequest {
    /// Root folder to scan (as the daemon sees it, e.g. `/media/movies`).
    path: String,
    /// Include files already in the library (SKADI-T-0320). `false` by default:
    /// they clutter the review list and invite an accidental re-import of
    /// something already held.
    #[serde(default)]
    include_imported: bool,
}

#[derive(Serialize)]
struct ProposedMatch {
    tmdb_id: u64,
    title: String,
    year: Option<u16>,
    score: f32,
}

/// One scanned candidate, **parse-only** (no metadata lookup). Scanning a large
/// library must be instant, so metadata matching is deferred to
/// `/library-import/match`, which the UI calls lazily per page (SKADI-T-0078).
#[derive(Serialize)]
struct ScanCandidateDto {
    /// Existing video file path (recorded verbatim on import — in place). Also
    /// the stable key the UI uses to merge match results back in.
    path: String,
    display_name: String,
    parsed_title: Option<String>,
    parsed_year: Option<u16>,
    /// TMDB id parsed from the folder/file name (Radarr-style) or the NFO
    /// sidecar, when present.
    tmdb_id: Option<u64>,
    quality_id: Option<String>,
    quality_name: Option<String>,
    /// Display metadata from `movie.nfo` to confirm a match at a glance
    /// (SKADI-T-0328).
    nfo_overview: Option<String>,
    nfo_genres: Vec<String>,
    nfo_rating: Option<String>,
    nfo_studio: Option<String>,
}

/// `POST /library-import/scan` — walk a root folder and return parsed candidates
/// **without any metadata lookups**. Read-only and fast (filesystem + parse
/// only), so it scales to a library of thousands. Metadata matching is a
/// separate, lazy step — see [`library_import_match`].
async fn library_import_scan(
    State(http): State<MoviesHttp>,
    Json(req): Json<ScanRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let started = std::time::Instant::now();
    // The walk is blocking + can be slow on a high-latency mount; keep it off the
    // async worker. `scan_candidates` parallelizes the subdir walks internally.
    let scan_path = req.path.clone();
    let candidates = tokio::task::spawn_blocking(move || {
        import::scan_candidates(std::path::Path::new(&scan_path))
    })
    .await
    .map_err(|e| ApiError(AppError::Internal(format!("scan task panicked: {e}"))))?
    .map_err(ApiError)?;
    // Drop files already in the library (SKADI-T-0320). Matched by **inode**, not
    // path: import places library files as hardlinks to their source
    // (SKADI-T-0424), so the held copy and the one still sitting in the scan
    // directory are the same inode under two names — comparing paths would miss
    // every one of them.
    let total_scanned = candidates.len();
    let candidates = if req.include_imported {
        candidates
    } else {
        let held: Vec<std::path::PathBuf> = repo(&http)
            .list_movies(MovieFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?
            .iter()
            .flat_map(|m| m.editions.iter())
            .filter_map(|e| e.file.as_ref().map(|f| f.path.clone()))
            .collect();
        // Stat'ing the library and the candidates is blocking I/O over what may be
        // a slow mount.
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
            parsed_title: c.title,
            parsed_year: c.year,
            tmdb_id: c.tmdb_id,
            quality_id: c.quality_id.map(|q| q.to_string()),
            quality_name: c.quality_name,
            nfo_overview: c.nfo_overview,
            nfo_genres: c.nfo_genres,
            nfo_rating: c.nfo_rating,
            nfo_studio: c.nfo_studio,
        })
        .collect();
    tracing::info!(
        path = %req.path,
        total = dtos.len(),
        // Logged, not silent (SKADI-T-0320): an operator who scanned 400 files and
        // sees 12 needs to know the other 388 were filtered, not missed.
        already_imported = hidden,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "library-import scan complete (parse-only)"
    );
    Ok(Json(dtos))
}

/// How many metadata lookups [`library_import_match`] runs concurrently within a
/// page. Bounded so a page doesn't open a burst the provider would rate-limit.
const MATCH_CONCURRENCY: usize = 6;

/// One candidate to match: its stable `path` key plus the parsed title/year and,
/// when known, a TMDB id (parsed from the folder name or typed by the operator)
/// that short-circuits the fuzzy search with an exact lookup.
#[derive(Deserialize)]
struct MatchItem {
    path: String,
    title: Option<String>,
    year: Option<u16>,
    #[serde(default)]
    tmdb_id: Option<u64>,
}

#[derive(Deserialize)]
struct MatchRequest {
    items: Vec<MatchItem>,
}

/// The metadata match resolved for one candidate, keyed by `path` so the UI can
/// merge it back into the row it scanned.
#[derive(Serialize)]
struct MatchDto {
    path: String,
    proposed: Option<ProposedMatch>,
    /// `high` (parsed year agrees), `low` (mismatch/unsure), or `none`.
    confidence: &'static str,
    needs_review: bool,
    already_in_library: bool,
}

/// Resolve one candidate (best-effort — a provider error just yields no match)
/// and run the already-in-library check. When the item carries a TMDB id (parsed
/// from the folder name or typed by the operator) the id is authoritative: an
/// exact `lookup` replaces the fuzzy title search and the match is `high`.
async fn resolve_match(
    provider: &dyn MetadataProvider,
    store: &Store,
    item: MatchItem,
) -> MatchDto {
    // `true` when the match came from an exact TMDB id (authoritative).
    let mut by_id = false;
    let proposed = if let Some(id) = item.tmdb_id {
        by_id = true;
        provider
            .lookup(&ExternalId::Tmdb(TmdbId(id)))
            .await
            .ok()
            .map(|rec| ProposedMatch {
                tmdb_id: id,
                year: rec.release_date.and_then(|d| {
                    u16::try_from(d.format("%Y").to_string().parse::<i32>().unwrap_or(0)).ok()
                }),
                title: rec.title,
                score: 1.0,
            })
    } else {
        match &item.title {
            Some(title) => {
                let q = MetadataQuery {
                    title: title.clone(),
                    year: item.year,
                    kind: MediaKind::Movie,
                };
                provider.search(&q).await.ok().and_then(|matches| {
                    // Best score first, then PREFER a result whose year agrees
                    // with the parse — taking the provider's first hit matched
                    // "Die Hard 2 (1990)" to "Die Hard (1988)" and friends
                    // (SKADI-T-0328 movie-import feedback). No year-agreeing
                    // result → best score, which the caller grades "low".
                    let mut with_id: Vec<_> = matches
                        .into_iter()
                        .filter(|m| m.external_ids.tmdb.is_some())
                        .collect();
                    with_id.sort_by(|a, b| b.score.total_cmp(&a.score));
                    let pos = item
                        .year
                        .and_then(|y| with_id.iter().position(|m| m.year == Some(y)))
                        .unwrap_or(0);
                    (!with_id.is_empty()).then(|| {
                        let m = with_id.swap_remove(pos);
                        ProposedMatch {
                            tmdb_id: m.external_ids.tmdb.map(|t| t.0).unwrap_or_default(),
                            title: m.title,
                            year: m.year,
                            score: m.score,
                        }
                    })
                })
            }
            None => None,
        }
    };

    let already_in_library = match &proposed {
        Some(p) => store
            .get_movie_by_tmdb(TmdbId(p.tmdb_id))
            .await
            .ok()
            .flatten()
            .is_some(),
        None => false,
    };

    let (confidence, needs_review) = match &proposed {
        None => ("none", true),
        // An exact-id match is authoritative regardless of the parsed year.
        Some(_) if by_id => ("high", false),
        Some(p) => match (item.year, p.year) {
            (Some(a), Some(b)) if a == b => ("high", false),
            _ => ("low", true),
        },
    };

    MatchDto {
        path: item.path,
        proposed,
        confidence,
        needs_review,
        already_in_library,
    }
}

/// `POST /library-import/match` — resolve metadata matches for a batch of scanned
/// candidates (one page's worth). The UI calls this lazily as the operator pages
/// through scan results, so a big library never triggers thousands of lookups up
/// front. Bounded concurrency ([`MATCH_CONCURRENCY`]); result order matches the
/// request via the `path` key.
async fn library_import_match(
    State(http): State<MoviesHttp>,
    Json(req): Json<MatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let started = std::time::Instant::now();
    let n = req.items.len();
    let sem = Arc::new(tokio::sync::Semaphore::new(MATCH_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();

    for (idx, item) in req.items.into_iter().enumerate() {
        let provider = http.provider.clone();
        let store = http.store.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("match semaphore not closed");
            (idx, resolve_match(provider.as_ref(), &store, item).await)
        });
    }

    let mut out: Vec<(usize, MatchDto)> = Vec::with_capacity(n);
    while let Some(res) = set.join_next().await {
        if let Ok(pair) = res {
            out.push(pair);
        }
    }
    out.sort_by_key(|(i, _)| *i);
    let dtos: Vec<MatchDto> = out.into_iter().map(|(_, d)| d).collect();

    tracing::info!(
        items = n,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "library-import match batch complete"
    );
    Ok(Json(dtos))
}

#[derive(Deserialize)]
struct CommitItem {
    /// Existing file path from the scan (the import *source*; never modified).
    path: String,
    tmdb_id: u64,
    /// Detected quality id from the scan; falls back to the lowest when absent.
    quality_id: Option<String>,
}

#[derive(Deserialize)]
struct CommitRequest {
    /// Quality profile (the upgrade policy) to assign the imported movies.
    /// Optional (SKADI-T-0304): the import UX never picks one — an omitted profile
    /// binds the first registered profile (operators can re-grade later). The
    /// file's *current* quality is detected, never chosen.
    #[serde(default)]
    profile: Option<String>,
    items: Vec<CommitItem>,
}

#[derive(Serialize)]
struct CommitResult {
    imported: usize,
    skipped: usize,
    /// Of `imported`: hardlinked to their canonical path (source left as-is).
    linked: usize,
    /// Of `imported`: already at their canonical path — nothing to do.
    in_place: usize,
    /// Benign notes (e.g. an occupied canonical path was adopted and the source
    /// left as a duplicate) — not errors (SKADI-T-0328).
    unmatched: Vec<String>,
    errors: Vec<String>,
}

/// `POST /library-import/commit` — restructure the confirmed items into the
/// canonical layout under the root folder (hardlink; in-place no-op when
/// already canonical; refuses cross-device copies) and register them as
/// `Imported` movies. Source files are never modified or removed.
async fn library_import_commit(
    State(http): State<MoviesHttp>,
    Json(req): Json<CommitRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Resolve the upgrade profile (SKADI-T-0304): an explicit id must reference a
    // registered row; omitted binds the first registered profile. The import UX no
    // longer offers a picker — the file's current quality is detected per item.
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
    // Derived root (SKADI-T-0302): movies adopt into `<library.root>/movie`.
    let root = RootFolder::for_domain(library_root(&http.store).await, MediaKind::Movie);

    let n_items = req.items.len();
    tracing::info!(items = n_items, "library-import commit: starting");

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut linked = 0usize;
    let mut in_place = 0usize;
    let mut unmatched = Vec::new();
    let mut errors = Vec::new();
    for item in req.items {
        let quality = item
            .quality_id
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok())
            .map(QualityId::from);
        let result = import::commit_item(
            repo(&http),
            http.provider.as_ref(),
            PathBuf::from(&item.path),
            TmdbId(item.tmdb_id),
            profile,
            root.clone(),
            quality,
        )
        .await;
        match result {
            Ok(Some((_, placement))) => {
                imported += 1;
                match placement {
                    import::Placement::Linked => linked += 1,
                    import::Placement::InPlace => in_place += 1,
                    import::Placement::AdoptedDest => {
                        // Benign (SKADI-T-0328): the file already at the
                        // canonical path was registered; this source stays put.
                        in_place += 1;
                        unmatched.push(format!(
                            "{}: a file already at the canonical path was adopted — this \
                             source was left in place as a duplicate",
                            item.path
                        ));
                    }
                }
            }
            Ok(None) => skipped += 1,
            Err(e) => errors.push(format!("{}: {e}", item.path)),
        }
    }
    tracing::info!(
        items = n_items,
        imported,
        skipped,
        linked,
        in_place,
        unmatched = unmatched.len(),
        errors = errors.len(),
        "library-import commit: done"
    );
    Ok(Json(CommitResult {
        imported,
        skipped,
        linked,
        in_place,
        unmatched,
        errors,
    }))
}

// --- library provider (SKADI-T-0055) ---

/// The movies domain's contribution to the unified `/library` view.
#[derive(Clone)]
pub struct MoviesLibrary {
    store: Store,
    /// Needed only by `add_from_list` (SKADI-T-0511), so it is optional: every
    /// existing construction site builds this for the read-side methods and has
    /// no provider to hand. Absent means the add hook reports that it is not
    /// configured, rather than the domain silently not being addable.
    provider: Option<Arc<dyn MetadataProvider>>,
}

impl MoviesLibrary {
    /// Build over the daemon's store.
    pub fn new(store: Store) -> Self {
        Self {
            store,
            provider: None,
        }
    }

    /// Attach the metadata provider that `add_from_list` needs (SKADI-T-0511).
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn MetadataProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Edition-kind id → display name, loaded once per listing (SKADI-T-0454).
    /// Best-effort: an unreadable registry leaves the names absent rather than
    /// failing the whole library listing over a cosmetic field.
    async fn kind_names(&self) -> std::collections::HashMap<String, String> {
        (&self.store as &dyn MoviesRepo)
            .list_edition_kinds()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|k| (k.id.to_string(), k.name))
            .collect()
    }
}

/// Coarse status discriminant string for the library view.
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

/// Map a movie onto the unified-library DTO, resolving edition-kind ids to names
/// through `kinds` (SKADI-T-0454). An id the registry does not carry simply has
/// no name; clients fall back to the id.
fn movie_to_dto_named(
    movie: Movie,
    kinds: &std::collections::HashMap<String, String>,
) -> LibraryItemDto {
    let editions = movie
        .editions
        .iter()
        .map(|e| LibraryEditionDto {
            id: e.id.to_string(),
            kind: e.kind.to_string(),
            kind_name: kinds.get(&e.kind.to_string()).cloned(),
            // Editions are monitored with their movie; the item-level filter
            // in `/wanted` already applies.
            monitored: true,
            status_kind: status_kind_str(&e.status).to_string(),
            quality: e.quality.map(|q| q.to_string()),
            quality_name: e
                .quality
                .and_then(|q| skadi_api::quality_display_name(&q.to_string())),
            media_info: e.media_info.clone(),
        })
        .collect();
    LibraryItemDto {
        kind: "movie".into(),
        id: movie.id.to_string(),
        title: movie.title,
        year: movie.year,
        monitored: movie.monitored,
        editions,
    }
}

#[async_trait]
impl LibraryProvider for MoviesLibrary {
    fn domain(&self) -> &str {
        DOMAIN_NAME
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    /// Add a movie by TMDB id on behalf of an import list (SKADI-T-0511).
    ///
    /// Goes through `add_movie` — the same function `POST /movies` calls — so a
    /// list-added movie and a hand-added one are the same row built the same way.
    async fn add_from_list(
        &self,
        id_kind: &str,
        external_id: &str,
        opts: &skadi_api::library::ImportListAddOptions,
    ) -> skadi_core::Result<bool> {
        if !id_kind.eq_ignore_ascii_case("tmdb") {
            return Err(AppError::Validation(format!(
                "movies can be added by tmdb id, not `{id_kind}`"
            )));
        }
        let provider = self.provider.as_ref().ok_or_else(|| {
            AppError::Internal("movies library has no metadata provider configured".into())
        })?;
        let tmdb: u64 = external_id
            .parse()
            .map_err(|_| AppError::Validation(format!("`{external_id}` is not a TMDB id")))?;
        let tmdb = TmdbId(tmdb);

        // Already present is not an error: it is the ordinary case on every sync
        // after the first, and the engine counts it separately.
        if (&self.store as &dyn MoviesRepo)
            .get_movie_by_tmdb(tmdb.clone())
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
        // The root is derived, not chosen (SKADI-T-0302), exactly as the HTTP add
        // does; `opts.root_folder` is accepted for wire compatibility and ignored
        // for the same reason `AddMovieRequest.root_folder` is.
        let root_folder = RootFolder::for_domain(library_root(&self.store).await, MediaKind::Movie);

        let mut movie =
            add_movie(&self.store, provider.as_ref(), tmdb, profile, root_folder).await?;

        // `Movie::new` monitors by default, which is right for a deliberate add
        // and wrong for a list: a list that starts acquiring on its first sync is
        // how someone wakes up to a full disk (SKADI-T-0511).
        if !opts.monitored && movie.monitored {
            movie.monitored = false;
            (&self.store as &dyn MoviesRepo)
                .upsert_movie(&movie)
                .await?;
        }
        Ok(true)
    }

    async fn items(&self, monitored: Option<bool>) -> skadi_core::Result<Vec<LibraryItemDto>> {
        let movies = (&self.store as &dyn MoviesRepo)
            .list_movies(MovieFilter {
                monitored,
                limit: None,
                offset: None,
            })
            .await?;
        let kinds = self.kind_names().await;
        Ok(movies
            .into_iter()
            .map(|m| movie_to_dto_named(m, &kinds))
            .collect())
    }

    async fn genres(&self) -> skadi_core::Result<Vec<skadi_api::library::GenreCountDto>> {
        let movies = (&self.store as &dyn MoviesRepo)
            .list_movies(MovieFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(skadi_api::library::genre_counts(
            movies.iter().map(|m| m.genres.as_slice()),
        ))
    }

    /// The bound reaches diesel (SKADI-T-0494), so a page costs a page rather
    /// than a full load of every movie and its editions.
    async fn items_page(
        &self,
        monitored: Option<bool>,
        limit: Option<usize>,
        offset: usize,
    ) -> skadi_core::Result<Vec<LibraryItemDto>> {
        let movies = (&self.store as &dyn MoviesRepo)
            .list_movies(MovieFilter {
                monitored,
                limit: limit.map(|l| l as i64),
                offset: Some(offset as i64),
            })
            .await?;
        let kinds = self.kind_names().await;
        Ok(movies
            .into_iter()
            .map(|m| movie_to_dto_named(m, &kinds))
            .collect())
    }

    async fn count(&self, monitored: Option<bool>) -> skadi_core::Result<usize> {
        let n = (&self.store as &dyn MoviesRepo)
            .count_movies(MovieFilter {
                monitored,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(n.max(0) as usize)
    }

    async fn occupied_folders(&self) -> skadi_core::Result<Vec<std::path::PathBuf>> {
        let movies = (&self.store as &dyn MoviesRepo)
            .list_movies(MovieFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        // Each movie occupies `<root>/<MovieFolder>` — the immediate child of its root
        // (derived through the naming engine so it matches what the importer writes).
        Ok(movies
            .into_iter()
            .filter_map(|m| {
                let p = crate::naming::canonical_movie_path(
                    &m.root_folder.path,
                    &m.title,
                    m.year,
                    m.external_ids.tmdb.as_ref().map(|t| t.0),
                    m.external_ids.imdb.as_ref().map(|i| i.0.as_str()),
                    None,
                    std::path::Path::new("x.mkv"),
                );
                let rel = p.strip_prefix(&m.root_folder.path).ok()?;
                let first = rel.components().next()?;
                Some(m.root_folder.path.join(first))
            })
            .collect())
    }
}

// --- edition kinds registry ---

#[derive(Serialize)]
struct EditionKindDto {
    id: String,
    name: String,
    normalized_tag: String,
    match_patterns: Vec<String>,
    builtin: bool,
}

impl From<EditionKind> for EditionKindDto {
    fn from(k: EditionKind) -> Self {
        EditionKindDto {
            id: k.id.to_string(),
            name: k.name,
            normalized_tag: k.normalized_tag,
            match_patterns: k.match_patterns,
            builtin: k.builtin,
        }
    }
}

#[derive(Deserialize)]
struct EditionKindRequest {
    name: String,
    normalized_tag: String,
    #[serde(default)]
    match_patterns: Vec<String>,
}

async fn list_kinds(State(http): State<MoviesHttp>) -> Result<impl IntoResponse, ApiError> {
    let kinds = repo(&http).list_edition_kinds().await?;
    let dtos: Vec<EditionKindDto> = kinds.into_iter().map(EditionKindDto::from).collect();
    Ok(Json(dtos))
}

async fn create_kind(
    State(http): State<MoviesHttp>,
    Json(req): Json<EditionKindRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = EditionKind {
        id: EditionKindId::new(),
        name: req.name,
        normalized_tag: req.normalized_tag,
        match_patterns: req.match_patterns,
        builtin: false,
    };
    repo(&http).upsert_edition_kind(&kind).await?;
    Ok((StatusCode::CREATED, Json(EditionKindDto::from(kind))))
}

async fn get_kind(
    State(http): State<MoviesHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id: EditionKindId = parse_id(&id, "edition kind")?;
    let kind = repo(&http)
        .get_edition_kind(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("edition kind {id} not found"))))?;
    Ok(Json(EditionKindDto::from(kind)))
}

async fn update_kind(
    State(http): State<MoviesHttp>,
    Path(id): Path<String>,
    Json(req): Json<EditionKindRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id: EditionKindId = parse_id(&id, "edition kind")?;
    let existing = repo(&http)
        .get_edition_kind(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("edition kind {id} not found"))))?;
    // `builtin` is immutable through the API — preserve the stored flag.
    let kind = EditionKind {
        id,
        name: req.name,
        normalized_tag: req.normalized_tag,
        match_patterns: req.match_patterns,
        builtin: existing.builtin,
    };
    repo(&http).upsert_edition_kind(&kind).await?;
    Ok(Json(EditionKindDto::from(kind)))
}

async fn delete_kind(
    State(http): State<MoviesHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id: EditionKindId = parse_id(&id, "edition kind")?;
    if repo(&http).get_edition_kind(id).await?.is_none() {
        return Err(ApiError(AppError::NotFound(format!(
            "edition kind {id} not found"
        ))));
    }
    // `delete_edition_kind` refuses built-in rows with `AppError::Validation`,
    // which the API mapper renders as 400. (The AC asked for 422; the workspace
    // maps Validation→400 uniformly — documented deviation, consistent with the
    // settings CRUD task.)
    repo(&http).delete_edition_kind(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /movies/{id}/editions/{eid}/video` — the imported file's bytes, with
/// single-range support so a player can seek (SKADI-T-0574).
///
/// Direct play: skadi has no encoder, so the client decodes what is on disk.
/// Measured over this library that is ~98 % viable — HEVC/H.264 video with AAC,
/// AC3 or E-AC3 audio, all of which ExoPlayer handles. The residue is DTS.
///
/// Only an `Imported` edition has bytes; anything else is a 404 that says so
/// rather than an empty 200, so a player shows an error instead of a stall.
async fn edition_video(
    State(http): State<MoviesHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let path = edition_video_path(&http, member, &id, &eid).await?;
    skadi_api::ranged::serve_file_range(&path, &headers, "video").await
}

/// The imported file of edition `eid` of movie `id`, after the same checks the
/// video route makes: the member may see the movie, the edition is the
/// movie's, and it is imported.
async fn edition_video_path(
    http: &MoviesHttp,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    id: &str,
    eid: &str,
) -> Result<PathBuf, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    let movie_id: MovieId = parse_id(id, "movie")?;
    let edition_id: MovieEditionId = parse_id(eid, "edition")?;
    let movie = repo(http)
        .get_movie(movie_id)
        .await?
        .filter(|m| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Movie,
                &m.id.to_string(),
                m.content_rating.as_deref(),
                &m.genres,
            )
        })
        .ok_or_else(|| ApiError(AppError::NotFound(format!("movie {movie_id} not found"))))?;
    // Check ownership rather than looking the edition up globally: edition ids
    // are opaque, so without this `/movies/{a}/editions/{b}/video` would serve a
    // different movie's file.
    let edition = movie
        .editions
        .iter()
        .find(|e| e.id == edition_id)
        .ok_or_else(|| {
            ApiError(AppError::NotFound(format!(
                "edition {edition_id} is not an edition of movie {movie_id}"
            )))
        })?;
    match &edition.status {
        AcquisitionStatus::Imported { file, .. } => Ok(file.path.clone()),
        _ => Err(ApiError(AppError::NotFound(
            "edition is not imported — no video to play yet".into(),
        ))),
    }
}

/// `GET /movies/{id}/editions/{eid}/subtitles` — the subtitle files beside the
/// edition's video (SKADI-T-0663). Embedded tracks are the player's to list.
async fn edition_subtitles(
    State(http): State<MoviesHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid)): Path<(String, String)>,
) -> Result<Json<Vec<skadi_importer::subtitles::SubtitleInfo>>, ApiError> {
    let path = edition_video_path(&http, member, &id, &eid).await?;
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

/// `GET /movies/{id}/editions/{eid}/subtitles/{n}` — subtitle `n` of the
/// listing, as UTF-8 text; `?format=vtt` converts it (SKADI-T-0663).
async fn edition_subtitle(
    State(http): State<MoviesHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, eid, n)): Path<(String, String, usize)>,
    Query(q): Query<SubtitleQuery>,
) -> Result<Response, ApiError> {
    let path = edition_video_path(&http, member, &id, &eid).await?;
    subtitle_response(path, n, q.format.as_deref() == Some("vtt")).await
}

async fn subtitle_response(path: PathBuf, n: usize, webvtt: bool) -> Result<Response, ApiError> {
    let found =
        tokio::task::spawn_blocking(move || skadi_importer::subtitles::body(&path, n, webvtt))
            .await
            .map_err(|e| ApiError(AppError::Internal(format!("subtitle read panicked: {e}"))))?;
    let (ct, text) =
        found.ok_or_else(|| ApiError(AppError::NotFound(format!("no subtitle {n}"))))?;
    Ok(([(axum::http::header::CONTENT_TYPE, ct)], text).into_response())
}
