//! `EpisodeMatcher` — `AcquirableMatcher` for the television domain (SKADI-T-0270).
//!
//! Constructed **per acquire run** with a snapshot of the run's [`Series`] + its
//! [`Episode`] rows. `match_file` re-parses each source filename with
//! [`parse_tv`] (the shared importer hands us the *movie* parse, which has no
//! episode info) and maps it to the episode(s) it satisfies:
//! - **single** `S01E05` → one episode,
//! - **season pack** → handled for free: the importer calls `match_file` once
//!   per file in the download, so each episode file matches independently,
//! - **multi-episode** `S01E05E06` (one file, several episodes) → one match per
//!   episode, same dest, `Overwrite` so each is recorded,
//! - **anime absolute** (`- 134`) → matched on `absolute_number`,
//! - **daily** (`2024-03-01`) → matched on `air_date`.
//!
//! No async, no DB, no global state — exhaustively unit-testable.

use std::path::{Path, PathBuf};

use skadi_importer::{AcquirableMatch, AcquirableMatcher, CollisionPolicy, CompletedDownload};
use skadi_quality::{ParsedRelease, parse_tv};

/// Minimum title coverage for a file to be accepted as this series
/// (SKADI-T-0445); mirrors the hunter's decide-side threshold.
const MIN_TITLE_COVERAGE: f32 = 0.6;

use crate::episode::Episode;
use crate::naming::{EpisodeNaming, SeriesNaming};
use crate::series::Series;

/// Domain-supplied matcher built per acquire run.
pub struct EpisodeMatcher {
    series: Series,
    episodes: Vec<Episode>,
    naming: SeriesNaming,
}

impl EpisodeMatcher {
    /// Build from a per-run snapshot with the **default** naming templates.
    #[must_use]
    pub fn new(series: Series, episodes: Vec<Episode>) -> Self {
        Self::with_naming(series, episodes, SeriesNaming::default())
    }

    /// Build with explicit (operator-configured) naming templates.
    #[must_use]
    pub fn with_naming(series: Series, episodes: Vec<Episode>, naming: SeriesNaming) -> Self {
        Self {
            series,
            episodes,
            naming,
        }
    }

    /// Is this parsed file plausibly *this* series (SKADI-T-0445)?
    ///
    /// Sonarr matches a download to a series by title (or a series id in the
    /// name) before it looks at season/episode numbers; skadi only checked the
    /// numbers, so a file from another show whose SxxEyy happened to exist here
    /// was imported under it. A file whose name carries no title at all is
    /// accepted — the importer only reaches this matcher for a download this
    /// series requested, and manual imports of bare `S01E01.mkv` must still work.
    fn is_this_series(&self, p: &ParsedRelease) -> bool {
        let Some(title) = p.title.as_deref() else {
            return true;
        };
        let relevance =
            skadi_quality::title_relevance(std::slice::from_ref(&self.series.title), title);
        relevance.coverage >= MIN_TITLE_COVERAGE
    }

    /// Does the download's own top-level folder name this series
    /// (SKADI-T-0591)? The fallback for a file whose own name abbreviates the
    /// show. A folder with no parsable title does not vouch for anything.
    fn download_folder_is_this_series(&self, completed: &CompletedDownload) -> bool {
        let Some(folder) = skadi_importer::download_root_name(completed) else {
            return false;
        };
        let fp = parse_tv(&folder);
        fp.title.is_some() && self.is_this_series(&fp)
    }

