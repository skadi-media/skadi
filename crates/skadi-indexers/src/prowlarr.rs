//! Prowlarr **aggregate** indexer client (SKADI-T-0179).
//!
//! Unlike [`Torznab`](crate::torznab::Torznab), which speaks to a single
//! per-indexer Torznab feed (`/<id>/api`), this talks to Prowlarr's JSON
//! `/api/v1/search` endpoint, which fans out across **every** indexer Prowlarr has
//! configured and merges the results. So skadi registers ONE `prowlarr` indexer
//! and automatically searches all of them — adding/removing indexers in Prowlarr
//! needs no skadi change.
//!
//! Auth is the Prowlarr API key, sent as the `X-Api-Key` header. The search query
//! is the domain's title alias(es); the JSON `ReleaseResource[]` maps onto
//! [`Release`] (a `magnetUrl` becomes a [`ReleaseFetch::Magnet`], otherwise the
//! `downloadUrl` — an ephemeral grab link that 30x-redirects to the real
//! magnet/torrent — becomes a [`ReleaseFetch::TorrentUrl`] the worker resolves).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use skadi_core::{AppError, IndexerId, MediaKind, Protocol, Result};
use skadi_http::HttpClient;

use crate::{Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch};

/// A configured Prowlarr aggregate endpoint.
pub struct Prowlarr {
    id: IndexerId,
    base_url: String,
    api_key: String,
    categories: Vec<Category>,
    http: HttpClient,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    flags: crate::config::IndexerFlags,
}

impl Prowlarr {
    /// Construct a client for `base_url` (the Prowlarr root; `/api/v1/search` is
    /// appended). `api_key` is the Prowlarr API key.
    #[must_use]
    pub fn new(
        id: IndexerId,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        categories: Vec<Category>,
        http: HttpClient,
    ) -> Self {
        Self {
            id,
            base_url: base_url.into(),
            api_key: api_key.into(),
            categories,
            http,
            flags: crate::config::IndexerFlags::default(),
        }
    }

    /// Apply the stored per-indexer flags (SKADI-T-0505).
    #[must_use]
    pub fn with_flags(mut self, flags: crate::config::IndexerFlags) -> Self {
        self.flags = flags;
        self
    }

    fn search_url(&self) -> String {
        format!("{}/api/v1/search", self.base_url.trim_end_matches('/'))
    }

    fn indexer_url(&self) -> String {
        format!("{}/api/v1/indexer", self.base_url.trim_end_matches('/'))
    }

    /// One `/api/v1/search` call for `query`, scoped to `categories`.
    async fn fetch(
        &self,
        query: &str,
        categories: &[Category],
        spec: Option<&dyn SearchQuery>,
    ) -> Result<Vec<Release>> {
        let url = self.search_url();
        let key = self.api_key.clone();
        // Repeated `categories` params (Prowlarr accepts a list), plus the query.
        let mut params: Vec<(String, String)> = vec![
            ("query".into(), query.to_string()),
            ("type".into(), "search".into()),
            ("limit".into(), "100".into()),
        ];
        // Ids and TV scope (SKADI-T-0503). A bare title query makes Prowlarr fall
        // back to text matching across every tracker it fronts, which is both
        // noisier and worse at finding the right release than the id search the
        // aggregate already supports.
        if let Some(spec) = spec {
            let ids = spec.external_ids();
            if let Some(id) = &ids.imdb {
                // Prowlarr wants the numeric part, as Torznab does.
                params.push(("imdbId".into(), id.0.trim_start_matches("tt").to_string()));
            }
            if let Some(id) = &ids.tmdb {
                params.push(("tmdbId".into(), id.0.to_string()));
            }
            if let Some(id) = &ids.tvdb {
                params.push(("tvdbId".into(), id.0.to_string()));
            }
            // `season`/`ep` arrive as scope extras, the same shape the cardigann
            // adapter consumes. Their presence is what makes this a tv search.
            let mut is_tv = false;
            for (k, v) in spec.extra_params() {
                if k.eq_ignore_ascii_case("season") || k.eq_ignore_ascii_case("ep") {
                    is_tv = true;
                }
                params.push((k.to_string(), v));
            }
            if is_tv || spec.kind() == MediaKind::Series {
                for p in &mut params {
                    if p.0 == "type" {
                        p.1 = "tvsearch".into();
                    }
                }
            }
        }
        for c in categories {
            params.push(("categories".into(), c.0.to_string()));
        }
        let resp = self
            .http
            .send_idempotent(|c| c.get(&url).header("X-Api-Key", key.as_str()).query(&params))
            .await?;
        let body = resp
            .text()
            .await
            .map_err(|e| AppError::Network(format!("reading Prowlarr response: {e}")))?;
        let items: Vec<ReleaseResource> = serde_json::from_str(&body)
            .map_err(|e| AppError::Internal(format!("parsing Prowlarr search JSON: {e}")))?;
        Ok(items
            .into_iter()
            .filter_map(|it| it.into_release(self.id))
            .collect())
    }
}

