//! The health check model and registry behind `GET /health/checks`
//! (SKADI-T-0679, COLLIERY-I-0294).
//!
//! A check is one [`HealthCheck`]: it has a stable `id`, a human `label`, and a
//! `run` that answers with an [`Outcome`] — a [`Severity`], a message and, when
//! something is wrong, a remediation that says how to fix it. The runner stamps
//! the outcome with the id, the label and the time into a [`CheckResult`].
//!
//! Checks come from [`CheckSource`]s. A source enumerates the checks it owns
//! for the current configuration (one per enabled domain, one per configured
//! provider, or a single fixed check), so the set of checks is known before any
//! of them runs. The [`HealthRegistry`] is the ordered list of sources; its
//! order is the order of the response.
//!
//! Extension points:
//! - a new check: implement [`HealthCheck`], and add a source for it to
//!   [`HealthRegistry::builtin`] (use [`single`] for a check that always exists);
//! - new inputs a check needs (a disk probe, the gluetun client, …): add a field
//!   to [`CheckContext`], filled in [`CheckContext::from_state`];
//! - how often a check runs: [`HealthCheck::ttl`]. The endpoint reads the
//!   results that [`HealthCache`] stores (SKADI-T-0680); the supervisor tick
//!   refreshes the ones older than their TTL, and `POST /health/checks/run`
//!   forces a run.
//!
//! The wire shape keeps the pre-T-0679 `{ name, status, detail }` fields as a
//! projection of the new ones (see [`CheckResult`]), in the same bare JSON array,
//! so the web dashboard, the settings pages and the Android client read the
//! response unchanged.

mod builtin;
mod cache;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};

use skadi_store::Store;

use crate::error::ApiError;
use crate::state::{AppState, DomainDescriptor};

pub use builtin::{
    DaemonCheck, DatabaseCheck, DomainCheck, DomainChecks, ProviderCheck, ProviderChecks,
    RootCheck, WorkerCheck,
};
pub use cache::HealthCache;

/// How bad a check result is.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Not run yet: the cache lists the check before its first run
    /// (SKADI-T-0680). Only [`CheckResult::pending`] has it; a check never
    /// answers it.
    Pending,
    Ok,
    /// Works, but needs attention soon.
    Warn,
    /// Broken: something the operator relies on does not work.
    Error,
}

impl Severity {
    /// The pre-T-0679 `status` value: `ok` | `warn` | `fail`. Pending maps to
    /// `warn`, which the clients of that field show as amber, "not known yet",
    /// and not as a failure.
    pub fn legacy_status(self) -> &'static str {
        match self {
            Severity::Pending => "warn",
            Severity::Ok => "ok",
            Severity::Warn => "warn",
            Severity::Error => "fail",
        }
    }
}

/// What one run of a check found. A warning or an error always says how to fix
/// it: the constructors take the remediation, so a check cannot forget it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub severity: Severity,
    pub message: String,
    pub remediation: Option<String>,
}

impl Outcome {
    pub fn ok(message: impl Into<String>) -> Self {
        Outcome {
            severity: Severity::Ok,
            message: message.into(),
            remediation: None,
        }
    }

    pub fn warn(message: impl Into<String>, remediation: impl Into<String>) -> Self {
        Outcome {
            severity: Severity::Warn,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }

    pub fn error(message: impl Into<String>, remediation: impl Into<String>) -> Self {
        Outcome {
            severity: Severity::Error,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }
}

/// One check's result as `/health/checks` returns it.
///
/// Serialized with the new fields and, for clients written before T-0679, the
/// old ones as a projection: `name` = `id`, `status` = the severity as
/// `ok`/`warn`/`fail`, `detail` = `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Stable identifier: `database`, `worker`, `root`, `domain:<name>`,
    /// `indexer:<name>`, … Clients key on it.
    pub id: String,
    /// Human name for the check.
    pub label: String,
    pub severity: Severity,
    pub message: String,
    /// How to fix it. Always set for `warn` and `error`.
    pub remediation: Option<String>,
    /// When the check ran. `None` for a check that has not run yet (a cache that
    /// lists a check before its first run); serialized as `null`.
    pub checked_at: Option<DateTime<Utc>>,
}

impl CheckResult {
    /// Stamp an outcome with the check's identity and the time it ran.
    pub fn new(id: String, label: String, outcome: Outcome, checked_at: DateTime<Utc>) -> Self {
        CheckResult {
            id,
            label,
            severity: outcome.severity,
            message: outcome.message,
            remediation: outcome.remediation,
            checked_at: Some(checked_at),
        }
    }

