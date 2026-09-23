//! `TelevisionModule` — `DomainModule` for the television domain (SKADI-T-0276).
//!
//! Assembles `skadi-tv` (entities, repo, metadata, wanted-query, matcher,
//! status-sink) into the [`HunterWorker`](skadi_hunter::HunterWorker) + a
//! metadata-refresh worker the daemon spawns. Mirrors `MoviesModule` seam-for-seam;
//! the TV metadata source is the keyless Skyhook [`SeriesMetadataProvider`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use diesel_migrations::EmbeddedMigrations;
use skadi_core::{AppError, DomainModule, MediaKind, Result, Worker, module::BoxedWorker};
use skadi_downloaders::Downloader;
use skadi_hunter::{
    DEFAULT_SWEEP_MAX_CONCURRENT, HunterServices, HunterWorker, ImporterFactory, WantedQuery,
    build_runner_for, cloacina_target_for, services::ScoringConfig,
};
use skadi_importer::{AcquirableRef, DefaultImporter, Importer};
use skadi_indexers::Indexer;
use skadi_metadata::SeriesMetadataProvider;
use skadi_notify::Notifier;
use skadi_quality::{QualityProfile, default_definitions};
use skadi_store::{ConfigRepo, SettingsRepo, Store};
use uuid::Uuid;

use crate::matcher::EpisodeMatcher;
use crate::naming::SeriesNaming;
use crate::repo::TvRepo;
use crate::status_sink::EpisodeStatusSink;
use crate::wanted::{SeriesWantedQuery, WantedScoring};

/// Default sweep cadence.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Shared dependencies the daemon hands every domain at construction.
pub struct SharedHunterDeps {
    pub indexers: Vec<Arc<dyn Indexer>>,
    pub downloaders: Vec<Arc<dyn Downloader>>,
    pub notifiers: Vec<Arc<dyn Notifier>>,
    pub scoring: ScoringConfig,
}

/// Live provider lists, swappable at runtime via the supervisor's reconcile.
struct LiveProviders {
    indexers: Vec<Arc<dyn Indexer>>,
    downloaders: Vec<Arc<dyn Downloader>>,
    notifiers: Vec<Arc<dyn Notifier>>,
}

/// The television `DomainModule`.
pub struct TelevisionModule {
    store: Store,
    providers: std::sync::RwLock<LiveProviders>,
    scoring: std::sync::RwLock<ScoringConfig>,
    default_profile: QualityProfile,
    runner: Arc<cloacina::runner::DefaultRunner>,
    wanted_query: std::sync::RwLock<Arc<SeriesWantedQuery>>,
    sweep_max_concurrent: usize,
    /// Keyless Skyhook provider for the refresh worker; set by the daemon after
    /// construction. `None` disables the refresh worker.
    series_provider: std::sync::RwLock<Option<Arc<dyn SeriesMetadataProvider>>>,
    breaker: Arc<skadi_hunter::CircuitBreaker>,
    refresh_interval: Duration,
    refresh_staleness: Duration,
}

impl TelevisionModule {
    /// Build against a freshly-derived runner target.
    pub async fn new(store: Store, skadi_url: &str, shared: SharedHunterDeps) -> Result<Self> {
        let target = cloacina_target_for(skadi_url)?;
        let runner = Arc::new(build_runner_for(&target).await?);
        Self::with_runner(store, runner, shared).await
    }

