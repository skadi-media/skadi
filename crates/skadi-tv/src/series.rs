//! [`Series`] — the library entity for the television domain (SKADI-T-0265).
//!
//! Mirrors the movies `Movie` (a [`LibraryItem`]): user-controlled `monitored`
//! status + TMDB-sourced descriptive fields, owning in-memory `seasons` +
//! `episodes` (the [`Episode`] acquirables the hunter works) which the repo
//! populates on load. The DB stores series/seasons/episodes in three tables.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{ExternalIds, LibraryItem, MediaKind, ProfileId, RootFolder, SeriesId};

use crate::episode::{Episode, Season};

/// How a series is numbered/named — drives search + naming (anime uses absolute
/// numbering, daily uses air-date episodes).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeriesType {
    /// Standard SxxEyy seasonal numbering.
    #[default]
    Standard,
    /// Anime: absolute episode numbering + anime naming.
    Anime,
    /// Daily/talk shows: air-date-keyed episodes.
    Daily,
}

impl SeriesType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesType::Standard => "standard",
            SeriesType::Anime => "anime",
            SeriesType::Daily => "daily",
        }
    }

    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "anime" => SeriesType::Anime,
            "daily" => SeriesType::Daily,
            _ => SeriesType::Standard,
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Series {
    pub id: SeriesId,
    pub external_ids: ExternalIds,
    pub title: String,
    pub year: Option<u16>,
    pub overview: Option<String>,
    /// Airing status from metadata (e.g. `"Continuing"` / `"Ended"`).
    pub status: Option<String>,
    pub network: Option<String>,
    pub runtime_minutes: Option<u32>,
    pub series_type: SeriesType,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub backdrop_url: Option<String>,
    /// TMDB genre names, in TMDB's order (SKADI-T-0605). Empty until a
    /// metadata refresh has run since the column existed.
    #[serde(default)]
    pub genres: Vec<String>,
    /// US TV parental guideline from the metadata source (SKADI-T-0610);
    /// `None` until a refresh reports one. Household policies read this.
    #[serde(default)]
    pub content_rating: Option<String>,
    /// Library-axis status: do we want this series in the library?
    pub monitored: bool,
    pub profile: ProfileId,
    pub root_folder: RootFolder,
    pub added_at: DateTime<Utc>,
    pub last_metadata_refresh: Option<DateTime<Utc>>,
    /// Per-season rows. Populated by `TvRepo` on load; empty on construction.
    #[serde(default)]
    pub seasons: Vec<Season>,
    /// Per-episode acquirables. Populated by `TvRepo` on load; empty on construction.
    #[serde(default)]
    pub episodes: Vec<Episode>,
}

impl Series {
    /// A fresh `Series` with no seasons/episodes, to be enriched by metadata sync.
    #[must_use]
    pub fn new(
        external_ids: ExternalIds,
        title: impl Into<String>,
        profile: ProfileId,
        root_folder: RootFolder,
    ) -> Self {
        Self {
            id: SeriesId::new(),
            external_ids,
            title: title.into(),
            year: None,
            overview: None,
            status: None,
            network: None,
            runtime_minutes: None,
            series_type: SeriesType::Standard,
            poster_url: None,
            backdrop_url: None,
            genres: Vec::new(),
            content_rating: None,
            monitored: true,
            profile,
            root_folder,
            added_at: Utc::now(),
            last_metadata_refresh: None,
            seasons: Vec::new(),
            episodes: Vec::new(),
        }
    }
}

impl LibraryItem for Series {
    type Id = SeriesId;
    type Acquirable = Episode;

    fn id(&self) -> &Self::Id {
        &self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Series
    }
    fn monitored(&self) -> bool {
        self.monitored
    }
    fn quality_profile(&self) -> ProfileId {
        self.profile
    }
    fn root_folder(&self) -> &RootFolder {
        &self.root_folder
    }
    fn external_ids(&self) -> &ExternalIds {
        &self.external_ids
    }
    fn acquirables(&self) -> Box<dyn Iterator<Item = Self::Acquirable> + '_> {
        Box::new(self.episodes.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::{Acquirable, TmdbId};

    fn sample() -> Series {
        Series::new(
            ExternalIds {
                tmdb: Some(TmdbId(1399)),
                ..Default::default()
            },
            "Game of Thrones",
            ProfileId::new(),
            RootFolder::new("/tv"),
        )
    }

    #[test]
    fn series_serde_round_trips_with_episodes() {
        let mut s = sample();
        s.year = Some(2011);
        s.series_type = SeriesType::Standard;
        s.episodes.push(Episode::missing(s.id, 1, 1));
        let json = serde_json::to_string(&s).unwrap();
        let back: Series = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn library_item_exposes_episodes() {
        let mut s = sample();
        s.episodes.push(Episode::missing(s.id, 1, 1));
        s.episodes.push(Episode::missing(s.id, 1, 2));
        assert_eq!(s.title(), "Game of Thrones");
        assert_eq!(s.kind(), MediaKind::Series);
        let acq: Vec<_> = s.acquirables().collect();
        assert_eq!(acq.len(), 2);
        assert!(acq[0].wanted());
    }

    #[test]
    fn series_type_round_trips() {
        for t in [SeriesType::Standard, SeriesType::Anime, SeriesType::Daily] {
            assert_eq!(SeriesType::from_str_lossy(t.as_str()), t);
        }
        assert_eq!(SeriesType::from_str_lossy("garbage"), SeriesType::Standard);
    }
}
