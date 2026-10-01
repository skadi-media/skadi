//! Keyless audiobook metadata via **Audnexus** (`api.audnex.us`) — SKADI-I-0017.
//!
//! Audnexus is a community Audible-metadata mirror keyed by **ASIN**. Like the
//! [`ServarrProvider`](crate::ServarrProvider) it is keyless project-run infra
//! with no third-party ToS/SLA — the same "private personal project only"
//! posture as ADR SKADI-A-0001 applies, and every request carries an honest
//! `skadi/<version>` User-Agent.
//!
//! Endpoints used (region-scoped, default `us`):
//! - `GET /books/{asin}` — a full book record (title/subtitle/authors/narrators/
//!   series+position/runtime/cover/release-date/format) → [`MetadataRecord`].
//! - `GET /authors/{asin}` — a single author (name/description/image).
//! - `GET /authors?name={q}` — author search → ASIN candidates.
//!
//! Audnexus is **ASIN-keyed**: it has no book-title search, so the
//! [`MetadataProvider::search`] (book search) returns empty — the add-flow is
//! ASIN-driven (paste an Audible link) and book discovery happens via the
//! indexer (AudiobookBay). Author search + lookup are exposed as inherent
//! methods for the audiobooks domain's author-monitoring (SKADI-T-0132).

use async_trait::async_trait;
use serde::Deserialize;

use skadi_core::{AppError, AsinId, ExternalIds, MediaKind, Result};
use skadi_http::HttpClient;

use crate::{
    ExternalId, ImageKind, ImageRef, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord,
};

const DEFAULT_BASE_URL: &str = "https://api.audnex.us";
const DEFAULT_REGION: &str = "us";

/// The honest identity we present (mirrors ADR SKADI-A-0001 condition 1).
const USER_AGENT: &str = concat!("skadi/", env!("CARGO_PKG_VERSION"));

/// A keyless audiobook metadata provider over the Audnexus API.
pub struct AudnexusProvider {
    base_url: String,
    region: String,
    http: HttpClient,
}

/// An author candidate from [`AudnexusProvider::search_authors`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AuthorMatch {
    pub asin: AsinId,
    pub name: String,
}

/// A fully-resolved author from [`AudnexusProvider::lookup_author`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AudnexusAuthor {
    pub asin: AsinId,
    pub name: String,
    pub description: Option<String>,
    pub image: Option<String>,
}

impl AudnexusProvider {
    /// Construct against the public endpoint (region `us`).
    #[must_use]
    pub fn new(http: HttpClient) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            region: DEFAULT_REGION.to_string(),
            http,
        }
    }

    /// Override the base URL (used by tests against a fake server).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the Audible region (default `us`).
    #[must_use]
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// Look up a single author by ASIN (`GET /authors/{asin}`).
    pub async fn lookup_author(&self, asin: &AsinId) -> Result<AudnexusAuthor> {
        let url = self.url(&format!("/authors/{}", asin.0));
        let region = self.region.clone();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url)
                    .header("User-Agent", USER_AGENT)
                    .query(&[("region", region.as_str())])
            })
            .await?;
        let body: AuthorResource = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding audnexus author: {e}")))?;
        Ok(AudnexusAuthor {
            asin: AsinId(body.asin),
            name: body.name,
            description: body.description,
            image: body.image,
        })
    }

    /// Search authors by name (`GET /authors?name={q}`). Returns ASIN candidates
    /// the audiobooks domain resolves with [`lookup_author`](Self::lookup_author).
    pub async fn search_authors(&self, name: &str) -> Result<Vec<AuthorMatch>> {
        let url = self.url("/authors");
        let region = self.region.clone();
        let name = name.to_string();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url)
                    .header("User-Agent", USER_AGENT)
                    .query(&[("name", name.as_str()), ("region", region.as_str())])
            })
            .await?;
        let body: Vec<AuthorResource> = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding audnexus author search: {e}")))?;
        // Audnexus returns the same author many times (e.g. "Jim Butcher" × 8);
        // collapse to the first occurrence of each ASIN, preserving order.
        let mut seen = std::collections::HashSet::new();
        Ok(body
            .into_iter()
            .filter(|a| !a.asin.is_empty() && seen.insert(a.asin.clone()))
            .map(|a| AuthorMatch {
                asin: AsinId(a.asin),
                name: a.name,
            })
            .collect())
    }
}

#[async_trait]
impl MetadataProvider for AudnexusProvider {
    fn name(&self) -> &str {
        "audnexus"
    }

