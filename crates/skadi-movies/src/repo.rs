//! `MoviesRepo` — CRUD for `edition_kinds`, `movies`, and `movie_editions`.
//!
//! Implemented on `skadi_store::Store` over a single
//! [`DualConnection`](skadi_store::DualConnection) code path via
//! [`Store::with_conn`] (SKADI-I-0015) — the domain runs its own queries on the
//! shared dual-backend connection, no per-backend pool dispatch. Mirrors the
//! `DomainStateRepo` pattern from `skadi-store`. All ids are stored as `TEXT`.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use diesel::prelude::*;
use diesel_dualdb::types::{Json, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use skadi_core::{
    AcquisitionStatus, AppError, EditionKindId, ExternalIds, FileRef, ImdbId, MovieEditionId,
    MovieId, ProfileId, QualityId, Result, RootFolder, RootFolderId, TmdbId,
};
use skadi_importer::AcquirableRef;
use skadi_store::Store;

use crate::edition::{EditionKind, MovieEdition};
use crate::movie::Movie;
use crate::schema::{edition_kinds, movie_editions, movies};

// ---------------------------------------------------------------------------
// Public trait + filter types
// ---------------------------------------------------------------------------

/// Filters for [`MoviesRepo::list_movies`].
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct MovieFilter {
    /// When `Some`, only return movies whose `monitored` flag matches.
    pub monitored: Option<bool>,
    /// Page size. `None` means unbounded — kept for callers that genuinely need
    /// every row (occupancy scans, sweeps), but the HTTP list paths must pass a
    /// bound (SKADI-T-0494): loading 1,818 movies with their editions to render
    /// 50 of them cost the same as loading all of them, because the slice used to
    /// happen in the handler after the query.
    pub limit: Option<i64>,
    /// Rows to skip; ignored when `limit` is `None`.
    pub offset: Option<i64>,
}

/// CRUD over movies, editions, and the edition-kinds registry.
///
/// `Movie` values returned from `get_movie`/`list_movies` come with their
/// `editions` field populated; writes to editions go through the edition-level
/// methods (don't try to upsert editions via `upsert_movie`).
#[async_trait]
pub trait MoviesRepo: Send + Sync {
    // --- movies ---
    async fn list_movies(&self, filter: MovieFilter) -> Result<Vec<Movie>>;
    /// How many movies match `filter`, ignoring its `limit`/`offset`. A paged
    /// client needs the total to render page controls (SKADI-T-0494); counting in
    /// SQL keeps that from costing a full load.
    async fn count_movies(&self, filter: MovieFilter) -> Result<i64>;
    async fn get_movie(&self, id: MovieId) -> Result<Option<Movie>>;
    async fn get_movie_by_tmdb(&self, tmdb: TmdbId) -> Result<Option<Movie>>;
    async fn upsert_movie(&self, movie: &Movie) -> Result<()>;
    async fn delete_movie(&self, id: MovieId) -> Result<()>;

    // --- editions ---
    async fn list_editions(&self, movie_id: MovieId) -> Result<Vec<MovieEdition>>;
    async fn get_edition(&self, id: MovieEditionId) -> Result<Option<MovieEdition>>;
    async fn get_edition_by_ref(&self, r: &AcquirableRef) -> Result<Option<MovieEdition>>;
    async fn upsert_edition(&self, edition: &MovieEdition) -> Result<()>;
    async fn set_edition_status(&self, id: MovieEditionId, status: AcquisitionStatus)
    -> Result<()>;

    // --- edition kinds (registry) ---
    async fn list_edition_kinds(&self) -> Result<Vec<EditionKind>>;
    async fn get_edition_kind(&self, id: EditionKindId) -> Result<Option<EditionKind>>;
    async fn get_edition_kind_by_tag(&self, tag: &str) -> Result<Option<EditionKind>>;
    async fn upsert_edition_kind(&self, kind: &EditionKind) -> Result<()>;
    /// Refuses to delete `builtin = true` rows so the matcher always has a
    /// Theatrical fallback.
    async fn delete_edition_kind(&self, id: EditionKindId) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn db_err(e: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("database error: {e}"))
}

fn parse_uuid(s: &str, ctx: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| AppError::Internal(format!("invalid {ctx} uuid: {e}")))
}

