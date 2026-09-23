//! End-to-end test for the movies domain (SKADI-T-0049).
//!
//! Drives the real `acquire` workflow against a real Cloacina runner with
//! mock indexer / downloader / notifier / real `DefaultImporter` (via the
//! `MovieImporterFactory`) — from a seeded Missing movie edition to
//! `Imported` with a file placed in the library and the status persisted on
//! the `movie_editions` row.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use diesel::connection::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::MigrationHarness;

use skadi_core::{
    AcquisitionStatus, DownloaderId, EditionKindId, ExternalIds, IndexerId, MediaKind, NotifierId,
    ProfileId, Protocol, Result as SkadiResult, RootFolder, TmdbId,
};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_hunter::{
    AcquireSeed, DEFAULT_SWEEP_MAX_CONCURRENT, services::ScoringConfig, set_services,
    start_acquire, sweep_once,
};
use skadi_indexers::{
    Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};
use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
use skadi_quality::{QualityProfile, default_definitions, parse};

use skadi_movies::{
    Movie, MovieEdition, MovieWantedQuery, MoviesRepo, SQLITE_MIGRATIONS, THEATRICAL_KIND_ID,
    WantedScoring,
};
use skadi_store::Store;

// --- mocks (mirror skadi-hunter's e2e scaffolding) ---

struct OneShotIndexer {
    id: IndexerId,
    release: Release,
}

