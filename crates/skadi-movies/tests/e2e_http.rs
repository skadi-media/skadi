//! Capstone end-to-end test through the HTTP boundary (SKADI-T-0057).
//!
//! Boots a real `skadi-api` server in-process (not the binary — so we can inject
//! mock hunter services via the process-global), then configures and drives it
//! **entirely through `skadi-client` over HTTP**, exactly as the CLI/a user
//! would: create a profile + root folder, enable the movies domain, add a movie
//! (TMDB lookup mocked), trigger a manual acquire, and poll until the edition
//! reaches `imported` with the file placed in the library.
//!
//! The acquire pipeline (indexer → downloader → importer → notifier) is mocked
//! the same way as the T-0049 domain e2e; what's new here is that *every*
//! interaction goes through the API surface and the typed client.
//!
//! Single-threaded: relies on the process-global hunter services + in-flight
//! tracker, so it owns its own test binary and runs one test.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use diesel::connection::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::MigrationHarness;

use skadi_api::{AppState, Config, DomainDescriptor, HttpModule};
use skadi_client::Client;
use skadi_core::{
    DownloaderId, ExternalIds, ImdbId, IndexerId, MediaKind, NotifierId, Protocol,
    Result as SkadiResult, TmdbId,
};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_hunter::services::ScoringConfig;
use skadi_hunter::{HunterServices, ImporterFactory, set_services};
use skadi_indexers::{
    Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
use skadi_quality::{QualityProfile, default_definitions, parse};
use skadi_store::{ConfigRepo, ConfigSource, Store};

use skadi_movies::{
    MovieMatcher, MovieStatusSink, MoviesHttp, MoviesLibrary, MoviesModule, MoviesRepo,
    SQLITE_MIGRATIONS, SharedHunterDeps,
};

// --- mocks (mirror the T-0049 domain e2e) ---

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
        // Wants both so the test can assert Grabbed precedes Imported (SKADI-T-0037).
        &[NotificationKind::Grabbed, NotificationKind::Imported]
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn notify(&self, event: &NotificationEvent) -> SkadiResult<()> {
        self.seen.lock().unwrap().push(event.clone());
        Ok(())
    }
}

struct MovieImporterFactoryForTest {
    repo: Arc<dyn MoviesRepo>,
}

