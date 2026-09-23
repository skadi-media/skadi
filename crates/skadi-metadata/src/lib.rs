//! `skadi-metadata` — the metadata-provider abstraction.
//!
//! Defines the [`MetadataProvider`] trait and the provider-neutral value types
//! ([`MetadataQuery`], [`MetadataMatch`], [`MetadataRecord`]). Concrete
//! providers (TMDB, …) implement the trait against this surface and map their
//! own payloads onto the neutral types, so domains never see provider-specific
//! shapes.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use skadi_core::{AsinId, ExternalIds, ImdbId, MediaKind, MusicBrainzId, Result, TmdbId, TvdbId};

pub mod arr_list;
pub mod audible_catalog;
pub mod audnexus;
pub mod import_list;
pub mod servarr;
pub mod skyhook;
pub mod tmdb;
pub use audible_catalog::{AudibleCatalogProvider, CatalogItem};
pub use audnexus::{AudnexusAuthor, AudnexusProvider, AuthorMatch};
pub use servarr::ServarrProvider;
pub use skyhook::SkyhookProvider;
pub use tmdb::TmdbProvider;

/// A single external id used as a lookup key for `lookup`/`refresh`.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum ExternalId {
    Tmdb(TmdbId),
    Imdb(ImdbId),
    Tvdb(TvdbId),
    MusicBrainz(MusicBrainzId),
    /// Audible ASIN — the audiobook lookup key (SKADI-I-0017).
    Asin(AsinId),
}

/// A free-text metadata search.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct MetadataQuery {
    pub title: String,
    pub year: Option<u16>,
    pub kind: MediaKind,
}

/// A candidate result from [`MetadataProvider::search`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MetadataMatch {
    pub external_ids: ExternalIds,
    pub title: String,
    pub year: Option<u16>,
    /// Provider relevance/popularity, higher is better.
    pub score: f32,
    /// Poster/cover URL for the result, when the search response carries one — so
    /// an add-search list can show artwork without a second per-item lookup.
    #[serde(default)]
    pub poster_url: Option<String>,
    /// Short synopsis/overview, when the search response carries one.
    #[serde(default)]
    pub overview: Option<String>,
}

/// The role of a referenced image.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum ImageKind {
    Poster,
    Backdrop,
}

/// A reference to a remote image (provider path/URL only — no download here).
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct ImageRef {
    pub kind: ImageKind,
    pub path: String,
}

/// The provider-neutral metadata for an item.
///
/// The trailing fields (`subtitle`/`authors`/`narrators`/`series`/
/// `series_position`/`abridged`) are **audiobook**-shaped (SKADI-I-0017); they
/// default to empty for video providers, so adding them is backward-compatible.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct MetadataRecord {
    pub external_ids: ExternalIds,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub runtime_minutes: Option<u32>,
    pub release_date: Option<NaiveDate>,
    pub images: Vec<ImageRef>,
    /// Subtitle (audiobooks often carry one), when present.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Author/creator names (audiobooks).
    #[serde(default)]
    pub authors: Vec<String>,
    /// Narrator names (audiobooks).
    #[serde(default)]
    pub narrators: Vec<String>,
    /// Series name (audiobooks), when the item belongs to one.
    #[serde(default)]
    pub series: Option<String>,
    /// Position within the series (string preserves decimal/part numbering).
    #[serde(default)]
    pub series_position: Option<String>,
    /// Abridgement: `Some(true)` = abridged, `Some(false)` = unabridged,
    /// `None` = unknown.
    #[serde(default)]
    pub abridged: Option<bool>,
    /// Collection/franchise id from the provider (movies; TMDB calls this
    /// `belongs_to_collection`) — SKADI-T-0581.
    ///
    /// Flat rather than a struct, and separate from `series` above, because the
    /// id is the part that makes two films provably the same franchise. `series`
    /// carries only a name, which is enough for audiobooks and not for this.
    #[serde(default)]
    pub collection_id: Option<i64>,
    /// Human-readable collection name, e.g. "The Taken Collection".
    #[serde(default)]
    pub collection_name: Option<String>,
    /// Genre names as the source reports them ("Science Fiction", "Drama"),
    /// in source order (SKADI-T-0605). Empty when the provider has none.
    #[serde(default)]
    pub genres: Vec<String>,
    /// Content rating as the source labels it — US MPAA for films ("PG-13"),
    /// US TV parental guidelines for series ("TV-14") (SKADI-T-0610).
    /// `None` when the source has none; never invented.
    #[serde(default)]
    pub content_rating: Option<String>,
}