    /// A check that has not run yet: `severity: pending`, `checked_at: null`.
    pub fn pending(id: String, label: String) -> Self {
        CheckResult {
            id,
            label,
            severity: Severity::Pending,
            message: "not checked yet".into(),
            remediation: None,
            checked_at: None,
        }
    }
}

#[derive(Serialize)]
struct WireCheck<'a> {
    id: &'a str,
    label: &'a str,
    severity: Severity,
    message: &'a str,
    remediation: Option<&'a str>,
    checked_at: Option<String>,
    // The pre-T-0679 projection.
    name: &'a str,
    status: &'static str,
    detail: &'a str,
}

impl Serialize for CheckResult {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        WireCheck {
            id: &self.id,
            label: &self.label,
            severity: self.severity,
            message: &self.message,
            remediation: self.remediation.as_deref(),
            checked_at: self
                .checked_at
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            name: &self.id,
            status: self.severity.legacy_status(),
            detail: &self.message,
        }
        .serialize(s)
    }
}

/// What the checks may read. Add a field here when a new check needs a new
/// input, and fill it in [`CheckContext::from_state`].
#[derive(Clone)]
pub struct CheckContext {
    pub store: Store,
    /// The domains compiled into this build (enabled or not).
    pub domains: Vec<DomainDescriptor>,
    /// Unasked-for exits per domain worker (`AppState::worker_failures`).
    pub worker_failures: Arc<tokio::sync::Mutex<HashMap<String, u64>>>,
}

impl CheckContext {
    pub fn from_state(state: &AppState) -> Result<Self, ApiError> {
        let store = state.store.clone().ok_or_else(|| {
            ApiError(skadi_core::AppError::Internal(
                "store not configured".into(),
            ))
        })?;
        Ok(CheckContext {
            store,
            domains: state.domains.clone(),
            worker_failures: state.worker_failures.clone(),
        })
    }
}

/// The budget of a check that does not set its own: a safety net, so that a
/// check that hangs reports an error instead of hanging the request.
pub const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a result stays fresh when the check does not set its own TTL. The
/// cheap local checks (database, library root, worker heartbeat) use it.
pub const DEFAULT_CHECK_TTL: Duration = Duration::from_secs(30);

/// One health check.
#[async_trait]
pub trait HealthCheck: Send + Sync {
    /// Stable id (see [`CheckResult::id`]).
    fn id(&self) -> String;
    /// Human name.
    fn label(&self) -> String;
    /// How long [`run`](Self::run) may take before the runner reports a timeout.
    fn timeout(&self) -> Duration {
        DEFAULT_CHECK_TIMEOUT
    }
    /// How long a result stays fresh before the next refresh runs the check
    /// again ([`HealthCache::refresh_due`]).
    fn ttl(&self) -> Duration {
        DEFAULT_CHECK_TTL
    }
    /// Probe and report. Never panics on a failed probe: a failure is an
    /// [`Outcome::error`], not an `Err`.
    async fn run(&self) -> Outcome;
}

/// Something that knows which checks exist for the current configuration.
/// Enumerating must be cheap (settings reads, no network): the probing belongs in
/// [`HealthCheck::run`].
#[async_trait]
pub trait CheckSource: Send + Sync {
    async fn checks(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>>;
}

/// A source with exactly one check, built from the context.
pub fn single<F>(build: F) -> Arc<dyn CheckSource>
where
    F: Fn(&CheckContext) -> Arc<dyn HealthCheck> + Send + Sync + 'static,
{
    Arc::new(Single(build))
}

struct Single<F>(F);

#[async_trait]
impl<F> CheckSource for Single<F>
where
    F: Fn(&CheckContext) -> Arc<dyn HealthCheck> + Send + Sync,
{
    async fn checks(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
        vec![(self.0)(ctx)]
    }
}

/// The ordered list of check sources.
#[derive(Clone, Default)]
pub struct HealthRegistry {
    sources: Vec<Arc<dyn CheckSource>>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a source; its checks come after those already registered.
    #[must_use]
    pub fn with(mut self, source: Arc<dyn CheckSource>) -> Self {
        self.sources.push(source);
        self
    }

    /// The checks skadi ships, in response order: daemon, database, each
    /// enabled domain, library root, download worker, then every configured
    /// indexer and download client (sorted by id).
    pub fn builtin() -> Self {
        Self::new()
            .with(single(|_| Arc::new(DaemonCheck)))
            .with(single(|ctx| {
                Arc::new(DatabaseCheck::new(ctx.store.clone()))
            }))
            .with(Arc::new(DomainChecks))
            .with(single(|ctx| Arc::new(RootCheck::new(ctx.store.clone()))))
            .with(single(|ctx| Arc::new(WorkerCheck::new(ctx.store.clone()))))
            .with(Arc::new(ProviderChecks))
    }