fn decode_acquirable_ref(r: &AcquirableRef) -> Result<MovieEditionId> {
    Uuid::parse_str(&r.0)
        .map(MovieEditionId::from)
        .map_err(|e| AppError::Validation(format!("invalid AcquirableRef for movie edition: {e}")))
}

/// Discriminant tag for `AcquisitionStatus`, stored in `status_kind` for
/// indexable lookups. Must stay in sync with the variant names.
fn status_kind_of(s: &AcquisitionStatus) -> &'static str {
    match s {
        AcquisitionStatus::Missing => "Missing",
        AcquisitionStatus::Searching { .. } => "Searching",
        AcquisitionStatus::Snatched { .. } => "Snatched",
        AcquisitionStatus::Downloading { .. } => "Downloading",
        AcquisitionStatus::Imported { .. } => "Imported",
        AcquisitionStatus::Cutoff => "Cutoff",
        AcquisitionStatus::Failed { .. } => "Failed",
    }
}

// ---------------------------------------------------------------------------
// Row models — one set, portable column types (Json/Timestamp), TEXT ids
// ---------------------------------------------------------------------------

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = movies)]
struct MovieRow {
    id: String,
    tmdb_id: i64,
    imdb_id: Option<String>,
    title: String,
    original_title: Option<String>,
    year: Option<i32>,
    overview: Option<String>,
    runtime_minutes: Option<i32>,
    monitored: bool,
    profile_id: String,
    root_folder_id: String,
    root_folder_path: String,
    added_at: Timestamp,
    last_metadata_refresh: Option<Timestamp>,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
    collection_tmdb_id: Option<i64>,
    collection_name: Option<String>,
    /// JSON array of genre names; NULL before the first refresh (SKADI-T-0605).
    genres: Option<String>,
    /// US MPAA certification; NULL before a refresh reports one (SKADI-T-0610).
    content_rating: Option<String>,
}

/// Genres travel as one JSON text column: a list of a few short strings per
/// row that is only ever read whole, so a join table would buy nothing.
pub(crate) fn decode_genres(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

pub(crate) fn encode_genres(genres: &[String]) -> Option<String> {
    if genres.is_empty() {
        None
    } else {
        serde_json::to_string(genres).ok()
    }
}

impl TryFrom<MovieRow> for Movie {
    type Error = AppError;
    fn try_from(r: MovieRow) -> Result<Self> {
        let tmdb_u: u64 = r
            .tmdb_id
            .try_into()
            .map_err(|_| AppError::Internal("tmdb_id is negative on disk".into()))?;
        Ok(Movie {
            id: MovieId::from(parse_uuid(&r.id, "movie")?),
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(tmdb_u)),
                imdb: r.imdb_id.map(ImdbId),
                ..Default::default()
            },
            title: r.title,
            original_title: r.original_title,
            year: r.year.and_then(|y| u16::try_from(y).ok()),
            overview: r.overview,
            runtime_minutes: r.runtime_minutes.and_then(|m| u32::try_from(m).ok()),
            monitored: r.monitored,
            profile: ProfileId::from(parse_uuid(&r.profile_id, "profile")?),
            root_folder: RootFolder {
                id: RootFolderId::from(parse_uuid(&r.root_folder_id, "root_folder")?),
                path: PathBuf::from(r.root_folder_path),
            },
            added_at: r.added_at.0,
            last_metadata_refresh: r.last_metadata_refresh.map(|t| t.0),
            poster_url: r.poster_url,
            backdrop_url: r.backdrop_url,
            // Both columns or neither: a name without an id cannot be grouped
            // on, and an id without a name cannot be labelled.
            collection: match (r.collection_tmdb_id, r.collection_name) {
                (Some(tmdb_id), Some(name)) => Some(crate::MovieCollection { tmdb_id, name }),
                _ => None,
            },
            genres: decode_genres(r.genres.as_deref()),
            content_rating: r.content_rating,
            editions: Vec::new(),
        })
    }
}

