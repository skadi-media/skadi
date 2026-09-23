//! Built-in **stub** indexer (SKADI-T-0104) — canned, downloadable releases for
//! `testing` mode, so the whole acquire pipeline (search → decide → snatch →
//! download → import) runs with **no external indexer** (Prowlarr/Jackett).
//! Parallels the built-in `skadi` downloader.
//!
//! The catalog is a handful of open movies whose `fetch` points at the same
//! webseed-backed torrents the worker functional test downloads (Sintel / Big
//! Buck Bunny / Cosmos Laundromat). On `search` it returns the catalog entries
//! whose title matches the query, named as a 1080p BluRay release so the
//! Standard profile accepts them and the importer matches the downloaded file
//! back to the added movie.

use async_trait::async_trait;
use std::collections::BTreeSet;

use skadi_core::{IndexerId, MediaKind, Protocol, Result};
use skadi_quality::parse;

use crate::{Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch};

/// One open-movie entry in the stub catalog.
struct CannedMovie {
    title: &'static str,
    year: u16,
    /// Approximate size in bytes (for display/scoring; not enforced).
    size: u64,
    fetch: fn() -> ReleaseFetch,
}

/// The canned set — open movies with real, webseed-backed torrents (the ones
/// the worker functional test proves downloadable).
fn catalog() -> &'static [CannedMovie] {
    &[
        CannedMovie {
            title: "Sintel",
            year: 2010,
            size: 1_300_000_000,
            fetch: || {
                ReleaseFetch::Magnet(
                    "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10\
                     &dn=Sintel\
                     &tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce\
                     &tr=udp%3A%2F%2Fexplodie.org%3A6969\
                     &ws=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2F\
                     &xs=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2Fsintel.torrent"
                        .into(),
                )
            },
        },
        CannedMovie {
            title: "Big Buck Bunny",
            year: 2008,
            size: 350_000_000,
            fetch: || {
                ReleaseFetch::TorrentUrl(
                    "https://webtorrent.io/torrents/big-buck-bunny.torrent".into(),
                )
            },
        },
        CannedMovie {
            title: "Cosmos Laundromat",
            year: 2015,
            size: 600_000_000,
            fetch: || {
                ReleaseFetch::TorrentUrl(
                    "https://webtorrent.io/torrents/cosmos-laundromat.torrent".into(),
                )
            },
        },
    ]
}

/// Normalize a title for matching: lowercase, keep alphanumerics, collapse
/// whitespace to single spaces.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_space = false;
        } else if !prev_space && !out.is_empty() {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim().to_string()
}

/// The built-in stub indexer. Returns canned open-movie releases matching the
/// query title.
pub struct StubIndexer {
    id: IndexerId,
}

impl StubIndexer {
    #[must_use]
    pub fn new(id: IndexerId) -> Self {
        Self { id }
    }

    /// Build the canned release for a catalog entry, named as a 1080p BluRay
    /// release so it classifies + passes a Standard profile.
    fn release_for(&self, movie: &CannedMovie) -> Release {
        let title = format!("{} {} 1080p BluRay x264-STUB", movie.title, movie.year);
        let parsed = parse(&title);
        Release {
            indexer: self.id,
            title,
            fetch: (movie.fetch)(),
            size: movie.size,
            published: chrono::Utc::now(),
            seeders: Some(100),
            categories: Vec::new(),
            parsed,
        }
    }
}

#[async_trait]
impl Indexer for StubIndexer {
    fn id(&self) -> IndexerId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn test(&self) -> Result<()> {
        Ok(())
    }
    async fn capabilities(&self) -> Result<IndexerCaps> {
        Ok(IndexerCaps {
            supports_rss: false,
            supports_search: true,
            id_params: BTreeSet::new(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: vec![Category(2000)],
        })
    }
    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        if query.kind() != MediaKind::Movie {
            return Ok(vec![]);
        }
        let wanted: Vec<String> = query.titles().iter().map(|t| normalize(t)).collect();
        let matches = catalog()
            .iter()
            .filter(|m| {
                let cat = normalize(m.title);
                wanted.iter().any(|w| w.contains(&cat) || cat.contains(w))
            })
            .map(|m| self.release_for(m))
            .collect();
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::ExternalIds;

    struct Q {
        titles: Vec<String>,
    }
    impl SearchQuery for Q {
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        fn titles(&self) -> &[String] {
            &self.titles
        }
        fn year(&self) -> Option<u16> {
            None
        }
        fn external_ids(&self) -> &ExternalIds {
            // A leaked default is fine for a test stub.
            Box::leak(Box::new(ExternalIds::default()))
        }
        fn categories(&self) -> &[Category] {
            &[]
        }
    }

    #[tokio::test]
    async fn matches_by_title_and_classifies_1080p() {
        let ix = StubIndexer::new(IndexerId::new());
        let res = ix
            .search(&Q {
                titles: vec!["Big Buck Bunny".into()],
            })
            .await
            .unwrap();
        assert_eq!(res.len(), 1, "one catalog match");
        assert!(res[0].title.contains("Big Buck Bunny"));
        assert!(res[0].title.contains("1080p"));
        assert!(matches!(res[0].fetch, ReleaseFetch::TorrentUrl(_)));
        assert_eq!(res[0].seeders, Some(100));
    }

    #[tokio::test]
    async fn sintel_is_a_magnet() {
        let ix = StubIndexer::new(IndexerId::new());
        let res = ix
            .search(&Q {
                titles: vec!["sintel".into()],
            })
            .await
            .unwrap();
        assert_eq!(res.len(), 1);
        assert!(matches!(res[0].fetch, ReleaseFetch::Magnet(_)));
    }

    #[tokio::test]
    async fn unknown_title_returns_nothing() {
        let ix = StubIndexer::new(IndexerId::new());
        let res = ix
            .search(&Q {
                titles: vec!["The Matrix".into()],
            })
            .await
            .unwrap();
        assert!(res.is_empty());
    }
}
