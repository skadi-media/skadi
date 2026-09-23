//! The Movie Database (TMDB) metadata provider.
//!
//! v3 JSON API via `skadi-http`. TMDB payloads are deserialized into private
//! response structs and mapped onto the provider-neutral [`MetadataRecord`] /
//! [`MetadataMatch`] — no TMDB shapes leak past this module. v0 looks up by
//! TMDB id (`/movie/{id}` with `append_to_response=external_ids`); other id
//! kinds are a follow-up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::NaiveDate;
use serde::Deserialize;

use skadi_core::{AppError, ExternalIds, ImdbId, MediaKind, Result, TmdbId};
use skadi_http::HttpClient;

use crate::{
    EpisodeMeta, ExternalId, ImageKind, ImageRef, MetadataMatch, MetadataProvider, MetadataQuery,
    MetadataRecord, SeasonMeta, SeriesMetadata,
};

const DEFAULT_BASE_URL: &str = "https://api.themoviedb.org/3";

/// TMDB serves image *paths* (e.g. `/abc.jpg`); the browser-loadable URL is the
/// image CDN base + a size + the path. We resolve here so `ImageRef.path` is
/// always an absolute URL regardless of provider (Servarr already returns full
/// URLs).
const TMDB_IMAGE_BASE: &str = "https://image.tmdb.org/t/p";

/// Resolve a TMDB image path to an absolute CDN URL at the given size. Passes
/// through anything that already looks absolute.
fn tmdb_image_url(path: &str, size: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{TMDB_IMAGE_BASE}/{size}{path}")
    }
}

/// A TMDB metadata provider.
pub struct TmdbProvider {
    base_url: String,
    api_key: String,
    http: HttpClient,
    /// Per-id lookup cache with a TTL (SKADI-T-0510).
    ///
    /// A refresh sweep, the add-item flow and an interactive lookup can all ask
    /// for the same id within seconds, and each was a full round trip against a
    /// rate-limited API for an answer that changes about as often as a film's
    /// release date does.
    ///
    /// A short TTL rather than a permanent cache: metadata *does* change — a
    /// poster is replaced, a runtime corrected — and the refresh worker exists to
    /// pick that up, so a cache that outlived the sweep interval would quietly
    /// defeat it.
    cache: Arc<Mutex<HashMap<String, (Instant, MetadataRecord)>>>,
}

/// How long a cached lookup stays fresh (SKADI-T-0510). Long enough to collapse
/// the burst of duplicate lookups one operation causes, far shorter than the
/// metadata refresh interval so it cannot mask a real update.
const CACHE_TTL: Duration = Duration::from_secs(300);

impl TmdbProvider {
    /// Construct with the default TMDB base URL.
    #[must_use]
    pub fn new(api_key: impl Into<String>, http: HttpClient) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: api_key.into(),
            http,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Override the base URL (used by tests against a fake server).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// A cached record for `key`, if one is still within [`CACHE_TTL`]
    /// (SKADI-T-0510).
    fn cached(&self, key: &str) -> Option<MetadataRecord> {
        // A poisoned lock would mean a panic while holding it; a cache is not
        // worth propagating that, so treat it as a miss.
        let guard = self.cache.lock().ok()?;
        let (at, record) = guard.get(key)?;
        (at.elapsed() < CACHE_TTL).then(|| record.clone())
    }

    fn remember(&self, key: &str, record: &MetadataRecord) {
        if let Ok(mut guard) = self.cache.lock() {
            // Drop anything already stale on the way past, so a long-lived
            // provider cannot accumulate every id it ever saw.
            guard.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
            guard.insert(key.to_string(), (Instant::now(), record.clone()));
        }
    }

    // --- TV (SKADI-I-0037 / T-0266) -----------------------------------------

    /// Search TMDB for a **series** (`/search/tv`).
    pub async fn search_series(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        let url = self.url("/search/tv");
        let api_key = self.api_key.clone();
        let title = query.title.clone();
        let year = query.year.map(|y| y.to_string());
        let resp = self
            .http
            .send_idempotent(|c| {
                let mut r = c
                    .get(&url)
                    .query(&[("api_key", api_key.as_str()), ("query", title.as_str())]);
                if let Some(y) = &year {
                    r = r.query(&[("first_air_date_year", y.as_str())]);
                }
                r
            })
            .await?;
        let body: TvSearchResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding TMDB tv search: {e}")))?;
        Ok(body
            .results
            .into_iter()
            .map(TvSearchResult::into_match)
            .collect())
    }

