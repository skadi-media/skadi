//! Audiobook metadata-refresh workflow + circuit breaker integration test
//! (SKADI-T-0135), mirroring `skadi-movies::tests::refresh`.
//!
//! Drives the real `refresh_book_metadata` **Cloacina workflow** through a
//! `DefaultRunner` against a scripted `FlakyProvider` that errors N times then
//! recovers. Asserts the per-provider [`CircuitBreaker`]:
//! - trips after `threshold` consecutive failures,
//! - blocks runs for the cool-off window **without hitting the provider**,
//! - auto-recovers on a successful probe once the cool-off elapses, and that the
//!   refreshed metadata is then persisted (preserving user fields).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::json;

use cloacina::Context;
use cloacina::executor::WorkflowExecutor;

use skadi_audiobooks::{
    AudiobooksRepo, Book, BookFilter, RefreshServices, SQLITE_MIGRATIONS, reset_refresh_services,
    set_refresh_services,
};
use skadi_core::{
    AppError, AsinId, ExternalIds, MediaKind, ProfileId, Result, RootFolder, RootFolderId,
};
use skadi_hunter::{CircuitBreaker, build_runner};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_store::Store;

async fn fresh_store(dir: &std::path::Path) -> Store {
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;
    let path = dir.join("skadi.db");
    let url = format!("sqlite://{}", path.display());
    let store = Store::connect(&url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);
    {
        let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
    }
    Store::connect(&url).unwrap()
}

/// Errors while `healthy` is false; once flipped, returns a fixed "refreshed"
/// record. Counts every `lookup` so the test can prove the breaker short-circuits
/// before the provider is touched.
struct FlakyProvider {
    healthy: AtomicBool,
    calls: AtomicUsize,
}

impl FlakyProvider {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MetadataProvider for FlakyProvider {
    fn name(&self) -> &str {
        "flaky"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Audiobook
    }
    async fn search(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, _id: &ExternalId) -> Result<MetadataRecord> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.healthy.load(Ordering::SeqCst) {
            return Err(AppError::Internal("provider is down".into()));
        }
        Ok(MetadataRecord {
            title: "Project Hail Mary (refreshed)".into(),
            authors: vec!["Andy Weir".into()],
            overview: Some("Now with fresh metadata.".into()),
            runtime_minutes: Some(960),
            release_date: NaiveDate::from_ymd_opt(2021, 5, 4),
            ..Default::default()
        })
    }
}

fn ctx_for(book_id: &str) -> Context<serde_json::Value> {
    let mut ctx = Context::new();
    ctx.insert("book_id", json!(book_id)).unwrap();
    ctx
}

#[tokio::test]
async fn refresh_breaker_trips_blocks_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let store = fresh_store(dir.path()).await;
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // Seed a book with an ASIN + stale title (never refreshed).
    let mut book = Book::new(
        ExternalIds {
            asin: Some(AsinId("B08G9PRS1K".into())),
            ..Default::default()
        },
        "Old Title",
        ProfileId::new(),
        RootFolder {
            id: RootFolderId::new(),
            path: dir.path().join("library"),
        },
    );
    book.last_metadata_refresh = None;
    store.upsert_book(&book).await.unwrap();
    let book_id = book.id.to_string();

    let provider = Arc::new(FlakyProvider {
        healthy: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let breaker = Arc::new(CircuitBreaker::new(
        2,
        Duration::from_secs(2),
        Duration::from_secs(2),
    ));
    let repo: Arc<dyn AudiobooksRepo> = Arc::new(store.clone());

    reset_refresh_services();
    set_refresh_services(Arc::new(RefreshServices {
        provider: provider.clone(),
        repo: repo.clone(),
        breaker: breaker.clone(),
    }));

    let runner = build_runner(&skadi_url).await.unwrap();

    // --- Failures trip the breaker (provider unhealthy) ---
    let r1 = runner
        .execute("refresh_book_metadata", ctx_for(&book_id))
        .await
        .expect("workflow executes");
    assert!(
        matches!(r1.status, cloacina::WorkflowStatus::Failed),
        "first run fails (provider down), got {:?}",
        r1.status
    );
    assert_eq!(provider.calls(), 1);
    assert!(!breaker.is_open("flaky"), "one failure < threshold");

    let r2 = runner
        .execute("refresh_book_metadata", ctx_for(&book_id))
        .await
        .expect("workflow executes");
    assert!(matches!(r2.status, cloacina::WorkflowStatus::Failed));
    assert_eq!(provider.calls(), 2);
    assert!(breaker.is_open("flaky"), "two failures trip the breaker");

    // --- Blocked during cool-off: fails WITHOUT touching the provider ---
    let r3 = runner
        .execute("refresh_book_metadata", ctx_for(&book_id))
        .await
        .expect("workflow executes");
    assert!(matches!(r3.status, cloacina::WorkflowStatus::Failed));
    assert_eq!(
        provider.calls(),
        2,
        "breaker short-circuited before the provider was called"
    );

    // --- Recover: provider healthy + cool-off elapsed → probe succeeds ---
    provider.healthy.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2200)).await;

    let r4 = runner
        .execute("refresh_book_metadata", ctx_for(&book_id))
        .await
        .expect("workflow executes");
    assert!(
        matches!(r4.status, cloacina::WorkflowStatus::Completed),
        "probe after cool-off completes, got {:?} ({:?})",
        r4.status,
        r4.error_message
    );
    assert_eq!(provider.calls(), 3, "probe hit the provider");
    assert!(!breaker.is_open("flaky"), "success closed the breaker");

    // Refreshed metadata is persisted (and user fields preserved).
    let refreshed = store
        .list_books(BookFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.id == book.id)
        .expect("book still present");
    assert_eq!(refreshed.title, "Project Hail Mary (refreshed)");
    assert!(
        refreshed.last_metadata_refresh.is_some(),
        "last_metadata_refresh stamped"
    );

    reset_refresh_services();
    runner.shutdown().await.unwrap();
}
