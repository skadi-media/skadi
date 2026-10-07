//! The stored results behind `GET /health/checks` (SKADI-T-0680).
//!
//! Before this, every `GET /health/checks` probed every provider live, each with
//! a 30 s budget, so one provider that was down made the dashboard and the
//! settings pages slow. Now the request only reads: [`HealthCache::snapshot`]
//! enumerates the checks of the current configuration (settings reads, no
//! network) and answers each from its stored result, or as pending
//! (`checked_at: null`) when it has never run.
//!
//! Results are refreshed off the request path:
//! - [`HealthCache::refresh_due`] runs each check whose result is missing or
//!   older than its [`HealthCheck::ttl`]. The supervisor tick spawns it, so a
//!   probe that hangs delays only its own row. A check that is still running from
//!   an earlier tick is not started again.
//! - [`HealthCache::run_now`] runs every check (or one, by id) at once, for
//!   `POST /health/checks/run`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::Utc;

use super::{CheckContext, CheckResult, HealthCheck, HealthRegistry, resolve, run_check};

/// A [`HealthRegistry`] with the last result of each of its checks.
pub struct HealthCache {
    registry: HealthRegistry,
    /// The last result per check id.
    results: Mutex<HashMap<String, CheckResult>>,
    /// Ids of the checks that a [`refresh_due`](Self::refresh_due) is running.
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl HealthCache {
    pub fn new(registry: HealthRegistry) -> Self {
        HealthCache {
            registry,
            results: Mutex::default(),
            in_flight: Arc::default(),
        }
    }

    /// Every check of the current configuration, in registry order, answered
    /// from its stored result. A check with no stored result is pending; a
    /// rollup is summarized from the stored results of its members. Runs no
    /// check.
    pub async fn snapshot(&self, ctx: &CheckContext) -> Vec<CheckResult> {
        let checks = self.registry.enumerate(ctx).await;
        let results = lock(&self.results);
        resolve(&checks, &results)
    }

    /// Run, concurrently, each check whose result is missing or older than its
    /// TTL and that is not already running; store the results. Returns when those
    /// checks have finished (each within its own timeout). Also forgets the
    /// results of checks that no longer exist (a deleted provider).
    ///
    /// Returns how many checks ran.
    pub async fn refresh_due(&self, ctx: &CheckContext) -> usize {
        let checks = self.registry.enumerate(ctx).await;
        self.forget_all_but(&checks);
        let now = Utc::now();
        let due: Vec<_> = {
            let results = lock(&self.results);
            let mut in_flight = lock(&self.in_flight);
            checks
                .into_iter()
                // A rollup probes nothing: it is summarized on read.
                .filter(|c| c.rollup_prefix().is_none())
                .filter(|c| {
                    let fresh = results
                        .get(&c.id())
                        .and_then(|r| r.checked_at)
                        .is_some_and(|at| {
                            chrono::Duration::from_std(c.ttl())
                                .is_ok_and(|ttl| now.signed_duration_since(at) < ttl)
                        });
                    // `insert` is false when the check is already running.
                    !fresh && in_flight.insert(c.id())
                })
                .collect()
        };
        let n = due.len();
        let handles: Vec<_> = due
            .into_iter()
            .map(|c| {
                let guard = InFlight {
                    set: self.in_flight.clone(),
                    id: c.id(),
                };
                tokio::spawn(async move {
                    let r = run_check(c.as_ref()).await;
                    drop(guard);
                    r
                })
            })
            .collect();
        self.store_all(handles).await;
        n
    }

    /// Run every check now (or only the check `id`; for a rollup, its
    /// members), store the results, and return the whole snapshot. `None` when
    /// `id` names no current check.
    pub async fn run_now(&self, ctx: &CheckContext, id: Option<&str>) -> Option<Vec<CheckResult>> {
        let checks = self.registry.enumerate(ctx).await;
        self.forget_all_but(&checks);
        let run: Vec<_> = match id {
            None => checks
                .into_iter()
                .filter(|c| c.rollup_prefix().is_none())
                .collect(),
            Some(id) => {
                let target = checks.iter().find(|c| c.id() == id)?;
                match target.rollup_prefix() {
                    None => vec![target.clone()],
                    Some(prefix) => checks
                        .iter()
                        .filter(|c| c.rollup_prefix().is_none() && c.id().starts_with(prefix))
                        .cloned()
                        .collect(),
                }
            }
        };
        let handles: Vec<_> = run
            .into_iter()
            .map(|c| tokio::spawn(async move { run_check(c.as_ref()).await }))
            .collect();
        self.store_all(handles).await;
        Some(self.snapshot(ctx).await)
    }

    async fn store_all(&self, handles: Vec<tokio::task::JoinHandle<CheckResult>>) {
        for h in handles {
            match h.await {
                Ok(r) => self.store(r),
                // A panic in a check is a bug; the check stays as it was (pending,
                // or its last result) and the next refresh tries it again.
                Err(e) => tracing::error!(error = %e, "a health check panicked"),
            }
        }
    }

    /// Keep the newer of the stored and the new result: a slow background probe
    /// must not replace a result that a forced run stored after it.
    fn store(&self, r: CheckResult) {
        let mut results = lock(&self.results);
        let newer = results
            .get(&r.id)
            .is_none_or(|old| old.checked_at <= r.checked_at);
        if newer {
            results.insert(r.id.clone(), r);
        }
    }

    fn forget_all_but(&self, checks: &[Arc<dyn HealthCheck>]) {
        let ids: HashSet<String> = checks.iter().map(|c| c.id()).collect();
        lock(&self.results).retain(|id, _| ids.contains(id));
    }
}

/// Takes the check out of the in-flight set when the probe ends, also when it
/// panics.
struct InFlight {
    set: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        lock(&self.set).remove(&self.id);
    }
}

