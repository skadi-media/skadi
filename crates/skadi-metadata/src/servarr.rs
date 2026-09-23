//! Keyless metadata via Servarr's public movie-metadata service
//! (`api.radarr.video`) — SKADI-T-0063.
//!
//! **Read [[SKADI-A-0001]] before touching this.** This leans on Servarr's
//! project-run infrastructure with no third-party ToS/SLA, accepted only while
//! Skadi is a private personal project. The ADR's review triggers are binding:
//! publication of this project in any form requires asking the Servarr team
//! first; if they object or the endpoint breaks, the exit ramp is the
//! [`TmdbProvider`](crate::TmdbProvider) (set `SKADI_TMDB_API_KEY`).
//!
//! Per the ADR, every request carries an honest `skadi/<version>` User-Agent —
//! never masquerading as Radarr — so Servarr can identify (and, if they wish,
//! block) this traffic.
//!
//! Wire notes (verified live 2026-06-04): `GET /v1/movie/{tmdbid}`,
//! `GET /v1/movie/imdb/{imdbid}`, and `GET /v1/search?q=&year=` all answer
//! keylessly; search items are full movie records, so one response struct
//! serves everything. Fields are PascalCase; unknown fields are ignored so
//! upstream additions don't break us.

use async_trait::async_trait;
use serde::Deserialize;

use skadi_core::{AppError, ExternalIds, ImdbId, MediaKind, Result, TmdbId};
use skadi_http::HttpClient;

use crate::{
    ExternalId, ImageKind, ImageRef, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord,
};

const DEFAULT_BASE_URL: &str = "https://api.radarr.video/v1";

/// The honest identity we present (ADR SKADI-A-0001 condition 1).
const USER_AGENT: &str = concat!("skadi/", env!("CARGO_PKG_VERSION"));

/// A keyless metadata provider over Servarr's public Radarr metadata API.
pub struct ServarrProvider {
    base_url: String,
    http: HttpClient,
}

impl ServarrProvider {
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
}

#[async_trait]
impl MetadataProvider for ServarrProvider {
    fn name(&self) -> &str {
        "servarr"
    }

    fn supports(&self, kind: MediaKind) -> bool {
        matches!(kind, MediaKind::Movie)
    }

    async fn search(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        let url = self.url("/search");
        let q = query.title.clone();
        let year = query.year.map(|y| y.to_string());
        let resp = self
            .http
            .send_idempotent(|c| {
                let mut r = c
                    .get(&url)
                    .header("User-Agent", USER_AGENT)
                    .query(&[("q", q.as_str())]);
                if let Some(y) = &year {
                    r = r.query(&[("year", y.as_str())]);
                }
                r
            })
            .await?;
        let body: Vec<MovieResource> = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding servarr search: {e}")))?;
        Ok(body.into_iter().map(MovieResource::into_match).collect())
    }

    async fn lookup(&self, id: &ExternalId) -> Result<MetadataRecord> {
        let url = match id {
            ExternalId::Tmdb(tmdb) => self.url(&format!("/movie/{}", tmdb.0)),
            ExternalId::Imdb(imdb) => self.url(&format!("/movie/imdb/{}", imdb.0)),
            other => {
                return Err(AppError::Validation(format!(
                    "ServarrProvider cannot look up by {other:?} (tmdb/imdb only)"
                )));
            }
        };
        let resp = self
            .http
            .send_idempotent(|c| c.get(&url).header("User-Agent", USER_AGENT))
            .await?;
        let movie: MovieResource = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding servarr movie: {e}")))?;
        Ok(movie.into_record())
    }
}

