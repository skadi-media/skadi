//! The two-level domain abstraction every media domain implements.
//!
//! - [`LibraryItem`] — what a user actively tracks (a movie, series, artist,
//!   album, book). One row in the user's mental model.
//! - [`Acquirable`] — the atomic unit a `Release` can satisfy (a movie edition,
//!   an episode, a track). What a downloader hands to the importer.
//!
//! The two traits reference each other through associated types: a
//! `LibraryItem` names its `Acquirable`, and that `Acquirable` names its `Item`
//! back. Domains use these as generic bounds rather than as `dyn` trait objects
//! (they are not object-safe — `acquirables()` returns `Self::Acquirable`).

use crate::external::ExternalIds;
use crate::folder::RootFolder;
use crate::id::{ItemId, ProfileId};
use crate::status::{AcquisitionStatus, MediaKind};

/// A thing the user actively wants in their library.
pub trait LibraryItem: Send + Sync {
    /// This item's identifier type (e.g. `MovieId`).
    type Id: ItemId;
    /// The acquirable unit this item yields (e.g. `MovieEdition`).
    type Acquirable: Acquirable<Item = Self>;

    fn id(&self) -> &Self::Id;
    fn title(&self) -> &str;
    fn kind(&self) -> MediaKind;
    fn monitored(&self) -> bool;
    fn quality_profile(&self) -> ProfileId;
    fn root_folder(&self) -> &RootFolder;
    /// External provider IDs (TMDB, TVDB, IMDb, MusicBrainz, ...).
    fn external_ids(&self) -> &ExternalIds;

    /// Movies yield editions, series yield episodes, albums yield tracks.
    fn acquirables(&self) -> Box<dyn Iterator<Item = Self::Acquirable> + '_>;
}

/// The atomic unit a `Release` can satisfy.
pub trait Acquirable: Send + Sync {
    /// The library item this acquirable belongs to.
    type Item: LibraryItem;
    /// This acquirable's identifier type (e.g. `MovieEditionId`).
    type Id: ItemId;

    fn id(&self) -> &Self::Id;
    /// The id of the [`LibraryItem`] this acquirable belongs to.
    fn parent(&self) -> &<Self::Item as LibraryItem>::Id;
    fn status(&self) -> &AcquisitionStatus;
    fn wanted(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{MovieEditionId, MovieId};

    // A minimal pair of types proving the traits are implementable and that the
    // associated-type cross-references resolve.
    struct Movie {
        id: MovieId,
        title: String,
        profile: ProfileId,
        root: RootFolder,
        external: ExternalIds,
        editions: Vec<Edition>,
    }

    #[derive(Clone)]
    struct Edition {
        id: MovieEditionId,
        parent: MovieId,
        status: AcquisitionStatus,
        wanted: bool,
    }

    impl LibraryItem for Movie {
        type Id = MovieId;
        type Acquirable = Edition;

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
            true
        }
        fn quality_profile(&self) -> ProfileId {
            self.profile
        }
        fn root_folder(&self) -> &RootFolder {
            &self.root
        }
        fn external_ids(&self) -> &ExternalIds {
            &self.external
        }
        fn acquirables(&self) -> Box<dyn Iterator<Item = Self::Acquirable> + '_> {
            Box::new(self.editions.iter().cloned())
        }
    }

    impl Acquirable for Edition {
        type Item = Movie;
        type Id = MovieEditionId;

        fn id(&self) -> &Self::Id {
            &self.id
        }
        fn parent(&self) -> &MovieId {
            &self.parent
        }
        fn status(&self) -> &AcquisitionStatus {
            &self.status
        }
        fn wanted(&self) -> bool {
            self.wanted
        }
    }

    #[test]
    fn traits_are_implementable_and_acquirables_iterate() {
        let movie_id = MovieId::new();
        let movie = Movie {
            id: movie_id,
            title: "Blade Runner".to_string(),
            profile: ProfileId::new(),
            root: RootFolder::new("/movies"),
            external: ExternalIds {
                tmdb: Some(crate::TmdbId(78)),
                ..Default::default()
            },
            editions: vec![
                Edition {
                    id: MovieEditionId::new(),
                    parent: movie_id,
                    status: AcquisitionStatus::Missing,
                    wanted: true,
                },
                Edition {
                    id: MovieEditionId::new(),
                    parent: movie_id,
                    status: AcquisitionStatus::Cutoff,
                    wanted: false,
                },
            ],
        };

        assert_eq!(movie.title(), "Blade Runner");
        assert_eq!(movie.kind(), MediaKind::Movie);

        let acquirables: Vec<_> = movie.acquirables().collect();
        assert_eq!(acquirables.len(), 2);
        assert_eq!(acquirables[0].parent(), movie.id());
        assert!(acquirables[0].wanted());
        assert!(matches!(acquirables[1].status(), AcquisitionStatus::Cutoff));
    }
}
