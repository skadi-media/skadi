//! `SeriesWantedQuery` — the hunter's sweep input for the television domain
//! (SKADI-T-0269).
//!
//! Emits one [`AcquireSeed`] per [`Episode`] that needs a release, mirroring
//! `MovieWantedQuery` but episode-scoped:
//! - **`wanted()`** — monitored series → monitored, **aired**, `Missing` (or
//!   retry-eligible `Failed`) episodes. Un-aired episodes are skipped so the
//!   sweep doesn't hammer indexers for content that doesn't exist yet.
//! - **`upgradable()`** — monitored series → monitored `Imported` episodes whose
//!   held quality ranks below the profile cutoff (when upgrades are allowed).
//!
//! Each seed carries a [`TvScope`] (`season` + `episode`) so the indexer issues a
//! Torznab **tv-search** (`tvdbid` + `season`/`ep`), keyed off the series'
//! `tvdb_id`. A single-episode search still surfaces season packs at the release
//! level; turning a grabbed pack into per-episode imports is the matcher's job
//! (SKADI-T-0270), as is emitting first-class whole-season seeds.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use skadi_core::{AcquisitionStatus, ExternalIds, MediaKind, QualityId, Result};
use skadi_hunter::{AcquireSeed, SearchSpec, TvScope, WantedQuery};
use skadi_indexers::Category;
use skadi_quality::QualityProfile;

use crate::episode::Episode;
use crate::repo::{SeriesFilter, TvRepo};
use crate::series::Series;

/// Torznab TV category — the default search scope for `SeriesWantedQuery`.
const TV_CATEGORY: Category = Category(5000);

/// Minimum number of missing episodes in a season before emitting a season-pack
/// seed (SKADI-T-0377). Below this threshold, only individual episode seeds are
/// emitted. Season packs are more efficient when multiple episodes are wanted.
const SEASON_PACK_THRESHOLD: usize = 3;

/// Convert a title to scene-style naming (dots for spaces/punctuation).
/// Scene releases name files like `Game.of.Thrones.S01E01` —
/// searching with spaces may miss them on some indexers (SKADI-T-0375).
/// Build the acquire seed for one (series, episode) — the single definition of
/// what a television search looks like (SKADI-T-0558).
///
/// Public so the manual acquire endpoints use *exactly* this, rather than
/// assembling their own `SearchSpec`. A manual acquire that searched differently
/// from the sweep would be a confusing bug: the operator triggers it precisely
/// because the sweep is not finding the release, and a different query would
/// change the answer for reasons nothing surfaces.
///
/// First-acquisition shape: `current_quality` / `current_format_score` are left
/// `None`. Upgrades flow through the sweep's `upgradable()`.
#[must_use]
pub fn episode_seed(series: &Series, episode: &Episode) -> AcquireSeed {
    AcquireSeed {
        acquirable: episode.acquirable_ref(),
        request: SearchSpec {
            // Sweep-driven; the interactive paths override this just before
            // searching (SKADI-T-0539).
            trigger: skadi_hunter::SearchTrigger::Automatic,
            kind: MediaKind::Series,
            titles: build_series_titles(series),
            year: series.year,
            external_ids: ExternalIds {
                tvdb: series.external_ids.tvdb.clone(),
                tmdb: series.external_ids.tmdb.clone(),
                imdb: series.external_ids.imdb.clone(),
                ..Default::default()
            },
            categories: vec![TV_CATEGORY],
            tv: Some(TvScope {
                season: episode.season,
                episode: Some(episode.number),
                absolute: episode.absolute_number,
                air_date: episode.air_date,
            }),
            series: None,
            // Filled in by `steps::search` from the item's tags
            // (SKADI-T-0556) — a database read, so it cannot happen in these
            // pure seed builders.
            tags: None,
        },
        profile: series.profile,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    }
}

fn scene_title(title: &str) -> String {
    title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '.' })
        .collect::<String>()
        .split('.')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// Build title variants for a series (SKADI-T-0375, SKADI-T-0376):
/// 1. Primary title
/// 2. Year-parenthesized variant (e.g. "The Office (2005)") if year is known
/// 3. Year-appended variant (e.g. "The Office 2005") if year is known
/// 4. Scene-style variant (e.g. "The.Office") if different from primary
fn build_series_titles(series: &Series) -> Vec<String> {
    let mut out = vec![series.title.clone()];
    // Year disambiguation aliases (SKADI-T-0376): helps distinguish shows with
    // common names (e.g. "The Office" US vs UK).
    if let Some(year) = series.year {
        out.push(format!("{} ({})", series.title, year));
        out.push(format!("{} {}", series.title, year));
    }
    // Scene-style variant (SKADI-T-0375)
    let scene = scene_title(&series.title);
    if scene != series.title && !out.contains(&scene) {
        out.push(scene);
    }
    out
}