#[async_trait]
impl Indexer for OneShotIndexer {
    fn id(&self) -> IndexerId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn capabilities(&self) -> SkadiResult<IndexerCaps> {
        Ok(IndexerCaps {
            supports_rss: true,
            supports_search: true,
            id_params: std::collections::BTreeSet::new(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: vec![],
        })
    }
    async fn search(&self, _query: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
        Ok(vec![self.release.clone()])
    }
}

struct ImmediateDownloader {
    id: DownloaderId,
    completed: Vec<std::path::PathBuf>,
}

#[async_trait]
impl Downloader for ImmediateDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, _r: &Release, c: &Category) -> SkadiResult<DownloadHandle> {
        Ok(DownloadHandle {
            native_id: "test-handle".into(),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        Ok(DownloadStatus::Completed {
            files: self.completed.clone(),
        })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

struct RecordingNotifier {
    id: NotifierId,
    seen: Arc<Mutex<Vec<NotificationEvent>>>,
}
#[async_trait]
impl Notifier for RecordingNotifier {
    fn id(&self) -> NotifierId {
        self.id
    }
    fn channels(&self) -> &[NotificationKind] {
        &[NotificationKind::Imported]
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn notify(&self, event: &NotificationEvent) -> SkadiResult<()> {
        self.seen.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn movies_module_drives_a_full_acquire_run_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv");
    // Pad past the matcher's 50 MB "is this a sample?" threshold.
    std::fs::write(&src, vec![0u8; 52 * 1024 * 1024]).unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // skadi-store + skadi-movies migrations.
    let store = Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);
    {
        let mut conn =
            SqliteConnection::establish(&dir.path().join("skadi.db").display().to_string())
                .unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
    }
    let store = Store::connect(&skadi_url).unwrap();

    // Scoring: profile that allows Bluray-720p + Bluray-1080p (cutoff 1080p).
    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "e2e".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // Seed: a monitored movie with a Missing Theatrical edition.
    let movie = Movie {
        content_rating: None,
        genres: Vec::new(),
        id: skadi_core::MovieId::new(),
        external_ids: ExternalIds {
            tmdb: Some(TmdbId(603)),
            ..Default::default()
        },
        title: "The Matrix".into(),
        original_title: None,
        year: Some(1999),
        overview: None,
        runtime_minutes: None,
        poster_url: None,
        backdrop_url: None,
        collection: None,
        monitored: true,
        profile: profile.id,
        root_folder: RootFolder {
            id: skadi_core::RootFolderId::new(),
            path: library.clone(),
        },
        added_at: Utc::now(),
        last_metadata_refresh: None,
        editions: vec![],
    };
    store.upsert_movie(&movie).await.unwrap();
    let edition = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
    store.upsert_edition(&edition).await.unwrap();

    // The single Bluray-1080p release the mock indexer returns. (The actual
    // `Release` value gets cloned into the indexer below; this `title` is
    // shared with the SearchSpec seed at the bottom of the test.)
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));

    // **v0 e2e shape**: rather than going through `MoviesModule::workers()` and
    // its sweep cadence (which would need the I-0008 daemon's lifecycle to
    // exercise honestly), assemble HunterServices directly with the same
    // movies pieces the module would have wired, then drive a single
    // `start_acquire`. This proves the integration end-to-end without
    // double-building a runner on the same hunter.db.
    use skadi_hunter::{ImporterFactory, build_runner};
    let runner = build_runner(&skadi_url).await.unwrap();
    // Re-construct services to install via set_services for the workflow.
    let repo_arc: Arc<dyn MoviesRepo> = Arc::new(store.clone());
    let factory: Arc<dyn ImporterFactory> = Arc::new(MovieImporterFactoryForTest {
        repo: repo_arc.clone(),
    });
    let svc = Arc::new(skadi_hunter::HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: Arc::new(skadi_movies::MovieStatusSink::new(
            repo_arc.clone(),
            Arc::new(store.clone()),
        )),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release: Release {
                indexer: IndexerId::new(),
                title: title.into(),
                fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
                size: 8_000_000_000,
                published: Utc::now(),
                seeders: Some(42),
                categories: Vec::new(),
                parsed: parse(title),
            },
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
        })],
        importer: Arc::new(skadi_importer::DefaultImporter::new(NoopMatcher)),
        importer_factory: Some(factory),
        notifiers: vec![Arc::new(RecordingNotifier {
            id: NotifierId::new(),
            seen: recorder.clone(),
        })],
        scoring: ScoringConfig {
            definitions: default_definitions(),
            profile: skadi_quality::QualityProfile {
                id: skadi_core::ProfileId::new(),
                name: "e2e".into(),
                allowed: vec![lo, hi],
                cutoff: hi,
                upgrade_allowed: false,
                formats: vec![],
                min_format_score: 0,
            },
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    skadi_hunter::services::reset_services();
    set_services(svc);

    // Drive one acquire run for this edition.
    let seed = AcquireSeed {
        acquirable: edition.acquirable_ref(),
        request: skadi_hunter::SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["The Matrix".into()],
            year: Some(1999),
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: movie.profile,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let skadi_hunter::AcquireOutcome::Started(result) =
        start_acquire(&runner, seed).await.expect("execute")
    else {
        panic!("expected Started");
    };
    assert!(
        matches!(result.status, cloacina::WorkflowStatus::Completed),
        "expected Completed, got {:?} (error: {:?})",
        result.status,
        result.error_message
    );

    // Verify: the edition row in the repo ended Imported, the file is in the
    // library, and the recording notifier got exactly one Imported event.
    let row = store.get_edition(edition.id).await.unwrap().unwrap();
    assert!(
        matches!(row.status, AcquisitionStatus::Imported { .. }),
        "expected Imported, got {:?}",
        row.status
    );
    assert!(row.file.is_some(), "Imported edition has file");
    assert!(
        library
            .join("the-matrix_(1999)_{tmdb-603}/theatrical/the-matrix_(1999).mkv")
            .exists(),
        "file placed at the canonical destination"
    );
    let events = recorder.lock().unwrap().len();
    assert_eq!(events, 1);

    runner.shutdown().await.unwrap();
}

/// SKADI-T-0036 (reframed): a real crash → daemon-owned recovery → re-acquire
/// loop. An edition left wedged in a non-terminal acquire state past
/// `STALE_ACQUIRE_GRACE` (simulating a daemon crash that cleared the in-flight
/// tracker while the DB still shows it Downloading) is reset to Missing by
/// `reconcile_stale` at the top of a sweep, and **the same sweep re-acquires it**
/// to Imported. Proves T-0112's recovery end-to-end through `sweep_once`.
#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn wedged_edition_is_recovered_and_reacquired_by_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, vec![0u8; 52 * 1024 * 1024]).unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let store = Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);
    {
        let mut conn =
            SqliteConnection::establish(&dir.path().join("skadi.db").display().to_string())
                .unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
    }
    let store = Store::connect(&skadi_url).unwrap();

    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "recover".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // A monitored movie whose Theatrical edition is WEDGED: Downloading, but its
    // updated_at is well past the grace and no run is in the (empty) tracker.
    let movie = Movie {
        content_rating: None,
        genres: Vec::new(),
        id: skadi_core::MovieId::new(),
        external_ids: ExternalIds {
            tmdb: Some(TmdbId(603)),
            ..Default::default()
        },
        title: "The Matrix".into(),
        original_title: None,
        year: Some(1999),
        overview: None,
        runtime_minutes: None,
        poster_url: None,
        backdrop_url: None,
        collection: None,
        monitored: true,
        profile: profile.id,
        root_folder: RootFolder {
            id: skadi_core::RootFolderId::new(),
            path: library.clone(),
        },
        added_at: Utc::now(),
        last_metadata_refresh: None,
        editions: vec![],
    };
    store.upsert_movie(&movie).await.unwrap();
    let mut edition = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
    edition.status = AcquisitionStatus::Downloading {
        release: skadi_core::ReleaseId::new(),
        progress: 0.4,
    };
    edition.updated_at = Utc::now()
        - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE).unwrap()
        - chrono::Duration::minutes(5);
    store.upsert_edition(&edition).await.unwrap();

    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    use skadi_hunter::{ImporterFactory, build_runner};
    let runner = build_runner(&skadi_url).await.unwrap();
    let repo_arc: Arc<dyn MoviesRepo> = Arc::new(store.clone());
    let factory: Arc<dyn ImporterFactory> = Arc::new(MovieImporterFactoryForTest {
        repo: repo_arc.clone(),
    });
    let svc = Arc::new(skadi_hunter::HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: Arc::new(skadi_movies::MovieStatusSink::new(
            repo_arc.clone(),
            Arc::new(store.clone()),
        )),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release: Release {
                indexer: IndexerId::new(),
                title: title.into(),
                fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
                size: 8_000_000_000,
                published: Utc::now(),
                seeders: Some(42),
                categories: Vec::new(),
                parsed: parse(title),
            },
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
        })],
        importer: Arc::new(skadi_importer::DefaultImporter::new(NoopMatcher)),
        importer_factory: Some(factory),
        notifiers: vec![Arc::new(RecordingNotifier {
            id: NotifierId::new(),
            seen: recorder.clone(),
        })],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    skadi_hunter::services::reset_services();
    set_services(svc);

    // One sweep: reconcile_stale resets the wedged edition to Missing, then
    // wanted() (same tick) re-acquires it to Imported.
    let query = MovieWantedQuery::new(
        Arc::new(store.clone()),
        WantedScoring {
            profile,
            upgrade_until_format_score: 0,
            regrab_unplayable: false,
        },
    );
    let started = sweep_once(&runner, &query, DEFAULT_SWEEP_MAX_CONCURRENT)
        .await
        .unwrap();
    assert_eq!(started, 1, "the recovered edition was re-acquired");

    let row = store.get_edition(edition.id).await.unwrap().unwrap();
    assert!(
        matches!(row.status, AcquisitionStatus::Imported { .. }),
        "wedged edition recovered to Imported, got {:?}",
        row.status
    );
    assert_eq!(recorder.lock().unwrap().len(), 1, "one Imported notify");

    runner.shutdown().await.unwrap();
}

