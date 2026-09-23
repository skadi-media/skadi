//! `TvRepo` — CRUD for `series`, `seasons`, and `episodes` (SKADI-T-0265).
//!
//! Implemented on `skadi_store::Store` over the single
//! [`DualConnection`](skadi_store::DualConnection) path via [`Store::with_conn`]
//! (SKADI-I-0015), exactly like `MoviesRepo`. All ids are `TEXT`; `air_date` is a
//! `TEXT` ISO date. `Series` loaded via `get_series`/`list_series` come with their
//! `seasons` + `episodes` populated.

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::{Json, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use skadi_core::{
    AcquisitionStatus, AppError, EpisodeId, ExternalIds, FileRef, ImdbId, ProfileId, QualityId,
    Result, RootFolder, RootFolderId, SeasonId, SeriesId, TmdbId, TvdbId,
};
use skadi_importer::AcquirableRef;
use skadi_store::Store;

use crate::episode::{Episode, Season};
use crate::schema::{episodes, seasons, series};
use crate::series::{Series, SeriesType};

/// Filters for [`TvRepo::list_series`].
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SeriesFilter {
    /// When `Some`, only return series whose `monitored` flag matches.
    pub monitored: Option<bool>,
    /// Page size. `None` is unbounded, for callers that genuinely need every row
    /// (occupancy scans, sweeps). The HTTP list paths pass a bound
    /// (SKADI-T-0494).
    pub limit: Option<i64>,
    /// Rows to skip; ignored when `limit` is `None`.
    pub offset: Option<i64>,
}

/// CRUD over series, seasons, and episodes.
#[async_trait]
pub trait TvRepo: Send + Sync {
    // --- series ---
    async fn list_series(&self, filter: SeriesFilter) -> Result<Vec<Series>>;
    /// How many series match `filter`, ignoring `limit`/`offset` — the total a
    /// paged client needs (SKADI-T-0494).
    async fn count_series(&self, filter: SeriesFilter) -> Result<i64>;
    async fn get_series(&self, id: SeriesId) -> Result<Option<Series>>;
    async fn get_series_by_tvdb(&self, tvdb: TvdbId) -> Result<Option<Series>>;
    async fn upsert_series(&self, series: &Series) -> Result<()>;
    async fn delete_series(&self, id: SeriesId) -> Result<()>;

    // --- seasons ---
    async fn list_seasons(&self, series_id: SeriesId) -> Result<Vec<Season>>;
    async fn upsert_season(&self, season: &Season) -> Result<()>;
    async fn set_season_monitored(&self, id: SeasonId, monitored: bool) -> Result<()>;

    // --- episodes ---
    async fn list_episodes(&self, series_id: SeriesId) -> Result<Vec<Episode>>;
    async fn get_episode(&self, id: EpisodeId) -> Result<Option<Episode>>;
    async fn get_episode_by_ref(&self, r: &AcquirableRef) -> Result<Option<Episode>>;
    async fn upsert_episode(&self, episode: &Episode) -> Result<()>;
    async fn set_episode_status(&self, id: EpisodeId, status: AcquisitionStatus) -> Result<()>;
    async fn set_episode_monitored(&self, id: EpisodeId, monitored: bool) -> Result<()>;
    /// Delete an episode row outright (SKADI-T-0447). Used only by the refresh
    /// prune, and only for episodes the provider has dropped *and* that hold no
    /// file — a row with a file is kept and unmonitored instead, never deleted.
    async fn delete_episode(&self, id: EpisodeId) -> Result<()>;
}

// --- helpers ---------------------------------------------------------------

pub(crate) fn db_err(e: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("database error: {e}"))
}

fn parse_uuid(s: &str, ctx: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| AppError::Internal(format!("invalid {ctx} uuid: {e}")))
}

fn decode_acquirable_ref(r: &AcquirableRef) -> Result<EpisodeId> {
    Uuid::parse_str(&r.0)
        .map(EpisodeId::from)
        .map_err(|e| AppError::Validation(format!("invalid AcquirableRef for episode: {e}")))
}

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

fn u16c(v: i32) -> u16 {
    u16::try_from(v).unwrap_or(0)
}

