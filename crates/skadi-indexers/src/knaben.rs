//! Knaben **aggregate** indexer client (SKADI-T-0618).
//!
//! Knaben is a meta-search over ~3M torrents harvested from the public trackers
//! (The Pirate Bay, 1337x, Nyaa, YTS, …), exposed as a genuine JSON API at
//! `POST https://api.knaben.org/v1` — no account, no API key, no Cardigann
//! definition. That is the whole reason it is worth a native client: it is one
//! of the well-known public indexers SKADI-T-0593 found the Cardigann catalog
//! *cannot* supply, and being account-less there is no ratio to breach and
//! nothing to be banned from.
//!
//! Shaped like [`Prowlarr`](crate::prowlarr::Prowlarr) — one config fans out
//! across many trackers — but POSTs a JSON body and carries no credential.
//!
//! Two things differ from a Torznab-family indexer and drive the design:
//!
//! 1. **Categories are Knaben's own scale**, not Newznab's: a hit carries
//!    `categoryId: [2000000, 3001000]` and the *same* `"Movies / HD"` label
//!    appears with different id sets depending on which tracker the row came
//!    from. The numbers are therefore unusable for the off-category gate, and
//!    the human-readable `category` label is the reliable signal — mapped onto
//!    Newznab codes by the table Cardigann already uses (SKADI-T-0506).
//! 2. **There is no season/episode parameter.** Like a public tracker
//!    definition, the episode marker has to be appended to the search text, so
//!    this reuses cardigann's `SxxEyy` suffix (SKADI-T-0587).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{AppError, IndexerId, MediaKind, Protocol, Result};
use skadi_http::HttpClient;

use crate::cardigann::{newznab_id, tv_keyword_suffix, with_tv_suffix};
use crate::{Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch};

/// The public API endpoint. No trailing slash — `…/v1/` is a 404.
pub const KNABEN_API_URL: &str = "https://api.knaben.org/v1";

/// Results per request. The API caps `size` at 300; 150 is its own default and
/// plenty for one title alias — the hunter ranks what it gets, it does not page.
const PAGE_SIZE: u32 = 150;

/// A configured Knaben endpoint.
pub struct Knaben {
    id: IndexerId,
    base_url: String,
    categories: Vec<Category>,
    http: HttpClient,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    flags: crate::config::IndexerFlags,
}

impl Knaben {
    /// Construct a client. `base_url` is the full API endpoint — defaulted to
    /// [`KNABEN_API_URL`] by the config layer, and overridden only by tests.
    #[must_use]
    pub fn new(
        id: IndexerId,
        base_url: impl Into<String>,
        categories: Vec<Category>,
        http: HttpClient,
    ) -> Self {
        Self {
            id,
            base_url: base_url.into(),
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

    /// One search request. An empty `query` is the recent-releases feed (the API
    /// then just returns the newest rows), which is what [`Indexer::rss`] wants.
    async fn fetch(&self, query: &str, scope: &[Category]) -> Result<Vec<Release>> {
        let url = self.base_url.clone();
        let body = SearchRequest {
            // `minimum_should_match` — every word must appear in the title.
            // Anything looser turns a two-word title into a feed of the whole
            // tracker, which the relevance gate then has to throw away.
            search_type: "100%",
            search_field: "title",
            query: query.to_string(),
            order_by: if query.is_empty() { "date" } else { "seeders" },
            order_direction: "desc",
            size: PAGE_SIZE,
            // This is a household media server: never index adult results, and
            // let Knaben drop its own high-virus-score rows.
            hide_xxx: true,
            hide_unsafe: true,
        };
        let resp = self
            .http
            .send_idempotent(|c| c.post(&url).json(&body))
            .await?;
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::Network(format!("reading Knaben response: {e}")))?;
        let parsed: SearchResponse = serde_json::from_str(&text)
            .map_err(|e| AppError::Internal(format!("parsing Knaben search JSON: {e}")))?;
        Ok(parsed
            .hits
            .into_iter()
            .filter_map(|h| h.into_release(self.id))
            .filter(|r| in_scope(scope, &r.categories))
            .collect())
    }
}

/// Whether a hit belongs to the configured categories.
///
/// Knaben answers a text query across everything it has, so a movie search also
/// returns software and music. Scoping is therefore **client-side**: the
/// configured Newznab ids cannot be sent upstream because Knaben's category
/// numbering is its own.
///
/// A hit whose label did not map to anything (`"Anime"`, and Knaben's other
/// non-Newznab labels) is **kept**: an unknown category is not evidence of a
/// wrong one, and dropping it would silently lose every anime result. Judging
/// those is the relevance gate's job, which sees the title.
fn in_scope(scope: &[Category], hit: &[Category]) -> bool {
    if scope.is_empty() || hit.is_empty() {
        return true;
    }
    let group = |c: &Category| c.0 / 1000;
    hit.iter()
        .any(|h| scope.iter().any(|s| group(s) == group(h)))
}

/// Map Knaben's label (`"Movies / HD"`, `"Audio / Audiobook"`) onto the Newznab
/// code the off-category gate reads. The separator is space-padded where
/// Newznab's is bare, so it is squeezed before the lookup; an unrecognised leaf
/// falls back to its group (`"Books / EBooks"` → `Books` → 7000) and a wholly
/// unrecognised label to nothing.
fn category_from_label(label: &str) -> Option<Category> {
    let squeezed: Vec<&str> = label.split('/').map(str::trim).collect();
    newznab_id(&squeezed.join("/")).map(Category)
}

/// The request body (only the fields we set; the API defaults the rest).
#[derive(Serialize)]
struct SearchRequest {
    search_type: &'static str,
    search_field: &'static str,
    query: String,
    order_by: &'static str,
    order_direction: &'static str,
    size: u32,
    hide_xxx: bool,
    hide_unsafe: bool,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    hits: Vec<Hit>,
}

/// The subset of a Knaben hit we consume.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hit {
    title: Option<String>,
    #[serde(default)]
    bytes: u64,
    seeders: Option<u32>,
    /// A complete, non-expiring magnet — unlike a Torznab proxy's download link,
    /// which is an ephemeral per-search token.
    magnet_url: Option<String>,
    /// The bare info-hash, used to build a magnet when `magnetUrl` is absent.
    hash: Option<String>,
    /// RFC3339 with an offset (`2026-09-22T10:55:00+00:00`).
    date: Option<String>,
    /// Knaben's own label, e.g. `"Movies / HD"`.
    category: Option<String>,
}