    /// How many season requests are in flight at once (SKADI-T-0514).
    ///
    /// Deliberately modest. The point is to stop a dozen-season refresh costing a
    /// dozen serial round trips, not to fetch every season at once: TMDB rate-limits,
    /// and a refresh sweep may already have several series in flight, so a wide
    /// fan-out here multiplies into a burst the provider will start 429ing.
    const SEASON_FETCH_CONCURRENCY: usize = 6;

    /// Look up a **series** by TMDB id and fetch every season's episodes
    /// (`/tv/{id}` + `/tv/{id}/season/{n}`), returning the neutral
    /// [`SeriesMetadata`].
    pub async fn lookup_series(&self, tmdb: TmdbId) -> Result<SeriesMetadata> {
        let url = self.url(&format!("/tv/{}", tmdb.0));
        let api_key = self.api_key.clone();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url).query(&[
                    ("api_key", api_key.as_str()),
                    ("append_to_response", "external_ids"),
                ])
            })
            .await?;
        let tv: Tv = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding TMDB tv: {e}")))?;

        let mut meta = tv.into_series_metadata();

        // Fetch episodes per season (skip nothing — specials included as season 0).
        //
        // Bounded-concurrent rather than sequential (SKADI-T-0514): a long-running
        // show is a dozen-plus round trips, and serialising them made a refresh
        // take as long as the slowest link times the season count. `buffered`
        // preserves input order, so `episodes` comes out in season order exactly as
        // the sequential loop produced it — the ordering is not incidental, the
        // callers index episodes by (season, number) but tests and diffs read the
        // list.
        use futures::stream::{StreamExt, TryStreamExt};
        let episodes: Vec<EpisodeMeta> = futures::stream::iter(meta.seasons.clone())
            .map(|season| {
                let s_url = self.url(&format!("/tv/{}/season/{}", tmdb.0, season.number));
                let api_key = self.api_key.clone();
                let http = &self.http;
                async move {
                    let resp = http
                        .send_idempotent(|c| c.get(&s_url).query(&[("api_key", api_key.as_str())]))
                        .await?;
                    let detail: SeasonDetail = resp
                        .json()
                        .await
                        .map_err(|e| AppError::Network(format!("decoding TMDB season: {e}")))?;
                    Ok::<_, AppError>(
                        detail
                            .episodes
                            .into_iter()
                            .map(|ep| ep.into_episode_meta(season.number))
                            .collect::<Vec<_>>(),
                    )
                }
            })
            .buffered(Self::SEASON_FETCH_CONCURRENCY)
            .try_concat()
            .await?;
        meta.episodes = episodes;
        Ok(meta)
    }
}

#[async_trait]
impl MetadataProvider for TmdbProvider {
    fn name(&self) -> &str {
        "tmdb"
    }

    fn supports(&self, kind: MediaKind) -> bool {
        matches!(kind, MediaKind::Movie | MediaKind::Series)
    }

    async fn search(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        let url = self.url("/search/movie");
        let api_key = self.api_key.clone();
        let title = query.title.clone();
        let year = query.year.map(|y| y.to_string());
        let resp = self
            .http
            .send_idempotent(|c| {
                let mut r = c
                    .get(&url)
                    .query(&[("api_key", api_key.as_str()), ("query", title.as_str())]);
                if let Some(y) = &year {
                    r = r.query(&[("year", y.as_str())]);
                }
                r
            })
            .await?;
        let body: SearchResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding TMDB search: {e}")))?;
        Ok(body
            .results
            .into_iter()
            .map(SearchResult::into_match)
            .collect())
    }