// --- row models ------------------------------------------------------------

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = series)]
struct SeriesRow {
    id: String,
    tvdb_id: i64,
    tmdb_id: Option<i64>,
    imdb_id: Option<String>,
    title: String,
    year: Option<i32>,
    overview: Option<String>,
    status: Option<String>,
    network: Option<String>,
    runtime_minutes: Option<i32>,
    series_type: String,
    monitored: bool,
    profile_id: String,
    root_folder_id: String,
    root_folder_path: String,
    added_at: Timestamp,
    last_metadata_refresh: Option<Timestamp>,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
    /// JSON array of genre names; NULL before the first refresh (SKADI-T-0605).
    genres: Option<String>,
    /// US TV parental guideline; NULL before a refresh reports one (SKADI-T-0610).
    content_rating: Option<String>,
}

/// Genres travel as one JSON text column (see the movies repo for why).
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

impl TryFrom<SeriesRow> for Series {
    type Error = AppError;
    fn try_from(r: SeriesRow) -> Result<Self> {
        let tvdb_u: u64 = r
            .tvdb_id
            .try_into()
            .map_err(|_| AppError::Internal("tvdb_id is negative on disk".into()))?;
        Ok(Series {
            id: SeriesId::from(parse_uuid(&r.id, "series")?),
            external_ids: ExternalIds {
                tvdb: Some(TvdbId(tvdb_u)),
                tmdb: r.tmdb_id.and_then(|v| u64::try_from(v).ok()).map(TmdbId),
                imdb: r.imdb_id.map(ImdbId),
                ..Default::default()
            },
            title: r.title,
            year: r.year.and_then(|y| u16::try_from(y).ok()),
            overview: r.overview,
            status: r.status,
            network: r.network,
            runtime_minutes: r.runtime_minutes.and_then(|m| u32::try_from(m).ok()),
            series_type: SeriesType::from_str_lossy(&r.series_type),
            poster_url: r.poster_url,
            backdrop_url: r.backdrop_url,
            genres: decode_genres(r.genres.as_deref()),
            content_rating: r.content_rating,
            monitored: r.monitored,
            profile: ProfileId::from(parse_uuid(&r.profile_id, "profile")?),
            root_folder: RootFolder {
                id: RootFolderId::from(parse_uuid(&r.root_folder_id, "root_folder")?),
                path: PathBuf::from(r.root_folder_path),
            },
            added_at: r.added_at.0,
            last_metadata_refresh: r.last_metadata_refresh.map(|t| t.0),
            seasons: Vec::new(),
            episodes: Vec::new(),
        })
    }
}

fn series_to_row(s: &Series) -> Result<SeriesRow> {
    Ok(SeriesRow {
        id: s.id.to_string(),
        tvdb_id: i64::try_from(
            s.external_ids
                .tvdb
                .as_ref()
                .ok_or_else(|| AppError::Validation("series missing tvdb id".into()))?
                .0,
        )
        .map_err(|_| AppError::Internal("tvdb_id overflows i64".into()))?,
        tmdb_id: s
            .external_ids
            .tmdb
            .as_ref()
            .and_then(|v| i64::try_from(v.0).ok()),
        imdb_id: s.external_ids.imdb.as_ref().map(|i| i.0.clone()),
        title: s.title.clone(),
        year: s.year.map(i32::from),
        overview: s.overview.clone(),
        status: s.status.clone(),
        network: s.network.clone(),
        runtime_minutes: s.runtime_minutes.and_then(|v| i32::try_from(v).ok()),
        series_type: s.series_type.as_str().to_string(),
        monitored: s.monitored,
        profile_id: s.profile.to_string(),
        root_folder_id: s.root_folder.id.to_string(),
        root_folder_path: s.root_folder.path.to_string_lossy().into_owned(),
        added_at: Timestamp(s.added_at),
        last_metadata_refresh: s.last_metadata_refresh.map(Timestamp),
        poster_url: s.poster_url.clone(),
        backdrop_url: s.backdrop_url.clone(),
        genres: encode_genres(&s.genres),
        content_rating: s.content_rating.clone(),
    })
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = seasons)]
struct SeasonRow {
    id: String,
    series_id: String,
    number: i32,
    monitored: bool,
    episode_count: i32,
    aired_count: i32,
}

