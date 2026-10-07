//! The audiobooks domain's HTTP surface (SKADI-T-0131).
//!
//! Implements [`HttpModule`](skadi_api::HttpModule) so the daemon can merge the
//! audiobooks routes into `/api/v1` without `skadi-api` depending on this crate.
//! Covers author management (`/authors`), book library management (`/books`),
//! the metadata lookups the "add" UI uses (`/authors/lookup`, `/books/lookup`),
//! and the per-book-file manual acquire / interactive search / grab / reset
//! endpoints — a deliberate parallel to `skadi_movies::http::MoviesHttp`.
//!
//! All handlers are stated on [`AudiobooksHttp`], a cheap-to-clone bundle of the
//! store, the Audnexus provider, and the module's Cloacina runner. The daemon
//! constructs one via [`AudiobooksHttp::new`] and hands `routes()` to the API.
//!
//! ## Provider type (test-mock note)
//!
//! The provider is stored as a **concrete** `Arc<AudnexusProvider>` rather than
//! `Arc<dyn MetadataProvider>`. The book routes only need the trait surface
//! (`add_book`/`refresh_book` coerce `&AudnexusProvider` to `&dyn`), but the
//! author routes (`POST /authors`, `GET /authors/lookup`) call the inherent
//! `AudnexusProvider::lookup_author`/`search_authors`, which are *not* on the
//! generic trait. Keeping the concrete type lets both work. For tests, an
//! `AudnexusProvider` is cheap to construct against a fake HTTP server (via
//! [`AudnexusProvider::with_base_url`]), so the book-add path is exercised with a
//! real provider pointed at a scripted wiremock endpoint — no boxed-trait
//! test-only constructor needed.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use skadi_api::bulk::{BulkAction, BulkOutcome, BulkReport, BulkRequest};
use skadi_api::{ApiError, HttpModule};
use skadi_core::{
    AcquisitionStatus, AppError, AsinId, AuthorId, BookFileId, BookId, MediaKind, ProfileId,
    QualityId, RootFolder,
};
use skadi_hunter::{AcquireSeed, start_acquire};
use skadi_metadata::{AudibleCatalogProvider, AudnexusProvider, ExternalId, MetadataProvider};
use skadi_store::{BlocklistRepo, ConfigRepo, DomainStateRepo, SettingsRepo, Store};

use crate::book::Book;
use crate::book_file::BookFile;
use crate::discovery::{AuthorDiscovery, ingest_author_works};
use crate::import;
use crate::metadata::{add_author, add_book};
use crate::repo::{AudiobooksRepo, AuthorFilter, BookFilter, WatchersRepo, WorksRepo};
use crate::work::WatchScope;

/// The audiobooks domain name in the `domains` table (must match
/// [`AudiobooksModule::name`](crate::module::AudiobooksModule::name)).
const DOMAIN_NAME: &str = "audiobooks";

/// Shared state for the audiobooks HTTP handlers.
#[derive(Clone)]
pub struct AudiobooksHttp {
    store: Store,
    provider: Arc<AudnexusProvider>,
    /// Audible catalog provider for "search by title" (SKADI-T-0151). `None` when
    /// not wired (e.g. the mock-provider e2e harness) — search then 503s.
    catalog: Option<Arc<AudibleCatalogProvider>>,
    runner: Arc<cloacina::runner::DefaultRunner>,
}

impl AudiobooksHttp {
    /// Bundle the pieces the audiobooks routes need. The daemon passes the store,
    /// the keyless Audnexus provider, the Audible catalog provider (for title
    /// search), and the module's runner
    /// ([`AudiobooksModule::runner`](crate::module::AudiobooksModule::runner)).
    pub fn new(
        store: Store,
        provider: Arc<AudnexusProvider>,
        catalog: Option<Arc<AudibleCatalogProvider>>,
        runner: Arc<cloacina::runner::DefaultRunner>,
    ) -> Self {
        Self {
            store,
            provider,
            catalog,
            runner,
        }
    }
}

/// The audiobook response schemas, free of any instance state — it only names
/// types — so they can be checked without standing up a store or a runner.
#[must_use]
pub fn audiobook_response_schemas() -> skadi_api::openapi::Schemas {
    let mut out = skadi_api::openapi::schema_for::<ChapterDto>();
    out.extend(skadi_api::openapi::schema_for::<WatcherDto>());
    out.extend(skadi_api::openapi::schema_for::<SeriesRollupDto>());
    // The domain model itself (SKADI-T-0548). `/books` serialises `Book`
    // directly, so this is the schema for the type the handler actually returns
    // — not a hand-written description of it that could disagree.
    out.extend(skadi_api::openapi::schema_for::<crate::book::Book>());
    out
}

impl HttpModule for AudiobooksHttp {
    /// The response payloads the Android client would otherwise hand-declare
    /// (SKADI-T-0546). Derived from the Rust types, so a field rename here
    /// changes the published document.
    fn schemas(&self) -> skadi_api::openapi::Schemas {
        audiobook_response_schemas()
    }

    fn routes(&self) -> Router {
        Router::new()
            // Static `/authors/lookup` registered before `/authors/{id}` — the
            // author search the "add author" UI uses.
            .route("/authors/lookup", get(lookup_authors))
            .route("/authors", get(list_authors).post(create_author))
            .route(
                "/authors/{id}",
                get(get_author).patch(patch_author).delete(delete_author),
            )
            // Static `/books/lookup` registered before `/books/{id}` — the
            // metadata preview the "add book" UI uses.
            .route("/books/lookup", get(lookup_book))
            // Free-text "search by title" over the Audible catalog (SKADI-T-0151),
            // mirroring movies' add-by-search.
            .route("/books/search", get(search_books))
            // One request for a selection on the wall (SKADI-T-0696).
            .route("/books/bulk", post(bulk_books))
            .route("/books", get(list_books).post(create_book))
            .route(
                "/books/{id}",
                get(get_book).patch(patch_book).delete(delete_book),
            )
            // Per-item and bulk metadata refresh (SKADI-T-0450). Both drive the
            // same `refresh_book_metadata` workflow the scheduled worker uses, so
            // an operator-triggered refresh and a stale-sweep refresh cannot drift
            // apart.
            .route("/books/{id}/refresh", post(refresh_book_route))
            .route("/books/refresh", post(refresh_all_books))
            .route("/books/merge-editions", post(merge_editions))
            .route("/books/{id}/files/{fid}/acquire", post(acquire_file))
            // Where an imported file lives on disk + how big it is, so the detail
            // page can show "the book is at <folder> · N files · size" (T-0147).
            .route("/books/{id}/files/{fid}/location", get(book_file_location))
            // Offline player (SKADI-I-0048): download the audio bytes (Range
            // support = resumable transfers, NOT streaming) + the chapter marks
            // the client stores alongside the download.
            .route("/books/{id}/files/{fid}/audio", get(book_file_audio))
            .route("/books/{id}/files/{fid}/chapters", get(book_file_chapters))
            // Interactive search + manual grab: list scored candidate releases
            // for a book file, and grab a chosen one.
            .route("/books/{id}/files/{fid}/releases", get(list_releases))
            .route("/books/{id}/files/{fid}/grab", post(grab_release))
            // Manual acquisition (SKADI-I-0043): paste a magnet or .torrent URL.
            .route("/books/{id}/files/{fid}/grab-link", post(grab_link))
            // Test-a-title / explain-decision (SKADI-T-0184): explain how a
            // release title would be judged by the live audiobook profile + axis.
            .route("/audiobooks/quality/test", post(test_release))
            // Recover a wedged file: force its status back to `Missing` so it can
            // be re-acquired (operator escape hatch when an acquire run was lost).
            .route("/books/{id}/files/{fid}/reset", post(reset_file))
            // Library import (SKADI-T-0134): scan an on-disk audiobook tree +
            // commit matched items by restructuring (hardlink) into the root
            // folder. NAMESPACED under `/audiobooks/` because movies owns the
            // bare `/library-import/*` and both routers merge under `/api/v1`.
            .route(
                "/audiobooks/library-import/scan",
                post(library_import_scan),
            )
            .route(
                "/audiobooks/library-import/match",
                post(library_import_match),
            )
            .route(
                "/audiobooks/library-import/commit",
                post(library_import_commit),
            )
            // The built-in, read-only audiobook quality ladder the config UI shows
            // (SKADI-T-0142). Audiobooks rank via this fixed M4B-first ladder, not
            // a configurable movie profile.
            .route("/audiobooks/quality", get(audiobook_quality))
            // Bodies of work + watchers (SKADI-I-0018): series completeness rollups
            // from the known-works catalog, and watcher set/clear/list.
            .route("/audiobooks/series", get(list_series))
            // Browse a body of work: every known work for an author/series (owned +
            // unowned), each flagged owned/watched (SKADI-T-0159).
            .route("/audiobooks/works", get(list_works))
            // Library-driven Discover (SKADI-T-0317): unowned known works, ranked.
            .route("/audiobooks/discover", get(discover))
            // Run a catalog ingest pass on demand (SKADI-T-0160): refreshes the
            // known-works store for every library author so imported books get
            // series/body-of-work data without waiting for the periodic sweep.
            .route("/audiobooks/catalog/refresh", post(refresh_catalog))
            .route("/watchers", get(list_watchers))
            .route(
                "/watchers/{scope}/{key}",
                axum::routing::put(set_watcher).delete(clear_watcher),
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

fn repo(http: &AudiobooksHttp) -> &dyn AudiobooksRepo {
    &http.store
}

/// The single `library.root` from the config plane (SKADI-T-0302), or the
/// registry default when unset. Audiobooks live under `<library.root>/audiobook`.
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

// --- authors ---

#[derive(Deserialize)]
struct ListParams {
    monitored: Option<bool>,
}

async fn list_authors(
    State(http): State<AudiobooksHttp>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let authors = repo(&http)
        .list_authors(AuthorFilter {
            monitored: params.monitored,
        })
        .await?;
    Ok(Json(authors))
}

async fn get_author(
    State(http): State<AudiobooksHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id: AuthorId = parse_id(&id, "author")?;
    let author = repo(&http)
        .get_author(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("author {id} not found"))))?;
    Ok(Json(author))
}

#[derive(Deserialize)]
struct PatchAuthor {
    monitored: Option<bool>,
}

/// `PATCH /authors/{id}` — toggle an author's `monitored` flag. A monitored
/// author is swept by author-discovery (SKADI-T-0132); unmonitoring stops
/// auto-adding their new releases (existing books are untouched).
async fn patch_author(
    State(http): State<AudiobooksHttp>,
    Path(id): Path<String>,
    Json(patch): Json<PatchAuthor>,
) -> Result<impl IntoResponse, ApiError> {
    let id: AuthorId = parse_id(&id, "author")?;
    let mut author = repo(&http)
        .get_author(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("author {id} not found"))))?;
    if let Some(m) = patch.monitored {
        author.monitored = m;
    }
    repo(&http).upsert_author(&author).await?;
    Ok(Json(author))
}

#[derive(Deserialize)]
struct AddAuthorRequest {
    asin: String,
}

