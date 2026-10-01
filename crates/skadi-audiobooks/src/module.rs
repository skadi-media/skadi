//! `AudiobooksModule` — `DomainModule` for the audiobooks domain (SKADI-T-0129).
//!
//! Assembles the `skadi-audiobooks` pieces into a
//! [`HunterWorker`](skadi_hunter::HunterWorker) the daemon spawns, mirroring
//! `skadi_movies::MoviesModule`. Differences: the `ScoringConfig` carries the
//! **audiobook** quality axis (`ScoringConfig.audiobook = Some(..)`), the
//! importer-factory builds an [`AudiobookMatcher`], and (for now) there is no
//! settings-driven live profile resolution — the audiobook profile handed in at
//! construction is used as-is. The metadata-refresh worker is deferred to
//! SKADI-T-0135 (the provider slot is wired now so the daemon hookup is stable).

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
use skadi_indexers::{Category, Indexer};
use skadi_metadata::{AudibleCatalogProvider, MetadataProvider};
use skadi_notify::Notifier;
use skadi_quality::QualityProfile;
use skadi_store::{ConfigRepo, Store};
use uuid::Uuid;

use crate::matcher::AudiobookMatcher;
use crate::repo::AudiobooksRepo;
use crate::status_sink::AudiobookStatusSink;
use crate::wanted::{AudiobookWantedQuery, WantedScoring};

/// Default sweep cadence (audiobooks acquire less often than movies churn, but a
/// shared default keeps things simple; tunable later via the config plane).
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Shared infra the daemon hands the module at construction.
pub struct SharedHunterDeps {
    pub indexers: Vec<Arc<dyn Indexer>>,
    pub downloaders: Vec<Arc<dyn Downloader>>,
    pub notifiers: Vec<Arc<dyn Notifier>>,
    /// Must carry `audiobook: Some(..)` (the audiobook quality axis).
    pub scoring: ScoringConfig,
}

struct LiveProviders {
    indexers: Vec<Arc<dyn Indexer>>,
    downloaders: Vec<Arc<dyn Downloader>>,
    notifiers: Vec<Arc<dyn Notifier>>,
}

/// The audiobooks `DomainModule`.
pub struct AudiobooksModule {
    store: Store,
    providers: std::sync::RwLock<LiveProviders>,
    scoring: ScoringConfig,
    runner: Arc<cloacina::runner::DefaultRunner>,
    wanted_query: Arc<AudiobookWantedQuery>,
    sweep_max_concurrent: usize,
    /// Per-ASIN enrichment provider (Audnexus). Used by author-discovery
    /// (SKADI-T-0132) to enrich each discovered ASIN, and by the (deferred,
    /// SKADI-T-0135) refresh worker. Wired by the daemon after construction.
    metadata_provider: std::sync::RwLock<Option<Arc<dyn MetadataProvider>>>,
    /// Author → products catalog provider (Audible) for author-discovery
    /// (SKADI-T-0132). When set together with `metadata_provider`, `workers()`
    /// spawns the discovery worker. Wired by the daemon after construction.
    catalog_provider: std::sync::RwLock<Option<Arc<AudibleCatalogProvider>>>,
    /// Per-provider circuit breaker shared by metadata-refresh runs (SKADI-T-0135).
    breaker: Arc<skadi_hunter::CircuitBreaker>,
}

impl AudiobooksModule {
    /// Build the module against the isolated Cloacina target derived from
    /// `skadi_url`.
    pub async fn new(store: Store, skadi_url: &str, shared: SharedHunterDeps) -> Result<Self> {
        let target = cloacina_target_for(skadi_url)?;
        let runner = Arc::new(build_runner_for(&target).await?);
        Self::with_runner(store, runner, shared).await
    }