    /// Which of this series' episodes does the parsed release satisfy?
    fn resolve<'a>(&'a self, p: &ParsedRelease) -> Vec<&'a Episode> {
        // Anime absolute numbering takes precedence when present.
        if !p.absolute.is_empty() {
            return self
                .episodes
                .iter()
                .filter(|e| e.absolute_number.is_some_and(|a| p.absolute.contains(&a)))
                .collect();
        }
        // Daily (date-keyed) when there's an air date and no SxxEyy.
        if p.episodes.is_empty()
            && let Some(date) = p
                .air_date
                .as_deref()
                .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        {
            return self
                .episodes
                .iter()
                .filter(|e| e.air_date == Some(date))
                .collect();
        }
        // Standard seasonal numbering (single or multi-episode).
        if let Some(season) = p.season
            && !p.episodes.is_empty()
        {
            let canonical: Vec<&Episode> = self
                .episodes
                .iter()
                .filter(|e| e.season == season && p.episodes.contains(&e.number))
                .collect();
            if !canonical.is_empty() {
                return canonical;
            }
            // Scene numbering fallback (SKADI-T-0273). Some scene groups number
            // by their own scheme rather than the metadata provider's — a
            // two-part premiere aired as one episode is numbered S01E01+S01E02 by
            // the scene and S01E01 canonically, and long-running shows drift by
            // a whole episode after a special is counted differently.
            //
            // Tried **only after** canonical numbering finds nothing, never
            // before: where both would match, the provider's numbering is the
            // one the library is organised by, and preferring scene numbers
            // would file an episode under the wrong name. The scene numbers are
            // populated per episode by the metadata sync, so a series with none
            // simply falls through to the empty result it produced before.
            return self
                .episodes
                .iter()
                .filter(|e| {
                    e.scene_season == Some(season)
                        && e.scene_episode.is_some_and(|n| p.episodes.contains(&n))
                })
                .collect();
        }
        // A bare season pack name with no episode number can't map to one episode;
        // the individual files inside the pack match on their own calls.
        Vec::new()
    }

    /// Canonical destination for placing `source` as one **explicit** `episode`
    /// of this series — used by library-import's manual mapper (SKADI-T-0324),
    /// where the operator picks the (season, episode) directly instead of the
    /// matcher resolving it from the filename. `resolution` feeds the quality
    /// token in the name when known.
    pub fn dest_for(&self, source: &Path, episode: &Episode, resolution: Option<&str>) -> PathBuf {
        let nums = [episode.number];
        let air = episode.air_date.map(|d| d.format("%Y-%m-%d").to_string());
        let naming = EpisodeNaming {
            series_title: &self.series.title,
            year: self.series.year,
            tmdb: self.series.external_ids.tmdb.as_ref().map(|t| t.0),
            season: episode.season,
            episodes: &nums,
            absolute: episode.absolute_number,
            air_date: air.as_deref(),
            episode_title: episode.title.as_deref(),
            quality: resolution,
            series_type: self.series.series_type,
        };
        self.naming
            .path(&self.series.root_folder.path, &naming, source)
    }

    fn dest(&self, source: &Path, p: &ParsedRelease, targets: &[&Episode]) -> PathBuf {
        let first = targets[0];
        let nums: Vec<u16> = targets.iter().map(|e| e.number).collect();
        let air = first.air_date.map(|d| d.format("%Y-%m-%d").to_string());
        let naming = EpisodeNaming {
            series_title: &self.series.title,
            year: self.series.year,
            tmdb: self.series.external_ids.tmdb.as_ref().map(|t| t.0),
            season: first.season,
            episodes: &nums,
            absolute: first.absolute_number,
            air_date: air.as_deref(),
            episode_title: first.title.as_deref(),
            quality: p.resolution.as_deref(),
            series_type: self.series.series_type,
        };
        self.naming
            .path(&self.series.root_folder.path, &naming, source)
    }
}