// --- helpers ---

struct MovieImporterFactoryForTest {
    repo: Arc<dyn MoviesRepo>,
}

#[async_trait]
impl skadi_hunter::ImporterFactory for MovieImporterFactoryForTest {
    async fn for_acquirable(
        &self,
        acquirable: &skadi_importer::AcquirableRef,
    ) -> SkadiResult<Arc<dyn skadi_importer::Importer>> {
        // Mirrors the production MovieImporterFactory in src/module.rs.
        let edition_id = uuid::Uuid::parse_str(&acquirable.0)
            .map(skadi_core::MovieEditionId::from)
            .map_err(|e| skadi_core::AppError::Validation(format!("bad ref: {e}")))?;
        let edition = self.repo.get_edition(edition_id).await?.unwrap();
        let movie = self.repo.get_movie(edition.movie_id).await?.unwrap();
        let kinds = self.repo.list_edition_kinds().await?;
        let matcher = skadi_movies::MovieMatcher::new(movie.clone(), movie.editions.clone(), kinds);
        Ok(Arc::new(skadi_importer::DefaultImporter::new(matcher)))
    }
}

struct NoopMatcher;
impl skadi_importer::AcquirableMatcher for NoopMatcher {
    fn match_file(
        &self,
        _p: &skadi_quality::ParsedRelease,
        _s: &std::path::Path,
        _c: &skadi_importer::CompletedDownload,
    ) -> Vec<skadi_importer::AcquirableMatch> {
        Vec::new()
    }
}