#[async_trait]
impl Indexer for Prowlarr {
    fn enable_rss(&self) -> bool {
        self.flags.enable_rss
    }

    fn enable_automatic_search(&self) -> bool {
        self.flags.enable_automatic_search
    }

    fn priority(&self) -> u32 {
        self.flags.priority
    }

    fn minimum_seeders(&self) -> u32 {
        self.flags.minimum_seeders
    }

    fn id(&self) -> IndexerId {
        self.id
    }

    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    /// Category-driven, exactly like Torznab: a Prowlarr indexer configured with
    /// `2000` serves movies, `3030` serves audiobooks, `5000` serves TV
    /// (SKADI-T-0499); configure several to serve several domains from one
    /// aggregate endpoint.
    fn supports(&self, kind: MediaKind) -> bool {
        let range = match kind {
            MediaKind::Movie => 2000..3000,
            MediaKind::Audiobook => 3000..4000,
            MediaKind::Series => 5000..6000,
            _ => return false,
        };
        if self.categories.is_empty() {
            return kind == MediaKind::Movie;
        }
        self.categories.iter().any(|c| range.contains(&c.0))
    }

    /// A **cheap** health probe (SKADI-T-0201): `GET /api/v1/indexer` returns
    /// Prowlarr's configured-indexer list, validating both reachability and the API
    /// key without fanning a real search across every tracker (which the old `test`
    /// did — slow and rate-limit-hostile). The body is ignored; a 2xx is the health
    /// signal (non-2xx surfaces as `AppError::Network` from the HTTP layer, so a bad
    /// key → 401 → `Err`).
    async fn test(&self) -> Result<()> {
        let url = self.indexer_url();
        let key = self.api_key.clone();
        self.http
            .send_idempotent(|c| c.get(&url).header("X-Api-Key", key.as_str()))
            .await
            .map(|_| ())
    }