    /// Build the module against an **already-built, shared** Cloacina runner
    /// (SKADI-T-0136), mirroring [`MoviesModule::with_runner`]. The daemon hands
    /// the same `Arc<DefaultRunner>` to every domain so all domains dispatch
    /// through one runner / `hunter.db`.
    pub async fn with_runner(
        store: Store,
        runner: Arc<cloacina::runner::DefaultRunner>,
        mut shared: SharedHunterDeps,
    ) -> Result<Self> {
        let sweep_max_concurrent = store
            .get_config("sweep_max_concurrent")
            .await
            .ok()
            .flatten()
            .and_then(|e| e.value.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_SWEEP_MAX_CONCURRENT);
        // Load the audiobook-domain custom-format registry (SKADI-T-0183). The
        // audiobook profile is the fixed built-in (no upgrades), but custom
        // formats still rank/penalize releases. Audiobook scoring is static (no
        // live profile reload), so resolve once at construction.
        shared.scoring.formats =
            skadi_hunter::services::resolve_custom_formats(&store, MediaKind::Audiobook).await;
        let profile = shared.scoring.profile.clone();
        let wanted_query = Arc::new(AudiobookWantedQuery::new(
            Arc::new(store.clone()),
            WantedScoring { profile },
        ));
        Ok(Self {
            store,
            providers: std::sync::RwLock::new(LiveProviders {
                indexers: shared.indexers,
                downloaders: shared.downloaders,
                notifiers: shared.notifiers,
            }),
            scoring: shared.scoring,
            runner,
            wanted_query,
            sweep_max_concurrent,
            metadata_provider: std::sync::RwLock::new(None),
            catalog_provider: std::sync::RwLock::new(None),
            breaker: Arc::new(skadi_hunter::CircuitBreaker::new(
                5,
                Duration::from_secs(60),
                Duration::from_secs(3600),
            )),
        })
    }

    /// Install the per-ASIN enrichment provider (Audnexus), used by author
    /// discovery and the deferred refresh worker. Wired by the daemon.
    pub fn set_metadata_provider(&self, provider: Arc<dyn MetadataProvider>) {
        *self
            .metadata_provider
            .write()
            .expect("metadata_provider lock poisoned") = Some(provider);
    }

    /// Install the Audible catalog provider for author discovery (SKADI-T-0132).
    /// Wired by the daemon; discovery only runs once both this and the metadata
    /// provider are set.
    pub fn set_catalog_provider(&self, provider: Arc<AudibleCatalogProvider>) {
        *self
            .catalog_provider
            .write()
            .expect("catalog_provider lock poisoned") = Some(provider);
    }

    /// The module's Cloacina runner (shared with the HTTP manual-acquire endpoint).
    pub fn runner(&self) -> Arc<cloacina::runner::DefaultRunner> {
        Arc::clone(&self.runner)
    }

    /// The module's backing store.
    pub fn store(&self) -> Store {
        self.store.clone()
    }

    fn build_services(&self) -> Arc<HunterServices> {
        let repo: Arc<dyn AudiobooksRepo> = Arc::new(self.store.clone());
        let providers = self.providers.read().expect("providers lock poisoned");
        Arc::new(HunterServices {
            kind: MediaKind::Audiobook,
            store: self.store.clone(),
            status: Arc::new(AudiobookStatusSink::new(
                repo.clone(),
                Arc::new(self.store.clone()),
            )),
            indexers: providers.indexers.clone(),
            downloaders: providers.downloaders.clone(),
            importer: Arc::new(DefaultImporter::new(NoMatcher)),
            importer_factory: Some(Arc::new(AudiobookImporterFactory {
                repo,
                config: self.store.clone(),
            })),
            notifiers: providers.notifiers.clone(),
            scoring: self.scoring.clone(),
        })
    }
}

#[async_trait]
impl skadi_api::ProviderReloader for AudiobooksModule {
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
        skadi_hunter::set_services(self.build_services());
        Ok(())
    }
}

