//! Capability-gated tiered request generation.
//!
//! Turns a [`SearchQuery`] + [`IndexerCaps`] into an ordered chain of tiers of
//! protocol-neutral [`ProviderRequest`]s (the *arr "good part", re-derived):
//! an **ID tier** first (most precise), then a **title tier** fallback. The
//! [`execute_tiers`] driver runs them in order and **early-exits** at the first
//! tier that yields any releases.
//!
//! Requests are *semantic* (`SearchTerm`), not wire-specific — the concrete
//! protocol client (Torznab, …) maps a [`ProviderRequest`] onto actual query
//! params. Keeping it semantic makes the tiering pure and unit-testable, and
//! lets other protocols reuse it.

use skadi_core::Result;

use crate::{
    Category, IdParam, IndexerCaps, Release, SearchMode, SearchQuery, TextSearch, normalize_title,
};

/// The search term of a single request.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum SearchTerm {
    /// A single external id (e.g. `imdbid=tt...`).
    Id { param: IdParam, value: String },
    /// Several ids combined into one request (when the indexer supports it).
    AggregateIds(Vec<(IdParam, String)>),
    /// A free-text title query (raw or normalized per the indexer's caps).
    Title(String),
}

/// One request to issue against an indexer.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ProviderRequest {
    pub term: SearchTerm,
    pub categories: Vec<Category>,
    pub extra: Vec<(&'static str, String)>,
}

/// A group of requests tried together; if any yields releases, later tiers are
/// skipped.
pub type Tier = Vec<ProviderRequest>;

/// Extract `(IdParam, value)` pairs the query carries *and* the indexer supports.
fn supported_ids(query: &dyn SearchQuery, caps: &IndexerCaps) -> Vec<(IdParam, String)> {
    let e = query.external_ids();
    let mut ids = Vec::new();
    if caps.id_params.contains(&IdParam::Imdb)
        && let Some(id) = &e.imdb
    {
        ids.push((IdParam::Imdb, id.0.clone()));
    }
    if caps.id_params.contains(&IdParam::Tmdb)
        && let Some(id) = &e.tmdb
    {
        ids.push((IdParam::Tmdb, id.0.to_string()));
    }
    if caps.id_params.contains(&IdParam::Tvdb)
        && let Some(id) = &e.tvdb
    {
        ids.push((IdParam::Tvdb, id.0.to_string()));
    }
    if caps.id_params.contains(&IdParam::MusicBrainz)
        && let Some(id) = &e.musicbrainz
    {
        ids.push((IdParam::MusicBrainz, id.0.clone()));
    }
    ids
}

/// Build the ordered tier chain for a query against an indexer's capabilities.
#[must_use]
pub fn build_tiers(query: &dyn SearchQuery, caps: &IndexerCaps) -> Vec<Tier> {
    let mode = query.mode();
    let categories = query.categories().to_vec();
    let extra = query.extra_params();
    let mut tiers: Vec<Tier> = Vec::new();

    // ID tier.
    if matches!(mode, SearchMode::Auto | SearchMode::IdOnly) {
        let ids = supported_ids(query, caps);
        if !ids.is_empty() {
            let reqs = if caps.supports_aggregate_ids && ids.len() > 1 {
                vec![ProviderRequest {
                    term: SearchTerm::AggregateIds(ids),
                    categories: categories.clone(),
                    extra: extra.clone(),
                }]
            } else {
                ids.into_iter()
                    .map(|(param, value)| ProviderRequest {
                        term: SearchTerm::Id { param, value },
                        categories: categories.clone(),
                        extra: extra.clone(),
                    })
                    .collect()
            };
            tiers.push(reqs);
        }
    }

    // Title tier.
    if matches!(mode, SearchMode::Auto | SearchMode::TitleOnly) {
        let titles = query.titles();
        let year = query.year();
        if !titles.is_empty() {
            let reqs = titles
                .iter()
                .map(|t| {
                    let base = match caps.text_search {
                        TextSearch::Normalized => normalize_title(t),
                        TextSearch::Raw => t.clone(),
                    };
                    // Append the release year to the text query. Public trackers
                    // (e.g. TPB/apibay) return nothing for a long bare title but
                    // match "<title> <year>" — this is what Radarr does for
                    // text-only indexers. Skip when the title already carries the
                    // year (e.g. a scene-style alias).
                    let value = match year {
                        Some(y) if !base.contains(&y.to_string()) => format!("{base} {y}"),
                        _ => base,
                    };
                    ProviderRequest {
                        term: SearchTerm::Title(value),
                        categories: categories.clone(),
                        extra: extra.clone(),
                    }
                })
                .collect();
            tiers.push(reqs);
        }
    }

    tiers
}

