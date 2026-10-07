//! The per-domain worker supervisor (SKADI-T-0052).
//!
//! Skadi compiles every domain in, but each domain is enabled/disabled at
//! runtime. The supervisor polls the `domains` table (via
//! [`DomainStateRepo`](skadi_store::DomainStateRepo)) and reconciles the running
//! workers against it: a domain that is enabled but not running gets its
//! [`workers()`](skadi_core::DomainModule::workers) spawned; a domain that is
//! disabled but running gets its workers cancelled and **awaited to completion**
//! before its entry is dropped.
//!
//! That await is the crux: it guarantees the previous worker (and everything it
//! captured — e.g. a domain's Cloacina runner handle) is fully dropped before a
//! later re-enable spawns fresh workers. Domains like movies rely on this
//! drop-before-respawn ordering.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use skadi_core::{DomainModule, Result};
use skadi_store::{DomainStateRepo, Store};

/// Default reconcile cadence. This is the floor for enable/disable latency; v0
/// accepts ~5s. The daemon may override via [`Supervisor::run`].
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(5);

/// One spawned worker plus the token that stops it.
struct RunningWorker {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

/// Reconciles compiled-in domains' workers against their runtime enable state,
/// and (SKADI-T-0062) re-publishes the live provider set when provider settings
/// change.
pub struct Supervisor {
    store: Store,
    registry: Vec<Arc<dyn DomainModule>>,
    /// Per-domain live workers. Empty/absent ⇒ the domain is not running.
    running: Mutex<HashMap<String, Vec<RunningWorker>>>,
    /// Receivers for rebuilt provider sets (SKADI-T-0062). Empty ⇒ no provider
    /// reload (the skeleton/tests that don't care).
    reloaders: Vec<Arc<dyn crate::providers::ProviderReloader>>,
    /// Fingerprint of the provider settings as last applied. `None` until the
    /// first reconcile, which always applies (bringing the daemon up with
    /// whatever is stored).
    last_fingerprint: Mutex<Option<u64>>,
    /// Set after the first successful provider reconcile, so the readiness probe
    /// can report the daemon as able to work (SKADI-T-0475). `None` for the
    /// skeleton/tests that have no `AppState` to share it with.
    ready_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Per-domain count of workers that ended without being asked to
    /// (SKADI-T-0523) — a panic, or a `run()` that returned. Never reset, so a
    /// domain that crash-loops shows a climbing number rather than looking
    /// healthy between restarts. Read by the health checks.
    /// Shared with [`AppState`](crate::AppState) when the daemon wires it, so the
    /// health endpoint reads the same counts (SKADI-T-0523).
    failures: Arc<Mutex<HashMap<String, u64>>>,
    /// The state whose API token this supervisor keeps current (SKADI-T-0466).
    /// `None` ⇒ not wired (the skeleton/tests).
    auth_state: Option<Arc<crate::AppState>>,
}

impl Supervisor {
    /// Build a supervisor over a store and the compiled-in domain registry.
    pub fn new(store: Store, registry: Vec<Arc<dyn DomainModule>>) -> Self {
        Self::with_reloaders(store, registry, Vec::new())
    }

    /// Build a supervisor that also reconciles provider config into the given
    /// reloaders (SKADI-T-0062).
    pub fn with_reloaders(
        store: Store,
        registry: Vec<Arc<dyn DomainModule>>,
        reloaders: Vec<Arc<dyn crate::providers::ProviderReloader>>,
    ) -> Self {
        Self {
            store,
            registry,
            running: Mutex::new(HashMap::new()),
            failures: Arc::new(Mutex::new(HashMap::new())),
            auth_state: None,
            reloaders,
            last_fingerprint: Mutex::new(None),
            ready_flag: None,
        }
    }

    /// Share the readiness flag from [`AppState`](crate::AppState), which this
    /// supervisor sets once it has completed a provider reconcile
    /// (SKADI-T-0475).
    #[must_use]
    pub fn with_readiness(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.ready_flag = Some(flag);
        self
    }