impl Hit {
    fn into_release(self, indexer: IndexerId) -> Option<Release> {
        let title = self.title.filter(|t| !t.trim().is_empty())?;
        let fetch = match self.magnet_url {
            Some(m) if m.starts_with("magnet:") => ReleaseFetch::Magnet(m),
            // No magnet on the row: the info-hash alone is a valid magnet, and
            // the worker's DHT/trackerless path resolves it.
            _ => ReleaseFetch::Magnet(format!(
                "magnet:?xt=urn:btih:{}",
                self.hash.filter(|h| !h.trim().is_empty())?
            )),
        };
        let published = self
            .date
            .as_deref()
            .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let parsed = skadi_quality::parse(&title);
        Some(Release {
            indexer,
            title,
            fetch,
            size: self.bytes,
            published,
            seeders: self.seeders,
            categories: self
                .category
                .as_deref()
                .and_then(category_from_label)
                .into_iter()
                .collect(),
            parsed,
        })
    }
}

#[async_trait]
impl Indexer for Knaben {
    fn enable_rss(&self) -> bool {
        self.flags.enable_rss
    }

    fn enable_automatic_search(&self) -> bool {
        self.flags.enable_automatic_search
    }

    fn applies_to_tags(&self, item_tags: &[String]) -> bool {
        self.flags.applies_to(item_tags)
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

    /// Category-driven, exactly like Torznab and Prowlarr (SKADI-T-0499).
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

    /// Static: Knaben publishes no capabilities endpoint. Search and the recent
    /// feed are both real, and the categories are whatever the operator scoped
    /// this indexer to.
    async fn capabilities(&self) -> Result<IndexerCaps> {
        Ok(IndexerCaps {
            supports_rss: true,
            supports_search: true,
            id_params: std::collections::BTreeSet::new(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: self.categories.clone(),
        })
    }

    /// The cheapest call the API has: one row of the recent feed. Validates
    /// reachability and that the response really is Knaben's JSON, without
    /// pulling 150 documents to do it.
    async fn test(&self) -> Result<()> {
        let url = self.base_url.clone();
        let body = SearchRequest {
            search_type: "100%",
            search_field: "title",
            query: String::new(),
            order_by: "date",
            order_direction: "desc",
            size: 1,
            hide_xxx: true,
            hide_unsafe: true,
        };
        let resp = self
            .http
            .send_idempotent(|c| c.post(&url).json(&body))
            .await?;
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::Network(format!("reading Knaben response: {e}")))?;
        serde_json::from_str::<SearchResponse>(&text)
            .map(|_| ())
            .map_err(|e| AppError::Network(format!("Knaben did not answer with its search JSON: {e}")))
    }

    /// The recent-releases feed (SKADI-T-0192): a query-less search, newest first.
    async fn rss(&self) -> Result<Vec<Release>> {
        self.fetch("", &self.categories).await
    }

    /// Search each title alias and merge, deduping by (normalized title, size)
    /// so one release surfaced by two aliases is not counted twice.
    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        let scope: Vec<Category> = if query.categories().is_empty() {
            self.categories.clone()
        } else {
            query.categories().to_vec()
        };
        let suffix = tv_keyword_suffix(query);
        let mut out: Vec<Release> = Vec::new();
        let mut seen: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
        for title in query.titles() {
            let term = with_tv_suffix(title, suffix.as_deref());
            for r in self.fetch(&term, &scope).await? {
                if seen.insert((crate::normalize_title(&r.title), r.size)) {
                    out.push(r);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchMode;
    use skadi_core::ExternalIds;
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A real `api.knaben.org/v1` answer (captured 2026-09-22, `query=Godzilla
    /// Minus One`, trimmed to four hits). Every assertion below is against what
    /// the service actually sends, not a hand-written idea of it.
    const SEARCH_FIXTURE: &str = include_str!("../tests/fixtures/knaben_search.json");

    fn http() -> HttpClient {
        HttpClient::new(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn maps_the_live_response_onto_releases() {
        let parsed: SearchResponse = serde_json::from_str(SEARCH_FIXTURE).unwrap();
        let id = IndexerId::new();
        let rels: Vec<Release> = parsed
            .hits
            .into_iter()
            .filter_map(|h| h.into_release(id))
            .collect();
        assert_eq!(rels.len(), 4);

        let first = &rels[0];
        assert_eq!(first.title, "Godzilla Minus One (2023) [1080p] [BluRay] [5.1]");
        assert_eq!(first.size, 2_469_606_195);
        assert_eq!(first.seeders, Some(297));
        assert_eq!(first.published.to_rfc3339(), "2024-05-01T22:00:00+00:00");
        // A whole magnet off the row — no second fetch, no expiring token.
        assert!(
            matches!(&first.fetch, ReleaseFetch::Magnet(m) if m.starts_with("magnet:?xt=urn:btih:")),
            "fetch was {:?}",
            first.fetch
        );
        // "Movies / HD" → the Newznab code the off-category gate reads.
        assert_eq!(first.categories, vec![Category(2040)]);
        // The quality parser ran, so ranking has something to work with.
        assert_eq!(first.parsed.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn a_hit_without_a_magnet_falls_back_to_its_info_hash() {
        let hit: Hit = serde_json::from_str(
            r#"{"title":"Some Release 1080p","bytes":42,"seeders":3,"magnetUrl":null,
                "hash":"ABCDEF0123","date":"2026-01-02T03:04:05+00:00","category":"Movies"}"#,
        )
        .unwrap();
        let r = hit.into_release(IndexerId::new()).unwrap();
        assert!(matches!(&r.fetch, ReleaseFetch::Magnet(m) if m == "magnet:?xt=urn:btih:ABCDEF0123"));
        assert_eq!(r.categories, vec![Category(2000)]);
    }

    #[test]
    fn a_hit_with_neither_magnet_nor_hash_is_dropped() {
        let hit: Hit = serde_json::from_str(r#"{"title":"No Way To Fetch This","bytes":1}"#).unwrap();
        assert!(hit.into_release(IndexerId::new()).is_none());
    }

    #[test]
    fn knaben_labels_map_onto_newznab_codes() {
        // The space-padded separator is what the API really sends.
        assert_eq!(category_from_label("Movies / HD"), Some(Category(2040)));
        assert_eq!(category_from_label("TV / UHD"), Some(Category(5045)));
        assert_eq!(category_from_label("Audio / Audiobook"), Some(Category(3030)));
        assert_eq!(category_from_label("Movies"), Some(Category(2000)));
        // An unknown leaf still resolves to the right group.
        assert_eq!(category_from_label("Books / EBooks"), Some(Category(7000)));
        // A label outside the Newznab tree maps to nothing rather than a guess.
        assert_eq!(category_from_label("Anime"), None);
    }

    #[test]
    fn scoping_keeps_the_right_group_and_never_drops_the_unlabelled() {
        let movies = [Category(2000)];
        assert!(in_scope(&movies, &[Category(2040)]), "same group, finer leaf");
        assert!(!in_scope(&movies, &[Category(4050)]), "PC/Games in a movie search");
        assert!(in_scope(&movies, &[]), "unmapped label is not evidence of a wrong one");
        assert!(in_scope(&[], &[Category(4050)]), "no scope configured ⇒ no filtering");
        // A TV+audiobook indexer keeps both, and still rejects software.
        let tv_books = [Category(5000), Category(3030)];
        assert!(in_scope(&tv_books, &[Category(5040)]));
        assert!(in_scope(&tv_books, &[Category(3030)]));
        assert!(!in_scope(&tv_books, &[Category(2040)]));
    }

    #[test]
    fn supports_is_category_driven() {
        let mk = |c: Vec<Category>| Knaben::new(IndexerId::new(), KNABEN_API_URL, c, http());
        assert!(mk(vec![Category(2000)]).supports(MediaKind::Movie));
        assert!(!mk(vec![Category(2000)]).supports(MediaKind::Series));
        assert!(mk(vec![Category(5000)]).supports(MediaKind::Series));
        assert!(mk(vec![Category(3030)]).supports(MediaKind::Audiobook));
        assert!(mk(vec![Category(2000), Category(5000)]).supports(MediaKind::Series));
    }

    struct TvQuery;
    static TITLE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    static IDS: std::sync::OnceLock<ExternalIds> = std::sync::OnceLock::new();
    const CATS: [Category; 1] = [Category(5000)];
    impl SearchQuery for TvQuery {
        fn kind(&self) -> MediaKind {
            MediaKind::Series
        }
        fn titles(&self) -> &[String] {
            TITLE.get_or_init(|| vec!["Andor".to_string()])
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
        fn extra_params(&self) -> Vec<(&'static str, String)> {
            vec![("season", "2".into()), ("ep", "5".into())]
        }
        fn mode(&self) -> SearchMode {
            SearchMode::Auto
        }
    }

    /// Knaben has no season/episode parameter, so a TV search has to carry the
    /// marker in the query text or it just gets the show's newest uploads
    /// (SKADI-T-0587, the same reason cardigann does it).
    #[tokio::test]
    async fn a_tv_search_appends_the_episode_marker_to_the_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .and(body_partial_json(serde_json::json!({
                "query": "Andor S02E05",
                "search_type": "100%",
                "search_field": "title",
                "hide_xxx": true,
                "hide_unsafe": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"hits":[{"title":"Andor S02E05 1080p WEB-DL","bytes":2000,"seeders":40,
                    "magnetUrl":"magnet:?xt=urn:btih:feedface","date":"2026-05-01T00:00:00+00:00",
                    "category":"TV / HD"}]}"#,
            ))
            .mount(&server)
            .await;

        let ix = Knaben::new(
            IndexerId::new(),
            format!("{}/v1", server.uri()),
            vec![Category(5000)],
            http(),
        );
        let out = ix.search(&TvQuery).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Andor S02E05 1080p WEB-DL");
        assert_eq!(out[0].categories, vec![Category(5040)]);
    }

    /// Everything Knaben returns for a text query, filtered to the configured
    /// categories — it answers across its whole index, so this is where a movie
    /// search stops being a software search.
    #[tokio::test]
    async fn out_of_scope_rows_are_dropped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"hits":[
                    {"title":"Gorilla Tag Mod Menu","bytes":1,"seeders":1,
                     "magnetUrl":"magnet:?xt=urn:btih:aa","category":"PC / Software"},
                    {"title":"Godzilla 2014 1080p BluRay","bytes":2,"seeders":9,
                     "magnetUrl":"magnet:?xt=urn:btih:bb","category":"Movies / HD"},
                    {"title":"Godzilla Singular Point","bytes":3,"seeders":5,
                     "magnetUrl":"magnet:?xt=urn:btih:cc","category":"Anime"}]}"#,
            ))
            .mount(&server)
            .await;

        let ix = Knaben::new(
            IndexerId::new(),
            format!("{}/v1", server.uri()),
            vec![Category(2000)],
            http(),
        );
        let titles: Vec<String> = ix.rss().await.unwrap().into_iter().map(|r| r.title).collect();
        assert_eq!(
            titles,
            vec!["Godzilla 2014 1080p BluRay", "Godzilla Singular Point"],
            "the software row goes; the anime row stays because its label is unmappable"
        );
    }

    #[tokio::test]
    async fn test_probe_asks_for_a_single_row() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .and(body_partial_json(serde_json::json!({"size": 1, "query": ""})))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"hits":[]}"#))
            .mount(&server)
            .await;
        let ix = Knaben::new(
            IndexerId::new(),
            format!("{}/v1", server.uri()),
            vec![Category(2000)],
            http(),
        );
        ix.test().await.unwrap();
    }

    /// A 200 that is not Knaben's JSON (a captive portal, an error page) must
    /// fail the health check rather than read as "reachable".
    #[tokio::test]
    async fn test_probe_rejects_a_non_knaben_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
            .mount(&server)
            .await;
        let ix = Knaben::new(
            IndexerId::new(),
            format!("{}/v1", server.uri()),
            vec![Category(2000)],
            http(),
        );
        assert!(ix.test().await.is_err());
    }
}
