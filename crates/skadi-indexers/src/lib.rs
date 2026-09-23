//! `skadi-indexers` — the indexer abstraction.
//!
//! Defines what an indexer *is* (the [`Indexer`] trait), what a search *asks
//! for* (the object-safe [`SearchQuery`] trait, implemented per-domain), what
//! it *returns* ([`Release`]), and what it can *do* ([`IndexerCaps`]). Concrete
//! protocol clients (Torznab, …) live in sibling modules/crates and implement
//! `Indexer` against this surface.
//!
//! The types here are protocol-neutral on purpose: `IndexerCaps`/`Category`
//! must serve Newznab/others later, not just Torznab.

use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{ExternalIds, IndexerId, MediaKind, Protocol, Result};
use skadi_quality::ParsedRelease;

pub mod cardigann;
pub mod categories;
pub mod config;
pub mod definitions;
pub mod flaresolverr;
pub mod health;
pub mod knaben;
pub mod normalize;
pub mod prowlarr;
pub mod ratelimit;
pub mod request;
pub mod stub;
pub mod torrent;
pub mod torznab;
pub use categories::{CategoryInfo, standard_categories};
pub use config::{
    IndexerConfig, IndexerFlags, KnabenConfig, ProwlarrConfig, StubConfig, TorznabConfig,
};
pub use health::{
    HealthTracked, IndexerHealth, IndexerHealthRegistry, IndexerHealthSnapshot, MIN_RATE_SAMPLES,
    RATE_WINDOW, indexer_health, indexer_health_tracked,
};
pub use knaben::{KNABEN_API_URL, Knaben};
pub use normalize::normalize_title;
pub use prowlarr::Prowlarr;
pub use ratelimit::{
    DEFAULT_BURST, DEFAULT_RATE_PER_MINUTE, RateLimited, RateLimiter, TokenBucket,
    indexer_rate_limited,
};
pub use request::{ProviderRequest, SearchTerm, Tier, build_tiers, execute_tiers};
pub use stub::StubIndexer;
pub use torznab::Torznab;

/// An external-id search parameter an indexer may support.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize)]
pub enum IdParam {
    Imdb,
    Tmdb,
    Tvdb,
    MusicBrainz,
}

/// How a caller wants the search steered across the ID and title tiers.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub enum SearchMode {
    /// ID tier first, then title tier (the usual policy).
    #[default]
    Auto,
    /// Only attempt ID-based search.
    IdOnly,
    /// Only attempt title-based search.
    TitleOnly,
}

/// Whether an indexer's text search wants raw or normalized titles.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum TextSearch {
    Raw,
    Normalized,
}

/// A search category (Torznab-style numeric category, kept protocol-neutral as
/// a plain code so other protocols can reuse it).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct Category(pub u32);

/// How to fetch a located release.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum ReleaseFetch {
    TorrentUrl(String),
    Magnet(String),
    /// Reserved for the deferred Usenet path (Newznab + SABnzbd).
    NzbUrl(String),
}

impl ReleaseFetch {
    /// Classify a download link **by its scheme** (SKADI-T-0565).
    ///
    /// The variant has to describe its own contents, because every consumer
    /// branches on it and none of them re-check. `CardigannIndexer::fetch`
    /// short-circuits with "magnets pass through untouched" — true only while
    /// nothing puts a magnet in [`ReleaseFetch::TorrentUrl`]. `to_release` did
    /// exactly that (it classified by which *field* the definition populated,
    /// not by what the string was), so six production snatches tried
    /// `GET magnet:?xt=urn:btih:…`, which is not a request that can succeed.
    ///
    /// Returns `None` for a link that cannot be acted on: a magnet with no
    /// `btih` hash is not addressable, and a scheme that is neither magnet nor
    /// HTTP is not something any downloader here can take.
    #[must_use]
    pub fn from_link(link: &str) -> Option<Self> {
        let link = link.trim();
        if link.starts_with("magnet:") {
            magnet_infohash(link)?;
            return Some(Self::Magnet(link.to_string()));
        }
        if link.starts_with("http://") || link.starts_with("https://") {
            return Some(Self::TorrentUrl(link.to_string()));
        }
        None
    }
}

