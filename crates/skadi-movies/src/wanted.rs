//! `MovieWantedQuery` — the hunter's sweep input for the movies domain
//! (SKADI-T-0047).
//!
//! Emits one [`AcquireSeed`] per `MovieEdition` that needs a release:
//! - **`wanted()`** — monitored movies with a `Missing` edition.
//! - **`upgradable()`** — monitored, `Imported` editions whose current quality
//!   rank in the profile is below the cutoff rank, when the profile permits
//!   upgrades.
//!
//! v0 uses `MoviesRepo::list_movies(monitored=true)` + a per-movie
//! `list_editions` call (simple, easy to test). T-0042 covers replacing this
//! double round-trip with a proper SQL join once we hit scale.

use std::sync::Arc;

use async_trait::async_trait;

use skadi_core::{AcquisitionStatus, ExternalIds, MediaKind, Result};
use skadi_hunter::{AcquireSeed, SearchSpec, WantedQuery};
use skadi_indexers::Category;
use skadi_quality::QualityProfile;

use crate::edition::MovieEdition;
use crate::movie::Movie;
use crate::repo::{MovieFilter, MoviesRepo};

/// Torznab movies category, the default search scope for `MovieWantedQuery`.
const MOVIE_CATEGORY: Category = Category(2000);

/// Whether the sweep should (re-)acquire an edition in this status: `Missing`, or
/// a transfer-`Failed` edition whose `retry_at` backoff has elapsed — recovering
/// a flaky source. A `Failed{retry_at: None}` (no-suitable-release / search
/// failure) is NOT auto-retried.
fn wants_acquire(status: &AcquisitionStatus, now: chrono::DateTime<chrono::Utc>) -> bool {
    match status {
        AcquisitionStatus::Missing => true,
        AcquisitionStatus::Failed {
            retry_at: Some(t), ..
        } => *t <= now,
        _ => false,
    }
}

/// Scoring inputs the sweep needs to decide whether an imported edition is
/// below the profile cutoff. v0 carries a single profile; the daemon (I-0008)
/// will swap this for a per-movie store-backed lookup.
#[derive(Clone)]
pub struct WantedScoring {
    pub profile: QualityProfile,
    /// Opt-in "upgrade until custom-format score" (SKADI-T-0187, ADR
    /// [[SKADI-A-0003]] §2): when `> 0`, the sweep also re-checks **at/above
    /// quality-cutoff** imported editions whose held format score is below this,
    /// so a Proper/Repack (a higher-scoring format) is found for an already-best-
    /// quality file. `0` (default) disables it — no at-cutoff re-search, so a
    /// fresh install never floods indexers. A profile policy surfaced here because
    /// only the sweep's `upgradable()` consumes it (the decision stays
    /// strictly-higher, SKADI-T-0186).
    pub upgrade_until_format_score: i32,
    /// Re-acquire files the media scan says will not direct-play (SKADI-T-0584).
    ///
    /// **Off by default, deliberately.** The scan and the re-grab are separate
    /// decisions: profiling the library is free and reversible, while re-grabbing
    /// spends bandwidth and retires files. Worse, until the direct-play custom
    /// formats carry a score, a re-grab can fetch another release with the same
    /// undecodable audio — trading one silent file for another.
    ///
    /// Config key `regrab_unplayable`.
    pub regrab_unplayable: bool,
}