/// Provider-neutral metadata for a **TV series** + its seasons + episodes
/// (SKADI-I-0037 / T-0266). Returned by a provider's TV lookup; the TV domain
/// maps it onto its `Series`/`Season`/`Episode` model.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SeriesMetadata {
    /// The series-level record (title/overview/ids/images; `release_date` is the
    /// first-air date).
    pub record: MetadataRecord,
    /// Airing status (e.g. `"Returning Series"` / `"Ended"`).
    pub status: Option<String>,
    pub network: Option<String>,
    /// Heuristic: Animation genre + a Japanese origin → likely anime (T-0272
    /// refines this).
    pub is_anime: bool,
    pub seasons: Vec<SeasonMeta>,
    pub episodes: Vec<EpisodeMeta>,
}

/// One season in a [`SeriesMetadata`].
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SeasonMeta {
    /// Season number; `0` = specials.
    pub number: u16,
    pub name: Option<String>,
    pub episode_count: u16,
    pub air_date: Option<NaiveDate>,
}

/// One episode in a [`SeriesMetadata`].
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct EpisodeMeta {
    pub season: u16,
    pub number: u16,
    /// Absolute episode number (anime), when the source provides it.
    pub absolute: Option<u32>,
    pub title: Option<String>,
    pub air_date: Option<NaiveDate>,
    pub overview: Option<String>,
}

/// A source of metadata (search/lookup/refresh). Object-safe so the daemon can
/// hold `Vec<Box<dyn MetadataProvider>>`.
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    fn name(&self) -> &str;
    fn supports(&self, kind: MediaKind) -> bool;
    async fn search(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>>;
    async fn lookup(&self, id: &ExternalId) -> Result<MetadataRecord>;
    /// Re-fetch the record for `id` (default: same as `lookup`; providers may
    /// override to bypass caches).
    async fn refresh(&self, id: &ExternalId) -> Result<MetadataRecord> {
        self.lookup(id).await
    }
}

/// A provider of full **TV series** metadata (series + seasons + episodes), keyed
/// by the TVDB id — TV is TheTVDB-native (Sonarr/Skyhook). The keyless
/// [`SkyhookProvider`](crate::SkyhookProvider) is the default implementation.
#[async_trait]
pub trait SeriesMetadataProvider: Send + Sync {
    async fn search_series(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>>;
    async fn lookup_series(&self, tvdb: TvdbId) -> Result<SeriesMetadata>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips() {
        let rec = MetadataRecord {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                imdb: Some(ImdbId("tt0133093".into())),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: Some("The Matrix".into()),
            overview: Some("A hacker learns the truth.".into()),
            runtime_minutes: Some(136),
            release_date: NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![ImageRef {
                kind: ImageKind::Poster,
                path: "/poster.jpg".into(),
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: MetadataRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    // Fake provider proving the trait is implementable and object-safe.
    struct FakeProvider;

    #[async_trait]
    impl MetadataProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn supports(&self, kind: MediaKind) -> bool {
            matches!(kind, MediaKind::Movie)
        }
        async fn search(&self, query: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
            Ok(vec![MetadataMatch {
                external_ids: ExternalIds {
                    tmdb: Some(TmdbId(1)),
                    ..Default::default()
                },
                title: query.title.clone(),
                year: query.year,
                score: 1.0,
                poster_url: None,
                overview: None,
            }])
        }
        async fn lookup(&self, _id: &ExternalId) -> Result<MetadataRecord> {
            Ok(MetadataRecord {
                title: "looked up".into(),
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn provider_is_object_safe_and_refresh_defaults_to_lookup() {
        let p: Box<dyn MetadataProvider> = Box::new(FakeProvider);
        assert!(p.supports(MediaKind::Movie));
        assert!(!p.supports(MediaKind::Music));
        let matches = p
            .search(&MetadataQuery {
                title: "Heat".into(),
                year: Some(1995),
                kind: MediaKind::Movie,
            })
            .await
            .unwrap();
        assert_eq!(matches[0].title, "Heat");
        let rec = p.refresh(&ExternalId::Tmdb(TmdbId(1))).await.unwrap();
        assert_eq!(rec.title, "looked up");
    }
}
