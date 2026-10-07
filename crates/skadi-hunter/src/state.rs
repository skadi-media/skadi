//! The per-run acquisition state and its Cloacina-context helpers.
//!
//! Cloacina's context is `Context<serde_json::Value>` (a JSON key/value store),
//! not a typed context — so [`AcquireState`] is **serialized into** the context
//! under a single key ([`STATE_KEY`]) rather than being the context's type
//! parameter. [`load_state`] / [`store_state`] are the only sanctioned way the
//! workflow tasks read and write it.

use cloacina::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use skadi_core::{AppError, ExternalIds, MediaKind, ProfileId, QualityId, Result};
use skadi_downloaders::DownloadHandle;
use skadi_importer::{AcquirableRef, ImportOutcome};
use skadi_indexers::{Category, Release, SearchMode, SearchQuery};

/// The context key the serialized [`AcquireState`] lives under.
pub const STATE_KEY: &str = "state";

/// The serializable search inputs for a run, seeded by the domain when it starts
/// an acquisition. `SearchQuery` is a *domain* trait the indexer layer consumes
/// as `&dyn SearchQuery`; the hunter turns this serde-friendly spec into one via
/// [`SearchSpec::query`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SearchSpec {
    pub kind: MediaKind,
    /// What started this search (SKADI-T-0539). Governs whether an indexer with
    /// `enable_automatic_search: false` is consulted.
    ///
    /// Defaults to [`SearchTrigger::Automatic`] so an in-flight run serialised
    /// before this field existed keeps working — every such run came from a
    /// sweep, so the default is also the truth about it.
    #[serde(default)]
    pub trigger: SearchTrigger,
    /// Title aliases to try, primary first.
    pub titles: Vec<String>,
    pub year: Option<u16>,
    pub external_ids: ExternalIds,
    pub categories: Vec<Category>,
    /// TV season/episode scope (`None` for movies). Drives the Torznab `season`/
    /// `ep` `extra_params` for a tv-search; a season pack sets `episode: None`.
    #[serde(default)]
    pub tv: Option<TvScope>,
    /// Audiobook series name (SKADI-T-0313), when the acquired book is in a series. The
    /// hunter also queries this so a multi-book pack is *found*, and `decide` gates packs on
    /// **its** coverage (vs. the per-book `titles` gate) and prefers them (SKADI-T-0314).
    /// `None` for standalone books and other domains.
    #[serde(default)]
    pub series: Option<String>,
    /// Tag ids on the item being acquired (SKADI-T-0556), scoping which indexers
    /// are consulted.
    ///
    /// `None` means **no item context** — a free-text operator search, where
    /// scoping is meaningless and every indexer is consulted. `Some(vec![])`
    /// means an item that genuinely has no tags, where *tagged* indexers do not
    /// apply but untagged ones still do.
    ///
    /// The distinction is load-bearing: collapsing them would make a manual
    /// `/search` silently skip every tagged indexer, which is exactly the
    /// tracker an operator reaches for a hand search.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// What started a search (SKADI-T-0539).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchTrigger {
    /// A sweep (wanted/upgrade/RSS). Honours `enable_automatic_search`.
    #[default]
    Automatic,
    /// The operator asked for this search by hand — the interactive-releases
    /// list, or a manual grab. **Ignores** `enable_automatic_search`: turning
    /// automatic search off is precisely how an operator keeps an indexer for
    /// exactly this, so filtering it here would defeat the setting.
    Interactive,
}

/// Season/episode scope for a TV search (SKADI-T-0269).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]

pub struct TvScope {
    pub season: u16,
    /// `Some` for a single-episode search; `None` for a whole-season (pack) search.
    pub episode: Option<u16>,
    /// The wanted episode's absolute number (anime), when the domain knows it —
    /// lets `decide`'s episode gate (SKADI-T-0386) verify an absolute-numbered
    /// release (`[Group] Show - 134`) that carries no `SxxEyy`.
    #[serde(default)]
    pub absolute: Option<u32>,
    /// The wanted episode's air date (daily shows), when known — verifies a
    /// date-keyed release (`Show.2024.03.01`) the same way.
    #[serde(default)]
    pub air_date: Option<chrono::NaiveDate>,
}