/// Whether the sweep should (re-)acquire an episode in this status: `Missing`, or
/// a transfer-`Failed` episode whose `retry_at` backoff has elapsed. A
/// `Failed{retry_at: None}` (no-suitable-release) is NOT auto-retried.
fn wants_acquire(status: &AcquisitionStatus, now: chrono::DateTime<Utc>) -> bool {
    match status {
        AcquisitionStatus::Missing => true,
        AcquisitionStatus::Failed {
            retry_at: Some(t), ..
        } => *t <= now,
        _ => false,
    }
}

/// Scoring inputs the sweep needs to decide whether an imported episode is below
/// the profile cutoff (mirrors the movies `WantedScoring`).
#[derive(Clone)]
pub struct WantedScoring {
    pub profile: QualityProfile,
    /// Opt-in "upgrade until custom-format score" (ADR SKADI-A-0003 §2); `0`
    /// disables the at-cutoff format re-search.
    pub upgrade_until_format_score: i32,
    /// Sonarr's "search for undated episodes" (SKADI-T-0446). `false` (the
    /// default) skips episodes the provider has not dated.
    pub search_undated_episodes: bool,
    /// Re-acquire episodes the media scan says will not direct-play
    /// (SKADI-T-0584). **Off by default** — see the movies doc; profiling is free
    /// and reversible, re-grabbing is neither, and without the direct-play
    /// formats scored a re-grab can fetch the same undecodable audio again.
    ///
    /// Config key `regrab_unplayable`.
    pub regrab_unplayable: bool,
}

/// `WantedQuery` impl for television.
/// `quality_id`'s rank within `profile`'s ladder, or `None` when it is not in it.
fn rank_in(profile: &QualityProfile, quality_id: QualityId) -> Option<usize> {
    profile.allowed.iter().position(|q| *q == quality_id)
}

/// The rank of `profile`'s cutoff. `usize::MAX` when the cutoff is somehow not in
/// `allowed` — nothing then counts as below it, which is the safe direction: an
/// unusable profile must not sweep the whole library into the upgrade queue.
fn cutoff_rank_in(profile: &QualityProfile) -> usize {
    profile
        .allowed
        .iter()
        .position(|q| *q == profile.cutoff)
        .unwrap_or(usize::MAX)
}

pub struct SeriesWantedQuery {
    repo: Arc<dyn TvRepo>,
    scoring: WantedScoring,
    /// Config plane, for resolving a series' **own** quality profile
    /// (SKADI-T-0537). `None` ⇒ the domain-wide profile, which is the
    /// pre-T-0537 behaviour and what a caller with no store still gets.
    store: Option<skadi_store::Store>,
}

impl SeriesWantedQuery {
    #[must_use]
    pub fn new(repo: Arc<dyn TvRepo>, scoring: WantedScoring) -> Self {
        Self {
            repo,
            scoring,
            store: None,
        }
    }

    /// Resolve each series' own profile rather than the domain-wide fallback
    /// (SKADI-T-0537).
    #[must_use]
    pub fn with_store(mut self, store: skadi_store::Store) -> Self {
        self.store = Some(store);
        self
    }

    /// The profile to judge a series against: its own when resolvable, else the
    /// domain-wide fallback (SKADI-T-0537). `cache` holds one resolution per
    /// distinct profile id for the whole sweep — `upgradable()` walks every
    /// episode of every series, so a per-item lookup would be one settings read
    /// per episode rather than per profile.
    async fn profile_for(
        &self,
        id: skadi_core::ProfileId,
        defs: &[skadi_quality::QualityDefinition],
        cache: &mut std::collections::HashMap<skadi_core::ProfileId, QualityProfile>,
    ) -> QualityProfile {
        if let Some(p) = cache.get(&id) {
            return p.clone();
        }
        let resolved = match &self.store {
            Some(store) => skadi_hunter::services::resolve_profile_by_id(store, id, defs).await,
            None => None,
        };
        let profile = resolved.unwrap_or_else(|| self.scoring.profile.clone());
        cache.insert(id, profile.clone());
        profile
    }

    /// Build a seed for `episode`. `current` is the held `(quality, format_score)`
    /// for an *upgrade* seed (`Some` from `upgradable()`), else `None`.
    fn seed_for(
        &self,
        series: &Series,
        episode: &Episode,
        current: Option<(QualityId, i32)>,
    ) -> AcquireSeed {
        let mut seed = episode_seed(series, episode);
        seed.current_quality = current.map(|(q, _)| q);
        seed.current_format_score = current.map(|(_, s)| s);
        // From the episode's own probe, for the same reason as movies: a caller
        // that forgets it silently restores the strictly-better bar and the
        // unwatchable file stays (SKADI-T-0584).
        seed.current_unplayable = episode
            .media_info
            .as_ref()
            .is_some_and(skadi_core::is_broken);
        seed
    }

