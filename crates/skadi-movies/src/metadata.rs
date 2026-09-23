//! TMDB-backed metadata sync (SKADI-T-0045).
//!
//! Two operations the rest of `skadi-movies` (and the future API) need:
//! - [`refresh_movie`] — (re)populate a `Movie`'s TMDB-sourced fields from a
//!   `MetadataProvider::lookup`. Preserves user-controlled fields (`id`,
//!   `monitored`, `profile`, `root_folder`) from `existing` when provided.
//! - [`add_movie`] — orchestrate "fetch + create movie row + create default
//!   Theatrical edition Missing" for a new TMDB id.
//!
//! Implemented over the existing [`MetadataProvider`] trait so the source is
//! swappable; the daemon supplies `TmdbProvider` (SKADI-T-0027) at runtime.
//!
//! Poster/backdrop image URLs from `MetadataRecord.images` are persisted on the
//! `Movie` (`poster_url`/`backdrop_url`) as of SKADI-T-0073; providers resolve
//! them to absolute URLs (Servarr returns full URLs; TMDB paths are resolved to
//! the image CDN), so the UI can render them directly.

use chrono::Utc;

use skadi_core::{
    AppError, EditionKindId, ExternalIds, ImdbId, ProfileId, Result, RootFolder, TmdbId,
};
use skadi_metadata::{ExternalId, ImageKind, MetadataProvider, MetadataRecord};

use crate::THEATRICAL_KIND_ID;
use crate::edition::MovieEdition;
use crate::movie::Movie;
use crate::repo::MoviesRepo;

/// Fetch the TMDB record for `tmdb` and shape it into a [`Movie`].
///
/// When `existing` is `Some`, preserves its `id`, `monitored`, `profile`,
/// `root_folder`, `added_at`, and (existing) `editions` — only the
/// TMDB-sourced fields are rewritten. When `None`, a fresh `Movie::new(...)`
/// is built from `provider_defaults`.
pub async fn refresh_movie(
    provider: &dyn MetadataProvider,
    tmdb: TmdbId,
    existing: Option<Movie>,
    provider_defaults: Option<MovieDefaults>,
) -> Result<Movie> {
    let record = provider.lookup(&ExternalId::Tmdb(tmdb.clone())).await?;

    let mut movie = match existing {
        Some(m) => m,
        None => {
            let defaults = provider_defaults.ok_or_else(|| {
                AppError::Validation(
                    "refresh_movie: provider_defaults required when existing is None".into(),
                )
            })?;
            Movie::new(
                ExternalIds {
                    tmdb: Some(tmdb.clone()),
                    ..Default::default()
                },
                String::new(), // filled below
                defaults.profile,
                defaults.root_folder,
            )
        }
    };

    apply_record(&mut movie, &record, tmdb);
    Ok(movie)
}

/// Defaults used when constructing a fresh `Movie` (see [`refresh_movie`]).
#[derive(Clone, Debug)]
pub struct MovieDefaults {
    pub profile: ProfileId,
    pub root_folder: RootFolder,
}

fn apply_record(movie: &mut Movie, record: &MetadataRecord, tmdb: TmdbId) {
    // Always (re-)assert tmdb on the movie; preserve any other ids the user or
    // a previous refresh attached.
    movie.external_ids.tmdb = Some(tmdb);
    // Inherit imdb only when the record carries it and we don't already have
    // one (user-set imdb wins; future task could reconcile actively).
    if movie.external_ids.imdb.is_none()
        && let Some(imdb) = record.external_ids.imdb.as_ref()
    {
        movie.external_ids.imdb = Some(ImdbId(imdb.0.clone()));
    }
    movie.title = record.title.clone();
    movie.original_title = record.original_title.clone();
    movie.year = record
        .release_date
        .map(|d| u16::try_from(d.format("%Y").to_string().parse::<i32>().unwrap_or(0)).ok())
        .unwrap_or(None);
    movie.overview = record.overview.clone();
    movie.runtime_minutes = record.runtime_minutes;
    // Same rule as the collection below: a lookup that reports no genres must
    // not erase the ones a previous refresh stored.
    if !record.genres.is_empty() {
        movie.genres = record.genres.clone();
    }
    if record.content_rating.is_some() {
        movie.content_rating = record.content_rating.clone();
    }
    // Only overwrite when the record carries an id, for the same reason as the
    // images below: a lookup that omits it must not wipe what we already know.
    // A film genuinely leaving a collection is rare enough to wait for the next
    // refresh that does report one.
    if let Some(id) = record.collection_id {
        movie.collection = Some(crate::MovieCollection {
            tmdb_id: id,
            name: record.collection_name.clone().unwrap_or_default(),
        });
    }
    // Poster/backdrop from the metadata record's images (provider-resolved to
    // absolute URLs). Only overwrite when the record actually carries one, so a
    // lookup that omits images doesn't wipe an existing poster.
    if let Some(p) = record.images.iter().find(|i| i.kind == ImageKind::Poster) {
        movie.poster_url = Some(p.path.clone());
    }
    if let Some(b) = record.images.iter().find(|i| i.kind == ImageKind::Backdrop) {
        movie.backdrop_url = Some(b.path.clone());
    }
    movie.last_metadata_refresh = Some(Utc::now());
}

