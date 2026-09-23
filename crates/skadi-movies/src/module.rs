//! `MoviesModule` — `DomainModule` for the movies domain (SKADI-T-0049).
//!
//! Assembles every other piece in `skadi-movies` (T-0043..T-0048) into a
//! [`HunterWorker`](skadi_hunter::HunterWorker) the daemon (I-0008) can spawn.
//! The hunter is edition-agnostic; this module is the per-domain wiring that
//! makes it actually drive movies.

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
use skadi_metadata::MetadataProvider;
use skadi_notify::Notifier;
use skadi_quality::{QualityProfile, default_definitions};
use skadi_store::{ConfigRepo, SettingsRepo, Store};
use uuid::Uuid;

use crate::matcher::MovieMatcher;
use crate::repo::MoviesRepo;
use crate::status_sink::MovieStatusSink;
use crate::wanted::{MovieWantedQuery, WantedScoring};

/// Default sweep cadence. v0; the daemon (I-0008) will make it configurable.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Shared dependencies the daemon hands every domain at construction —
/// indexers, downloaders, notifiers, the scoring config.
pub struct SharedHunterDeps {
    pub indexers: Vec<Arc<dyn Indexer>>,
    pub downloaders: Vec<Arc<dyn Downloader>>,
    pub notifiers: Vec<Arc<dyn Notifier>>,
    pub scoring: ScoringConfig,
}

/// The live provider lists, swappable at runtime (SKADI-T-0062): the
/// supervisor's provider reconcile replaces them via
/// [`ProviderReloader`](skadi_api::ProviderReloader) without rebuilding the
/// module.
struct LiveProviders {
    indexers: Vec<Arc<dyn Indexer>>,
    downloaders: Vec<Arc<dyn Downloader>>,
    notifiers: Vec<Arc<dyn Notifier>>,
}

/// The movies `DomainModule`. Construct once at daemon startup with the
/// shared infra, then return it from the daemon's compile-time domain registry.
pub struct MoviesModule {
    store: Store,
    /// Providers behind a lock so a settings change can swap them live.
    providers: std::sync::RwLock<LiveProviders>,
    /// Scoring config behind a lock so a `profiles` settings change can swap the
    /// active quality profile live (SKADI-T-0067). `decide` reads it via the
    /// re-published `HunterServices`.
    scoring: std::sync::RwLock<ScoringConfig>,
    /// The permissive profile handed in at construction — the fallback used when
    /// no (or an invalid) `profiles` row is configured.
    default_profile: QualityProfile,
    /// The Cloacina runner is built once per module so its workflow registry
    /// + background services live for the module's lifetime.
    runner: Arc<cloacina::runner::DefaultRunner>,
    /// Wanted-query (sweep enumeration) behind a lock so it is rebuilt with the
    /// active profile on a `profiles` change; newly-spawned workers read it.
    wanted_query: std::sync::RwLock<Arc<MovieWantedQuery>>,
    /// Max concurrent acquire runs per sweep tick (SKADI-T-0040), read from the
    /// `sweep_max_concurrent` config key at construction.
    sweep_max_concurrent: usize,
    /// Metadata provider for the periodic refresh worker (SKADI-T-0041),
    /// installed by the daemon after construction (it's built later). `None`
    /// disables the refresh worker.
    metadata_provider: std::sync::RwLock<Option<Arc<dyn MetadataProvider>>>,
    /// Per-provider circuit breaker shared by refresh runs (SKADI-T-0041).
    breaker: Arc<skadi_hunter::CircuitBreaker>,
    /// How often the refresh worker looks for stale movies.
    refresh_interval: Duration,
    /// A movie is refreshed when `last_metadata_refresh` is older than this.
    refresh_staleness: Duration,
}

impl MoviesModule {
    /// Build the module. The Cloacina runner is constructed against the
    /// isolated target derived from `skadi_url` (PG schema / sibling
    /// `hunter.db`); see `skadi_hunter::cloacina_target_for`.
    pub async fn new(store: Store, skadi_url: &str, shared: SharedHunterDeps) -> Result<Self> {
        let target = cloacina_target_for(skadi_url)?;
        let runner = Arc::new(build_runner_for(&target).await?);
        Self::with_runner(store, runner, shared).await
    }

