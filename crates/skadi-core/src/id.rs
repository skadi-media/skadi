//! Newtype identifiers for Skadi domain entities.
//!
//! Every identifier is a UUID-backed newtype. No raw [`Uuid`] (or `i64`) should
//! appear in public function signatures — use these types instead, so the
//! compiler prevents passing a `MovieId` where a `ReleaseId` is expected.
//!
//! UUIDs are chosen over autoincrementing integers for clean cross-instance
//! import/export, collision-free seeding from snapshots, and no information
//! leakage in URLs.

use std::fmt::Debug;
use std::hash::Hash;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Marker trait implemented by every Skadi identifier newtype.
///
/// Generic code that is parametric over "some identifier" can bound on this
/// trait rather than naming a concrete ID type.
pub trait ItemId: Copy + Eq + Hash + Debug + Send + Sync + 'static {}

/// Defines a UUID-backed newtype identifier implementing [`ItemId`].
///
/// Each generated type derives the standard value traits plus `Serialize` /
/// `Deserialize` (serializing transparently as the inner UUID), and gains
/// `new()`, `as_uuid()`, `into_uuid()`, `Default`, `From<Uuid>`, and `Display`.
macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        // JsonSchema alongside Serialize (SKADI-T-0548). These ids are
        // already part of the wire contract — the serde derives below put
        // them there — so describing them introduces no coupling that did
        // not already exist.
        #[derive(
            Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize,
            schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Create a new identifier backed by a random (v4) UUID.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Borrow the inner [`Uuid`].
            #[must_use]
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Consume the identifier and return the inner [`Uuid`].
            #[must_use]
            pub fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl ItemId for $name {}
    };
}

id_type!(
    /// Identifies a movie library item.
    MovieId
);
id_type!(
    /// Identifies a specific edition (cut) of a movie — the acquirable unit.
    MovieEditionId
);
id_type!(
    /// Identifies a movie edition *kind* (Theatrical, Extended, Director's Cut, …).
    /// The kind set is a runtime-configurable registry (`edition_kinds` table in
    /// `skadi-movies`) rather than a hard enum; see SKADI-I-0007.
    EditionKindId
);
id_type!(
    /// Identifies a TV series library item.
    SeriesId
);
id_type!(
    /// Identifies an episode — the acquirable unit for a series.
    EpisodeId
);
id_type!(
    /// Identifies a season (a grouping of episodes within a series).
    SeasonId
);
id_type!(
    /// Identifies a release located via an indexer.
    ReleaseId
);
id_type!(
    /// Identifies a configured indexer.
    IndexerId
);
id_type!(
    /// Identifies a configured downloader client.
    DownloaderId
);
id_type!(
    /// Identifies a quality profile.
    ProfileId
);
id_type!(
    /// Identifies a configured root folder.
    RootFolderId
);
id_type!(
    /// Identifies a quality definition (resolution + source + codec + modifier).
    QualityId
);
id_type!(
    /// Identifies a custom format scoring rule.
    CustomFormatId
);
id_type!(
    /// Identifies a configured notifier.
    NotifierId
);
id_type!(
    /// Identifies an audiobook author (SKADI-I-0017). Authors are an
    /// organizational entity above books, monitorable for new releases.
    AuthorId
);
id_type!(
    /// Identifies an audiobook series (e.g. "Stormlight Archive") — SKADI-I-0017.
    /// Distinct from [`SeriesId`] (a TV-series library item).
    BookSeriesId
);
id_type!(
    /// Identifies an audiobook (a Book) library item (SKADI-I-0017).
    BookId
);
id_type!(
    /// Identifies a book's audiobook file — the acquirable unit (SKADI-I-0017).
    BookFileId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn new_ids_are_unique() {
        assert_ne!(MovieId::new(), MovieId::new());
    }

    #[test]
    fn from_and_into_uuid_round_trip() {
        let raw = Uuid::new_v4();
        let id = MovieId::from(raw);
        assert_eq!(id.as_uuid(), &raw);
        assert_eq!(id.into_uuid(), raw);
    }

    #[test]
    fn equality_is_value_based() {
        let raw = Uuid::new_v4();
        assert_eq!(MovieId::from(raw), MovieId::from(raw));
    }

    #[test]
    fn usable_as_map_and_set_keys() {
        let id = ReleaseId::new();
        let mut set = HashSet::new();
        set.insert(id);
        assert!(set.contains(&id));

        let mut map = HashMap::new();
        map.insert(id, "release");
        assert_eq!(map.get(&id), Some(&"release"));
    }

    #[test]
    fn serializes_transparently_as_a_uuid_string() {
        let raw = Uuid::new_v4();
        let id = ProfileId::from(raw);
        let json = serde_json::to_string(&id).unwrap();
        // Transparent: identical to serializing the bare UUID.
        assert_eq!(json, serde_json::to_string(&raw).unwrap());
        assert_eq!(json, format!("\"{raw}\""));
    }

    #[test]
    fn json_round_trips() {
        let id = MovieEditionId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: MovieEditionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_matches_inner_uuid() {
        let raw = Uuid::new_v4();
        assert_eq!(IndexerId::from(raw).to_string(), raw.to_string());
    }
}