impl SearchSpec {
    /// The item's human-readable name for the live queue (SKADI-T-0690): the
    /// primary title, plus `SxxEyy` for an episode or `Season N` for a season
    /// pack. `None` when the spec carries no title at all.
    #[must_use]
    pub fn display_title(&self) -> Option<String> {
        let base = self
            .titles
            .first()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())?;
        Some(match self.tv {
            Some(TvScope {
                season,
                episode: Some(ep),
                ..
            }) => format!("{base} S{season:02}E{ep:02}"),
            Some(TvScope { season, .. }) => format!("{base} Season {season}"),
            None => base.to_string(),
        })
    }

    /// Borrow as a `&dyn SearchQuery` for the indexer layer.
    #[must_use]
    pub fn query(&self) -> SpecQuery<'_> {
        SpecQuery(self)
    }
}

/// Aliases an **automatic** TV or movie search sends per tracker
/// (SKADI-T-0595). The domains seed four ("Critical Role", "Critical Role
/// (2015)", "Critical Role 2015", "Critical.Role"), and every tracker tokenises
/// the last three the same way — but each is its own request, its own
/// FlareSolverr solve. Two keeps the year variant for same-named shows.
/// Audiobooks keep every alias: theirs are distinct (title, title + author,
/// series) and the series one is what finds a pack. Interactive searches are
/// untouched — an operator waiting on the list wants the whole net cast.
pub const AUTO_SEARCH_MAX_ALIASES: usize = 2;

/// The aliases a spec's search sends: all of them, except for an automatic TV
/// or movie search, which sends the first [`AUTO_SEARCH_MAX_ALIASES`].
#[must_use]
pub fn auto_search_titles(spec: &SearchSpec) -> &[String] {
    let capped = spec.trigger == SearchTrigger::Automatic
        && matches!(spec.kind, MediaKind::Series | MediaKind::Movie);
    if capped && spec.titles.len() > AUTO_SEARCH_MAX_ALIASES {
        &spec.titles[..AUTO_SEARCH_MAX_ALIASES]
    } else {
        &spec.titles
    }
}

/// Adapts a [`SearchSpec`] to the indexer layer's [`SearchQuery`] trait.
pub struct SpecQuery<'a>(&'a SearchSpec);

impl SearchQuery for SpecQuery<'_> {
    fn kind(&self) -> MediaKind {
        self.0.kind
    }
    fn titles(&self) -> &[String] {
        auto_search_titles(self.0)
    }
    fn year(&self) -> Option<u16> {
        self.0.year
    }
    fn external_ids(&self) -> &ExternalIds {
        &self.0.external_ids
    }
    fn categories(&self) -> &[Category] {
        &self.0.categories
    }
    fn extra_params(&self) -> Vec<(&'static str, String)> {
        match &self.0.tv {
            None => Vec::new(),
            Some(scope) => {
                let mut v = vec![("season", scope.season.to_string())];
                if let Some(ep) = scope.episode {
                    v.push(("ep", ep.to_string()));
                }
                v
            }
        }
    }
    fn mode(&self) -> SearchMode {
        SearchMode::Auto
    }
}