    /// Build a seed for a full season (SKADI-T-0377). Emitted when multiple
    /// episodes are missing from the same season, so a single season-pack
    /// download can satisfy them all.
    fn season_pack_seed(
        &self,
        series: &Series,
        season: u16,
        current: Option<(QualityId, i32)>,
    ) -> AcquireSeed {
        AcquireSeed {
            // Season-level acquirable ref: "season-{series_id}-{season}" (SKADI-T-0590).
            acquirable: crate::acquirable::TvAcquirable::season_ref(series.id, season),
            request: SearchSpec {
                // Sweep-driven; the interactive paths override this just before
                // searching (SKADI-T-0539).
                trigger: skadi_hunter::SearchTrigger::Automatic,
                kind: MediaKind::Series,
                titles: build_series_titles(series),
                year: series.year,
                external_ids: ExternalIds {
                    tvdb: series.external_ids.tvdb.clone(),
                    tmdb: series.external_ids.tmdb.clone(),
                    imdb: series.external_ids.imdb.clone(),
                    ..Default::default()
                },
                categories: vec![TV_CATEGORY],
                tv: Some(TvScope {
                    season,
                    episode: None, // full season, not a specific episode
                    absolute: None,
                    air_date: None,
                }),
                series: None,
                // Filled in by `steps::search` from the item's tags
                // (SKADI-T-0556) — a database read, so it cannot happen in these
                // pure seed builders.
                tags: None,
            },
            profile: series.profile,
            current_quality: current.map(|(q, _)| q),
            current_format_score: current.map(|(_, s)| s),
            // Always false for a SEASON pack (SKADI-T-0584): the acquirable
            // covers many episodes, so "the held file does not play" has no
            // single answer here. The episode-level seed carries it.
            current_unplayable: false,
        }
    }
}

#[async_trait]
impl WantedQuery for SeriesWantedQuery {
    async fn wanted(&self) -> Result<Vec<AcquireSeed>> {
        let series_list = self
            .repo
            .list_series(SeriesFilter {
                monitored: Some(true),
                limit: None,
                offset: None,
            })
            .await?;
        let now = Utc::now();
        let today = now.date_naive();
        let mut out = Vec::new();

        for series in &series_list {
            // Collect missing episodes
            let missing: Vec<&Episode> = series
                .episodes
                .iter()
                .filter(|ep| {
                    ep.monitored
                        && ep.has_aired_or_undated(today, self.scoring.search_undated_episodes)
                        && wants_acquire(&ep.status, now)
                })
                .collect();

            // Group by season (SKADI-T-0377)
            // Pack seeds count every *unsatisfied* episode in the season (missing,
            // or failed and merely waiting out its backoff), not only the ones due
            // this tick (SKADI-T-0607). Retry backoffs stagger, so "three due at
            // the same moment" almost never happened on a long-failed season and
            // the pack — the one search likely to fill it — was never run. A due
            // episode in the season is still required, so the pack search rides
            // along with the episode's own re-check rather than every sweep.
            let mut unsatisfied: HashMap<u16, usize> = HashMap::new();
            for ep in &series.episodes {
                if ep.monitored
                    && ep.has_aired_or_undated(today, self.scoring.search_undated_episodes)
                    && matches!(
                        ep.status,
                        AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
                    )
                {
                    *unsatisfied.entry(ep.season).or_default() += 1;
                }
            }
            let mut due_seasons: Vec<u16> = missing.iter().map(|ep| ep.season).collect();
            due_seasons.sort_unstable();
            due_seasons.dedup();

            for season in due_seasons {
                if season != 0
                    && unsatisfied.get(&season).copied().unwrap_or(0) >= SEASON_PACK_THRESHOLD
                {
                    out.push(self.season_pack_seed(series, season, None));
                }
            }

            for ep in missing {
                out.push(self.seed_for(series, ep, None));
            }
        }
        // Sweep size (SKADI-T-0456): previously invisible, so "why is it
        // searching hundreds of things?" had no answer short of reading the
        // database. DEBUG — it fires every sweep, and the number only matters
        // when something looks wrong.
        tracing::debug!(
            domain = "television",
            seeds = out.len(),
            "sweep produced seeds"
        );
        Ok(out)
    }

