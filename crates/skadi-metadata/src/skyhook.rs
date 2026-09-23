//! Keyless TV metadata via Sonarr's public Skyhook service
//! (`skyhook.sonarr.tv`, TheTVDB-backed) — SKADI-I-0037 / T-0266.
//!
//! The TV twin of [`crate::ServarrProvider`]: the same project-run, keyless
//! `*arr` metadata proxy pattern, but over TheTVDB for series. This is exactly
//! how Sonarr fetches series/season/episode metadata — keyed by `tvdbId`. The
//! same ADR ([[SKADI-A-0001]]) review triggers apply (private-project only;
//! honest `skadi/<version>` UA so Servarr can identify the traffic); the exit
//! ramp is the [`TmdbProvider`](crate::TmdbProvider) (`SKADI_TMDB_API_KEY`).
//!
//! Wire notes (verified live 2026-06-21): `GET /v1/tvdb/search/en?term=` returns
//! series candidates; `GET /v1/tvdb/shows/en/{tvdbId}` returns the full series
//! with `seasons[]` + **all** `episodes[]` in one response (no per-season fetch).
//! Fields are camelCase; unknown fields ignored so upstream additions don't break
//! us. Skyhook conveniently carries `tmdbId` + `imdbId` alongside `tvdbId`.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde::Deserialize;

use skadi_core::{AppError, ExternalIds, ImdbId, MediaKind, Result, TmdbId, TvdbId};
use skadi_http::HttpClient;

use crate::{
    EpisodeMeta, ExternalId, ImageKind, ImageRef, MetadataMatch, MetadataProvider, MetadataQuery,
    MetadataRecord, SeasonMeta, SeriesMetadata,
};

const DEFAULT_BASE_URL: &str = "https://skyhook.sonarr.tv/v1/tvdb";

/// The honest identity we present (ADR SKADI-A-0001 condition 1).
const USER_AGENT: &str = concat!("skadi/", env!("CARGO_PKG_VERSION"));

/// A keyless TV metadata provider over Sonarr's public Skyhook (TheTVDB).
pub struct SkyhookProvider {
    base_url: String,
    http: HttpClient,
}

impl SkyhookProvider {
    /// Construct against the public endpoint.
    #[must_use]
    pub fn new(http: HttpClient) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            http,
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

    /// Search for a series by term (`/search/en?term=`).
    pub async fn search_series(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        let url = self.url("/search/en");
        let term = query.title.clone();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url)
                    .header("User-Agent", USER_AGENT)
                    .query(&[("term", term.as_str())])
            })
            .await?;
        let body: Vec<Show> = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding skyhook search: {e}")))?;
        Ok(body.into_iter().map(Show::into_match).collect())
    }

    /// Look up a series by TVDB id (`/shows/en/{tvdbId}`) — returns the series +
    /// every season + every episode in one response.
    pub async fn lookup_series(&self, tvdb: TvdbId) -> Result<SeriesMetadata> {
        let url = self.url(&format!("/shows/en/{}", tvdb.0));
        let resp = self
            .http
            .send_idempotent(|c| c.get(&url).header("User-Agent", USER_AGENT))
            .await?;
        let show: Show = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding skyhook show: {e}")))?;
        Ok(show.into_series_metadata())
    }
}

#[async_trait]
impl MetadataProvider for SkyhookProvider {
    fn name(&self) -> &str {
        "skyhook"
    }

    fn supports(&self, kind: MediaKind) -> bool {
        matches!(kind, MediaKind::Series)
    }

    async fn search(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        self.search_series(query).await
    }

    /// The neutral `lookup` returns only the series-level record; callers wanting
    /// seasons/episodes use [`SkyhookProvider::lookup_series`].
    async fn lookup(&self, id: &ExternalId) -> Result<MetadataRecord> {
        let ExternalId::Tvdb(tvdb) = id else {
            return Err(AppError::Validation(
                "SkyhookProvider can only look up by a TVDB id".into(),
            ));
        };
        Ok(self.lookup_series(TvdbId(tvdb.0)).await?.record)
    }
}

#[async_trait]
impl crate::SeriesMetadataProvider for SkyhookProvider {
    async fn search_series(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        SkyhookProvider::search_series(self, query).await
    }
    async fn lookup_series(&self, tvdb: TvdbId) -> Result<SeriesMetadata> {
        SkyhookProvider::lookup_series(self, tvdb).await
    }
}