    /// Share the [`AppState`](crate::AppState) whose API token this supervisor
    /// keeps current, so rotating `api_token` applies on the next tick
    /// (SKADI-T-0466). The same tick also refreshes the health checks of that
    /// state (SKADI-T-0680). The refresh itself lives on `AppState` so tests can drive
    /// it without standing up a supervisor.
    #[must_use]
    pub fn with_live_token(mut self, state: Arc<crate::AppState>) -> Self {
        self.auth_state = Some(state);
        self
    }

    /// Share the worker-failure counts with [`AppState`](crate::AppState) so the
    /// health endpoint reports them (SKADI-T-0523).
    #[must_use]
    pub fn with_failure_counts(mut self, counts: Arc<Mutex<HashMap<String, u64>>>) -> Self {
        self.failures = counts;
        self
    }

    /// Reconcile once: refresh providers if their settings changed, then start
    /// workers for newly-enabled domains and stop them for newly-disabled ones.
    /// Idempotent — an already-correct domain is a no-op.
    ///
    /// Provider reconcile runs *first* so a freshly-enabled domain's workers
    /// come up seeing current providers.
    pub async fn tick(&self) -> Result<()> {
        // A provider reconcile failure must not abort the tick (SKADI-T-0519).
        // It used to: one unreadable credential propagated out of
        // `build_providers`, the `?` here returned before the domain loop, and no
        // domain worker ever started — the daemon logged "supervisor reconcile
        // tick failed" every few seconds and did nothing else. Enabled domains
        // should still come up, running with the last good provider set.
        //
        // The error is held rather than swallowed: the domain loop runs first,
        // then the tick still reports the failure, so health and the operator's
        // logs keep telling the truth about the broken provider.
        let provider_err = self.reconcile_providers().await.err();
        // Before the enabled/running comparison below, so a domain whose workers
        // died is seen as not-running and restarted by the existing branch
        // (SKADI-T-0523).
        self.reap_finished().await;
        if let Some(state) = &self.auth_state {
            state.refresh_api_token().await;
            state.refresh_members().await;
            // Spawned, not awaited: a provider probe can take its whole 30 s
            // budget, and the tick must not wait on it. The cache does not
            // start a check that an earlier tick is still running (SKADI-T-0680).
            let state = state.clone();
            tokio::spawn(async move { state.refresh_health().await });
        }
        for module in &self.registry {
            let name = module.name();
            let enabled = self
                .store
                .get(name)
                .await?
                .map(|s| s.enabled)
                .unwrap_or(false);

            let is_running = {
                let running = self.running.lock().await;
                running.get(name).is_some_and(|v| !v.is_empty())
            };

            match (enabled, is_running) {
                (true, false) => self.start_domain(module).await,
                (false, true) => self.stop_domain(name).await,
                _ => {}
            }
        }
        match provider_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Rebuild + re-publish the provider set when the provider **or** active
    /// profile settings fingerprint differs from the last applied (SKADI-T-0062,
    /// extended for profiles in SKADI-T-0067). No reloaders ⇒ no work. The next
    /// workflow stage of any in-flight run sees the new set + profile.
    async fn reconcile_providers(&self) -> Result<()> {
        if self.reloaders.is_empty() {
            return Ok(());
        }
        let current = crate::providers::service_fingerprint(&self.store).await?;
        {
            let last = self.last_fingerprint.lock().await;
            if *last == Some(current) {
                return Ok(());
            }
        }
        // Build once per reloader: ProviderSet isn't Clone (trait objects), and
        // v0 has exactly one domain, so per-reloader builds are fine.
        for reloader in &self.reloaders {
            let set = crate::providers::build_providers(&self.store).await?;
            tracing::info!(
                indexers = set.indexers.len(),
                downloaders = set.downloaders.len(),
                notifiers = set.notifiers.len(),
                "provider/profile settings changed — applying rebuilt provider set + profile"
            );
            // The reloader re-resolves the active quality profile from the store
            // as part of apply() (SKADI-T-0067), so a profile-only change is
            // picked up here too.
            reloader.apply(set).await?;
        }
        *self.last_fingerprint.lock().await = Some(current);
        // The daemon can now do its job: providers are built and published
        // (SKADI-T-0475). Set once and left set — a later reconcile failure is
        // reported through health checks rather than by flapping readiness,
        // because the last good provider set is still live.
        if let Some(flag) = &self.ready_flag {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// Spawn every worker the module yields, each under its own cancellation
    /// token, and record the handles.
    async fn start_domain(&self, module: &Arc<dyn DomainModule>) {
        let name = module.name();
        let mut spawned = Vec::new();
        for worker in module.workers() {
            let cancel = CancellationToken::new();
            let fut = worker.run(cancel.clone());
            let handle = tokio::spawn(fut);
            spawned.push(RunningWorker { cancel, handle });
        }
        let count = spawned.len();
        self.running.lock().await.insert(name.to_string(), spawned);
        tracing::info!(
            domain = name,
            workers = count,
            "domain enabled — workers started"
        );
    }

    /// Cancel and **await** a domain's workers, then drop its entry. Awaiting is
    /// what upholds the drop-before-respawn guarantee.
    async fn stop_domain(&self, name: &str) {
        let workers = self.running.lock().await.remove(name);
        let Some(workers) = workers else { return };
        for w in &workers {
            w.cancel.cancel();
        }
        for w in workers {
            let _ = w.handle.await;
        }
        tracing::info!(domain = name, "domain disabled — workers stopped");
    }

    /// Stop every running domain (used on daemon shutdown).
    pub async fn stop_all(&self) {
        let names: Vec<String> = {
            let running = self.running.lock().await;
            running.keys().cloned().collect()
        };
        for name in names {
            self.stop_domain(&name).await;
        }
    }

    /// Run the reconcile loop until `cancel` fires, then stop all workers.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        // The first immediate tick brings already-enabled domains up at startup.
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Err(e) = self.tick().await {
                        tracing::error!(error = %e, "supervisor reconcile tick failed");
                    }
                }
            }
        }
        self.stop_all().await;
    }

