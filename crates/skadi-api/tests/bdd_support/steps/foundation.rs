//! Foundation steps (pass P5): daemon bootstrap order (C06), the supervisor's
//! worker lifecycle and reload-error handling (C06), the provider factory's
//! credential handling (C04), the settings hot-reload contract (C03), the hunter
//! sweep trigger (C07) and the workflow engine's storage target (C08).
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cucumber::{given, then, when};
use diesel::prelude::*;
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use tokio_util::sync::CancellationToken;

use skadi_api::{Config, Supervisor};
use skadi_core::{BoxFuture, BoxedWorker, DomainModule, MediaKind, Worker};
use skadi_store::{DomainStateRepo, SettingsRepo, Store};

use crate::bdd_support::{AltStore, Sup, World};

const PROBE_SQLITE: EmbeddedMigrations = embed_migrations!("test_migrations/sqlite");
const PROBE_POSTGRES: EmbeddedMigrations = embed_migrations!("test_migrations/postgres");

/// What a probe worker does once spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    /// Run until cancelled (a healthy long-running worker).
    WaitForCancel,
    /// Return immediately (a worker that crashed / finished on its own).
    ExitImmediately,
}

#[derive(Debug)]
pub struct ProbeWorker {
    mode: WorkerMode,
    started: Arc<AtomicUsize>,
    stopped: Arc<AtomicUsize>,
}

impl Worker for ProbeWorker {
    fn name(&self) -> &str {
        "probe"
    }
    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            if self.mode == WorkerMode::WaitForCancel {
                cancel.cancelled().await;
            }
            self.stopped.fetch_add(1, Ordering::SeqCst);
        })
    }
}

/// A compiled-in domain double: real embedded migrations, counting workers.
#[derive(Debug)]
pub struct ProbeModule {
    pub name: &'static str,
    pub mode: WorkerMode,
    pub started: Arc<AtomicUsize>,
    pub stopped: Arc<AtomicUsize>,
}

impl DomainModule for ProbeModule {
    fn name(&self) -> &'static str {
        self.name
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    fn sqlite_migrations(&self) -> EmbeddedMigrations {
        PROBE_SQLITE
    }
    fn postgres_migrations(&self) -> EmbeddedMigrations {
        PROBE_POSTGRES
    }
    fn workers(&self) -> Vec<BoxedWorker> {
        vec![Box::new(ProbeWorker {
            mode: self.mode,
            started: self.started.clone(),
            stopped: self.stopped.clone(),
        })]
    }
}

/// A reloader that always fails to apply (a domain whose provider swap errors).
#[derive(Debug, Default)]
pub struct FailingReloader;

#[async_trait::async_trait]
impl skadi_api::ProviderReloader for FailingReloader {
    async fn apply(&self, _set: skadi_api::ProviderSet) -> skadi_core::Result<()> {
        Err(skadi_core::AppError::Internal(
            "provider apply failed".into(),
        ))
    }
}

fn probe(name: &'static str, mode: WorkerMode) -> Arc<ProbeModule> {
    Arc::new(ProbeModule {
        name,
        mode,
        started: Arc::new(AtomicUsize::new(0)),
        stopped: Arc::new(AtomicUsize::new(0)),
    })
}

fn registry(w: &World) -> Vec<Arc<dyn DomainModule>> {
    w.modules
        .iter()
        .map(|m| m.clone() as Arc<dyn DomainModule>)
        .collect()
}

async fn rebuild_supervisor(w: &mut World) {
    let store = w.store().await;
    let mut reloaders: Vec<Arc<dyn skadi_api::ProviderReloader>> = w
        .reloaders
        .iter()
        .map(|r| r.clone() as Arc<dyn skadi_api::ProviderReloader>)
        .collect();
    if w.failing_reloader {
        reloaders.push(Arc::new(FailingReloader));
    }
    w.supervisor = Some(Sup(Supervisor::with_reloaders(
        store,
        registry(w),
        reloaders,
    )));
}

