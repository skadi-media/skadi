//! [`Movie`] — the library entity for the movies domain.
//!
//! Per SKADI-I-0007, `Movie` carries the user-controlled library status
//! (`monitored`) plus the TMDB-sourced descriptive fields, and owns an
//! in-memory `editions: Vec<MovieEdition>` so the [`LibraryItem`] trait's
//! `acquirables()` method can yield them. The DB stores movies + editions in
//! separate tables (T-0044); the repo populates `editions` when loading a
//! `Movie`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{ExternalIds, LibraryItem, MediaKind, MovieId, ProfileId, RootFolder};

use crate::edition::MovieEdition;

/// A TMDB collection: the franchise grouping for a film.
///
/// Carries the id as well as the name because names are not stable — TMDB
/// renames collections — and the id is what makes two films provably the same
/// franchise rather than merely similarly titled.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MovieCollection {
    pub tmdb_id: i64,
    pub name: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Movie {
    pub id: MovieId,
    pub external_ids: ExternalIds,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub overview: Option<String>,
    pub runtime_minutes: Option<u32>,
    /// Absolute poster image URL from metadata (provider-resolved), if any.
    #[serde(default)]
    pub poster_url: Option<String>,
    /// Absolute backdrop/fanart image URL from metadata, if any.
    #[serde(default)]
    pub backdrop_url: Option<String>,
    /// TMDB collection this film belongs to — the franchise, e.g.
    /// "The Taken Collection" (SKADI-T-0581). `None` means either not in one or
    /// not yet refreshed; the two are deliberately indistinguishable here,
    /// because a client that groups by collection treats both the same way.
    #[serde(default)]
    pub collection: Option<MovieCollection>,
    /// TMDB genre names, in TMDB's order (SKADI-T-0605). Empty until a
    /// metadata refresh has run since the column existed.
    #[serde(default)]
    pub genres: Vec<String>,
    /// US MPAA certification from the metadata source (SKADI-T-0610); `None`
    /// until a refresh reports one. Household policies read this.
    #[serde(default)]
    pub content_rating: Option<String>,
    /// Library-axis status: do we want this movie in the library?
    pub monitored: bool,
    pub profile: ProfileId,
    pub root_folder: RootFolder,
    pub added_at: DateTime<Utc>,
    pub last_metadata_refresh: Option<DateTime<Utc>>,
    /// Per-edition rows for this movie. Populated by `MoviesRepo` on load;
    /// empty on a freshly-constructed `Movie`.
    #[serde(default)]
    pub editions: Vec<MovieEdition>,
}

impl Movie {
    /// A fresh `Movie` with no editions, intended to be enriched by metadata
    /// sync and persisted by the repo.
    #[must_use]
    pub fn new(
        external_ids: ExternalIds,
        title: impl Into<String>,
        profile: ProfileId,
        root_folder: RootFolder,
    ) -> Self {
        Self {
            id: MovieId::new(),
            external_ids,
            title: title.into(),
            original_title: None,
            year: None,
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            genres: Vec::new(),
            content_rating: None,
            monitored: true,
            profile,
            root_folder,
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        }
    }
}

impl LibraryItem for Movie {
    type Id = MovieId;
    type Acquirable = MovieEdition;

    fn id(&self) -> &Self::Id {
        &self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Movie
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
        Box::new(self.editions.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edition::MovieEdition;
    use skadi_core::{Acquirable, AcquisitionStatus, EditionKindId, TmdbId};

    fn sample_movie() -> Movie {
        Movie::new(
            ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            "The Matrix",
            ProfileId::new(),
            RootFolder::new("/movies"),
        )
    }

    #[test]
    fn movie_serde_round_trips_with_editions() {
        let mut m = sample_movie();
        m.year = Some(1999);
        m.runtime_minutes = Some(136);
        m.editions
            .push(MovieEdition::missing(m.id, EditionKindId::new()));
        let json = serde_json::to_string(&m).unwrap();
        let back: Movie = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn library_item_impl_exposes_editions() {
        let mut m = sample_movie();
        let kind = EditionKindId::new();
        m.editions.push(MovieEdition::missing(m.id, kind));
        m.editions.push(MovieEdition {
            status: AcquisitionStatus::Cutoff,
            ..MovieEdition::missing(m.id, EditionKindId::new())
        });

        assert_eq!(m.title(), "The Matrix");
        assert_eq!(m.kind(), MediaKind::Movie);
        assert!(m.monitored());

        let acquirables: Vec<_> = m.acquirables().collect();
        assert_eq!(acquirables.len(), 2);
        assert!(acquirables[0].wanted());
        assert!(!acquirables[1].wanted(), "Cutoff edition is not wanted");
    }
}