    fn supports(&self, kind: MediaKind) -> bool {
        matches!(kind, MediaKind::Audiobook)
    }

    /// Audnexus has no book-title search (it is ASIN-keyed); book discovery is
    /// via the indexer, and the add-flow is ASIN-driven. Returns empty.
    async fn search(&self, _query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        Ok(Vec::new())
    }

    async fn lookup(&self, id: &ExternalId) -> Result<MetadataRecord> {
        let asin = match id {
            ExternalId::Asin(a) => a,
            other => {
                return Err(AppError::Validation(format!(
                    "AudnexusProvider can only look up by ASIN (got {other:?})"
                )));
            }
        };
        let url = self.url(&format!("/books/{}", asin.0));
        let region = self.region.clone();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url)
                    .header("User-Agent", USER_AGENT)
                    .query(&[("region", region.as_str())])
            })
            .await?;
        let book: BookResource = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding audnexus book: {e}")))?;
        Ok(book.into_record())
    }
}

/// The subset of the Audnexus book resource we consume (camelCase on the wire;
/// unknown fields ignored so upstream additions don't break us).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookResource {
    asin: String,
    title: String,
    subtitle: Option<String>,
    #[serde(default)]
    authors: Vec<NamedAsin>,
    #[serde(default)]
    narrators: Vec<Named>,
    series_primary: Option<SeriesRef>,
    runtime_length_min: Option<u32>,
    image: Option<String>,
    release_date: Option<String>,
    /// `"unabridged"` / `"abridged"`.
    format_type: Option<String>,
    summary: Option<String>,
    isbn: Option<String>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NamedAsin {
    name: String,
    #[allow(dead_code)]
    asin: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesRef {
    name: String,
    /// Position within the series, e.g. `"1"` or `"3.5"` (string on the wire).
    position: Option<String>,
}

/// An author resource (used by both `/authors/{asin}` and `/authors?name=`).
#[derive(Deserialize)]
struct AuthorResource {
    asin: String,
    name: String,
    description: Option<String>,
    image: Option<String>,
}

/// Parse the date part out of an ISO timestamp (`2021-05-04T00:00:00.000Z`).
fn date_of(s: &Option<String>) -> Option<chrono::NaiveDate> {
    s.as_deref()
        .and_then(|s| s.get(..10))
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
}

impl BookResource {
    fn into_record(self) -> MetadataRecord {
        let abridged = self.format_type.as_deref().and_then(|f| match f {
            "abridged" => Some(true),
            "unabridged" => Some(false),
            _ => None,
        });
        let images = self
            .image
            .map(|path| {
                vec![ImageRef {
                    kind: ImageKind::Poster,
                    path,
                }]
            })
            .unwrap_or_default();
        let (series, series_position) = self
            .series_primary
            .map(|s| (Some(s.name), s.position))
            .unwrap_or((None, None));
        // ISBN is parsed off the wire but dropped: Skadi has no ISBN id type yet,
        // and ASIN is the audiobook key. (Carried for a future cross-reference.)
        let _ = &self.isbn;
        let external_ids = ExternalIds {
            asin: Some(AsinId(self.asin)),
            ..Default::default()
        };
        MetadataRecord {
            content_rating: None,
            genres: Vec::new(),
            external_ids,
            title: self.title,
            original_title: None,
            overview: self.summary,
            runtime_minutes: self.runtime_length_min,
            release_date: date_of(&self.release_date),
            images,
            subtitle: self.subtitle,
            // Role suffixes ("- editor", "- translator") are parsed out here,
            // and only real authors kept (SKADI-T-0652).
            authors: crate::contributors::select_author_names(
                self.authors.into_iter().map(|a| a.name),
            ),
            narrators: self.narrators.into_iter().map(|n| n.name).collect(),
            series,
            series_position,
            abridged,
            // Audiobooks have no TMDB collection; their grouping is `series`
            // above. Listed rather than defaulted so this initializer stays
            // exhaustive — that is what surfaced this field in the first place.
            collection_id: None,
            collection_name: None,
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

    fn phm_json() -> serde_json::Value {
        serde_json::json!({
            "asin": "B08G9PRS1K",
            "title": "Project Hail Mary",
            "subtitle": "A Novel",
            "authors": [{ "asin": "A1", "name": "Andy Weir" }],
            "narrators": [{ "name": "Ray Porter" }],
            "seriesPrimary": { "asin": "S1", "name": "Standalone", "position": "1" },
            "runtimeLengthMin": 970,
            "image": "https://img/phm.jpg",
            "releaseDate": "2021-05-04T00:00:00.000Z",
            "formatType": "unabridged",
            "summary": "Ryland Grace wakes up alone.",
            "language": "english",
            "isbn": "9780593135204",
            "genres": [{ "name": "Sci-Fi" }]
        })
    }

    #[tokio::test]
    async fn lookup_by_asin_maps_the_record() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/books/B08G9PRS1K"))
            .and(header("user-agent", USER_AGENT))
            .and(query_param("region", "us"))
            .respond_with(ResponseTemplate::new(200).set_body_json(phm_json()))
            .mount(&server)
            .await;

        let p = AudnexusProvider::new(http()).with_base_url(server.uri());
        let rec = p
            .lookup(&ExternalId::Asin(AsinId("B08G9PRS1K".into())))
            .await
            .unwrap();

        assert_eq!(rec.title, "Project Hail Mary");
        assert_eq!(rec.subtitle.as_deref(), Some("A Novel"));
        assert_eq!(rec.external_ids.asin, Some(AsinId("B08G9PRS1K".into())));
        assert_eq!(rec.authors, vec!["Andy Weir".to_string()]);
        assert_eq!(rec.narrators, vec!["Ray Porter".to_string()]);
        assert_eq!(rec.series.as_deref(), Some("Standalone"));
        assert_eq!(rec.series_position.as_deref(), Some("1"));
        assert_eq!(rec.runtime_minutes, Some(970));
        assert_eq!(rec.abridged, Some(false));
        assert_eq!(
            rec.release_date,
            chrono::NaiveDate::from_ymd_opt(2021, 5, 4)
        );
        assert_eq!(rec.images.len(), 1);
    }

    #[tokio::test]
    async fn lookup_by_non_asin_is_validation() {
        let p = AudnexusProvider::new(http());
        let err = p
            .lookup(&ExternalId::Tmdb(skadi_core::TmdbId(1)))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[tokio::test]
    async fn book_search_is_empty_audnexus_has_no_title_search() {
        let p = AudnexusProvider::new(http());
        let matches = p
            .search(&MetadataQuery {
                title: "Project Hail Mary".into(),
                year: None,
                kind: MediaKind::Audiobook,
            })
            .await
            .unwrap();
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn author_lookup_and_search() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/authors/A1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "asin": "A1",
                "name": "Andy Weir",
                "description": "Author of The Martian.",
                "image": "https://img/aw.jpg"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/authors"))
            .and(query_param("name", "andy weir"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "asin": "A1", "name": "Andy Weir" },
                { "asin": "A2", "name": "Andy Weireld" }
            ])))
            .mount(&server)
            .await;

        let p = AudnexusProvider::new(http()).with_base_url(server.uri());
        let author = p.lookup_author(&AsinId("A1".into())).await.unwrap();
        assert_eq!(author.name, "Andy Weir");
        assert_eq!(
            author.description.as_deref(),
            Some("Author of The Martian.")
        );

        let results = p.search_authors("andy weir").await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].asin, AsinId("A1".into()));
        assert_eq!(results[0].name, "Andy Weir");
    }

    #[tokio::test]
    async fn search_authors_dedups_by_asin() {
        // Audnexus repeats the same author many times (the "Jim Butcher × 8"
        // problem, SKADI-T-0152); search collapses to one per ASIN, in order.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/authors"))
            .and(query_param("name", "jim butcher"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "asin": "B1", "name": "Jim Butcher" },
                { "asin": "B1", "name": "Jim Butcher" },
                { "asin": "B1", "name": "Jim Butcher" },
                { "asin": "B2", "name": "Storm Front … Paperback" },
                { "asin": "", "name": "no asin — drop" }
            ])))
            .mount(&server)
            .await;
        let p = AudnexusProvider::new(http()).with_base_url(server.uri());
        let results = p.search_authors("jim butcher").await.unwrap();
        assert_eq!(
            results
                .iter()
                .map(|r| r.asin.0.as_str())
                .collect::<Vec<_>>(),
            vec!["B1", "B2"],
            "one row per ASIN, empty ASIN dropped, order preserved"
        );
    }

    #[tokio::test]
    async fn malformed_body_is_a_clean_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/books/X"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let p = AudnexusProvider::new(http()).with_base_url(server.uri());
        let err = p
            .lookup(&ExternalId::Asin(AsinId("X".into())))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
    }
}