    async fn upgradable(&self) -> Result<Vec<AcquireSeed>> {
        // NOT short-circuited on the domain-wide profile (SKADI-T-0537): with
        // per-item profiles, `upgrade_allowed` on the *default* says nothing about
        // a series assigned a profile that does allow upgrades.
        let defs = skadi_quality::default_definitions();
        let mut profiles: std::collections::HashMap<skadi_core::ProfileId, QualityProfile> =
            std::collections::HashMap::new();
        let series_list = self
            .repo
            .list_series(SeriesFilter {
                monitored: Some(true),
                limit: None,
                offset: None,
            })
            .await?;
        let mut out = Vec::new();
        for series in &series_list {
            // The series' OWN profile (SKADI-T-0537), resolved once per distinct
            // id across the whole sweep.
            let profile = self.profile_for(series.profile, &defs, &mut profiles).await;
            if !profile.upgrade_allowed {
                continue;
            }
            let cutoff_rank = cutoff_rank_in(&profile);
            for episode in &series.episodes {
                if !episode.monitored {
                    continue;
                }
                let (qid, fscore) = match &episode.status {
                    AcquisitionStatus::Imported { quality, score, .. } => (*quality, *score),
                    _ => continue,
                };
                // An UNASSESSED episode is not an upgrade candidate (SKADI-T-0399):
                // 18,449 adopted episodes on prod carried the SDTV floor, so every
                // one of them read as "below cutoff". A *known* tier outside the
                // profile still ranks 0 (Radarr/Sonarr parity: out-of-profile counts
                // as cutoff-unmet).
                if skadi_quality::is_unknown_quality(qid) {
                    continue;
                }
                let rank = rank_in(&profile, qid).unwrap_or(0);
                let wants_format_upgrade = fscore < self.scoring.upgrade_until_format_score;
                // Third axis (SKADI-T-0584) — see the movies sweep. Requires a
                // probe: unscanned is not the same as broken.
                let unplayable = episode
                    .media_info
                    .as_ref()
                    .is_some_and(skadi_core::is_broken);
                if rank < cutoff_rank
                    || wants_format_upgrade
                    || (unplayable && self.scoring.regrab_unplayable)
                {
                    out.push(self.seed_for(series, episode, Some((qid, fscore))));
                }
            }
        }
        // Sweep size (SKADI-T-0456): previously invisible, so "why is it
        // searching hundreds of things?" had no answer short of reading the
        // database. DEBUG — it fires every sweep, and the number only matters
        // when something looks wrong.
        tracing::debug!(
            domain = "television",
            seeds = out.len(),
            "sweep produced seeds"
        );
        Ok(out)
    }

    async fn reconcile_stale(&self) -> Result<usize> {
        // Recover episodes wedged in a non-terminal acquire state past the grace:
        // reset to `Missing` so the next `wanted()` re-acquires them.
        let cutoff = Utc::now()
            - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE)
                .unwrap_or_else(|_| chrono::Duration::seconds(900));
        let series_list = self
            .repo
            .list_series(SeriesFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        let mut recovered = 0;
        for series in &series_list {
            for episode in &series.episodes {
                // Heal a demoted import (SKADI-T-0385/0388): `file_path` is written
                // ONLY by an `Imported` status write, so a `Missing`/`Failed` row that
                // still carries a path was imported once and later overwritten by a
                // duplicate acquire run. If the file is still on disk the library holds
                // it — restore `Imported` rather than re-grabbing it every sweep.
                if matches!(
                    episode.status,
                    AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
                ) {
                    if let Some(imported) = restore_demoted_import(episode).await {
                        tracing::warn!(
                            episode = %episode.id,
                            path = %imported_path(&imported),
                            "restoring demoted episode import (file still on disk)"
                        );
                        self.repo.set_episode_status(episode.id, imported).await?;
                        recovered += 1;
                    }
                    continue;
                }
                let in_flight = matches!(
                    episode.status,
                    AcquisitionStatus::Searching { .. }
                        | AcquisitionStatus::Snatched { .. }
                        | AcquisitionStatus::Downloading { .. }
                );
                if in_flight && episode.updated_at < cutoff {
                    // A wedged row whose file is on disk is the same demoted import:
                    // restore it directly rather than bouncing through `Missing`, which
                    // would let `wanted()` re-grab it in this very sweep.
                    let next = match restore_demoted_import(episode).await {
                        Some(imported) => {
                            tracing::warn!(
                                episode = %episode.id,
                                stuck_since = %episode.updated_at,
                                path = %imported_path(&imported),
                                "recovering wedged episode (file on disk → Imported)"
                            );
                            imported
                        }
                        None => {
                            tracing::warn!(
                                episode = %episode.id,
                                stuck_since = %episode.updated_at,
                                "recovering wedged episode (reset to Missing)"
                            );
                            AcquisitionStatus::Missing
                        }
                    };
                    self.repo.set_episode_status(episode.id, next).await?;
                    recovered += 1;
                }
            }
        }
        Ok(recovered)
    }
}