/// `WantedQuery` impl for movies.
/// `quality_id`'s rank within `profile`'s ladder, or `None` when it is not in it.
fn rank_in(profile: &QualityProfile, quality_id: skadi_core::QualityId) -> Option<usize> {
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

pub struct MovieWantedQuery {
    repo: Arc<dyn MoviesRepo>,
    scoring: WantedScoring,
    /// Config plane, for resolving an item's **own** quality profile
    /// (SKADI-T-0537). `None` ⇒ every item is judged against the domain-wide
    /// profile, which is the pre-T-0537 behaviour and what a caller with no store
    /// (the BDD harness, unit tests) still gets.
    store: Option<skadi_store::Store>,
}

impl MovieWantedQuery {
    #[must_use]
    pub fn new(repo: Arc<dyn MoviesRepo>, scoring: WantedScoring) -> Self {
        Self {
            repo,
            scoring,
            store: None,
        }
    }

    /// Resolve each item's own profile rather than the domain-wide fallback
    /// (SKADI-T-0537).
    #[must_use]
    pub fn with_store(mut self, store: skadi_store::Store) -> Self {
        self.store = Some(store);
        self
    }

    /// The profile to judge `item` against: its own when we can resolve one,
    /// else the domain-wide fallback (SKADI-T-0537).
    ///
    /// `decide` has scored each candidate against the item's own profile since
    /// SKADI-T-0531, but *upgradability* was still judged on the domain-wide one.
    /// The two halves disagreed: an item on a non-default profile could be
    /// re-searched every sweep and never be grabbable, or be genuinely upgradable
    /// and never re-searched.
    ///
    /// `cache` holds one resolution per distinct profile id for the whole sweep —
    /// `upgradable()` walks the entire library, so a per-item lookup would be one
    /// settings read per item rather than per profile.
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
        // A missing or invalid profile row falls back to the domain-wide default
        // — which SKADI-T-0532 made an operator-chosen row rather than whichever
        // sorted first, so the fallback is now a real answer instead of an
        // arbitrary one.
        let profile = resolved.unwrap_or_else(|| self.scoring.profile.clone());
        cache.insert(id, profile.clone());
        profile
    }

    /// Build a seed for `edition`. `current` is the `(quality, format_score)` the
    /// edition already holds when this is an *upgrade* seed (`Some` from
    /// `upgradable()`), or `None` for a first acquisition (from `wanted()`) —
    /// threaded so the hunter's `decide` only grabs a strictly-better release on
    /// the quality axis (SKADI-T-0182) or the format-score axis (SKADI-T-0186).
    fn seed_for(
        &self,
        movie: &Movie,
        edition: &MovieEdition,
        current: Option<(skadi_core::QualityId, i32)>,
    ) -> AcquireSeed {
        AcquireSeed {
            acquirable: edition.acquirable_ref(),
            request: SearchSpec {
                // Sweep-driven; the interactive paths override this just before
                // searching (SKADI-T-0539).
                trigger: skadi_hunter::SearchTrigger::Automatic,
                kind: MediaKind::Movie,
                titles: build_titles(movie),
                year: movie.year,
                external_ids: ExternalIds {
                    tmdb: movie.external_ids.tmdb.clone(),
                    imdb: movie.external_ids.imdb.clone(),
                    ..Default::default()
                },
                categories: vec![MOVIE_CATEGORY],
                tv: None,
                series: None,
                // Filled in by `steps::search` from the item's tags
                // (SKADI-T-0556) — a database read, so it cannot happen in these
                // pure seed builders.
                tags: None,
            },
            profile: movie.profile,
            current_quality: current.map(|(q, _)| q),
            current_format_score: current.map(|(_, s)| s),
            // Read from the edition's own probe rather than passed in: every
            // caller would otherwise have to remember it, and a forgotten
            // `false` silently reinstates the strictly-better bar that keeps an
            // unwatchable file in place (SKADI-T-0584).
            current_unplayable: edition
                .media_info
                .as_ref()
                .is_some_and(skadi_core::is_broken),
        }
    }
}

/// Convert a title to scene-style naming (dots for spaces/punctuation).
/// Scene releases name files like `Rogue.One.A.Star.Wars.Story.2016` —
/// searching with spaces may miss them on some indexers (SKADI-T-0375).
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

/// Primary title first; original_title appended if distinct; scene-style
/// variant appended if different from the primary.
fn build_titles(movie: &Movie) -> Vec<String> {
    let mut out = vec![movie.title.clone()];
    if let Some(ot) = &movie.original_title
        && ot != &movie.title
    {
        out.push(ot.clone());
    }
    // Scene-style variant: non-alphanumeric → dots, consecutive dots collapsed.
    let scene = scene_title(&movie.title);
    if scene != movie.title && !out.contains(&scene) {
        out.push(scene);
    }
    out
}

#[async_trait]
impl WantedQuery for MovieWantedQuery {
    async fn wanted(&self) -> Result<Vec<AcquireSeed>> {
        let movies = self
            .repo
            .list_movies(MovieFilter {
                monitored: Some(true),
                limit: None,
                offset: None,
            })
            .await?;
        let now = chrono::Utc::now();
        let mut out = Vec::new();
        for movie in &movies {
            for edition in &movie.editions {
                if wants_acquire(&edition.status, now) {
                    // First acquisition: no held quality/score to upgrade over.
                    out.push(self.seed_for(movie, edition, None));
                }
            }
        }
        // Sweep size (SKADI-T-0456): previously invisible, so "why is it
        // searching hundreds of things?" had no answer short of reading the
        // database. DEBUG — it fires every sweep, and the number only matters
        // when something looks wrong.
        tracing::debug!(domain = "movies", seeds = out.len(), "sweep produced seeds");
        Ok(out)
    }