/// The data carried through one acquisition run, accumulated stage by stage.
///
/// Capabilities (store, HTTP client, indexers, downloaders, notifiers) are
/// **not** here — they're non-serializable and live in the process-global
/// `HunterServices` (see [`crate::services`]). This struct holds only serde
/// data, which is what Cloacina persists across task boundaries.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AcquireState {
    /// The acquirable this run is trying to satisfy (opaque domain ref).
    pub acquirable: AcquirableRef,
    /// What to search for (domain-seeded).
    pub request: SearchSpec,
    /// Quality profile to score and gate candidate releases against.
    pub profile: ProfileId,
    /// The quality this acquirable **already holds**, when this run is an
    /// *upgrade* attempt over an already-`Imported` item (SKADI-T-0182). `None`
    /// for a first acquisition. Threaded into [`crate::pipeline::Scoring`] so
    /// `decide_id(candidate, current)` only accepts a *strictly better* release
    /// (upgrade-until-cutoff). The domain reads it from
    /// [`skadi_core::AcquisitionStatus::Imported`]'s `quality` field — see
    /// `upgradable()` in the movies/audiobooks `WantedQuery` impls.
    #[serde(default)]
    pub current_quality: Option<QualityId>,
    /// The aggregate **custom-format score** of the file this acquirable already
    /// holds, when this run is an *upgrade* attempt (SKADI-T-0186). `None` for a
    /// first acquisition. Threaded into [`crate::pipeline::Scoring`] as a second
    /// upgrade axis: a same-quality candidate with a strictly-higher format score
    /// is an upgrade (proper/repack). The domain reads it from
    /// [`skadi_core::AcquisitionStatus::Imported`]'s `score` field.
    #[serde(default)]
    pub current_format_score: Option<i32>,
    /// The held file was probed and will not direct-play (SKADI-T-0584). Set by
    /// the domain's `upgradable()` from the stored `media_info`.
    #[serde(default)]
    pub current_unplayable: bool,
    /// Releases found by `search`.
    #[serde(default)]
    pub candidates: Vec<Release>,
    /// The release `decide` picked, if any. Holds the full [`Release`] (it has no
    /// id of its own and `snatch` needs its fetch URL).
    #[serde(default)]
    pub chosen: Option<Release>,
    /// What `decide` weighed and why it dropped the rest (SKADI-T-0380). Set by
    /// [`crate::pipeline::decide`] on both outcomes — a `None` chosen with a
    /// populated tally is exactly the "181 candidates, nothing grabbed" case the
    /// trace needs to explain. `None` before `decide` has run.
    #[serde(default)]
    pub tally: Option<crate::pipeline::DecisionTally>,
    /// The download handle `snatch` produced, if any.
    #[serde(default)]
    pub handle: Option<DownloadHandle>,
    /// The file paths `monitor` observed on `Completed`, ready for `import`.
    #[serde(default)]
    pub completed_paths: Option<Vec<std::path::PathBuf>>,
    /// The import result `import` produced, if any.
    #[serde(default)]
    pub outcome: Option<ImportOutcome>,
    /// Set by a step when the run has terminally failed (a hard download/import
    /// failure). `monitor` returns `Ok` after setting this so Cloacina does NOT
    /// burn its 48 retry attempts re-polling a dead download; the downstream
    /// `import`/`notify` steps see the flag and no-op. The file is left
    /// `Failed{retry_at}`, so the next sweep re-acquires it after the backoff —
    /// recovering a flaky source (SKADI-I-0017).
    #[serde(default)]
    pub terminal_failure: bool,
    /// Set by `monitor` when the transfer was removed on request (the operator
    /// removed it from Activity or Downloads) instead of failing on its own
    /// (SKADI-T-0691). The run then ends without auto-blocklisting the release.
    #[serde(default)]
    pub transfer_removed: bool,
    /// How many times this acquirable has *already* failed not-found (search
    /// returned nothing / no suitable release) before this run — read from the
    /// prior `Failed.attempts` when `search` starts. Drives the escalating
    /// re-check backoff this run writes on a not-found failure (SKADI-T-0177).
    #[serde(default)]
    pub prior_not_found_attempts: u32,
    /// `true` for a run a person started by picking a release
    /// ([`crate::worker::start_grab`]): an explicit override that may replace
    /// an `Imported` file, so the "already imported by another run" guard in
    /// `snatch`/`monitor` (SKADI-T-0388) does not apply to it.
    #[serde(default)]
    pub manual: bool,
    /// The in-flight tracker run id this workflow was launched under
    /// ([`crate::worker::start_acquire`] / `start_grab`). Persisted in the
    /// Cloacina context so a workflow replayed after a restart can prove which
    /// tracker entry is its own — two replayed workflows for one acquirable
    /// must not both snatch (SKADI-T-0388). `None` for states persisted before
    /// this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Every candidate `decide` accepted, as indices into `candidates`, best
    /// first (SKADI-T-0589). `chosen` is `candidates[ranked[0]]`; `snatch` walks
    /// the rest when a grab fails before a transfer exists (a detail page that
    /// yields no torrent, a downloader that refuses the link), so one dead link
    /// no longer fails the whole run. Empty for states persisted before this
    /// field existed — `snatch` then has only `chosen` to try, as before.
    #[serde(default)]
    pub ranked: Vec<usize>,
    /// Candidates `snatch` tried and skipped this run, for the task wrapper to
    /// blocklist and trace (SKADI-T-0589).
    #[serde(default)]
    pub snatch_failures: Vec<SnatchFailure>,
}