/// The subset of Servarr's movie resource we consume. PascalCase on the wire;
/// everything optional-where-possible and unknown fields ignored, so upstream
/// schema drift degrades gracefully instead of erroring.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MovieResource {
    tmdb_id: u64,
    imdb_id: Option<String>,
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    runtime: Option<u32>,
    year: Option<u16>,
    #[serde(default)]
    popularity: f32,
    /// Theatrical premiere (RFC3339); preferred for `release_date`.
    in_cinema: Option<String>,
    /// Festival/first premiere; fallback when no theatrical date.
    premier: Option<String>,
    physical_release: Option<String>,
    #[serde(default)]
    images: Vec<Image>,
    /// Franchise membership (SKADI-T-0581). Null for a standalone film.
    ///
    /// This is the path that actually runs in the default deployment: without
    /// `SKADI_TMDB_API_KEY` skadi uses the keyless Servarr proxy, so parsing
    /// this only in the TMDB provider would ship a feature that never populates.
    collection: Option<CollectionResource>,
    /// Genre names as Servarr reports them ("Action", "Science Fiction").
    #[serde(default)]
    genres: Vec<String>,
    /// US MPAA certification ("R", "PG-13", "Not Rated"); PascalCase on the wire.
    /// Radarr's own resource carries this; the public metadata proxy does not.
    #[serde(default)]
    certification: Option<String>,
    /// What api.radarr.video actually sends (SKADI-T-0610): one entry per
    /// country. The US one is the MPAA label the household policy reads.
    #[serde(default)]
    certifications: Vec<CountryCertification>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CountryCertification {
    #[serde(default)]
    country: String,
    #[serde(default)]
    certification: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CollectionResource {
    tmdb_id: i64,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Image {
    cover_type: String,
    url: String,
}

/// Parse the date part out of an RFC3339-ish timestamp (`1999-03-31T00:00:00Z`).
fn date_of(s: &Option<String>) -> Option<chrono::NaiveDate> {
    s.as_deref()
        .and_then(|s| s.get(..10))
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
}

impl MovieResource {
    fn external_ids(&self) -> ExternalIds {
        ExternalIds {
            tmdb: Some(TmdbId(self.tmdb_id)),
            imdb: self.imdb_id.clone().map(ImdbId),
            ..Default::default()
        }
    }

    fn into_match(self) -> MetadataMatch {
        let poster_url = self
            .images
            .iter()
            .find(|i| i.cover_type == "Poster")
            .map(|i| i.url.clone());
        MetadataMatch {
            external_ids: self.external_ids(),
            title: self.title,
            year: self.year,
            score: self.popularity,
            poster_url,
            overview: self.overview,
        }
    }

    fn into_record(self) -> MetadataRecord {
        // Release-date precedence per the task notes: InCinema → Premier →
        // PhysicalRelease.
        let release_date = date_of(&self.in_cinema)
            .or_else(|| date_of(&self.premier))
            .or_else(|| date_of(&self.physical_release));
        let images = self
            .images
            .iter()
            .filter_map(|i| {
                let kind = match i.cover_type.as_str() {
                    "Poster" => ImageKind::Poster,
                    "Fanart" => ImageKind::Backdrop,
                    _ => return None,
                };
                Some(ImageRef {
                    kind,
                    path: i.url.clone(),
                })
            })
            .collect();
        MetadataRecord {
            external_ids: self.external_ids(),
            title: self.title,
            original_title: self.original_title,
            overview: self.overview,
            runtime_minutes: self.runtime,
            release_date,
            images,
            collection_id: self.collection.as_ref().map(|c| c.tmdb_id),
            collection_name: self
                .collection
                .as_ref()
                .and_then(|c| c.name.clone())
                .filter(|n| !n.is_empty()),
            genres: self.genres,
            content_rating: self
                .certification
                .or_else(|| {
                    self.certifications
                        .iter()
                        .find(|c| c.country.eq_ignore_ascii_case("US"))
                        .map(|c| c.certification.clone())
                })
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty() && !c.eq_ignore_ascii_case("Not Rated") && !c.eq_ignore_ascii_case("NR")),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn http() -> HttpClient {
        HttpClient::new(Duration::from_secs(5)).unwrap()
    }

    fn matrix_json() -> serde_json::Value {
        serde_json::json!({
            "TmdbId": 603,
            "ImdbId": "tt0133093",
            "Title": "The Matrix",
            "Overview": "A hacker learns the truth.",
            "OriginalTitle": "The Matrix",
            "Runtime": 136,
            "Popularity": 203.5,
            "Year": 1999,
            "Premier": "1999-03-24T00:00:00Z",
            "InCinema": "1999-03-31T00:00:00Z",
            "PhysicalRelease": "1999-11-25T00:00:00Z",
            "Images": [
                { "CoverType": "Poster", "Url": "https://img/poster.jpg" },
                { "CoverType": "Fanart", "Url": "https://img/fanart.jpg" },
                { "CoverType": "Banner", "Url": "https://img/banner.jpg" }
            ],
            // Fields we don't consume must be tolerated:
            "Genres": ["Action"],
            // The public proxy sends per-country certifications, not the
            // Radarr-only `Certification` string.
            "Certifications": [
                { "Country": "GB", "Certification": "15" },
                { "Country": "US", "Certification": "R" }
            ],
            "MovieRatings": { "Imdb": { "Value": 8.7 } }
        })
    }

    #[tokio::test]
    async fn lookup_by_tmdb_maps_the_record_and_sends_honest_ua() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movie/603"))
            .and(header("user-agent", USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(matrix_json()))
            .mount(&server)
            .await;

        let p = ServarrProvider::new(http()).with_base_url(server.uri());
        let rec = p.lookup(&ExternalId::Tmdb(TmdbId(603))).await.unwrap();

        assert_eq!(rec.title, "The Matrix");
        assert_eq!(rec.genres, vec!["Action".to_string()]);
        assert_eq!(rec.content_rating.as_deref(), Some("R"));
        assert_eq!(rec.external_ids.tmdb, Some(TmdbId(603)));
        assert_eq!(
            rec.external_ids.imdb.as_ref().map(|i| i.0.as_str()),
            Some("tt0133093")
        );
        assert_eq!(rec.runtime_minutes, Some(136));
        // InCinema wins the release-date precedence.
        assert_eq!(
            rec.release_date,
            chrono::NaiveDate::from_ymd_opt(1999, 3, 31)
        );
        // Poster + Fanart mapped; unknown cover types dropped.
        assert_eq!(rec.images.len(), 2);
        assert!(matches!(rec.images[0].kind, ImageKind::Poster));
    }

    /// Collection membership survives the Servarr mapping (SKADI-T-0581).
    ///
    /// The shape is taken from a real `api.radarr.video/v1/movie/8681` response:
    /// `Collection` is a PascalCase object with `TmdbId` and `Name`, and the
    /// other members it advertises (Images/Parts) are null in practice.
    #[tokio::test]
    async fn lookup_carries_collection_membership() {
        let server = MockServer::start().await;
        let mut body = matrix_json();
        body["Collection"] = serde_json::json!({
            "TmdbId": 135483,
            "Name": "Taken Collection",
            "Images": null,
            "Parts": null,
        });
        Mock::given(method("GET"))
            .and(path("/movie/603"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let p = ServarrProvider::new(http()).with_base_url(server.uri());
        let rec = p.lookup(&ExternalId::Tmdb(TmdbId(603))).await.unwrap();
        assert_eq!(rec.collection_id, Some(135483));
        assert_eq!(rec.collection_name.as_deref(), Some("Taken Collection"));
    }

    /// A standalone film must come back with *no* collection rather than an
    /// empty one — the client collapses on the id, and a present-but-blank id
    /// would group every unrelated standalone film into one tile.
    #[tokio::test]
    async fn a_film_with_no_collection_maps_to_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movie/603"))
            .respond_with(ResponseTemplate::new(200).set_body_json(matrix_json()))
            .mount(&server)
            .await;

        let p = ServarrProvider::new(http()).with_base_url(server.uri());
        let rec = p.lookup(&ExternalId::Tmdb(TmdbId(603))).await.unwrap();
        assert_eq!(rec.collection_id, None);
        assert_eq!(rec.collection_name, None);
    }

    #[tokio::test]
    async fn lookup_by_imdb_uses_the_imdb_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movie/imdb/tt0133093"))
            .respond_with(ResponseTemplate::new(200).set_body_json(matrix_json()))
            .mount(&server)
            .await;

        let p = ServarrProvider::new(http()).with_base_url(server.uri());
        let rec = p
            .lookup(&ExternalId::Imdb(ImdbId("tt0133093".into())))
            .await
            .unwrap();
        assert_eq!(rec.title, "The Matrix");
    }

    #[tokio::test]
    async fn search_maps_matches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "the matrix"))
            .and(query_param("year", "1999"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([matrix_json()])),
            )
            .mount(&server)
            .await;

        let p = ServarrProvider::new(http()).with_base_url(server.uri());
        let matches = p
            .search(&MetadataQuery {
                title: "the matrix".into(),
                year: Some(1999),
                kind: MediaKind::Movie,
            })
            .await
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].title, "The Matrix");
        assert_eq!(matches[0].year, Some(1999));
        assert_eq!(matches[0].external_ids.tmdb, Some(TmdbId(603)));
    }

    #[tokio::test]
    async fn malformed_body_is_a_clean_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movie/603"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let p = ServarrProvider::new(http()).with_base_url(server.uri());
        let err = p.lookup(&ExternalId::Tmdb(TmdbId(603))).await.unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
    }

    #[tokio::test]
    async fn unsupported_id_kind_is_validation() {
        let p = ServarrProvider::new(http());
        let err = p
            .lookup(&ExternalId::Tvdb(skadi_core::TvdbId(1)))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    /// Live smoke against the real endpoint. Ignored by default — run
    /// explicitly with `cargo test -p skadi-metadata -- --ignored` when you
    /// want to verify the real-world contract. See SKADI-A-0001.
    #[tokio::test]
    #[ignore = "hits the real api.radarr.video (see ADR SKADI-A-0001)"]
    async fn live_smoke_matrix_lookup() {
        let p = ServarrProvider::new(http());
        let rec = p.lookup(&ExternalId::Tmdb(TmdbId(603))).await.unwrap();
        assert_eq!(rec.title, "The Matrix");
        assert_eq!(rec.runtime_minutes, Some(136));
    }
}