/// Run the tier chain, fetching each request via `fetch`, returning the releases
/// from the first tier that yields any (early-exit). Within a tier all requests
/// are run and their results concatenated.
pub async fn execute_tiers<F, Fut>(tiers: &[Tier], fetch: F) -> Result<Vec<Release>>
where
    F: Fn(&ProviderRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Release>>>,
{
    for tier in tiers {
        let mut out = Vec::new();
        for req in tier {
            out.extend(fetch(req).await?);
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReleaseFetch;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use chrono::Utc;
    use skadi_core::{ExternalIds, ImdbId, IndexerId, MediaKind, TmdbId};
    use skadi_quality::ParsedRelease;

    struct Q {
        titles: Vec<String>,
        ids: ExternalIds,
        mode: SearchMode,
        year: Option<u16>,
    }
    impl SearchQuery for Q {
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        fn titles(&self) -> &[String] {
            &self.titles
        }
        fn year(&self) -> Option<u16> {
            self.year
        }
        fn external_ids(&self) -> &ExternalIds {
            &self.ids
        }
        fn categories(&self) -> &[Category] {
            &[]
        }
        fn mode(&self) -> SearchMode {
            self.mode
        }
    }

    fn caps(id_params: &[IdParam], aggregate: bool, text: TextSearch) -> IndexerCaps {
        IndexerCaps {
            supports_rss: true,
            supports_search: true,
            id_params: id_params.iter().copied().collect::<BTreeSet<_>>(),
            supports_aggregate_ids: aggregate,
            text_search: text,
            categories: vec![],
        }
    }

    fn query(mode: SearchMode) -> Q {
        Q {
            titles: vec!["The Matrix".to_string()],
            ids: ExternalIds {
                imdb: Some(ImdbId("tt0133093".into())),
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            mode,
            year: None,
        }
    }

    #[test]
    fn aggregate_ids_when_supported() {
        let q = query(SearchMode::Auto);
        let tiers = build_tiers(
            &q,
            &caps(&[IdParam::Imdb, IdParam::Tmdb], true, TextSearch::Raw),
        );
        // id tier (1 aggregated request) + title tier.
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].len(), 1);
        assert!(matches!(tiers[0][0].term, SearchTerm::AggregateIds(ref v) if v.len() == 2));
    }

    #[test]
    fn per_id_when_aggregate_unsupported() {
        let q = query(SearchMode::Auto);
        let tiers = build_tiers(
            &q,
            &caps(&[IdParam::Imdb, IdParam::Tmdb], false, TextSearch::Raw),
        );
        assert_eq!(tiers[0].len(), 2, "one request per id");
        assert!(
            tiers[0]
                .iter()
                .all(|r| matches!(r.term, SearchTerm::Id { .. }))
        );
    }

    #[test]
    fn id_tier_skipped_when_unsupported() {
        let q = query(SearchMode::Auto);
        // Indexer supports no id params → only the title tier.
        let tiers = build_tiers(&q, &caps(&[], false, TextSearch::Raw));
        assert_eq!(tiers.len(), 1);
        assert!(matches!(tiers[0][0].term, SearchTerm::Title(_)));
    }

    #[test]
    fn title_normalized_per_caps() {
        let q = query(SearchMode::TitleOnly);
        let raw = build_tiers(&q, &caps(&[], false, TextSearch::Raw));
        assert_eq!(raw[0][0].term, SearchTerm::Title("The Matrix".into()));
        let norm = build_tiers(&q, &caps(&[], false, TextSearch::Normalized));
        assert_eq!(norm[0][0].term, SearchTerm::Title("matrix".into()));
    }

    #[test]
    fn title_tier_appends_year() {
        // The year is appended to the text query (Radarr-style; required by
        // public trackers that don't match a bare long title) — SKADI-T-0069.
        let q = Q {
            titles: vec!["Rogue One: A Star Wars Story".to_string()],
            ids: ExternalIds::default(),
            mode: SearchMode::TitleOnly,
            year: Some(2016),
        };
        let raw = build_tiers(&q, &caps(&[], false, TextSearch::Raw));
        assert_eq!(
            raw[0][0].term,
            SearchTerm::Title("Rogue One: A Star Wars Story 2016".into())
        );
        // Not duplicated when the title already carries the year.
        let q2 = Q {
            titles: vec!["The Matrix 1999".to_string()],
            ids: ExternalIds::default(),
            mode: SearchMode::TitleOnly,
            year: Some(1999),
        };
        let raw2 = build_tiers(&q2, &caps(&[], false, TextSearch::Raw));
        assert_eq!(raw2[0][0].term, SearchTerm::Title("The Matrix 1999".into()));
    }

    #[test]
    fn mode_prunes_tiers() {
        let q = query(SearchMode::IdOnly);
        let tiers = build_tiers(&q, &caps(&[IdParam::Imdb], false, TextSearch::Raw));
        assert_eq!(tiers.len(), 1);
        assert!(matches!(tiers[0][0].term, SearchTerm::Id { .. }));
    }

    fn a_release() -> Release {
        Release {
            indexer: IndexerId::new(),
            title: "x".into(),
            fetch: ReleaseFetch::Magnet("magnet:?x".into()),
            size: 1,
            published: Utc::now(),
            seeders: None,
            categories: Vec::new(),
            parsed: ParsedRelease::default(),
        }
    }

    #[tokio::test]
    async fn executor_early_exits_after_id_tier() {
        let q = query(SearchMode::Auto);
        let tiers = build_tiers(&q, &caps(&[IdParam::Imdb], false, TextSearch::Raw));

        let seen: Mutex<Vec<SearchTerm>> = Mutex::new(Vec::new());
        let releases = execute_tiers(&tiers, |req| {
            seen.lock().unwrap().push(req.term.clone());
            let term = req.term.clone();
            async move {
                // ID tier returns a hit; title tier (if reached) would return empty.
                match term {
                    SearchTerm::Title(_) => Ok(vec![]),
                    _ => Ok(vec![a_release()]),
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(releases.len(), 1);
        let seen = seen.lock().unwrap();
        assert!(
            !seen.iter().any(|t| matches!(t, SearchTerm::Title(_))),
            "title tier must not be fetched once the ID tier yields results"
        );
    }

    #[tokio::test]
    async fn executor_falls_through_to_title_tier() {
        let q = query(SearchMode::Auto);
        let tiers = build_tiers(&q, &caps(&[IdParam::Imdb], false, TextSearch::Raw));

        let releases = execute_tiers(&tiers, |req| {
            let term = req.term.clone();
            async move {
                // ID tier empty, title tier hits.
                match term {
                    SearchTerm::Title(_) => Ok(vec![a_release()]),
                    _ => Ok(vec![]),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(releases.len(), 1);
    }
}