fn movie_to_row(m: &Movie) -> Result<MovieRow> {
    Ok(MovieRow {
        id: m.id.to_string(),
        tmdb_id: i64::try_from(
            m.external_ids
                .tmdb
                .as_ref()
                .ok_or_else(|| AppError::Validation("movie missing tmdb id".into()))?
                .0,
        )
        .map_err(|_| AppError::Internal("tmdb_id overflows i64".into()))?,
        imdb_id: m.external_ids.imdb.as_ref().map(|i| i.0.clone()),
        title: m.title.clone(),
        original_title: m.original_title.clone(),
        year: m.year.map(i32::from),
        overview: m.overview.clone(),
        runtime_minutes: m.runtime_minutes.and_then(|v| i32::try_from(v).ok()),
        monitored: m.monitored,
        profile_id: m.profile.to_string(),
        root_folder_id: m.root_folder.id.to_string(),
        root_folder_path: m.root_folder.path.to_string_lossy().into_owned(),
        added_at: Timestamp(m.added_at),
        last_metadata_refresh: m.last_metadata_refresh.map(Timestamp),
        poster_url: m.poster_url.clone(),
        backdrop_url: m.backdrop_url.clone(),
        collection_tmdb_id: m.collection.as_ref().map(|c| c.tmdb_id),
        collection_name: m.collection.as_ref().map(|c| c.name.clone()),
        genres: encode_genres(&m.genres),
        content_rating: m.content_rating.clone(),
    })
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = movie_editions)]
struct EditionRow {
    id: String,
    movie_id: String,
    kind_id: String,
    status_json: Json<AcquisitionStatus>,
    status_kind: String,
    file_path: Option<String>,
    quality_id: Option<String>,
    format_score: i32,
    updated_at: Timestamp,
    /// Probed media-info as JSON (SKADI-T-0236); `None` until the post-import probe.
    media_info: Option<String>,
}

impl TryFrom<EditionRow> for MovieEdition {
    type Error = AppError;
    fn try_from(r: EditionRow) -> Result<Self> {
        Ok(MovieEdition {
            id: MovieEditionId::from(parse_uuid(&r.id, "edition")?),
            movie_id: MovieId::from(parse_uuid(&r.movie_id, "movie")?),
            kind: EditionKindId::from(parse_uuid(&r.kind_id, "edition_kind")?),
            status: r.status_json.0,
            file: r.file_path.map(|p| FileRef {
                path: PathBuf::from(p),
            }),
            quality: r
                .quality_id
                .map(|q| parse_uuid(&q, "quality").map(QualityId::from))
                .transpose()?,
            format_score: r.format_score,
            updated_at: r.updated_at.0,
            // Malformed stored JSON degrades to "not probed" rather than erroring a load.
            media_info: r
                .media_info
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
        })
    }
}

fn edition_to_row(e: &MovieEdition) -> EditionRow {
    EditionRow {
        id: e.id.to_string(),
        movie_id: e.movie_id.to_string(),
        kind_id: e.kind.to_string(),
        status_json: Json(e.status.clone()),
        status_kind: status_kind_of(&e.status).to_string(),
        file_path: e
            .file
            .as_ref()
            .map(|f| f.path.to_string_lossy().into_owned()),
        quality_id: e.quality.map(|q| q.to_string()),
        format_score: e.format_score,
        updated_at: Timestamp(e.updated_at),
        media_info: e
            .media_info
            .as_ref()
            .and_then(|m| serde_json::to_string(m).ok()),
    }
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = edition_kinds)]
struct KindRow {
    id: String,
    name: String,
    normalized_tag: String,
    match_patterns: Json<Vec<String>>,
    builtin: bool,
}