    async fn lookup(&self, id: &ExternalId) -> Result<MetadataRecord> {
        let ExternalId::Tmdb(tmdb_id) = id else {
            return Err(AppError::Validation(
                "TmdbProvider can only look up by a TMDB id (other id kinds: follow-up)".into(),
            ));
        };
        let url = self.url(&format!("/movie/{}", tmdb_id.0));
        // Serve a fresh cached record (SKADI-T-0510) before spending a request.
        if let Some(hit) = self.cached(&url) {
            return Ok(hit);
        }
        let api_key = self.api_key.clone();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url).query(&[
                    ("api_key", api_key.as_str()),
                    ("append_to_response", "external_ids,release_dates"),
                ])
            })
            .await?;
        let movie: Movie = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding TMDB movie: {e}")))?;
        let record = movie.into_record();
        self.remember(&url, &record);
        Ok(record)
    }
}

fn year_of(release_date: &Option<String>) -> Option<u16> {
    release_date
        .as_deref()
        .filter(|s| s.len() >= 4)
        .and_then(|s| s[..4].parse().ok())
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    id: u64,
    title: String,
    release_date: Option<String>,
    #[serde(default)]
    popularity: f32,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}

impl SearchResult {
    fn into_match(self) -> MetadataMatch {
        let year = year_of(&self.release_date);
        let poster_url = self
            .poster_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| tmdb_image_url(p, "w200"));
        MetadataMatch {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(self.id)),
                ..Default::default()
            },
            title: self.title,
            year,
            score: self.popularity,
            poster_url,
            overview: self.overview.filter(|s| !s.is_empty()),
        }
    }
}

#[derive(Deserialize)]
struct Movie {
    id: u64,
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    runtime: Option<u32>,
    release_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    external_ids: Option<MovieExternalIds>,
    /// TMDB returns this on the movie *detail* endpoint only, and null for a
    /// standalone film (SKADI-T-0581).
    belongs_to_collection: Option<Collection>,
    #[serde(default)]
    genres: Vec<NamedRef>,
    /// `append_to_response=release_dates` (SKADI-T-0610): certifications live
    /// per country, per release; we keep the US theatrical one.
    #[serde(default)]
    release_dates: Option<ReleaseDates>,
}

#[derive(Deserialize, Default)]
struct ReleaseDates {
    #[serde(default)]
    results: Vec<CountryReleases>,
}

#[derive(Deserialize)]
struct CountryReleases {
    iso_3166_1: String,
    #[serde(default)]
    release_dates: Vec<ReleaseEntry>,
}

#[derive(Deserialize)]
struct ReleaseEntry {
    #[serde(default)]
    certification: String,
    /// 1 premiere, 2 limited, 3 theatrical, 4 digital, 5 physical, 6 TV.
    #[serde(default, rename = "type")]
    kind: u8,
}

impl ReleaseDates {
    /// The US certification: theatrical first, then any US release that
    /// carries one. Empty strings (TMDB's "unknown") never count.
    fn us_certification(&self) -> Option<String> {
        let us = self.results.iter().find(|c| c.iso_3166_1 == "US")?;
        let rated = |e: &&ReleaseEntry| !e.certification.trim().is_empty();
        us.release_dates
            .iter()
            .filter(rated)
            .min_by_key(|e| match e.kind {
                3 => 0,
                2 => 1,
                1 => 2,
                _ => 3,
            })
            .map(|e| e.certification.trim().to_string())
    }
}

#[derive(Deserialize)]
struct Collection {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
struct MovieExternalIds {
    imdb_id: Option<String>,
}

impl Movie {
    fn into_record(self) -> MetadataRecord {
        let mut images = Vec::new();
        if let Some(p) = self.poster_path {
            images.push(ImageRef {
                kind: ImageKind::Poster,
                path: tmdb_image_url(&p, "w500"),
            });
        }
        if let Some(b) = self.backdrop_path {
            images.push(ImageRef {
                kind: ImageKind::Backdrop,
                path: tmdb_image_url(&b, "w780"),
            });
        }
        let imdb = self
            .external_ids
            .and_then(|e| e.imdb_id)
            .filter(|s| !s.is_empty())
            .map(ImdbId);
        let release_date = self
            .release_date
            .as_deref()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        let (collection_id, collection_name) = match self.belongs_to_collection {
            Some(c) => (Some(c.id as i64), Some(c.name)),
            None => (None, None),
        };
        let genres: Vec<String> = self.genres.into_iter().map(|g| g.name).collect();
        let content_rating = self
            .release_dates
            .as_ref()
            .and_then(ReleaseDates::us_certification)
            .filter(|c| !c.eq_ignore_ascii_case("NR") && !c.eq_ignore_ascii_case("Not Rated"));
        MetadataRecord {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(self.id)),
                imdb,
                ..Default::default()
            },
            content_rating,
            title: self.title,
            original_title: self.original_title,
            overview: self.overview,
            runtime_minutes: self.runtime,
            release_date,
            images,
            collection_id,
            collection_name,
            genres,
            ..Default::default()
        }
    }
}