    async fn reconcile_stale(&self) -> Result<usize> {
        // Recover editions wedged in a non-terminal acquire state for longer than
        // the grace (SKADI-T-0112): reset to `Missing` so this sweep's `wanted()`
        // re-acquires them. `monitored: None` so even a stuck edition on a now-
        // unmonitored movie gets un-wedged (it just stays `Missing` then).
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE)
                .unwrap_or_else(|_| chrono::Duration::seconds(900));
        let movies = self
            .repo
            .list_movies(MovieFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        let mut recovered = 0;
        for movie in &movies {
            for edition in &movie.editions {
                // Heal a demoted import (SKADI-T-0385/0388): `file_path` is written
                // ONLY by an `Imported` status write, so a `Missing`/`Failed` row that
                // still carries a path was imported once and later overwritten by a
                // duplicate acquire run. If the file is still on disk the library holds
                // it — restore `Imported` rather than re-grabbing it every sweep (live,
                // this is how a held 101 Dalmatians (1996) kept snatching its sequel).
                if matches!(
                    edition.status,
                    AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
                ) {
                    if let Some(imported) = restore_demoted_import(edition).await {
                        tracing::warn!(
                            edition = %edition.id,
                            path = %imported_path(&imported),
                            "restoring demoted edition import (file still on disk)"
                        );
                        self.repo.set_edition_status(edition.id, imported).await?;
                        recovered += 1;
                    }
                    continue;
                }
                let in_flight = matches!(
                    edition.status,
                    AcquisitionStatus::Searching { .. }
                        | AcquisitionStatus::Snatched { .. }
                        | AcquisitionStatus::Downloading { .. }
                );
                if in_flight && edition.updated_at < cutoff {
                    // A wedged row whose file is on disk is the same demoted import:
                    // restore it directly rather than bouncing through `Missing`, which
                    // would let `wanted()` re-grab it in this very sweep.
                    let next = match restore_demoted_import(edition).await {
                        Some(imported) => {
                            tracing::warn!(
                                edition = %edition.id,
                                stuck_since = %edition.updated_at,
                                path = %imported_path(&imported),
                                "recovering wedged edition (file on disk → Imported)"
                            );
                            imported
                        }
                        None => {
                            tracing::warn!(
                                edition = %edition.id,
                                stuck_since = %edition.updated_at,
                                "recovering wedged edition (reset to Missing)"
                            );
                            AcquisitionStatus::Missing
                        }
                    };
                    self.repo.set_edition_status(edition.id, next).await?;
                    recovered += 1;
                }
            }
        }
        Ok(recovered)
    }

