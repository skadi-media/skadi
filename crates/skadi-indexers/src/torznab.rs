//! Torznab indexer client (hand-rolled, no third-party torznab crate).
//!
//! Speaks the public Torznab protocol (Newznab + torrent `torznab:attr`
//! extensions): `t=caps` → [`IndexerCaps`], `t=movie`/`t=search` → [`Release`]s.
//! Wire-level query-param mapping lives here; the protocol-neutral tier logic is
//! reused from [`crate::request`]. XML is parsed event-by-event with `quick-xml`
//! (our own parser — nothing vendored).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;

use skadi_core::{AppError, IndexerId, MediaKind, Protocol, Result};
use skadi_http::HttpClient;

use crate::request::{ProviderRequest, SearchTerm, build_tiers, execute_tiers};
use crate::{
    Category, IdParam, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};

/// A configured Torznab indexer endpoint.
pub struct Torznab {
    id: IndexerId,
    base_url: String,
    api_key: String,
    categories: Vec<Category>,
    http: HttpClient,
    /// Cached `t=caps` (SKADI-T-0512). Capabilities describe what the indexer
    /// *supports* — its modes, id params and category tree — which changes when
    /// the operator upgrades their indexer, not between two searches a second
    /// apart. Re-fetching per search doubled every search's round trips and its
    /// contribution to any rate limit, for an answer that had not changed.
    ///
    /// Held for the life of the `Torznab` value. That is the right lifetime
    /// rather than a TTL: the provider set is rebuilt whenever indexer settings
    /// change (SKADI-T-0459), so a re-configured indexer gets a fresh client and
    /// therefore a fresh fetch, with no staleness window to tune.
    caps: tokio::sync::OnceCell<IndexerCaps>,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    flags: crate::config::IndexerFlags,
}

impl Torznab {
    /// Construct a client for `base_url` (the indexer root; `/api` is appended).
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
            caps: tokio::sync::OnceCell::new(),
            flags: crate::config::IndexerFlags::default(),
        }
    }

    /// Apply the stored per-indexer flags (SKADI-T-0505).
    #[must_use]
    pub fn with_flags(mut self, flags: crate::config::IndexerFlags) -> Self {
        self.flags = flags;
        self
    }

    fn api(&self) -> String {
        format!("{}/api", self.base_url.trim_end_matches('/'))
    }

    /// Map a protocol-neutral [`ProviderRequest`] onto Torznab query params.
    fn query_params(&self, req: &ProviderRequest) -> Vec<(String, String)> {
        let mut params: Vec<(String, String)> = Vec::new();
        match &req.term {
            SearchTerm::Id { param, value } => {
                params.push(("t".into(), t_for(*param).into()));
                params.push((id_param(*param).into(), id_value(*param, value)));
            }
            SearchTerm::AggregateIds(ids) => {
                params.push(("t".into(), "movie".into()));
                for (param, value) in ids {
                    params.push((id_param(*param).into(), id_value(*param, value)));
                }
            }
            SearchTerm::Title(q) => {
                params.push(("t".into(), "search".into()));
                params.push(("q".into(), q.clone()));
            }
        }
        let cats = if req.categories.is_empty() {
            &self.categories
        } else {
            &req.categories
        };
        if !cats.is_empty() {
            let joined = cats
                .iter()
                .map(|c| c.0.to_string())
                .collect::<Vec<_>>()
                .join(",");
            params.push(("cat".into(), joined));
        }
        for (k, v) in &req.extra {
            params.push(((*k).to_string(), v.clone()));
        }
        params.push(("apikey".into(), self.api_key.clone()));
        params
    }

    async fn fetch(&self, req: &ProviderRequest) -> Result<Vec<Release>> {
        let params = self.query_params(req);
        let api = self.api();
        let resp = self
            .http
            .send_idempotent(|c| c.get(&api).query(&params))
            .await?;
        let xml = resp
            .text()
            .await
            .map_err(|e| AppError::Network(format!("reading Torznab response: {e}")))?;
        parse_releases(&xml, self.id)
    }
}