/// A release `snatch` gave up on before a transfer existed (SKADI-T-0589).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SnatchFailure {
    /// `skadi_indexers::release_key` of the release, the blocklist's key.
    pub release_key: String,
    pub title: String,
    pub indexer: String,
    pub error: String,
    /// Whether the release itself is at fault and should be blocklisted: its
    /// indexer could not resolve the link (a detail page with no torrent). A
    /// downloader refusing the add is a client problem (SKADI-T-0188) and
    /// leaves the release grabbable.
    #[serde(default)]
    pub blocklist: bool,
}

impl AcquireState {
    /// A fresh state for `acquirable` searching per `request` under `profile`,
    /// before any stage has run.
    #[must_use]
    pub fn new(acquirable: AcquirableRef, request: SearchSpec, profile: ProfileId) -> Self {
        Self {
            acquirable,
            request,
            profile,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
            candidates: Vec::new(),
            chosen: None,
            tally: None,
            handle: None,
            completed_paths: None,
            outcome: None,
            terminal_failure: false,
            transfer_removed: false,
            prior_not_found_attempts: 0,
            manual: false,
            run_id: None,
            ranked: Vec::new(),
            snatch_failures: Vec::new(),
        }
    }

    /// Mark this run as an upgrade over an already-held file: its `quality` and
    /// aggregate custom-format `format_score` (SKADI-T-0182 / SKADI-T-0186).
    /// Builder form so `new()`'s many call sites stay unchanged.
    #[must_use]
    pub fn with_current(mut self, quality: Option<QualityId>, format_score: Option<i32>) -> Self {
        self.current_quality = quality;
        self.current_format_score = format_score;
        self
    }

    /// Mark the held file as one that will not direct-play (SKADI-T-0584).
    #[must_use]
    pub fn with_current_unplayable(mut self, unplayable: bool) -> Self {
        self.current_unplayable = unplayable;
        self
    }

    /// Mark this run as an upgrade over an already-held `current` quality
    /// (SKADI-T-0182). Builder form so `new()`'s many call sites stay unchanged.
    #[must_use]
    pub fn with_current_quality(mut self, current: Option<QualityId>) -> Self {
        self.current_quality = current;
        self
    }

    /// Serialize into a fresh Cloacina [`Context`] under [`STATE_KEY`]. Used to
    /// seed a run before `runner.execute(...)`.
    pub fn into_context(&self) -> Result<Context<Value>> {
        let mut ctx = Context::new();
        ctx.insert(STATE_KEY, serde_json::to_value(self).map_err(ser_err)?)
            .map_err(ctx_err)?;
        Ok(ctx)
    }
}

/// Read the [`AcquireState`] out of a run's context.
pub fn load_state(ctx: &Context<Value>) -> Result<AcquireState> {
    let value = ctx
        .get(STATE_KEY)
        .ok_or_else(|| AppError::Internal(format!("workflow context missing {STATE_KEY:?}")))?;
    serde_json::from_value(value.clone()).map_err(ser_err)
}