    /// Build against the daemon's **shared** Cloacina runner.
    pub async fn with_runner(
        store: Store,
        runner: Arc<cloacina::runner::DefaultRunner>,
        shared: SharedHunterDeps,
    ) -> Result<Self> {
        let default_profile = shared.scoring.profile.clone();
        let sweep_max_concurrent = store
            .get_config("sweep_max_concurrent")
            .await
            .ok()
            .flatten()
            .and_then(|e| e.value.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_SWEEP_MAX_CONCURRENT);
        let refresh_interval = Duration::from_secs(
            store
                .get_config("metadata_refresh_interval_secs")
                .await
                .ok()
                .flatten()
                .and_then(|e| e.value.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(6 * 3600),
        );
        let refresh_staleness = Duration::from_secs(
            store
                .get_config("metadata_staleness_secs")
                .await
                .ok()
                .flatten()
                .and_then(|e| e.value.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(7 * 24 * 3600),
        );
        let breaker_threshold = store
            .get_config("metadata_breaker_threshold")
            .await
            .ok()
            .flatten()
            .and_then(|e| e.value.parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(5);
        let breaker = Arc::new(skadi_hunter::CircuitBreaker::new(
            breaker_threshold,
            Duration::from_secs(60),
            Duration::from_secs(3600),
        ));
        let wanted_query = Arc::new(
            SeriesWantedQuery::new(
                Arc::new(store.clone()),
                WantedScoring {
                    profile: default_profile.clone(),
                    upgrade_until_format_score: 0,
                    regrab_unplayable: false,
                    // Boot default; `reload_profile` reads the operator's setting.
                    search_undated_episodes: false,
                },
            )
            // Judge upgradability on each series' own profile (SKADI-T-0537).
            .with_store(store.clone()),
        );
        Ok(Self {
            store,
            providers: std::sync::RwLock::new(LiveProviders {
                indexers: shared.indexers,
                downloaders: shared.downloaders,
                notifiers: shared.notifiers,
            }),
            scoring: std::sync::RwLock::new(shared.scoring),
            default_profile,
            runner,
            wanted_query: std::sync::RwLock::new(wanted_query),
            sweep_max_concurrent,
            series_provider: std::sync::RwLock::new(None),
            breaker,
            refresh_interval,
            refresh_staleness,
        })
    }

    /// Install the keyless Skyhook provider used by the refresh worker (and shared
    /// with the HTTP add/lookup handlers). The daemon calls this after building it.
    pub fn set_series_provider(&self, provider: Arc<dyn SeriesMetadataProvider>) {
        *self
            .series_provider
            .write()
            .expect("series_provider lock poisoned") = Some(provider);
    }

    /// Sonarr's "search for undated episodes" (SKADI-T-0446), off unless set.
    async fn search_undated_episodes(&self) -> bool {
        use skadi_store::ConfigRepo;
        let Ok(entries) = self.store.list_config().await else {
            return false;
        };
        let view =
            skadi_config::ConfigView::from_pairs(entries.into_iter().map(|e| (e.key, e.value)));
        view.get_bool("tv.search_undated_episodes").unwrap_or(false)
    }

    async fn reload_profile(&self) {
        let defs = default_definitions();
        let profile = resolve_active_profile(&self.store, &self.default_profile, &defs).await;
        let formats =
            skadi_hunter::services::resolve_custom_formats(&self.store, MediaKind::Series).await;
        {
            let mut sc = self.scoring.write().expect("scoring lock poisoned");
            sc.profile = profile.clone();
            sc.formats = formats;
        }
        let upgrade_until_format_score = resolve_format_upgrade_cutoff(&self.store).await;
        // Re-acquire unplayable files (SKADI-T-0584). Default FALSE: scanning the
        // library and acting on the scan are separate decisions, and the operator
        // makes the second one.
        let regrab_unplayable = self
            .store
            .get_config("regrab_unplayable")
            .await
            .ok()
            .flatten()
            .is_some_and(|e| matches!(e.value.trim(), "true" | "1" | "yes"));
        let wq = Arc::new(
            SeriesWantedQuery::new(
                Arc::new(self.store.clone()),
                WantedScoring {
                    profile,
                    upgrade_until_format_score,
                    regrab_unplayable,
                    search_undated_episodes: self.search_undated_episodes().await,
                },
            )
            .with_store(self.store.clone()),
        );
        *self
            .wanted_query
            .write()
            .expect("wanted_query lock poisoned") = wq;
    }

    /// The module's Cloacina runner (shared with the HTTP manual-acquire path).
    pub fn runner(&self) -> Arc<cloacina::runner::DefaultRunner> {
        Arc::clone(&self.runner)
    }

    /// The module's backing store.
    pub fn store(&self) -> Store {
        self.store.clone()
    }

    fn build_services(&self) -> Arc<HunterServices> {
        let repo: Arc<dyn TvRepo> = Arc::new(self.store.clone());
        let providers = self.providers.read().expect("providers lock poisoned");
        let scoring = self.scoring.read().expect("scoring lock poisoned").clone();
        Arc::new(HunterServices {
            kind: MediaKind::Series,
            store: self.store.clone(),
            status: Arc::new(EpisodeStatusSink::new(
                repo.clone(),
                Arc::new(self.store.clone()),
            )),
            indexers: providers.indexers.clone(),
            downloaders: providers.downloaders.clone(),
            importer: Arc::new(DefaultImporter::new(NoMatcher)),
            importer_factory: Some(Arc::new(EpisodeImporterFactory {
                repo,
                config: self.store.clone(),
            })),
            notifiers: providers.notifiers.clone(),
            scoring,
        })
    }
}

#[async_trait]
impl skadi_api::ProviderReloader for TelevisionModule {
    async fn apply(&self, set: skadi_api::ProviderSet) -> Result<()> {
        {
            let mut providers = self
                .providers
                .write()
                .map_err(|_| AppError::Internal("providers lock poisoned".into()))?;
            providers.indexers = set.indexers;
            providers.downloaders = set.downloaders;
            providers.notifiers = set.notifiers;
        }
        self.reload_profile().await;
        skadi_hunter::set_services(self.build_services());
        Ok(())
    }
}

impl DomainModule for TelevisionModule {
    fn name(&self) -> &'static str {
        "television"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Series
    }
    fn sqlite_migrations(&self) -> EmbeddedMigrations {
        crate::SQLITE_MIGRATIONS
    }
    fn postgres_migrations(&self) -> EmbeddedMigrations {
        crate::POSTGRES_MIGRATIONS
    }
    fn workers(&self) -> Vec<BoxedWorker> {
        let services = self.build_services();
        let query = self
            .wanted_query
            .read()
            .expect("wanted_query lock poisoned")
            .clone() as Arc<dyn WantedQuery>;
        let mut workers: Vec<BoxedWorker> = vec![Box::new(SpawnedHunterWorker {
            services,
            runner: Arc::clone(&self.runner),
            interval: DEFAULT_SWEEP_INTERVAL,
            query,
            max_concurrent: self.sweep_max_concurrent,
        })];

        if let Some(provider) = self
            .series_provider
            .read()
            .expect("series_provider lock poisoned")
            .clone()
        {
            let refresh_services = Arc::new(crate::refresh::RefreshServices {
                provider,
                repo: Arc::new(self.store.clone()),
                breaker: Arc::clone(&self.breaker),
            });
            workers.push(Box::new(crate::refresh::SeriesRefreshWorker::new(
                refresh_services,
                Arc::clone(&self.runner),
                self.refresh_interval,
                self.refresh_staleness,
            )));
        }
        // Library media scan (SKADI-T-0583): profiles every imported file so the
        // library can say what will not direct-play. Per-domain rather than one
        // global worker, so a disabled domain stops scanning with everything
        // else it owns.
        workers.push(Box::new(skadi_library_scan::LibraryScanWorker::new(
            vec![Arc::new(crate::scan::EpisodeScanSource::new(
                self.store.clone(),
            ))],
            Arc::new(skadi_library_scan::DefaultProber),
        )));
        workers
    }
}

struct SpawnedHunterWorker {
    services: Arc<HunterServices>,
    runner: Arc<cloacina::runner::DefaultRunner>,
    interval: Duration,
    query: Arc<dyn WantedQuery>,
    max_concurrent: usize,
}

impl Worker for SpawnedHunterWorker {
    fn name(&self) -> &str {
        "television-hunter"
    }
    fn run(
        self: Box<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        let worker = HunterWorker::new(
            self.services,
            self.runner,
            self.interval,
            self.query,
            self.max_concurrent,
        )
        .with_rss(skadi_hunter::DEFAULT_RSS_INTERVAL);
        Box::pin(async move { Box::new(worker).run(cancel).await })
    }
}

// --- ImporterFactory ---

struct EpisodeImporterFactory {
    repo: Arc<dyn TvRepo>,
    config: Store,
}

impl EpisodeImporterFactory {
    async fn naming(&self) -> SeriesNaming {
        match self.config.list_config().await {
            Ok(entries) => {
                let view = skadi_config::ConfigView::from_pairs(
                    entries.into_iter().map(|e| (e.key, e.value)),
                );
                SeriesNaming::from_view(&view)
            }
            Err(_) => SeriesNaming::default(),
        }
    }

    /// The operator-settable import guards (SKADI-T-0417, SKADI-T-0416): library
    /// roots, the marker requirement, the free-space reserve and the file-size
    /// floor. An unreadable config plane yields the all-off default rather than
    /// failing every import.
    async fn guards(&self) -> skadi_config::ImportGuards {
        use skadi_store::ConfigRepo;
        match self.config.list_config().await {
            Ok(entries) => {
                let view = skadi_config::ConfigView::from_pairs(
                    entries.into_iter().map(|e| (e.key, e.value)),
                );
                skadi_config::import_guards(&view)
            }
            Err(_) => skadi_config::ImportGuards::default(),
        }
    }
}

#[async_trait]
impl ImporterFactory for EpisodeImporterFactory {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> Result<Arc<dyn Importer>> {
        // Either shape names one series (SKADI-T-0590): an episode ref through
        // its episode, a season-pack ref directly.
        let series_id = match crate::acquirable::TvAcquirable::parse(acquirable)? {
            crate::acquirable::TvAcquirable::Season { series, .. } => series,
            crate::acquirable::TvAcquirable::Episode(episode_id) => {
                self.repo
                    .get_episode(episode_id)
                    .await?
                    .ok_or_else(|| AppError::NotFound(format!("Episode {episode_id} not found")))?
                    .series_id
            }
        };
        // Load the series with ALL its episodes so the matcher can resolve every
        // file in a season-pack download, not just the one we're acquiring.
        let series = self
            .repo
            .get_series(series_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("Series {series_id} not found")))?;
        let episodes = series.episodes.clone();
        let matcher = EpisodeMatcher::with_naming(series, episodes, self.naming().await);
        let g = self.guards().await;
        Ok(Arc::new(
            DefaultImporter::new(matcher)
                .with_library_roots(g.library_roots)
                .with_require_root_marker(g.require_root_marker)
                .with_min_free_bytes(g.min_free_bytes)
                .with_min_file_bytes(g.min_file_bytes)
                .with_recycle_bin(g.recycle_bin)
                // SKADI-T-0138: acquire-path placement policy. Library-import
                // has its own path and always hardlinks.
                .with_move_on_import(g.move_on_import),
        ) as Arc<dyn Importer>)
    }
}

/// Fallback matcher; TV always sets a factory, so this never matches.
struct NoMatcher;
impl skadi_importer::AcquirableMatcher for NoMatcher {
    fn match_file(
        &self,
        _parsed: &skadi_quality::ParsedRelease,
        _source: &std::path::Path,
        _completed: &skadi_importer::CompletedDownload,
    ) -> Vec<skadi_importer::AcquirableMatch> {
        Vec::new()
    }
}

// --- profile resolution (mirrors the movies module) ---

#[derive(serde::Deserialize)]
struct ProfileSpec {
    #[serde(default)]
    name: Option<String>,
    allowed: Vec<Uuid>,
    cutoff: Uuid,
    #[serde(default)]
    upgrade_allowed: bool,
    #[serde(default)]
    min_format_score: i32,
    #[serde(default)]
    formats: Vec<skadi_quality::CustomFormatScore>,
    #[serde(default)]
    upgrade_until_format_score: i32,
}

async fn resolve_format_upgrade_cutoff(store: &Store) -> i32 {
    let Ok(mut rows) = store.list_settings("profiles").await else {
        return 0;
    };
    // Deterministic for the same reason as `resolve_active_profile` (SKADI-T-0531):
    // an unordered `.next()` made this opt-in setting depend on row order.
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.into_iter()
        .next()
        .and_then(|row| serde_json::from_value::<ProfileSpec>(row.body).ok())
        .map_or(0, |spec| spec.upgrade_until_format_score)
}

async fn resolve_active_profile(
    store: &Store,
    default: &QualityProfile,
    defs: &[skadi_quality::QualityDefinition],
) -> QualityProfile {
    let rows = match store.list_settings("profiles").await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "reading profiles settings; using default profile");
            return default.clone();
        }
    };
    // Pick deterministically (SKADI-T-0531). This used to take whichever row came
    // back first, so with the six seeded profiles the domain-wide fallback — and
    // with it which items count as upgradable — changed between processes. Items
    // are scored against their OWN profile in `decide`; this is only the fallback
    // for an item whose profile row is missing, so a stable choice beats a random
    // one. Sorting by id is arbitrary but reproducible; a real "default profile"
    // setting is SKADI-T-0532.
    // Prefer the row the operator marked default (SKADI-T-0532), falling back to
    // the lowest id only when none is marked. "Lowest uuid wins" was reproducible
    // after SKADI-T-0531 but still arbitrary: an operator could not see which
    // profile was acting as the fallback, could not choose it, and had no reason
    // to expect the answer to survive a reseed.
    let mut rows = rows;
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    // Read off the raw body rather than `ProfileSpec`: the choice of row happens
    // before any row is deserialised, and a profile whose spec is invalid should
    // still be able to *be* the marked default — otherwise a typo elsewhere in
    // the row would silently move the fallback.
    let marked = rows.iter().position(|r| {
        r.body
            .get("default")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    });
    let Some(row) = marked.map_or_else(|| rows.first().cloned(), |i| rows.get(i).cloned()) else {
        return default.clone();
    };
    let spec: ProfileSpec = match serde_json::from_value(row.body.clone()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(id = %row.id, error = %e, "invalid profile body; using default profile");
            return default.clone();
        }
    };
    let known: std::collections::HashSet<Uuid> = defs.iter().map(|d| d.id.into_uuid()).collect();
    let valid = !spec.allowed.is_empty()
        && spec.allowed.iter().all(|q| known.contains(q))
        && spec.allowed.contains(&spec.cutoff);
    if !valid {
        tracing::warn!(id = %row.id, "profile references unknown qualities or cutoff not in allowed; using default profile");
        return default.clone();
    }
    let id = Uuid::parse_str(&row.id)
        .map(skadi_core::ProfileId::from)
        .unwrap_or(default.id);
    QualityProfile {
        id,
        name: spec.name.unwrap_or_else(|| "profile".into()),
        allowed: spec
            .allowed
            .into_iter()
            .map(skadi_core::QualityId::from)
            .collect(),
        cutoff: skadi_core::QualityId::from(spec.cutoff),
        upgrade_allowed: spec.upgrade_allowed,
        formats: spec.formats,
        min_format_score: spec.min_format_score,
    }
}