    /// Build the module against an **already-built, shared** Cloacina runner
    /// (SKADI-T-0136). The daemon builds one runner and hands the same `Arc` to
    /// every domain module so all domains dispatch through a single runner /
    /// `hunter.db` instead of one runner per module colliding on the same file.
    pub async fn with_runner(
        store: Store,
        runner: Arc<cloacina::runner::DefaultRunner>,
        shared: SharedHunterDeps,
    ) -> Result<Self> {
        let default_profile = shared.scoring.profile.clone();
        // Sweep concurrency cap (SKADI-T-0040), from the config plane.
        let sweep_max_concurrent = store
            .get_config("sweep_max_concurrent")
            .await
            .ok()
            .flatten()
            .and_then(|e| e.value.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_SWEEP_MAX_CONCURRENT);

        // Metadata-refresh knobs (SKADI-T-0041), from the config plane.
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
            MovieWantedQuery::new(
                Arc::new(store.clone()),
                WantedScoring {
                    profile: default_profile.clone(),
                    // Off at startup (like `default_profile`); the real value loads on
                    // the first reconcile via `reload_profile` (SKADI-T-0187).
                    upgrade_until_format_score: 0,
                    regrab_unplayable: false,
                },
            )
            // Judge upgradability on each item's own profile (SKADI-T-0537).
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
            metadata_provider: std::sync::RwLock::new(None),
            breaker,
            refresh_interval,
            refresh_staleness,
        })
    }

    /// Install the metadata provider used by the periodic refresh worker
    /// (SKADI-T-0041). The daemon calls this after it builds the provider (which
    /// happens after the module is constructed); until then no refresh worker
    /// is spawned.
    pub fn set_metadata_provider(&self, provider: Arc<dyn MetadataProvider>) {
        *self
            .metadata_provider
            .write()
            .expect("metadata_provider lock poisoned") = Some(provider);
    }

    /// Resolve the active quality profile from the `profiles` settings and swap
    /// it into `scoring` + `wanted_query` (SKADI-T-0067). Falls back to
    /// [`Self::default_profile`] when none is configured or the stored profile
    /// is invalid. Called from [`apply`](skadi_api::ProviderReloader::apply).
    async fn reload_profile(&self) {
        let defs = default_definitions();
        let profile = resolve_active_profile(&self.store, &self.default_profile, &defs).await;
        // Load this domain's custom-format registry too (SKADI-T-0183) so a
        // `custom_formats` edit — or a profile changing its format scores — takes
        // effect on the same reconcile. Movies load only `domain == Movie` rows.
        let formats =
            skadi_hunter::services::resolve_custom_formats(&self.store, MediaKind::Movie).await;
        {
            let mut sc = self.scoring.write().expect("scoring lock poisoned");
            sc.profile = profile.clone();
            sc.formats = formats;
        }
        // Opt-in "upgrade until custom-format score" (SKADI-T-0187): re-read from
        // the active profile row so a settings change takes effect on this reconcile.
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
            MovieWantedQuery::new(
                Arc::new(self.store.clone()),
                WantedScoring {
                    profile,
                    upgrade_until_format_score,
                    regrab_unplayable,
                },
            )
            .with_store(self.store.clone()),
        );
        *self
            .wanted_query
            .write()
            .expect("wanted_query lock poisoned") = wq;
    }

    /// The module's Cloacina runner, for the manual-acquire endpoint
    /// (SKADI-T-0054). Shares the same runner the worker uses, so manual and
    /// swept runs go through one registry.
    pub fn runner(&self) -> Arc<cloacina::runner::DefaultRunner> {
        Arc::clone(&self.runner)
    }

    /// The module's backing store (for the HTTP module's repo access).
    pub fn store(&self) -> Store {
        self.store.clone()
    }

    fn build_services(&self) -> Arc<HunterServices> {
        let repo: Arc<dyn MoviesRepo> = Arc::new(self.store.clone());
        let providers = self.providers.read().expect("providers lock poisoned");
        let scoring = self.scoring.read().expect("scoring lock poisoned").clone();
        Arc::new(HunterServices {
            kind: skadi_core::MediaKind::Movie,
            store: self.store.clone(),
            status: Arc::new(MovieStatusSink::new(
                repo.clone(),
                Arc::new(self.store.clone()),
            )),
            indexers: providers.indexers.clone(),
            downloaders: providers.downloaders.clone(),
            // Static importer is a no-op fallback; the factory below is the
            // real path for movies.
            importer: Arc::new(DefaultImporter::new(NoMatcher)),
            importer_factory: Some(Arc::new(MovieImporterFactory {
                repo,
                config: self.store.clone(),
            })),
            notifiers: providers.notifiers.clone(),
            scoring,
        })
    }
}