    /// Number of domains currently running workers (for diagnostics/tests).
    pub async fn running_domains(&self) -> usize {
        self.running.lock().await.len()
    }

    /// How many workers have ended on their own, per domain (SKADI-T-0523).
    ///
    /// A climbing count is the signal that a domain is crash-looping: the
    /// supervisor restarts it on the next tick, so `running_domains` alone would
    /// show it as healthy the whole time.
    pub async fn worker_failures(&self) -> HashMap<String, u64> {
        self.failures.lock().await.clone()
    }

    /// Drop workers that have ended on their own and record why (SKADI-T-0523).
    ///
    /// A spawned worker that returned or panicked stayed in `running` forever: the
    /// domain still counted as running, so `tick`'s restart branch never fired and
    /// nothing reported it. The domain was simply dead, quietly, until the daemon
    /// was restarted.
    ///
    /// Reaping here — before the enabled/running comparison — makes the existing
    /// `(enabled, !running)` branch do the restart, so there is one place that
    /// starts workers rather than two.
    async fn reap_finished(&self) {
        let mut running = self.running.lock().await;
        let mut failures = self.failures.lock().await;
        let mut emptied = Vec::new();
        for (name, workers) in running.iter_mut() {
            let mut alive = Vec::with_capacity(workers.len());
            for w in std::mem::take(workers) {
                if !w.handle.is_finished() {
                    alive.push(w);
                    continue;
                }
                // Already finished, so this returns immediately — no await on a
                // live worker while holding the lock.
                match w.handle.await {
                    Ok(()) => tracing::warn!(
                        domain = %name,
                        "worker returned on its own; it should run until cancelled"
                    ),
                    Err(e) if e.is_panic() => {
                        tracing::error!(domain = %name, "worker panicked: {e}");
                    }
                    // Cancelled: `stop_domain` aborts nothing, so this is only
                    // reachable if the runtime is shutting down.
                    Err(e) => tracing::warn!(domain = %name, "worker ended: {e}"),
                }
                *failures.entry(name.clone()).or_insert(0) += 1;
            }
            *workers = alive;
            if workers.is_empty() {
                emptied.push(name.clone());
            }
        }
        for name in emptied {
            running.remove(&name);
            tracing::warn!(
                domain = %name,
                "every worker for this domain has ended; it will restart on this tick if still enabled"
            );
        }
    }
}
