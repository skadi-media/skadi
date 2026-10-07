//! Integration tests for `skadi-hunter`'s storage isolation (SKADI-T-0029).
//!
//! Proves the load-bearing design decision: when skadi-store and the Cloacina
//! runner are pointed at the same deployment, their tables do **not** collide.
//! For SQLite that means Cloacina's tables land in the sibling `hunter.db` and
//! skadi-store's land in `skadi.db`, with neither bleeding into the other.

use diesel::connection::Connection;
use diesel::sql_query;
use diesel::sqlite::SqliteConnection;
use diesel::{QueryableByName, RunQueryDsl};

use skadi_hunter::{build_runner, cloacina_target_for};
use skadi_store::Store;

#[derive(QueryableByName)]
struct TableName {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

/// List user table names in a SQLite database file.
fn tables(path: &str) -> Vec<String> {
    let mut conn = SqliteConnection::establish(path).expect("open sqlite db");
    // Wait out any transient lock from a not-yet-fully-dropped pool connection.
    sql_query("PRAGMA busy_timeout = 10000")
        .execute(&mut conn)
        .expect("set busy_timeout");
    sql_query(
        "SELECT name FROM sqlite_master \
         WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .load::<TableName>(&mut conn)
    .expect("list tables")
    .into_iter()
    .map(|t| t.name)
    .collect()
}

#[tokio::test]
async fn sqlite_storage_is_isolated_from_skadi_store() {
    let dir = tempfile::tempdir().unwrap();
    let skadi_db = dir.path().join("skadi.db");
    let hunter_db = dir.path().join("hunter.db");
    let skadi_url = format!("sqlite://{}", skadi_db.display());

    // Sanity: derivation puts Cloacina in the sibling file.
    let target = cloacina_target_for(&skadi_url).unwrap();
    assert_eq!(target.url, format!("sqlite://{}", hunter_db.display()));
    assert!(target.schema.is_none(), "SQLite uses a file, not a schema");

    // Migrate skadi-store into skadi.db, then drop the pool so its (non-WAL)
    // connection is released before we open the file for inspection.
    let store = Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);

    // Build the Cloacina runner → migrates hunter.db; then shut it down.
    let runner = build_runner(&skadi_url).await.unwrap();
    runner.shutdown().await.unwrap();

    assert!(skadi_db.exists(), "skadi.db created");
    assert!(hunter_db.exists(), "hunter.db created");

    let skadi_tables = tables(&skadi_db.display().to_string());
    let hunter_tables = tables(&hunter_db.display().to_string());

    // skadi-store's tables live in skadi.db only.
    assert!(
        skadi_tables.iter().any(|t| t == "domains"),
        "skadi.db should have skadi-store tables; got {skadi_tables:?}"
    );
    assert!(
        skadi_tables.iter().any(|t| t == "credentials"),
        "skadi.db should have credentials; got {skadi_tables:?}"
    );
    assert!(
        !skadi_tables.iter().any(|t| t == "task_executions"),
        "skadi.db must NOT contain Cloacina tables; got {skadi_tables:?}"
    );

    // Cloacina's tables live in hunter.db only.
    assert!(
        hunter_tables.iter().any(|t| t == "task_executions"),
        "hunter.db should have Cloacina tables; got {hunter_tables:?}"
    );
    assert!(
        hunter_tables.iter().any(|t| t == "workflow_executions"),
        "hunter.db should have workflow_executions; got {hunter_tables:?}"
    );
    assert!(
        !hunter_tables
            .iter()
            .any(|t| t == "domains" || t == "credentials"),
        "hunter.db must NOT contain skadi-store tables; got {hunter_tables:?}"
    );
}

// ---------------------------------------------------------------------------
// End-to-end DAG test (SKADI-T-0033): execute the real `acquire` workflow
// against a Cloacina runner with mocked indexers/downloaders/importer/notifier
// and assert the full search → decide → snatch → monitor → import → notify
// pipeline drives the acquirable to Imported.
// ---------------------------------------------------------------------------

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use cloacina::executor::WorkflowExecutor;

use skadi_core::{
    AcquisitionStatus, AppError, DownloaderId, ExternalIds, IndexerId, MediaKind, NotifierId,
    ProfileId, Protocol, Result as SkadiResult,
};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_importer::{
    AcquirableMatch, AcquirableMatcher, AcquirableRef, CollisionPolicy, CompletedDownload,
    DefaultImporter, Importer,
};
use skadi_indexers::{
    Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};
use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
use skadi_quality::{QualityProfile, default_definitions, parse};

use skadi_hunter::services::{ScoringConfig, reset_services};
use skadi_hunter::{
    AcquireState, HunterServices, InMemoryStatusSink, SearchSpec, StatusSink as _,
    build_runner_for, set_services,
};

/// Wait (briefly) for `want` notify events to land. `sweep_once` returns when
/// every run has finished, but the notifier is driven from the run's own task, so
/// the last event can arrive a beat later — asserting the count immediately is a
/// race the suite used to win by luck.
/// Wait for every detached acquire run to finish and release its in-flight
/// slot (SKADI-T-0595), force-finishing leftovers so nothing leaks into the
/// next test: the tracker is process-global and the suite reuses release
/// titles, so a leaked release-title claim makes a later test's snatch end as
/// "already in flight for another item".
async fn drain_tracker() {
    for _ in 0..600 {
        if skadi_hunter::tracker().snapshot().is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    for r in skadi_hunter::tracker().snapshot() {
        eprintln!("force-finishing leaked in-flight run {}", r.acquirable_ref);
        skadi_hunter::tracker().finish(&r.acquirable_ref);
    }
}

async fn wait_for_events<T>(recorder: &std::sync::Arc<std::sync::Mutex<Vec<T>>>, want: usize) {
    for _ in 0..100 {
        if recorder.lock().unwrap().len() >= want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

// --- mocks ---

struct OneShotIndexer {
    id: IndexerId,
    release: Release,
    /// Per-search counter so each search answers with a distinct release title
    /// (see `search`).
    searches: std::sync::atomic::AtomicUsize,
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
        // Each search answers with its own release *title*. A real indexer does:
        // a release name identifies one file, so two different items never see the
        // identical name unless it is a pack. Returning one fixed title made every
        // sweeping run look like it had chosen the same download, which the
        // in-flight release claim (SKADI-T-0402) correctly collapses to one grab.
        let n = self
            .searches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut r = self.release.clone();
        r.title = format!("{} [{n}]", r.title);
        Ok(vec![r])
    }
}

/// Per-run copy of a completed file, so concurrent runs do not import the same
/// path (see `ImmediateDownloader::add`).
fn copy_for_run(src: &std::path::Path, n: usize) -> std::path::PathBuf {
    // Own *directory*, same file name: the importer derives the library name from
    // the file it is given, so renaming the copy would rename what lands in the
    // library (and break the end-to-end test's path assertion).
    let name = src.file_name().unwrap_or_default();
    src.with_file_name(format!("run{n}")).join(name)
}

struct ImmediateDownloader {
    id: DownloaderId,
    completed: Vec<std::path::PathBuf>,
    /// Counter making each add a distinct handle + file copy.
    adds: std::sync::atomic::AtomicUsize,
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
        // A distinct handle per add, and a distinct **copy** of the completed
        // file for it. A real client hands each transfer its own bytes; returning
        // one shared path made two concurrent runs import the same file to the
        // same destination, so whichever lost the race was skipped as
        // already-present and never notified — a latent race in the fixture.
        let n = self.adds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        for src in &self.completed {
            let dst = copy_for_run(src, n);
            let _ = std::fs::copy(src, &dst);
        }
        Ok(DownloadHandle {
            native_id: format!("test-handle-{n}"),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        let n: usize = h
            .native_id
            .rsplit('-')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Ok(DownloadStatus::Completed {
            files: self
                .completed
                .iter()
                .map(|p| {
                    let c = copy_for_run(p, n);
                    if c.exists() { c } else { p.clone() }
                })
                .collect(),
        })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

/// Places every file into one dir and credits it to `acquirable`.
///
/// The ref matters: since the pack fan-out (SKADI-T-0310) `import` writes
/// `Imported` keyed by the acquirable the **matcher** returns, not the run's
/// own. A test must therefore credit the same ref it seeded, or the run's ref is
/// left at the `Snatched` that `snatch` wrote (SKADI-T-0384).
struct PlaceInDir {
    dir: std::path::PathBuf,
    acquirable: AcquirableRef,
}
impl PlaceInDir {
    /// The default harness ref, used by tests that seed `ed-1`.
    fn new(dir: std::path::PathBuf) -> Self {
        Self::crediting(dir, "ed-1")
    }
    fn crediting(dir: std::path::PathBuf, acquirable: &str) -> Self {
        Self {
            dir,
            acquirable: AcquirableRef(acquirable.into()),
        }
    }
}
impl AcquirableMatcher for PlaceInDir {
    fn match_file(
        &self,
        _parsed: &skadi_quality::ParsedRelease,
        source: &Path,
        _completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        let dest = self.dir.join(source.file_name().unwrap());
        // This harness deliberately routes every run to one dir, so opt into Overwrite
        // (the default Skip would reject the 2nd+ run as a collision — SKADI-T-0217).
        vec![
            AcquirableMatch::new(self.acquirable.clone(), dest)
                .with_collision(CollisionPolicy::Overwrite),
        ]
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
        // Both kinds (SKADI-T-0538): an import that supersedes a held file now
        // emits `Upgraded` rather than `Imported`, and these tests assert *that*
        // a run notified, not which kind it chose.
        &[NotificationKind::Imported, NotificationKind::Upgraded]
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
async fn end_to_end_acquire_workflow_drives_to_imported() {
    // Layout: source movie file, library dir, skadi.db + hunter.db.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // Pick a profile whose `allowed` includes Bluray-1080p so decide accepts.
    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let lo = find("Bluray-720p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "e2e".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // The mock release whose name parses to Bluray-1080p.
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };

    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    // Migrate skadi-store tables (incl. decision_history) so the grab can persist
    // its decision explanation (SKADI-T-0187).
    store.run_migrations().await.unwrap();
    let status = Arc::new(InMemoryStatusSink::new());
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
            adds: std::sync::atomic::AtomicUsize::new(0),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(library.clone())))
            as Arc<dyn Importer>,
        importer_factory: None,
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

    reset_services();
    set_services(services);

    // Build the Cloacina runner against an isolated hunter.db.
    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // Seed the initial context with an AcquireState the domain would set up.
    let acquirable = AcquirableRef("ed-1".into());
    let state = AcquireState::new(
        acquirable.clone(),
        SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile.id,
    );
    let ctx = state.into_context().unwrap();

    let result = runner
        .execute("acquire", ctx)
        .await
        .expect("workflow execute Ok");
    assert!(
        matches!(result.status, cloacina::WorkflowStatus::Completed),
        "expected Completed, got {:?} (msg: {:?})",
        result.status,
        result.error_message
    );

    // Status sink ended up at Imported.
    let last = status
        .get_status(&acquirable)
        .await
        .unwrap()
        .expect("status");
    assert!(
        matches!(last, AcquisitionStatus::Imported { .. }),
        "expected Imported, got {last:?}"
    );

    // Notifier recorded one Imported event with our title/year.
    let events = recorder.lock().unwrap().clone();
    assert_eq!(events.len(), 1, "exactly one notify");
    match &events[0] {
        NotificationEvent::Imported(p) => {
            assert_eq!(p.title, "Movie");
            assert_eq!(p.year, Some(2020));
        }
        other => panic!("expected Imported, got {other:?}"),
    }

    // The imported file actually exists in the library.
    assert!(
        library
            .join("Movie.2020.1080p.BluRay.x264-GRP.mkv")
            .exists()
    );

    // The grab persisted a decision-history row explaining *why* (SKADI-T-0187).
    {
        use skadi_store::DecisionHistoryRepo;
        let store2 = skadi_store::Store::connect(&skadi_url).unwrap();
        let decisions = store2.list_decisions(10, 0).await.unwrap();
        assert_eq!(decisions.len(), 1, "one decision recorded at grab");
        assert_eq!(decisions[0].acquirable_ref, "ed-1");
        assert!(
            decisions[0].title.contains("1080p"),
            "{}",
            decisions[0].title
        );
        assert!(
            decisions[0].explanation.contains("\"accepted\":true"),
            "explanation JSON persisted: {}",
            decisions[0].explanation
        );
        assert_eq!(store2.decisions_for("ed-1").await.unwrap().len(), 1);
        // The chosen release's stable identity is persisted for grab→import
        // correlation (SKADI-T-0196): the magnet's btih info-hash.
        assert_eq!(
            decisions[0].release_key.as_deref(),
            Some("btih:abc"),
            "release_key correlates the decision to its grab"
        );
    }

    runner.shutdown().await.unwrap();
    let _ = AppError::Network("unused — keep import alive".into());
}

// ---------------------------------------------------------------------------
// Trigger surface tests (SKADI-T-0034): sweep_once + HunterWorker cancellation
// ---------------------------------------------------------------------------

use async_trait::async_trait as async_trait_2;
use skadi_core::Worker as _;
use skadi_hunter::worker::{AcquireSeed, HunterWorker, WantedQuery, sweep_once};
// `AcquireOutcome` + `start_acquire` are imported from the crate root lower in
// this file (they're re-exported there) and are visible module-wide.
use tokio_util::sync::CancellationToken;

/// Build a `HunterServices` whose pipeline succeeds end-to-end, using the same
/// mock shapes as the e2e DAG test. Pulled out as a helper so trigger tests can
/// re-use it without re-defining the mocks.
fn make_e2e_services(
    library: std::path::PathBuf,
    src: std::path::PathBuf,
    recorder: Arc<Mutex<Vec<NotificationEvent>>>,
    profile: QualityProfile,
    defs: Vec<skadi_quality::QualityDefinition>,
    store: skadi_store::Store,
) -> Arc<HunterServices> {
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src],
            adds: std::sync::atomic::AtomicUsize::new(0),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(library))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![Arc::new(RecordingNotifier {
            id: NotifierId::new(),
            seen: recorder,
        })],
        scoring: ScoringConfig {
            definitions: defs,
            profile,
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    })
}

struct StubQuery(Vec<AcquireSeed>);

#[async_trait_2]
impl WantedQuery for StubQuery {
    async fn wanted(&self) -> Result<Vec<AcquireSeed>, AppError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn sweep_once_starts_one_run_per_wanted_item() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let lo = find("Bluray-720p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "sweep".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = make_e2e_services(
        library.clone(),
        src.clone(),
        recorder.clone(),
        profile.clone(),
        defs.clone(),
        store,
    );

    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // Three distinct seeds → three runs → three Imported events.
    let seeds: Vec<AcquireSeed> = (0..3)
        .map(|i| AcquireSeed {
            acquirable: AcquirableRef(format!("ed-{i}")),
            request: SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            profile: profile.id,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        })
        .collect();
    let query = StubQuery(seeds);
    let started = sweep_once(&runner, &query, 4).await.unwrap();
    assert_eq!(started, 3, "all three runs started");

    // The subject here is the sweep's fan-out (asserted above): one run per
    // wanted item. The notify count is NOT three-for-three, because all three
    // seeds are editions of the *same* movie and therefore resolve to the same
    // library destination — whichever run places second finds the file already
    // there and is skipped rather than importing a duplicate. Which runs win is a
    // genuine race, so assert what is invariant: the sweep produced work, and at
    // least one run carried an item all the way to Imported.
    wait_for_events(&recorder, 3).await;
    let events = recorder.lock().unwrap().len();
    assert!(
        (1..=3).contains(&events),
        "between one and three imports, got {events}"
    );

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn detached_sweep_frees_lanes_and_still_imports() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let lo = find("Bluray-720p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "sweep".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = make_e2e_services(
        library.clone(),
        src.clone(),
        recorder.clone(),
        profile.clone(),
        defs.clone(),
        store,
    );

    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // Three distinct seeds → three runs → three Imported events.
    let seeds: Vec<AcquireSeed> = (0..3)
        .map(|i| AcquireSeed {
            acquirable: AcquirableRef(format!("ed-{i}")),
            request: SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            profile: profile.id,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        })
        .collect();
    let query = StubQuery(seeds);
    // SKADI-T-0595: the detached sweep returns as soon as every seed has been
    // searched, decided and handed to its own workflow task; the imports land
    // afterwards, and the tracker releases each item when its workflow ends.
    let runner = std::sync::Arc::new(runner);
    let started = skadi_hunter::sweep_once_detached(std::sync::Arc::clone(&runner), &query, 4)
        .await
        .unwrap();
    assert_eq!(started, 3, "all three runs launched");

    // The subject here is the sweep's fan-out (asserted above): one run per
    // wanted item. The notify count is NOT three-for-three, because all three
    // seeds are editions of the *same* movie and therefore resolve to the same
    // library destination — whichever run places second finds the file already
    // there and is skipped rather than importing a duplicate. Which runs win is a
    // genuine race, so assert what is invariant: the sweep produced work, and at
    // least one run carried an item all the way to Imported.
    wait_for_events(&recorder, 3).await;
    let events = recorder.lock().unwrap().len();
    assert!(
        (1..=3).contains(&events),
        "between one and three imports, got {events}"
    );

    // Wait for the detached workflows to finish before tearing the runner down.
    // Generous: three workflows hop through the Cloacina scheduler (snatch →
    // monitor → import → notify) and can take well over five seconds together.
    for _ in 0..1200 {
        if skadi_hunter::tracker().snapshot().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Never leak an in-flight entry into the next test: the suite reuses
    // acquirable refs, and a leftover "ed-0" makes a later `start_acquire`
    // return AlreadyInFlight and that test fail for the wrong reason.
    let left: Vec<String> = skadi_hunter::tracker()
        .snapshot()
        .into_iter()
        .map(|r| r.acquirable_ref)
        .collect();
    for r in &left {
        skadi_hunter::tracker().finish(r);
    }
    runner.shutdown().await.unwrap();
    assert!(
        left.is_empty(),
        "every detached run released its in-flight slot; leaked: {left:?}"
    );
}

/// SKADI-T-0182: an **upgrade** run (`current_quality = Some`) that finds no
/// strictly-better release must leave the held file `Imported` — it must NOT
/// regress it to `Searching` (search entry) or `Failed{NoSuitableRelease}`
/// (decide miss). Here the only available release is the *same* quality the file
/// already holds, so `decide` rejects it and `start_acquire` returns `NoRelease`
/// while the status sink stays at `Imported`.
#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn upgrade_run_with_no_better_release_preserves_imported_status() {
    use skadi_core::FileRef;

    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let src = dir.path().join("Movie.2020.720p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let lo = find("Bluray-720p");
    let hi = find("Bluray-1080p");
    // Upgrades ON, cutoff at 1080p — the held 720p file is below cutoff (a real
    // upgrade candidate), but the only available release is also 720p.
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "upgrade".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    };

    // The only release the indexer offers parses to 720p — equal to the held
    // quality, so it is not a strict upgrade.
    let title = "Movie.2020.720p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:eq".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };

    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let status = Arc::new(InMemoryStatusSink::new());
    let acquirable = AcquirableRef("ed-1".into());
    // Pre-seed the held file as Imported at 720p.
    status
        .set_status(
            &acquirable,
            AcquisitionStatus::Imported {
                file: FileRef { path: src.clone() },
                quality: lo,
                score: 0,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();

    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
            adds: std::sync::atomic::AtomicUsize::new(0),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(library))) as Arc<dyn Importer>,
        importer_factory: None,
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
    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // An upgrade seed for the held 720p file.
    let seed = AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: Some(lo),
        current_format_score: None,
        current_unplayable: false,
    };

    let outcome = start_acquire(&runner, seed).await.unwrap();
    assert!(
        matches!(outcome, AcquireOutcome::NoRelease),
        "no strict upgrade available → NoRelease, got {outcome:?}"
    );

    // The held file is STILL Imported at 720p — not Searching, not Failed.
    let after = status
        .get_status(&acquirable)
        .await
        .unwrap()
        .expect("status");
    assert!(
        matches!(after, AcquisitionStatus::Imported { quality, .. } if quality == lo),
        "upgrade miss must preserve Imported, got {after:?}"
    );
    // And nothing was grabbed/notified.
    assert!(
        recorder.lock().unwrap().is_empty(),
        "no notify on an upgrade miss"
    );

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn sweep_once_dedupes_duplicate_seeds() {
    // A wanted list with a duplicate acquirable starts exactly one run for it
    // (SKADI-T-0039) — `[ed-0, ed-0, ed-1]` → 2 runs, not 3.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "dedup".into(),
        allowed: vec![find("Bluray-720p"), find("Bluray-1080p")],
        cutoff: find("Bluray-1080p"),
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = make_e2e_services(library, src, recorder.clone(), profile.clone(), defs, store);
    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let seed = |r: &str| AcquireSeed {
        acquirable: AcquirableRef(r.into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let query = StubQuery(vec![seed("ed-0"), seed("ed-0"), seed("ed-1")]);
    let started = sweep_once(&runner, &query, 4).await.unwrap();
    // The subject: the duplicate `ed-0` seed collapses, so two runs start, not
    // three. The import count that follows is a race — both editions belong to
    // the same movie and resolve to one library destination, so the run that
    // places second is skipped as already-present (see
    // `sweep_once_starts_one_run_per_wanted_item`).
    assert_eq!(started, 2, "duplicate ed-0 collapsed to one run");
    wait_for_events(&recorder, 2).await;
    let events = recorder.lock().unwrap().len();
    assert!(
        (1..=2).contains(&events),
        "one or two imports from two runs, got {events}"
    );

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn sweep_once_bounds_concurrency() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A downloader that sleeps in `add` (the snatch boundary) while tracking the
    // peak number of simultaneously-running runs — proves the sweep semaphore
    // caps in-flight runs (SKADI-T-0040).
    struct ConcurrencyProbe {
        id: DownloaderId,
        inflight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        completed: Vec<std::path::PathBuf>,
    }
    #[async_trait]
    impl Downloader for ConcurrencyProbe {
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
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(DownloadHandle {
                native_id: "h".into(),
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

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "conc".into(),
        allowed: vec![find("Bluray-1080p")],
        cutoff: find("Bluray-1080p"),
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let peak = Arc::new(AtomicUsize::new(0));
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: skadi_store::Store::connect(&skadi_url).unwrap(),
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(ConcurrencyProbe {
            id: DownloaderId::new(),
            inflight: Arc::new(AtomicUsize::new(0)),
            peak: peak.clone(),
            completed: vec![src],
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(library))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // 10 distinct seeds, cap of 2.
    let seeds: Vec<AcquireSeed> = (0..10)
        .map(|i| AcquireSeed {
            acquirable: AcquirableRef(format!("ed-{i}")),
            request: SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            profile: profile.id,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        })
        .collect();
    let started = sweep_once(&runner, &StubQuery(seeds), 2).await.unwrap();
    assert_eq!(started, 10, "all ten runs started");
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "peak concurrent runs ({}) must not exceed the cap of 2",
        peak.load(Ordering::SeqCst)
    );

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn start_acquire_skips_when_already_in_flight() {
    // A run already tracked for the acquirable means a second start_acquire is a
    // no-op `AlreadyInFlight` — no double-snatch (SKADI-T-0039).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "inflight".into(),
        allowed: vec![find("Bluray-1080p")],
        cutoff: find("Bluray-1080p"),
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = make_e2e_services(library, src, recorder.clone(), profile.clone(), defs, store);
    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // Simulate a run already holding the slot for "ed-9".
    assert!(skadi_hunter::tracker::tracker().try_start("held", MediaKind::Movie, "ed-9"));

    let seed = AcquireSeed {
        acquirable: AcquirableRef("ed-9".into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let outcome = start_acquire(&runner, seed).await.unwrap();
    assert!(
        matches!(outcome, AcquireOutcome::AlreadyInFlight),
        "second start must be skipped, got {outcome:?}"
    );
    assert!(
        recorder.lock().unwrap().is_empty(),
        "skipped run must not execute (no notify)"
    );

    // Cleanup the simulated slot so the global tracker is clean for later tests.
    skadi_hunter::tracker::tracker().finish("ed-9");
    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn hunter_worker_cancels_cleanly() {
    // No work to do — empty WantedQuery — but the worker must spin up, accept
    // the cancellation token, and return promptly.
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // Minimal services (the worker doesn't run the pipeline this test).
    let defs = default_definitions();
    let any_q = defs[0].id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "cancel".into(),
        allowed: vec![any_q],
        cutoff: any_q,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![],
        downloaders: vec![],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile,
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    struct Empty;
    #[async_trait_2]
    impl WantedQuery for Empty {
        async fn wanted(&self) -> Result<Vec<AcquireSeed>, AppError> {
            Ok(vec![])
        }
    }

    let worker = Box::new(HunterWorker::new(
        services,
        Arc::new(runner),
        std::time::Duration::from_millis(50),
        Arc::new(Empty),
        4,
    ));
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(worker.run(cancel.clone()));

    // Let the worker tick at least once, then cancel.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    cancel.cancel();

    // Must return promptly (well under 2s).
    let res = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    assert!(res.is_ok(), "worker did not finish within 2s after cancel");
    res.unwrap().expect("worker join");
    drain_tracker().await;
}

// ---------------------------------------------------------------------------
// Resilience tests (SKADI-T-0035): failure-injection (Failed status persisted),
// persistence across runners (the v0 honest substitute for full mid-run crash
// recovery — that depends on Cloacina's stale-claim recovery timing and is
// flagged as a follow-up), DomainModule wiring, and a gated Postgres path.
// ---------------------------------------------------------------------------

use diesel_migrations::EmbeddedMigrations;
use skadi_core::{
    DomainModule, FailureReason,
    module::{BoxFuture as CoreBoxFuture, BoxedWorker},
};
use skadi_hunter::{AcquireOutcome, start_acquire};

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn failure_injection_lands_acquirable_in_failed_status() {
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // Profile with allowed Bluray-1080p — but NO indexers, so `search` yields
    // zero candidates and `decide` errors `no suitable release`. The task
    // wrapper writes Failed{NoSuitableRelease} to the status sink before
    // returning, which the workflow surfaces as a non-Completed status.
    let defs = default_definitions();
    let any_q = defs[0].id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "fail".into(),
        allowed: vec![any_q],
        cutoff: any_q,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();
    let status = Arc::new(InMemoryStatusSink::new());
    // The injection: an indexer that *runs* but returns no releases. Search
    // succeeds with empty candidates; decide fails with NoSuitableRelease and
    // its task wrapper writes Failed{NoSuitableRelease}.
    struct EmptyIndexer {
        id: IndexerId,
    }
    #[async_trait]
    impl Indexer for EmptyIndexer {
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
        async fn search(&self, _q: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
            Ok(vec![])
        }
    }
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: status.clone(),
        indexers: vec![Arc::new(EmptyIndexer {
            id: IndexerId::new(),
        })],
        downloaders: vec![],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-fail".into());
    let state = AcquireState::new(
        acquirable.clone(),
        SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![],
            tv: None,
            series: None,
            tags: None,
        },
        profile.id,
    );
    let ctx = state.into_context().unwrap();

    let _ = runner.execute("acquire", ctx).await; // Ok or Err — what matters is the status.
    // (Cloacina returns Ok with status=Failed for task failures; either way the
    // sink should have the terminal Failed write.)

    let last = status.get_status(&acquirable).await.unwrap();
    match last {
        Some(AcquisitionStatus::Failed {
            reason: FailureReason::NoSuitableRelease,
            ..
        }) => {}
        other => panic!("expected Failed{{NoSuitableRelease}}, got {other:?}"),
    }

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn cloacina_state_persists_across_runner_restarts() {
    // V0 honest substitute for full mid-run crash-resume: build runner_a, run a
    // workflow to completion, shut it down, build runner_b on the same
    // hunter.db, run another workflow. If Cloacina's persistence is intact and
    // its tables survive the restart, both runs succeed. Full mid-run crash
    // recovery requires tuning Cloacina's stale_claim_threshold +
    // stale_claim_sweep_interval (default 60s + 30s) — flagged as a follow-up.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "restart".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = make_e2e_services(library, src, recorder.clone(), profile.clone(), defs, store);
    reset_services();
    set_services(services);

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();

    // --- run on runner_a, then shut it down hard ---
    let runner_a = build_runner_for(&target).await.unwrap();
    let seed_a = AcquireSeed {
        acquirable: AcquirableRef("ed-a".into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let AcquireOutcome::Started(res_a) = start_acquire(&runner_a, seed_a).await.unwrap() else {
        panic!("expected Started");
    };
    assert!(matches!(res_a.status, cloacina::WorkflowStatus::Completed));
    runner_a.shutdown().await.unwrap();
    drop(runner_a);
    // Give Cloacina's deadpool a moment to fully release the SQLite WAL lock
    // (the shutdown call returns before all background tasks have dropped
    // their connections).
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    // --- runner_b on the same hunter.db: tables, migrations, state intact ---
    let runner_b = build_runner_for(&target).await.unwrap();
    let seed_b = AcquireSeed {
        acquirable: AcquirableRef("ed-b".into()),
        request: seed_a_request(),
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let AcquireOutcome::Started(res_b) = start_acquire(&runner_b, seed_b).await.unwrap() else {
        panic!("expected Started");
    };
    assert!(matches!(res_b.status, cloacina::WorkflowStatus::Completed));
    runner_b.shutdown().await.unwrap();

    // Both runs notified.
    assert_eq!(recorder.lock().unwrap().len(), 2);
}

fn seed_a_request() -> SearchSpec {
    SearchSpec {
        tags: None,
        trigger: Default::default(),
        kind: MediaKind::Movie,
        titles: vec!["Movie".into()],
        year: Some(2020),
        external_ids: ExternalIds::default(),
        categories: vec![Category(2000)],
        tv: None,
        series: None,
    }
}

#[test]
fn hunter_module_wiring_compiles_under_domainmodule_trait() {
    // The hunter is edition-agnostic; each domain wires its own DomainModule
    // that returns a HunterWorker (and the rest) from `workers()`. This test
    // just proves the type fit: a tiny demo module whose `workers()` yields a
    // boxed worker that implements `skadi_core::Worker`. No runtime here —
    // building a real HunterWorker needs an async runtime for the Cloacina
    // runner; that's covered by the worker-cancellation test above.
    struct DemoWorker;
    impl skadi_core::Worker for DemoWorker {
        fn name(&self) -> &str {
            "demo"
        }
        fn run(self: Box<Self>, _cancel: CancellationToken) -> CoreBoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }
    struct DemoHunterModule;
    impl DomainModule for DemoHunterModule {
        fn name(&self) -> &'static str {
            "demo-hunter"
        }
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        // Borrows skadi-store's embedded migrations purely to satisfy the
        // reshaped trait (SKADI-T-0051); the demo module ships no schema.
        fn sqlite_migrations(&self) -> EmbeddedMigrations {
            skadi_store::SQLITE_MIGRATIONS
        }
        fn postgres_migrations(&self) -> EmbeddedMigrations {
            skadi_store::POSTGRES_MIGRATIONS
        }
        fn workers(&self) -> Vec<BoxedWorker> {
            vec![Box::new(DemoWorker)]
        }
    }
    let module = DemoHunterModule;
    assert_eq!(module.name(), "demo-hunter");
    assert_eq!(module.workers().len(), 1);
}

// --- Postgres path (gated) ---

/// `SKADI_TEST_DATABASE_URL` is set by `angreal db test` to the compose
/// Postgres URL. When unset, this test is skipped.
fn pg_url() -> Option<String> {
    std::env::var("SKADI_TEST_DATABASE_URL").ok()
}

#[tokio::test]
async fn postgres_schema_isolation_smoke_when_pg_available() {
    let Some(url) = pg_url() else {
        eprintln!("SKADI_TEST_DATABASE_URL unset — skipping Postgres path");
        return;
    };

    // Cloacina state lives in a **separate Postgres database** (`cloacina`) on
    // the same server, with the `cloacina_hunter` schema inside it. Cloacina
    // 0.6.x forced that database name itself; since 0.11 it honours the URL's
    // database, so `cloacina_target_for` selects it explicitly (SKADI-T-0390)
    // — production state stays put across the upgrade. Create the `cloacina`
    // database here if absent, then proceed.
    use diesel::RunQueryDsl as _;
    use diesel::pg::PgConnection;

    let mut admin = PgConnection::establish(&url).expect("connect pg (user db)");
    let _ = diesel::sql_query("CREATE DATABASE cloacina").execute(&mut admin); // ignore "already exists"

    let target = skadi_hunter::cloacina_target_for(&url).unwrap();
    let cloacina_url = {
        let mut u = url::Url::parse(&url).unwrap();
        u.set_path(skadi_hunter::CLOACINA_PG_DATABASE);
        u.to_string()
    };
    assert_eq!(
        target.url, cloacina_url,
        "target selects the cloacina database"
    );
    assert_eq!(
        target.schema.as_deref(),
        Some(skadi_hunter::HUNTER_PG_SCHEMA)
    );

    // Inspect / clean the schema on the `cloacina` database.
    let mut conn = PgConnection::establish(&cloacina_url).expect("connect pg cloacina db");
    diesel::sql_query(format!(
        "DROP SCHEMA IF EXISTS {} CASCADE",
        skadi_hunter::HUNTER_PG_SCHEMA
    ))
    .execute(&mut conn)
    .expect("drop schema");

    // Build the runner: migrations land in cloacina/cloacina_hunter.
    let runner = build_runner_for(&target).await.unwrap();

    // Cloacina's tables exist in the dedicated schema of the `cloacina` db.
    let n: i64 = diesel::sql_query(format!(
        "SELECT COUNT(*)::BIGINT AS c FROM information_schema.tables \
         WHERE table_schema = '{}' AND table_name = 'task_executions'",
        skadi_hunter::HUNTER_PG_SCHEMA
    ))
    .get_result::<CountRow>(&mut conn)
    .expect("count cloacina tables")
    .c;
    assert_eq!(n, 1, "cloacina tables should live in dedicated schema");

    let n_public: i64 = diesel::sql_query(
        "SELECT COUNT(*)::BIGINT AS c FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'task_executions'",
    )
    .get_result::<CountRow>(&mut conn)
    .expect("count public tables")
    .c;
    assert_eq!(n_public, 0, "public schema must not have Cloacina tables");

    runner.shutdown().await.unwrap();

    // Cleanup.
    diesel::sql_query(format!(
        "DROP SCHEMA IF EXISTS {} CASCADE",
        skadi_hunter::HUNTER_PG_SCHEMA
    ))
    .execute(&mut conn)
    .expect("drop schema cleanup");
}

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    c: i64,
}

// ---------------------------------------------------------------------------
// New-grain tests (SKADI-T-0042 / ADR SKADI-A-0002): search/decide run
// in-process and only a chosen release launches an `acquire_release` workflow.
// These assert the grain's two new behaviours that the old per-acquirable
// `acquire` tests don't cover: (1) a no-suitable-release item launches ZERO
// workflow runs, and (2) a per-release failure stays isolated to that item and
// blocklists the release — the granular retry/failure semantics the ADR
// promised to preserve.
// ---------------------------------------------------------------------------

use skadi_hunter::{AcquireOutcome as Outcome2, start_acquire as start_acquire_2};
use skadi_store::BlocklistRepo as _;

/// An indexer that runs but returns no releases (search succeeds empty; decide
/// then finds nothing).
struct EmptyResultIndexer {
    id: IndexerId,
}
#[async_trait]
impl Indexer for EmptyResultIndexer {
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
    async fn search(&self, _q: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
        Ok(vec![])
    }
}

/// A downloader that records whether `add` was ever called (it must NOT be, on
/// the no-release path) and otherwise fails — used to prove no snatch happened.
struct NeverDownloader {
    id: DownloaderId,
    added: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait]
impl Downloader for NeverDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, _r: &Release, _c: &Category) -> SkadiResult<DownloadHandle> {
        self.added.store(true, std::sync::atomic::Ordering::SeqCst);
        Err(AppError::Internal("add should not be called".into()))
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        Err(AppError::Internal("status should not be called".into()))
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn new_grain_no_suitable_release_launches_no_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let any_q = defs[0].id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "norel".into(),
        allowed: vec![any_q],
        cutoff: any_q,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let status = Arc::new(InMemoryStatusSink::new());
    let added = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: status.clone(),
        indexers: vec![Arc::new(EmptyResultIndexer {
            id: IndexerId::new(),
        })],
        downloaders: vec![Arc::new(NeverDownloader {
            id: DownloaderId::new(),
            added: added.clone(),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-norel".into());
    let seed = AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    // In-process search/decide find nothing → NoRelease, no workflow launched.
    let outcome = start_acquire_2(&runner, seed).await.unwrap();
    assert!(
        matches!(outcome, Outcome2::NoRelease),
        "expected NoRelease, got {outcome:?}"
    );
    // The downloader was never touched (no snatch ran).
    assert!(
        !added.load(std::sync::atomic::Ordering::SeqCst),
        "no acquire_release workflow should have launched"
    );
    // decide wrote Failed{NoSuitableRelease} to the status sink.
    let last = status.get_status(&acquirable).await.unwrap();
    assert!(
        matches!(
            last,
            Some(AcquisitionStatus::Failed {
                reason: FailureReason::NoSuitableRelease,
                ..
            })
        ),
        "expected Failed{{NoSuitableRelease}}, got {last:?}"
    );

    runner.shutdown().await.unwrap();
}

/// A downloader whose `add` always fails — the chosen release can't be snatched.
struct AddFailsDownloader {
    id: DownloaderId,
}
#[async_trait]
impl Downloader for AddFailsDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, _r: &Release, _c: &Category) -> SkadiResult<DownloadHandle> {
        Err(AppError::Internal("downloader rejected the torrent".into()))
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        Ok(DownloadStatus::Completed { files: vec![] })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn new_grain_per_release_failure_is_isolated_and_retried_with_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "isolate".into(),
        allowed: vec![hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let expected_key = skadi_indexers::release_key(&release);

    let status = Arc::new(InMemoryStatusSink::new());
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(AddFailsDownloader {
            id: DownloaderId::new(),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-iso".into());
    let seed = AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    // search+decide succeed (release chosen) → acquire_release launches, but
    // snatch fails. The run is Started (a workflow ran) and ends non-Completed.
    let outcome = start_acquire_2(&runner, seed).await.unwrap();
    let Outcome2::Started(res) = outcome else {
        panic!("expected Started (a release was chosen), got {outcome:?}");
    };
    assert!(
        !matches!(res.status, cloacina::WorkflowStatus::Completed),
        "snatch failure should not complete the run; got {:?}",
        res.status
    );
    // Per-item failure: this acquirable is Failed **with a retry-after backoff**
    // (a transfer failure is retried by a later sweep — flaky-source recovery —
    // not parked permanently).
    let last = status.get_status(&acquirable).await.unwrap();
    assert!(
        matches!(
            last,
            Some(AcquisitionStatus::Failed {
                retry_at: Some(_),
                ..
            })
        ),
        "expected Failed with a retry_at backoff, got {last:?}"
    );
    // A transfer failure is NOT auto-blocklisted: the release looked good (it was
    // chosen) — blocklisting on the first failure would permanently park a
    // flaky-but-good release. (A genuinely-dead release is blocklisted from the UI.)
    let blocked = store.blocked_keys().await.unwrap();
    assert!(
        !blocked.contains(&expected_key),
        "a transfer failure must not auto-blocklist the release; got {blocked:?}"
    );

    runner.shutdown().await.unwrap();
}

/// Lightweight benchmark validation for ADR SKADI-A-0002's headline claim: under
/// the new grain a sweep over W wanted items that find **no suitable release**
/// produces **zero** Cloacina workflow rows (vs. the old grain's W runs × 6
/// tasks). This is the common steady-state case; the analytic table lives in the
/// ADR. Here we prove the 0-row floor directly by counting `workflow_executions`
/// in hunter.db after an all-no-release sweep.
#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn new_grain_no_release_sweep_writes_zero_workflow_rows() {
    let dir = tempfile::tempdir().unwrap();
    let hunter_db = dir.path().join("hunter.db");
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let any_q = defs[0].id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "bench".into(),
        allowed: vec![any_q],
        cutoff: any_q,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![Arc::new(EmptyResultIndexer {
            id: IndexerId::new(),
        })],
        downloaders: vec![],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // W = 8 distinct wanted items, none of which have a suitable release.
    let seeds: Vec<AcquireSeed> = (0..8)
        .map(|i| AcquireSeed {
            acquirable: AcquirableRef(format!("ed-{i}")),
            request: SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            profile: profile.id,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        })
        .collect();
    let started = sweep_once(&runner, &StubQuery(seeds), 4).await.unwrap();
    assert_eq!(started, 0, "no-release items launch zero runs");

    runner.shutdown().await.unwrap();
    // Let Cloacina's pool release the WAL lock before inspecting the file.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let mut conn =
        SqliteConnection::establish(&hunter_db.display().to_string()).expect("open hunter.db");
    sql_query("PRAGMA busy_timeout = 10000")
        .execute(&mut conn)
        .unwrap();
    let rows: i64 = sql_query("SELECT COUNT(*) AS c FROM workflow_executions")
        .get_result::<CountRow>(&mut conn)
        .expect("count workflow_executions")
        .c;
    assert_eq!(
        rows, 0,
        "an all-no-release sweep must persist zero workflow rows (ADR SKADI-A-0002)"
    );
}

// ---------------------------------------------------------------------------
// Auto-blocklist on confirmed failure (SKADI-T-0188): a *terminal download
// failure* or an *import failure* blocklists the chosen release (it's at fault),
// so the next sweep re-decides and grabs the next-best candidate instead of
// re-grabbing the same dead release forever. Transient failures (a busy
// downloader, a slow-but-live transfer) keep the release grabbable — covered by
// `new_grain_per_release_failure_is_isolated_and_retried_with_backoff` above.
// ---------------------------------------------------------------------------

/// Downloader whose `add` succeeds (snatch ok) but whose `status` reports a hard
/// `Failed` — a terminal download failure (monitor maps it to `Internal` →
/// `FailureReason::DownloadFailed`).
struct DownloadFailsDownloader {
    id: DownloaderId,
}
#[async_trait]
impl Downloader for DownloadFailsDownloader {
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
            native_id: "h".into(),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        Ok(DownloadStatus::Failed {
            reason: "all seeders vanished".into(),
        })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

/// A matcher that places no files — every import "produces zero placed files",
/// which `pipeline::import` treats as a hard `ImportFailed`.
struct RejectAllMatcher;
impl AcquirableMatcher for RejectAllMatcher {
    fn match_file(
        &self,
        _parsed: &skadi_quality::ParsedRelease,
        _source: &Path,
        _completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        vec![]
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn terminal_download_failure_blocklists_the_release() {
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "dl-fail".into(),
        allowed: vec![hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:dead".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let expected_key = skadi_indexers::release_key(&release);

    let status = Arc::new(InMemoryStatusSink::new());
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(DownloadFailsDownloader {
            id: DownloaderId::new(),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-dl".into());
    let seed = AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    // A release is chosen and a workflow runs; monitor's terminal-failure branch
    // returns Ok (to stop Cloacina's retry storm), so the run "Completes".
    let outcome = start_acquire_2(&runner, seed).await.unwrap();
    assert!(
        matches!(outcome, Outcome2::Started(_)),
        "a release was chosen → Started, got {outcome:?}"
    );

    let blocked = store.blocked_keys().await.unwrap();
    assert!(
        blocked.contains(&expected_key),
        "terminal download failure must auto-blocklist the release; got {blocked:?}"
    );

    // The acquirable is Failed with a retry-after backoff (re-swept later — it just
    // won't re-pick the now-blocked release).
    let last = status.get_status(&acquirable).await.unwrap();
    assert!(
        matches!(
            last,
            Some(AcquisitionStatus::Failed {
                retry_at: Some(_),
                ..
            })
        ),
        "expected Failed with a retry_at backoff, got {last:?}"
    );

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn import_failure_blocklists_the_release() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "imp-fail".into(),
        allowed: vec![hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:badfile".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let expected_key = skadi_indexers::release_key(&release);

    let status = Arc::new(InMemoryStatusSink::new());
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        // Download completes (snatch + monitor ok); the importer then rejects every
        // file, so `import` is a hard failure.
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
            adds: std::sync::atomic::AtomicUsize::new(0),
        })],
        importer: Arc::new(DefaultImporter::new(RejectAllMatcher)) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-imp".into());
    let seed = AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let _ = start_acquire_2(&runner, seed).await.unwrap();

    let blocked = store.blocked_keys().await.unwrap();
    assert!(
        blocked.contains(&expected_key),
        "import failure must auto-blocklist the release; got {blocked:?}"
    );
    let last = status.get_status(&acquirable).await.unwrap();
    assert!(
        matches!(
            last,
            Some(AcquisitionStatus::Failed {
                retry_at: Some(_),
                ..
            })
        ),
        "expected Failed with a retry_at backoff, got {last:?}"
    );

    runner.shutdown().await.unwrap();
}

/// An indexer that returns a fixed list of releases (so `decide` has a *next-best*
/// to fall back to once the best one is blocklisted).
struct MultiIndexer {
    id: IndexerId,
    releases: Vec<Release>,
}
#[async_trait]
impl Indexer for MultiIndexer {
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
    async fn search(&self, _q: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
        Ok(self.releases.clone())
    }
}

/// A downloader that fails the transfer for the "bad" release (encoded into the
/// handle's `native_id` at `add` time) and completes it for the good one.
struct SelectiveDownloader {
    id: DownloaderId,
    good_file: std::path::PathBuf,
}
#[async_trait]
impl Downloader for SelectiveDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, r: &Release, c: &Category) -> SkadiResult<DownloadHandle> {
        // Encode which release this is into the handle so `status` (which only sees
        // the handle) can decide its fate.
        let tag = if r.title.contains("1080p") {
            "bad"
        } else {
            "good"
        };
        Ok(DownloadHandle {
            native_id: tag.into(),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        if h.native_id == "bad" {
            Ok(DownloadStatus::Failed {
                reason: "dead torrent".into(),
            })
        } else {
            Ok(DownloadStatus::Completed {
                files: vec![self.good_file.clone()],
            })
        }
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn blocklisted_release_is_skipped_so_next_sweep_grabs_next_best() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    // The good (720p) release's completed file.
    let good_src = dir.path().join("Movie.2020.720p.BluRay.x264-GRP.mkv");
    std::fs::write(&good_src, b"video").unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let lo = find("Bluray-720p");
    let hi = find("Bluray-1080p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "next-best".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // Two candidates: the best (1080p) is a dead torrent; the next-best (720p) is good.
    let bad_title = "Movie.2020.1080p.BluRay.x264-GRP";
    let good_title = "Movie.2020.720p.BluRay.x264-GRP";
    let bad = Release {
        indexer: IndexerId::new(),
        title: bad_title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:bad1080".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(99),
        categories: Vec::new(),
        parsed: parse(bad_title),
    };
    let good = Release {
        indexer: IndexerId::new(),
        title: good_title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:good720".into()),
        size: 5_000_000_000,
        published: Utc::now(),
        seeders: Some(50),
        categories: Vec::new(),
        parsed: parse(good_title),
    };
    let bad_key = skadi_indexers::release_key(&bad);
    let good_key = skadi_indexers::release_key(&good);

    let status = Arc::new(InMemoryStatusSink::new());
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![Arc::new(MultiIndexer {
            id: IndexerId::new(),
            releases: vec![bad, good],
        })],
        downloaders: vec![Arc::new(SelectiveDownloader {
            id: DownloaderId::new(),
            good_file: good_src.clone(),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::crediting(
            library.clone(),
            "ed-nb",
        ))) as Arc<dyn Importer>,
        importer_factory: None,
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
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-nb".into());
    let mk_seed = || AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };

    // First sweep: decide picks the best (1080p) release; it dies → blocklisted.
    let _ = start_acquire_2(&runner, mk_seed()).await.unwrap();
    let blocked = store.blocked_keys().await.unwrap();
    assert!(
        blocked.contains(&bad_key),
        "the dead best-release must be blocklisted after the first sweep; got {blocked:?}"
    );
    assert!(
        recorder.lock().unwrap().is_empty(),
        "nothing imported yet (the only grab so far failed)"
    );

    // Second sweep: decide now EXCLUDES the blocklisted 1080p and grabs the
    // next-best 720p, which completes → Imported.
    let outcome = start_acquire_2(&runner, mk_seed()).await.unwrap();
    assert!(
        matches!(outcome, Outcome2::Started(_)),
        "the next-best release should be chosen and run, got {outcome:?}"
    );

    let last = status.get_status(&acquirable).await.unwrap();
    assert!(
        matches!(last, Some(AcquisitionStatus::Imported { quality, .. }) if quality == lo),
        "the next-best 720p release should now be Imported, got {last:?}"
    );
    // The good release was NOT blocklisted; the bad one still is.
    let blocked = store.blocked_keys().await.unwrap();
    assert!(blocked.contains(&bad_key), "bad release stays blocked");
    assert!(
        !blocked.contains(&good_key),
        "the successfully-imported release must not be blocklisted; got {blocked:?}"
    );
    // Exactly one Imported notification (the successful 720p grab).
    assert_eq!(
        recorder.lock().unwrap().len(),
        1,
        "one successful import notify"
    );

    runner.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Live acquisition transparency (SKADI-T-0190): while a run is in flight the
// process-global tracker exposes the stage, the chosen release, the candidate
// count, and the decision — which `/activity` serves verbatim.
// ---------------------------------------------------------------------------

/// A downloader that snatches instantly but lingers in `status` (the `monitor`
/// stage). The delay is *after* `snatch`, so by the time the observer catches the
/// run, `record_decision_history` has already set the tracker's `decision` — letting
/// the test assert the chosen release, candidate count, stage, AND decision together.
struct LingeringDownloader {
    id: DownloaderId,
    completed: Vec<std::path::PathBuf>,
}
#[async_trait]
impl Downloader for LingeringDownloader {
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
            native_id: "lingering".into(),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        // Linger in the download stage so the observer can snapshot a run that has
        // already been snatched (decision recorded) but not yet imported.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        Ok(DownloadStatus::Completed {
            files: self.completed.clone(),
        })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn activity_tracker_exposes_stage_chosen_and_decision_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let lo = find("Bluray-720p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "activity".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(LingeringDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(library))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let seed = AcquireSeed {
        acquirable: AcquirableRef("ed-act".into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };

    // Observe the global tracker concurrently with the run (single-threaded
    // runtime: the observer's sleeps interleave with the run's lingering download).
    // Wait for `decision` specifically — it's set at the snatch boundary, so its
    // presence proves the full chosen/candidates/decision triple is wired.
    let observed = std::sync::Mutex::new(None);
    let checker = async {
        for _ in 0..80 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Some(m) = skadi_hunter::tracker()
                .snapshot()
                .into_iter()
                .find(|m| m.acquirable_ref == "ed-act" && m.decision.is_some())
            {
                *observed.lock().unwrap() = Some(m);
                break;
            }
        }
    };
    let (res, _) = tokio::join!(start_acquire_2(&runner, seed), checker);
    res.unwrap();

    let m = observed
        .lock()
        .unwrap()
        .clone()
        .expect("a snatched, in-flight run should be visible on /activity mid-run");
    // The stub suffixes each search with a counter so distinct runs see distinct
    // release names (see `OneShotIndexer::search`); the title still identifies the
    // release the run chose, which is what /activity exposes.
    assert!(
        m.chosen_title
            .as_deref()
            .is_some_and(|t| t.starts_with("Movie.2020.1080p.BluRay.x264-GRP")),
        "the chosen release title is exposed: {:?}",
        m.chosen_title
    );
    assert_eq!(
        m.candidates_considered,
        Some(1),
        "the candidate count is exposed"
    );
    // The classified profile decision is exposed (set at the snatch boundary).
    assert!(
        matches!(
            m.decision.as_deref(),
            Some("Accept" | "MeetsCutoff" | "Upgrade")
        ),
        "the decision label should be exposed, got {:?}",
        m.decision
    );
    assert!(
        matches!(
            m.current_stage.as_str(),
            "snatching" | "downloading" | "importing"
        ),
        "stage should be at/after snatch, got {:?}",
        m.current_stage
    );

    // After the run completes the tracker is empty again (best-effort, in-memory).
    assert!(
        !skadi_hunter::tracker()
            .snapshot()
            .iter()
            .any(|m| m.acquirable_ref == "ed-act"),
        "the finished run is removed from /activity"
    );

    runner.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// RSS fast pass (SKADI-T-0192): rss_sweep pulls each indexer's recent feed and
// starts acquire runs ONLY for wanted items whose title appears in the feed —
// a cheap trigger, not a blind grab (start_acquire still searches + decides).
// ---------------------------------------------------------------------------

/// An indexer that serves the same release from both its RSS feed and a search,
/// so an RSS-matched wanted item can be confirmed + grabbed by `start_acquire`.
struct FeedIndexer {
    id: IndexerId,
    release: Release,
}
#[async_trait]
impl Indexer for FeedIndexer {
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
    async fn search(&self, _q: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
        Ok(vec![self.release.clone()])
    }
    async fn rss(&self) -> SkadiResult<Vec<Release>> {
        Ok(vec![self.release.clone()])
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn rss_sweep_only_acquires_wanted_items_present_in_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Sintel.2010.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let lo = find("Bluray-720p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "rss".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    // The feed (and search) offer Sintel; its parsed title is "Sintel".
    let title = "Sintel.2010.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:sintel".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(20),
        categories: Vec::new(),
        parsed: parse(title),
    };

    let status = Arc::new(InMemoryStatusSink::new());
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: status.clone(),
        indexers: vec![Arc::new(FeedIndexer {
            id: IndexerId::new(),
            release,
        })],
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
            adds: std::sync::atomic::AtomicUsize::new(0),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::crediting(
            library,
            "ed-sintel",
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services.clone());

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    // Two wanted items: "Sintel" (in the feed) and "Nonexistent Movie" (not).
    let mk = |aref: &str, title: &str| AcquireSeed {
        acquirable: AcquirableRef(aref.into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec![title.into()],
            year: Some(2010),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let query = StubQuery(vec![
        mk("ed-sintel", "Sintel"),
        mk("ed-other", "Nonexistent Movie"),
    ]);

    let started = skadi_hunter::rss_sweep(&runner, &services.indexers, &query, MediaKind::Movie, 4)
        .await
        .unwrap();

    // Only the feed-present item is searched + grabbed.
    assert_eq!(started, 1, "only the in-feed wanted item should run");
    let sintel = status
        .get_status(&AcquirableRef("ed-sintel".into()))
        .await
        .unwrap();
    assert!(
        matches!(sintel, Some(AcquisitionStatus::Imported { .. })),
        "the in-feed item should be acquired, got {sintel:?}"
    );
    // The item absent from the feed is never touched by the RSS pass.
    let other = status
        .get_status(&AcquirableRef("ed-other".into()))
        .await
        .unwrap();
    assert!(
        other.is_none(),
        "an item not in the feed must not be searched by the RSS pass, got {other:?}"
    );

    runner.shutdown().await.unwrap();
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn rss_sweep_is_a_noop_when_feed_is_empty() {
    // An indexer with the default (empty) RSS feed → nothing is triggered, even
    // though there is a wanted item.
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let any_q = defs[0].id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "rss-empty".into(),
        allowed: vec![any_q],
        cutoff: any_q,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: Arc::new(InMemoryStatusSink::new()),
        // EmptyResultIndexer uses the default rss() (empty feed).
        indexers: vec![Arc::new(EmptyResultIndexer {
            id: IndexerId::new(),
        })],
        downloaders: vec![],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services.clone());

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let seed = AcquireSeed {
        acquirable: AcquirableRef("ed-x".into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Whatever".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let started = skadi_hunter::rss_sweep(
        &runner,
        &services.indexers,
        &StubQuery(vec![seed]),
        MediaKind::Movie,
        4,
    )
    .await
    .unwrap();
    assert_eq!(started, 0, "empty feed → no acquisitions triggered");

    runner.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Manual "search all" trigger (SKADI-T-0193): request_sweep() makes a running
// HunterWorker sweep immediately, without waiting for its (here: 1h) timer.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn search_all_trigger_runs_a_sweep_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let lo = find("Bluray-720p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "trigger".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));
    let services = make_e2e_services(library, src, recorder.clone(), profile.clone(), defs, store);
    reset_services();

    let target = skadi_hunter::cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let seed = AcquireSeed {
        acquirable: AcquirableRef("ed-trigger".into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };

    // A 1-hour sweep interval: the scheduled timer will NOT fire during this test,
    // so any sweep we observe came from the manual trigger.
    let worker = Box::new(HunterWorker::new(
        services,
        Arc::new(runner),
        std::time::Duration::from_secs(3600),
        Arc::new(StubQuery(vec![seed])),
        4,
    ));
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(worker.run(cancel.clone()));

    // Let the worker install services + park in its select loop.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        recorder.lock().unwrap().is_empty(),
        "no sweep should have run before the trigger (1h timer)"
    );

    // Poke it: POST /search-all calls exactly this.
    skadi_hunter::request_sweep();

    // The manual sweep should grab + import + notify within a few seconds.
    let mut grabbed = false;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if !recorder.lock().unwrap().is_empty() {
            grabbed = true;
            break;
        }
    }
    assert!(
        grabbed,
        "request_sweep() should have driven a sweep → grab → notify"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    drain_tracker().await;
}

/// An `ImporterFactory` that always fails to resolve a per-run importer — this
/// models *our own infra* failing (a per-run snapshot lookup), NOT the release
/// being bad. Per SKADI-T-0188 it must stay on the plain retryable path and must
/// NOT blocklist the release.
struct FailingImporterFactory;
#[async_trait]
impl skadi_hunter::ImporterFactory for FailingImporterFactory {
    async fn for_acquirable(&self, _a: &AcquirableRef) -> SkadiResult<Arc<dyn Importer>> {
        Err(AppError::Internal("importer factory unavailable".into()))
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn importer_factory_error_does_not_blocklist_the_release() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let hi = find("Bluray-1080p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "factory-fail".into(),
        allowed: vec![hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:factory".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let expected_key = skadi_indexers::release_key(&release);

    let status = Arc::new(InMemoryStatusSink::new());
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        // The download completes fine; the FACTORY (not the file) is what fails.
        downloaders: vec![Arc::new(ImmediateDownloader {
            id: DownloaderId::new(),
            completed: vec![src.clone()],
            adds: std::sync::atomic::AtomicUsize::new(0),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: Some(Arc::new(FailingImporterFactory)),
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();

    let acquirable = AcquirableRef("ed-fac".into());
    let seed = AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let _ = start_acquire_2(&runner, seed).await.unwrap();

    // The release is NOT blocklisted — an infra (factory) failure is retryable, not
    // the release's fault.
    let blocked = store.blocked_keys().await.unwrap();
    assert!(
        !blocked.contains(&expected_key),
        "an importer-factory error must NOT blocklist the release; got {blocked:?}"
    );
    // But the item is still Failed with a retry-after backoff (it'll be re-swept).
    let last = status.get_status(&acquirable).await.unwrap();
    assert!(
        matches!(
            last,
            Some(AcquisitionStatus::Failed {
                retry_at: Some(_),
                ..
            })
        ),
        "expected Failed with a retry_at backoff, got {last:?}"
    );

    runner.shutdown().await.unwrap();
}

/// Counts `add` calls and keeps every transfer "downloading" so a run stays in
/// flight for as long as the test wants.
struct CountingDownloader {
    id: DownloaderId,
    adds: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl Downloader for CountingDownloader {
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
        let n = self.adds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(DownloadHandle {
            native_id: format!("counted-{n}"),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        Ok(DownloadStatus::Downloading { progress: 0.1 })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

/// SKADI-T-0388: two `acquire_release` workflows for the same acquirable were
/// both queued behind a blocked scheduler, the daemon restarted (fresh, empty
/// tracker), and Cloacina recovery replayed **both** — the second must not grab
/// the release again, and the sweep must see the acquirable as in flight.
///
/// Drives the step bodies directly with two persisted states that carry
/// different tracker run ids, exactly what the replayed contexts hold. A real
/// mid-run crash-resume through Cloacina isn't reproducible in 0.6.1 (its
/// stale-claim window is a fixed 60 s + 30 s), see
/// `cloacina_state_persists_across_runner_restarts`.
#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn replayed_duplicate_workflow_after_restart_snatches_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "replay".into(),
        allowed: vec![hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let status = Arc::new(InMemoryStatusSink::new());
    let adds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:replay".into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store,
        status: status.clone(),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release: release.clone(),
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(CountingDownloader {
            id: DownloaderId::new(),
            adds: adds.clone(),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::new(
            dir.path().to_path_buf(),
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let acquirable = AcquirableRef("ed-replay".into());
    let tracker = skadi_hunter::tracker();
    // "Restart": nothing is tracked for this acquirable.
    assert!(!tracker.is_active(&acquirable.0));

    // Two workflows launched pre-restart under different run ids, both with
    // the same chosen release and no handle yet (neither reached snatch).
    let replayed = |run_id: &str| {
        let mut state = AcquireState::new(
            acquirable.clone(),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            profile.id,
        );
        state.run_id = Some(run_id.into());
        state.candidates = vec![release.clone()];
        state.chosen = Some(release.clone());
        state.into_context().unwrap()
    };
    let mut ctx_a = replayed("run-a");
    let mut ctx_b = replayed("run-b");

    // Replay A: adopts the run and snatches.
    skadi_hunter::steps::snatch(&mut ctx_a).await.unwrap();
    assert_eq!(adds.load(std::sync::atomic::Ordering::SeqCst), 1);
    let a = skadi_hunter::load_state(&ctx_a).unwrap();
    assert!(a.handle.is_some());
    assert!(!a.terminal_failure);
    let tracked = tracker.snapshot();
    let mine = tracked
        .iter()
        .find(|m| m.acquirable_ref == acquirable.0)
        .expect("replay A is tracked");
    assert_eq!(mine.run_id, "run-a");
    assert!(mine.adopted);

    // The sweep, meanwhile, sees the acquirable in flight and stays out.
    assert!(!tracker.try_start("sweep", MediaKind::Movie, acquirable.0.clone()));

    // Replay B: foreign run already in flight → superseded, no second grab.
    skadi_hunter::steps::snatch(&mut ctx_b).await.unwrap();
    assert_eq!(
        adds.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the duplicate replay must not grab again"
    );
    let b = skadi_hunter::load_state(&ctx_b).unwrap();
    assert!(b.handle.is_none());
    assert!(b.terminal_failure, "duplicate ends terminally at snatch");
    // ...and its remaining steps are no-ops.
    skadi_hunter::steps::monitor(&mut ctx_b).await.unwrap();
    skadi_hunter::steps::import(&mut ctx_b).await.unwrap();
    skadi_hunter::steps::notify(&mut ctx_b).await.unwrap();
    assert_eq!(adds.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Ending B did not evict A's tracker entry; A is still the run in flight.
    let still = tracker.snapshot();
    let a_meta = still
        .iter()
        .find(|m| m.acquirable_ref == acquirable.0)
        .expect("replay A still tracked after B ended");
    assert_eq!(a_meta.run_id, "run-a");
    assert!(!tracker.try_start("sweep", MediaKind::Movie, acquirable.0.clone()));

    // Status reflects exactly one snatch (B never wrote over it).
    assert!(matches!(
        status.get_status(&acquirable).await.unwrap(),
        Some(AcquisitionStatus::Snatched { .. })
    ));

    // Cleanup so later serial tests start from an empty tracker.
    tracker.finish_adopted(&acquirable.0, Some("run-a"));
    assert!(!tracker.is_active(&acquirable.0));
}

// ---------------------------------------------------------------------------
// SKADI-T-0689: the run tells the download row what it is for and how its
// import went, so the Downloads page can offer a manual import of it.
// ---------------------------------------------------------------------------

/// A downloader backed by the real `downloads` queue: `add` enqueues a row (its
/// `acquirable_ref` is the release title, as `DbDownloader` does) and completes
/// it at once with the given files; the row id is the handle.
struct QueueDownloader {
    id: DownloaderId,
    store: Store,
    completed: Vec<std::path::PathBuf>,
}

#[async_trait]
impl Downloader for QueueDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, r: &Release, c: &Category) -> SkadiResult<DownloadHandle> {
        use skadi_store::DownloadJobRepo;
        let job = self
            .store
            .enqueue(&skadi_store::NewDownloadJob {
                acquirable_ref: r.title.clone(),
                source: "magnet:?xt=urn:btih:queue".into(),
                category: Some(format!("{}", c.0)),
                incomplete_dir: None,
                complete_dir: None,
            })
            .await?;
        let files: Vec<String> = self
            .completed
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.store.mark_complete(&job.id, &files).await?;
        Ok(DownloadHandle {
            native_id: job.id,
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

/// A matcher that matches nothing: every file is rejected, so the import fails.
struct MatchNothing;
impl AcquirableMatcher for MatchNothing {
    fn match_file(
        &self,
        _parsed: &skadi_quality::ParsedRelease,
        _source: &Path,
        _completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        Vec::new()
    }
}

/// Run one acquire of `ed-row` through a [`QueueDownloader`] with `importer`,
/// and return the download row it left.
async fn acquire_through_the_queue(
    magnet: &str,
    importer: Arc<dyn Importer>,
) -> skadi_store::DownloadJob {
    use skadi_store::DownloadJobRepo;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&src, b"video").unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "queue".into(),
        allowed: vec![hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let title = "Movie.2020.1080p.BluRay.x264-GRP";
    let release = Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet(magnet.into()),
        size: 8_000_000_000,
        published: Utc::now(),
        seeders: Some(42),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release,
            searches: std::sync::atomic::AtomicUsize::new(0),
        })],
        downloaders: vec![Arc::new(QueueDownloader {
            id: DownloaderId::new(),
            store: store.clone(),
            completed: vec![src.clone()],
        })],
        importer,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);
    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();
    let seed = AcquireSeed {
        acquirable: AcquirableRef("ed-row".into()),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };
    let _ = start_acquire_2(&runner, seed).await.unwrap();
    drain_tracker().await;
    runner.shutdown().await.unwrap();

    let rows = store.list_downloads().await.unwrap();
    assert_eq!(rows.len(), 1, "one transfer enqueued: {rows:?}");
    rows.into_iter().next().unwrap()
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn a_grab_records_its_target_and_a_good_import_on_the_download_row() {
    let library = tempfile::tempdir().unwrap();
    let job = acquire_through_the_queue(
        "magnet:?xt=urn:btih:rowok",
        Arc::new(DefaultImporter::new(PlaceInDir::crediting(
            library.path().to_path_buf(),
            "ed-row",
        ))),
    )
    .await;
    // The row's own ref is the release title, not the item.
    assert!(
        job.acquirable_ref.starts_with("Movie.2020.1080p"),
        "{}",
        job.acquirable_ref
    );
    assert_eq!(job.target_kind.as_deref(), Some("movie"));
    assert_eq!(job.target_ref.as_deref(), Some("ed-row"));
    assert_eq!(job.import_state, Some(skadi_store::ImportState::Imported));
    assert_eq!(job.import_error, None);
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn a_rejected_import_is_recorded_as_failed_on_the_download_row() {
    let job = acquire_through_the_queue(
        "magnet:?xt=urn:btih:rowbad",
        Arc::new(DefaultImporter::new(MatchNothing)),
    )
    .await;
    assert_eq!(job.target_ref.as_deref(), Some("ed-row"));
    assert_eq!(job.import_state, Some(skadi_store::ImportState::Failed));
    assert!(
        job.import_error.as_deref().is_some_and(|e| !e.is_empty()),
        "the reason is kept: {:?}",
        job.import_error
    );
}

// ---------------------------------------------------------------------------
// Operator actions from an Activity row (SKADI-T-0691). The operator removes a
// live run's transfer: on its own (Remove), after blocklisting its release
// (Remove and blocklist), or to search afresh (Retry). The downloader below
// plays the operator's part on its first status poll — exactly what the API
// does to the store and tracker — then reports the transfer `Removed`, as the
// built-in client does for a `remove_requested` row.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum OperatorAction {
    Remove,
    RemoveAndBlocklist,
    Retry,
}

/// Fetches the "bad" (1080p) release until the operator takes it down; the
/// good (720p) one completes.
struct OperatorRemovesDownloader {
    id: DownloaderId,
    good_file: std::path::PathBuf,
    store: Store,
    action: OperatorAction,
    acquirable: String,
    bad_key: String,
    bad_title: String,
}
#[async_trait]
impl Downloader for OperatorRemovesDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, r: &Release, c: &Category) -> SkadiResult<DownloadHandle> {
        let tag = if r.title.contains("1080p") {
            "bad"
        } else {
            "good"
        };
        Ok(DownloadHandle {
            native_id: tag.into(),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        if h.native_id != "bad" {
            return Ok(DownloadStatus::Completed {
                files: vec![self.good_file.clone()],
            });
        }
        match self.action {
            OperatorAction::Remove => {}
            OperatorAction::RemoveAndBlocklist => {
                // What the Activity row does first: block the run's release
                // (POST /blocklist, with the key from /decisions).
                self.store
                    .block(&skadi_store::NewBlocklistEntry {
                        release_key: self.bad_key.clone(),
                        title: self.bad_title.clone(),
                        acquirable_ref: Some(self.acquirable.clone()),
                        indexer: None,
                        reason: Some("removed from the queue".into()),
                        expires_at: None,
                    })
                    .await?;
            }
            OperatorAction::Retry => {
                assert!(skadi_hunter::tracker().request_retry(&self.acquirable));
            }
        }
        Ok(DownloadStatus::Removed)
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

/// One item with two candidates, the best (1080p) fetched first; the operator
/// takes its transfer down with `action`. Runs one acquire, then a second one
/// (the next sweep), and returns what the test needs to judge them.
struct RemovalRun {
    store: Store,
    status: Arc<InMemoryStatusSink>,
    acquirable: AcquirableRef,
    bad_key: String,
    good_key: String,
    lo: skadi_core::QualityId,
    /// The status right after the first run (the removal).
    after_removal: Option<AcquisitionStatus>,
    /// The blocklist right after the first run.
    blocked_after_removal: Vec<skadi_store::BlocklistEntry>,
}

async fn run_operator_removal(action: OperatorAction, second_run: bool) -> RemovalRun {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let good_src = dir.path().join("Movie.2020.720p.BluRay.x264-OPS.mkv");
    std::fs::write(&good_src, b"video").unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&skadi_url).unwrap();
    store.run_migrations().await.unwrap();

    let defs = default_definitions();
    let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
    let lo = find("Bluray-720p");
    let hi = find("Bluray-1080p");
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "operator-remove".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };
    let bad_title = "Movie.2020.1080p.BluRay.x264-OPS";
    let good_title = "Movie.2020.720p.BluRay.x264-OPS";
    let mk = |title: &str, hash: &str, size: i64, seeders: u32| Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch: ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{hash}")),
        size: size as u64,
        published: Utc::now(),
        seeders: Some(seeders),
        categories: Vec::new(),
        parsed: parse(title),
    };
    let bad = mk(bad_title, "opsbad1080", 8_000_000_000, 99);
    let good = mk(good_title, "opsgood720", 5_000_000_000, 50);
    let bad_key = skadi_indexers::release_key(&bad);
    let good_key = skadi_indexers::release_key(&good);
    let acquirable = AcquirableRef(format!("ed-ops-{action:?}").to_lowercase());

    let status = Arc::new(InMemoryStatusSink::new());
    let services = Arc::new(HunterServices {
        kind: skadi_core::MediaKind::Movie,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![Arc::new(MultiIndexer {
            id: IndexerId::new(),
            releases: vec![bad, good],
        })],
        downloaders: vec![Arc::new(OperatorRemovesDownloader {
            id: DownloaderId::new(),
            good_file: good_src.clone(),
            store: store.clone(),
            action,
            acquirable: acquirable.0.clone(),
            bad_key: bad_key.clone(),
            bad_title: bad_title.into(),
        })],
        importer: Arc::new(DefaultImporter::new(PlaceInDir::crediting(
            library.clone(),
            &acquirable.0,
        ))) as Arc<dyn Importer>,
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services);

    let target = cloacina_target_for(&skadi_url).unwrap();
    let runner = build_runner_for(&target).await.unwrap();
    let mk_seed = || AcquireSeed {
        acquirable: acquirable.clone(),
        request: SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec!["Movie".into()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: profile.id,
        current_quality: None,
        current_format_score: None,
        current_unplayable: false,
    };

    let first = start_acquire_2(&runner, mk_seed()).await.unwrap();
    assert!(matches!(first, Outcome2::Started(_)), "{first:?}");
    drain_tracker().await;
    let after_removal = status.get_status(&acquirable).await.unwrap();
    let blocked_after_removal = store.list_blocklist().await.unwrap();
    if second_run {
        let next = start_acquire_2(&runner, mk_seed()).await.unwrap();
        assert!(matches!(next, Outcome2::Started(_)), "{next:?}");
        drain_tracker().await;
    }
    runner.shutdown().await.unwrap();
    // Keep the temp dir alive until the runner is down.
    drop(dir);
    RemovalRun {
        store,
        status,
        acquirable,
        bad_key,
        good_key,
        lo,
        after_removal,
        blocked_after_removal,
    }
}

fn retry_at(status: &Option<AcquisitionStatus>) -> chrono::DateTime<Utc> {
    match status {
        Some(AcquisitionStatus::Failed {
            retry_at: Some(at), ..
        }) => *at,
        other => panic!("expected Failed with a retry_at, got {other:?}"),
    }
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn remove_and_blocklist_from_activity_makes_the_next_sweep_take_another_release() {
    let r = run_operator_removal(OperatorAction::RemoveAndBlocklist, true).await;
    // The operator's block is the only one: the hunter did not add its own
    // auto-block on top (the removal was not the release's failure).
    let keys: Vec<_> = r
        .blocked_after_removal
        .iter()
        .map(|e| (e.release_key.clone(), e.reason.clone()))
        .collect();
    assert_eq!(
        keys,
        vec![(r.bad_key.clone(), Some("removed from the queue".into()))]
    );
    // Due at once: the bad release cannot come back, so there is nothing to
    // back off from.
    assert!(retry_at(&r.after_removal) <= Utc::now());
    // The next sweep chose the other release and imported it.
    let last = r.status.get_status(&r.acquirable).await.unwrap();
    assert!(
        matches!(last, Some(AcquisitionStatus::Imported { quality, .. }) if quality == r.lo),
        "the next sweep should import the 720p release, got {last:?}"
    );
    let blocked = r.store.blocked_keys().await.unwrap();
    assert!(blocked.contains(&r.bad_key));
    assert!(!blocked.contains(&r.good_key));
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn a_plain_remove_from_activity_blocklists_nothing_and_backs_off() {
    let r = run_operator_removal(OperatorAction::Remove, false).await;
    assert!(
        r.blocked_after_removal.is_empty(),
        "a remove without blocklist must leave the release grabbable: {:?}",
        r.blocked_after_removal
    );
    assert!(
        retry_at(&r.after_removal) > Utc::now(),
        "a plain remove waits out the transfer backoff"
    );
}

#[tokio::test]
#[serial_test::serial(hunter_registry)]
async fn retry_from_activity_blocklists_nothing_and_searches_again_at_once() {
    let r = run_operator_removal(OperatorAction::Retry, false).await;
    assert!(r.blocked_after_removal.is_empty());
    assert!(retry_at(&r.after_removal) <= Utc::now());
}