/// The maps are only touched between awaits, so a poisoned lock means a panic in
/// this module's own bookkeeping; the data is still usable.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::super::{Outcome, Severity, single};
    use super::*;

    /// Counts its runs; sleeps `delay` per run.
    struct Counting {
        runs: Arc<AtomicUsize>,
        delay: Duration,
        ttl: Duration,
    }

    #[async_trait]
    impl HealthCheck for Counting {
        fn id(&self) -> String {
            "counting".into()
        }
        fn label(&self) -> String {
            "Counting".into()
        }
        fn ttl(&self) -> Duration {
            self.ttl
        }
        async fn run(&self) -> Outcome {
            self.runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Outcome::ok("ran")
        }
    }

    async fn ctx() -> Option<(tempfile::TempDir, CheckContext)> {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        // No SQLite backend in this build: the BDD scenarios cover the cache
        // against a real store.
        let store = skadi_store::Store::connect(&url).ok()?;
        Some((
            dir,
            CheckContext {
                store,
                domains: vec![],
                worker_failures: Arc::default(),
                disk_probe: super::super::statvfs_probe(),
            },
        ))
    }

    fn cache(runs: &Arc<AtomicUsize>, delay: Duration, ttl: Duration) -> Arc<HealthCache> {
        let runs = runs.clone();
        Arc::new(HealthCache::new(HealthRegistry::new().with(single(
            move |_| {
                Arc::new(Counting {
                    runs: runs.clone(),
                    delay,
                    ttl,
                })
            },
        ))))
    }

    #[tokio::test]
    async fn a_snapshot_runs_nothing_and_lists_a_never_run_check_as_pending() {
        let Some((_dir, ctx)) = ctx().await else {
            return;
        };
        let runs = Arc::default();
        let c = cache(&runs, Duration::ZERO, Duration::from_secs(60));
        let snap = c.snapshot(&ctx).await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].severity, Severity::Pending);
        assert!(snap[0].checked_at.is_none());
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        assert_eq!(c.refresh_due(&ctx).await, 1);
        let snap = c.snapshot(&ctx).await;
        assert_eq!(snap[0].severity, Severity::Ok);
        assert!(snap[0].checked_at.is_some());
    }

    #[tokio::test]
    async fn a_fresh_result_is_not_run_again_until_its_ttl_passes() {
        let Some((_dir, ctx)) = ctx().await else {
            return;
        };
        let runs = Arc::default();
        let c = cache(&runs, Duration::ZERO, Duration::from_secs(60));
        c.refresh_due(&ctx).await;
        assert_eq!(c.refresh_due(&ctx).await, 0, "fresh: not due");
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let runs = Arc::default();
        let c = cache(&runs, Duration::ZERO, Duration::ZERO);
        c.refresh_due(&ctx).await;
        assert_eq!(c.refresh_due(&ctx).await, 1, "a zero TTL is always due");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_running_check_is_not_started_again_by_the_next_tick() {
        let Some((_dir, ctx)) = ctx().await else {
            return;
        };
        let runs = Arc::default();
        let c = cache(&runs, Duration::from_millis(200), Duration::ZERO);
        let first = tokio::spawn({
            let c = c.clone();
            let ctx = ctx.clone();
            async move { c.refresh_due(&ctx).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(c.refresh_due(&ctx).await, 0, "still in flight");
        assert_eq!(first.await.unwrap(), 1);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(c.refresh_due(&ctx).await, 1, "free again once it ended");
    }

    #[tokio::test]
    async fn run_now_runs_a_fresh_check_and_refuses_an_unknown_id() {
        let Some((_dir, ctx)) = ctx().await else {
            return;
        };
        let runs = Arc::default();
        let c = cache(&runs, Duration::ZERO, Duration::from_secs(60));
        c.refresh_due(&ctx).await;
        let snap = c.run_now(&ctx, Some("counting")).await.expect("known id");
        assert_eq!(snap[0].severity, Severity::Ok);
        assert_eq!(runs.load(Ordering::SeqCst), 2, "forced past the TTL");
        assert!(c.run_now(&ctx, Some("nope")).await.is_none());
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
