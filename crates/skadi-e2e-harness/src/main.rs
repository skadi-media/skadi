//! Test-only daemon for the Playwright web E2E (SKADI-T-0120).
//!
//! Boots `skadi-api` exactly as the real daemon does, but installs MOCK hunter
//! services (a one-shot indexer + an immediate downloader + a fake TMDB) so the
//! whole add → search → grab → import flow runs deterministically offline. With
//! `--features embed-ui` it serves the real built UI at `/`, which Playwright
//! drives in a headless browser. Env:
//! - `SKADI_E2E_PORT`    (default 8191)
//! - `SKADI_E2E_WORKDIR` (default /tmp/skadi-e2e) — DB + library + the fixture
//!   `.mkv` the immediate-downloader "completes"; the spec points a root folder
//!   at `<workdir>/library`.

mod mocks;

use std::path::PathBuf;
use std::sync::Arc;

use diesel::connection::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::MigrationHarness;

use std::time::Duration;

use skadi_api::{AppState, Config, DomainDescriptor, HttpModule, LibraryProvider};
use skadi_audiobooks::{
    AudiobookStatusSink, AudiobooksHttp, AudiobooksModule, AudiobooksRepo,
    default_audiobook_profile,
};
use skadi_core::{DownloaderId, IndexerId, MediaKind, NotifierId, ProfileId};
use skadi_http::HttpClient;
use skadi_hunter::services::{AudiobookScoring, ScoringConfig};
use skadi_hunter::{
    HunterServices, ImporterFactory, build_runner_for, cloacina_target_for, set_services,
};
use skadi_indexers::{Release, ReleaseFetch};
use skadi_metadata::{AudnexusProvider, MetadataProvider};
use skadi_movies::{
    MovieStatusSink, MoviesHttp, MoviesLibrary, MoviesModule, MoviesRepo, SQLITE_MIGRATIONS,
    SharedHunterDeps,
};
use skadi_quality::audiobook::default_audiobook_definitions;
use skadi_quality::{QualityProfile, default_definitions, parse, parse_audiobook};
use skadi_store::Store;

