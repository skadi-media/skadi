//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use skadi_core::{ExternalIds, ProfileId, QualityId, RootFolder, SeriesId, TvdbId};
use skadi_hunter::AcquireSeed;
use skadi_importer::AcquirableMatch;
use skadi_quality::{QualityProfile, default_definitions};
use skadi_store::Store;
use skadi_testsupport::TestDb;
use skadi_tv::{Episode, EpisodeMatcher, Series, TvRepo};

pub struct Db(pub TestDb);
impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Db({})", self.0.backend_name())
    }
}

pub struct Matcher(pub EpisodeMatcher);
impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EpisodeMatcher")
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub db: Option<Db>,
    pub profile: Option<QualityProfile>,
    pub upgrade_until_format_score: i32,
    /// Sonarr's "search for undated episodes" (SKADI-T-0446).
    pub search_undated_episodes: bool,
    /// Series by title (as saved).
    pub series: HashMap<String, Series>,
    /// Episodes by "Title/SxxEyy".
    pub episodes: HashMap<String, Episode>,
    pub seeds: Vec<AcquireSeed>,
    pub recovered: usize,
    pub tmp: Option<tempfile::TempDir>,
    pub files: HashMap<String, std::path::PathBuf>,
    pub matcher: Option<Matcher>,
    pub matches: Vec<AcquirableMatch>,
    pub error: Option<String>,
    pub last_series: Option<Series>,
    pub listed: Vec<Series>,
    pub library_items: Vec<skadi_api::LibraryItemDto>,
    pub history: Vec<skadi_store::HistoryEntry>,
    pub read_status: Option<Option<skadi_core::AcquisitionStatus>>,
    pub provider: Option<steps::metadata::FakeProvider>,
}

static TVDB_SEQ: AtomicU64 = AtomicU64::new(50_000);

pub fn ep_key(title: &str, season: u16, number: u16) -> String {
    format!("{title}/S{season:02}E{number:02}")
}

impl World {
    pub fn store(&self) -> Store {
        self.db
            .as_ref()
            .expect("library not set up")
            .0
            .store
            .clone()
    }

    pub fn repo(&self) -> std::sync::Arc<dyn TvRepo> {
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

    pub fn new_series(title: &str, year: u16, monitored: bool) -> Series {
        let mut s = Series::new(
            ExternalIds {
                tvdb: Some(TvdbId(TVDB_SEQ.fetch_add(1, Ordering::SeqCst))),
                tmdb: Some(skadi_core::TmdbId(1399)),
                ..Default::default()
            },
            title,
            ProfileId::new(),
            RootFolder::new("/tv"),
        );
        s.year = Some(year);
        s.monitored = monitored;
        s
    }

    pub async fn save_series(&mut self, title: &str, year: u16, monitored: bool) -> Series {
        let s = Self::new_series(title, year, monitored);
        self.store().upsert_series(&s).await.expect("upsert series");
        self.series.insert(title.to_string(), s.clone());
        s
    }

    pub fn series_id(&self, title: &str) -> SeriesId {
        self.series
            .get(title)
            .unwrap_or_else(|| panic!("unknown series {title:?}"))
            .id
    }

    /// Save an episode row and remember it under `Title/SxxEyy`.
    pub async fn save_episode(&mut self, title: &str, mut ep: Episode) -> Episode {
        // Mirror what `set_episode_status(Imported)` denormalizes.
        if let skadi_core::AcquisitionStatus::Imported {
            file,
            quality,
            score,
            ..
        } = &ep.status
        {
            ep.file = Some(file.clone());
            ep.quality = Some(*quality);
            ep.format_score = *score;
        }
        self.store()
            .upsert_episode(&ep)
            .await
            .expect("upsert episode");
        self.episodes
            .insert(ep_key(title, ep.season, ep.number), ep.clone());
        ep
    }

    pub fn episode(&self, title: &str, season: u16, number: u16) -> Episode {
        self.episodes
            .get(&ep_key(title, season, number))
            .unwrap_or_else(|| panic!("unknown episode {}", ep_key(title, season, number)))
            .clone()
    }

    pub async fn reload_episode(&self, title: &str, season: u16, number: u16) -> Episode {
        let e = self.episode(title, season, number);
        self.store()
            .get_episode(e.id)
            .await
            .expect("get episode")
            .expect("episode row present")
    }

    pub fn seed_for(&self, title: &str, season: u16, number: u16) -> &AcquireSeed {
        let e = self.episode(title, season, number);
        self.seeds
            .iter()
            .find(|s| s.acquirable == e.acquirable_ref())
            .unwrap_or_else(|| {
                panic!(
                    "no seed for {}; seeds: {:?}",
                    ep_key(title, season, number),
                    self.seeds
                        .iter()
                        .map(|s| (&s.acquirable.0, s.request.tv))
                        .collect::<Vec<_>>()
                )
            })
    }
}