/// The stored shape of a `profiles` settings row (SKADI-T-0067). The settings
/// row id is used as the [`ProfileId`]; `allowed`/`cutoff` reference quality
/// definition ids from `GET /quality/definitions`.
#[derive(serde::Deserialize)]
struct ProfileSpec {
    #[serde(default)]
    name: Option<String>,
    /// Allowed quality definition ids, ordered low→high (rank = index).
    allowed: Vec<Uuid>,
    /// At/above this quality, no further upgrades are pursued. Must be in `allowed`.
    cutoff: Uuid,
    #[serde(default)]
    upgrade_allowed: bool,
    #[serde(default)]
    min_format_score: i32,
    /// Per-custom-format score assignments (SKADI-T-0183): each references a
    /// `custom_formats` row id and the score that format contributes. `default`
    /// (empty) for profiles written before custom formats existed — preserving
    /// the no-format-scoring behavior until the operator assigns scores.
    #[serde(default)]
    formats: Vec<skadi_quality::CustomFormatScore>,
    /// Opt-in "upgrade until custom-format score" (SKADI-T-0187): `> 0` makes the
    /// sweep re-check at/above-quality-cutoff editions for a higher-format-score
    /// Proper/Repack. `default` (`0`) disables it (no at-cutoff re-search).
    #[serde(default)]
    upgrade_until_format_score: i32,
}

/// Read the opt-in `upgrade_until_format_score` (SKADI-T-0187) from the active
/// (first) `profiles` row. `0` (off) on any missing/malformed row — so a fresh
/// install never re-searches imported editions for propers.
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

/// Read the active quality profile from the `profiles` settings, mapping it onto
/// the built-in quality definitions. The **first** profile row is the active
/// one (v0 single global scoring profile). Returns `default` when none is
/// configured or the stored profile is malformed / references unknown qualities
/// — so behavior is unchanged until the user sets a valid profile.
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
        tracing::warn!(
            id = %row.id,
            "profile references unknown qualities or cutoff not in allowed; using default profile"
        );
        return default.clone();
    }
    let id = Uuid::parse_str(&row.id)
        .map(skadi_core::ProfileId::from)
        .unwrap_or(default.id);
    let profile = QualityProfile {
        id,
        name: spec.name.unwrap_or_else(|| "profile".into()),
        allowed: spec
            .allowed
            .into_iter()
            .map(skadi_core::QualityId::from)
            .collect(),
        cutoff: skadi_core::QualityId::from(spec.cutoff),
        upgrade_allowed: spec.upgrade_allowed,
        // Per-profile custom-format score assignments (SKADI-T-0183). The format
        // *definitions* these reference are loaded separately into
        // `ScoringConfig.formats` via `resolve_custom_formats`.
        formats: spec.formats,
        min_format_score: spec.min_format_score,
    };
    tracing::info!(name = %profile.name, allowed = profile.allowed.len(), "active quality profile resolved from settings");
    profile
}

/// Live provider reload (SKADI-T-0062): swap the providers and re-publish the
/// hunter's global services registry. The pipeline reads `services()` fresh at
/// every workflow stage, so the next stage of any in-flight run — and the next
/// sweep — sees the new providers. Domain-specific pieces (`status` sink,
/// `importer_factory`, scoring) are preserved because `build_services()`
/// reassembles them from the module's own state.
#[async_trait]
impl skadi_api::ProviderReloader for MoviesModule {
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
        // Re-resolve the active quality profile too (SKADI-T-0067), so a
        // `profiles` change takes effect on the same reconcile.
        self.reload_profile().await;
        skadi_hunter::set_services(self.build_services());
        Ok(())
    }
}