impl TryFrom<SeasonRow> for Season {
    type Error = AppError;
    fn try_from(r: SeasonRow) -> Result<Self> {
        Ok(Season {
            id: SeasonId::from(parse_uuid(&r.id, "season")?),
            series_id: SeriesId::from(parse_uuid(&r.series_id, "series")?),
            number: u16c(r.number),
            monitored: r.monitored,
            episode_count: u16c(r.episode_count),
            aired_count: u16c(r.aired_count),
        })
    }
}

fn season_to_row(s: &Season) -> SeasonRow {
    SeasonRow {
        id: s.id.to_string(),
        series_id: s.series_id.to_string(),
        number: i32::from(s.number),
        monitored: s.monitored,
        episode_count: i32::from(s.episode_count),
        aired_count: i32::from(s.aired_count),
    }
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = episodes)]
struct EpisodeRow {
    id: String,
    series_id: String,
    season: i32,
    number: i32,
    absolute_number: Option<i32>,
    scene_season: Option<i32>,
    scene_episode: Option<i32>,
    title: Option<String>,
    air_date: Option<String>,
    monitored: bool,
    status_json: Json<AcquisitionStatus>,
    status_kind: String,
    file_path: Option<String>,
    quality_id: Option<String>,
    format_score: i32,
    updated_at: Timestamp,
    /// Probed media-info as JSON (SKADI-T-0451).
    media_info: Option<String>,
}

impl TryFrom<EpisodeRow> for Episode {
    type Error = AppError;
    fn try_from(r: EpisodeRow) -> Result<Self> {
        Ok(Episode {
            id: EpisodeId::from(parse_uuid(&r.id, "episode")?),
            series_id: SeriesId::from(parse_uuid(&r.series_id, "series")?),
            season: u16c(r.season),
            number: u16c(r.number),
            absolute_number: r.absolute_number.and_then(|v| u32::try_from(v).ok()),
            scene_season: r.scene_season.map(u16c),
            scene_episode: r.scene_episode.map(u16c),
            title: r.title,
            air_date: r
                .air_date
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            monitored: r.monitored,
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
            // A row written before the probe, or by an older build, simply has
            // none; unparseable JSON is treated the same rather than failing the
            // whole episode load over a diagnostic field.
            media_info: r
                .media_info
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
        })
    }
}