impl DomainModule for AudiobooksModule {
    fn name(&self) -> &'static str {
        "audiobooks"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Audiobook
    }
    fn sqlite_migrations(&self) -> EmbeddedMigrations {
        crate::SQLITE_MIGRATIONS
    }
    fn postgres_migrations(&self) -> EmbeddedMigrations {
        crate::POSTGRES_MIGRATIONS
    }
    fn workers(&self) -> Vec<BoxedWorker> {
        let services = self.build_services();
        let query = Arc::clone(&self.wanted_query) as Arc<dyn WantedQuery>;
        let mut workers: Vec<BoxedWorker> = vec![Box::new(SpawnedHunterWorker {
            services,
            runner: Arc::clone(&self.runner),
            interval: DEFAULT_SWEEP_INTERVAL,
            query,
            max_concurrent: self.sweep_max_concurrent,
        })];

        // Metadata + discovery workers need the providers the daemon wires after
        // construction. `metadata_provider` (Audnexus) drives the refresh worker
        // (SKADI-T-0135); paired with `catalog_provider` (Audible) it also drives
        // author-discovery (SKADI-T-0132).
        let enrich = self
            .metadata_provider
            .read()
            .expect("metadata_provider lock poisoned")
            .clone();
        let catalog = self
            .catalog_provider
            .read()
            .expect("catalog_provider lock poisoned")
            .clone();

        // Metadata-refresh worker (SKADI-T-0135): keeps book metadata fresh on a
        // schedule, behind a per-provider circuit breaker, on the shared runner.
        if let Some(enrich) = enrich.clone() {
            let services = Arc::new(crate::refresh::RefreshServices {
                provider: enrich,
                repo: Arc::new(self.store.clone()),
                breaker: Arc::clone(&self.breaker),
            });
            workers.push(Box::new(crate::refresh::BookRefreshWorker::new(
                services,
                Arc::clone(&self.runner),
                crate::refresh::DEFAULT_REFRESH_INTERVAL,
                crate::refresh::DEFAULT_REFRESH_STALENESS,
            )));
        }

        // Contributor-role normalisation (SKADI-T-0652): rewrites role-suffixed
        // author strings stored before ingest parsed them. Runs once per start and
        // exits; idempotent, so there is nothing to track. Workers are spawned
        // concurrently, so this does NOT finish before discovery's first pass; that
        // pass may query one role-suffixed name once. Harmless: `name_key` strips
        // roles, so matching is unaffected, and the next pass sees clean names.
        workers.push(Box::new(crate::roles::AuthorMaintenanceWorker::new(
            self.store.clone(),
        )));

        // Author-discovery worker (SKADI-T-0132): only once both the catalog
        // provider (author → products) and the enrichment provider are wired.
        if let (Some(enrich), Some(catalog)) = (enrich, catalog) {
            let discovery = Arc::new(crate::discovery::AuthorDiscovery::new(
                Arc::new(self.store.clone()),
                self.store.clone(),
                catalog,
                enrich,
            ));
            workers.push(Box::new(crate::discovery::AuthorDiscoveryWorker::new(
                discovery,
                crate::discovery::DEFAULT_DISCOVERY_INTERVAL,
            )));
        }

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
        "audiobooks-hunter"
    }
    fn run(
        self: Box<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        // RSS fast pass (SKADI-T-0192): catch newly-posted audiobooks within
        // minutes via a short-cadence recent-feed pull, alongside the full sweep.
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

struct AudiobookImporterFactory {
    repo: Arc<dyn AudiobooksRepo>,
    /// Source for operator naming-template config (SKADI-T-0228); `Store` is a `ConfigRepo`.
    config: Store,
}

impl AudiobookImporterFactory {
    async fn naming(&self) -> crate::naming::AudiobookNaming {
        use skadi_store::ConfigRepo;
        match self.config.list_config().await {
            Ok(entries) => {
                let view = skadi_config::ConfigView::from_pairs(
                    entries.into_iter().map(|e| (e.key, e.value)),
                );
                crate::naming::AudiobookNaming::from_view(&view)
            }
            Err(_) => crate::naming::AudiobookNaming::default(),
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
impl ImporterFactory for AudiobookImporterFactory {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> Result<Arc<dyn Importer>> {
        let file_id = Uuid::parse_str(&acquirable.0)
            .map(skadi_core::BookFileId::from)
            .map_err(|e| AppError::Validation(format!("bad acquirable ref: {e}")))?;
        let file = self
            .repo
            .get_book_file(file_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("BookFile {file_id} not found")))?;
        let book = self
            .repo
            .get_book(file.book_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("Book {} not found", file.book_id)))?;

        // Pack-aware matching (SKADI-T-0309): load the other library books in this book's
        // series as candidates, each with a target file, so a multi-book pack download fans
        // each file out to the right book instead of dumping them all under this one. Best
        // effort — on any repo error we fall back to single-book matching.
        let mut siblings: Vec<(crate::book::Book, crate::book_file::BookFile)> = Vec::new();
        if let Some(sid) = book.series.as_ref().map(|s| s.series_id)
            && let Ok(all) = self
                .repo
                .list_books(crate::repo::BookFilter {
                    monitored: None,
                    limit: None,
                    offset: None,
                })
                .await
        {
            for sib in all {
                if sib.id == book.id || sib.series.as_ref().map(|s| s.series_id) != Some(sid) {
                    continue;
                }
                if let Ok(Some(f)) = self
                    .repo
                    .list_book_files(sib.id)
                    .await
                    .map(|fs| fs.into_iter().next())
                {
                    siblings.push((sib, f));
                }
            }
        }
        let matcher = AudiobookMatcher::with_candidates(book, file, siblings, self.naming().await);
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

/// Fallback matcher for [`HunterServices::importer`]; audiobooks always set the
/// factory, so this never matches.
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

/// Stable id for the built-in audiobook quality profile. Audiobooks rank via the
/// Torznab categories the audiobook search sweeps. **`3030` (Audio › Audiobook)
/// only.** SKADI-T-0176 briefly widened this to the Books family (`7000`/`7020`) to
/// catch audiobooks mis-filed as Books, but reverted in SKADI-T-0178: those
/// categories are dominated by ebooks/manga/courses that fuzzy-match a title's
/// words (e.g. "Demon World" → "Realist Demon King" manga), which download then
/// fail import. Clean audiobook signal comes from the audiobook category and a
/// dedicated source (AudioBookBay), not generic-Books fishing. The
/// [`looks_like_ebook`](skadi_quality::audiobook::looks_like_ebook) guard stays as
/// a belt-and-suspenders filter for the occasional mislabeled 3030 result.
pub const AUDIOBOOK_SEARCH_CATEGORIES: &[Category] = &[Category(3030)];

/// built-in M4B-first ladder ([`skadi_quality::audiobook::default_audiobook_definitions`]),
/// not a stored movie `profiles` row — so books bind to this synthetic id rather
/// than forcing operators to register a (movie-shaped) quality profile, which is
/// what used to leak an "Audiobooks" entry into the movies profile list
/// (SKADI-T-0142). The value is arbitrary but fixed so it's stable across runs.
#[must_use]
pub fn audiobook_builtin_profile_id() -> skadi_core::ProfileId {
    skadi_core::ProfileId::from(Uuid::from_u128(0x000A_0D10_B00C))
}

/// Build a permissive audiobook quality profile over every default audiobook
/// definition (allowed low→high, cutoff = highest, no upgrades). The daemon uses
/// this as the audiobooks domain's startup/default profile. Carries the stable
/// [`audiobook_builtin_profile_id`] so books that bind to the built-in profile
/// and this object agree on identity.
#[must_use]
pub fn default_audiobook_profile() -> QualityProfile {
    let defs = skadi_quality::audiobook::default_audiobook_definitions();
    let allowed: Vec<_> = defs.iter().map(|d| d.id).collect();
    let cutoff = *allowed.last().expect("audiobook definitions are non-empty");
    QualityProfile {
        id: audiobook_builtin_profile_id(),
        name: "Audiobook Standard".into(),
        allowed,
        cutoff,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_hunter::services::AudiobookScoring;
    use skadi_quality::audiobook::default_audiobook_definitions;

    async fn store() -> Store {
        let path = skadi_core::unique_temp_path("ab-mod").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn module_builds_and_exposes_one_hunter_worker() {
        let store = store().await;
        // The runner URL must point at a database in a **directory of its own**
        // (SKADI-T-0542). `cloacina_target_for` derives the runner's database
        // from the *parent directory* of this URL — `<dir>/hunter.db` — so any
        // two tests whose databases share a directory silently share one runner
        // database and race each other's migrations.
        //
        // The old value here was `sqlite://:memory:`, which has no parent at
        // all, so it hit that function's fallback and every concurrent process
        // opened `./hunter.db` in the crate root. Hence the load-dependent
        // "Failed to set WAL mode: database is locked", and hence three stray
        // `hunter.db` files that had accumulated on disk.
        //
        // A per-test directory fixes it at the source. A unique *file name* in
        // the shared temp dir does not — that was tried, and turned the WAL
        // error into "table contexts already exists".
        let dir = tempfile::tempdir().expect("tempdir");
        let url = format!("sqlite://{}", dir.path().join("skadi.db").display());
        let url = url.as_str();
        let scoring = ScoringConfig {
            definitions: vec![],
            profile: default_audiobook_profile(),
            formats: vec![],
            min_seeders: 0,
            audiobook: Some(AudiobookScoring {
                definitions: default_audiobook_definitions(),
                allow_abridged: false,
            }),
        };
        let module = AudiobooksModule::new(
            store,
            url,
            SharedHunterDeps {
                indexers: vec![],
                downloaders: vec![],
                notifiers: vec![],
                scoring,
            },
        )
        .await
        .unwrap();
        assert_eq!(module.name(), "audiobooks");
        assert_eq!(module.kind(), MediaKind::Audiobook);
        // Asserted by name, not by raw count: the point is "exactly one hunter,
        // and no provider-dependent workers without providers". The role
        // normaliser (SKADI-T-0652) is unconditional — it only needs the store.
        let names: Vec<String> = module
            .workers()
            .iter()
            .map(|w| w.name().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["audiobooks-hunter", "audiobooks-author-maintenance"],
            "one hunter worker plus author maintenance (refresh and discovery deferred)"
        );
        module.runner().shutdown().await.unwrap();
    }
}