impl From<KindRow> for EditionKind {
    fn from(r: KindRow) -> Self {
        EditionKind {
            id: EditionKindId::from(Uuid::parse_str(&r.id).unwrap_or_default()),
            name: r.name,
            normalized_tag: r.normalized_tag,
            match_patterns: r.match_patterns.0,
            builtin: r.builtin,
        }
    }
}

fn kind_to_row(k: &EditionKind) -> KindRow {
    KindRow {
        id: k.id.to_string(),
        name: k.name.clone(),
        normalized_tag: k.normalized_tag.clone(),
        match_patterns: Json(k.match_patterns.clone()),
        builtin: k.builtin,
    }
}

// ---------------------------------------------------------------------------
// impl MoviesRepo for Store — single DualConnection path
// ---------------------------------------------------------------------------

#[async_trait]
impl MoviesRepo for Store {
    async fn count_movies(&self, filter: MovieFilter) -> Result<i64> {
        self.with_conn(move |conn| {
            let mut q = movies::table.into_boxed();
            if let Some(m) = filter.monitored {
                q = q.filter(movies::monitored.eq(m));
            }
            q.count().get_result::<i64>(conn).map_err(db_err)
        })
        .await
    }

    async fn list_movies(&self, filter: MovieFilter) -> Result<Vec<Movie>> {
        // Both the `monitored` filter and the page bound belong in SQL
        // (SKADI-T-0494). `monitored` used to be applied to the loaded Vec, which
        // is not just wasteful — combined with a limit it would page over the
        // wrong set.
        let out = self
            .with_conn(move |conn| {
                let mut q = movies::table
                    .select(MovieRow::as_select())
                    .order(movies::added_at.asc())
                    .into_boxed();
                if let Some(m) = filter.monitored {
                    q = q.filter(movies::monitored.eq(m));
                }
                if let Some(limit) = filter.limit {
                    q = q.limit(limit).offset(filter.offset.unwrap_or(0));
                }
                let rows: Vec<MovieRow> = q.load(conn).map_err(db_err)?;
                rows.into_iter()
                    .map(Movie::try_from)
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        let mut out = out;
        if !out.is_empty() {
            // Batch-load every edition for these movies in ONE query, converting
            // rows exactly as `list_editions` does (same `updated_at.asc()`
            // ordering), grouped by movie id — a CONSTANT number of queries
            // regardless of movie count.
            let ids: Vec<String> = out.iter().map(|mv| mv.id.to_string()).collect();
            let mut editions_by_movie: HashMap<MovieId, Vec<MovieEdition>> = self
                .with_conn(move |conn| {
                    let rows: Vec<EditionRow> = movie_editions::table
                        .filter(movie_editions::movie_id.eq_any(&ids))
                        .select(EditionRow::as_select())
                        .order(movie_editions::updated_at.asc())
                        .load(conn)
                        .map_err(db_err)?;
                    let mut map: HashMap<MovieId, Vec<MovieEdition>> = HashMap::new();
                    for row in rows {
                        let e = MovieEdition::try_from(row)?;
                        map.entry(e.movie_id).or_default().push(e);
                    }
                    Ok(map)
                })
                .await?;
            for mv in &mut out {
                mv.editions = editions_by_movie.remove(&mv.id).unwrap_or_default();
            }
        }
        Ok(out)
    }

    async fn get_movie(&self, id: MovieId) -> Result<Option<Movie>> {
        let key = id.to_string();
        let movie = self
            .with_conn(move |conn| {
                let row: Option<MovieRow> = movies::table
                    .find(key)
                    .select(MovieRow::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                row.map(Movie::try_from).transpose()
            })
            .await?;
        match movie {
            None => Ok(None),
            Some(mut m) => {
                m.editions = self.list_editions(m.id).await?;
                Ok(Some(m))
            }
        }
    }

    async fn get_movie_by_tmdb(&self, tmdb: TmdbId) -> Result<Option<Movie>> {
        let id_i64 =
            i64::try_from(tmdb.0).map_err(|_| AppError::Internal("tmdb overflow".into()))?;
        let movie = self
            .with_conn(move |conn| {
                let row: Option<MovieRow> = movies::table
                    .filter(movies::tmdb_id.eq(id_i64))
                    .select(MovieRow::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                row.map(Movie::try_from).transpose()
            })
            .await?;
        match movie {
            None => Ok(None),
            Some(mut m) => {
                m.editions = self.list_editions(m.id).await?;
                Ok(Some(m))
            }
        }
    }

    async fn upsert_movie(&self, movie: &Movie) -> Result<()> {
        let row = movie_to_row(movie)?;
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(movies::table)
                        .values(&row)
                        .on_conflict(movies::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(movies::table)
                        .values(&row)
                        .on_conflict(movies::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_movie(&self, id: MovieId) -> Result<()> {
        let key = id.to_string();
        let n = self
            .with_conn(move |conn| {
                diesel::delete(movies::table.find(key))
                    .execute(conn)
                    .map_err(db_err)
            })
            .await?;
        // Destructive and previously invisible (SKADI-T-0456): a movie vanishing
        // from the library left no trace of who removed it or when. INFO, because
        // this is an operator action, not routine churn.
        tracing::info!(movie = %id, rows = n, "movie deleted");
        Ok(())
    }

    async fn list_editions(&self, movie_id: MovieId) -> Result<Vec<MovieEdition>> {
        let key = movie_id.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<EditionRow> = movie_editions::table
                .filter(movie_editions::movie_id.eq(key))
                .select(EditionRow::as_select())
                .order(movie_editions::updated_at.asc())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(MovieEdition::try_from).collect()
        })
        .await
    }

    async fn get_edition(&self, id: MovieEditionId) -> Result<Option<MovieEdition>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<EditionRow> = movie_editions::table
                .find(key)
                .select(EditionRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(MovieEdition::try_from).transpose()
        })
        .await
    }

    async fn get_edition_by_ref(&self, r: &AcquirableRef) -> Result<Option<MovieEdition>> {
        let id = decode_acquirable_ref(r)?;
        self.get_edition(id).await
    }

    async fn upsert_edition(&self, edition: &MovieEdition) -> Result<()> {
        let row = edition_to_row(edition);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(movie_editions::table)
                        .values(&row)
                        .on_conflict(movie_editions::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(movie_editions::table)
                        .values(&row)
                        .on_conflict(movie_editions::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn set_edition_status(
        &self,
        id: MovieEditionId,
        status: AcquisitionStatus,
    ) -> Result<()> {
        let key = id.to_string();
        let status_kind = status_kind_of(&status).to_string();
        // When Imported, also reflect file/quality/score so callers can use
        // set_edition_status as a thin one-call update.
        let imported = match &status {
            AcquisitionStatus::Imported {
                file,
                quality,
                score,
                ..
            } => Some((
                file.path.to_string_lossy().into_owned(),
                quality.to_string(),
                *score,
            )),
            _ => None,
        };
        self.with_conn(move |conn| {
            let affected = diesel::update(movie_editions::table.find(&key))
                .set((
                    movie_editions::status_json.eq(Json(status)),
                    movie_editions::status_kind.eq(status_kind),
                    movie_editions::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?;
            // A status write that matched no row is a silent no-op, and the
            // hunter cannot tell it from a successful one (SKADI-T-0455).
            // Report it; every caller already treats a status write as
            // best-effort, so this surfaces the case without failing a run.
            if affected == 0 {
                return Err(AppError::NotFound(format!("movie edition {key}")));
            }
            if let Some((fp, qid, fs)) = imported {
                diesel::update(movie_editions::table.find(&key))
                    .set((
                        movie_editions::file_path.eq(Some(fp)),
                        movie_editions::quality_id.eq(Some(qid)),
                        movie_editions::format_score.eq(fs),
                    ))
                    .execute(conn)
                    .map_err(db_err)?;
            }
            Ok(())
        })
        .await
    }

    async fn list_edition_kinds(&self) -> Result<Vec<EditionKind>> {
        self.with_conn(|conn| {
            let rows: Vec<KindRow> = edition_kinds::table
                .select(KindRow::as_select())
                .order(edition_kinds::name.asc())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(EditionKind::from).collect())
        })
        .await
    }

    async fn get_edition_kind(&self, id: EditionKindId) -> Result<Option<EditionKind>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<KindRow> = edition_kinds::table
                .find(key)
                .select(KindRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(EditionKind::from))
        })
        .await
    }

    async fn get_edition_kind_by_tag(&self, tag: &str) -> Result<Option<EditionKind>> {
        let tag = tag.to_string();
        self.with_conn(move |conn| {
            let row: Option<KindRow> = edition_kinds::table
                .filter(edition_kinds::normalized_tag.eq(tag))
                .select(KindRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(EditionKind::from))
        })
        .await
    }

    async fn upsert_edition_kind(&self, kind: &EditionKind) -> Result<()> {
        // Validate before writing (SKADI-T-0452, REQ-UNITS.11). There is no
        // UNIQUE on `normalized_tag` in the schema, and the tag is what goes into
        // an import path — two kinds sharing one tag means two different editions
        // resolving to the same folder, which is a data-loss shape, not a
        // cosmetic clash. An empty name gives the UI a blank row it cannot label.
        if kind.name.trim().is_empty() {
            return Err(AppError::Validation(
                "edition kind: name must not be empty".into(),
            ));
        }
        if kind.normalized_tag.trim().is_empty() {
            return Err(AppError::Validation(
                "edition kind: normalized_tag must not be empty".into(),
            ));
        }
        // Case-insensitive: the tag names a folder, and two tags differing only
        // in case collide on a case-insensitive filesystem (macOS, SMB).
        let tag = kind.normalized_tag.trim().to_lowercase();
        let clash = self
            .list_edition_kinds()
            .await?
            .into_iter()
            .find(|k| k.id != kind.id && k.normalized_tag.trim().to_lowercase() == tag);
        if let Some(clash) = clash {
            return Err(AppError::Validation(format!(
                "edition kind: normalized_tag {:?} is already used by {:?}",
                kind.normalized_tag, clash.name
            )));
        }
        let row = kind_to_row(kind);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(edition_kinds::table)
                        .values(&row)
                        .on_conflict(edition_kinds::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(edition_kinds::table)
                        .values(&row)
                        .on_conflict(edition_kinds::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await?;
        // Registry mutation (SKADI-T-0456). The tag goes into an import path, so
        // a change here silently relocates where future editions land — worth a
        // line naming the tag, not just the id.
        tracing::info!(
            kind = %kind.id,
            name = %kind.name,
            tag = %kind.normalized_tag,
            "edition kind saved"
        );
        Ok(())
    }

    async fn delete_edition_kind(&self, id: EditionKindId) -> Result<()> {
        // Refuse on builtin rows — the matcher's Theatrical fallback depends on
        // them surviving forever.
        if let Some(existing) = self.get_edition_kind(id).await?
            && existing.builtin
        {
            return Err(AppError::Validation(format!(
                "cannot delete built-in edition kind '{}'",
                existing.name
            )));
        }
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::delete(edition_kinds::table.find(key))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await?;
        tracing::info!(kind = %id, "edition kind deleted");
        Ok(())
    }
}