/// A candidate release located via an indexer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Release {
    pub indexer: IndexerId,
    pub title: String,
    pub fetch: ReleaseFetch,
    pub size: u64,
    pub published: DateTime<Utc>,
    /// `None` for protocols without a seeder concept (e.g. Usenet).
    pub seeders: Option<u32>,
    /// Torznab/Newznab categories the indexer tagged this result with. Used to
    /// reject off-category junk at decide (SKADI-T-0360): a text `q=` search on a
    /// public tracker often returns games/music/apps regardless of the `cat=`
    /// filter, and without the per-result category there's nothing to filter on.
    /// May be empty when the indexer omits categories.
    pub categories: Vec<Category>,
    /// Structured parse of `title` (drives quality scoring / matching).
    pub parsed: ParsedRelease,
}

/// Canonical blocklist identity for a release (SKADI-T-0115): a stable key that
/// matches the *same* release across repeated searches. For magnets it's the
/// lowercased BitTorrent info-hash (`btih:<hash>` from the `xt=urn:btih:` param).
///
/// For a `.torrent`/NZB URL the key is **NOT** the URL: Torznab/Newznab proxies
/// (Prowlarr/Jackett) mint a fresh per-search download token, so the same release
/// gets a different URL every search — keying on it makes the blocklist useless
/// and a failed release is re-grabbed forever. Instead key on a stable content
/// identity (`indexer + normalized title + exact size`), which is identical for
/// the same release across searches. Used to record a failed release and to skip
/// it on the next search/decide.
#[must_use]
pub fn release_key(release: &Release) -> String {
    match &release.fetch {
        ReleaseFetch::Magnet(uri) => magnet_infohash(uri)
            .map(|h| format!("btih:{h}"))
            .unwrap_or_else(|| uri.clone()),
        // Stable across searches (ephemeral proxy tokens in the URL are ignored).
        ReleaseFetch::TorrentUrl(_) | ReleaseFetch::NzbUrl(_) => {
            format!(
                "rel:{}|{}|{}",
                release.indexer,
                normalize_title(&release.title),
                release.size
            )
        }
    }
}

/// Extract the lowercased `btih` info-hash from a magnet URI, if present.
fn magnet_infohash(magnet: &str) -> Option<String> {
    magnet.split(['?', '&']).find_map(|p| {
        p.strip_prefix("xt=")
            .and_then(|v| v.strip_prefix("urn:btih:"))
            .map(str::to_ascii_lowercase)
    })
}

/// The magnet's `dn=` display name (`+`→space, best-effort), if present.
fn magnet_dn(magnet: &str) -> Option<String> {
    magnet.split(['?', '&']).find_map(|p| {
        p.strip_prefix("dn=")
            .map(|s| s.replace('+', " "))
            .filter(|s| !s.trim().is_empty())
    })
}

/// The last path segment of a URL (query/fragment stripped, `.torrent` suffix
/// removed) — a fallback title for a pasted `.torrent` link.
fn link_filename(url: &str) -> Option<String> {
    url.rsplit('/')
        .next()
        .map(|s| s.split(['?', '#']).next().unwrap_or(s))
        .map(|s| s.strip_suffix(".torrent").unwrap_or(s).to_string())
        .filter(|s| !s.trim().is_empty())
}

/// Build a synthetic [`Release`] from an operator-pasted **magnet or `.torrent`
/// URL** (manual acquisition, SKADI-I-0043). The title comes from `title`, else the
/// magnet's `dn=` / the URL filename, else a placeholder; `parse` is the domain's
/// title parser (`parse`/`parse_tv`/`parse_audiobook`). Returns `None` for anything
/// that isn't a `magnet:?xt=urn:btih:…` or an `http(s)` URL. `size`/`seeders` are
/// unknown (the grab skips scoring; the import probes the real file); the indexer id
/// is a fresh synthetic one.
#[must_use]
pub fn release_from_link(
    link: &str,
    title: Option<&str>,
    parse: impl Fn(&str) -> ParsedRelease,
) -> Option<Release> {
    let link = link.trim();
    // This path already classified by scheme; it now shares the one constructor
    // with cardigann's, which did not (SKADI-T-0565).
    let fetch = ReleaseFetch::from_link(link)?;
    let derived = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if link.starts_with("magnet:") {
                magnet_dn(link)
            } else {
                link_filename(link)
            }
        })
        .unwrap_or_else(|| "Manual link".to_string());
    let parsed = parse(&derived);
    Some(Release {
        indexer: IndexerId::new(),
        title: derived,
        fetch,
        size: 0,
        published: Utc::now(),
        seeders: None,
        categories: Vec::new(),
        parsed,
    })
}