// --- TV response shapes (private; mapped to the neutral types) --------------

fn date_of(s: &Option<String>) -> Option<NaiveDate> {
    s.as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

#[derive(Deserialize)]
struct TvSearchResponse {
    results: Vec<TvSearchResult>,
}

#[derive(Deserialize)]
struct TvSearchResult {
    id: u64,
    name: String,
    first_air_date: Option<String>,
    #[serde(default)]
    popularity: f32,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}

impl TvSearchResult {
    fn into_match(self) -> MetadataMatch {
        let poster_url = self
            .poster_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| tmdb_image_url(p, "w200"));
        MetadataMatch {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(self.id)),
                ..Default::default()
            },
            title: self.name,
            year: year_of(&self.first_air_date),
            score: self.popularity,
            poster_url,
            overview: self.overview.filter(|s| !s.is_empty()),
        }
    }
}

#[derive(Deserialize)]
struct Tv {
    id: u64,
    name: String,
    original_name: Option<String>,
    overview: Option<String>,
    first_air_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    status: Option<String>,
    #[serde(default)]
    episode_run_time: Vec<u32>,
    #[serde(default)]
    origin_country: Vec<String>,
    #[serde(default)]
    genres: Vec<NamedRef>,
    #[serde(default)]
    networks: Vec<NamedRef>,
    #[serde(default)]
    seasons: Vec<TvSeason>,
    external_ids: Option<TvExternalIds>,
}

#[derive(Deserialize)]
struct NamedRef {
    name: String,
}

#[derive(Deserialize)]
struct TvExternalIds {
    imdb_id: Option<String>,
    tvdb_id: Option<u64>,
}

#[derive(Deserialize)]
struct TvSeason {
    season_number: i32,
    #[serde(default)]
    episode_count: u32,
    name: Option<String>,
    air_date: Option<String>,
}

#[derive(Deserialize)]
struct SeasonDetail {
    #[allow(dead_code)]
    season_number: i32,
    #[serde(default)]
    episodes: Vec<TvEpisode>,
}

#[derive(Deserialize)]
struct TvEpisode {
    episode_number: i32,
    name: Option<String>,
    air_date: Option<String>,
    overview: Option<String>,
}

impl TvEpisode {
    fn into_episode_meta(self, season: u16) -> EpisodeMeta {
        EpisodeMeta {
            season,
            number: u16::try_from(self.episode_number).unwrap_or(0),
            absolute: None,
            title: self.name.filter(|s| !s.is_empty()),
            air_date: date_of(&self.air_date),
            overview: self.overview.filter(|s| !s.is_empty()),
        }
    }
}