impl AcquirableMatcher for EpisodeMatcher {
    fn match_file(
        &self,
        _parsed: &ParsedRelease,
        source: &Path,
        completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        if skadi_importer::looks_like_sample(source) {
            return vec![];
        }
        // Only a video container can be an episode (SKADI-T-0591). A pack's
        // per-episode PNG screencaps (`s13e01 ….png`) parsed to a valid episode
        // code, were placed as the episodes, and superseded — deleted — the
        // real files already in the library.
        if !skadi_importer::is_video_file(source) {
            return vec![];
        }
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let p = parse_tv(name);
        // The file must actually belong to this series (SKADI-T-0445). Sonarr
        // checks the series title before importing; without this a mis-grabbed
        // `The.Wire.S01E01` was placed as Game of Thrones S01E01, because the
        // season/episode numbers matched an episode row and nothing else was
        // checked. Season/episode identity is `resolve`'s job; this is identity of
        // the *show*.
        //
        // A file that names the show by an abbreviation the title check cannot
        // score (`MST3K s13e01 ….mkv`) is judged by its download's own folder
        // instead (SKADI-T-0591): the release folder carries the full title, and
        // the download was already accepted for this series by the identity gate.
        if !self.is_this_series(&p) && !self.download_folder_is_this_series(completed) {
            return vec![];
        }
        let targets = self.resolve(&p);
        if targets.is_empty() {
            return vec![];
        }
        let dest = self.dest(source, &p, &targets);
        // A multi-episode file satisfies several acquirables at one path; Overwrite
        // so each placement (a hardlink to the same inode) records its episode.
        let multi = targets.len() > 1;
        targets
            .iter()
            .map(|ep| {
                let mut m = AcquirableMatch::new(ep.acquirable_ref(), dest.clone());
                if multi {
                    m = m.with_collision(CollisionPolicy::Overwrite);
                }
                if let Some(existing) = ep.file.as_ref() {
                    m = m.superseding(vec![existing.path.clone()]);
                }
                m
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::NaiveDate;
    use skadi_core::{ExternalIds, ProfileId, RootFolder, TmdbId, TvdbId};
    use skadi_downloaders::DownloadHandle;
    use skadi_importer::CompletedDownload;

    pub(super) fn series(kind: crate::SeriesType) -> Series {
        let mut s = Series::new(
            ExternalIds {
                tvdb: Some(TvdbId(121361)),
                tmdb: Some(TmdbId(1399)),
                ..Default::default()
            },
            "Game of Thrones",
            ProfileId::new(),
            RootFolder::new("/tv"),
        );
        s.year = Some(2011);
        s.series_type = kind;
        s
    }

    pub(super) fn ep(series: &Series, season: u16, number: u16) -> Episode {
        let mut e = Episode::missing(series.id, season, number);
        e.title = Some(format!("Episode {number}"));
        e
    }

    pub(super) fn completed() -> CompletedDownload {
        CompletedDownload {
            handle: DownloadHandle {
                native_id: "hash".into(),
                category: "tv".into(),
            },
            files: vec![],
            category: "tv".into(),
        }
    }

    fn match_one(m: &EpisodeMatcher, file: &str) -> Vec<AcquirableMatch> {
        let src = PathBuf::from(format!("/dl/{file}"));
        m.match_file(&ParsedRelease::default(), &src, &completed())
    }

    /// The MST3K season 13 pack (SKADI-T-0591): a per-episode screencap is not
    /// an episode, whatever its name parses to.
    #[test]
    fn non_video_files_are_never_episodes() {
        let s = series(crate::SeriesType::Standard);
        let e = ep(&s, 13, 1);
        let m = EpisodeMatcher::new(s, vec![e]);
        assert!(
            match_one(
                &m,
                "s13e01 Santo in the Treasure of Dracula top-original.png"
            )
            .is_empty()
        );
        assert!(match_one(&m, "Game.of.Thrones.S13E01.1080p.mka").is_empty());
        assert!(match_one(&m, "Game.of.Thrones.S13E01.1080p.srt").is_empty());
        assert_eq!(match_one(&m, "Game.of.Thrones.S13E01.1080p.mkv").len(), 1);
    }

    /// A file that abbreviates the show is vouched for by its download folder
    /// (SKADI-T-0591) — and not when the folder names something else.
    #[test]
    fn download_folder_vouches_for_an_abbreviated_file_name() {
        let s = series(crate::SeriesType::Standard);
        let e = ep(&s, 1, 5);
        let m = EpisodeMatcher::new(s, vec![e.clone()]);
        let file = "/dl/complete/Game of Thrones GoT S01 Pack/GoT s01e05 The Wolf and the Lion.mkv";
        let mut c = completed();
        c.files = vec![
            PathBuf::from(file),
            PathBuf::from("/dl/complete/Game of Thrones GoT S01 Pack/extras/GoT s01e05.nfo"),
        ];
        let got = m.match_file(&ParsedRelease::default(), Path::new(file), &c);
        assert_eq!(got.len(), 1, "the pack folder names the series");
        assert_eq!(got[0].acquirable, e.acquirable_ref());

        let other = "/dl/complete/The Wire S01 Pack/GoT s01e05.mkv";
        c.files = vec![PathBuf::from(other)];
        assert!(
            m.match_file(&ParsedRelease::default(), Path::new(other), &c)
                .is_empty(),
            "a folder naming another show does not vouch"
        );
    }

    #[test]
    fn single_episode_matches_and_names() {
        let s = series(crate::SeriesType::Standard);
        let e = ep(&s, 1, 5);
        let m = EpisodeMatcher::new(s.clone(), vec![e.clone()]);
        let got = match_one(&m, "Game.of.Thrones.S01E05.1080p.WEB-DL.x265-GRP.mkv");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].acquirable, e.acquirable_ref());
        let dest = got[0].dest.to_string_lossy();
        assert!(
            dest.contains("Season_01") && dest.contains("S01E05"),
            "{dest}"
        );
    }

    #[test]
    fn season_pack_matches_each_file_independently() {
        let s = series(crate::SeriesType::Standard);
        let eps: Vec<Episode> = (1..=3).map(|n| ep(&s, 1, n)).collect();
        let m = EpisodeMatcher::new(s.clone(), eps.clone());
        // Each file in the pack is a separate match_file call.
        for n in 1..=3u16 {
            let got = match_one(
                &m,
                &format!("Game.of.Thrones.S01E0{n}.1080p.BluRay.x264-GRP.mkv"),
            );
            assert_eq!(got.len(), 1, "file {n} matches one episode");
            assert_eq!(got[0].acquirable, eps[(n - 1) as usize].acquirable_ref());
        }
    }

    #[test]
    fn multi_episode_file_matches_both_with_overwrite() {
        let s = series(crate::SeriesType::Standard);
        let (e5, e6) = (ep(&s, 1, 5), ep(&s, 1, 6));
        let m = EpisodeMatcher::new(s.clone(), vec![e5.clone(), e6.clone()]);
        let got = match_one(&m, "Game.of.Thrones.S01E05E06.1080p.WEB.mkv");
        assert_eq!(got.len(), 2);
        assert!(
            got.iter()
                .all(|x| x.on_collision == CollisionPolicy::Overwrite)
        );
        let refs: Vec<_> = got.iter().map(|x| &x.acquirable).collect();
        assert!(refs.contains(&&e5.acquirable_ref()) && refs.contains(&&e6.acquirable_ref()));
        assert!(got[0].dest.to_string_lossy().contains("S01E05-E06"));
    }

    #[test]
    fn anime_matches_on_absolute_number() {
        let s = series(crate::SeriesType::Anime);
        let mut e = ep(&s, 1, 28);
        e.absolute_number = Some(28);
        let m = EpisodeMatcher::new(s.clone(), vec![e.clone()]);
        let got = match_one(&m, "[SubsPlease] Game of Thrones - 28 (1080p) [ABCD].mkv");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].acquirable, e.acquirable_ref());
    }

    #[test]
    fn daily_matches_on_air_date() {
        let s = series(crate::SeriesType::Daily);
        let mut e = ep(&s, 2024, 42);
        e.air_date = Some(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap());
        let m = EpisodeMatcher::new(s.clone(), vec![e.clone()]);
        let got = match_one(&m, "Game.of.Thrones.2024.03.01.1080p.WEB.h264-GRP.mkv");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].acquirable, e.acquirable_ref());
    }

    #[test]
    fn wrong_season_and_samples_do_not_match() {
        let s = series(crate::SeriesType::Standard);
        let e = ep(&s, 1, 5);
        let m = EpisodeMatcher::new(s.clone(), vec![e]);
        assert!(
            match_one(&m, "Game.of.Thrones.S02E05.1080p.mkv").is_empty(),
            "wrong season"
        );
        assert!(
            match_one(&m, "Game.of.Thrones.S01E05.sample.mkv").is_empty(),
            "sample rejected"
        );
    }
}