/// What an indexer can do — introspected so the request generator emits the
/// most precise query form the indexer understands.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct IndexerCaps {
    pub supports_rss: bool,
    pub supports_search: bool,
    /// External-id params the indexer accepts (e.g. `imdbid`, `tmdbid`).
    pub id_params: BTreeSet<IdParam>,
    /// Whether multiple ids can be combined in one request.
    pub supports_aggregate_ids: bool,
    /// Which title form the indexer's text search expects.
    pub text_search: TextSearch,
    /// Categories the indexer exposes.
    pub categories: Vec<Category>,
}

/// What a search is looking for. Implemented by each *domain* (movies builds a
/// `MovieQuery`), consumed as `&dyn SearchQuery` by the indexer layer — so this
/// trait MUST stay object-safe (no generics / associated types / `Self`-returns).
pub trait SearchQuery: Send + Sync {
    /// The media kind, so an indexer can decline (`supports`) unrelated kinds.
    fn kind(&self) -> MediaKind;
    /// Raw title aliases to try in the title tier (primary first).
    fn titles(&self) -> &[String];
    /// Release year, when known.
    fn year(&self) -> Option<u16>;
    /// External identifiers for the ID tier.
    fn external_ids(&self) -> &ExternalIds;
    /// Categories to scope the search to.
    fn categories(&self) -> &[Category];
    /// Scope-specific extra provider params (e.g. `season`/`ep` for TV). Empty
    /// for movies.
    fn extra_params(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }
    /// How to steer the tiers.
    fn mode(&self) -> SearchMode {
        SearchMode::Auto
    }
}

/// A configured indexer.
#[async_trait]
pub trait Indexer: Send + Sync {
    fn id(&self) -> IndexerId;
    fn protocol(&self) -> Protocol;
    /// Whether this indexer can serve the given media kind.
    fn supports(&self, kind: MediaKind) -> bool;
    /// Negotiate capabilities (typically `t=caps`).
    async fn capabilities(&self) -> Result<IndexerCaps>;

    /// Whether the RSS sweep should poll this indexer (SKADI-T-0505). Defaults to
    /// `true` so an indexer that predates the flag, or one with no config behind
    /// it (the stub), behaves exactly as before.
    fn enable_rss(&self) -> bool {
        true
    }

    /// Whether automatic (sweep-driven) searches should use this indexer
    /// (SKADI-T-0505). An interactive search the operator drives by hand ignores
    /// this — that is the whole point of turning it off.
    fn enable_automatic_search(&self) -> bool {
        true
    }

    /// Whether this indexer applies to an item carrying `item_tags`
    /// (SKADI-T-0556).
    ///
    /// The default is `true` for **every** item — an indexer that has not opted
    /// into tag scoping must keep serving everything. Returning `false` by
    /// default would take every untagged indexer out of the fan-out on upgrade,
    /// and the symptom would be silent: searches simply stop finding things.
    fn applies_to_tags(&self, _item_tags: &[String]) -> bool {
        true
    }

    /// Tie-break order across indexers, lower first (Sonarr's 1-50, default 25)
    /// — SKADI-T-0539. The operator's statement that they trust one tracker's
    /// releases over another's.
    fn priority(&self) -> u32 {
        25
    }