#[async_trait]
impl Indexer for Torznab {
    fn id(&self) -> IndexerId {
        self.id
    }

    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    /// Whether this endpoint serves a domain, derived from its configured
    /// Torznab/Newznab categories: the 2000s are Movies, the 3000s are Audio
    /// (3030 = Audiobook), the 5000s are TV. A Torznab indexer registered with
    /// `cat 3030` thus serves the audiobooks domain (SKADI-I-0017); one with
    /// `cat 2000` serves movies and one with `cat 5000` serves TV
    /// (SKADI-T-0499 — the TV range was missing, so the whole Torznab path was
    /// invisible to the TV domain). When no categories are configured we default
    /// to Movies for backwards compatibility with v0 movie-only indexers.
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

    /// A `t=caps` round-trip: proves the endpoint is reachable and the API key
    /// is accepted.
    async fn test(&self) -> Result<()> {
        self.capabilities().await.map(|_| ())
    }

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

    async fn capabilities(&self) -> Result<IndexerCaps> {
        // Cached after the first success (SKADI-T-0512). `get_or_try_init` does
        // NOT cache a failure, which is what we want: an indexer that was down
        // when the daemon started must be re-asked, not written off for the life
        // of the process.
        self.caps
            .get_or_try_init(|| async {
                let api = self.api();
                let key = self.api_key.clone();
                let resp = self
                    .http
                    .send_idempotent(|c| {
                        c.get(&api)
                            .query(&[("t", "caps"), ("apikey", key.as_str())])
                    })
                    .await?;
                let xml = resp
                    .text()
                    .await
                    .map_err(|e| AppError::Network(format!("reading Torznab caps: {e}")))?;
                parse_caps(&xml)
            })
            .await
            .cloned()
    }

    /// The recent-releases feed (SKADI-T-0192): a query-less `t=search` over the
    /// configured categories — the classic torznab RSS feed. Newest-first as the
    /// upstream orders it.
    async fn rss(&self) -> Result<Vec<Release>> {
        let req = ProviderRequest {
            term: SearchTerm::Title(String::new()),
            categories: self.categories.clone(),
            extra: Vec::new(),
        };
        self.fetch(&req).await
    }

    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        let caps = self.capabilities().await?;
        let tiers = build_tiers(query, &caps);
        execute_tiers(&tiers, |req| {
            // Clone so the returned future doesn't borrow the per-call `&req`
            // (keeps the `Fn(&ProviderRequest) -> Fut` bound satisfiable).
            let req = req.clone();
            async move { self.fetch(&req).await }
        })
        .await
    }
}

fn t_for(param: IdParam) -> &'static str {
    match param {
        IdParam::Imdb | IdParam::Tmdb => "movie",
        IdParam::Tvdb => "tvsearch",
        IdParam::MusicBrainz => "music",
    }
}

fn id_param(param: IdParam) -> &'static str {
    match param {
        IdParam::Imdb => "imdbid",
        IdParam::Tmdb => "tmdbid",
        IdParam::Tvdb => "tvdbid",
        IdParam::MusicBrainz => "musicbrainzid",
    }
}

fn id_value(param: IdParam, value: &str) -> String {
    match param {
        // Torznab `imdbid` is the numeric part without the leading "tt".
        IdParam::Imdb => value.trim_start_matches("tt").to_string(),
        _ => value.to_string(),
    }
}