/// Write the [`AcquireState`] back into a run's context (insert or update).
pub fn store_state(ctx: &mut Context<Value>, state: &AcquireState) -> Result<()> {
    let value = serde_json::to_value(state).map_err(ser_err)?;
    if ctx.get(STATE_KEY).is_some() {
        ctx.update(STATE_KEY, value).map_err(ctx_err)
    } else {
        ctx.insert(STATE_KEY, value).map_err(ctx_err)
    }
}

fn ser_err(e: serde_json::Error) -> AppError {
    AppError::Internal(format!("acquire-state serde: {e}"))
}

fn ctx_err(e: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("workflow context: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: MediaKind, trigger: SearchTrigger, n: usize) -> SearchSpec {
        SearchSpec {
            trigger,
            kind,
            titles: (0..n).map(|i| format!("Alias {i}")).collect(),
            year: None,
            external_ids: ExternalIds::default(),
            categories: Vec::new(),
            tv: None,
            series: None,
            tags: None,
        }
    }

    /// SKADI-T-0690: the live queue's item name.
    #[test]
    fn display_title_names_the_item() {
        let mut s = spec(MediaKind::Movie, SearchTrigger::Automatic, 2);
        assert_eq!(s.display_title().as_deref(), Some("Alias 0"));
        s.tv = Some(TvScope {
            season: 1,
            episode: Some(2),
            absolute: None,
            air_date: None,
        });
        assert_eq!(s.display_title().as_deref(), Some("Alias 0 S01E02"));
        s.tv = Some(TvScope {
            season: 3,
            episode: None,
            absolute: None,
            air_date: None,
        });
        assert_eq!(s.display_title().as_deref(), Some("Alias 0 Season 3"));
        assert_eq!(
            spec(MediaKind::Movie, SearchTrigger::Automatic, 0).display_title(),
            None
        );
    }

    /// SKADI-T-0595: automatic TV/movie searches send two aliases; audiobooks
    /// and interactive searches send them all.
    #[test]
    fn automatic_tv_and_movie_searches_cap_their_aliases() {
        assert_eq!(
            auto_search_titles(&spec(MediaKind::Series, SearchTrigger::Automatic, 4)).len(),
            2
        );
        assert_eq!(
            auto_search_titles(&spec(MediaKind::Movie, SearchTrigger::Automatic, 3)).len(),
            2
        );
        assert_eq!(
            auto_search_titles(&spec(MediaKind::Series, SearchTrigger::Automatic, 1)).len(),
            1
        );
        assert_eq!(
            auto_search_titles(&spec(MediaKind::Audiobook, SearchTrigger::Automatic, 3)).len(),
            3
        );
        assert_eq!(
            auto_search_titles(&spec(MediaKind::Series, SearchTrigger::Interactive, 4)).len(),
            4
        );
    }

    fn sample() -> AcquireState {
        let request = SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["The Matrix".into()],
            year: Some(1999),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        };
        AcquireState::new(
            AcquirableRef("movie-edition-1".into()),
            request,
            ProfileId::new(),
        )
    }

    #[test]
    fn round_trips_through_json() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        let back: AcquireState = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn into_context_then_load_is_identity() {
        let s = sample();
        let ctx = s.into_context().unwrap();
        assert_eq!(load_state(&ctx).unwrap(), s);
    }

    #[test]
    fn store_state_inserts_then_updates() {
        let mut ctx = Context::new();
        let mut s = sample();
        // First write inserts.
        store_state(&mut ctx, &s).unwrap();
        assert_eq!(load_state(&ctx).unwrap(), s);
        // Second write updates in place (no "key exists" error).
        s.request.year = Some(2000);
        store_state(&mut ctx, &s).unwrap();
        assert_eq!(load_state(&ctx).unwrap().request.year, Some(2000));
    }

    #[test]
    fn load_from_empty_context_errors() {
        let ctx: Context<Value> = Context::new();
        assert!(matches!(load_state(&ctx), Err(AppError::Internal(_))));
    }
}