/// `POST /authors` — add (or return the existing) monitored author by Audnexus
/// author ASIN. Idempotent on the ASIN (see [`add_author`]).
async fn create_author(
    State(http): State<AudiobooksHttp>,
    Json(req): Json<AddAuthorRequest>,
) -> Result<Response, ApiError> {
    let author = add_author(repo(&http), http.provider.as_ref(), AsinId(req.asin)).await?;
    // Know-only (SKADI-I-0018): ingest the author's catalog into known-works in the
    // background so their body of work is browsable; acquires nothing (no watcher).
    if let Some(catalog) = http.catalog.clone() {
        let store = http.store.clone();
        let name = author.name.clone();
        let asin = author.asin.clone();
        tokio::spawn(async move {
            let _ = ingest_author_works(&store, catalog.as_ref(), &name, asin.as_ref()).await;
        });
    }
    Ok((StatusCode::CREATED, Json(author)).into_response())
}

/// `GET /audiobooks/series` — series completeness rollups (SKADI-I-0018): per
/// series, how many members there are and how many the library holds, keyed by
/// series name (the join the library UI uses). Watched flag included.
///
/// Members are known works **and** library books, merged (SKADI-T-0651). See
/// [`rollup_series`].
async fn list_series(State(http): State<AudiobooksHttp>) -> Result<impl IntoResponse, ApiError> {
    let works: Vec<_> = http
        .store
        .list_all_works()
        .await
        .map_err(ApiError)?
        .into_iter()
        .filter(|w| shown_language(w.language.as_deref()))
        .collect();
    let books = http
        .store
        .list_books(BookFilter::default())
        .await
        .map_err(ApiError)?;
    let watchers = http.store.list_watchers().await.unwrap_or_default();
    let watched_series: std::collections::HashSet<String> = watchers
        .iter()
        .filter(|w| w.scope == WatchScope::Series)
        .map(|w| w.key.clone())
        .collect();
    Ok(Json(rollup_series(&works, &books, &watched_series)))
}

/// Whether a series position is a real one (SKADI-T-0654): non-blank after
/// trimming. `"0"` counts — prequels exist (*Ball Lightning* is position 0).
fn has_position(position: Option<&str>) -> bool {
    position.is_some_and(|p| !p.trim().is_empty())
}

