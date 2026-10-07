//! Supervisor reconciliation tests (SKADI-T-0052).
//!
//! Uses a deterministic counting domain (no Cloacina runner) to prove the
//! supervisor starts workers when a domain is enabled, cancels + awaits them
//! when disabled, is idempotent while steady, and — critically — does not panic
//! on re-enable, which verifies the drop-before-respawn guarantee.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use tokio_util::sync::CancellationToken;

use skadi_api::Supervisor;
use skadi_core::{BoxFuture, BoxedWorker, DomainModule, MediaKind, Worker};
use skadi_store::{DomainStateRepo, Store};
use skadi_testsupport::TestDb;

const TEST_SQLITE: EmbeddedMigrations = embed_migrations!("test_migrations/sqlite");
const TEST_POSTGRES: EmbeddedMigrations = embed_migrations!("test_migrations/postgres");

struct CountingWorker {
    started: Arc<AtomicUsize>,
    stopped: Arc<AtomicUsize>,
}

impl Worker for CountingWorker {
    fn name(&self) -> &str {
        "counting"
    }
    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            cancel.cancelled().await;
            self.stopped.fetch_add(1, Ordering::SeqCst);
        })
    }
}

struct CountingModule {
    started: Arc<AtomicUsize>,
    stopped: Arc<AtomicUsize>,
}

impl DomainModule for CountingModule {
    fn name(&self) -> &'static str {
        "counter"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    fn sqlite_migrations(&self) -> EmbeddedMigrations {
        TEST_SQLITE
    }
    fn postgres_migrations(&self) -> EmbeddedMigrations {
        TEST_POSTGRES
    }
    fn workers(&self) -> Vec<BoxedWorker> {
        vec![Box::new(CountingWorker {
            started: self.started.clone(),
            stopped: self.stopped.clone(),
        })]
    }
}

async fn temp_store() -> (Store, TestDb) {
    // Postgres-default isolated DB (SQLite fallback). Caller keeps the `TestDb`
    // guard alive for the test's duration (SKADI-T-0077).
    let db = TestDb::new_store_only().await;
    (db.store.clone(), db)
}

/// Poll until `counter` reaches `target` or time out (workers start on the tokio
/// executor asynchronously, so the count lags the spawning `tick`).
async fn wait_for(counter: &AtomicUsize, target: usize) {
    for _ in 0..200 {
        if counter.load(Ordering::SeqCst) >= target {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "counter never reached {target}; got {}",
        counter.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn reconciles_workers_against_enable_state() {
    let (store, _db) = temp_store().await;
    let started = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicUsize::new(0));
    let module = Arc::new(CountingModule {
        started: started.clone(),
        stopped: stopped.clone(),
    });
    let sup = Supervisor::new(store.clone(), vec![module]);

    // Disabled by default → tick is a no-op.
    sup.tick().await.unwrap();
    assert_eq!(sup.running_domains().await, 0);
    assert_eq!(started.load(Ordering::SeqCst), 0);

    // Enable → one worker starts.
    store.set_enabled("counter", true).await.unwrap();
    sup.tick().await.unwrap();
    assert_eq!(sup.running_domains().await, 1);
    wait_for(&started, 1).await;

    // Steady state → tick does not restart.
    sup.tick().await.unwrap();
    assert_eq!(started.load(Ordering::SeqCst), 1);

    // Disable → worker is cancelled and awaited (stopped incremented) before
    // the entry is dropped.
    store.set_enabled("counter", false).await.unwrap();
    sup.tick().await.unwrap();
    assert_eq!(sup.running_domains().await, 0);
    assert_eq!(stopped.load(Ordering::SeqCst), 1);

    // Re-enable → must NOT panic (drop-before-respawn upheld) and a fresh worker
    // starts.
    store.set_enabled("counter", true).await.unwrap();
    sup.tick().await.unwrap();
    wait_for(&started, 2).await;
    assert_eq!(sup.running_domains().await, 1);

    // Shutdown stops everything.
    sup.stop_all().await;
    assert_eq!(sup.running_domains().await, 0);
    assert_eq!(stopped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn run_loop_brings_up_enabled_domain_and_stops_on_cancel() {
    let (store, _db) = temp_store().await;
    let started = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicUsize::new(0));
    store.set_enabled("counter", true).await.unwrap();
    let module = Arc::new(CountingModule {
        started: started.clone(),
        stopped: stopped.clone(),
    });
    let sup = Arc::new(Supervisor::new(store.clone(), vec![module]));

    let cancel = CancellationToken::new();
    let loop_handle = tokio::spawn(sup.clone().run(cancel.clone(), Duration::from_millis(20)));

    wait_for(&started, 1).await;
    cancel.cancel();
    loop_handle.await.unwrap();

    assert_eq!(stopped.load(Ordering::SeqCst), 1);
}

/// SKADI-T-0680: the supervisor tick refreshes the health checks with no request,
/// in the background, and a later tick past the TTL moves `checked_at` on.
#[tokio::test]
async fn the_tick_refreshes_the_health_checks_without_a_request() {
    use skadi_api::health_checks::{CheckContext, Severity};

    let (store, _db) = temp_store().await;
    let state = skadi_api::AppState::new(
        skadi_api::Config {
            database_url: "sqlite://:memory:".into(),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            bearer_token: None,
        },
        Some(store.clone()),
    );
    let ctx = CheckContext::from_state(&state).unwrap();
    let database = |snap: Vec<skadi_api::health_checks::CheckResult>| {
        snap.into_iter().find(|r| r.id == "database").unwrap()
    };
    assert_eq!(
        database(state.health.snapshot(&ctx).await).severity,
        Severity::Pending
    );

    let sup = Supervisor::new(store, vec![]).with_live_token(state.clone());
    sup.tick().await.unwrap();
    let mut first = None;
    for _ in 0..200 {
        let db = database(state.health.snapshot(&ctx).await);
        if let Some(at) = db.checked_at {
            assert_eq!(db.severity, Severity::Ok);
            first = Some(at);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let first = first.expect("the tick ran the database check");

    // Fresh: the next tick does not run it again.
    sup.tick().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        database(state.health.snapshot(&ctx).await).checked_at,
        Some(first)
    );
}