impl DomainModule for MoviesModule {
    fn name(&self) -> &'static str {
        "movies"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
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

        // Metadata-refresh worker (SKADI-T-0041), only once a provider is set.
        if let Some(provider) = self
            .metadata_provider
            .read()
            .expect("metadata_provider lock poisoned")
            .clone()
        {
            let refresh_services = Arc::new(crate::refresh::RefreshServices {
                provider,
                repo: Arc::new(self.store.clone()),
                breaker: Arc::clone(&self.breaker),
            });
            workers.push(Box::new(crate::refresh::MovieRefreshWorker::new(
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
            vec![Arc::new(crate::scan::MovieScanSource::new(
                self.store.clone(),
            ))],
            Arc::new(skadi_library_scan::DefaultProber),
        )));
        workers
    }
}

/// A trivial `Worker` shim that owns its `Arc<DefaultRunner>` clone, so each
/// `workers()` call yields an independent boxed worker without cloning the
/// runner type itself (`DefaultRunner` is not `Clone`; the `Arc` is).
struct SpawnedHunterWorker {
    services: Arc<HunterServices>,
    runner: Arc<cloacina::runner::DefaultRunner>,
    interval: Duration,
    query: Arc<dyn WantedQuery>,
    max_concurrent: usize,
}

impl Worker for SpawnedHunterWorker {
    fn name(&self) -> &str {
        "movies-hunter"
    }
    fn run(
        self: Box<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        // `HunterWorker` shares the runner via `Arc` (SKADI-T-0056), so the same
        // runner backs both this worker and the module's HTTP manual-acquire
        // endpoint. No ownership dance, no `try_unwrap`, no panic on re-enable —
        // which resolves the v0 lifecycle limitation flagged in T-0049/T-0052.
        // Enable the RSS fast pass (SKADI-T-0192): a short-cadence recent-feed pull
        // that triggers acquisitions for wanted movies within minutes, alongside
        // the full sweep.
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

struct MovieImporterFactory {
    repo: Arc<dyn MoviesRepo>,
    /// Source for operator naming-template config (SKADI-T-0227); the `Store` is also a
    /// `ConfigRepo`.
    config: Store,
}

impl MovieImporterFactory {
    /// Resolve the operator's movie naming templates from the `config` table, falling
    /// back to the built-in defaults if the table is unreadable.
    async fn naming(&self) -> crate::naming::MovieNaming {
        use skadi_store::ConfigRepo;
        match self.config.list_config().await {
            Ok(entries) => {
                let view = skadi_config::ConfigView::from_pairs(
                    entries.into_iter().map(|e| (e.key, e.value)),
                );
                crate::naming::MovieNaming::from_view(&view)
            }
            Err(_) => crate::naming::MovieNaming::default(),
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
impl ImporterFactory for MovieImporterFactory {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> Result<Arc<dyn Importer>> {
        let edition_id = Uuid::parse_str(&acquirable.0)
            .map(skadi_core::MovieEditionId::from)
            .map_err(|e| AppError::Validation(format!("bad acquirable ref: {e}")))?;
        let edition =
            self.repo.get_edition(edition_id).await?.ok_or_else(|| {
                AppError::NotFound(format!("MovieEdition {} not found", edition_id))
            })?;
        let movie = self
            .repo
            .get_movie(edition.movie_id)
            .await?
            .ok_or_else(|| {
                AppError::NotFound(format!("Movie {} not found for edition", edition.movie_id))
            })?;
        let kinds = self.repo.list_edition_kinds().await?;
        let matcher = MovieMatcher::with_naming(
            movie.clone(),
            movie.editions.clone(),
            kinds,
            self.naming().await,
        );
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

/// Fallback matcher used in [`HunterServices::importer`] when no factory is
/// set; movies *always* sets a factory, so this never matches.
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

#[cfg(test)]
mod profile_tests {
    use super::*;
    use skadi_quality::{Decision, Quality, QualityProfile};
    use skadi_store::Store;

    /// A permissive default over all definitions (matches the daemon's startup
    /// profile): the fallback used when no profile is configured.
    fn permissive_default(defs: &[skadi_quality::QualityDefinition]) -> QualityProfile {
        let allowed: Vec<_> = defs.iter().map(|d| d.id).collect();
        let cutoff = *allowed.last().unwrap();
        QualityProfile {
            id: skadi_core::ProfileId::new(),
            name: "default".into(),
            allowed,
            cutoff,
            upgrade_allowed: false,
            formats: vec![],
            min_format_score: 0,
        }
    }

    /// Turn a definition into a candidate [`Quality`] for `decide`.
    fn candidate(d: &skadi_quality::QualityDefinition) -> Quality {
        Quality {
            id: d.id,
            resolution: d.resolution,
            source: d.source,
            codec: None,
            modifier: None,
        }
    }

    async fn store() -> Store {
        let path = skadi_core::unique_temp_path("profile").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn no_profile_row_falls_back_to_default() {
        let defs = default_definitions();
        let default = permissive_default(&defs);
        let resolved = resolve_active_profile(&store().await, &default, &defs).await;
        assert_eq!(resolved.allowed.len(), defs.len());
        assert_eq!(resolved.id, default.id);
    }

    #[tokio::test]
    async fn stored_1080p_cap_rejects_uhd() {
        let defs = default_definitions();
        let default = permissive_default(&defs);
        // Everything up to and including "Bluray-1080p" (index 9); 2160p excluded.
        let cap_idx = defs.iter().position(|d| d.name == "Bluray-1080p").unwrap();
        let allowed: Vec<String> = defs[..=cap_idx].iter().map(|d| d.id.to_string()).collect();
        let cutoff = defs[cap_idx].id.to_string();

        let store = store().await;
        let id = uuid::Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "name": "1080p",
            "allowed": allowed,
            "cutoff": cutoff,
            "upgrade_allowed": false,
            "min_format_score": 0
        });
        store.put_setting("profiles", &id, &body).await.unwrap();

        let resolved = resolve_active_profile(&store, &default, &defs).await;
        assert_eq!(resolved.name, "1080p");
        assert_eq!(resolved.allowed.len(), cap_idx + 1);
        // Row id becomes the ProfileId.
        assert_eq!(resolved.id.to_string(), id);

        // A UHD (Bluray-2160p) candidate is rejected; a 1080p one is accepted.
        let uhd = candidate(defs.iter().find(|d| d.name == "Bluray-2160p").unwrap());
        let hd = candidate(&defs[cap_idx]);
        assert_eq!(resolved.decide(&uhd, None), Decision::Reject);
        assert_eq!(resolved.decide(&hd, None), Decision::Accept);
    }

    #[tokio::test]
    async fn profile_custom_format_scores_round_trip(/* SKADI-T-0183 */) {
        let defs = default_definitions();
        let default = permissive_default(&defs);
        let store = store().await;
        let fmt = skadi_core::CustomFormatId::new();
        let body = serde_json::json!({
            "name": "with-formats",
            "allowed": [defs[0].id.to_string()],
            "cutoff": defs[0].id.to_string(),
            "formats": [{ "format": fmt.to_string(), "score": 75 }],
        });
        store
            .put_setting("profiles", &uuid::Uuid::new_v4().to_string(), &body)
            .await
            .unwrap();
        let resolved = resolve_active_profile(&store, &default, &defs).await;
        // The per-profile format score assignment loads (no longer dropped).
        assert_eq!(resolved.formats.len(), 1);
        assert_eq!(resolved.formats[0].format, fmt);
        assert_eq!(resolved.formats[0].score, 75);
    }

    #[tokio::test]
    async fn invalid_profile_falls_back_to_default() {
        let defs = default_definitions();
        let default = permissive_default(&defs);
        let store = store().await;
        let id = uuid::Uuid::new_v4().to_string();
        // cutoff not in allowed → invalid → default.
        let body = serde_json::json!({
            "name": "broken",
            "allowed": [defs[0].id.to_string()],
            "cutoff": defs[5].id.to_string()
        });
        store.put_setting("profiles", &id, &body).await.unwrap();
        let resolved = resolve_active_profile(&store, &default, &defs).await;
        assert_eq!(resolved.id, default.id, "invalid profile should fall back");
    }
}
