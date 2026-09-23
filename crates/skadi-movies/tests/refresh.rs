//! Metadata-refresh workflow + circuit breaker integration test (SKADI-T-0041).
//!
//! Drives the real `refresh_movie_metadata` **Cloacina workflow** through a
//! `DefaultRunner` against a scripted [`FlakyProvider`] that errors N times then
//! recovers. Asserts the per-provider [`CircuitBreaker`]:
//! - trips after `threshold` consecutive failures,
//! - blocks runs for the cool-off window **without hitting the provider**,
//! - auto-recovers on a successful probe once the cool-off elapses, and that the
//!   refreshed metadata is then persisted to the repo.
//!
//! Each `runner.execute("refresh_movie_metadata", ctx)` is a genuine Cloacina
//! workflow execution (status assertions below prove Failed → Completed).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use diesel::connection::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::MigrationHarness;
use serde_json::json;

use cloacina::Context;
use cloacina::executor::WorkflowExecutor;

use skadi_core::{
    AppError, ExternalIds, MediaKind, ProfileId, Result, RootFolder, RootFolderId, TmdbId,
};
use skadi_hunter::{CircuitBreaker, build_runner};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_movies::movie::Movie;
use skadi_movies::repo::MovieFilter;
use skadi_movies::{
    MoviesRepo, RefreshServices, SQLITE_MIGRATIONS, reset_refresh_services, set_refresh_services,
};
use skadi_store::Store;

async fn fresh_store(dir: &std::path::Path) -> Store {
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

/// A provider that errors while `healthy` is false and returns a fixed,
/// "refreshed" record once flipped healthy. Counts every `lookup` so the test
/// can prove the breaker short-circuits before the provider is touched.
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
        kind == MediaKind::Movie
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
            external_ids: ExternalIds::default(),
            title: "The Matrix (refreshed)".into(),
            original_title: Some("The Matrix".into()),
            overview: Some("Now with fresh metadata.".into()),
            runtime_minutes: Some(136),
            release_date: NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![],
            ..Default::default()
        })
    }
}

/// Build the per-run context carrying `movie_id` (mirrors the worker's private
/// `refresh_context`; the key is the workflow's `MOVIE_ID_KEY`).
fn ctx_for(movie_id: &str) -> Context<serde_json::Value> {
    let mut ctx = Context::new();
    ctx.insert("movie_id", json!(movie_id)).unwrap();
    ctx
}

#[tokio::test]
async fn refresh_breaker_trips_blocks_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let store = fresh_store(dir.path()).await;
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // Seed a monitored movie with a TMDB id and stale title (never refreshed).
    let movie = Movie {
        content_rating: None,
        genres: Vec::new(),
        id: skadi_core::MovieId::new(),
        external_ids: ExternalIds {
            tmdb: Some(TmdbId(603)),
            ..Default::default()
        },
        title: "Old Title".into(),
        original_title: None,
        year: Some(1999),
        overview: None,
        runtime_minutes: None,
        poster_url: None,
        backdrop_url: None,
        collection: None,
        monitored: true,
        profile: ProfileId::new(),
        root_folder: RootFolder {
            id: RootFolderId::new(),
            path: dir.path().join("library"),
        },
        added_at: Utc::now(),
        last_metadata_refresh: None,
        editions: vec![],
    };
    store.upsert_movie(&movie).await.unwrap();
    let movie_id = movie.id.to_string();

    let provider = Arc::new(FlakyProvider {
        healthy: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    // Threshold 2. Cool-off is generous (2s) so the "open" window robustly
    // survives Cloacina's run finalization between the trip and the assertions;
    // the recovery leg sleeps past it.
    let breaker = Arc::new(CircuitBreaker::new(
        2,
        Duration::from_secs(2),
        Duration::from_secs(2),
    ));
    let repo: Arc<dyn MoviesRepo> = Arc::new(store.clone());

    reset_refresh_services();
    set_refresh_services(Arc::new(RefreshServices {
        provider: provider.clone(),
        repo: repo.clone(),
        breaker: breaker.clone(),
    }));

    let runner = build_runner(&skadi_url).await.unwrap();

    // --- Failures trip the breaker (provider unhealthy) ---
    let r1 = runner
        .execute("refresh_movie_metadata", ctx_for(&movie_id))
        .await
        .expect("workflow executes");
    assert!(
        matches!(r1.status, cloacina::WorkflowStatus::Failed),
        "first run fails (provider down), got {:?}",
        r1.status
    );
    assert_eq!(provider.calls(), 1, "provider hit once");
    assert!(!breaker.is_open("flaky"), "one failure < threshold");

    let r2 = runner
        .execute("refresh_movie_metadata", ctx_for(&movie_id))
        .await
        .expect("workflow executes");
    assert!(matches!(r2.status, cloacina::WorkflowStatus::Failed));
    assert_eq!(provider.calls(), 2, "provider hit twice");
    assert!(breaker.is_open("flaky"), "two failures trip the breaker");

    // --- Blocked during cool-off: the run fails WITHOUT touching the provider ---
    let r3 = runner
        .execute("refresh_movie_metadata", ctx_for(&movie_id))
        .await
        .expect("workflow executes");
    assert!(
        matches!(r3.status, cloacina::WorkflowStatus::Failed),
        "open breaker fails the run fast"
    );
    assert_eq!(
        provider.calls(),
        2,
        "breaker short-circuited before the provider was called"
    );

    // --- Recover: provider healthy + cool-off elapsed → probe succeeds ---
    provider.healthy.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2200)).await;

    let r4 = runner
        .execute("refresh_movie_metadata", ctx_for(&movie_id))
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

    // Refreshed metadata is persisted.
    let refreshed = store
        .list_movies(MovieFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.id == movie.id)
        .expect("movie still present");
    assert_eq!(refreshed.title, "The Matrix (refreshed)");
    assert!(
        refreshed.last_metadata_refresh.is_some(),
        "last_metadata_refresh stamped"
    );

    reset_refresh_services();
    runner.shutdown().await.unwrap();
}