/// Add a brand-new movie by TMDB id: refuses duplicates, runs metadata sync,
/// writes the `Movie` row, and creates one default `Theatrical` `Missing`
/// edition. Returns the persisted `Movie` (with `editions` populated).
pub async fn add_movie(
    repo: &dyn MoviesRepo,
    provider: &dyn MetadataProvider,
    tmdb: TmdbId,
    profile: ProfileId,
    root_folder: RootFolder,
) -> Result<Movie> {
    if let Some(existing) = repo.get_movie_by_tmdb(tmdb.clone()).await? {
        return Err(AppError::Validation(format!(
            "movie with TMDB id {} already exists ({})",
            tmdb.0, existing.title
        )));
    }
    let mut movie = refresh_movie(
        provider,
        tmdb,
        None,
        Some(MovieDefaults {
            profile,
            root_folder,
        }),
    )
    .await?;
    repo.upsert_movie(&movie).await?;

    // Default Theatrical edition. The Theatrical kind id is deterministic
    // (seeded by migrations); fall back to a registry lookup by tag if the
    // hardcoded id ever changes.
    let kind_id = EditionKindId::from(THEATRICAL_KIND_ID);
    if repo.get_edition_kind(kind_id).await?.is_none() {
        let by_tag = repo
            .get_edition_kind_by_tag("Theatrical")
            .await?
            .ok_or_else(|| {
                AppError::Internal(
                    "Theatrical edition kind missing from registry (migrations not seeded?)".into(),
                )
            })?;
        let edition = MovieEdition::missing(movie.id, by_tag.id);
        repo.upsert_edition(&edition).await?;
        movie.editions.push(edition);
    } else {
        let edition = MovieEdition::missing(movie.id, kind_id);
        repo.upsert_edition(&edition).await?;
        movie.editions.push(edition);
    }

    Ok(movie)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::NaiveDate;
    use skadi_core::{ImdbId, MediaKind};
    use skadi_metadata::{MetadataMatch, MetadataQuery};

    /// A scripted metadata provider whose `lookup` returns a canned record.
    struct FakeProvider {
        record: MetadataRecord,
    }

    #[async_trait]
    impl MetadataProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn supports(&self, kind: MediaKind) -> bool {
            kind == MediaKind::Movie
        }
        async fn search(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
            Ok(vec![])
        }
        async fn lookup(&self, _id: &ExternalId) -> Result<MetadataRecord> {
            Ok(self.record.clone())
        }
    }

    fn provider_with(title: &str, year: i32, runtime: u32, imdb: Option<&str>) -> FakeProvider {
        FakeProvider {
            record: MetadataRecord {
                external_ids: ExternalIds {
                    imdb: imdb.map(|i| ImdbId(i.into())),
                    ..Default::default()
                },
                title: title.into(),
                original_title: Some(title.into()),
                overview: Some("A movie.".into()),
                runtime_minutes: Some(runtime),
                release_date: NaiveDate::from_ymd_opt(year, 3, 31),
                images: vec![],
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn refresh_movie_maps_record_fields_onto_existing_movie() {
        let mut existing = Movie::new(
            ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            "stale title",
            ProfileId::new(),
            RootFolder::new("/movies"),
        );
        existing.monitored = false; // user-set, must survive refresh
        let original_profile = existing.profile;

        let provider = provider_with("The Matrix", 1999, 136, Some("tt0133093"));
        let refreshed = refresh_movie(&provider, TmdbId(603), Some(existing.clone()), None)
            .await
            .unwrap();

        assert_eq!(refreshed.title, "The Matrix");
        assert_eq!(refreshed.year, Some(1999));
        assert_eq!(refreshed.runtime_minutes, Some(136));
        assert_eq!(refreshed.external_ids.imdb.as_ref().unwrap().0, "tt0133093");
        // User-set fields preserved.
        assert_eq!(refreshed.id, existing.id);
        assert!(!refreshed.monitored);
        assert_eq!(refreshed.profile, original_profile);
        // Stamped refresh time.
        assert!(refreshed.last_metadata_refresh.is_some());
    }

    #[tokio::test]
    async fn refresh_maps_poster_and_backdrop_from_record_images() {
        use skadi_metadata::{ImageKind, ImageRef};
        let mut provider = provider_with("The Matrix", 1999, 136, None);
        provider.record.images = vec![
            ImageRef {
                kind: ImageKind::Poster,
                path: "https://img/p.jpg".into(),
            },
            ImageRef {
                kind: ImageKind::Backdrop,
                path: "https://img/b.jpg".into(),
            },
        ];
        let defaults = MovieDefaults {
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
        };
        let movie = refresh_movie(&provider, TmdbId(603), None, Some(defaults))
            .await
            .unwrap();
        assert_eq!(movie.poster_url.as_deref(), Some("https://img/p.jpg"));
        assert_eq!(movie.backdrop_url.as_deref(), Some("https://img/b.jpg"));
    }

    #[tokio::test]
    async fn refresh_movie_without_existing_needs_defaults_to_build_a_new_one() {
        let provider = provider_with("The Matrix", 1999, 136, None);
        let err = refresh_movie(&provider, TmdbId(603), None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));

        let defaults = MovieDefaults {
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
        };
        let fresh = refresh_movie(&provider, TmdbId(603), None, Some(defaults))
            .await
            .unwrap();
        assert_eq!(fresh.title, "The Matrix");
        assert_eq!(fresh.year, Some(1999));
    }
}