/// Parse a Torznab `t=caps` document into [`IndexerCaps`].
pub fn parse_caps(xml: &str) -> Result<IndexerCaps> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut supports_search = false;
    let mut id_params = std::collections::BTreeSet::new();
    let mut categories = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| AppError::Internal(format!("parsing Torznab caps XML: {e}")))?
        {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    b"search" | b"movie-search" | b"tv-search" | b"movie" | b"tvsearch" => {
                        let mut available = false;
                        let mut supported = String::new();
                        for attr in e.attributes().flatten() {
                            match attr.key.local_name().as_ref() {
                                b"available" => {
                                    available =
                                        attr.unescape_value().map(|v| v == "yes").unwrap_or(false);
                                }
                                b"supportedParams" => {
                                    supported = attr
                                        .unescape_value()
                                        .map(|v| v.into_owned())
                                        .unwrap_or_default();
                                }
                                _ => {}
                            }
                        }
                        if available {
                            supports_search = true;
                            for p in supported.split(',') {
                                match p.trim() {
                                    "imdbid" => {
                                        id_params.insert(IdParam::Imdb);
                                    }
                                    "tmdbid" => {
                                        id_params.insert(IdParam::Tmdb);
                                    }
                                    "tvdbid" => {
                                        id_params.insert(IdParam::Tvdb);
                                    }
                                    "musicbrainzid" => {
                                        id_params.insert(IdParam::MusicBrainz);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    b"category" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"id"
                                && let Ok(v) = attr.unescape_value()
                                && let Ok(n) = v.parse::<u32>()
                            {
                                categories.push(Category(n));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(IndexerCaps {
        supports_rss: true,
        supports_search,
        id_params,
        // Torznab caps don't advertise aggregate-id support; be conservative.
        supports_aggregate_ids: false,
        text_search: TextSearch::Raw,
        categories,
    })
}

#[derive(Default)]
struct ItemAcc {
    title: String,
    link: Option<String>,
    pub_date: Option<String>,
    size: Option<u64>,
    /// `<enclosure length=…>` — the size fallback when no `size` attr is given
    /// (SKADI-T-0507).
    enclosure_length: Option<u64>,
    /// A bare `infohash` attr, from which a magnet is built (SKADI-T-0507).
    infohash: Option<String>,
    seeders: Option<u32>,
    categories: Vec<u32>,
}

impl ItemAcc {
    fn into_release(self, indexer: IndexerId) -> Option<Release> {
        if self.title.is_empty() {
            return None;
        }
        // Prefer an explicit link; fall back to building a magnet from a bare
        // info-hash (SKADI-T-0507). Without the fallback such an item had no
        // fetch and was dropped.
        let fetch = match self.link {
            Some(link) if link.starts_with("magnet:") => ReleaseFetch::Magnet(link),
            Some(link) => ReleaseFetch::TorrentUrl(link),
            None => {
                // Lowercased: hex in a magnet's `btih` is case-insensitive by
                // spec, but lowercase is the canonical form every client and
                // tracker emits, and it keeps `release_key` de-duplication from
                // treating the same torrent as two just because one indexer
                // shouted its hash.
                let hash = self.infohash?.to_ascii_lowercase();
                ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{hash}"))
            }
        };
        let published = self
            .pub_date
            .as_deref()
            .and_then(|d| DateTime::parse_from_rfc2822(d).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let parsed = skadi_quality::parse(&self.title);
        Some(Release {
            indexer,
            title: self.title,
            fetch,
            // `size` attr first, then the enclosure length (SKADI-T-0507).
            size: self.size.or(self.enclosure_length).unwrap_or(0),
            published,
            seeders: self.seeders,
            categories: self.categories.into_iter().map(Category).collect(),
            parsed,
        })
    }
}

/// Parse a Torznab search response (RSS) into [`Release`]s.
pub fn parse_releases(xml: &str, indexer: IndexerId) -> Result<Vec<Release>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut releases = Vec::new();
    let mut cur: Option<ItemAcc> = None;
    let mut text_field: Option<Field> = None;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| AppError::Internal(format!("parsing Torznab response XML: {e}")))?
        {
            Event::Eof => break,
            Event::Start(e) => match e.local_name().as_ref() {
                b"item" => cur = Some(ItemAcc::default()),
                b"title" => text_field = Some(Field::Title),
                b"link" => text_field = Some(Field::Link),
                b"pubDate" => text_field = Some(Field::PubDate),
                b"size" => text_field = Some(Field::Size),
                _ => {}
            },
            Event::Empty(e) => {
                let Some(item) = cur.as_mut() else { continue };
                match e.local_name().as_ref() {
                    b"enclosure" => {
                        for attr in e.attributes().flatten() {
                            let Ok(v) = attr.unescape_value() else {
                                continue;
                            };
                            match attr.key.local_name().as_ref() {
                                b"url" if item.link.is_none() => {
                                    item.link = Some(v.into_owned());
                                }
                                // Newznab's original convention (SKADI-T-0507):
                                // the byte count lives on the enclosure, and only
                                // newer indexers also emit a `size` attr. Without
                                // this a release from an older indexer had size 0,
                                // which the size-sanity gate reads as "unknown" —
                                // so nothing checked it, and the free-space
                                // pre-check had nothing to reserve against.
                                //
                                // A `size` attr still wins: it is the explicit
                                // statement, and `length` is sometimes the padded
                                // on-the-wire size.
                                b"length" if item.enclosure_length.is_none() => {
                                    item.enclosure_length = v.parse().ok();
                                }
                                _ => {}
                            }
                        }
                    }
                    // torznab:attr / newznab:attr name=.. value=..
                    b"attr" => {
                        let mut a_name = String::new();
                        let mut a_value = String::new();
                        for attr in e.attributes().flatten() {
                            match attr.key.local_name().as_ref() {
                                b"name" => {
                                    a_name = attr
                                        .unescape_value()
                                        .map(|v| v.into_owned())
                                        .unwrap_or_default();
                                }
                                b"value" => {
                                    a_value = attr
                                        .unescape_value()
                                        .map(|v| v.into_owned())
                                        .unwrap_or_default();
                                }
                                _ => {}
                            }
                        }
                        match a_name.as_str() {
                            "seeders" => item.seeders = a_value.parse().ok(),
                            "size" if item.size.is_none() => item.size = a_value.parse().ok(),
                            "magneturl" if item.link.is_none() => item.link = Some(a_value),
                            // Some indexers publish only the info-hash and expect
                            // the client to build the magnet, as Sonarr does
                            // (SKADI-T-0507). Without this the item had no fetch
                            // at all and was dropped silently — a grabbable
                            // release that simply never appeared.
                            "infohash" if item.infohash.is_none() => {
                                item.infohash = Some(a_value);
                            }
                            // Per-result category for the decide-time off-category gate
                            // (SKADI-T-0360). Torznab emits one attr per category id.
                            "category" => {
                                if let Ok(c) = a_value.parse::<u32>() {
                                    item.categories.push(c);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let (Some(item), Some(field)) = (cur.as_mut(), text_field) {
                    let s = t
                        .unescape()
                        .map_err(|e| AppError::Internal(format!("Torznab text: {e}")))?
                        .into_owned();
                    match field {
                        Field::Title => item.title = s,
                        Field::Link => {
                            if item.link.is_none() {
                                item.link = Some(s);
                            }
                        }
                        Field::PubDate => item.pub_date = Some(s),
                        Field::Size => item.size = s.parse().ok(),
                    }
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"item" => {
                    if let Some(item) = cur.take()
                        && let Some(release) = item.into_release(indexer)
                    {
                        releases.push(release);
                    }
                }
                b"title" | b"link" | b"pubDate" | b"size" => text_field = None,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }

    Ok(releases)
}

#[derive(Copy, Clone)]
enum Field {
    Title,
    Link,
    PubDate,
    Size,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchMode;
    use skadi_core::{ExternalIds, ImdbId};
    use std::time::Duration;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CAPS_XML: &str = r#"<?xml version="1.0"?>
<caps>
  <searching>
    <search available="yes" supportedParams="q" />
    <movie-search available="yes" supportedParams="q,imdbid,tmdbid" />
  </searching>
  <categories>
    <category id="2000" name="Movies" />
  </categories>
</caps>"#;

    const SEARCH_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Blade.Runner.1982.2160p.UHD.BluRay.x265-GROUP</title>
      <link>magnet:?xt=urn:btih:deadbeef</link>
      <pubDate>Tue, 10 Jan 2023 12:00:00 +0000</pubDate>
      <size>15000000000</size>
      <torznab:attr name="seeders" value="42" />
      <torznab:attr name="category" value="2000" />
      <torznab:attr name="category" value="2040" />
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_caps() {
        let caps = parse_caps(CAPS_XML).unwrap();
        assert!(caps.supports_search);
        assert!(caps.id_params.contains(&IdParam::Imdb));
        assert!(caps.id_params.contains(&IdParam::Tmdb));
        assert_eq!(caps.categories, vec![Category(2000)]);
    }

    #[test]
    fn supports_is_category_driven() {
        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let mk = |cats: Vec<Category>| {
            Torznab::new(IndexerId::new(), "http://x", "k", cats, http.clone())
        };
        // 3030 (audiobook) → serves audiobooks, not movies.
        let ab = mk(vec![Category(3030)]);
        assert!(ab.supports(MediaKind::Audiobook));
        assert!(!ab.supports(MediaKind::Movie));
        // 2000 (movies) → serves movies, not audiobooks.
        let mv = mk(vec![Category(2000)]);
        assert!(mv.supports(MediaKind::Movie));
        assert!(!mv.supports(MediaKind::Audiobook));
        // No categories → default to movies (v0 back-compat).
        let none = mk(vec![]);
        assert!(none.supports(MediaKind::Movie));
        assert!(!none.supports(MediaKind::Audiobook));
    }

    #[test]
    fn parses_releases() {
        let rels = parse_releases(SEARCH_XML, IndexerId::new()).unwrap();
        assert_eq!(rels.len(), 1);
        let r = &rels[0];
        assert_eq!(r.seeders, Some(42));
        assert_eq!(r.size, 15_000_000_000);
        assert!(matches!(r.fetch, ReleaseFetch::Magnet(_)));
        // title was run through the quality parser
        assert_eq!(r.parsed.year, Some(1982));
        assert_eq!(r.parsed.resolution.as_deref(), Some("2160p"));
        // per-result categories are captured for the off-category gate (T-0360)
        assert_eq!(r.categories, vec![Category(2000), Category(2040)]);
    }

    #[test]
    fn imdbid_strips_tt() {
        assert_eq!(id_value(IdParam::Imdb, "tt0083658"), "0083658");
        assert_eq!(id_value(IdParam::Tmdb, "603"), "603");
    }

    struct MovieQuery {
        titles: Vec<String>,
        ids: ExternalIds,
    }
    impl SearchQuery for MovieQuery {
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        fn titles(&self) -> &[String] {
            &self.titles
        }
        fn year(&self) -> Option<u16> {
            Some(1982)
        }
        fn external_ids(&self) -> &ExternalIds {
            &self.ids
        }
        fn categories(&self) -> &[Category] {
            &[]
        }
        fn mode(&self) -> SearchMode {
            SearchMode::Auto
        }
    }

    // Conformance harness: a fake Torznab server validates the wire mapping +
    // tiering end-to-end. The id tier returns a hit, so the title tier (t=search)
    // must never be requested (early-exit).
    #[tokio::test]
    async fn conformance_id_tier_hits_and_title_tier_is_skipped() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("t", "caps"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CAPS_XML))
            .mount(&server)
            .await;

        // ID-tier movie search by imdbid → returns a release.
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("t", "movie"))
            .and(query_param("imdbid", "0133093"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH_XML))
            .expect(1)
            .mount(&server)
            .await;

        // Title-tier text search must NOT be hit once the ID tier yields results.
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("t", "search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH_XML))
            .expect(0)
            .mount(&server)
            .await;

        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let torznab = Torznab::new(
            IndexerId::new(),
            server.uri(),
            "secret",
            vec![Category(2000)],
            http,
        );

        // imdb-only so the ID tier is a single request (multiple ids would
        // fan out to one request each — cross-id dedup is a future concern).
        let query = MovieQuery {
            titles: vec!["Blade Runner".to_string()],
            ids: ExternalIds {
                imdb: Some(ImdbId("tt0133093".into())),
                ..Default::default()
            },
        };

        let releases = torznab.search(&query).await.unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].seeders, Some(42));
        // wiremock verifies the expect(0)/expect(1) counts on server drop.
    }

    /// RSS = a query-less `t=search` over the configured categories (the classic
    /// torznab recent feed). No `t=caps` round-trip — RSS is a direct feed pull.
    #[tokio::test]
    async fn rss_does_a_query_less_t_search() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("t", "search"))
            .and(query_param("q", ""))
            .and(query_param("cat", "2000"))
            .and(query_param("apikey", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH_XML))
            .expect(1)
            .mount(&server)
            .await;
        // caps must NOT be hit for an RSS pull.
        Mock::given(method("GET"))
            .and(path("/api"))
            .and(query_param("t", "caps"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CAPS_XML))
            .expect(0)
            .mount(&server)
            .await;

        let http = HttpClient::new(Duration::from_secs(5)).unwrap();
        let torznab = Torznab::new(
            IndexerId::new(),
            server.uri(),
            "secret",
            vec![Category(2000)],
            http,
        );
        let releases = torznab.rss().await.unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].seeders, Some(42));
        assert!(matches!(releases[0].fetch, ReleaseFetch::Magnet(_)));
    }
}