impl Tv {
    fn into_series_metadata(self) -> SeriesMetadata {
        let mut images = Vec::new();
        if let Some(p) = self.poster_path {
            images.push(ImageRef {
                kind: ImageKind::Poster,
                path: tmdb_image_url(&p, "w500"),
            });
        }
        if let Some(b) = self.backdrop_path {
            images.push(ImageRef {
                kind: ImageKind::Backdrop,
                path: tmdb_image_url(&b, "w780"),
            });
        }
        let (imdb, tvdb) = match self.external_ids {
            Some(e) => (
                e.imdb_id.filter(|s| !s.is_empty()).map(ImdbId),
                e.tvdb_id.map(skadi_core::TvdbId),
            ),
            None => (None, None),
        };
        // Anime heuristic: Animation genre + a Japanese origin country.
        let is_anime = self
            .genres
            .iter()
            .any(|g| g.name.eq_ignore_ascii_case("Animation"))
            && self.origin_country.iter().any(|c| c == "JP");
        let genres: Vec<String> = self.genres.iter().map(|g| g.name.clone()).collect();
        let seasons = self
            .seasons
            .iter()
            .filter(|s| s.season_number >= 0)
            .map(|s| SeasonMeta {
                number: u16::try_from(s.season_number).unwrap_or(0),
                name: s.name.clone().filter(|n| !n.is_empty()),
                episode_count: u16::try_from(s.episode_count).unwrap_or(0),
                air_date: date_of(&s.air_date),
            })
            .collect();
        SeriesMetadata {
            record: MetadataRecord {
                external_ids: ExternalIds {
                    tmdb: Some(TmdbId(self.id)),
                    imdb,
                    tvdb,
                    ..Default::default()
                },
                title: self.name,
                original_title: self.original_name,
                overview: self.overview,
                runtime_minutes: self.episode_run_time.first().copied(),
                release_date: date_of(&self.first_air_date),
                images,
                genres,
                ..Default::default()
            },
            status: self.status,
            network: self.networks.into_iter().next().map(|n| n.name),
            is_anime,
            seasons,
            episodes: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider(base: String) -> TmdbProvider {
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        TmdbProvider::new("k", http).with_base_url(base)
    }

    #[test]
    fn year_parsing() {
        assert_eq!(year_of(&Some("1999-03-31".to_string())), Some(1999));
        assert_eq!(year_of(&Some("".to_string())), None);
        assert_eq!(year_of(&None), None);
    }

    #[tokio::test]
    async fn search_maps_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/movie"))
            .and(query_param("query", "The Matrix"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    {"id": 603, "title": "The Matrix", "release_date": "1999-03-31", "popularity": 88.5}
                ]
            })))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let matches = p
            .search(&MetadataQuery {
                title: "The Matrix".into(),
                year: Some(1999),
                kind: MediaKind::Movie,
            })
            .await
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].external_ids.tmdb, Some(TmdbId(603)));
        assert_eq!(matches[0].year, Some(1999));
        assert!((matches[0].score - 88.5).abs() < 0.01);
    }

    #[tokio::test]
    async fn lookup_maps_movie_with_external_ids() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movie/603"))
            .and(query_param("append_to_response", "external_ids,release_dates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 603,
                "title": "The Matrix",
                "original_title": "The Matrix",
                "overview": "A hacker discovers reality is a simulation.",
                "runtime": 136,
                "release_date": "1999-03-31",
                "poster_path": "/poster.jpg",
                "backdrop_path": "/backdrop.jpg",
                "external_ids": { "imdb_id": "tt0133093" },
                "release_dates": { "results": [
                    { "iso_3166_1": "GB", "release_dates": [ { "certification": "15", "type": 3 } ] },
                    { "iso_3166_1": "US", "release_dates": [
                        { "certification": "", "type": 1 },
                        { "certification": "R", "type": 3 },
                        { "certification": "NR", "type": 5 }
                    ] }
                ] }
            })))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let rec = p.lookup(&ExternalId::Tmdb(TmdbId(603))).await.unwrap();
        assert_eq!(rec.title, "The Matrix");
        assert_eq!(rec.runtime_minutes, Some(136));
        assert_eq!(rec.external_ids.imdb, Some(ImdbId("tt0133093".into())));
        assert_eq!(rec.release_date, NaiveDate::from_ymd_opt(1999, 3, 31));
        assert_eq!(rec.images.len(), 2);
        assert_eq!(
            rec.content_rating.as_deref(),
            Some("R"),
            "US theatrical certification"
        );
    }

    #[tokio::test]
    async fn lookup_by_non_tmdb_id_is_rejected() {
        let p = provider("http://unused".into());
        let err = p
            .lookup(&ExternalId::Imdb(ImdbId("tt0133093".into())))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[tokio::test]
    async fn lookup_series_fetches_seasons_and_episodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/tv/1399"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1399,
                "name": "Game of Thrones",
                "first_air_date": "2011-04-17",
                "status": "Ended",
                "episode_run_time": [60],
                "origin_country": ["US"],
                "genres": [{"name": "Sci-Fi & Fantasy"}],
                "networks": [{"name": "HBO"}],
                "external_ids": {"imdb_id": "tt0944947", "tvdb_id": 121361},
                "seasons": [
                    {"season_number": 0, "episode_count": 2, "name": "Specials", "air_date": null},
                    {"season_number": 1, "episode_count": 2, "name": "Season 1", "air_date": "2011-04-17"}
                ]
            })))
            .mount(&server)
            .await;
        for (s, n) in [(0, "Specials"), (1, "Season 1")] {
            Mock::given(method("GET"))
                .and(path(format!("/tv/1399/season/{s}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "season_number": s,
                    "episodes": [
                        {"episode_number": 1, "name": format!("{n} E1"), "air_date": "2011-04-17", "overview": "x"},
                        {"episode_number": 2, "name": format!("{n} E2"), "air_date": "2011-04-24", "overview": "y"}
                    ]
                })))
                .mount(&server)
                .await;
        }

        let p = provider(server.uri());
        let s = p.lookup_series(TmdbId(1399)).await.unwrap();
        assert_eq!(s.record.title, "Game of Thrones");
        assert_eq!(s.record.external_ids.tvdb.map(|t| t.0), Some(121361));
        assert_eq!(s.status.as_deref(), Some("Ended"));
        assert_eq!(s.network.as_deref(), Some("HBO"));
        assert!(!s.is_anime);
        assert_eq!(s.record.genres, vec!["Sci-Fi & Fantasy".to_string()]);
        assert_eq!(s.seasons.len(), 2);
        assert_eq!(s.episodes.len(), 4, "2 specials + 2 S1 episodes");
        // Season order is preserved across the concurrent fetch (SKADI-T-0514).
        // `buffered` yields in input order; `buffer_unordered` would not, and the
        // count assertion above would not have caught the difference.
        assert_eq!(
            s.episodes.iter().map(|e| e.season).collect::<Vec<_>>(),
            vec![0, 0, 1, 1],
            "seasons come back in order despite the concurrent fetch"
        );
        assert_eq!(s.episodes[0].title.as_deref(), Some("Specials E1"));
    }

    /// Live: hit the real TMDB TV API with the operator's key (read from env, set
    /// by `deploy/.env`). Proves the full series→seasons→episodes fetch end-to-end.
    #[tokio::test]
    #[ignore = "hits the real TMDB API; needs SKADI_TMDB_API_KEY"]
    async fn live_tmdb_tv_lookup() {
        let Ok(key) = std::env::var("SKADI_TMDB_API_KEY") else {
            eprintln!("SKADI_TMDB_API_KEY unset — skipping live TMDB test");
            return;
        };
        let http = HttpClient::new(Duration::from_secs(20)).unwrap();
        let p = TmdbProvider::new(key, http);

        let matches = p
            .search_series(&MetadataQuery {
                title: "Severance".into(),
                year: None,
                kind: MediaKind::Series,
            })
            .await
            .unwrap();
        println!("\nsearch 'Severance' → {} matches", matches.len());
        if let Some(m) = matches.first() {
            println!(
                "  top: {} ({:?}) tmdb={:?}",
                m.title, m.year, m.external_ids.tmdb
            );
        }

        let s = p.lookup_series(TmdbId(1399)).await.unwrap();
        println!(
            "\nseries: {} (first aired {:?}) status={:?} network={:?} anime={} imdb={:?} tvdb={:?}",
            s.record.title,
            s.record.release_date,
            s.status,
            s.network,
            s.is_anime,
            s.record.external_ids.imdb,
            s.record.external_ids.tvdb.map(|t| t.0),
        );
        println!("seasons: {}", s.seasons.len());
        for season in &s.seasons {
            println!(
                "  S{:02} {:?} — {} eps",
                season.number, season.name, season.episode_count
            );
        }
        println!("episodes fetched: {}", s.episodes.len());
        for e in s.episodes.iter().take(6) {
            println!(
                "  S{:02}E{:02} {:?} ({:?})",
                e.season, e.number, e.title, e.air_date
            );
        }
        assert!(!matches.is_empty(), "expected Severance search hits");
        assert!(!s.seasons.is_empty());
        assert!(s.episodes.len() > 60, "Game of Thrones has 70+ episodes");
    }
}