#[cfg(test)]
mod numbering_tests {
    use super::tests::{completed, ep, series};
    use super::*;

    fn match_one(m: &EpisodeMatcher, file: &str) -> Vec<AcquirableMatch> {
        let src = PathBuf::from(format!("/dl/{file}"));
        m.match_file(&ParsedRelease::default(), &src, &completed())
    }

    /// SKADI-T-0272: an absolute-numbered anime release resolves to the episode
    /// carrying that absolute number, not to whatever S01E28 would mean.
    #[test]
    fn an_absolute_numbered_release_resolves_by_absolute_number() {
        let s = series(crate::SeriesType::Anime);
        let mut e = ep(&s, 2, 4);
        e.absolute_number = Some(28);
        let m = EpisodeMatcher::new(s.clone(), vec![e.clone()]);

        let got = match_one(
            &m,
            "[SubsPlease] Game of Thrones - 28 (1080p) [ABCD1234].mkv",
        );
        assert_eq!(got.len(), 1, "matched by absolute number");
        assert_eq!(got[0].acquirable, e.acquirable_ref());
    }

    #[test]
    fn an_absolute_number_that_matches_nothing_matches_nothing() {
        // Absolute numbering takes precedence when present, so a release naming
        // an absolute the series does not have must not silently fall through to
        // seasonal numbering and match the wrong episode.
        let s = series(crate::SeriesType::Anime);
        let mut e = ep(&s, 1, 1);
        e.absolute_number = Some(1);
        let m = EpisodeMatcher::new(s, vec![e]);
        assert!(
            match_one(
                &m,
                "[SubsPlease] Game of Thrones - 99 (1080p) [ABCD1234].mkv"
            )
            .is_empty()
        );
    }