    async fn upgradable(&self) -> Result<Vec<AcquireSeed>> {
        // NOT short-circuited on the domain-wide profile (SKADI-T-0537): with
        // per-item profiles, `upgrade_allowed` on the *default* says nothing about
        // an item assigned a profile that does allow upgrades. The per-item check
        // is in the loop; when no store is wired every item resolves to the
        // domain profile anyway, so this costs a list_movies and nothing else.
        let defs = skadi_quality::default_definitions();
        let mut profiles: std::collections::HashMap<skadi_core::ProfileId, QualityProfile> =
            std::collections::HashMap::new();
        let movies = self
            .repo
            .list_movies(MovieFilter {
                monitored: Some(true),
                limit: None,
                offset: None,
            })
            .await?;
        let mut out = Vec::new();
        for movie in &movies {
            // The movie's OWN profile (SKADI-T-0537), resolved once per distinct
            // id across the whole sweep.
            let profile = self.profile_for(movie.profile, &defs, &mut profiles).await;
            // `upgrade_allowed` is per-profile too: an item on a profile with
            // upgrades off must not be swept just because the *domain* default
            // allows them.
            if !profile.upgrade_allowed {
                continue;
            }
            let cutoff_rank = cutoff_rank_in(&profile);
            for edition in &movie.editions {
                // Read the current quality from the status variant — single
                // source of truth (the `quality` column is a denormalized
                // mirror that MovieStatusSink keeps in sync on Imported).
                // CONSUMED INTERFACE (skadi_core): AcquisitionStatus::Imported
                // carries the held `quality: QualityId` and aggregate format
                // `score: i32`; both thread into the seed so the hunter upgrades
                // on the quality axis (SKADI-T-0182) or the format-score axis
                // (SKADI-T-0186, proper/repack).
                let (qid, fscore) = match &edition.status {
                    AcquisitionStatus::Imported { quality, score, .. } => (*quality, *score),
                    _ => continue,
                };
                // An UNASSESSED file is not an upgrade candidate (SKADI-T-0399): we
                // do not know what we hold, so "below cutoff" is not a fact about it.
                // 1,354 adopted editions on prod carried the SDTV floor and dragged
                // the whole library into the sweep the moment upgrades were enabled.
                // A *known* tier outside the profile still ranks 0 (Radarr parity:
                // out-of-profile counts as cutoff-unmet).
                if skadi_quality::is_unknown_quality(qid) {
                    continue;
                }
                let rank = rank_in(&profile, qid).unwrap_or(0);
                // Quality-axis: held below the quality cutoff (SKADI-T-0182). OR
                // format-axis (SKADI-T-0187, opt-in): held at/above the quality
                // cutoff but its format score is below `upgrade_until_format_score`
                // — re-check for a Proper/Repack. Default `0` keeps the format
                // condition off (`fscore < 0` is never true for a real score).
                let wants_format_upgrade = fscore < self.scoring.upgrade_until_format_score;
                // Third axis (SKADI-T-0584): the file was probed and does not
                // direct-play — DTS-only audio, an m2ts, 10-bit H.264. It can sit
                // at the top of the ladder with a high format score and still be
                // unwatchable on a phone, so neither of the other two axes will
                // ever pick it up.
                //
                // Requires a probe: an UNSCANNED file is not a candidate, for the
                // same reason an unassessed quality is not (SKADI-T-0399) — "we
                // never looked" must not read as "it is broken", or the whole
                // library sweeps the moment the feature ships.
                let unplayable = edition
                    .media_info
                    .as_ref()
                    .is_some_and(skadi_core::is_broken);
                if rank < cutoff_rank
                    || wants_format_upgrade
                    || (unplayable && self.scoring.regrab_unplayable)
                {
                    out.push(self.seed_for(movie, edition, Some((qid, fscore))));
                }
            }
        }
        // Sweep size (SKADI-T-0456): previously invisible, so "why is it
        // searching hundreds of things?" had no answer short of reading the
        // database. DEBUG — it fires every sweep, and the number only matters
        // when something looks wrong.
        tracing::debug!(domain = "movies", seeds = out.len(), "sweep produced seeds");
        Ok(out)
    }
}