fn episode_to_row(e: &Episode) -> EpisodeRow {
    EpisodeRow {
        id: e.id.to_string(),
        series_id: e.series_id.to_string(),
        season: i32::from(e.season),
        number: i32::from(e.number),
        absolute_number: e.absolute_number.and_then(|v| i32::try_from(v).ok()),
        scene_season: e.scene_season.map(i32::from),
        scene_episode: e.scene_episode.map(i32::from),
        title: e.title.clone(),
        air_date: e.air_date.map(|d| d.format("%Y-%m-%d").to_string()),
        monitored: e.monitored,
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

// --- impl TvRepo for Store -------------------------------------------------

#[async_trait]
impl TvRepo for Store {
    async fn count_series(&self, filter: SeriesFilter) -> Result<i64> {
        self.with_conn(move |conn| {
            let mut q = series::table.into_boxed();
            if let Some(m) = filter.monitored {
                q = q.filter(series::monitored.eq(m));
            }
            q.count().get_result::<i64>(conn).map_err(db_err)
        })
        .await
    }

    async fn list_series(&self, filter: SeriesFilter) -> Result<Vec<Series>> {
        // Filter and bound in SQL (SKADI-T-0494): `monitored` applied to the
        // loaded Vec would page over the wrong set once a limit is present.
        let out = self
            .with_conn(move |conn| {
                let mut q = series::table
                    .select(SeriesRow::as_select())
                    .order(series::added_at.asc())
                    .into_boxed();
                if let Some(m) = filter.monitored {
                    q = q.filter(series::monitored.eq(m));
                }
                if let Some(limit) = filter.limit {
                    q = q.limit(limit).offset(filter.offset.unwrap_or(0));
                }
                let rows: Vec<SeriesRow> = q.load(conn).map_err(db_err)?;
                rows.into_iter()
                    .map(Series::try_from)
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        let mut out = out;
        // Load the page's seasons and episodes in **two** queries rather than two
        // per series (SKADI-T-0494). The loop that was here issued 2N round trips
        // — 100 for a 50-series page — which is most of what kept a bounded
        // `/series` at 56 ms after the query bound had already done its work.
        //
        // Grouping is by series id in memory; the alternative, a join, would
        // multiply each series row by its episode count over the wire.
        let ids: Vec<String> = out.iter().map(|s| s.id.to_string()).collect();
        if !ids.is_empty() {
            let (seasons, episodes) = self
                .with_conn(move |conn| {
                    let seasons: Vec<SeasonRow> = seasons::table
                        .filter(seasons::series_id.eq_any(&ids))
                        .select(SeasonRow::as_select())
                        .order(seasons::number.asc())
                        .load(conn)
                        .map_err(db_err)?;
                    let episodes: Vec<EpisodeRow> = episodes::table
                        .filter(episodes::series_id.eq_any(&ids))
                        .select(EpisodeRow::as_select())
                        .order((episodes::season.asc(), episodes::number.asc()))
                        .load(conn)
                        .map_err(db_err)?;
                    Ok((seasons, episodes))
                })
                .await?;

            // The per-series order the single-series calls produce is preserved:
            // rows arrive already ordered and are appended in that order.
            let mut by_series_seasons: std::collections::HashMap<String, Vec<Season>> =
                std::collections::HashMap::new();
            for row in seasons {
                let key = row.series_id.clone();
                by_series_seasons
                    .entry(key)
                    .or_default()
                    .push(Season::try_from(row)?);
            }
            let mut by_series_eps: std::collections::HashMap<String, Vec<Episode>> =
                std::collections::HashMap::new();
            for row in episodes {
                let key = row.series_id.clone();
                by_series_eps
                    .entry(key)
                    .or_default()
                    .push(Episode::try_from(row)?);
            }
            for s in &mut out {
                let key = s.id.to_string();
                s.seasons = by_series_seasons.remove(&key).unwrap_or_default();
                s.episodes = by_series_eps.remove(&key).unwrap_or_default();
            }
        }
        Ok(out)
    }

    async fn get_series(&self, id: SeriesId) -> Result<Option<Series>> {
        let key = id.to_string();
        let s = self
            .with_conn(move |conn| {
                let row: Option<SeriesRow> = series::table
                    .find(key)
                    .select(SeriesRow::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                row.map(Series::try_from).transpose()
            })
            .await?;
        match s {
            None => Ok(None),
            Some(mut s) => {
                s.seasons = self.list_seasons(s.id).await?;
                s.episodes = self.list_episodes(s.id).await?;
                Ok(Some(s))
            }
        }
    }

    async fn get_series_by_tvdb(&self, tvdb: TvdbId) -> Result<Option<Series>> {
        let id_i64 =
            i64::try_from(tvdb.0).map_err(|_| AppError::Internal("tvdb overflow".into()))?;
        let found = self
            .with_conn(move |conn| {
                let row: Option<SeriesRow> = series::table
                    .filter(series::tvdb_id.eq(id_i64))
                    .select(SeriesRow::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                row.map(Series::try_from).transpose()
            })
            .await?;
        match found {
            None => Ok(None),
            Some(s) => self.get_series(s.id).await,
        }
    }

    async fn upsert_series(&self, s: &Series) -> Result<()> {
        let row = series_to_row(s)?;
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(series::table)
                        .values(&row)
                        .on_conflict(series::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sq| {
                    diesel::insert_into(series::table)
                        .values(&row)
                        .on_conflict(series::id)
                        .do_update()
                        .set(&row)
                        .execute(sq)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_series(&self, id: SeriesId) -> Result<()> {
        let key = id.to_string();
        // Delete children explicitly: SQLite doesn't enforce `ON DELETE CASCADE`
        // without the `foreign_keys` pragma, so don't rely on it (dual-backend).
        self.with_conn(move |conn| {
            diesel::delete(episodes::table.filter(episodes::series_id.eq(&key)))
                .execute(conn)
                .map_err(db_err)?;
            diesel::delete(seasons::table.filter(seasons::series_id.eq(&key)))
                .execute(conn)
                .map_err(db_err)?;
            diesel::delete(series::table.find(&key))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await?;
        // Destructive and previously invisible (SKADI-T-0456). Cascades to every
        // season and episode, so this is the one line explaining why a few hundred
        // rows disappeared at once.
        tracing::info!(series = %id, "series deleted (with its seasons and episodes)");
        Ok(())
    }

    async fn list_seasons(&self, series_id: SeriesId) -> Result<Vec<Season>> {
        let key = series_id.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<SeasonRow> = seasons::table
                .filter(seasons::series_id.eq(key))
                .select(SeasonRow::as_select())
                .order(seasons::number.asc())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(Season::try_from).collect()
        })
        .await
    }

    async fn upsert_season(&self, s: &Season) -> Result<()> {
        let row = season_to_row(s);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(seasons::table)
                        .values(&row)
                        .on_conflict(seasons::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sq| {
                    diesel::insert_into(seasons::table)
                        .values(&row)
                        .on_conflict(seasons::id)
                        .do_update()
                        .set(&row)
                        .execute(sq)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn set_season_monitored(&self, id: SeasonId, monitored: bool) -> Result<()> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::update(seasons::table.find(key))
                .set(seasons::monitored.eq(monitored))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn list_episodes(&self, series_id: SeriesId) -> Result<Vec<Episode>> {
        let key = series_id.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<EpisodeRow> = episodes::table
                .filter(episodes::series_id.eq(key))
                .select(EpisodeRow::as_select())
                .order((episodes::season.asc(), episodes::number.asc()))
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(Episode::try_from).collect()
        })
        .await
    }

    async fn get_episode(&self, id: EpisodeId) -> Result<Option<Episode>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<EpisodeRow> = episodes::table
                .find(key)
                .select(EpisodeRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(Episode::try_from).transpose()
        })
        .await
    }

    async fn get_episode_by_ref(&self, r: &AcquirableRef) -> Result<Option<Episode>> {
        self.get_episode(decode_acquirable_ref(r)?).await
    }

    async fn upsert_episode(&self, e: &Episode) -> Result<()> {
        let row = episode_to_row(e);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(episodes::table)
                        .values(&row)
                        .on_conflict(episodes::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sq| {
                    diesel::insert_into(episodes::table)
                        .values(&row)
                        .on_conflict(episodes::id)
                        .do_update()
                        .set(&row)
                        .execute(sq)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_episode(&self, id: EpisodeId) -> Result<()> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::delete(episodes::table.filter(episodes::id.eq(&key)))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn set_episode_status(&self, id: EpisodeId, status: AcquisitionStatus) -> Result<()> {
        let key = id.to_string();
        let status_kind = status_kind_of(&status).to_string();
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
            let affected = diesel::update(episodes::table.find(&key))
                .set((
                    episodes::status_json.eq(Json(status)),
                    episodes::status_kind.eq(status_kind),
                    episodes::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?;
            // A status write that matched no row is a silent no-op, and the
            // hunter cannot tell it from a successful one (SKADI-T-0455).
            // Report it; every caller already treats a status write as
            // best-effort, so this surfaces the case without failing a run.
            if affected == 0 {
                return Err(AppError::NotFound(format!("episode {key}")));
            }
            if let Some((fp, qid, fs)) = imported {
                diesel::update(episodes::table.find(&key))
                    .set((
                        episodes::file_path.eq(Some(fp)),
                        episodes::quality_id.eq(Some(qid)),
                        episodes::format_score.eq(fs),
                    ))
                    .execute(conn)
                    .map_err(db_err)?;
            }
            Ok(())
        })
        .await
    }

    async fn set_episode_monitored(&self, id: EpisodeId, monitored: bool) -> Result<()> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::update(episodes::table.find(key))
                .set(episodes::monitored.eq(monitored))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }
}