    /// Real capabilities (SKADI-T-0205): pull `GET /api/v1/indexer` and aggregate
    /// the configured trackers' advertised categories + rss/search support, so the
    /// config UI can discover the actual category tree instead of relying on raw
    /// Newznab numbers. Validated against a live Prowlarr 2.4 (the per-indexer
    /// `capabilities.categories` + `supportsRss`/`supportsSearch` shape). On a
    /// network/parse failure it degrades to the configured categories rather than
    /// erroring (callers treat caps as best-effort metadata).
    async fn capabilities(&self) -> Result<IndexerCaps> {
        let fallback = || IndexerCaps {
            supports_rss: false,
            supports_search: true,
            id_params: std::collections::BTreeSet::new(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: self.categories.clone(),
        };

        let url = self.indexer_url();
        let key = self.api_key.clone();
        let body = match self
            .http
            .send_idempotent(|c| c.get(&url).header("X-Api-Key", key.as_str()))
            .await
        {
            Ok(resp) => match resp.text().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "reading Prowlarr indexer caps; using configured categories");
                    return Ok(fallback());
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "fetching Prowlarr indexer caps; using configured categories");
                return Ok(fallback());
            }
        };
        let indexers: Vec<IndexerResource> = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "parsing Prowlarr indexer caps; using configured categories");
                return Ok(fallback());
            }
        };

        let mut cats: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        let mut supports_rss = false;
        let mut supports_search = false;
        for ix in &indexers {
            supports_rss |= ix.supports_rss;
            supports_search |= ix.supports_search;
            collect_category_ids(&ix.capabilities.categories, &mut cats);
        }
        Ok(IndexerCaps {
            supports_rss,
            supports_search,
            id_params: std::collections::BTreeSet::new(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: cats.into_iter().map(Category).collect(),
        })
    }

    /// The recent-releases feed (SKADI-T-0192): a query-less aggregate search over
    /// the configured categories. Validated against a live Prowlarr 2.4 — an empty
    /// `query` with `type=search` returns the recent feed, newest-first, in the
    /// same `ReleaseResource` JSON a normal search yields.
    async fn rss(&self) -> Result<Vec<Release>> {
        self.fetch("", &self.categories, None).await
    }

    /// Search each title alias and merge, deduping by (normalized title, size) so
    /// the same release surfaced by two aliases isn't double-counted.
    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        let cats: Vec<Category> = if query.categories().is_empty() {
            self.categories.clone()
        } else {
            query.categories().to_vec()
        };
        let mut out: Vec<Release> = Vec::new();
        let mut seen: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
        for title in query.titles() {
            let releases = self.fetch(title, &cats, Some(query)).await?;
            for r in releases {
                if seen.insert((crate::normalize_title(&r.title), r.size)) {
                    out.push(r);
                }
            }
        }
        Ok(out)
    }
}

/// The subset of a `/api/v1/indexer` row we consume for capabilities
/// (SKADI-T-0205).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexerResource {
    #[serde(default)]
    supports_rss: bool,
    #[serde(default)]
    supports_search: bool,
    #[serde(default)]
    capabilities: IndexerCapsResource,
}

#[derive(Deserialize, Default)]
struct IndexerCapsResource {
    #[serde(default)]
    categories: Vec<CategoryNode>,
}

/// A node in Prowlarr's category tree (`{id, name, subCategories}`); we only need
/// the id (+ recursive children).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CategoryNode {
    id: u32,
    #[serde(default)]
    sub_categories: Vec<CategoryNode>,
}

/// Flatten a category tree into the set of all ids (parents + descendants).
fn collect_category_ids(nodes: &[CategoryNode], out: &mut std::collections::BTreeSet<u32>) {
    for n in nodes {
        out.insert(n.id);
        collect_category_ids(&n.sub_categories, out);
    }
}

/// The subset of Prowlarr's `ReleaseResource` JSON we consume.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseResource {
    title: Option<String>,
    #[serde(default)]
    size: u64,
    seeders: Option<u32>,
    download_url: Option<String>,
    magnet_url: Option<String>,
    publish_date: Option<String>,
    /// Torznab category ids Prowlarr tagged the result with (`[{id,name},…]`), for
    /// the decide-time off-category gate (SKADI-T-0360). Absent → empty.
    #[serde(default)]
    categories: Vec<CategoryResource>,
}

/// One entry of Prowlarr's `categories` array — we only need the numeric id.
#[derive(Debug, Deserialize)]
struct CategoryResource {
    id: u32,
}

