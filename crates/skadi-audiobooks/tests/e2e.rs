//! End-to-end test for the audiobooks domain (SKADI-T-0130).
//!
//! The phase-2 capstone: drives the real `acquire` Cloacina workflow against a
//! real runner with a mock indexer (returns one audiobook release) / mock
//! downloader (completes with an M4B) / recording notifier / real
//! `DefaultImporter` (via an `AudiobookMatcher` importer-factory), from a seeded
//! monitored Book with a Missing BookFile to **Imported** — the file placed at
//! the canonical Author/Book path and the status persisted on the `book_files`
//! row. Mirrors `skadi-movies::e2e`.
//!
//! Runs on SQLite by default; on Postgres when `SKADI_TEST_DATABASE_URL` is set
//! (via [`skadi_testsupport::TestDb`], which scopes the Cloacina hunter schema
//! into the same throwaway database).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;

use skadi_audiobooks::{
    AudiobookMatcher, AudiobooksRepo, Author, Book, BookFile, SQLITE_MIGRATIONS,
    default_audiobook_profile,
};
use skadi_core::{
    AcquisitionStatus, AsinId, DownloaderId, ExternalIds, IndexerId, MediaKind, NotifierId,
    Protocol, Result as SkadiResult, RootFolder,
};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_hunter::services::AudiobookScoring;
use skadi_hunter::{
    AcquireSeed, HunterServices, ImporterFactory, build_runner, services::ScoringConfig,
    set_services, start_acquire,
};
use skadi_indexers::{
    Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};
use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
use skadi_quality::audiobook::default_audiobook_definitions;
use skadi_quality::parse;
use skadi_testsupport::TestDb;

// --- mocks (mirror skadi-movies' e2e scaffolding) ---

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
        kind == MediaKind::Audiobook
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
async fn audiobooks_module_drives_a_full_acquire_run_end_to_end() {
    // Isolated DB (SQLite default; Postgres when SKADI_TEST_DATABASE_URL is set).
    let db = TestDb::new(SQLITE_MIGRATIONS, skadi_audiobooks::POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();

    // Filesystem: a downloaded M4B and the library root it will be placed under.
    let fs = tempfile::tempdir().unwrap();
    let src = fs.path().join("Andy Weir - Project Hail Mary.m4b");
    std::fs::write(&src, vec![0u8; 4 * 1024 * 1024]).unwrap();
    let library = fs.path().join("library");
    std::fs::create_dir_all(&library).unwrap();

    // Scoring: the permissive default audiobook profile (allows every default
    // definition; cutoff = M4B-256, upgrades off). The release below classifies
    // as M4B-128 — allowed and below cutoff, so a Missing file gets grabbed.
    let profile = default_audiobook_profile();

    // Seed: a monitored author + book with a Missing BookFile.
    let asin = AsinId("B08G9PRS1K".into());
    let author = Author::new("Andy Weir");
    store.upsert_author(&author).await.unwrap();
    let mut book = Book::new(
        ExternalIds {
            asin: Some(asin.clone()),
            ..Default::default()
        },
        "Project Hail Mary",
        profile.id,
        RootFolder {
            id: skadi_core::RootFolderId::new(),
            path: library.clone(),
        },
    );
    book.author_id = Some(author.id);
    book.authors = vec!["Andy Weir".into()];
    book.year = Some(2021);
    store.upsert_book(&book).await.unwrap();
    let file = BookFile::missing(book.id);
    store.upsert_book_file(&file).await.unwrap();

    // The single audiobook release the mock indexer returns: an M4B at 128 kbps.
    let title = "Andy Weir - Project Hail Mary (2021) [M4B 128kbps]";
    let recorder = Arc::new(Mutex::new(Vec::<NotificationEvent>::new()));

    let runner = build_runner(db.url()).await.unwrap();
    let repo_arc: Arc<dyn AudiobooksRepo> = Arc::new(store.clone());
    let factory: Arc<dyn ImporterFactory> = Arc::new(AudiobookImporterFactoryForTest {
        repo: repo_arc.clone(),
    });
    let svc = Arc::new(HunterServices {
        kind: MediaKind::Audiobook,
        store: store.clone(),
        status: Arc::new(skadi_audiobooks::AudiobookStatusSink::new(
            repo_arc.clone(),
            Arc::new(store.clone()),
        )),
        indexers: vec![Arc::new(OneShotIndexer {
            id: IndexerId::new(),
            release: Release {
                indexer: IndexerId::new(),
                title: title.into(),
                fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
                size: 350_000_000,
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
            definitions: vec![],
            profile: profile.clone(),
            formats: vec![],
            min_seeders: 0,
            audiobook: Some(AudiobookScoring {
                definitions: default_audiobook_definitions(),
                allow_abridged: false,
            }),
        },
    });
    skadi_hunter::services::reset_services();
    set_services(svc);

    // Drive one acquire run for this book file.
    let seed = AcquireSeed {
        acquirable: file.acquirable_ref(),
        request: skadi_hunter::SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Audiobook,
            titles: vec!["Project Hail Mary".into()],
            year: Some(2021),
            external_ids: ExternalIds {
                asin: Some(asin.clone()),
                ..Default::default()
            },
            categories: vec![Category(3030)],
            tv: None,
            series: None,
            tags: None,
        },
        profile: book.profile,
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

    // Verify: the BookFile row ended Imported with a file, the audiobook landed
    // at the canonical Author/Book destination, and one Imported notify fired.
    let row = store.get_book_file(file.id).await.unwrap().unwrap();
    assert!(
        matches!(row.status, AcquisitionStatus::Imported { .. }),
        "expected Imported, got {:?}",
        row.status
    );
    assert!(row.file.is_some(), "Imported BookFile has a file");
    let dest = library
        .join("andy-weir")
        .join("project-hail-mary_{asin-B08G9PRS1K}")
        .join("project-hail-mary.m4b");
    assert!(
        dest.exists(),
        "file placed at the canonical destination: {}",
        dest.display()
    );
    assert_eq!(recorder.lock().unwrap().len(), 1, "one Imported notify");

    runner.shutdown().await.unwrap();
}

// --- helpers ---

struct AudiobookImporterFactoryForTest {
    repo: Arc<dyn AudiobooksRepo>,
}

#[async_trait]
impl ImporterFactory for AudiobookImporterFactoryForTest {
    async fn for_acquirable(
        &self,
        acquirable: &skadi_importer::AcquirableRef,
    ) -> SkadiResult<Arc<dyn skadi_importer::Importer>> {
        // Mirrors the production AudiobookImporterFactory in src/module.rs.
        let file_id = uuid::Uuid::parse_str(&acquirable.0)
            .map(skadi_core::BookFileId::from)
            .map_err(|e| skadi_core::AppError::Validation(format!("bad ref: {e}")))?;
        let file = self.repo.get_book_file(file_id).await?.unwrap();
        let book = self.repo.get_book(file.book_id).await?.unwrap();
        let matcher = AudiobookMatcher::new(book, file);
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