/// Build the series rollups from known works and library books together
/// (SKADI-T-0651).
///
/// This used to count the known-works store alone, so a book that was in the
/// library and in a series but that discovery had never found did not count at
/// all. *A Game of Thrones* — owned, position 1 — was invisible to *A Song of Ice
/// and Fire*, whose rollup read `total 2, owned 1` from two anthologies.
///
/// - **Members** = works carrying the series ∪ library books whose
///   `series.name` is the series. Deduplicated by **book ASIN**; a library book
///   with no ASIN is its own member.
/// - **Owned** = a library row exists for the member. That is the rule the web's
///   tiles already use (`merge_owned_missing` shows every library row as owned),
///   so the count and the tiles agree. It is not "on disk": a book a watcher has
///   queued but not yet downloaded is a library row too.
/// - **Join.** Library books carry a series *name* but no series ASIN, so books
///   join rollups by series name, compared case- and whitespace-insensitively.
///   When books gain a series ASIN, join on that first.
/// - **Only positioned members count** (SKADI-T-0654). A member with no series
///   position — *Dangerous Women* and *The Book of Swords* in *A Song of Ice and
///   Fire* — is counted in `related` instead. Audible tags those anthologies into
///   the series because each contains a series novella; counting them is what let
///   two anthologies stand in for a five-novel series. A member is positioned if
///   either its work or its library book carries a position; `"0"` (a prequel)
///   and `"1.5"` are positions, `null` and blank are not.
fn rollup_series(
    works: &[crate::work::Work],
    books: &[Book],
    watched_series: &std::collections::HashSet<String>,
) -> Vec<SeriesRollupDto> {
    use std::collections::{HashMap, HashSet};

    fn key(name: &str) -> String {
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    struct Acc {
        name: String,
        series_asin: Option<String>,
        members: HashSet<String>,
        owned: HashSet<String>,
        positioned: HashSet<String>,
        watched: bool,
    }
    let mut by_series: HashMap<String, Acc> = HashMap::new();
    let acc = |by: &mut HashMap<String, Acc>, name: &str| -> String {
        let k = key(name);
        by.entry(k.clone()).or_insert_with(|| Acc {
            name: name.trim().to_string(),
            series_asin: None,
            members: HashSet::new(),
            owned: HashSet::new(),
            positioned: HashSet::new(),
            watched: false,
        });
        k
    };

    // ASIN -> the library holds it.
    let held: HashSet<&str> = books
        .iter()
        .filter_map(|b| b.external_ids.asin.as_ref().map(|a| a.0.as_str()))
        .collect();

    for w in works {
        let Some(name) = w.series_name.as_deref() else {
            continue;
        };
        let k = acc(&mut by_series, name);
        let e = by_series.get_mut(&k).expect("just inserted");
        if e.series_asin.is_none() {
            e.series_asin = w.series_asin.as_ref().map(|a| a.0.clone());
        }
        e.members.insert(w.asin.0.clone());
        if has_position(w.series_position.as_deref()) {
            e.positioned.insert(w.asin.0.clone());
        }
        if held.contains(w.asin.0.as_str()) {
            e.owned.insert(w.asin.0.clone());
        }
        if let Some(sa) = w.series_asin.as_ref()
            && watched_series.contains(&sa.0)
        {
            e.watched = true;
        }
    }

    for b in books {
        let Some(series) = b.series.as_ref() else {
            continue;
        };
        if series.name.trim().is_empty() {
            continue;
        }
        let k = acc(&mut by_series, &series.name);
        let e = by_series.get_mut(&k).expect("just inserted");
        // Same ASIN as a work → the same member, counted once.
        let member = b
            .external_ids
            .asin
            .as_ref()
            .map_or_else(|| format!("book:{}", b.id), |a| a.0.clone());
        e.members.insert(member.clone());
        if has_position(series.position.as_deref()) {
            e.positioned.insert(member.clone());
        }
        e.owned.insert(member);
    }

    let mut out: Vec<SeriesRollupDto> = by_series
        .into_values()
        .map(|a| SeriesRollupDto {
            name: a.name,
            series_asin: a.series_asin,
            total: a.positioned.len(),
            owned: a.owned.intersection(&a.positioned).count(),
            related: a.members.len() - a.positioned.len(),
            watched: a.watched,
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// `GET /watchers` — all active watchers.
async fn list_watchers(State(http): State<AudiobooksHttp>) -> Result<impl IntoResponse, ApiError> {
    let watchers = http.store.list_watchers().await.map_err(ApiError)?;
    let dtos: Vec<WatcherDto> = watchers
        .into_iter()
        .map(|w| WatcherDto {
            scope: w.scope.as_str().to_string(),
            key: w.key,
        })
        .collect();
    Ok(Json(dtos))
}

/// `PUT /watchers/{scope}/{key}` — start watching an author/series/book for
/// acquisition. The next sweep materialises its unowned works as Missing books.
async fn set_watcher(
    State(http): State<AudiobooksHttp>,
    Path((scope, key)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = WatchScope::parse(&scope).ok_or_else(|| {
        ApiError(AppError::Validation(format!(
            "unknown watch scope: {scope}"
        )))
    })?;
    http.store
        .set_watcher(scope, &key)
        .await
        .map_err(ApiError)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /watchers/{scope}/{key}` — stop watching (does not delete owned books).
async fn clear_watcher(
    State(http): State<AudiobooksHttp>,
    Path((scope, key)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = WatchScope::parse(&scope).ok_or_else(|| {
        ApiError(AppError::Validation(format!(
            "unknown watch scope: {scope}"
        )))
    })?;
    http.store
        .clear_watcher(scope, &key)
        .await
        .map_err(ApiError)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, schemars::JsonSchema)]
struct SeriesRollupDto {
    name: String,
    series_asin: Option<String>,
    /// Positioned members only (SKADI-T-0654).
    total: usize,
    /// Positioned members the library holds.
    owned: usize,
    /// Members with no series position — usually anthologies that contain a
    /// series story. Listed by clients under "Related"; never part of `total`
    /// or `owned`. Additive, so older clients ignore it.
    related: usize,
    watched: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
struct WatcherDto {
    scope: String,
    key: String,
}

#[derive(Deserialize)]
struct WorksParams {
    author: Option<String>,
    series: Option<String>,
}

/// One known work in a body-of-work browse (SKADI-T-0159): the catalog record
/// plus whether it's owned (and which library book) and whether a watcher covers
/// it (author / series / book scope).
#[derive(Serialize)]
struct WorkDto {
    asin: String,
    title: String,
    authors: Vec<String>,
    series_name: Option<String>,
    series_position: Option<String>,
    cover_url: Option<String>,
    release_date: Option<String>,
    owned: bool,
    book_id: Option<String>,
    watched: bool,
}

/// `GET /audiobooks/works?author=<asin>` / `?series=<asin>` — every known work in
/// a body of work (owned + unowned), ordered by series position then title, each
/// flagged owned/watched. Unowned entries are the "missing" bibliography the UI
/// offers watch/add on (SKADI-T-0159). Exactly one of `author`/`series` required.
async fn list_works(
    State(http): State<AudiobooksHttp>,
    Query(params): Query<WorksParams>,
) -> Result<impl IntoResponse, ApiError> {
    let works = if let Some(a) = params.author.as_deref().filter(|s| !s.is_empty()) {
        http.store
            .list_works_by_author(&AsinId(a.to_string()))
            .await
    } else if let Some(s) = params.series.as_deref().filter(|s| !s.is_empty()) {
        http.store
            .list_works_by_series(&AsinId(s.to_string()))
            .await
    } else {
        return Err(ApiError(AppError::Validation(
            "works requires an `author` or `series` query param".into(),
        )));
    }
    .map_err(ApiError)?;
    // English-only catalog (SKADI-T-0160): hide classified non-English editions.
    let works: Vec<_> = works
        .into_iter()
        .filter(|w| shown_language(w.language.as_deref()))
        .collect();

    // Ownership: ASIN -> library book id.
    let books = http
        .store
        .list_books(BookFilter::default())
        .await
        .map_err(ApiError)?;
    let owned: std::collections::HashMap<String, String> = books
        .iter()
        .filter_map(|b| {
            b.external_ids
                .asin
                .as_ref()
                .map(|a| (a.0.clone(), b.id.to_string()))
        })
        .collect();

    // Watch coverage by scope.
    let watchers = http.store.list_watchers().await.unwrap_or_default();
    let mut w_authors = std::collections::HashSet::new();
    let mut w_series = std::collections::HashSet::new();
    let mut w_books = std::collections::HashSet::new();
    for w in watchers {
        match w.scope {
            WatchScope::Author => w_authors.insert(w.key),
            WatchScope::Series => w_series.insert(w.key),
            WatchScope::Book => w_books.insert(w.key),
        };
    }

    let mut out: Vec<WorkDto> = works
        .into_iter()
        .map(|w| {
            let book_id = owned.get(&w.asin.0).cloned();
            let watched = w_books.contains(&w.asin.0)
                || w.series_asin
                    .as_ref()
                    .is_some_and(|s| w_series.contains(&s.0))
                || w.author_asin
                    .as_ref()
                    .is_some_and(|a| w_authors.contains(&a.0));
            WorkDto {
                owned: book_id.is_some(),
                book_id,
                watched,
                asin: w.asin.0,
                title: w.title,
                authors: w.authors,
                series_name: w.series_name,
                series_position: w.series_position,
                cover_url: w.cover_url,
                release_date: w.release_date,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        series_pos_key(a.series_position.as_deref())
            .partial_cmp(&series_pos_key(b.series_position.as_deref()))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    Ok(Json(out))
}

/// Leading numeric series position (e.g. `"2.5"` -> 2.5) for ordering; unknown /
/// non-numeric positions sort last.
fn series_pos_key(pos: Option<&str>) -> f64 {
    pos.map(|s| s.trim())
        .and_then(|s| {
            let num: String = s
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            num.parse::<f64>().ok()
        })
        .unwrap_or(f64::MAX)
}

#[derive(serde::Deserialize)]
struct DiscoverParams {
    limit: Option<usize>,
}

/// `GET /audiobooks/discover?limit=N` (SKADI-T-0317) — library-driven recommendations: catalog
/// works by your authors/series that aren't in your library yet, ranked upcoming →
/// recently-released → undated. The "what to listen to next" feed, built from the known-works
/// graph (no external popular feed needed). Reuses [`WorkDto`] (all `owned: false`); the UI bands
/// them into "Coming soon" vs "Available now" from `release_date`.
async fn discover(
    State(http): State<AudiobooksHttp>,
    Query(params): Query<DiscoverParams>,
) -> Result<impl IntoResponse, ApiError> {
    let works = http.store.list_all_works().await.map_err(ApiError)?;
    // English-only catalog (SKADI-T-0160), consistent with the works browse.
    let works: Vec<_> = works
        .into_iter()
        .filter(|w| shown_language(w.language.as_deref()))
        .collect();
    // Owned = any library book carrying the ASIN (incl. Missing/queued), so Discover surfaces
    // only works not yet in the library at all.
    let books = http
        .store
        .list_books(BookFilter::default())
        .await
        .map_err(ApiError)?;
    let owned: std::collections::HashSet<String> = books
        .iter()
        .filter_map(|b| b.external_ids.asin.as_ref().map(|a| a.0.clone()))
        .collect();
    let today = chrono::Utc::now().date_naive();
    let ranked = crate::discovery::rank_discoveries(works, &owned, today);
    let limit = params.limit.unwrap_or(120).min(500);
    let out: Vec<WorkDto> = ranked
        .into_iter()
        .take(limit)
        .map(|w| WorkDto {
            owned: false,
            book_id: None,
            watched: false,
            asin: w.asin.0,
            title: w.title,
            authors: w.authors,
            series_name: w.series_name,
            series_position: w.series_position,
            cover_url: w.cover_url,
            release_date: w.release_date,
        })
        .collect();
    Ok(Json(out))
}

/// Whether a work is shown in the (English-only) catalog views (SKADI-T-0160):
/// keep English, and keep **unclassified** (`None`) so a transient language-lookup
/// failure never hides a real title — only a positively-non-English work is hidden.
fn shown_language(lang: Option<&str>) -> bool {
    crate::work::is_catalog_language(lang)
}

/// `POST /audiobooks/catalog/refresh` — kick off a catalog ingest pass in the
/// background (SKADI-T-0160): refresh the known-works store for every library
/// author so imported books get series/body-of-work data on demand, then resolve
/// watchers. `202 Accepted`; `503` when the Audible catalog provider isn't wired.
async fn refresh_catalog(
    State(http): State<AudiobooksHttp>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(catalog) = http.catalog.clone() else {
        return Err(ApiError(AppError::Config(
            "audible catalog provider not configured".into(),
        )));
    };
    let repo: Arc<dyn AudiobooksRepo> = Arc::new(http.store.clone());
    let enrich: Arc<dyn MetadataProvider> = http.provider.clone();
    let discovery = AuthorDiscovery::new(repo, http.store.clone(), catalog, enrich);
    tokio::spawn(async move {
        let (ingested, acquired) = discovery.run_full().await;
        tracing::info!(ingested, acquired, "catalog refresh: pass complete");
    });
    Ok(StatusCode::ACCEPTED)
}

async fn delete_author(
    State(http): State<AudiobooksHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id: AuthorId = parse_id(&id, "author")?;
    if repo(&http).get_author(id).await?.is_none() {
        return Err(ApiError(AppError::NotFound(format!(
            "author {id} not found"
        ))));
    }
    repo(&http).delete_author(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AuthorLookupParams {
    name: String,
}

/// Max author candidates enriched per search (each is one Audnexus lookup).
const MAX_AUTHOR_CANDIDATES: usize = 10;

/// Max book matches returned from `GET /books/search` (SKADI-T-0248): an add picker
/// only needs the closest few, and the unified add page stacks three domains.
const MAX_BOOK_SEARCH_RESULTS: usize = 10;

/// One author candidate the "add author" UI can act on (SKADI-T-0152): enriched
/// with a portrait + bio snippet so a watchlist pick is actually distinguishable.
#[derive(Serialize)]
struct AuthorLookupResult {
    asin: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Trim a bio to a short, word-boundary snippet for a card.
fn snippet(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    let cut = head.rfind(' ').unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

/// `GET /authors/lookup?name=` — search authors by name via Audnexus, then
/// **dedup** (Audnexus returns the same author many times), **enrich** each with
/// the full record (portrait + bio, concurrently), and **drop non-authors** —
/// book-product ASINs leak into author search but carry no bio/image
/// (SKADI-T-0152). Candidates the operator resolves with `POST /authors`.
async fn lookup_authors(
    State(http): State<AudiobooksHttp>,
    Query(params): Query<AuthorLookupParams>,
) -> Result<impl IntoResponse, ApiError> {
    let matches: Vec<_> = http
        .provider
        .search_authors(&params.name)
        .await
        .map_err(ApiError)?
        .into_iter()
        .take(MAX_AUTHOR_CANDIDATES)
        .collect();

    // Enrich each unique candidate concurrently (one Audnexus call apiece).
    let mut set = tokio::task::JoinSet::new();
    for (idx, m) in matches.into_iter().enumerate() {
        let provider = http.provider.clone();
        set.spawn(async move { (idx, provider.lookup_author(&m.asin).await.ok()) });
    }
    let mut rows: Vec<(usize, AuthorLookupResult)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        let Ok((idx, Some(author))) = joined else {
            continue;
        };
        let image = author.image.filter(|s| !s.trim().is_empty());
        let description = author.description.filter(|s| !s.trim().is_empty());
        // A genuine author profile has a bio and/or portrait; the book-product
        // ASINs that pollute author search have neither — drop them.
        if image.is_none() && description.is_none() {
            continue;
        }
        rows.push((
            idx,
            AuthorLookupResult {
                asin: author.asin.0,
                name: author.name,
                image,
                description: description.map(|d| snippet(&d, 240)),
            },
        ));
    }
    rows.sort_by_key(|(i, _)| *i); // preserve search relevance order
    let results: Vec<AuthorLookupResult> = rows.into_iter().map(|(_, r)| r).collect();
    Ok(Json(results))
}

// --- books ---

#[derive(Deserialize)]
struct ListBooksParams {
    monitored: Option<bool>,
    /// Restrict to one author's books.
    author: Option<String>,
    /// Page size. Absent means every book — 2.0 MB over a prod-sized catalog
    /// (SKADI-T-0494).
    limit: Option<i64>,
    /// Rows to skip; ignored without a `limit`.
    offset: Option<i64>,
}

async fn list_books(
    State(http): State<AudiobooksHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Query(params): Query<ListBooksParams>,
) -> Result<impl IntoResponse, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    // A non-admin's page is cut from the policy-filtered list, so the store is
    // asked for everything and paged here (SKADI-T-0617, see movies).
    let paged_by_store = member.is_admin();
    let page = |books: Vec<Book>| -> Vec<Book> {
        books
            .into_iter()
            .skip(params.offset.unwrap_or(0).max(0) as usize)
            .take(params.limit.map_or(usize::MAX, |l| l.max(0) as usize))
            .collect()
    };
    let permitted = |b: &Book| {
        member.policy.permits(
            member.role,
            skadi_core::MediaKind::Audiobook,
            &b.id.to_string(),
            None,
            &[],
        )
    };
    // `?author=` narrows to one author (and is combined with the monitored
    // filter); otherwise list all books.
    let (books, total) = match params.author {
        Some(a) => {
            // One author's books: a small, naturally bounded set, so this path
            // still filters and pages in memory.
            let author_id: AuthorId = parse_id(&a, "author")?;
            let mut books = repo(&http).list_books_by_author(author_id).await?;
            if let Some(m) = params.monitored {
                books.retain(|b| b.monitored == m);
            }
            // Household policy (SKADI-T-0612): a kid sees only allow-listed books.
            books.retain(permitted);
            let total = books.len() as i64;
            (page(books), total)
        }
        None => {
            let filter = BookFilter {
                monitored: params.monitored,
                limit: if paged_by_store { params.limit } else { None },
                offset: if paged_by_store { params.offset } else { None },
            };
            let total = repo(&http)
                .count_books(BookFilter {
                    limit: None,
                    offset: None,
                    ..filter
                })
                .await?;
            let mut books = repo(&http).list_books(filter).await?;
            if paged_by_store {
                (books, total)
            } else {
                books.retain(permitted);
                let total = books.len() as i64;
                (page(books), total)
            }
        }
    };
    Ok(([("x-total-count", total.to_string())], Json(books)))
}

#[derive(Deserialize)]
struct BookLookupParams {
    asin: String,
}

/// `GET /books/lookup?asin=` — the Audnexus metadata record for an ASIN (the
/// "add book" preview). Audnexus has no title search, so this is keyed by ASIN.
async fn lookup_book(
    State(http): State<AudiobooksHttp>,
    Query(params): Query<BookLookupParams>,
) -> Result<impl IntoResponse, ApiError> {
    let record = http
        .provider
        .lookup(&ExternalId::Asin(AsinId(params.asin)))
        .await
        .map_err(ApiError)?;
    Ok(Json(record))
}

/// One Audible catalog search hit on the wire (SKADI-T-0151).
#[derive(Serialize)]
struct BookSearchDto {
    asin: String,
    title: String,
    #[serde(default)]
    authors: Vec<String>,
    year: Option<i32>,
    cover_url: Option<String>,
}

#[derive(Deserialize)]
struct BookSearchParams {
    q: Option<String>,
}

/// `GET /books/search?q=` — free-text title/author search over the Audible
/// catalog, for the "search by title" add flow. Empty query → empty list; 503
/// when the catalog provider isn't wired.
async fn search_books(
    State(http): State<AudiobooksHttp>,
    Query(params): Query<BookSearchParams>,
) -> Result<impl IntoResponse, ApiError> {
    use chrono::Datelike;
    let q = params.q.unwrap_or_default();
    let q = q.trim();
    if q.is_empty() {
        return Ok(Json(Vec::<BookSearchDto>::new()));
    }
    let catalog = http.catalog.as_ref().ok_or_else(|| {
        ApiError(AppError::Internal(
            "audiobook search is not configured on this daemon".into(),
        ))
    })?;
    let items = catalog.search(q).await.map_err(ApiError)?;
    let dtos = items
        .into_iter()
        // Cap to the closest matches (SKADI-T-0248): Audible returns them in
        // relevance order, and the unified add page can't show every hit per domain.
        .take(MAX_BOOK_SEARCH_RESULTS)
        .map(|it| BookSearchDto {
            asin: it.asin.0,
            title: it.title,
            authors: it.authors,
            year: it.release_date.map(|d| d.year()),
            cover_url: it.cover_url,
        })
        .collect::<Vec<_>>();
    Ok(Json(dtos))
}

#[derive(Deserialize)]
struct AddBookRequest {
    asin: String,
    /// Quality profile id. Omitted → the first registered profile. When given,
    /// it must reference a registered `profiles` settings row.
    profile: Option<String>,
    /// Kick off the book file's acquire run right away (default). Needs the
    /// audiobooks domain enabled; otherwise the add still succeeds and the
    /// response carries `search_started: false`.
    #[serde(default = "default_search")]
    search: bool,
}

fn default_search() -> bool {
    true
}

/// `POST /books` — add an audiobook by ASIN. Resolves profile/root the same way
/// movies `create_movie` does (explicit ids must reference a registered row;
/// omitted falls back to the first registered one), then defers to [`add_book`].
async fn create_book(
    State(http): State<AudiobooksHttp>,
    Json(req): Json<AddBookRequest>,
) -> Result<Response, ApiError> {
    // Resolve the profile. Audiobooks rank via the built-in M4B-first ladder, not
    // a stored movie `profiles` row, so an omitted profile binds to the stable
    // built-in audiobook profile id — operators no longer have to register a
    // (movie-shaped) profile, which is what used to leak an "Audiobooks" entry
    // into the movies profile list (SKADI-T-0142). An *explicit* id is still
    // validated against the registered profiles, so typos remain 422s.
    let profile: ProfileId = match &req.profile {
        Some(p) => {
            let id: ProfileId = parse_id(p, "profile")?;
            let profiles = http.store.list_settings("profiles").await?;
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
        None => crate::audiobook_builtin_profile_id(),
    };

    // Derived root (SKADI-T-0302): books live under `<library.root>/audiobook`.
    let root_folder = RootFolder::for_domain(library_root(&http.store).await, MediaKind::Audiobook);

    match add_book(
        repo(&http),
        http.provider.as_ref(),
        AsinId(req.asin),
        profile,
        root_folder,
    )
    .await
    {
        Ok(book) => {
            let search_started = if req.search {
                maybe_start_search(&http, &book).await?
            } else {
                false
            };
            let mut body = serde_json::to_value(&book)
                .map_err(|e| ApiError(AppError::Internal(format!("serializing book: {e}"))))?;
            body["search_started"] = serde_json::json!(search_started);
            Ok((StatusCode::CREATED, Json(body)).into_response())
        }
        // add_book returns Validation for a duplicate ASIN; surface that as 409
        // Conflict rather than the generic 400 the mapper would give.
        Err(AppError::Validation(msg)) if msg.contains("already exists") => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "duplicate", "message": msg })),
        )
            .into_response()),
        Err(e) => Err(ApiError(e)),
    }
}

/// One row of the built-in audiobook quality ladder on the wire (SKADI-T-0142).
#[derive(Serialize)]
struct AudiobookQualityDto {
    /// Display name, e.g. `M4B-256`.
    name: String,
    /// Container family, e.g. `M4B` / `MP3`.
    format: String,
    /// Nominal bitrate in kbps (`0` = VBR/unknown).
    kbps: u32,
}

/// `GET /audiobooks/quality` — the built-in, format-first audiobook quality
/// ladder, ordered **best → fallback**. Read-only: audiobooks rank via this
/// fixed ladder (every M4B above every MP3), not a configurable movie profile,
/// so the Audiobooks · Config UI just displays it.
async fn audiobook_quality() -> Json<Vec<AudiobookQualityDto>> {
    use skadi_quality::audiobook::{AudioBitrate, AudioFormat, default_audiobook_definitions};

    fn format_label(f: AudioFormat) -> &'static str {
        match f {
            AudioFormat::M4b => "M4B",
            AudioFormat::M4a => "M4A",
            AudioFormat::Mp3 => "MP3",
            AudioFormat::Aac => "AAC",
            AudioFormat::Flac => "FLAC",
            AudioFormat::Ogg => "OGG",
            AudioFormat::Opus => "Opus",
        }
    }
    fn kbps(b: AudioBitrate) -> u32 {
        match b {
            AudioBitrate::Vbr => 0,
            AudioBitrate::Kbps32 => 32,
            AudioBitrate::Kbps64 => 64,
            AudioBitrate::Kbps96 => 96,
            AudioBitrate::Kbps128 => 128,
            AudioBitrate::Kbps192 => 192,
            AudioBitrate::Kbps256 => 256,
        }
    }

    // `default_audiobook_definitions()` is ordered low→high; present best-first.
    // The Unknown sentinel (SKADI-T-0175) is an internal acquire fallback, not a
    // real rung of the ladder — hide it from the operator-facing list.
    let rows = default_audiobook_definitions()
        .into_iter()
        .filter(|d| d.id != skadi_quality::audiobook::unknown_audiobook_id())
        .rev()
        .map(|d| AudiobookQualityDto {
            name: d.name,
            format: format_label(d.format).to_string(),
            kbps: kbps(d.bitrate),
        })
        .collect();
    Json(rows)
}

/// Search-on-add: fire the acquire run for the new book's file when the
/// audiobooks domain is enabled. Fire-and-forget like the manual acquire
/// endpoint; returns whether a run was started.
async fn maybe_start_search(http: &AudiobooksHttp, book: &Book) -> Result<bool, ApiError> {
    let enabled = http
        .store
        .get(DOMAIN_NAME)
        .await?
        .map(|s| s.enabled)
        .unwrap_or(false);
    let Some(file) = book.files.first() else {
        return Ok(false);
    };
    if !enabled {
        return Ok(false);
    }
    let seed = acquire_seed(book, file);
    let runner = http.runner.clone();
    tokio::spawn(async move {
        if let Err(e) = start_acquire(&runner, seed).await {
            tracing::warn!(error = %e, "search-on-add acquire run failed to start");
        }
    });
    Ok(true)
}

/// Refresh one book's metadata now (SKADI-T-0450).
async fn refresh_book_route(
    State(http): State<AudiobooksHttp>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use cloacina::executor::WorkflowExecutor;
    let id: BookId = parse_id(&id, "book")?;
    // 404 before doing any work, so a typo'd id is not reported as a queued refresh.
    repo(&http)
        .get_book(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("book {id} not found"))))?;
    let ctx = crate::refresh::refresh_context(&id.to_string())?;
    http.runner
        .execute("refresh_book_metadata", ctx)
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("refresh run failed: {e}"))))?;
    Ok(Json(serde_json::json!({ "refreshed": id.to_string() })))
}

/// Refresh every book (SKADI-T-0450).
///
/// Serial on purpose: one provider request per book, and a whole catalogue at
/// once is exactly the burst that gets an API key rate-limited — this library is
/// ~790 books. Reports what it managed, so a partial failure is visible instead
/// of silently leaving most of the catalogue stale.
async fn refresh_all_books(
    State(http): State<AudiobooksHttp>,
) -> Result<impl IntoResponse, ApiError> {
    use cloacina::executor::WorkflowExecutor;
    let books = repo(&http)
        .list_books(BookFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await?;
    let requested = books.len();
    let mut refreshed = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for book in books {
        let Ok(ctx) = crate::refresh::refresh_context(&book.id.to_string()) else {
            failed.push(book.id.to_string());
            continue;
        };
        match http.runner.execute("refresh_book_metadata", ctx).await {
            Ok(_) => refreshed += 1,
            Err(e) => {
                tracing::warn!(book = %book.id, error = %e, "bulk refresh: run failed");
                failed.push(book.id.to_string());
            }
        }
    }
    Ok(Json(serde_json::json!({
        "requested": requested,
        "refreshed": refreshed,
        "failed": failed,
    })))
}

async fn get_book(
    State(http): State<AudiobooksHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let member = skadi_api::household::member_or_admin(member);
    let id: BookId = parse_id(&id, "book")?;
    let book = repo(&http)
        .get_book(id)
        .await?
        .filter(|b| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Audiobook,
                &b.id.to_string(),
                None,
                &[],
            )
        })
        .ok_or_else(|| ApiError(AppError::NotFound(format!("book {id} not found"))))?;
    Ok(Json(book))
}

#[derive(Deserialize, Default)]
struct PatchBook {
    monitored: Option<bool>,
    /// The item's tags, as settings-record ids (SKADI-T-0560).
    ///
    /// Absent means "leave them alone"; `[]` means "remove them all". A bare
    /// `Vec` would make every PATCH that omits tags silently clear them.
    tags: Option<Vec<String>>,
}

async fn patch_book(
    State(http): State<AudiobooksHttp>,
    Path(id): Path<String>,
    Json(patch): Json<PatchBook>,
) -> Result<impl IntoResponse, ApiError> {
    let id: BookId = parse_id(&id, "book")?;
    Ok(Json(apply_patch(&http, id, patch).await?))
}

/// The one PATCH path, shared by `PATCH /books/{id}` and the bulk
/// monitor/unmonitor (SKADI-T-0696). 404 when the book is not there.
async fn apply_patch(
    http: &AudiobooksHttp,
    id: BookId,
    patch: PatchBook,
) -> Result<Book, ApiError> {
    let mut book = repo(http)
        .get_book(id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("book {id} not found"))))?;
    if let Some(m) = patch.monitored {
        book.monitored = m;
    }
    repo(http).upsert_book(&book).await?;
    if let Some(tags) = patch.tags {
        use skadi_store::ItemTagRepo;
        http.store
            .set_tags("audiobook", &book.id.0.to_string(), &tags)
            .await?;
    }
    Ok(book)
}

/// Delete options (SKADI-T-0316): `?delete_files=true` also removes the imported files +
/// prunes the now-empty folders. The web UI sends it behind a confirmation.
#[derive(serde::Deserialize)]
struct DeleteOpts {
    #[serde(default)]
    delete_files: bool,
}

async fn delete_book(
    State(http): State<AudiobooksHttp>,
    Path(id): Path<String>,
    axum::extract::Query(opts): axum::extract::Query<DeleteOpts>,
) -> Result<impl IntoResponse, ApiError> {
    let id: BookId = parse_id(&id, "book")?;
    // 404 if it isn't there, so DELETE is honest about what it removed.
    if !delete_one(&http, id, opts.delete_files).await? {
        return Err(ApiError(AppError::NotFound(format!("book {id} not found"))));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The one delete path, shared by `DELETE /books/{id}` and the bulk delete
/// (SKADI-T-0696). With `delete_files`, removes the files the book records as
/// imported (those exact paths, nothing else) and prunes the emptied folders
/// up to the book's root, never past it. Returns `false` when the book is not
/// there.
async fn delete_one(
    http: &AudiobooksHttp,
    id: BookId,
    delete_files: bool,
) -> Result<bool, ApiError> {
    let Some(book) = repo(http).get_book(id).await? else {
        return Ok(false);
    };
    // Remove the imported files + prune empty folders first (SKADI-T-0316), before the rows go.
    if delete_files {
        let paths: Vec<std::path::PathBuf> = repo(http)
            .list_book_files(id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|f| f.file.map(|fr| fr.path))
            .collect();
        let root = std::path::PathBuf::from(&book.root_folder.path);
        let removed = tokio::task::spawn_blocking(move || {
            skadi_importer::delete_files_and_prune(&paths, &root)
        })
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("delete task panicked: {e}"))))?;
        tracing::info!(book = %id, removed = removed.len(), "deleted book files on library delete");
    }
    // Take the item's tag membership with it (SKADI-T-0560). Otherwise the rows
    // outlive the item and a later one reusing the id would inherit them — the
    // silent orphaning the vision calls out as something skadi does not do.
    {
        use skadi_store::ItemTagRepo;
        let _ = http.store.clear_item("audiobook", &id.0.to_string()).await;
    }
    repo(http).delete_book(id).await?;
    Ok(true)
}

/// `POST /books/bulk` (SKADI-T-0696): apply one action to many books in one
/// request. Each id goes through the single-item path (`apply_patch`,
/// `start_file_acquire`, `delete_one`), so a bulk action cannot do what the
/// single route would not.
async fn bulk_books(
    State(http): State<AudiobooksHttp>,
    skadi_api::error::ApiJson(req): skadi_api::error::ApiJson<BulkRequest>,
) -> Result<Response, ApiError> {
    let ids: Vec<(String, BookId)> = req.parsed_ids()?;
    if req.action == BulkAction::Search && !domain_enabled(&http).await? {
        return Ok(domain_disabled_response());
    }
    let mut report = BulkReport::new(req.action, ids.len());
    for (raw, id) in ids {
        let outcome = match req.action {
            BulkAction::Monitor | BulkAction::Unmonitor => {
                let patch = PatchBook {
                    monitored: Some(req.action == BulkAction::Monitor),
                    ..PatchBook::default()
                };
                apply_patch(&http, id, patch)
                    .await
                    .map(|_| BulkOutcome::Done)
            }
            BulkAction::Search => search_book(&http, id).await,
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

/// Search one book now: start the manual acquire for each file that is not
/// imported and not already in flight.
async fn search_book(http: &AudiobooksHttp, id: BookId) -> Result<BulkOutcome, ApiError> {
    let Some(book) = repo(http).get_book(id).await? else {
        return Ok(BulkOutcome::NotFound);
    };
    let mut started = 0;
    for file in &book.files {
        if matches!(file.status, AcquisitionStatus::Imported { .. }) {
            continue;
        }
        if start_file_acquire(http, &book, file) {
            started += 1;
        }
    }
    Ok(BulkOutcome::Searched(started))
}

/// Whether the audiobooks domain is enabled; manual acquire is refused while off.
async fn domain_enabled(http: &AudiobooksHttp) -> Result<bool, ApiError> {
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
            "message": "the audiobooks domain is disabled; enable it before acquiring"
        })),
    )
        .into_response()
}

// --- manual acquire ---

/// The acquire seed for one (book, file) on the interactive paths (manual
/// acquire, search on add). Delegates to [`crate::wanted::book_seed`] so these
/// paths carry the author and series exactly as the sweep does (SKADI-T-0670).
fn acquire_seed(book: &Book, file: &BookFile) -> AcquireSeed {
    crate::wanted::book_seed(book, file, None)
}

/// Resolve (book, file) and verify the file belongs to the book — the common
/// preamble for the per-file acquire/releases/grab/reset endpoints.
async fn load_book_file(
    http: &AudiobooksHttp,
    book_id: BookId,
    file_id: BookFileId,
) -> Result<(Book, BookFile), ApiError> {
    let book = repo(http)
        .get_book(book_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("book {book_id} not found"))))?;
    let file = repo(http)
        .get_book_file(file_id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("book file {file_id} not found"))))?;
    if file.book_id != book_id {
        return Err(ApiError(AppError::NotFound(format!(
            "book file {file_id} does not belong to book {book_id}"
        ))));
    }
    Ok((book, file))
}

/// Where an imported book file lives on disk, for the detail page's management
/// section (SKADI-T-0147).
#[derive(Serialize)]
struct FileLocationDto {
    /// The canonical library folder the book's files live in.
    folder: String,
    /// Container format, derived from the actual file extension (e.g. `MP3`,
    /// `M4B`) — truthful even when the recorded quality tier disagrees.
    format: String,
    /// Number of regular files under the folder (recursive).
    file_count: usize,
    /// Total bytes of those files.
    total_bytes: u64,
    /// When the file was imported (RFC 3339).
    imported_at: Option<String>,
}

/// `GET /books/{id}/files/{fid}/location` — the on-disk folder + size/count for an
/// imported book file. 404 until the file is imported (no location yet).
async fn book_file_location(
    State(http): State<AudiobooksHttp>,
    Path((id, fid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;
    let (_, file) = load_book_file(&http, book_id, file_id).await?;

    let (path, at) = match &file.status {
        AcquisitionStatus::Imported { file, at, .. } => (file.path.clone(), *at),
        _ => {
            return Err(ApiError(AppError::NotFound(
                "book file is not imported — no on-disk location yet".into(),
            )));
        }
    };
    let folder = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| path.clone());
    let format = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_uppercase)
        .unwrap_or_default();

    // Stat the folder off the async runtime — recursive read_dir is blocking fs.
    let to_stat = folder.clone();
    let (file_count, total_bytes) = tokio::task::spawn_blocking(move || dir_stats(&to_stat))
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("stat folder: {e}"))))?;

    Ok((
        StatusCode::OK,
        Json(FileLocationDto {
            folder: folder.to_string_lossy().into_owned(),
            format,
            file_count,
            total_bytes,
            imported_at: Some(at.to_rfc3339()),
        }),
    )
        .into_response())
}

/// `GET /books/{id}/files/{fid}/audio` — the imported audio file's bytes, with
/// single-range support (SKADI-T-0329 / I-0048). Range exists so the offline
/// player can RESUME interrupted multi-GB downloads — this is a download
/// endpoint, not a streaming product. Only an `Imported` file has bytes.
async fn book_file_audio(
    State(http): State<AudiobooksHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, fid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;
    let member = skadi_api::household::member_or_admin(member);
    if !member.is_admin() {
        // Household policy (SKADI-T-0612): a hidden book's bytes are hidden too.
        let ok = repo(&http).get_book(book_id).await?.is_some_and(|b| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Audiobook,
                &b.id.to_string(),
                None,
                &[],
            )
        });
        if !ok {
            return Err(ApiError(AppError::NotFound(format!(
                "book {book_id} not found"
            ))));
        }
    }
    let (_, file) = load_book_file(&http, book_id, file_id).await?;
    let path = match &file.status {
        AcquisitionStatus::Imported { file, .. } => file.path.clone(),
        _ => {
            return Err(ApiError(AppError::NotFound(
                "book file is not imported — no audio to download yet".into(),
            )));
        }
    };

    // Shared with movies and television (SKADI-T-0574). The Range logic here
    // was the only correct one in the codebase; it now lives in
    // `skadi_api::ranged` so a third copy does not drift from this one.
    skadi_api::ranged::serve_file_range(&path, &headers, "audio").await
}

/// One chapter mark for the player (SKADI-T-0330).
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ChapterDto {
    index: usize,
    title: String,
    start_s: f64,
    end_s: f64,
}

/// `GET /books/{id}/files/{fid}/chapters` — the imported file's embedded
/// chapter marks (QuickTime chapter track), fetched once at download time and
/// stored offline by the player. A book without readable chapters returns `[]`
/// — the player degrades to a plain seek bar, never an error.
async fn book_file_chapters(
    State(http): State<AudiobooksHttp>,
    member: Option<axum::extract::Extension<skadi_api::household::Member>>,
    Path((id, fid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;
    let member = skadi_api::household::member_or_admin(member);
    if !member.is_admin() {
        // Household policy (SKADI-T-0612): a hidden book's bytes are hidden too.
        let ok = repo(&http).get_book(book_id).await?.is_some_and(|b| {
            member.policy.permits(
                member.role,
                skadi_core::MediaKind::Audiobook,
                &b.id.to_string(),
                None,
                &[],
            )
        });
        if !ok {
            return Err(ApiError(AppError::NotFound(format!(
                "book {book_id} not found"
            ))));
        }
    }
    let (_, file) = load_book_file(&http, book_id, file_id).await?;
    let path = match &file.status {
        AcquisitionStatus::Imported { file, .. } => file.path.clone(),
        _ => {
            return Err(ApiError(AppError::NotFound(
                "book file is not imported — no chapters yet".into(),
            )));
        }
    };
    // Blocking box-walk over a (possibly NFS) file — off the async runtime.
    let marks = tokio::task::spawn_blocking(move || {
        skadi_media_probe::chapters::chapters(&path).unwrap_or_default()
    })
    .await
    .map_err(|e| ApiError(AppError::Internal(format!("chapter probe: {e}"))))?;
    let dtos: Vec<ChapterDto> = marks
        .into_iter()
        .map(|m| ChapterDto {
            index: m.index,
            title: m.title,
            start_s: m.start_secs,
            end_s: m.end_secs,
        })
        .collect();
    Ok((StatusCode::OK, Json(dtos)).into_response())
}

/// Count regular files + sum their sizes under `dir` (recursive, best-effort —
/// unreadable entries are skipped rather than failing the whole report).
fn dir_stats(dir: &std::path::Path) -> (usize, u64) {
    let mut count = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                count += 1;
                if let Ok(md) = entry.metadata() {
                    bytes += md.len();
                }
            }
        }
    }
    (count, bytes)
}

/// The fresh in-flight check shared by acquire + grab (SKADI-T-0112): a file
/// already being worked (and recently updated) must not get a second run.
fn fresh_in_flight(status: &AcquisitionStatus, updated_at: chrono::DateTime<Utc>) -> bool {
    let in_flight = matches!(
        status,
        AcquisitionStatus::Searching { .. }
            | AcquisitionStatus::Snatched { .. }
            | AcquisitionStatus::Downloading { .. }
    );
    let fresh = (Utc::now() - updated_at)
        < chrono::Duration::seconds(skadi_hunter::STALE_ACQUIRE_GRACE.as_secs() as i64);
    in_flight && fresh
}

async fn acquire_file(
    State(http): State<AudiobooksHttp>,
    Path((id, fid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;

    // Manual acquire requires the audiobooks domain to be enabled.
    if !domain_enabled(&http).await? {
        return Ok(domain_disabled_response());
    }

    let (book, file) = load_book_file(&http, book_id, file_id).await?;
    if !start_file_acquire(&http, &book, &file) {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this book file is already in progress"
            })),
        )
            .into_response());
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "run_id": fid, "accepted": true })),
    )
        .into_response())
}

/// The manual acquire for one book file, shared by `POST …/acquire` and the
/// bulk search (SKADI-T-0696). Returns `false` (and starts nothing) when a fresh
/// run is already working the file.
fn start_file_acquire(http: &AudiobooksHttp, book: &Book, file: &BookFile) -> bool {
    if fresh_in_flight(&file.status, file.updated_at) {
        return false;
    }
    let seed = acquire_seed(book, file);
    // Fire-and-forget: the acquire workflow runs on its own task and the file's
    // status is persisted by the pipeline's status sink. The caller answers 202
    // with the file id as the correlation reference.
    let runner = http.runner.clone();
    tokio::spawn(async move {
        if let Err(e) = start_acquire(&runner, seed).await {
            tracing::warn!(error = %e, "manual acquire run failed to start");
        }
    });
    true
}

/// Force a wedged book file back to `Missing` so it can be re-acquired. Mirrors
/// `skadi_movies::http::reset_edition`.
async fn reset_file(
    State(http): State<AudiobooksHttp>,
    Path((id, fid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;

    let (_book, file) = load_book_file(&http, book_id, file_id).await?;
    let previous = format!("{:?}", file.status);
    repo(&http)
        .set_book_file_status(file_id, AcquisitionStatus::Missing)
        .await?;
    tracing::info!(file = %file_id, %previous, "book file reset to Missing");

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "file": fid,
            "status": "Missing",
            "previous": previous,
        })),
    )
        .into_response())
}

/// One candidate release for the interactive-search UI: the raw
/// [`Release`](skadi_indexers::Release) (echoed back to `grab`) plus derived
/// display fields and the profile verdict. Mirrors the movies `ReleaseCandidate`.
#[derive(Serialize)]
struct ReleaseCandidate {
    release: skadi_indexers::Release,
    release_key: String,
    quality: String,
    age_days: i64,
    protocol: &'static str,
    accepted: bool,
    reason: String,
}

/// Body of `POST /audiobooks/quality/test` (SKADI-T-0184): a release title and an
/// optional size (bytes).
#[derive(Deserialize)]
struct TestReleaseRequest {
    title: String,
    #[serde(default)]
    size: Option<u64>,
}

/// `POST /audiobooks/quality/test` — explain how `title` would be judged against
/// the audiobooks domain's **live** profile + audiobook axis + custom formats,
/// without a search or grab (SKADI-T-0184). Returns the full
/// [`ReleaseExplanation`](skadi_hunter::ReleaseExplanation). Mirrors
/// `skadi_movies::http::test_release` but scores on the audiobook axis.
async fn test_release(
    State(_http): State<AudiobooksHttp>,
    Json(req): Json<TestReleaseRequest>,
) -> Result<Response, ApiError> {
    if req.title.trim().is_empty() {
        return Err(ApiError(AppError::Validation(
            "title must not be empty".into(),
        )));
    }
    let svc = skadi_hunter::try_services_for(MediaKind::Audiobook).ok_or_else(|| {
        ApiError(AppError::Config(
            "providers not initialised yet; try again once the daemon has reconciled".into(),
        ))
    })?;
    let no_block = std::collections::HashSet::new();
    let scoring = skadi_hunter::Scoring {
        definitions: &svc.scoring.definitions,
        profile: &svc.scoring.profile,
        formats: &svc.scoring.formats,
        min_seeders: 0,
        // Per-indexer priority + seeder floor (SKADI-T-0539).
        indexer_flags: &skadi_hunter::indexer_flags(&svc.indexers),
        blocklisted: &no_block,
        audiobook: svc.scoring.audiobook.as_ref(),
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let release = skadi_indexers::Release {
        indexer: skadi_core::IndexerId::new(),
        title: req.title.clone(),
        fetch: skadi_indexers::ReleaseFetch::Magnet("magnet:?xt=urn:btih:test".into()),
        size: req.size.unwrap_or(0),
        published: Utc::now(),
        seeders: None,
        categories: Vec::new(),
        parsed: skadi_quality::parse(&req.title),
    };
    Ok(Json(skadi_hunter::explain(&release, &scoring)).into_response())
}

/// `GET /books/{id}/files/{fid}/releases` — run the live indexer search for the
/// file and return each candidate scored against the active profile. Read-only;
/// does not snatch. Mirrors `skadi_movies::http::list_releases`.
async fn list_releases(
    State(http): State<AudiobooksHttp>,
    Path((id, fid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;
    let (book, file) = load_book_file(&http, book_id, file_id).await?;

    let svc = skadi_hunter::try_services_for(MediaKind::Audiobook).ok_or_else(|| {
        ApiError(AppError::Config(
            "providers not initialised yet; try again once the daemon has reconciled".into(),
        ))
    })?;

    let seed = acquire_seed(&book, &file);
    let mut state = skadi_hunter::AcquireState::new(seed.acquirable, seed.request, seed.profile);
    {
        // The operator asked for this list by hand, so indexers with automatic
        // search off are still consulted (SKADI-T-0539).
        state.request.trigger = skadi_hunter::SearchTrigger::Interactive;
        skadi_hunter::search(&mut state, &svc.indexers)
    }
    .await
    .map_err(ApiError)?;

    // Reflect the blocklist in the verdicts: a blocklisted candidate shows as
    // rejected rather than being hidden.
    let blocklisted = http.store.blocked_keys().await.unwrap_or_default();
    // If the file is already imported, reflect upgrade semantics in the verdicts
    // (SKADI-T-0182/0186): a candidate no better than the held file (on quality or
    // format score) shows as "not an upgrade" rather than accepted.
    let (current_quality, current_format_score) = match &file.status {
        skadi_core::AcquisitionStatus::Imported { quality, score, .. } => {
            (Some(*quality), Some(*score))
        }
        _ => (None, None),
    };
    // Always false for audiobooks: `current_unplayable` is about direct-playing
    // VIDEO, and an audiobook has none (SKADI-T-0584).
    let current_unplayable = false;
    let scoring = skadi_hunter::Scoring {
        definitions: &svc.scoring.definitions,
        profile: &svc.scoring.profile,
        formats: &svc.scoring.formats,
        min_seeders: svc.scoring.min_seeders,
        // Per-indexer priority + seeder floor (SKADI-T-0539).
        indexer_flags: &skadi_hunter::indexer_flags(&svc.indexers),
        blocklisted: &blocklisted,
        audiobook: svc.scoring.audiobook.as_ref(),
        current_quality,
        current_format_score,
        current_unplayable,
    };
    let now = Utc::now();
    let candidates: Vec<ReleaseCandidate> = state
        .candidates
        .iter()
        .map(|r| {
            let (verdict, _) = skadi_hunter::evaluate(r, &scoring);
            ReleaseCandidate {
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

/// `POST /books/{id}/files/{fid}/grab` — grab a specific release the operator
/// picked. Starts the acquire workflow at the snatch step. Needs the audiobooks
/// domain enabled and honours the in-flight guard, like the auto-acquire trigger.
/// Mirrors `skadi_movies::http::grab_release`.
async fn grab_release(
    State(http): State<AudiobooksHttp>,
    Path((id, fid)): Path<(String, String)>,
    Json(req): Json<GrabRequest>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;

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
                "message": "the audiobooks domain is disabled; enable it before grabbing"
            })),
        )
            .into_response());
    }

    let (book, file) = load_book_file(&http, book_id, file_id).await?;
    if fresh_in_flight(&file.status, file.updated_at) {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this book file is already in progress"
            })),
        )
            .into_response());
    }

    let seed = acquire_seed(&book, &file);
    let runner = http.runner.clone();
    let release = req.release;
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
        Json(serde_json::json!({ "file": fid, "accepted": true })),
    )
        .into_response())
}

/// Body of `POST …/grab-link` (SKADI-I-0043): an operator-pasted magnet or
/// `.torrent` URL for this book file, with an optional title.
#[derive(Deserialize)]
struct GrabLinkRequest {
    link: String,
    #[serde(default)]
    title: Option<String>,
}

/// `POST /books/{id}/files/{fid}/grab-link` — manual acquisition: synthesize a
/// release from the pasted magnet or `.torrent` URL (audiobook title parser) + grab
/// it through the same snatch-step pipeline, tracked + imported against the book file.
async fn grab_link(
    State(http): State<AudiobooksHttp>,
    Path((id, fid)): Path<(String, String)>,
    Json(req): Json<GrabLinkRequest>,
) -> Result<Response, ApiError> {
    let book_id: BookId = parse_id(&id, "book")?;
    let file_id: BookFileId = parse_id(&fid, "book file")?;
    let release = skadi_indexers::release_from_link(
        &req.link,
        req.title.as_deref(),
        skadi_quality::parser::parse_audiobook,
    )
    .ok_or_else(|| {
        ApiError(AppError::Validation(
            "not a valid magnet or .torrent URL (expected magnet:?xt=urn:btih:… or http(s)://…)"
                .into(),
        ))
    })?;

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
                "message": "the audiobooks domain is disabled; enable it before grabbing"
            })),
        )
            .into_response());
    }
    let (book, file) = load_book_file(&http, book_id, file_id).await?;
    if fresh_in_flight(&file.status, file.updated_at) {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "already_in_flight",
                "message": "an acquire run for this book file is already in progress"
            })),
        )
            .into_response());
    }

    let seed = acquire_seed(&book, &file);
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
            tracing::warn!(error = %e, "manual magnet grab failed to start");
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "file": fid, "accepted": true })),
    )
        .into_response())
}

// --- library import (SKADI-T-0134) ---

#[derive(Deserialize)]
struct ScanRequest {
    /// Root folder to scan (as the daemon sees it, e.g. `/media/audiobooks`).
    path: String,
    /// Include books already in the library (SKADI-T-0541). `false` by default.
    #[serde(default)]
    include_imported: bool,
}

/// One scanned audiobook candidate, **parse-only** (no metadata lookup).
/// `path` is the stable key (the first audio file) the UI merges match results
/// back into; `files` is the full set placed on commit.
#[derive(Serialize)]
struct ScanCandidateDto {
    path: String,
    files: Vec<String>,
    display_name: String,
    parsed_author: Option<String>,
    parsed_title: Option<String>,
    parsed_series: Option<String>,
    parsed_series_position: Option<String>,
    /// ASIN parsed from the folder/file name (canonical `{asin-…}` tag), if any.
    asin: Option<String>,
    single_file: bool,
}

/// `POST /audiobooks/library-import/scan` — walk a root folder and return parsed
/// candidates **without any metadata lookups**. Read-only and fast. ASIN-driven
/// matching is a separate step ([`library_import_match`]).
async fn library_import_scan(
    State(http): State<AudiobooksHttp>,
    Json(req): Json<ScanRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let started = std::time::Instant::now();
    let scan_path = req.path.clone();
    // The walk is blocking + can be slow on a high-latency mount; keep it off the
    // async worker.
    let candidates = tokio::task::spawn_blocking(move || {
        import::scan_candidates(std::path::Path::new(&scan_path))
    })
    .await
    .map_err(|e| ApiError(AppError::Internal(format!("scan task panicked: {e}"))))?
    .map_err(ApiError)?;
    // Drop books already in the library (SKADI-T-0541). Matched by **inode** on
    // the candidate's key file: import hardlinks library files to their source
    // (SKADI-T-0424), so the held copy and the scanned one share an inode under
    // two different names.
    //
    // The key file, not every file: a multi-file book is one candidate, and its
    // key path is the same identity the UI merges match results back onto.
    let total_scanned = candidates.len();
    let candidates = if req.include_imported {
        candidates
    } else {
        let held: Vec<std::path::PathBuf> = repo(&http)
            .list_books(BookFilter::default())
            .await?
            .iter()
            .flat_map(|b| b.files.iter())
            .filter_map(|f| f.file.as_ref().map(|x| x.path.clone()))
            .collect();
        let keys: Vec<std::path::PathBuf> = candidates
            .iter()
            .map(|c| c.key_path().to_path_buf())
            .collect();
        let keep: Vec<bool> = tokio::task::spawn_blocking(move || {
            let ids = skadi_importer::file_identities(&held);
            keys.iter()
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
            path: c.key_path().to_string_lossy().into_owned(),
            files: c
                .files
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            display_name: c.display_name,
            parsed_author: c.author,
            parsed_title: c.title,
            parsed_series: c.series,
            parsed_series_position: c.series_position,
            asin: c.asin.map(|a| a.0),
            single_file: c.single_file,
        })
        .collect();
    tracing::info!(
        path = %req.path,
        total = dtos.len(),
        already_imported = hidden,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "audiobook library-import scan complete (parse-only)"
    );
    Ok(Json(dtos))
}

/// How many metadata lookups [`library_import_match`] runs concurrently within a
/// page. Bounded so a page doesn't open a burst Audnexus would rate-limit.
const MATCH_CONCURRENCY: usize = 6;

/// One candidate to match: its stable `path` key plus the ASIN (parsed from the
/// folder name or typed by the operator). Audnexus has no title search, so a
/// match needs an ASIN — a candidate with none is returned `unmatched`.
#[derive(Deserialize)]
struct MatchItem {
    path: String,
    #[serde(default)]
    asin: Option<String>,
}

#[derive(Deserialize)]
struct MatchRequest {
    items: Vec<MatchItem>,
}

/// The proposed Audnexus match for a candidate (built from the ASIN lookup).
#[derive(Serialize)]
struct ProposedMatch {
    asin: String,
    title: String,
    author: Option<String>,
    series: Option<String>,
    year: Option<u16>,
}

/// The match resolved for one candidate, keyed by `path`. `confidence` is `high`
/// when an ASIN looked up cleanly, else `none` (the operator must paste an ASIN).
#[derive(Serialize)]
struct MatchDto {
    path: String,
    proposed: Option<ProposedMatch>,
    confidence: &'static str,
    needs_review: bool,
    already_in_library: bool,
}

/// Resolve one candidate via its ASIN (Audnexus is ASIN-keyed; no title search).
async fn resolve_match(http: &AudiobooksHttp, item: MatchItem) -> MatchDto {
    let asin = item.asin.as_deref().map(|s| AsinId(s.trim().to_string()));
    let proposed = match &asin {
        Some(asin) if !asin.0.is_empty() => http
            .provider
            .lookup(&ExternalId::Asin(asin.clone()))
            .await
            .ok()
            .map(|rec| ProposedMatch {
                asin: asin.0.clone(),
                title: rec.title,
                author: rec.authors.first().cloned(),
                series: rec.series,
                year: rec
                    .release_date
                    .map(|d| chrono::Datelike::year(&d))
                    .and_then(|y| u16::try_from(y).ok()),
            }),
        _ => None,
    };

    let already_in_library = match (&asin, &proposed) {
        (Some(asin), Some(_)) => repo(http)
            .get_book_by_asin(asin)
            .await
            .ok()
            .flatten()
            .is_some(),
        _ => false,
    };

    let (confidence, needs_review) = match &proposed {
        Some(_) => ("high", false),
        None => ("none", true),
    };

    MatchDto {
        path: item.path,
        proposed,
        confidence,
        needs_review,
        already_in_library,
    }
}

/// `POST /audiobooks/library-import/match` — resolve ASIN-keyed matches for a
/// batch (one page) of scanned candidates. Bounded concurrency
/// ([`MATCH_CONCURRENCY`]); result order matches the request via the `path` key.
async fn library_import_match(
    State(http): State<AudiobooksHttp>,
    Json(req): Json<MatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let started = std::time::Instant::now();
    let n = req.items.len();
    let sem = Arc::new(tokio::sync::Semaphore::new(MATCH_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();

    for (idx, item) in req.items.into_iter().enumerate() {
        let http = http.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("match semaphore not closed");
            (idx, resolve_match(&http, item).await)
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
        "audiobook library-import match batch complete"
    );
    Ok(Json(dtos))
}

#[derive(Deserialize)]
struct CommitItem {
    /// The audio source files for this audiobook (from the scan; never modified).
    files: Vec<String>,
    /// The operator-confirmed Audible ASIN to build the book from.
    asin: String,
    /// Detected audiobook quality id; falls back to the lowest when absent.
    #[serde(default)]
    quality_id: Option<String>,
}

#[derive(Deserialize)]
struct CommitRequest {
    /// Quality profile to assign the imported books. Optional (SKADI-T-0301):
    /// audiobooks rank via the built-in M4B-first ladder, not a tunable video
    /// `profiles` row, so an omitted profile binds to the stable built-in
    /// audiobook profile id — the import UI no longer offers a (video) profile.
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

/// `POST /audiobooks/library-import/commit` — build each confirmed item from
/// Audnexus and restructure its audio file(s) into the canonical layout under the
/// root folder (hardlink; in-place no-op when already canonical), registering an
/// `Imported` book file. Source files are never modified or removed; an ASIN
/// already in the library is skipped.
async fn library_import_commit(
    State(http): State<AudiobooksHttp>,
    Json(req): Json<CommitRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Resolve the profile the same way `POST /books` does: an explicit id must
    // reference a registered row (typos stay 422s); omitted binds to the built-in
    // audiobook ladder (SKADI-T-0301 — no video profile leaks into the import UI).
    let profile: ProfileId = match &req.profile {
        Some(p) if !p.trim().is_empty() => {
            let id: ProfileId = parse_id(p, "profile")?;
            let profiles = http.store.list_settings("profiles").await?;
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
        _ => crate::audiobook_builtin_profile_id(),
    };
    // Derived root (SKADI-T-0302): books adopt into `<library.root>/audiobook`.
    let root = RootFolder::for_domain(library_root(&http.store).await, MediaKind::Audiobook);

    let n_items = req.items.len();
    tracing::info!(items = n_items, "audiobook library-import commit: starting");

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut linked = 0usize;
    let mut in_place = 0usize;
    let mut unmatched = Vec::new();
    let mut errors = Vec::new();
    // Distinct author names of newly-imported books, so we can catalogue their
    // bodies of work right after the commit (SKADI-T-0160).
    let mut imported_authors: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in req.items {
        let quality = item
            .quality_id
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok())
            .map(QualityId::from);
        let files: Vec<PathBuf> = item.files.iter().map(PathBuf::from).collect();
        let label = files
            .first()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| item.asin.clone());
        let result = import::commit_item(
            repo(&http),
            http.provider.as_ref(),
            files,
            AsinId(item.asin),
            profile,
            root.clone(),
            quality,
        )
        .await;
        match result {
            Ok(Some((book, placement))) => {
                imported += 1;
                for name in book.authors {
                    let name = name.trim();
                    if !name.is_empty() {
                        imported_authors.insert(name.to_string());
                    }
                }
                match placement {
                    import::Placement::Linked => linked += 1,
                    import::Placement::InPlace => in_place += 1,
                    import::Placement::AdoptedDest => {
                        // Benign (SKADI-T-0328): the file already at the
                        // canonical path was registered; this source stays put.
                        in_place += 1;
                        unmatched.push(format!(
                            "{label}: a file already at the canonical path was adopted — \
                             this source was left in place as a duplicate"
                        ));
                    }
                }
            }
            Ok(None) => skipped += 1,
            Err(e) => errors.push(format!("{label}: {e}")),
        }
    }

    // Catalogue the imported authors' bodies of work in the background (know-only:
    // no acquisition) so series completeness shows up without waiting for the
    // periodic sweep (SKADI-T-0160).
    if let Some(catalog) = http.catalog.clone()
        && !imported_authors.is_empty()
    {
        let store = http.store.clone();
        tokio::spawn(async move {
            for name in imported_authors {
                if let Err(e) = ingest_author_works(&store, catalog.as_ref(), &name, None).await {
                    tracing::warn!(author = %name, error = %e, "import: catalog ingest failed");
                }
            }
        });
    }
    tracing::info!(
        items = n_items,
        imported,
        skipped,
        linked,
        in_place,
        unmatched = unmatched.len(),
        errors = errors.len(),
        "audiobook library-import commit: done"
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

// --- unified library provider (SKADI-T-0423) ---------------------------------

/// Presents the audiobook catalog on `/library` and `/root-folders/{id}/unmapped`.
///
/// Audiobooks registered no `LibraryProvider` at all, so the unified library
/// silently omitted every book — `/library` returned movies and series only, and
/// `unmapped` reported every author folder as unoccupied. That is wider than the
/// ticket's "occupied_folders defaults to empty": there was nothing to default.
pub struct AudiobooksLibrary {
    store: Store,
}

impl AudiobooksLibrary {
    /// Build over the daemon's store.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

/// Coarse status discriminant for the library view, matching the other domains.
fn library_status_kind(status: &AcquisitionStatus) -> &'static str {
    match status {
        AcquisitionStatus::Missing => "missing",
        AcquisitionStatus::Searching { .. } => "searching",
        AcquisitionStatus::Snatched { .. } => "snatched",
        AcquisitionStatus::Downloading { .. } => "downloading",
        AcquisitionStatus::Imported { .. } => "imported",
        AcquisitionStatus::Failed { .. } => "failed",
        AcquisitionStatus::Cutoff => "cutoff",
    }
}

fn book_to_library_dto(book: Book) -> skadi_api::LibraryItemDto {
    let editions = book
        .files
        .iter()
        .map(|f| skadi_api::LibraryEditionDto {
            id: f.id.to_string(),
            // Audiobooks have no edition-kind registry yet (SKADI-T-0448); the
            // file itself is the acquirable unit.
            kind: "audiobook".into(),
            kind_name: None,
            status_kind: library_status_kind(&f.status).to_string(),
            monitored: f.monitored,
            quality: f.quality.map(|q| q.to_string()),
            // Audiobook qualities live on their own ladder, not the video one,
            // so the shared video lookup would not find them (SKADI-T-0454).
            quality_name: None,
            media_info: f.media_info.clone(),
        })
        .collect();
    skadi_api::LibraryItemDto {
        kind: "audiobook".into(),
        id: book.id.to_string(),
        title: book.title,
        year: book.year,
        monitored: book.monitored,
        editions,
    }
}

#[async_trait::async_trait]
impl skadi_api::LibraryProvider for AudiobooksLibrary {
    fn domain(&self) -> &str {
        DOMAIN_NAME
    }

    fn kind(&self) -> skadi_core::MediaKind {
        skadi_core::MediaKind::Audiobook
    }

    async fn items(
        &self,
        monitored: Option<bool>,
    ) -> skadi_core::Result<Vec<skadi_api::LibraryItemDto>> {
        let books = (&self.store as &dyn AudiobooksRepo)
            .list_books(BookFilter {
                monitored,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(books.into_iter().map(book_to_library_dto).collect())
    }

    /// The bound reaches diesel (SKADI-T-0494); `list_books` hydrates per row, so
    /// an unbounded call hydrates the whole catalog.
    async fn items_page(
        &self,
        monitored: Option<bool>,
        limit: Option<usize>,
        offset: usize,
    ) -> skadi_core::Result<Vec<skadi_api::LibraryItemDto>> {
        let books = (&self.store as &dyn AudiobooksRepo)
            .list_books(BookFilter {
                monitored,
                limit: limit.map(|l| l as i64),
                offset: Some(offset as i64),
            })
            .await?;
        Ok(books.into_iter().map(book_to_library_dto).collect())
    }

    async fn count(&self, monitored: Option<bool>) -> skadi_core::Result<usize> {
        let n = (&self.store as &dyn AudiobooksRepo)
            .count_books(BookFilter {
                monitored,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(n.max(0) as usize)
    }

    /// Each book occupies `<root>/<AuthorFolder>` — the immediate child of its
    /// root, derived through the naming engine so it matches what the importer
    /// writes (SKADI-T-0423).
    async fn occupied_folders(&self) -> skadi_core::Result<Vec<std::path::PathBuf>> {
        let books = (&self.store as &dyn AudiobooksRepo)
            .list_books(BookFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        Ok(books
            .into_iter()
            .filter_map(|b| {
                let p = crate::naming::canonical_audiobook_path(
                    &b.root_folder.path,
                    b.authors.first().map(String::as_str),
                    b.series.as_ref().map(|s| s.name.as_str()),
                    None,
                    &b.title,
                    b.external_ids.asin.as_ref().map(|a| a.0.as_str()),
                    std::path::Path::new("x.m4b"),
                    true,
                );
                let rel = p.strip_prefix(&b.root_folder.path).ok()?;
                let first = rel.components().next()?;
                Some(b.root_folder.path.join(first))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests covering the routing + parse-error edges that don't need a
    //! database. The full HTTP-boundary coverage (CRUD, add-by-ASIN against a
    //! scripted Audnexus server, acquire/releases) lives in `tests/http.rs`.

    use super::*;
    use skadi_core::ExternalIds;

    // The Range parser moved to `skadi_api::ranged` (SKADI-T-0574) so movies and
    // television share it rather than growing copies. These assertions stay here
    // deliberately: they are what proves the extraction was faithful to the
    // behaviour audiobook resume depends on.
    use skadi_api::ranged::parse_byte_range;

    #[test]
    fn byte_range_parsing_covers_download_resume_forms() {
        // No header → whole file (200).
        assert_eq!(parse_byte_range(None, 100), None);
        // Open-ended resume: `bytes=40-` → the rest of the file.
        assert_eq!(parse_byte_range(Some("bytes=40-"), 100), Some(Ok((40, 99))));
        // Closed range, end clamped to the file.
        assert_eq!(parse_byte_range(Some("bytes=0-49"), 100), Some(Ok((0, 49))));
        assert_eq!(
            parse_byte_range(Some("bytes=90-1000"), 100),
            Some(Ok((90, 99)))
        );
        // Suffix form: the last N bytes.
        assert_eq!(parse_byte_range(Some("bytes=-10"), 100), Some(Ok((90, 99))));
        // Genuinely unsatisfiable single ranges → 416: start past EOF, `-0`.
        assert_eq!(parse_byte_range(Some("bytes=100-"), 100), Some(Err(())));
        assert_eq!(parse_byte_range(Some("bytes=-0"), 100), Some(Err(())));
        // RFC 7233: invalid/unsupported Range headers are IGNORED (200), NOT
        // 416 — inverted range, multi-range, and garbage all serve the whole
        // file (review pass 2, server finding 6).
        assert_eq!(parse_byte_range(Some("bytes=50-40"), 100), None);
        assert_eq!(parse_byte_range(Some("bytes=0-1,5-9"), 100), None);
        assert_eq!(parse_byte_range(Some("elephants=1-2"), 100), None);
    }

    // --- SKADI-T-0651: series rollups merge library books with known works ---

    /// A series work with no position — counted as *related* since SKADI-T-0654.
    fn work(asin: &str, series: &str) -> crate::work::Work {
        let mut w = crate::work::Work::new(AsinId(asin.into()), asin);
        w.series_name = Some(series.into());
        w.series_asin = Some(AsinId("SERIES".into()));
        w
    }

    fn work_at(asin: &str, series: &str, position: &str) -> crate::work::Work {
        let mut w = work(asin, series);
        w.series_position = Some(position.into());
        w
    }

    fn lib_book(asin: Option<&str>, series: &str, position: Option<&str>) -> Book {
        let mut b = Book::new(
            ExternalIds {
                asin: asin.map(|a| AsinId(a.into())),
                ..Default::default()
            },
            asin.unwrap_or("no-asin"),
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        b.series = Some(crate::author::SeriesLink {
            series_id: skadi_core::BookSeriesId::new(),
            name: series.into(),
            position: position.map(str::to_string),
        });
        b
    }

    fn rollup(works: &[crate::work::Work], books: &[Book]) -> Vec<SeriesRollupDto> {
        rollup_series(works, books, &std::collections::HashSet::new())
    }

    #[test]
    fn a_work_and_a_library_book_with_different_asins_are_two_members() {
        let r = rollup(
            &[work_at("W1", "Saga", "2")],
            &[lib_book(Some("B1"), "Saga", Some("1"))],
        );
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].total, r[0].owned),
            (2, 1),
            "the library book is a member and owned"
        );
    }

    #[test]
    fn the_same_asin_as_a_work_and_a_book_is_one_member() {
        let r = rollup(
            &[work("B1", "Saga")],
            &[lib_book(Some("B1"), "Saga", Some("1"))],
        );
        assert_eq!((r[0].total, r[0].owned), (1, 1));
    }

    #[test]
    fn a_library_series_with_no_known_works_still_gets_a_rollup() {
        let r = rollup(&[], &[lib_book(Some("B1"), "Unindexed Saga", Some("1"))]);
        assert_eq!(
            r.len(),
            1,
            "discovery never saw this series, the library did"
        );
        assert_eq!(r[0].name, "Unindexed Saga");
        assert_eq!((r[0].total, r[0].owned), (1, 1));
        assert_eq!(r[0].series_asin, None);
    }

    #[test]
    fn library_books_join_by_name_ignoring_case_and_spacing() {
        let r = rollup(
            &[work_at("W1", "A Song of Ice and Fire", "2")],
            &[lib_book(Some("B1"), "a song of  ice and fire ", Some("1"))],
        );
        assert_eq!(r.len(), 1, "one series, not two");
        assert_eq!(r[0].total, 2);
    }

    #[test]
    fn a_library_book_without_an_asin_is_its_own_member() {
        let r = rollup(
            &[],
            &[
                lib_book(None, "Saga", Some("1")),
                lib_book(None, "Saga", Some("2")),
            ],
        );
        assert_eq!((r[0].total, r[0].owned), (2, 2));
    }

    /// The case that exposed it, from production data on 2026-09-30: the works
    /// store knew two anthologies and not A Game of Thrones, and the rollup read
    /// `total 2, owned 1`.
    #[test]
    fn a_song_of_ice_and_fire_counts_the_owned_first_novel() {
        let asoiaf = "A Song of Ice and Fire";
        let works = [work("B00GXJN3U6", asoiaf), work("B0756MFZTR", asoiaf)];
        let books = [
            lib_book(Some("B002UZZ93G"), asoiaf, Some("1")), // A Game of Thrones
            lib_book(Some("B00GXJN3U6"), asoiaf, None),      // Dangerous Women
        ];
        let r = rollup(&works, &books);
        assert_eq!(r.len(), 1);
        // SKADI-T-0651 made A Game of Thrones count at all; SKADI-T-0654 stops the
        // two unpositioned anthologies counting towards completeness. Before both:
        // total 2, owned 1 — two anthologies. Now: one novel, owned, plus two
        // related.
        assert_eq!(
            (r[0].total, r[0].owned, r[0].related),
            (1, 1, 2),
            "A Game of Thrones counts; Dangerous Women and The Book of Swords are related"
        );
    }

    // --- SKADI-T-0654: unpositioned members are related, not counted ---

    #[test]
    fn positions_one_zero_and_fractional_count_null_and_blank_do_not() {
        let positions = [Some("1"), Some("0"), Some("1.5"), None, Some("  ")];
        let books: Vec<Book> = positions
            .iter()
            .enumerate()
            .map(|(i, p)| lib_book(Some(&format!("B{i}")), "Saga", *p))
            .collect();
        let r = rollup(&[], &books);
        assert_eq!(
            (r[0].total, r[0].related),
            (3, 2),
            "\"1\", \"0\" and \"1.5\" are positioned; null and blank are related"
        );
    }

    #[test]
    fn related_members_are_excluded_from_total_and_owned() {
        let r = rollup(
            &[
                work_at("W1", "Saga", "1"),
                work("W2", "Saga"),
                work("W3", "Saga"),
            ],
            &[lib_book(Some("W2"), "Saga", None)],
        );
        assert_eq!(r[0].total, 1, "only the positioned work");
        assert_eq!(
            r[0].owned, 0,
            "the owned book is related, so it is not counted"
        );
        assert_eq!(r[0].related, 2);
    }

    #[test]
    fn a_position_on_either_the_work_or_the_book_makes_a_member_positioned() {
        let r = rollup(
            &[work("B1", "Saga")],
            &[lib_book(Some("B1"), "Saga", Some("3"))],
        );
        assert_eq!((r[0].total, r[0].related), (1, 0));
    }

    #[test]
    fn acquire_seed_carries_asin_and_audiobook_category() {
        let book = Book::new(
            ExternalIds {
                asin: Some(AsinId("B08G9PRS1K".into())),
                ..Default::default()
            },
            "Project Hail Mary",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        let file = BookFile::missing(book.id);
        let seed = acquire_seed(&book, &file);
        assert_eq!(seed.request.kind, MediaKind::Audiobook);
        assert_eq!(
            seed.request.external_ids.asin,
            Some(AsinId("B08G9PRS1K".into()))
        );
        assert_eq!(
            seed.request.categories,
            crate::module::AUDIOBOOK_SEARCH_CATEGORIES.to_vec()
        );
        assert_eq!(seed.acquirable, file.acquirable_ref());
    }

    /// SKADI-T-0670: search on add carries the author alias (so `decide`'s
    /// author gate applies) and the series, exactly as the sweep does.
    #[test]
    fn acquire_seed_carries_the_author_and_series_like_the_sweep() {
        let mut book = Book::new(
            ExternalIds::default(),
            "Daemon",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        book.authors = vec!["Daniel Suarez".into()];
        book.series = Some(crate::author::SeriesLink {
            series_id: skadi_core::BookSeriesId::new(),
            name: "Daemon".into(),
            position: Some("1".into()),
        });
        let file = BookFile::missing(book.id);
        let seed = acquire_seed(&book, &file);
        assert_eq!(
            seed.request.titles,
            vec![
                "Daemon".to_string(),
                "Daemon Daniel Suarez".to_string(),
                "Daemon".to_string()
            ]
        );
        assert_eq!(seed.request.series.as_deref(), Some("Daemon"));
    }
}

/// Whether a maintenance pass writes (`?apply=true`) or only reports.
#[derive(Debug, serde::Deserialize)]
struct ApplyOpts {
    /// Defaults to **false**: a bare POST reports and changes nothing.
    ///
    /// A merge that deletes rows is not something to trigger by forgetting a
    /// query parameter, so the safe value is the one you get by saying nothing.
    #[serde(default)]
    apply: bool,
}

/// Fold duplicate `books` rows into one book with several editions
/// (SKADI-T-0562).
///
/// Lives on the daemon rather than in the CLI because the daemon owns the
/// database connection; in production that is Postgres, and a second writer
/// reaching in beside the running hunter is how a merge races an import.
async fn merge_editions(
    State(http): State<AudiobooksHttp>,
    axum::extract::Query(opts): axum::extract::Query<ApplyOpts>,
) -> Result<impl IntoResponse, ApiError> {
    let report = crate::maintenance::merge_book_editions(repo(&http), opts.apply).await?;
    Ok(axum::Json(serde_json::json!({
        "applied": opts.apply,
        "scanned": report.scanned,
        "groups": report.groups,
        "moved": report.moved,
        "removed": report.removed,
        "conflicted": report.conflicted,
        "candidates": report.candidates.iter().map(|c| serde_json::json!({
            "canonical": c.canonical.to_string(),
            "title": c.title,
            "duplicates": c.duplicates.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "moves": c.moves,
            "conflicts": c.conflicts,
        })).collect::<Vec<_>>(),
    })))
}
