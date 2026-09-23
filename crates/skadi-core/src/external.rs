//! External provider identifiers.
//!
//! [`ExternalIds`] is a first-class struct rather than a `HashMap<String, String>`
//! so the storage representation is uniform and per-provider lookups are typed.
//! It is JSON-serialized with absent providers omitted.
//!
//! Provider IDs are *external* identifiers (assigned by TMDB, IMDb, etc.), not
//! Skadi-internal UUIDs, so they do not use the [`id_type!`](crate::id) macro:
//! their inner representation matches the upstream provider (numeric or string).

use serde::{Deserialize, Serialize};

/// Defines a transparent newtype wrapper around an external provider ID.
macro_rules! provider_id {
    ($(#[$meta:meta])* $name:ident($inner:ty)) => {
        $(#[$meta])*
        #[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, schemars::JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub $inner);

        impl From<$inner> for $name {
            fn from(value: $inner) -> Self {
                Self(value)
            }
        }
    };
}

provider_id!(
    /// The Movie Database numeric ID.
    TmdbId(u64)
);
provider_id!(
    /// TheTVDB numeric ID.
    TvdbId(u64)
);
provider_id!(
    /// IMDb identifier, e.g. `tt0083658`.
    ImdbId(String)
);
provider_id!(
    /// MusicBrainz identifier (a UUID, kept as its canonical string form).
    MusicBrainzId(String)
);
provider_id!(
    /// Goodreads numeric ID.
    GoodreadsId(u64)
);
provider_id!(
    /// Audible ASIN — the primary key for audiobooks (e.g. `B08G9PRS1K`),
    /// kept as its string form (SKADI-I-0017).
    AsinId(String)
);

/// External identifiers a library item is known by across metadata providers.
///
/// Every field is optional and per-provider; absent providers are omitted from
/// the serialized JSON. Domain modules choose which providers are relevant
/// (movies are primarily TMDB; music is MusicBrainz; etc.).
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExternalIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<TmdbId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<TvdbId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<ImdbId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub musicbrainz: Option<MusicBrainzId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodreads: Option<GoodreadsId>,
    /// Audible ASIN (audiobooks, SKADI-I-0017).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asin: Option<AsinId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_round_trip() {
        let ids = ExternalIds {
            tmdb: Some(TmdbId(603)),
            tvdb: Some(TvdbId(1234)),
            imdb: Some(ImdbId("tt0133093".to_string())),
            musicbrainz: Some(MusicBrainzId(
                "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d".to_string(),
            )),
            goodreads: Some(GoodreadsId(42)),
            asin: Some(AsinId("B08G9PRS1K".to_string())),
        };
        let json = serde_json::to_string(&ids).unwrap();
        let back: ExternalIds = serde_json::from_str(&json).unwrap();
        assert_eq!(ids, back);
    }

    #[test]
    fn partial_round_trip_omits_absent_providers() {
        let ids = ExternalIds {
            tmdb: Some(TmdbId(603)),
            ..Default::default()
        };
        let json = serde_json::to_string(&ids).unwrap();
        // Only the populated provider is present in the JSON.
        assert_eq!(json, r#"{"tmdb":603}"#);

        let back: ExternalIds = serde_json::from_str(&json).unwrap();
        assert_eq!(ids, back);
        assert!(back.imdb.is_none());
    }

    #[test]
    fn empty_deserializes_from_object() {
        let ids: ExternalIds = serde_json::from_str("{}").unwrap();
        assert_eq!(ids, ExternalIds::default());
    }

    #[test]
    fn provider_ids_serialize_transparently() {
        assert_eq!(serde_json::to_string(&TmdbId(603)).unwrap(), "603");
        assert_eq!(
            serde_json::to_string(&ImdbId("tt0083658".to_string())).unwrap(),
            r#""tt0083658""#
        );
    }
}