    /// SKADI-T-0273: scene numbering bridges to the canonical episode when
    /// canonical numbering finds nothing.
    #[test]
    fn a_scene_numbered_release_bridges_to_the_canonical_episode() {
        let s = series(crate::SeriesType::Standard);
        // Canonically S01E01, but the scene numbers it S01E02 — the shape a
        // two-part premiere counted as one episode produces.
        let mut e = ep(&s, 1, 1);
        e.scene_season = Some(1);
        e.scene_episode = Some(2);
        let m = EpisodeMatcher::new(s.clone(), vec![e.clone()]);

        let got = match_one(&m, "Game.of.Thrones.S01E02.1080p.WEB-DL.x264-GRP.mkv");
        assert_eq!(got.len(), 1, "S01E02 bridges to the canonical S01E01");
        assert_eq!(got[0].acquirable, e.acquirable_ref());
        // Named canonically, not by the scene number — the library is organised
        // by the provider's numbering.
        let dest = got[0].dest.to_string_lossy();
        assert!(dest.contains("S01E01"), "{dest}");
    }

    #[test]
    fn canonical_numbering_wins_when_both_would_match() {
        // The one that matters: if a release's numbers match one episode
        // canonically and a *different* one by scene, canonical takes it.
        // Preferring scene would file the episode under the wrong name.
        let s = series(crate::SeriesType::Standard);
        let mut canonical = ep(&s, 1, 2);
        canonical.scene_season = Some(1);
        canonical.scene_episode = Some(9);
        let mut scened = ep(&s, 1, 5);
        scened.scene_season = Some(1);
        scened.scene_episode = Some(2);
        let m = EpisodeMatcher::new(s, vec![canonical.clone(), scened]);

        let got = match_one(&m, "Game.of.Thrones.S01E02.1080p.WEB-DL.x264-GRP.mkv");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].acquirable,
            canonical.acquirable_ref(),
            "canonical S01E02 must win over the episode whose *scene* number is 2"
        );
    }

    #[test]
    fn a_series_with_no_scene_numbers_is_unaffected() {
        // The fallback must not change behaviour for the overwhelming majority
        // of series, which carry no scene numbering at all.
        let s = series(crate::SeriesType::Standard);
        let m = EpisodeMatcher::new(s.clone(), vec![ep(&s, 1, 1)]);
        assert!(match_one(&m, "Game.of.Thrones.S01E07.1080p.WEB-DL.x264-GRP.mkv").is_empty());
    }
}
