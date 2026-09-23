//! [`Season`] (grouping) + [`Episode`] (the [`Acquirable`] unit) for the TV
//! domain (SKADI-T-0265). Episode replaces movies' `MovieEdition`: one acquirable
//! per `(series, season, episode)`, carrying scene/absolute numbering for the
//! matcher (T-0270/0272/0273) and a per-episode `monitored` flag (T-0271).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{
    Acquirable, AcquisitionStatus, EpisodeId, FileRef, QualityId, SeasonId, SeriesId,
};
use skadi_importer::AcquirableRef;

/// One season of a [`crate::series::Series`] — a grouping with its own monitor
/// flag + aired/total counts (season 0 = specials).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Season {
    pub id: SeasonId,
    pub series_id: SeriesId,
    /// Season number; `0` = specials.
    pub number: u16,
    pub monitored: bool,
    /// Total episodes the metadata lists for this season.
    pub episode_count: u16,
    /// Episodes already aired (air_date <= today).
    pub aired_count: u16,
}

impl Season {
    #[must_use]
    pub fn new(series_id: SeriesId, number: u16) -> Self {
        Self {
            id: SeasonId::new(),
            series_id,
            number,
            monitored: true,
            episode_count: 0,
            aired_count: 0,
        }
    }
}

/// One episode — the unit the hunter acquires. `(series_id, season, number)` is
/// unique per series.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Episode {
    pub id: EpisodeId,
    pub series_id: SeriesId,
    pub season: u16,
    pub number: u16,
    /// Absolute episode number (anime); `None` for standard series.
    pub absolute_number: Option<u32>,
    /// Scene numbering for trackers that index by it (T-0273).
    pub scene_season: Option<u16>,
    pub scene_episode: Option<u16>,
    pub title: Option<String>,
    pub air_date: Option<NaiveDate>,
    /// Per-episode monitor flag (T-0271).
    pub monitored: bool,
    pub status: AcquisitionStatus,
    pub file: Option<FileRef>,
    pub quality: Option<QualityId>,
    pub format_score: i32,
    pub updated_at: DateTime<Utc>,
    /// Probed media-info of the imported file (SKADI-T-0451); `None` until the
    /// post-import probe runs. Movies and audiobooks already persisted this —
    /// TV had no column, so the probe's result was computed and dropped.
    #[serde(default)]
    pub media_info: Option<skadi_core::MediaInfo>,
}

impl Episode {
    /// A fresh `Missing`, monitored episode.
    #[must_use]
    pub fn missing(series_id: SeriesId, season: u16, number: u16) -> Self {
        Self {
            id: EpisodeId::new(),
            series_id,
            season,
            number,
            absolute_number: None,
            scene_season: None,
            scene_episode: None,
            title: None,
            air_date: None,
            monitored: true,
            status: AcquisitionStatus::Missing,
            file: None,
            quality: None,
            format_score: 0,
            updated_at: Utc::now(),
            media_info: None,
        }
    }

    /// The opaque [`AcquirableRef`] the hunter carries through a workflow run —
    /// the episode id's canonical UUID string (decoded back by the status sink).
    #[must_use]
    pub fn acquirable_ref(&self) -> AcquirableRef {
        AcquirableRef(self.id.to_string())
    }

    /// Has this episode already aired (so it's worth searching)?
    ///
    /// **An episode with no air date has not aired** (SKADI-T-0446). This used to
    /// return `true`, so every undated episode was searched on every sweep. Those
    /// are exactly the rows least likely to have a real release: a placeholder the
    /// metadata provider has not dated yet, or a season stub. Each one cost a
    /// round of indexer queries forever, and any "match" for an episode that does
    /// not exist yet is by definition wrong.
    ///
    /// Sonarr does the same and offers an explicit opt-in for the cases where an
    /// undated episode really is out — see [`has_aired_or_undated`].
    #[must_use]
    pub fn has_aired(&self, today: NaiveDate) -> bool {
        self.air_date.is_some_and(|d| d <= today)
    }

    /// [`has_aired`](Self::has_aired), but an undated episode counts as aired —
    /// Sonarr's "search for undated episodes" (SKADI-T-0446). Driven by
    /// `tv.search_undated_episodes`, off by default.
    #[must_use]
    pub fn has_aired_or_undated(&self, today: NaiveDate, include_undated: bool) -> bool {
        match self.air_date {
            Some(d) => d <= today,
            None => include_undated,
        }
    }
}

impl Acquirable for Episode {
    type Item = crate::series::Series;
    type Id = EpisodeId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
    fn parent(&self) -> &SeriesId {
        &self.series_id
    }
    fn status(&self) -> &AcquisitionStatus {
        &self.status
    }

    /// Wanted = monitored AND still needing a (first) release. The series-level
    /// `monitored` + below-cutoff upgrade logic combine in `SeriesWantedQuery`.
    fn wanted(&self) -> bool {
        self.monitored
            && matches!(
                self.status,
                AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep() -> Episode {
        Episode::missing(SeriesId::new(), 1, 5)
    }

    #[test]
    fn episode_serde_round_trips() {
        let mut e = ep();
        e.absolute_number = Some(42);
        e.title = Some("Pilot".into());
        e.air_date = Some(NaiveDate::from_ymd_opt(2011, 4, 17).unwrap());
        let back: Episode = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn season_serde_round_trips() {
        let s = Season::new(SeriesId::new(), 2);
        let back: Season = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn monitored_missing_is_wanted_unmonitored_is_not() {
        let mut e = ep();
        assert!(e.wanted());
        e.monitored = false;
        assert!(!e.wanted(), "unmonitored episode is never wanted");
        e.monitored = true;
        e.status = AcquisitionStatus::Cutoff;
        assert!(!e.wanted(), "Cutoff is not wanted");
    }

    #[test]
    fn acquirable_ref_is_episode_id_string() {
        let e = ep();
        assert_eq!(e.acquirable_ref().0, e.id.to_string());
    }

    #[test]
    fn has_aired_respects_air_date() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let mut e = ep();
        // SKADI-T-0446: this asserted the opposite — an undated episode was
        // treated as aired, so it was searched on every sweep forever.
        assert!(!e.has_aired(today), "no air date => has not aired");
        e.air_date = Some(NaiveDate::from_ymd_opt(2030, 1, 1).unwrap());
        assert!(!e.has_aired(today), "future episode hasn't aired");
        e.air_date = Some(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        assert!(e.has_aired(today), "past episode has aired");
    }

    #[test]
    fn undated_episodes_are_searched_only_when_opted_in() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let mut e = ep();
        assert!(!e.has_aired_or_undated(today, false));
        assert!(
            e.has_aired_or_undated(today, true),
            "Sonarr's search-unaired"
        );
        // The opt-in covers *undated*, not *future-dated* — an episode with a real
        // air date still in the future is never searched either way.
        e.air_date = Some(NaiveDate::from_ymd_opt(2030, 1, 1).unwrap());
        assert!(!e.has_aired_or_undated(today, true));
    }
}