async fn wait_for(counter: &AtomicUsize, target: usize) -> bool {
    for _ in 0..200 {
        if counter.load(Ordering::SeqCst) >= target {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

// ---- supervisor -----------------------------------------------------------------

#[given(expr = "the supervisor supervises the {string} domain whose worker {}")]
async fn supervises(w: &mut World, name: String, behaviour: String) {
    let mode = match behaviour.as_str() {
        "waits for cancellation" => WorkerMode::WaitForCancel,
        "exits immediately" => WorkerMode::ExitImmediately,
        other => panic!("unknown worker behaviour {other:?}"),
    };
    let name: &'static str = Box::leak(name.into_boxed_str());
    w.modules.push(probe(name, mode));
    rebuild_supervisor(w).await;
}

#[given("a provider reloader that always fails is registered with the supervisor")]
async fn failing(w: &mut World) {
    w.failing_reloader = true;
    rebuild_supervisor(w).await;
}

#[when("the supervisor reconciles, tolerating an error")]
async fn reconcile_tolerant(w: &mut World) {
    let r = w.supervisor.as_ref().expect("supervisor").0.tick().await;
    w.tick_err = r.err().map(|e| e.to_string());
}

#[then(expr = "the reconcile failed mentioning {string}")]
async fn tick_failed(w: &mut World, needle: String) {
    let e = w.tick_err.clone().expect("expected the tick to fail");
    assert!(e.contains(&needle), "{e:?} lacks {needle:?}");
}

#[when(expr = "the {string} domain is disabled")]
async fn disable(w: &mut World, name: String) {
    w.store()
        .await
        .set_enabled(&name, false)
        .await
        .expect("disable");
}

#[then(expr = "{int} domain(s) is/are running")]
async fn running(w: &mut World, n: usize) {
    let got = w
        .supervisor
        .as_ref()
        .expect("supervisor")
        .0
        .running_domains()
        .await;
    assert_eq!(got, n);
}

#[then(expr = "the {string} worker has started {int} time(s) and stopped {int} time(s)")]
async fn worker_counts(w: &mut World, name: String, started: usize, stopped: usize) {
    let m = w
        .modules
        .iter()
        .find(|m| m.name == name)
        .expect("probe module");
    // Workers start on the executor asynchronously; give them a moment.
    wait_for(&m.started, started).await;
    wait_for(&m.stopped, stopped).await;
    assert_eq!(m.started.load(Ordering::SeqCst), started, "started");
    assert_eq!(m.stopped.load(Ordering::SeqCst), stopped, "stopped");
}

#[when("the supervisor shuts down")]
async fn shutdown(w: &mut World) {
    w.supervisor
        .as_ref()
        .expect("supervisor")
        .0
        .stop_all()
        .await;
}

/// Sonarr's scheduler re-runs a task that died (SKADI-T-0523). The supervisor
/// used to treat a domain as running while its `running` entry was non-empty,
/// never checking whether the `JoinHandle` had finished — so a worker that
/// returned or panicked left the domain counted as live, unrestarted and
/// unreported, until the daemon was restarted.
#[then(expr = "the {string} domain is restarted or reported as not running")]
async fn dead_worker(w: &mut World, name: String) {
    let sup = &w.supervisor.as_ref().expect("supervisor").0;
    let m = w
        .modules
        .iter()
        .find(|m| m.name == name)
        .expect("probe module");

    // The failure is counted, so a crash-loop is visible: the supervisor restarts
    // the domain each tick, and without this counter it would look healthy the
    // whole time.
    let failures = sup.worker_failures().await;
    assert!(
        failures.get(&name).copied().unwrap_or(0) > 0,
        "the ended worker should be counted as a failure; got {failures:?}"
    );

    let running = sup.running_domains().await;
    let started = m.started.load(Ordering::SeqCst);
    assert!(
        started >= 2 || running == 0,
        "worker exited after {started} start(s) yet the supervisor still counts {running} \
         running domain(s) and never respawns it"
    );
}

// ---- bootstrap ----------------------------------------------------------------

#[when(expr = "the daemon bootstraps with the {string} domain compiled in")]
async fn bootstrap(w: &mut World, name: String) {
    // Make sure the scenario store exists so bootstrap and the world share a DB.
    let _ = w.store().await;
    let url = w.db.as_ref().expect("db").0.url().to_string();
    let name: &'static str = Box::leak(name.into_boxed_str());
    if !w.modules.iter().any(|m| m.name == name) {
        w.modules.push(probe(name, WorkerMode::WaitForCancel));
    }
    let config = Config {
        database_url: url,
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    skadi_api::bootstrap(&config, &registry(w))
        .await
        .expect("bootstrap");
    // Restore any env the scenario set for the seeding run.
    restore_env(w);
}

#[then(expr = "the domain {string} is registered but disabled")]
async fn registered_disabled(w: &mut World, name: String) {
    let s = w
        .store()
        .await
        .get(&name)
        .await
        .expect("get")
        .unwrap_or_else(|| panic!("{name} has no domains row"));
    assert!(!s.enabled);
    assert!(s.enabled_at.is_none());
}

#[derive(QueryableByName)]
struct TableName {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

async fn table_names(store: &Store) -> Vec<String> {
    store
        .with_conn(|conn| {
            let rows: Vec<TableName> = conn
                .dispatch(
                    |pg| {
                        diesel::sql_query(
                            "SELECT table_name AS name FROM information_schema.tables \
                             WHERE table_schema = 'public'",
                        )
                        .load(pg)
                    },
                    |sq| {
                        diesel::sql_query("SELECT name FROM sqlite_master WHERE type = 'table'")
                            .load(sq)
                    },
                )
                .map_err(|e| skadi_core::AppError::Internal(e.to_string()))?;
            Ok(rows.into_iter().map(|r| r.name).collect())
        })
        .await
        .expect("table listing")
}

#[then(expr = "the table {string} exists")]
async fn table_exists(w: &mut World, table: String) {
    let store = w.store().await;
    let names = table_names(&store).await;
    assert!(names.contains(&table), "missing {table}; have {names:?}");
}

#[then(expr = "the {word} settings kind holds {int} document(s)")]
async fn kind_count(w: &mut World, kind: String, n: usize) {
    let got = w.store().await.list_settings(&kind).await.unwrap().len();
    assert_eq!(got, n, "{kind}");
}

#[then(expr = "the {word} settings kind holds at least {int} document(s)")]
async fn kind_at_least(w: &mut World, kind: String, n: usize) {
    let got = w.store().await.list_settings(&kind).await.unwrap().len();
    assert!(got >= n, "{kind}: {got} < {n}");
}

#[when(expr = "the operator deletes one {word} document")]
async fn delete_one(w: &mut World, kind: String) {
    let store = w.store().await;
    let first = store
        .list_settings(&kind)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("a document to delete");
    assert!(store.delete_setting(&kind, &first.id).await.unwrap());
}

#[then(expr = "exactly {int} built-in downloader(s) is/are configured")]
async fn builtin_downloaders(w: &mut World, n: usize) {
    let got = w
        .store()
        .await
        .list_settings("downloaders")
        .await
        .unwrap()
        .iter()
        .filter(|s| s.body["kind"] == "skadi")
        .count();
    assert_eq!(got, n);
}

// ---- env (@serial) --------------------------------------------------------------

#[given(expr = "the process environment sets {string} to {string}")]
async fn env_set(w: &mut World, key: String, value: String) {
    w.env_touched.push((key.clone(), std::env::var(&key).ok()));
    // SAFETY: scenarios that touch the environment are tagged `@serial`.
    unsafe { std::env::set_var(&key, &value) };
}

fn restore_env(w: &mut World) {
    for (key, prior) in w.env_touched.drain(..) {
        // SAFETY: `@serial` scenario; see `env_set`.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }
    }
}

/// Open the scenario store *now*, under whatever `SKADI_SECRET_KEY` is set.
#[given("the daemon's store is opened under that key")]
async fn store_under_key(w: &mut World) {
    let _ = w.store().await;
    restore_env(w);
}

#[when(expr = "the daemon restarts with the master key {string}")]
async fn restart_with_key(w: &mut World, key: String) {
    env_set(w, "SKADI_SECRET_KEY".into(), key).await;
    let url = w.db.as_ref().expect("db").0.url().to_string();
    w.alt_store = Some(AltStore(Store::connect(&url).expect("reconnect")));
    restore_env(w);
}

// ---- provider factory (C04) ------------------------------------------------------------

async fn build_with(w: &mut World, store: Store) {
    w.build = Some(
        skadi_api::build_providers(&store)
            .await
            .map(|s| (s.indexers.len(), s.downloaders.len(), s.notifiers.len()))
            .map_err(|e| e.to_string()),
    );
}

#[when("the provider set is built from the store")]
async fn build(w: &mut World) {
    let store = w.store().await;
    build_with(w, store).await;
}

#[when("the restarted daemon builds the provider set")]
async fn build_alt(w: &mut World) {
    let store = w.alt_store.as_ref().expect("restarted store").0.clone();
    build_with(w, store).await;
}

#[then(expr = "the provider set holds {int} indexer(s), {int} downloader(s) and {int} notifier(s)")]
async fn set_counts(w: &mut World, i: usize, d: usize, n: usize) {
    match w.build.as_ref().expect("a build") {
        Ok(got) => assert_eq!(got, &(i, d, n)),
        Err(e) => panic!("provider build failed: {e}"),
    }
}

#[then(expr = "the provider build failed mentioning {string}")]
async fn build_failed(w: &mut World, needle: String) {
    match w.build.as_ref().expect("a build") {
        Err(e) => assert!(e.contains(&needle), "{e:?} lacks {needle:?}"),
        Ok(set) => panic!("expected the build to fail, got {set:?}"),
    }
}

/// "One bad row never sinks the set" (`crates/skadi-api/src/providers.rs:16-20`)
/// — except a credential that fails to decrypt: `store.get_secret(..)?` at
/// `providers.rs:140/154/192/217` aborts the whole build, the supervisor tick
/// returns early (`supervisor.rs:83`) and no domain worker ever starts.
#[then("the provider set built with the unreadable credential skipped")]
async fn skipped_bad_row(w: &mut World) {
    match w.build.as_ref().expect("a build") {
        Ok(got) => assert_eq!(got.0, 0, "the unreadable indexer must be skipped"),
        Err(e) => panic!(
            "build_providers aborted on one undecryptable credential ({e}) instead of \
             skipping that row — the supervisor tick fails every 5 s and never reaches \
             the domain reconcile"
        ),
    }
}

#[derive(QueryableByName)]
struct NonceRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Binary>)]
    nonce: Option<Vec<u8>>,
}

#[then(expr = "the credential for {word} {string} is sealed at rest")]
async fn sealed(w: &mut World, kind: String, name: String) {
    let id = w.ids.get(&name).cloned().expect("remembered id");
    let store = w.store().await;
    let nonce = store
        .with_conn(move |conn| {
            let q = "SELECT nonce FROM credentials WHERE owner_kind = $1 AND owner_id = $2";
            let q_sq = "SELECT nonce FROM credentials WHERE owner_kind = ? AND owner_id = ?";
            let rows: Vec<NonceRow> = conn
                .dispatch(
                    |pg| {
                        diesel::sql_query(q)
                            .bind::<diesel::sql_types::Text, _>(&kind)
                            .bind::<diesel::sql_types::Text, _>(&id)
                            .load(pg)
                    },
                    |sq| {
                        diesel::sql_query(q_sq)
                            .bind::<diesel::sql_types::Text, _>(&kind)
                            .bind::<diesel::sql_types::Text, _>(&id)
                            .load(sq)
                    },
                )
                .map_err(|e| skadi_core::AppError::Internal(e.to_string()))?;
            Ok(rows.into_iter().next().map(|r| r.nonce))
        })
        .await
        .expect("raw row")
        .expect("credential row");
    assert_eq!(
        nonce.map(|n| n.len()),
        Some(12),
        "sealed rows carry a 12-byte nonce"
    );
}

// ---- hunter trigger (C07) ------------------------------------------------------------

#[given("a subscription to the hunter sweep trigger")]
async fn subscribe(w: &mut World) {
    w.sweep_rx = Some(skadi_hunter::trigger::subscribe());
}

#[then("the hunter sweep trigger has been poked")]
async fn poked(w: &mut World) {
    let rx = w.sweep_rx.as_mut().expect("subscription");
    assert!(
        rx.has_changed().expect("sender alive"),
        "no sweep was requested"
    );
}

#[then("the hunter sweep trigger has not been poked")]
async fn not_poked(w: &mut World) {
    let rx = w.sweep_rx.as_mut().expect("subscription");
    assert!(
        !rx.has_changed().expect("sender alive"),
        "a sweep was requested by the settings reload"
    );
}

// ---- workflow engine (C08) -----------------------------------------------------------

#[then(expr = "the workflow engine for {string} is isolated at {string} with schema {string}")]
async fn target(_w: &mut World, url: String, want: String, schema: String) {
    let t = skadi_hunter::cloacina_target_for(&url).expect("target");
    assert_eq!(t.url, want);
    if schema == "none" {
        assert_eq!(t.schema, None);
    } else {
        assert_eq!(t.schema.as_deref(), Some(schema.as_str()));
    }
}

#[then(expr = "deriving a workflow target for {string} is a configuration error")]
async fn bad_target(_w: &mut World, url: String) {
    assert!(matches!(
        skadi_hunter::cloacina_target_for(&url),
        Err(skadi_core::AppError::Config(_))
    ));
}

#[then("ensuring the cloacina database for a sqlite URL is a no-op")]
async fn ensure_sqlite(_w: &mut World) {
    skadi_api::ensure_cloacina_database("sqlite:///tmp/does-not-matter.db")
        .await
        .expect("no-op");
}

#[when("a workflow runner is built against the scenario database and shut down")]
async fn runner_round_trip(w: &mut World) {
    let _ = w.store().await;
    let url = w.db.as_ref().expect("db").0.url().to_string();
    let runner = skadi_hunter::build_runner(&url).await.expect("runner");
    runner.shutdown().await.expect("shutdown");
    w.notes.push(format!("runner:{url}"));
}

#[then("the hunter's own database sits beside the store")]
async fn hunter_db(w: &mut World) {
    let url = w.db.as_ref().expect("db").0.url().to_string();
    if let Some(path) = url.strip_prefix("sqlite://") {
        let sibling = std::path::Path::new(path).with_file_name("hunter.db");
        assert!(sibling.exists(), "{} missing", sibling.display());
    }
}

// ---- config resolution / cadences (C03, C07) ----------------------------------------

#[when("the daemon resolves its runtime config from the table")]
async fn resolve_config(w: &mut World) {
    let store = w.store().await;
    let view = skadi_api::load_config_view(&store).await.expect("view");
    let mut config = Config {
        database_url: w.db.as_ref().expect("db").0.url().to_string(),
        bind_addr: "127.0.0.1:1".parse().unwrap(),
        bearer_token: Some("from-env".into()),
    };
    config.resolve(&view).expect("resolve");
    w.notes.push(format!("bind:{}", config.bind_addr));
    w.notes.push(format!(
        "token:{}",
        config.bearer_token.unwrap_or_else(|| "none".into())
    ));
}

#[then(expr = "the daemon binds {string} and requires the token {string}")]
async fn resolved(w: &mut World, bind: String, token: String) {
    assert!(w.notes.contains(&format!("bind:{bind}")), "{:?}", w.notes);
    assert!(w.notes.contains(&format!("token:{token}")), "{:?}", w.notes);
}

#[then("the supervisor ticks every 5 s, the RSS pass every 60 s and history is kept 90 days")]
async fn cadences(_w: &mut World) {
    assert_eq!(skadi_api::DEFAULT_TICK_INTERVAL.as_secs(), 5);
    assert_eq!(skadi_hunter::DEFAULT_RSS_INTERVAL.as_secs(), 60);
    assert_eq!(skadi_hunter::DEFAULT_HISTORY_RETENTION_DAYS, 90);
    assert_eq!(skadi_hunter::STALE_ACQUIRE_GRACE.as_secs(), 15 * 60);
}

/// Sonarr exposes "RSS Sync Interval" (Settings → Indexers); skadi's sweep
/// cadence is `DEFAULT_SWEEP_INTERVAL` (300 s) hard-coded per domain module
/// with no config key.
#[then("the wanted-sweep interval is an operator setting")]
async fn sweep_interval_setting(_w: &mut World) {
    let key = [
        "sweep_interval_secs",
        "hunter.sweep_interval_secs",
        "rss_sync_interval_secs",
    ]
    .iter()
    .find(|k| skadi_config::spec(k).is_some());
    assert!(
        key.is_some(),
        "no sweep-interval config key is registered; the 300 s cadence is a const in \
         crates/skadi-movies/src/module.rs:33 (and tv/audiobooks)"
    );
}

/// `When`-keyword twin of the `Given the {string} domain is enabled` step in
/// `http.rs` (cucumber matches steps per keyword).
#[when(expr = "the {string} domain is enabled")]
async fn enable_when(w: &mut World, name: String) {
    w.store()
        .await
        .set_enabled(&name, true)
        .await
        .expect("enable");
}