    /// Per-indexer seeder floor overriding the profile's; `0` ⇒ use the
    /// profile's (SKADI-T-0539).
    fn minimum_seeders(&self) -> u32 {
        0
    }
    /// Run a search and return located releases.
    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>>;
    /// Pull the indexer's **recent-releases feed** — the cheap RSS pass the hunter
    /// runs on a fast tick to catch newly-posted releases within minutes, without
    /// a per-item search fan-out (SKADI-T-0192). Newest-first, no query term.
    ///
    /// Default: an empty feed, for indexers that don't support RSS (the built-in
    /// stub, mocks). Real clients override it — for the torznab/Prowlarr family
    /// it's a query-less `t=search` (validated against a live Prowlarr 2.4: an
    /// empty-`query` aggregate search returns the recent feed, newest-first).
    async fn rss(&self) -> Result<Vec<Release>> {
        Ok(Vec::new())
    }
    /// Resolve a chosen release's [`ReleaseFetch`] at grab time, just before it's
    /// handed to a downloader. This is the indexer's chance to turn an **unresolved
    /// detail-page link into a real fetch** — e.g. a cardigann `download` block that
    /// scrapes the info-hash off the torrent's detail page and builds a magnet
    /// (AudioBookBay-style, SKADI-T-0306).
    ///
    /// Default: a no-op — the search row already yielded the final magnet/`.torrent`,
    /// which is the case for the overwhelming majority of indexers (so there's no
    /// extra fetch on the common path).
    async fn resolve_fetch(&self, fetch: &ReleaseFetch) -> Result<ReleaseFetch> {
        Ok(fetch.clone())
    }
    /// Verify reachability + credentials (SKADI-T-0061).
    ///
    /// **Required, no default** — a provider that can't fail its health check
    /// is worse than none, so every implementor (mocks included) declares its
    /// own. Real clients make a cheap authenticated call (e.g. `t=caps`).
    async fn test(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::TmdbId;

    // A minimal domain-style query, proving `SearchQuery` is implementable and
    // object-safe (`&dyn SearchQuery`).
    struct MovieQuery {
        titles: Vec<String>,
        ids: ExternalIds,
        cats: Vec<Category>,
    }

    impl SearchQuery for MovieQuery {
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        fn titles(&self) -> &[String] {
            &self.titles
        }
        fn year(&self) -> Option<u16> {
            Some(1999)
        }
        fn external_ids(&self) -> &ExternalIds {
            &self.ids
        }
        fn categories(&self) -> &[Category] {
            &self.cats
        }
    }

    #[test]
    fn search_query_is_object_safe() {
        let q = MovieQuery {
            titles: vec!["The Matrix".to_string()],
            ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            cats: vec![Category(2000)],
        };
        let dynq: &dyn SearchQuery = &q;
        assert_eq!(dynq.kind(), MediaKind::Movie);
        assert_eq!(dynq.year(), Some(1999));
        assert_eq!(dynq.mode(), SearchMode::Auto);
        assert!(dynq.extra_params().is_empty());
        assert_eq!(dynq.external_ids().tmdb, Some(TmdbId(603)));
    }

    #[test]
    fn release_round_trips() {
        let r = Release {
            indexer: IndexerId::new(),
            title: "The.Matrix.1999.1080p.BluRay.x264-AMIABLE".to_string(),
            fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".to_string()),
            size: 8_000_000_000,
            published: Utc::now(),
            seeders: Some(42),
            categories: Vec::new(),
            parsed: ParsedRelease::default(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: Release = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    fn rel(fetch: ReleaseFetch) -> Release {
        Release {
            indexer: IndexerId::new(),
            title: "t".into(),
            fetch,
            size: 1,
            published: Utc::now(),
            seeders: None,
            categories: Vec::new(),
            parsed: ParsedRelease::default(),
        }
    }

    #[test]
    fn release_key_uses_magnet_infohash() {
        // Magnet → lowercased btih hash, stable across differing trackers/names.
        let a = rel(ReleaseFetch::Magnet(
            "magnet:?xt=urn:btih:DD8255ECDC7CA55FB0BBF81323D87062DB1F6D1C&dn=Movie".into(),
        ));
        let b = rel(ReleaseFetch::Magnet(
            "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&tr=udp://x".into(),
        ));
        assert_eq!(release_key(&a), release_key(&b));
        assert_eq!(
            release_key(&a),
            "btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
        );
    }

    #[test]
    fn release_key_url_is_stable_identity_not_the_url() {
        // The SAME release re-fetched through a Torznab/Prowlarr proxy gets a
        // fresh per-search download token in the URL. The key must ignore that
        // (key on indexer + normalized title + size) so a failed release stays
        // blocklisted across searches.
        let indexer = IndexerId::new();
        let make = |url: &str| Release {
            indexer,
            title: "Andy Weir - Project Hail Mary [M4B 128kbps]".into(),
            fetch: ReleaseFetch::TorrentUrl(url.into()),
            size: 932_188_736,
            published: Utc::now(),
            seeders: Some(42),
            categories: Vec::new(),
            parsed: ParsedRelease::default(),
        };
        let first = make("http://gluetun:9696/1/download?apikey=k&link=AAAA&file=x");
        let second = make("http://gluetun:9696/1/download?apikey=k&link=BBBB&file=x");
        assert_eq!(
            release_key(&first),
            release_key(&second),
            "same release, different proxy tokens => same key"
        );

        // A genuinely different release (different size) keys differently.
        let mut bigger = make("http://gluetun:9696/1/download?apikey=k&link=CCCC");
        bigger.size = 700_000_000;
        assert_ne!(release_key(&first), release_key(&bigger));

        // A magnet with no btih param falls back to the raw URI.
        assert_eq!(
            release_key(&rel(ReleaseFetch::Magnet("magnet:?dn=nohash".into()))),
            "magnet:?dn=nohash"
        );
    }
}