/// The `Imported` status to restore for an edition row that still has its library
/// file on disk (SKADI-T-0385/0388), or `None` when the row never recorded a path
/// or the file is gone (a real loss — leave it wanted). Quality/score come from
/// the row's own columns (also written only at import); a row with no quality
/// falls back to the lowest default tier so a later upgrade run can replace it.
async fn restore_demoted_import(edition: &MovieEdition) -> Option<AcquisitionStatus> {
    let path = edition.file.as_ref()?.path.clone();
    let probe = path.clone();
    let exists = tokio::task::spawn_blocking(move || probe.is_file())
        .await
        .unwrap_or(false);
    if !exists {
        return None;
    }
    Some(AcquisitionStatus::Imported {
        file: skadi_core::FileRef { path },
        // A row with no recorded quality is restored as UNKNOWN, never as the
        // ladder's lowest tier: "we never assessed this file" must not read as
        // "SDTV", which made every healed row an upgrade candidate (SKADI-T-0399).
        quality: edition.quality.unwrap_or(skadi_quality::UNKNOWN_QUALITY_ID),
        score: edition.format_score,
        at: chrono::Utc::now(),
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
    use chrono::Utc;
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;
    use skadi_core::{
        AcquisitionStatus, EditionKindId, FileRef, ProfileId, QualityId, RootFolder, TmdbId,
    };
    use skadi_quality::{QualityProfile, default_definitions};
    use skadi_store::Store;

    use crate::SQLITE_MIGRATIONS;
    use crate::THEATRICAL_KIND_ID;

    async fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skadi.db");
        let url = format!("sqlite://{}", path.display());
        let s = Store::connect(&url).unwrap();
        s.run_migrations().await.unwrap();
        drop(s);
        let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
        (dir, Store::connect(&url).unwrap())
    }

    fn profile_two_ranks() -> (QualityProfile, QualityId, QualityId) {
        // Same pattern used in skadi-hunter's pipeline tests.
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

    async fn seed(store: &Store, monitored: bool, status: AcquisitionStatus) -> Movie {
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: skadi_core::ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: None,
            year: Some(1999),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        store.upsert_movie(&movie).await.unwrap();
        let mut e = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
        e.status = status;
        store.upsert_edition(&e).await.unwrap();
        movie
    }

    /// Seed a movie whose imported edition is **at cutoff** — the normal upgrade
    /// axes cannot touch it — carrying `media_info`.
    async fn seed_at_cutoff_with_media(
        store: &Store,
        quality: skadi_core::QualityId,
        info: Option<skadi_core::MediaInfo>,
    ) -> Movie {
        let movie = seed(
            store,
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/x.mkv".into(),
                },
                quality,
                // A high held score, so the format axis cannot fire either.
                score: 10_000,
                at: Utc::now(),
            },
        )
        .await;
        let mut e = store.list_editions(movie.id).await.unwrap().remove(0);
        e.media_info = info;
        store.upsert_edition(&e).await.unwrap();
        movie
    }

    fn dts_only() -> skadi_core::MediaInfo {
        skadi_core::MediaInfo {
            container: Some("mkv".into()),
            audio_tracks: vec![skadi_core::AudioInfo {
                codec: Some("dts".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// The whole point of SKADI-T-0584: a file at the top of the ladder with a
    /// high format score, which plays **silently** on a phone. Neither existing
    /// upgrade axis can reach it.
    #[tokio::test]
    async fn an_unplayable_file_is_upgradable_even_at_cutoff() {
        let (_d, store) = fresh_store().await;
        let (profile, _lo, hi) = profile_two_ranks();
        let _m = seed_at_cutoff_with_media(&store, hi, Some(dts_only())).await;

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: true,
            },
        );
        let seeds = q.upgradable().await.unwrap();
        assert_eq!(seeds.len(), 1, "a silent file must be re-acquirable");
        assert!(
            seeds[0].current_unplayable,
            "the seed must tell decide to relax the strictly-better bar, or the \
             search runs and rejects everything"
        );
    }

    /// The kill switch (SKADI-T-0584). Scanning the library and acting on the
    /// scan are separate decisions: with `regrab_unplayable` off — the default —
    /// a broken file is still reported, and still not re-acquired.
    ///
    /// This matters because four of the live profiles have `upgrade_allowed:
    /// true`, so without the gate the sweep would start re-grabbing the moment
    /// the scan found its first DTS-only file.
    #[tokio::test]
    async fn the_regrab_gate_is_off_by_default() {
        let (_d, store) = fresh_store().await;
        let (profile, _lo, hi) = profile_two_ranks();
        let _m = seed_at_cutoff_with_media(&store, hi, Some(dts_only())).await;

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(
            q.upgradable().await.unwrap().is_empty(),
            "an unplayable file must NOT be re-acquired until the operator opts in"
        );
    }

    /// The guard that stops this feature sweeping the entire library: a file that
    /// has simply not been scanned yet is NOT a candidate. 1,818 movie editions
    /// on prod had no media info at all when this shipped.
    #[tokio::test]
    async fn an_unscanned_file_at_cutoff_is_not_swept() {
        let (_d, store) = fresh_store().await;
        let (profile, _lo, hi) = profile_two_ranks();
        let _m = seed_at_cutoff_with_media(&store, hi, None).await;

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(
            q.upgradable().await.unwrap().is_empty(),
            "unscanned must not read as broken"
        );
    }

    /// ...and a file that was scanned and is fine stays put.
    #[tokio::test]
    async fn a_playable_file_at_cutoff_is_not_swept() {
        let (_d, store) = fresh_store().await;
        let (profile, _lo, hi) = profile_two_ranks();
        let good = skadi_core::MediaInfo {
            container: Some("mkv".into()),
            audio_tracks: vec![skadi_core::AudioInfo {
                codec: Some("aac".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _m = seed_at_cutoff_with_media(&store, hi, Some(good)).await;

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(q.upgradable().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn wanted_returns_missing_editions_on_monitored_movies_only() {
        let (_d, store) = fresh_store().await;
        let (profile, _, _) = profile_two_ranks();
        let _wanted_movie = seed(&store, true, AcquisitionStatus::Missing).await;
        let _unmonitored = seed_unrelated_unmonitored(&store).await;

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let seeds = q.wanted().await.unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].profile, _wanted_movie.profile);
        // First acquisition: no held quality to upgrade over (SKADI-T-0182).
        assert_eq!(seeds[0].current_quality, None);
        assert_eq!(seeds[0].request.year, Some(1999));
        assert_eq!(seeds[0].request.kind, MediaKind::Movie);
        assert_eq!(seeds[0].request.categories, vec![MOVIE_CATEGORY]);
    }

    #[tokio::test]
    async fn reconcile_stale_resets_wedged_editions_only_past_the_grace() {
        let (_d, store) = fresh_store().await;
        let (profile, _, _) = profile_two_ranks();

        // Seed a movie with a `Downloading` edition stamped at `updated_at`.
        async fn seed_downloading(
            store: &Store,
            tmdb: u64,
            updated_at: chrono::DateTime<Utc>,
        ) -> skadi_core::MovieEditionId {
            let movie = Movie {
                content_rating: None,
                genres: Vec::new(),
                id: skadi_core::MovieId::new(),
                external_ids: skadi_core::ExternalIds {
                    tmdb: Some(TmdbId(tmdb)),
                    ..Default::default()
                },
                title: format!("M{tmdb}"),
                original_title: None,
                year: Some(2020),
                overview: None,
                runtime_minutes: None,
                poster_url: None,
                backdrop_url: None,
                collection: None,
                monitored: true,
                profile: ProfileId::new(),
                root_folder: RootFolder::new("/movies"),
                added_at: Utc::now(),
                last_metadata_refresh: None,
                editions: Vec::new(),
            };
            store.upsert_movie(&movie).await.unwrap();
            let mut e = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
            e.status = AcquisitionStatus::Downloading {
                release: skadi_core::ReleaseId::new(),
                progress: 0.0,
            };
            e.updated_at = updated_at;
            store.upsert_edition(&e).await.unwrap();
            e.id
        }

        // One wedged well past the grace, one fresh (just started).
        let stale = seed_downloading(
            &store,
            701,
            Utc::now()
                - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE).unwrap()
                - chrono::Duration::minutes(5),
        )
        .await;
        let fresh = seed_downloading(&store, 702, Utc::now()).await;

        let q = MovieWantedQuery::new(
            Arc::new(store.clone()),
            WantedScoring {
                profile,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let recovered = q.reconcile_stale().await.unwrap();
        assert_eq!(recovered, 1, "only the wedged edition is recovered");

        // Stale → reset to Missing; fresh → untouched (defers to Cloacina).
        assert!(matches!(
            store.get_edition(stale).await.unwrap().unwrap().status,
            AcquisitionStatus::Missing
        ));
        assert!(matches!(
            store.get_edition(fresh).await.unwrap().unwrap().status,
            AcquisitionStatus::Downloading { .. }
        ));

        // And now the reset edition shows up as wanted (so the same sweep re-acquires it).
        let wanted = q.wanted().await.unwrap();
        assert!(wanted.iter().any(|s| s.acquirable.0 == stale.to_string()));
    }

    /// SKADI-T-0385/0388: an edition imported once and later demoted (a duplicate
    /// run overwrote `Imported`) keeps its `file_path`; if that file is still on
    /// disk, `reconcile_stale` restores `Imported` instead of letting `wanted()`
    /// re-grab a film the library already holds. A demoted row whose file is
    /// gone stays wanted; a wedged in-flight row with its file goes straight to
    /// `Imported` (not via `Missing`).
    #[tokio::test]
    async fn reconcile_stale_restores_demoted_imports_whose_file_exists() {
        let (_d, store) = fresh_store().await;
        let (profile, lo, _) = profile_two_ranks();
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("101-dalmatians_(1996).mkv");
        std::fs::write(&present, b"x").unwrap();
        let gone = dir.path().join("gone.mkv");

        async fn seed_edition(store: &Store, tmdb: u64) -> skadi_core::MovieEditionId {
            let movie = Movie {
                content_rating: None,
                genres: Vec::new(),
                id: skadi_core::MovieId::new(),
                external_ids: skadi_core::ExternalIds {
                    tmdb: Some(TmdbId(tmdb)),
                    ..Default::default()
                },
                title: format!("M{tmdb}"),
                original_title: None,
                year: Some(1996),
                overview: None,
                runtime_minutes: None,
                poster_url: None,
                backdrop_url: None,
                collection: None,
                monitored: true,
                profile: ProfileId::new(),
                root_folder: RootFolder::new("/movies"),
                added_at: Utc::now(),
                last_metadata_refresh: None,
                editions: Vec::new(),
            };
            store.upsert_movie(&movie).await.unwrap();
            let e = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
            store.upsert_edition(&e).await.unwrap();
            e.id
        }
        let imported_at = |path: &std::path::Path| AcquisitionStatus::Imported {
            file: skadi_core::FileRef {
                path: path.to_path_buf(),
            },
            quality: lo,
            score: 7,
            at: Utc::now(),
        };

        // Import, then demote — `file_path`/`quality_id` survive the demotion.
        let demoted = seed_edition(&store, 801).await;
        store
            .set_edition_status(demoted, imported_at(&present))
            .await
            .unwrap();
        store
            .set_edition_status(demoted, AcquisitionStatus::Missing)
            .await
            .unwrap();
        // Demoted, but the file is gone: a real loss, stays wanted.
        let lost = seed_edition(&store, 802).await;
        store
            .set_edition_status(lost, imported_at(&gone))
            .await
            .unwrap();
        store
            .set_edition_status(lost, AcquisitionStatus::Missing)
            .await
            .unwrap();
        // Wedged in-flight past the grace with its file on disk → Imported directly.
        let wedged = seed_edition(&store, 803).await;
        store
            .set_edition_status(wedged, imported_at(&present))
            .await
            .unwrap();
        let mut row = store.get_edition(wedged).await.unwrap().unwrap();
        row.status = AcquisitionStatus::Searching {
            since: Utc::now(),
            attempts: 1,
        };
        row.updated_at = Utc::now()
            - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE).unwrap()
            - chrono::Duration::minutes(5);
        store.upsert_edition(&row).await.unwrap();

        let q = MovieWantedQuery::new(
            Arc::new(store.clone()),
            WantedScoring {
                profile,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let recovered = q.reconcile_stale().await.unwrap();
        assert_eq!(recovered, 2, "demoted + wedged restored; lost untouched");

        for id in [demoted, wedged] {
            match store.get_edition(id).await.unwrap().unwrap().status {
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
                other => panic!("{id}: expected Imported, got {other:?}"),
            }
        }
        assert!(matches!(
            store.get_edition(lost).await.unwrap().unwrap().status,
            AcquisitionStatus::Missing
        ));
        // Only the lost one is wanted — the held films are not re-grabbed.
        let wanted = q.wanted().await.unwrap();
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].acquirable.0, lost.to_string());
    }

    // A second movie that's unmonitored with the same TMDB id namespace would
    // collide; use a distinct TmdbId.
    async fn seed_unrelated_unmonitored(store: &Store) -> Movie {
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: skadi_core::ExternalIds {
                tmdb: Some(TmdbId(604)),
                ..Default::default()
            },
            title: "Off the books".into(),
            original_title: None,
            year: Some(2010),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored: false,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        store.upsert_movie(&movie).await.unwrap();
        let e = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
        store.upsert_edition(&e).await.unwrap();
        movie
    }

    #[tokio::test]
    async fn upgradable_returns_imported_below_cutoff_when_profile_allows() {
        let (_d, store) = fresh_store().await;
        let (profile, lo, _hi) = profile_two_ranks();
        // Movie imported at lo (Bluray-720p), profile cutoff is hi (1080p) → upgradable.
        let _m = seed(
            &store,
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/x.mkv".into(),
                },
                quality: lo,
                score: 0,
                at: Utc::now(),
            },
        )
        .await;

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        let seeds = q.upgradable().await.unwrap();
        assert_eq!(seeds.len(), 1);
        // The upgrade seed carries the held quality + format score so the hunter's
        // `decide` upgrades on either axis (SKADI-T-0182 / SKADI-T-0186).
        assert_eq!(seeds[0].current_quality, Some(lo));
        assert_eq!(seeds[0].current_format_score, Some(0));
    }

    #[tokio::test]
    async fn upgradable_returns_nothing_when_at_cutoff() {
        let (_d, store) = fresh_store().await;
        let (profile, _lo, hi) = profile_two_ranks();
        let _m = seed(
            &store,
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/x.mkv".into(),
                },
                quality: hi,
                score: 0,
                at: Utc::now(),
            },
        )
        .await;
        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(q.upgradable().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upgradable_at_cutoff_emits_when_format_cutoff_opted_in() {
        // SKADI-T-0187: an edition at the quality cutoff with a held format score
        // below `upgrade_until_format_score` is re-checked (opt-in) for a proper.
        let (_d, store) = fresh_store().await;
        let (profile, _lo, hi) = profile_two_ranks();
        let _m = seed(
            &store,
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/x.mkv".into(),
                },
                quality: hi, // at the quality cutoff
                score: 0,    // non-proper
                at: Utc::now(),
            },
        )
        .await;
        // Opt in (cutoff 100): held score 0 < 100 → emitted, carrying (quality, score).
        let q = MovieWantedQuery::new(
            Arc::new(store.clone()),
            WantedScoring {
                profile: profile.clone(),
                upgrade_until_format_score: 100,
                regrab_unplayable: false,
            },
        );
        let seeds = q.upgradable().await.unwrap();
        assert_eq!(seeds.len(), 1, "at-cutoff item re-checked when opted in");
        assert_eq!(seeds[0].current_quality, Some(hi));
        assert_eq!(seeds[0].current_format_score, Some(0));

        // A file already at/above the format cutoff is NOT re-checked.
        let q_done = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile,
                upgrade_until_format_score: 0, // (held 0 is not < 0)
                regrab_unplayable: false,
            },
        );
        assert!(q_done.upgradable().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upgradable_returns_nothing_when_profile_disallows() {
        let (_d, store) = fresh_store().await;
        let (mut profile, lo, _hi) = profile_two_ranks();
        profile.upgrade_allowed = false;
        let _m = seed(
            &store,
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/x.mkv".into(),
                },
                quality: lo,
                score: 0,
                at: Utc::now(),
            },
        )
        .await;
        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(q.upgradable().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unmonitored_movies_never_appear_in_wanted_or_upgradable() {
        let (_d, store) = fresh_store().await;
        let (profile, lo, _) = profile_two_ranks();
        // Single movie, unmonitored, with a Missing edition AND an Imported-below-cutoff
        // edition. Neither should be emitted.
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: skadi_core::ExternalIds {
                tmdb: Some(TmdbId(700)),
                ..Default::default()
            },
            title: "Quiet".into(),
            original_title: None,
            year: Some(2001),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored: false,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        store.upsert_movie(&movie).await.unwrap();
        let e1 = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
        store.upsert_edition(&e1).await.unwrap();
        let mut e2 = MovieEdition::missing(
            movie.id,
            EditionKindId::from(uuid::uuid!("00000000-0000-0000-0000-000000000002")),
        );
        e2.status = AcquisitionStatus::Imported {
            file: FileRef {
                path: "/movies/x.mkv".into(),
            },
            quality: lo,
            score: 0,
            at: Utc::now(),
        };
        store.upsert_edition(&e2).await.unwrap();

        let q = MovieWantedQuery::new(
            Arc::new(store),
            WantedScoring {
                profile,
                upgrade_until_format_score: 0,
                regrab_unplayable: false,
            },
        );
        assert!(q.wanted().await.unwrap().is_empty());
        assert!(q.upgradable().await.unwrap().is_empty());
    }

    // --- scene-style title variants (SKADI-T-0375) ---

    #[test]
    fn scene_title_converts_spaces_and_punctuation_to_dots() {
        assert_eq!(super::scene_title("The Matrix"), "The.Matrix");
        assert_eq!(
            super::scene_title("Rogue One: A Star Wars Story"),
            "Rogue.One.A.Star.Wars.Story"
        );
        assert_eq!(
            super::scene_title("Spider-Man: No Way Home"),
            "Spider.Man.No.Way.Home"
        );
        assert_eq!(super::scene_title("Fast & Furious"), "Fast.Furious");
        // Already scene-style → unchanged
        assert_eq!(super::scene_title("The.Matrix"), "The.Matrix");
        // Consecutive punctuation collapsed
        assert_eq!(super::scene_title("Test  --  Movie"), "Test.Movie");
    }

    #[test]
    fn build_titles_includes_scene_variant() {
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: Default::default(),
            title: "The Matrix".into(),
            original_title: None,
            year: Some(1999),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored: true,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        let titles = super::build_titles(&movie);
        assert_eq!(titles, vec!["The Matrix", "The.Matrix"]);
    }

    #[test]
    fn build_titles_does_not_duplicate_scene_if_already_scene() {
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: Default::default(),
            title: "The.Matrix".into(), // already scene-style
            original_title: None,
            year: Some(1999),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored: true,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        let titles = super::build_titles(&movie);
        // Only one entry, no duplicate
        assert_eq!(titles, vec!["The.Matrix"]);
    }
}