/// The `Imported` status to restore for an episode row that still has its library
/// file on disk (SKADI-T-0385/0388), or `None` when the row never recorded a path
/// or the file is gone (a real loss — leave it wanted). Quality/score come from
/// the row's own columns (also written only at import); a row with no quality
/// falls back to the lowest default tier so a later upgrade run can replace it.
async fn restore_demoted_import(episode: &Episode) -> Option<AcquisitionStatus> {
    let path = episode.file.as_ref()?.path.clone();
    let probe = path.clone();
    let exists = tokio::task::spawn_blocking(move || probe.is_file())
        .await
        .unwrap_or(false);
    if !exists {
        return None;
    }
    Some(AcquisitionStatus::Imported {
        file: skadi_core::FileRef { path },
        // No recorded quality ⇒ UNKNOWN, not the ladder's lowest tier: the healed
        // row must not become an upgrade candidate (SKADI-T-0399).
        quality: episode.quality.unwrap_or(skadi_quality::UNKNOWN_QUALITY_ID),
        score: episode.format_score,
        at: Utc::now(),
    })
}

fn imported_path(status: &AcquisitionStatus) -> String {
    match status {
        AcquisitionStatus::Imported { file, .. } => file.path.display().to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use skadi_core::{FileRef, ProfileId, RootFolder, TvdbId};
    use skadi_indexers::SearchQuery;
    use skadi_quality::default_definitions;
    use skadi_testsupport::TestDb;
    use std::path::PathBuf;

    fn profile_two_ranks() -> (QualityProfile, QualityId, QualityId) {
        let defs = default_definitions();
        let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
        let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
        let profile = QualityProfile {
            id: ProfileId::new(),
            name: "test".into(),
            allowed: vec![lo, hi],
            cutoff: hi,
            upgrade_allowed: true,
            formats: vec![],
            min_format_score: 0,
        };
        (profile, lo, hi)
    }

    async fn seed_series(store: &impl TvRepo, monitored: bool) -> Series {
        let mut s = Series::new(
            ExternalIds {
                tvdb: Some(TvdbId(121361)),
                ..Default::default()
            },
            "Game of Thrones",
            ProfileId::new(),
            RootFolder::new("/tv"),
        );
        s.monitored = monitored;
        s.year = Some(2011);
        store.upsert_series(&s).await.unwrap();
        s
    }

    fn aired_ep(series: &Series, season: u16, number: u16) -> Episode {
        let mut e = Episode::missing(series.id, season, number);
        e.air_date = Some(NaiveDate::from_ymd_opt(2011, 4, 17).unwrap());
        e
    }

    #[tokio::test]
    async fn wanted_emits_per_episode_seeds_for_aired_monitored_missing() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, _, _) = profile_two_ranks();
        let series = seed_series(store, true).await;

        // aired+monitored+missing → wanted
        store
            .upsert_episode(&aired_ep(&series, 1, 1))
            .await
            .unwrap();
        // un-aired → skipped
        let mut future = Episode::missing(series.id, 1, 2);
        future.air_date = Some(NaiveDate::from_ymd_opt(2999, 1, 1).unwrap());
        store.upsert_episode(&future).await.unwrap();
        // unmonitored → skipped
        let mut unmon = aired_ep(&series, 1, 3);
        unmon.monitored = false;
        store.upsert_episode(&unmon).await.unwrap();

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let seeds = q.wanted().await.unwrap();
        assert_eq!(seeds.len(), 1, "only the aired+monitored+missing episode");
        let spec = &seeds[0].request;
        assert_eq!(spec.kind, MediaKind::Series);
        assert_eq!(spec.categories, vec![TV_CATEGORY]);
        assert_eq!(
            spec.tv,
            Some(TvScope {
                season: 1,
                episode: Some(1),
                absolute: None,
                air_date: Some(NaiveDate::from_ymd_opt(2011, 4, 17).unwrap()),
            })
        );
        assert_eq!(spec.external_ids.tvdb.as_ref().map(|t| t.0), Some(121361));
        // the tv scope becomes Torznab season/ep extra-params
        assert_eq!(
            spec.query().extra_params(),
            vec![("season", "1".to_string()), ("ep", "1".to_string())]
        );
    }

    #[tokio::test]
    async fn unmonitored_series_yields_nothing() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, _, _) = profile_two_ranks();
        let series = seed_series(store, false).await;
        store
            .upsert_episode(&aired_ep(&series, 1, 1))
            .await
            .unwrap();

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(q.wanted().await.unwrap().is_empty());
    }

    /// SKADI-T-0537: an item assigned a non-default profile must be judged
    /// against **that** profile. `decide` has scored candidates against the
    /// item's own profile since SKADI-T-0531, so a mismatch here meant the two
    /// halves disagreed — an item could be re-searched every sweep and never be
    /// grabbable, or be genuinely upgradable and never re-searched.
    #[tokio::test]
    async fn upgradable_judges_against_the_series_own_profile() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (domain_profile, lo, hi) = profile_two_ranks();

        // The series' OWN profile cuts off at `lo`, so a file held at `lo` is
        // already at cutoff and is NOT upgradable — even though the domain-wide
        // profile (cutoff `hi`) would say it is.
        let own = QualityProfile {
            id: ProfileId::new(),
            name: "own".into(),
            allowed: vec![lo, hi],
            cutoff: lo,
            upgrade_allowed: true,
            formats: vec![],
            min_format_score: 0,
        };
        skadi_store::SettingsRepo::put_setting(
            &db.store,
            "profiles",
            &own.id.to_string(),
            &serde_json::json!({
                "name": own.name,
                "allowed": [lo.to_string(), hi.to_string()],
                "cutoff": lo.to_string(),
                "upgrade_allowed": true,
                "media_type": "video",
            }),
        )
        .await
        .unwrap();

        let mut series = seed_series(store, true).await;
        series.profile = own.id;
        store.upsert_series(&series).await.unwrap();

        let mut ep = aired_ep(&series, 1, 1);
        ep.status = AcquisitionStatus::Imported {
            file: FileRef {
                path: PathBuf::from("got.s01e01.mkv"),
            },
            quality: lo,
            score: 0,
            at: Utc::now(),
        };
        store.upsert_episode(&ep).await.unwrap();

        let scoring = WantedScoring {
            profile: domain_profile,
            search_undated_episodes: false,
            upgrade_until_format_score: 0,
            regrab_unplayable: false,
        };
        // Without a store every item falls back to the domain profile, which
        // says "upgradable" — this is the behaviour the ticket describes.
        let without = SeriesWantedQuery::new(Arc::new(db.store.clone()), scoring.clone());
        assert_eq!(
            without.upgradable().await.unwrap().len(),
            1,
            "the domain-wide profile alone reports it upgradable"
        );

        // With the store, the series' own profile is consulted and it is not.
        let with = SeriesWantedQuery::new(Arc::new(db.store.clone()), scoring)
            .with_store(db.store.clone());
        assert!(
            with.upgradable().await.unwrap().is_empty(),
            "at its own profile's cutoff, so not upgradable"
        );
    }

    /// SKADI-T-0537: a series whose profile row is missing falls back to the
    /// domain-wide default rather than being dropped from the sweep — the
    /// fallback is what SKADI-T-0532 made an operator-chosen row.
    #[tokio::test]
    async fn a_missing_profile_row_falls_back_to_the_domain_default() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, lo, _hi) = profile_two_ranks();
        // `seed_series` assigns a fresh ProfileId with no settings row behind it.
        let series = seed_series(store, true).await;
        let mut ep = aired_ep(&series, 1, 1);
        ep.status = AcquisitionStatus::Imported {
            file: FileRef {
                path: PathBuf::from("got.s01e01.mkv"),
            },
            quality: lo,
            score: 0,
            at: Utc::now(),
        };
        store.upsert_episode(&ep).await.unwrap();

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        )
        .with_store(db.store.clone());
        assert_eq!(q.upgradable().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn upgradable_emits_for_below_cutoff_imports() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, lo, _hi) = profile_two_ranks();
        let series = seed_series(store, true).await;

        // Imported at the LOW quality (below cutoff) → upgradable.
        let mut ep = aired_ep(&series, 1, 1);
        ep.status = AcquisitionStatus::Imported {
            file: FileRef {
                path: PathBuf::from("got.s01e01.mkv"),
            },
            quality: lo,
            score: 0,
            at: Utc::now(),
        };
        store.upsert_episode(&ep).await.unwrap();

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        // Not "wanted" (it's Imported, not Missing) …
        assert!(q.wanted().await.unwrap().is_empty());
        // … but it IS upgradable, and the seed carries the held quality.
        let up = q.upgradable().await.unwrap();
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].current_quality, Some(lo));
    }

    /// SKADI-T-0385/0388 heal, TV edition: a row that still carries a `file_path`
    /// (only an `Imported` write sets it) but was demoted to `Missing` by a
    /// duplicate acquire run — or is wedged in-flight past the grace — is
    /// restored to `Imported` when its file is still on disk; a demoted row whose
    /// file is gone stays `Missing` and remains wanted.
    #[tokio::test]
    async fn reconcile_stale_restores_demoted_imports_whose_file_exists() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, lo, _hi) = profile_two_ranks();
        let series = seed_series(store, true).await;

        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("got.s01e01.mkv");
        std::fs::write(&present, b"x").unwrap();
        let gone = tmp.path().join("got.s01e02.mkv");

        let imported_at = |path: &PathBuf| AcquisitionStatus::Imported {
            file: FileRef { path: path.clone() },
            quality: lo,
            score: 7,
            at: Utc::now(),
        };

        // demoted: Imported → Missing, file still present
        let demoted = aired_ep(&series, 1, 1);
        store.upsert_episode(&demoted).await.unwrap();
        store
            .set_episode_status(demoted.id, imported_at(&present))
            .await
            .unwrap();
        store
            .set_episode_status(demoted.id, AcquisitionStatus::Missing)
            .await
            .unwrap();

        // lost: Imported → Missing, file gone
        let lost = aired_ep(&series, 1, 2);
        store.upsert_episode(&lost).await.unwrap();
        store
            .set_episode_status(lost.id, imported_at(&gone))
            .await
            .unwrap();
        store
            .set_episode_status(lost.id, AcquisitionStatus::Missing)
            .await
            .unwrap();

        // wedged: Imported, then overwritten by a Searching run that stalled past
        // the grace — file still present
        let wedged = aired_ep(&series, 1, 3);
        store.upsert_episode(&wedged).await.unwrap();
        store
            .set_episode_status(wedged.id, imported_at(&present))
            .await
            .unwrap();
        let mut row = store.get_episode(wedged.id).await.unwrap().unwrap();
        row.status = AcquisitionStatus::Searching {
            since: Utc::now(),
            attempts: 1,
        };
        row.updated_at = Utc::now()
            - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE).unwrap()
            - chrono::Duration::minutes(5);
        store.upsert_episode(&row).await.unwrap();

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let recovered = q.reconcile_stale().await.unwrap();
        assert_eq!(recovered, 2, "demoted + wedged healed; lost left alone");

        for id in [demoted.id, wedged.id] {
            let ep = store.get_episode(id).await.unwrap().unwrap();
            match ep.status {
                AcquisitionStatus::Imported {
                    file,
                    quality,
                    score,
                    ..
                } => {
                    assert_eq!(file.path, present);
                    assert_eq!(quality, lo);
                    assert_eq!(score, 7);
                }
                other => panic!("expected Imported, got {other:?}"),
            }
        }
        let lost_row = store.get_episode(lost.id).await.unwrap().unwrap();
        assert!(matches!(lost_row.status, AcquisitionStatus::Missing));

        // Only the lost episode is still wanted — the healed ones must NOT be
        // re-grabbed (the 12 Monkeys S01E01–04 loop).
        let seeds = q.wanted().await.unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].request.tv.as_ref().unwrap().episode, Some(2));
    }

    // --- scene-style title variants (SKADI-T-0375) ---

    #[test]
    fn scene_title_converts_spaces_and_punctuation_to_dots() {
        assert_eq!(super::scene_title("Game of Thrones"), "Game.of.Thrones");
        assert_eq!(super::scene_title("The Walking Dead"), "The.Walking.Dead");
        assert_eq!(super::scene_title("The Office (US)"), "The.Office.US");
        // Already scene-style → unchanged
        assert_eq!(super::scene_title("Game.of.Thrones"), "Game.of.Thrones");
    }

    #[test]
    fn build_series_titles_includes_year_and_scene_variants() {
        use crate::series::SeriesType;
        let series = Series {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::SeriesId::new(),
            external_ids: ExternalIds::default(),
            title: "Game of Thrones".into(),
            year: Some(2011),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            status: Some("Ended".into()),
            network: None,
            series_type: SeriesType::Standard,
            seasons: Vec::new(),
            monitored: true,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/tv"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            episodes: Vec::new(),
        };
        let titles = super::build_series_titles(&series);
        // Primary, year-paren, year-appended, scene-style
        assert_eq!(
            titles,
            vec![
                "Game of Thrones",
                "Game of Thrones (2011)",
                "Game of Thrones 2011",
                "Game.of.Thrones"
            ]
        );
    }

    #[test]
    fn build_series_titles_no_year_variants_when_year_unknown() {
        use crate::series::SeriesType;
        let series = Series {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::SeriesId::new(),
            external_ids: ExternalIds::default(),
            title: "The Office".into(),
            year: None, // year unknown
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            status: Some("Ended".into()),
            network: None,
            series_type: SeriesType::Standard,
            seasons: Vec::new(),
            monitored: true,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/tv"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            episodes: Vec::new(),
        };
        let titles = super::build_series_titles(&series);
        // Only primary and scene-style (no year variants)
        assert_eq!(titles, vec!["The Office", "The.Office"]);
    }

    // --- season pack preference (SKADI-T-0377) ---

    #[tokio::test]
    async fn wanted_emits_season_pack_seed_when_backed_off_episodes_make_up_the_count() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, _, _) = profile_two_ranks();
        let series = seed_series(store, true).await;

        // S1: one episode due now, three failed and still waiting out their
        // backoff. Only one is due — but four are unsatisfied, so the pack
        // search must ride along with the due episode's re-check.
        store
            .upsert_episode(&aired_ep(&series, 1, 1))
            .await
            .unwrap();
        for n in 2..=4 {
            let mut e = aired_ep(&series, 1, n);
            e.status = AcquisitionStatus::Failed {
                reason: skadi_core::FailureReason::NoSuitableRelease,
                retry_at: Some(Utc::now() + chrono::Duration::hours(12)),
                attempts: 5,
            };
            store.upsert_episode(&e).await.unwrap();
        }
        // S2: three failed, none due → no pack seed (nothing to ride along with).
        for n in 1..=3 {
            let mut e = aired_ep(&series, 2, n);
            e.status = AcquisitionStatus::Failed {
                reason: skadi_core::FailureReason::NoSuitableRelease,
                retry_at: Some(Utc::now() + chrono::Duration::hours(12)),
                attempts: 5,
            };
            store.upsert_episode(&e).await.unwrap();
        }

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let seeds = q.wanted().await.unwrap();
        let packs: Vec<_> = seeds
            .iter()
            .filter(|s| s.acquirable.0.starts_with("season-"))
            .collect();
        assert_eq!(
            packs.len(),
            1,
            "one pack seed (S1), got {:?}",
            seeds
                .iter()
                .map(|s| s.acquirable.0.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(packs[0].request.tv.as_ref().unwrap().season, 1);
        assert_eq!(seeds.len(), 2, "pack + the one due episode");
    }

    #[tokio::test]
    async fn wanted_emits_season_pack_seed_when_multiple_episodes_missing() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let (profile, _, _) = profile_two_ranks();
        let series = seed_series(store, true).await;

        // Add 4 missing episodes in season 1 (exceeds SEASON_PACK_THRESHOLD of 3)
        for n in 1..=4 {
            store
                .upsert_episode(&aired_ep(&series, 1, n))
                .await
                .unwrap();
        }
        // Add 2 missing episodes in season 2 (below threshold)
        for n in 1..=2 {
            store
                .upsert_episode(&aired_ep(&series, 2, n))
                .await
                .unwrap();
        }
        // Add 3 missing specials (at threshold) — season 0 never gets a pack seed.
        for n in 1..=3 {
            store
                .upsert_episode(&aired_ep(&series, 0, n))
                .await
                .unwrap();
        }

        let q = SeriesWantedQuery::new(
            Arc::new(db.store.clone()),
            WantedScoring {
                profile,
                search_undated_episodes: false,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let seeds = q.wanted().await.unwrap();

        // Should have: 1 season pack (S1) + 4 episode seeds (S1) + 2 episode seeds (S2)
        // + 3 special seeds (S0, no pack) = 10
        assert_eq!(seeds.len(), 10, "expected 10 seeds, got {:?}", seeds.len());
        assert!(
            seeds
                .iter()
                .all(|s| s.request.tv.as_ref().unwrap().season != 0
                    || s.request.tv.as_ref().unwrap().episode.is_some()),
            "no season-0 pack seed"
        );

        // First seed should be the season pack for S1 (episode: None)
        let season_pack = &seeds[0];
        assert_eq!(season_pack.request.tv.as_ref().unwrap().season, 1);
        assert_eq!(
            season_pack.request.tv.as_ref().unwrap().episode,
            None,
            "season pack should have episode: None"
        );
        assert!(
            season_pack.acquirable.0.starts_with("season-"),
            "season pack acquirable ref should start with 'season-'"
        );

        // Season 2 should NOT have a season pack (only 2 episodes)
        let s2_packs: Vec<_> = seeds
            .iter()
            .filter(|s| {
                s.request.tv.as_ref().unwrap().season == 2
                    && s.request.tv.as_ref().unwrap().episode.is_none()
            })
            .collect();
        assert!(
            s2_packs.is_empty(),
            "season 2 should not have a season pack (only 2 episodes)"
        );
    }
}