impl ReleaseResource {
    fn into_release(self, indexer: IndexerId) -> Option<Release> {
        let title = self.title.filter(|t| !t.is_empty())?;
        // Prefer a real magnet; otherwise the ephemeral grab link (the worker's
        // resolver follows its redirects to the magnet/torrent).
        let fetch = match self.magnet_url {
            Some(m) if m.starts_with("magnet:") => ReleaseFetch::Magnet(m),
            _ => ReleaseFetch::TorrentUrl(self.download_url?),
        };
        let published = self
            .publish_date
            .as_deref()
            .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let parsed = skadi_quality::parse(&title);
        Some(Release {
            indexer,
            title,
            fetch,
            size: self.size,
            published,
            seeders: self.seeders,
            categories: self.categories.iter().map(|c| Category(c.id)).collect(),
            parsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchMode;
    use skadi_core::ExternalIds;
    use std::time::Duration;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SEARCH_JSON: &str = r#"[
      {"title":"Dungeon Crawler Carl (Audiobook) M4B","size":1200000000,"seeders":12,
       "downloadUrl":"http://prowlarr/15/download?link=abc","magnetUrl":null,
       "publishDate":"2026-05-12T00:00:00Z","indexer":"AudioBook Bay"},
      {"title":"Dungeon Crawler Carl [MP3 128]","size":500000000,"seeders":3,
       "downloadUrl":"http://prowlarr/8/download?link=def",
       "magnetUrl":"magnet:?xt=urn:btih:deadbeef","publishDate":"2025-01-01T00:00:00Z"}
    ]"#;

    struct AbQuery;
    impl SearchQuery for AbQuery {
        fn kind(&self) -> MediaKind {
            MediaKind::Audiobook
        }
        fn titles(&self) -> &[String] {
            std::slice::from_ref(TITLE.get_or_init(|| "Dungeon Crawler Carl".to_string()))
        }
        fn year(&self) -> Option<u16> {
            None
        }
        fn external_ids(&self) -> &ExternalIds {
            IDS.get_or_init(ExternalIds::default)
        }
        fn categories(&self) -> &[Category] {
            &CATS
        }
        fn mode(&self) -> SearchMode {
            SearchMode::Auto
        }
    }
    static TITLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static IDS: std::sync::OnceLock<ExternalIds> = std::sync::OnceLock::new();
    const CATS: [Category; 1] = [Category(3030)];

    #[test]
    fn supports_is_category_driven() {
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let mk =
            |c: Vec<Category>| Prowlarr::new(IndexerId::new(), "http://x", "k", c, http.clone());
        assert!(mk(vec![Category(3030)]).supports(MediaKind::Audiobook));
        assert!(!mk(vec![Category(3030)]).supports(MediaKind::Movie));
        assert!(mk(vec![Category(2000), Category(3030)]).supports(MediaKind::Movie));
    }

    #[test]
    fn maps_json_to_releases() {
        let items: Vec<ReleaseResource> = serde_json::from_str(SEARCH_JSON).unwrap();
        let id = IndexerId::new();
        let rels: Vec<Release> = items
            .into_iter()
            .filter_map(|i| i.into_release(id))
            .collect();
        assert_eq!(rels.len(), 2);
        // First has no magnet → TorrentUrl(downloadUrl); second has a magnet.
        assert!(matches!(rels[0].fetch, ReleaseFetch::TorrentUrl(_)));
        assert!(matches!(rels[1].fetch, ReleaseFetch::Magnet(_)));
        assert_eq!(rels[0].seeders, Some(12));
        assert_eq!(rels[0].size, 1_200_000_000);
    }

    /// A real Prowlarr 2.4 empty-`query` aggregate-search response (captured from a
    /// live container fronting a torznab upstream; the live API key was redacted to
    /// `APIKEY`). This is the faithful RSS target — the recent feed, newest-first.
    const RSS_FIXTURE: &str = include_str!("../tests/fixtures/prowlarr_rss_empty.json");

    #[tokio::test]
    async fn rss_pulls_recent_feed_with_empty_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search"))
            .and(query_param("query", ""))
            .and(query_param("categories", "2000"))
            .and(header("X-Api-Key", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RSS_FIXTURE))
            .expect(1)
            .mount(&server)
            .await;

        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let pw = Prowlarr::new(
            IndexerId::new(),
            server.uri(),
            "secret",
            vec![Category(2000)],
            http,
        );
        let releases = pw.rss().await.unwrap();
        // The live Prowlarr feed carried 3 recent items, newest-first.
        assert_eq!(releases.len(), 3, "real Prowlarr RSS fixture has 3 items");
        assert!(
            releases[0].title.contains("Big Buck Bunny"),
            "newest-first: {}",
            releases[0].title
        );
        assert_eq!(releases[0].seeders, Some(87));
        // Prowlarr rewrites the fetch to its own /download proxy (not a magnet:),
        // so the client classifies it as a TorrentUrl the worker resolves later.
        assert!(matches!(releases[0].fetch, ReleaseFetch::TorrentUrl(_)));
    }

    #[tokio::test]
    async fn test_probes_indexer_list_not_a_full_search() {
        let server = MockServer::start().await;
        // The cheap probe: GET /api/v1/indexer with the key → 200.
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .and(header("X-Api-Key", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .expect(1)
            .mount(&server)
            .await;
        // A real search must NOT be issued by a health check.
        Mock::given(method("GET"))
            .and(path("/api/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .expect(0)
            .mount(&server)
            .await;

        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let pw = Prowlarr::new(
            IndexerId::new(),
            server.uri(),
            "secret",
            vec![Category(2000)],
            http,
        );
        pw.test().await.unwrap();
        // wiremock verifies the expect(1)/expect(0) on drop.
    }

    /// A real Prowlarr 2.4 `/api/v1/indexer` response (captured from a live
    /// container, trimmed to the fields `capabilities()` reads).
    const INDEXER_LIST_FIXTURE: &str = include_str!("../tests/fixtures/prowlarr_indexer_list.json");

    #[tokio::test]
    async fn capabilities_aggregates_categories_from_indexer_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .and(header("X-Api-Key", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(INDEXER_LIST_FIXTURE))
            .expect(1)
            .mount(&server)
            .await;

        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let pw = Prowlarr::new(IndexerId::new(), server.uri(), "secret", vec![], http);
        let caps = pw.capabilities().await.unwrap();
        assert!(caps.supports_search);
        assert!(caps.supports_rss, "the live indexer advertises RSS");
        let ids: Vec<u32> = caps.categories.iter().map(|c| c.0).collect();
        assert!(ids.contains(&2000), "movies category discovered: {ids:?}");
        assert!(ids.contains(&5000), "tv category discovered: {ids:?}");
    }

    #[tokio::test]
    async fn capabilities_falls_back_to_configured_categories_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let pw = Prowlarr::new(
            IndexerId::new(),
            server.uri(),
            "k",
            vec![Category(2000)],
            http,
        );
        // A failed caps query degrades to the configured categories, never errors.
        let caps = pw.capabilities().await.unwrap();
        assert_eq!(caps.categories, vec![Category(2000)]);
    }

    #[tokio::test]
    async fn test_fails_on_bad_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let pw = Prowlarr::new(IndexerId::new(), server.uri(), "wrong", vec![], http);
        assert!(pw.test().await.is_err(), "401 must surface as an error");
    }

    #[tokio::test]
    async fn search_hits_aggregate_endpoint_with_key_and_category() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search"))
            .and(query_param("query", "Dungeon Crawler Carl"))
            .and(query_param("categories", "3030"))
            .and(header("X-Api-Key", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH_JSON))
            .expect(1)
            .mount(&server)
            .await;

        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let pw = Prowlarr::new(
            IndexerId::new(),
            server.uri(),
            "secret",
            vec![Category(3030)],
            http,
        );
        let releases = pw.search(&AbQuery).await.unwrap();
        assert_eq!(releases.len(), 2);
    }
}
