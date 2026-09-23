//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use skadi_core::QualityId;
use skadi_core::{ExternalIds, MovieId, ProfileId, RootFolder, TmdbId};
use skadi_hunter::AcquireSeed;
use skadi_importer::AcquirableMatch;
use skadi_movies::{EditionKind, Movie, MovieEdition, MovieMatcher, MoviesRepo};
use skadi_quality::{QualityProfile, default_definitions};
use skadi_store::Store;
use skadi_testsupport::TestDb;

/// `TestDb` has no `Debug`; the world needs one.
pub struct Db(pub TestDb);
impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Db({})", self.0.backend_name())
    }
}

/// `MovieMatcher` has no `Debug` either.
pub struct Matcher(pub MovieMatcher);
impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MovieMatcher")
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub db: Option<Db>,
    pub profile: Option<QualityProfile>,
    pub upgrade_until_format_score: i32,
    /// Movies by title.
    pub movies: HashMap<String, Movie>,
    /// Theatrical (or first) edition per movie title.
    pub editions: HashMap<String, MovieEdition>,
    pub seeds: Vec<AcquireSeed>,
    pub recovered: usize,
    pub tmp: Option<tempfile::TempDir>,
    /// Library files created for a movie title (for demotion scenarios).
    pub files: HashMap<String, std::path::PathBuf>,
    pub matcher: Option<Matcher>,
    pub matches: Vec<AcquirableMatch>,
    /// The last error message captured by a `When` step, if any.
    pub error: Option<String>,
    pub kinds: Vec<EditionKind>,
    pub last_movie: Option<Movie>,
    pub listed: Vec<Movie>,
    pub library_items: Vec<skadi_api::LibraryItemDto>,
    pub history: Vec<skadi_store::HistoryEntry>,
    pub read_status: Option<Option<skadi_core::AcquisitionStatus>>,
    pub provider: Option<steps::metadata::FakeProvider>,
}

static TMDB_SEQ: AtomicU64 = AtomicU64::new(10_000);

impl World {
    pub fn store(&self) -> Store {
        self.db
            .as_ref()
            .expect("library not set up")
            .0
            .store
            .clone()
    }

    pub fn repo(&self) -> std::sync::Arc<dyn MoviesRepo> {
        std::sync::Arc::new(self.store())
    }

    pub fn profile(&self) -> QualityProfile {
        self.profile.clone().expect("profile not set up")
    }

    pub fn tmp(&mut self) -> std::path::PathBuf {
        if self.tmp.is_none() {
            self.tmp = Some(tempfile::tempdir().expect("tempdir"));
        }
        self.tmp.as_ref().unwrap().path().to_path_buf()
    }

    pub fn quality_named(name: &str) -> QualityId {
        default_definitions()
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("no default quality named {name:?}"))
            .id
    }

    pub fn quality_name(id: QualityId) -> String {
        default_definitions()
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// A fresh, unsaved movie with a unique TMDB id.
    pub fn new_movie(title: &str, year: u16, monitored: bool) -> Movie {
        let mut m = Movie::new(
            ExternalIds {
                tmdb: Some(TmdbId(TMDB_SEQ.fetch_add(1, Ordering::SeqCst))),
                ..Default::default()
            },
            title,
            ProfileId::new(),
            RootFolder::new("/movies"),
        );
        m.year = Some(year);
        m.monitored = monitored;
        m
    }

    /// Save a movie plus one Theatrical edition and remember both.
    pub async fn save_movie_with_edition(
        &mut self,
        title: &str,
        year: u16,
        monitored: bool,
        edition_status: skadi_core::AcquisitionStatus,
    ) -> (Movie, MovieEdition) {
        let movie = Self::new_movie(title, year, monitored);
        let store = self.store();
        store.upsert_movie(&movie).await.expect("upsert movie");
        let mut e = MovieEdition::missing(
            movie.id,
            skadi_core::EditionKindId::from(skadi_movies::THEATRICAL_KIND_ID),
        );
        // Mirror what `set_edition_status(Imported)` denormalizes so the row is
        // shaped like a real import (file/quality/score columns filled).
        if let skadi_core::AcquisitionStatus::Imported {
            file,
            quality,
            score,
            ..
        } = &edition_status
        {
            e.file = Some(file.clone());
            e.quality = Some(*quality);
            e.format_score = *score;
        }
        e.status = edition_status;
        store.upsert_edition(&e).await.expect("upsert edition");
        self.movies.insert(title.to_string(), movie.clone());
        self.editions.insert(title.to_string(), e.clone());
        (movie, e)
    }

    pub fn movie_id(&self, title: &str) -> MovieId {
        self.movies
            .get(title)
            .unwrap_or_else(|| panic!("unknown movie {title:?}"))
            .id
    }

    pub async fn reload_edition(&self, title: &str) -> MovieEdition {
        let e = self
            .editions
            .get(title)
            .unwrap_or_else(|| panic!("no edition for {title:?}"));
        self.store()
            .get_edition(e.id)
            .await
            .expect("get edition")
            .expect("edition row present")
    }

    pub fn seed_for(&self, title: &str) -> &AcquireSeed {
        let e = self
            .editions
            .get(title)
            .unwrap_or_else(|| panic!("no edition for {title:?}"));
        self.seeds
            .iter()
            .find(|s| s.acquirable == e.acquirable_ref())
            .unwrap_or_else(|| panic!("no seed emitted for {title:?}; seeds: {:?}", self.seeds))
    }
}