#[async_trait]
impl ImporterFactory for MovieImporterFactoryForTest {
    async fn for_acquirable(
        &self,
        acquirable: &skadi_importer::AcquirableRef,
    ) -> SkadiResult<Arc<dyn skadi_importer::Importer>> {
        let edition_id = uuid::Uuid::parse_str(&acquirable.0)
            .map(skadi_core::MovieEditionId::from)
            .map_err(|e| skadi_core::AppError::Validation(format!("bad ref: {e}")))?;
        let edition = self.repo.get_edition(edition_id).await?.unwrap();
        let movie = self.repo.get_movie(edition.movie_id).await?.unwrap();
        let kinds = self.repo.list_edition_kinds().await?;
        let matcher = MovieMatcher::new(movie.clone(), movie.editions.clone(), kinds);
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

/// A TMDB provider stub returning a canned Matrix record for any lookup.
struct FakeTmdb;

#[async_trait]
impl MetadataProvider for FakeTmdb {
    fn name(&self) -> &str {
        "fake-tmdb"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn search(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, _id: &ExternalId) -> SkadiResult<MetadataRecord> {
        Ok(MetadataRecord {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                imdb: Some(ImdbId("tt0133093".into())),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: Some("The Matrix".into()),
            overview: Some("A hacker learns the truth.".into()),
            runtime_minutes: Some(136),
            release_date: chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![],
            ..Default::default()
        })
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn configure_via_api_add_movie_and_observe_acquire_to_imported() {
    let dir = tempfile::tempdir().unwrap();
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

    drive_e2e(&skadi_url, store, dir.path()).await;
}

/// Postgres-gated counterpart (`#[ignore]` by default; run via `angreal db test`
/// with `SKADI_TEST_DATABASE_URL` set). Uses `skadi_api::bootstrap` to create the
/// `cloacina` database + run all migrations, then drives the identical flow.
#[tokio::test]
#[serial_test::serial(hunter_registry)]
#[ignore = "requires SKADI_TEST_DATABASE_URL (Postgres)"]
async fn configure_via_api_add_movie_and_observe_acquire_to_imported_postgres() {
    let Ok(pg_url) = std::env::var("SKADI_TEST_DATABASE_URL") else {
        eprintln!("SKADI_TEST_DATABASE_URL unset — skipping Postgres e2e");
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    // Bootstrap creates the cloacina DB + runs skadi-store + skadi-movies
    // migrations against the configured Postgres.
    let config = Config {
        database_url: pg_url.clone(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    // The module's Cloacina runner connects to the `cloacina` database at
    // construction, so it must exist first (idempotent; bootstrap calls it too).
    skadi_api::ensure_cloacina_database(&pg_url)
        .await
        .expect("create cloacina db");
    let module_for_bootstrap: Arc<dyn skadi_core::DomainModule> = Arc::new(
        MoviesModule::new(
            Store::connect(&pg_url).unwrap(),
            &pg_url,
            SharedHunterDeps {
                indexers: vec![],
                downloaders: vec![],
                notifiers: vec![],
                scoring: ScoringConfig {
                    definitions: default_definitions(),
                    profile: permissive_profile(),
                    formats: vec![],
                    min_seeders: 0,
                    audiobook: None,
                },
            },
        )
        .await
        .unwrap(),
    );
    skadi_api::bootstrap(&config, &[module_for_bootstrap])
        .await
        .expect("pg bootstrap");

    let store = Store::connect(&pg_url).unwrap();
    drive_e2e(&pg_url, store, dir.path()).await;
}

/// A permissive default profile over all built-in definitions.
fn permissive_profile() -> QualityProfile {
    let defs = default_definitions();
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

/// The shared end-to-end flow, parameterized by the already-migrated store +
/// backend URL. `work_dir` holds the source file + library.
async fn drive_e2e(skadi_url: &str, store: Store, work_dir: &std::path::Path) {
    let src = work_dir.join("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv");
    // Pad past the matcher's 50 MB "is this a sample?" threshold.
    std::fs::write(&src, vec![0u8; 52 * 1024 * 1024]).unwrap();
    let library = work_dir.join("library");
    std::fs::create_dir_all(&library).unwrap();

    // Scoring profile allowing Bluray-720p + Bluray-1080p (cutoff 1080p).
    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
    let scoring_profile = QualityProfile {
        id: skadi_core::ProfileId::new(),
        name: "e2e".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // Build the movies module (owns the Cloacina runner) — same as the daemon.
    let module = Arc::new(
        MoviesModule::new(
            store.clone(),
            skadi_url,
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
        .unwrap(),
    );

    // Install mock hunter services into the process-global so the manual-acquire
    // workflow (driven via the runner) uses the mocked indexer/downloader/etc.
    // We deliberately do NOT run the supervisor here — that would call
    // `module.workers()` and install the module's own (empty-provider) services,
    // clobbering these mocks.
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
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
        importer: Arc::new(skadi_importer::DefaultImporter::new(NoopMatcher)),
        importer_factory: Some(factory),
        notifiers: vec![Arc::new(RecordingNotifier {
            id: NotifierId::new(),
            seen: recorder.clone(),
        })],
        scoring: ScoringConfig {
            definitions: defs,
            profile: scoring_profile,
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    skadi_hunter::services::reset_services();
    set_services(svc);

    // Build the HTTP surface exactly as the daemon does.
    let provider: Arc<dyn MetadataProvider> = Arc::new(FakeTmdb);
    let movies_http = MoviesHttp::new(module.store(), provider, module.runner());
    let movies_library: Arc<dyn skadi_api::LibraryProvider> =
        Arc::new(MoviesLibrary::new(module.store()));

    let port = free_port();
    let config = Config {
        database_url: skadi_url.to_string(),
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        bearer_token: None, // open mode for the test
    };
    let state = AppState::new_full(
        config,
        Some(store.clone()),
        vec![DomainDescriptor {
            name: "movies".into(),
            kind: MediaKind::Movie,
        }],
        vec![movies_library],
    );
    let cancel = state.cancel.clone();
    let routes = HttpModule::routes(&movies_http);
    let server = tokio::spawn(skadi_api::serve(state, vec![routes]));

    let base = format!("http://127.0.0.1:{port}");
    let client = Client::new(base.clone(), None);

    // Wait for /health. Budgets in this test are generous because it boots a real
    // daemon and, under a full `cargo test --workspace`, competes with every other
    // crate's suite for the machine — the original 20/30 s passed in isolation and
    // timed out in the workspace run.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if client.health().await.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "server never came up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 1) Create a quality profile + root folder via the settings API.
    let profile = client
        .create_setting(
            "profiles",
            &serde_json::json!({ "name": "e2e", "cutoff": "Bluray-1080p" }),
        )
        .await
        .expect("create profile");
    let profile_id = profile["id"].as_str().unwrap().to_string();
    // Single library root (SKADI-T-0302): point `library.root` at the test dir;
    // movies land under `<library.root>/movie`. No more root_folders settings.
    store
        .set_config(
            "library.root",
            &library.display().to_string(),
            ConfigSource::Runtime,
        )
        .await
        .expect("set library.root");

    // 2) Enable the movies domain.
    let dom = client.set_domain_enabled("movies", true).await.unwrap();
    assert_eq!(dom["enabled"], true);

    // 3) Add the movie (TMDB lookup mocked) into the library root folder.
    //    search=false so the manual acquire below is the ONLY run (this e2e
    //    exercises the manual endpoint deterministically).
    let movie = client
        .add_movie(
            603,
            Some(&profile_id),
            Some(&library.display().to_string()),
            false,
        )
        .await
        .expect("add movie");
    assert_eq!(movie["title"], "The Matrix");
    let movie_id = movie["id"].as_str().unwrap().to_string();
    let edition_id = movie["editions"][0]["id"].as_str().unwrap().to_string();

    // 4) Interactive search (SKADI-T-0114): list scored release candidates for
    //    the edition, then grab the chosen one. This exercises the manual
    //    search→grab path instead of the auto-acquire trigger.
    let candidates = client
        .list_releases(&movie_id, &edition_id)
        .await
        .expect("list releases");
    let cands = candidates.as_array().expect("releases is an array");
    assert!(!cands.is_empty(), "mock indexer release should be listed");
    let chosen = cands
        .iter()
        .find(|c| c["accepted"] == true)
        .expect("at least one accepted candidate");
    assert!(
        chosen["quality"].as_str().is_some_and(|q| !q.is_empty()),
        "candidate carries a human quality name: {chosen:?}"
    );
    let release = chosen["release"].clone();

    // 4b) Blocklist round-trip (SKADI-T-0115): block the chosen release, confirm
    //     it now lists as rejected "blocklisted", then unblock so the grab can
    //     proceed. Exercises the blocklist API + the shared evaluate() filter.
    let parsed: skadi_indexers::Release =
        serde_json::from_value(release.clone()).expect("candidate deserializes to Release");
    let key = skadi_indexers::release_key(&parsed);
    let blocked = client
        .block_release(&serde_json::json!({
            "release_key": key,
            "title": parsed.title,
            "acquirable_ref": edition_id,
            "reason": "manual test block",
        }))
        .await
        .expect("block release");
    let block_id = blocked["id"].as_str().unwrap().to_string();
    assert_eq!(blocked["release_key"], key);

    let after_block = client
        .list_releases(&movie_id, &edition_id)
        .await
        .expect("list after block");
    let blocked_cand = after_block
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["release"]["title"] == parsed.title.as_str())
        .expect("the candidate is still listed");
    assert_eq!(
        blocked_cand["accepted"], false,
        "blocked candidate rejected"
    );
    assert_eq!(blocked_cand["reason"], "blocklisted");

    // It shows up in the global blocklist view.
    let bl = client.list_blocklist(None).await.expect("list blocklist");
    assert_eq!(bl.as_array().unwrap().len(), 1);

    // Unblock and confirm the verdict flips back to accepted.
    client
        .unblock_release(&block_id)
        .await
        .expect("unblock release");
    let after_unblock = client
        .list_releases(&movie_id, &edition_id)
        .await
        .expect("list after unblock");
    assert!(
        after_unblock
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["accepted"] == true),
        "candidate accepted again after unblock"
    );

    let acc = client
        .grab_release(&movie_id, &edition_id, &release)
        .await
        .expect("grab");
    assert_eq!(acc["accepted"], true);

    // 5) Poll until the edition reports imported.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut imported = false;
    while Instant::now() < deadline {
        let got = client.get_movie(&movie_id).await.unwrap();
        let status = got["editions"][0]["status"].clone();
        // The movie DTO serializes the full `AcquisitionStatus`; Imported is an
        // object with an "Imported" key (serde external tagging).
        if status.get("Imported").is_some() || status == serde_json::json!("Imported") {
            imported = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(imported, "edition never reached Imported");

    // 6) File landed at the sanitized destination.
    assert!(
        library
            .join("movie/the-matrix_(1999)_{tmdb-603}_{imdb-tt0133093}/theatrical/the-matrix_(1999).mkv")
            .exists(),
        "imported file present in the library"
    );

    // 7) Library lists the movie; activity is empty post-completion.
    let lib = client.library(Some("movie"), None).await.unwrap();
    assert_eq!(lib.as_array().unwrap().len(), 1);
    assert_eq!(lib[0]["title"], "The Matrix");

    // The status is persisted mid-pipeline (in `import`), so the edition can
    // read `imported` a beat before the spawned `start_acquire` task returns and
    // removes its tracker entry (the `notify` stage + return still run). Poll the
    // activity view until it drains rather than asserting on the race.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut drained = false;
    while Instant::now() < deadline {
        if client
            .activity()
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
        {
            drained = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(drained, "in-flight activity never drained after completion");

    // The recording notifier saw Grabbed (at snatch) THEN Imported (SKADI-T-0037).
    let kinds: Vec<_> = recorder.lock().unwrap().iter().map(|e| e.kind()).collect();
    assert_eq!(
        kinds,
        vec![NotificationKind::Grabbed, NotificationKind::Imported],
        "expected Grabbed before Imported, got {kinds:?}"
    );

    // 8) Clean shutdown.
    cancel.cancel();
    let _ = server.await;
    module.runner().shutdown().await.unwrap();
}