fn date_of(s: &Option<String>) -> Option<NaiveDate> {
    s.as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

fn year_of(s: &Option<String>) -> Option<u16> {
    s.as_deref()
        .filter(|s| s.len() >= 4)
        .and_then(|s| s[..4].parse().ok())
}

// --- Skyhook response shapes (camelCase; mapped to the neutral types) --------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Show {
    tvdb_id: u64,
    #[serde(default)]
    tmdb_id: Option<u64>,
    #[serde(default)]
    imdb_id: Option<String>,
    title: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    first_aired: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    runtime: Option<u32>,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    original_country: Option<String>,
    /// US TV parental guideline ("TV-14"); camelCase `contentRating` on the wire.
    #[serde(default)]
    content_rating: Option<String>,
    /// Non-empty when the series is mapped to AniList/MyAnimeList → it's anime.
    /// `IgnoredAny` so we only count entries, whatever their shape.
    #[serde(default)]
    ani_list_ids: Vec<serde::de::IgnoredAny>,
    #[serde(default)]
    mal_ids: Vec<serde::de::IgnoredAny>,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    seasons: Vec<ShowSeason>,
    #[serde(default)]
    episodes: Vec<ShowEpisode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Image {
    cover_type: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShowSeason {
    season_number: i32,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShowEpisode {
    season_number: i32,
    episode_number: i32,
    #[serde(default)]
    absolute_episode_number: Option<u32>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
}

impl Show {
    fn into_match(self) -> MetadataMatch {
        let poster_url = self
            .images
            .iter()
            .find(|i| i.cover_type.eq_ignore_ascii_case("poster"))
            .map(|i| i.url.clone());
        MetadataMatch {
            external_ids: ExternalIds {
                tvdb: Some(TvdbId(self.tvdb_id)),
                tmdb: self.tmdb_id.map(TmdbId),
                imdb: self.imdb_id.filter(|s| !s.is_empty()).map(ImdbId),
                ..Default::default()
            },
            title: self.title,
            year: year_of(&self.first_aired),
            score: 0.0,
            poster_url,
            overview: self.overview,
        }
    }

    fn into_series_metadata(self) -> SeriesMetadata {
        let images = self
            .images
            .iter()
            .filter_map(|i| match i.cover_type.to_ascii_lowercase().as_str() {
                "poster" => Some(ImageRef {
                    kind: ImageKind::Poster,
                    path: i.url.clone(),
                }),
                "fanart" => Some(ImageRef {
                    kind: ImageKind::Backdrop,
                    path: i.url.clone(),
                }),
                _ => None,
            })
            .collect();

        // Anime signal: an AniList/MAL mapping, or Animation + a Japanese origin.
        let is_anime = !self.ani_list_ids.is_empty()
            || !self.mal_ids.is_empty()
            || (self
                .genres
                .iter()
                .any(|g| g.eq_ignore_ascii_case("Animation"))
                && self.original_country.as_deref().is_some_and(|c| {
                    c.eq_ignore_ascii_case("jp") || c.eq_ignore_ascii_case("Japan")
                }));

        // Episode counts per season (episodes carry the truth; seasons[] is just labels).
        let mut seasons: Vec<SeasonMeta> = self
            .seasons
            .iter()
            .filter(|s| s.season_number >= 0)
            .map(|s| SeasonMeta {
                number: u16::try_from(s.season_number).unwrap_or(0),
                name: s.name.clone().filter(|n| !n.is_empty()),
                episode_count: 0,
                air_date: None,
            })
            .collect();

        let episodes: Vec<EpisodeMeta> = self
            .episodes
            .iter()
            .filter(|e| e.season_number >= 0)
            .map(|e| EpisodeMeta {
                season: u16::try_from(e.season_number).unwrap_or(0),
                number: u16::try_from(e.episode_number).unwrap_or(0),
                absolute: e.absolute_episode_number,
                title: e.title.clone().filter(|t| !t.is_empty()),
                air_date: date_of(&e.air_date),
                overview: e.overview.clone().filter(|o| !o.is_empty()),
            })
            .collect();

        // Roll the per-episode counts up into the seasons.
        for s in &mut seasons {
            let eps: Vec<&EpisodeMeta> = episodes.iter().filter(|e| e.season == s.number).collect();
            s.episode_count = u16::try_from(eps.len()).unwrap_or(u16::MAX);
            s.air_date = eps.iter().filter_map(|e| e.air_date).min();
        }

        SeriesMetadata {
            record: MetadataRecord {
                external_ids: ExternalIds {
                    tvdb: Some(TvdbId(self.tvdb_id)),
                    tmdb: self.tmdb_id.map(TmdbId),
                    imdb: self.imdb_id.filter(|s| !s.is_empty()).map(ImdbId),
                    ..Default::default()
                },
                title: self.title,
                original_title: None,
                overview: self.overview,
                runtime_minutes: self.runtime,
                release_date: date_of(&self.first_aired),
                images,
                genres: self.genres.clone(),
                content_rating: self.content_rating.clone().filter(|c| !c.trim().is_empty()),
                ..Default::default()
            },
            status: self.status,
            network: self.network,
            is_anime,
            seasons,
            episodes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider(base: String) -> SkyhookProvider {
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        SkyhookProvider::new(http).with_base_url(base)
    }

    #[tokio::test]
    async fn lookup_series_maps_show_seasons_and_episodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/shows/en/121361"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tvdbId": 121361, "tmdbId": 1399, "imdbId": "tt0944947",
                "title": "Game of Thrones", "firstAired": "2011-04-17",
                "status": "Ended", "network": "HBO", "runtime": 60,
                "genres": ["Drama", "Fantasy"], "contentRating": "TV-MA", "aniListIds": [], "malIds": [],
                "images": [
                    {"coverType": "Poster", "url": "https://x/poster.jpg"},
                    {"coverType": "Banner", "url": "https://x/banner.jpg"}
                ],
                "seasons": [
                    {"seasonNumber": 0}, {"seasonNumber": 1, "name": "Winter is Coming"}
                ],
                "episodes": [
                    {"seasonNumber": 1, "episodeNumber": 1, "title": "Winter Is Coming", "airDate": "2011-04-17"},
                    {"seasonNumber": 1, "episodeNumber": 2, "title": "The Kingsroad", "airDate": "2011-04-24"},
                    {"seasonNumber": 0, "episodeNumber": 1, "title": "Inside GoT", "airDate": "2010-12-05"}
                ]
            })))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let s = p.lookup_series(TvdbId(121361)).await.unwrap();
        assert_eq!(s.record.title, "Game of Thrones");
        assert_eq!(
            s.record.genres,
            vec!["Drama".to_string(), "Fantasy".to_string()]
        );
        assert_eq!(s.record.content_rating.as_deref(), Some("TV-MA"));
        assert_eq!(s.record.external_ids.tvdb.map(|t| t.0), Some(121361));
        assert_eq!(s.record.external_ids.tmdb.map(|t| t.0), Some(1399));
        assert_eq!(s.network.as_deref(), Some("HBO"));
        assert!(!s.is_anime);
        assert_eq!(s.record.images.len(), 1, "only Poster/Fanart kept");
        assert_eq!(s.seasons.len(), 2);
        let s1 = s.seasons.iter().find(|x| x.number == 1).unwrap();
        assert_eq!(s1.episode_count, 2, "S1 has 2 episodes");
        assert_eq!(s1.name.as_deref(), Some("Winter is Coming"));
        assert_eq!(s.episodes.len(), 3);
    }

    #[tokio::test]
    async fn anime_detected_from_anilist_mapping() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/shows/en/278157"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tvdbId": 278157, "title": "Frieren",
                "genres": ["Animation"], "originalCountry": "jp",
                "aniListIds": [154587], "malIds": [52991],
                "seasons": [{"seasonNumber": 1}],
                "episodes": [
                    {"seasonNumber": 1, "episodeNumber": 1, "absoluteEpisodeNumber": 1, "title": "The Journey's End"}
                ]
            })))
            .mount(&server)
            .await;
        let p = provider(server.uri());
        let s = p.lookup_series(TvdbId(278157)).await.unwrap();
        assert!(s.is_anime, "AniList mapping ⇒ anime");
        assert_eq!(s.episodes[0].absolute, Some(1));
    }

    #[tokio::test]
    async fn search_maps_tvdb_keyed_matches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/en"))
            .and(query_param("term", "severance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"tvdbId": 371980, "title": "Severance", "firstAired": "2022-02-18", "status": "Continuing"}
            ])))
            .mount(&server)
            .await;
        let p = provider(server.uri());
        let m = p
            .search(&MetadataQuery {
                title: "severance".into(),
                year: None,
                kind: MediaKind::Series,
            })
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].external_ids.tvdb.as_ref().map(|t| t.0), Some(371980));
        assert_eq!(m[0].year, Some(2022));
    }

    /// Live: hit the real keyless Skyhook (no key needed). Proves the full
    /// series→seasons→episodes fetch end-to-end.
    #[tokio::test]
    #[ignore = "hits the real skyhook.sonarr.tv (keyless; network)"]
    async fn live_skyhook_lookup() {
        let http = HttpClient::new(Duration::from_secs(20)).unwrap();
        let p = SkyhookProvider::new(http);
        let s = p.lookup_series(TvdbId(121361)).await.unwrap();
        println!(
            "\nseries: {} (aired {:?}) status={:?} network={:?} anime={} tmdb={:?} imdb={:?}",
            s.record.title,
            s.record.release_date,
            s.status,
            s.network,
            s.is_anime,
            s.record.external_ids.tmdb.map(|t| t.0),
            s.record.external_ids.imdb,
        );
        println!("seasons: {}", s.seasons.len());
        for season in &s.seasons {
            println!(
                "  S{:02} {:?} — {} eps",
                season.number, season.name, season.episode_count
            );
        }
        println!("episodes: {}", s.episodes.len());
        for e in s.episodes.iter().filter(|e| e.season == 1).take(4) {
            println!(
                "  S{:02}E{:02} {:?} ({:?})",
                e.season, e.number, e.title, e.air_date
            );
        }
        assert!(s.episodes.len() > 60, "GoT has 70+ episodes across seasons");
    }
}