    /// Every check for the current configuration, without running any.
    pub async fn enumerate(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
        let mut all = Vec::new();
        for source in &self.sources {
            all.extend(source.checks(ctx).await);
        }
        all
    }

    /// Enumerate and run every check concurrently. The result is in
    /// registration order whatever order the checks finish in.
    pub async fn run_all(&self, ctx: &CheckContext) -> Vec<CheckResult> {
        let checks = self.enumerate(ctx).await;
        let handles: Vec<_> = checks
            .into_iter()
            .map(|c| tokio::spawn(async move { run_check(c.as_ref()).await }))
            .collect();
        let mut results = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(r) => results.push(r),
                // `run_check` cannot know the id once the task is gone; a panic
                // in a check is a bug, so log it rather than inventing a row.
                Err(e) => tracing::error!(error = %e, "a health check panicked"),
            }
        }
        results
    }
}

/// Run one check within its budget and stamp the result.
pub async fn run_check(check: &dyn HealthCheck) -> CheckResult {
    let budget = check.timeout();
    let outcome = match tokio::time::timeout(budget, check.run()).await {
        Ok(o) => o,
        Err(_) => Outcome::error(
            format!("timed out after {}s", budget.as_secs()),
            "The check did not answer in time: the service it probes is slow, hung or unreachable. Check that it is running and that the daemon can reach it.",
        ),
    };
    CheckResult::new(check.id(), check.label(), outcome, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str, Outcome, Duration);

    #[async_trait]
    impl HealthCheck for Fixed {
        fn id(&self) -> String {
            self.0.into()
        }
        fn label(&self) -> String {
            format!("Fixed {}", self.0)
        }
        fn timeout(&self) -> Duration {
            Duration::from_millis(50)
        }
        async fn run(&self) -> Outcome {
            tokio::time::sleep(self.2).await;
            self.1.clone()
        }
    }

    #[test]
    fn severity_projects_onto_the_legacy_status() {
        assert_eq!(Severity::Ok.legacy_status(), "ok");
        assert_eq!(Severity::Warn.legacy_status(), "warn");
        assert_eq!(Severity::Error.legacy_status(), "fail");
    }

    #[test]
    fn a_result_serializes_the_new_fields_and_the_legacy_projection() {
        let at = DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let r = CheckResult::new(
            "worker".into(),
            "Download worker".into(),
            Outcome::error("silent", "start it"),
            at,
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["id"], "worker");
        assert_eq!(v["label"], "Download worker");
        assert_eq!(v["severity"], "error");
        assert_eq!(v["message"], "silent");
        assert_eq!(v["remediation"], "start it");
        assert_eq!(v["checked_at"], "2026-10-06T12:00:00.000Z");
        assert_eq!(v["name"], "worker");
        assert_eq!(v["status"], "fail");
        assert_eq!(v["detail"], "silent");

        let ok = CheckResult::new("db".into(), "Database".into(), Outcome::ok("up"), at);
        let v = serde_json::to_value(&ok).unwrap();
        assert!(
            v["remediation"].is_null(),
            "an ok check has remediation: null"
        );
        let pending = CheckResult {
            checked_at: None,
            ..ok
        };
        assert!(serde_json::to_value(&pending).unwrap()["checked_at"].is_null());
    }

    #[tokio::test]
    async fn a_check_that_overruns_its_budget_is_an_error_with_a_remediation() {
        let slow = Fixed("slow", Outcome::ok("late"), Duration::from_secs(5));
        let r = run_check(&slow).await;
        assert_eq!(r.id, "slow");
        assert_eq!(r.severity, Severity::Error);
        assert!(r.message.contains("timed out"));
        assert!(r.remediation.is_some());
        assert!(r.checked_at.is_some());
    }

    struct FixedSource(Vec<Arc<dyn HealthCheck>>);

    #[async_trait]
    impl CheckSource for FixedSource {
        async fn checks(&self, _ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn results_keep_registration_order_whatever_finishes_first() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        let Ok(store) = Store::connect(&url) else {
            // No SQLite backend in this build: the order is still covered by the
            // BDD scenarios, which run against a real store.
            return;
        };
        let ctx = CheckContext {
            store,
            domains: vec![],
            worker_failures: Arc::default(),
        };
        let registry = HealthRegistry::new()
            .with(Arc::new(FixedSource(vec![Arc::new(Fixed(
                "first",
                Outcome::ok("a"),
                Duration::from_millis(30),
            ))])))
            .with(single(|_| {
                Arc::new(Fixed("second", Outcome::warn("b", "fix b"), Duration::ZERO))
            }));
        let ids: Vec<_> = registry
            .run_all(&ctx)
            .await
            .into_iter()
            .map(|r| (r.id, r.severity))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("first".into(), Severity::Ok),
                ("second".into(), Severity::Warn)
            ]
        );
    }
}