use mocks::{
    AudiobookImporterFactoryForTest, FakeTmdb, ImmediateDownloader, MovieImporterFactoryForTest,
    NoopNotifier, OneShotIndexer, default_importer_noop, spawn_fake_audnexus,
};

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt().try_init();

    let port: u16 = std::env::var("SKADI_E2E_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8191);
    let workdir = PathBuf::from(
        std::env::var("SKADI_E2E_WORKDIR").unwrap_or_else(|_| "/tmp/skadi-e2e".into()),
    );
    std::fs::create_dir_all(&workdir).expect("create workdir");

    // Fixture file the immediate-downloader reports complete. Padded past the
    // matcher's ~50 MB "is this a sample?" threshold so it imports.
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let src = workdir.join(format!("{title}.mkv"));
    std::fs::write(&src, vec![0u8; 52 * 1024 * 1024]).expect("write fixture mkv");
    let library = workdir.join("library");
    std::fs::create_dir_all(&library).expect("create library");

    // Audiobook fixture: a small `.m4b` the immediate-downloader "completes". The
    // audiobook matcher has no sample-size threshold, so a tiny file imports.
    let ab_title = "Andy Weir - Project Hail Mary (2021) [M4B 128kbps]";
    let ab_src = workdir.join("Andy Weir - Project Hail Mary.m4b");
    std::fs::write(&ab_src, vec![0u8; 4 * 1024 * 1024]).expect("write fixture m4b");

    let db_path = workdir.join("skadi.db");
    let skadi_url = format!("sqlite://{}", db_path.display());

    // Migrations: skadi-store, then the movies + audiobooks domains' embedded sets.
    let store = Store::connect(&skadi_url).expect("connect store");
    store.run_migrations().await.expect("store migrations");
    drop(store);
    {
        let mut conn =
            SqliteConnection::establish(&db_path.display().to_string()).expect("sqlite connect");
        conn.run_pending_migrations(SQLITE_MIGRATIONS)
            .expect("movies migrations");
        conn.run_pending_migrations(skadi_audiobooks::SQLITE_MIGRATIONS)
            .expect("audiobooks migrations");
    }
    let store = Store::connect(&skadi_url).expect("reconnect store");

    // Permissive scoring profile (Bluray-720p + Bluray-1080p, cutoff 1080p).
    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
    let scoring_profile = QualityProfile {
        id: ProfileId::new(),
        name: "e2e".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // One SHARED Cloacina runner handed to BOTH domain modules (SKADI-T-0136), so
    // movies (kind=Movie) and audiobooks (kind=Audiobook) dispatch through one
    // runner / `hunter.db` instead of each module building its own (which would
    // collide on the same sibling file).
    let target = cloacina_target_for(&skadi_url).expect("cloacina target");
    let runner = Arc::new(
        build_runner_for(&target)
            .await
            .expect("build shared runner"),
    );

    // Movies module against the shared runner.
    let module = Arc::new(
        MoviesModule::with_runner(
            store.clone(),
            runner.clone(),
            SharedHunterDeps {
                indexers: vec![],
                downloaders: vec![],
                notifiers: vec![],
                scoring: ScoringConfig {
                    definitions: defs.clone(),
                    profile: scoring_profile.clone(),
                    formats: vec![],
                    min_seeders: 0,
                    audiobook: None,
                },
            },
        )
        .await
        .expect("build movies module"),
    );

    // Audiobooks module against the SAME shared runner. The audiobook scoring axis
    // (`audiobook: Some(..)`) + the permissive default audiobook profile.
    let ab_profile = default_audiobook_profile();
    let ab_scoring = ScoringConfig {
        definitions: vec![],
        profile: ab_profile.clone(),
        formats: vec![],
        min_seeders: 0,
        audiobook: Some(AudiobookScoring {
            definitions: default_audiobook_definitions(),
            allow_abridged: false,
        }),
    };
    let ab_module = Arc::new(
        AudiobooksModule::with_runner(
            store.clone(),
            runner.clone(),
            skadi_audiobooks::SharedHunterDeps {
                indexers: vec![],
                downloaders: vec![],
                notifiers: vec![],
                scoring: ab_scoring.clone(),
            },
        )
        .await
        .expect("build audiobooks module"),
    );

    // Install mock services into the process-global. No supervisor here — it
    // would clobber these with the module's empty-provider services.
    let repo_arc: Arc<dyn MoviesRepo> = Arc::new(store.clone());
    let factory: Arc<dyn ImporterFactory> = Arc::new(MovieImporterFactoryForTest {
        repo: repo_arc.clone(),
    });
    let svc = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: Arc::new(MovieStatusSink::new(
            repo_arc.clone(),
            Arc::new(store.clone()),
        )),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            kind: MediaKind::Movie,
            release: Release {
                indexer: IndexerId::new(),
                title: title.into(),
                fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
                size: 8_000_000_000,
                published: chrono::Utc::now(),
                seeders: Some(42),
                categories: Vec::new(),
                parsed: parse(title),
            },
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
        })],
        importer: default_importer_noop(),
        importer_factory: Some(factory),
        notifiers: vec![Arc::new(NoopNotifier {
            id: NotifierId::new(),
        })],
        scoring: ScoringConfig {
            definitions: defs,
            profile: scoring_profile,
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    // Mock audiobook services (kind=Audiobook) — coexist with movies in the
    // per-kind registry (SKADI-T-0136): `set_services` keys by `services.kind`.
    let ab_repo: Arc<dyn AudiobooksRepo> = Arc::new(store.clone());
    let ab_factory: Arc<dyn ImporterFactory> = Arc::new(AudiobookImporterFactoryForTest {
        repo: ab_repo.clone(),
    });
    let ab_svc = Arc::new(HunterServices {
        kind: MediaKind::Audiobook,
        store: store.clone(),
        status: Arc::new(AudiobookStatusSink::new(
            ab_repo.clone(),
            Arc::new(store.clone()),
        )),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            kind: MediaKind::Audiobook,
            release: Release {
                indexer: IndexerId::new(),
                title: ab_title.into(),
                fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:def".into()),
                size: 350_000_000,
                published: chrono::Utc::now(),
                seeders: Some(42),
                categories: Vec::new(),
                parsed: parse_audiobook(ab_title),
            },
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![ab_src.clone()],
        })],
        importer: default_importer_noop(),
        importer_factory: Some(ab_factory),
        notifiers: vec![Arc::new(NoopNotifier {
            id: NotifierId::new(),
        })],
        scoring: ab_scoring,
    });

    // Reset once, then install BOTH kinds' services (keyed by kind, they coexist).
    skadi_hunter::services::reset_services();
    set_services(svc);
    set_services(ab_svc);

    // HTTP surface, exactly as the daemon builds it.
    let provider: Arc<dyn MetadataProvider> = Arc::new(FakeTmdb);
    let movies_http = MoviesHttp::new(module.store(), provider, module.runner());
    let movies_library: Arc<dyn LibraryProvider> = Arc::new(MoviesLibrary::new(module.store()));

    // Audiobooks HTTP: a real `AudnexusProvider` pointed at an in-process mock of
    // the Audnexus API. `AudiobooksHttp` requires the concrete provider type (its
    // author routes call inherent methods), and the add-by-ASIN flow only needs a
    // working `lookup`, so a scripted stub server is the simplest offline answer.
    let audnexus_url = spawn_fake_audnexus().await;
    let audnexus = Arc::new(
        AudnexusProvider::new(HttpClient::new(Duration::from_secs(5)).expect("http client"))
            .with_base_url(audnexus_url),
    );
    // No live Audible catalog in the mock-provider harness → title search 503s.
    let audiobooks_http =
        AudiobooksHttp::new(ab_module.store(), audnexus, None, ab_module.runner());

    let config = Config {
        database_url: skadi_url.clone(),
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        bearer_token: None,
    };
    let mut state = AppState::new_full(
        config,
        Some(store.clone()),
        vec![
            DomainDescriptor {
                name: "movies".into(),
                kind: MediaKind::Movie,
            },
            DomainDescriptor {
                name: "audiobooks".into(),
                kind: MediaKind::Audiobook,
            },
        ],
        // No audiobooks `LibraryProvider` exists in the crate; movies is the only
        // unified-library contributor. The audiobooks spec asserts state via the
        // book detail page + `GET /books`, not the unified `/library`.
        vec![movies_library],
    );
    // The domain sets applied above, so the `database` health check expects
    // them (SKADI-T-0682).
    let mut domain_migrations = store
        .migration_versions(SQLITE_MIGRATIONS, skadi_movies::POSTGRES_MIGRATIONS)
        .expect("movies migration versions");
    domain_migrations.extend(
        store
            .migration_versions(
                skadi_audiobooks::SQLITE_MIGRATIONS,
                skadi_audiobooks::POSTGRES_MIGRATIONS,
            )
            .expect("audiobooks migration versions"),
    );
    Arc::get_mut(&mut state)
        .expect("AppState not yet shared")
        .domain_migrations = domain_migrations.into();
    let state = state;
    let routes = HttpModule::routes(&movies_http);
    let ab_routes = HttpModule::routes(&audiobooks_http);

    eprintln!(
        "skadi-e2e-harness: http://127.0.0.1:{port}  library={}",
        library.display()
    );
    skadi_api::serve(state, vec![routes, ab_routes])
        .await
        .expect("serve");
}
