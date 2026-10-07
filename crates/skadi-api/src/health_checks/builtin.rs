//! The checks skadi ships (moved from `diagnostics.rs` in SKADI-T-0679, with
//! the same pass/fail verdicts): daemon, database, enabled domains, library
//! root, download worker, and the reachability of each configured provider.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use skadi_store::{DomainStateRepo, SettingsRepo, Store};

use super::{CheckContext, CheckSource, HealthCheck, Outcome};
use crate::diagnostics::{ROOT_PROBE_TIMEOUT, library_root_path, probe_root_bounded};
use crate::providers::build_one;

/// Per-provider reachability budget — a slow/dead provider reports an error
/// rather than hanging the whole endpoint. 30 s, not 10 (SKADI-T-0587): a
/// CloudFlare-fronted tracker answers through FlareSolverr, whose solve alone
/// routinely takes 10–20 s, so the shorter budget reported nine of nineteen
/// working indexers as failing.
const PROVIDER_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Consecutive failures at which an indexer's health check answers from the
/// cached health registry instead of a live `test()` (SKADI-T-0308). Live-testing
/// a dead CloudFlare tracker serializes behind the single-threaded FlareSolverr
/// and made the whole `/health/checks` page take ~10s; once the search path has
/// marked it down this many times we trust that. Matches the search
/// circuit-breaker default.
const CHECK_CACHED_FAILS: u32 = 3;

/// How stale a worker heartbeat may be before the worker counts as silent. The
/// worker ticks every few seconds; two minutes is well clear of a slow tick and
/// still notices a wedged or dead worker promptly.
const WORKER_SILENT_AFTER: chrono::Duration = chrono::Duration::minutes(2);

// ---- daemon --------------------------------------------------------------------

/// The daemon itself: always ok, and names the build.
pub struct DaemonCheck;

#[async_trait]
impl HealthCheck for DaemonCheck {
    fn id(&self) -> String {
        "daemon".into()
    }
    fn label(&self) -> String {
        "Daemon".into()
    }
    async fn run(&self) -> Outcome {
        Outcome::ok(format!("skadi {}", env!("CARGO_PKG_VERSION")))
    }
}

// ---- database ------------------------------------------------------------------

/// Database reachability: a cheap read that touches the connection.
pub struct DatabaseCheck {
    store: Store,
}

impl DatabaseCheck {
    pub fn new(store: Store) -> Self {
        DatabaseCheck { store }
    }
}

#[async_trait]
impl HealthCheck for DatabaseCheck {
    fn id(&self) -> String {
        "database".into()
    }
    fn label(&self) -> String {
        "Database".into()
    }
    async fn run(&self) -> Outcome {
        match self.store.list_settings("profiles").await {
            Ok(_) => Outcome::ok("reachable"),
            Err(e) => Outcome::error(
                format!("{e}"),
                "Check that the database server is running and that SKADI_DATABASE_URL points at it; the daemon log has the connection error.",
            ),
        }
    }
}

// ---- domains -------------------------------------------------------------------

/// One check per *enabled* domain. A disabled domain is not a warning to act
/// on, and showing it contradicted the sidebar and the dashboard, which list
/// enabled domains only.
pub struct DomainChecks;

#[async_trait]
impl CheckSource for DomainChecks {
    async fn checks(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
        let mut out: Vec<Arc<dyn HealthCheck>> = Vec::new();
        for d in &ctx.domains {
            let enabled = ctx
                .store
                .get(&d.name)
                .await
                .ok()
                .flatten()
                .is_some_and(|s| s.enabled);
            if enabled {
                out.push(Arc::new(DomainCheck {
                    domain: d.name.clone(),
                }));
            }
        }
        out
    }
}

/// An enabled domain.
pub struct DomainCheck {
    pub domain: String,
}

#[async_trait]
impl HealthCheck for DomainCheck {
    fn id(&self) -> String {
        format!("domain:{}", self.domain)
    }
    fn label(&self) -> String {
        format!("Domain {}", self.domain)
    }
    async fn run(&self) -> Outcome {
        Outcome::ok("enabled")
    }
}

// ---- library root --------------------------------------------------------------

/// The library root (SKADI-T-0467/0430): every import lands here, so a missing,
/// non-directory or read-only root is the single most consequential
/// misconfiguration in the stack.
pub struct RootCheck {
    store: Store,
}

impl RootCheck {
    pub fn new(store: Store) -> Self {
        RootCheck { store }
    }
}

const ROOT_REMEDIATION: &str = "Set library.root (Settings → Library) to a directory that exists and that the daemon can write to; for a network mount, check that it is mounted and writable.";

#[async_trait]
impl HealthCheck for RootCheck {
    fn id(&self) -> String {
        "root".into()
    }
    fn label(&self) -> String {
        "Library root".into()
    }
    // The probe has its own deadline with a more useful message; this only has
    // to be longer than it.
    fn timeout(&self) -> Duration {
        ROOT_PROBE_TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self) -> Outcome {
        let path = match library_root_path(&self.store).await {
            Ok(p) => p,
            Err(e) => {
                return Outcome::error(format!("library.root unreadable: {e:?}"), ROOT_REMEDIATION);
            }
        };
        match probe_root_bounded(&path).await {
            Some(status) => match status.problem() {
                None => Outcome::ok(format!("{path} is writable")),
                Some(problem) => Outcome::error(format!("{path}: {problem}"), ROOT_REMEDIATION),
            },
            // A health endpoint that hangs is worse than one that reports a hang
            // (SKADI-T-0430, NFR-ROOTS.1).
            None => Outcome::error(
                format!(
                    "{path}: did not answer within {}s — a hung or disconnected mount is the usual cause",
                    ROOT_PROBE_TIMEOUT.as_secs()
                ),
                "Check the mount that backs library.root: remount it, or restart the host's NFS/SMB client.",
            ),
        }
    }
}

// ---- download worker -----------------------------------------------------------

/// Liveness of the download worker, from its heartbeat row (SKADI-T-0288,
/// SKADI-T-0467). It runs out-of-process, in the VPN namespace, and reaches the
/// daemon only through the database, so its heartbeat is the only thing that
/// says it is alive.
pub struct WorkerCheck {
    store: Store,
}

impl WorkerCheck {
    pub fn new(store: Store) -> Self {
        WorkerCheck { store }
    }
}

#[async_trait]
impl HealthCheck for WorkerCheck {
    fn id(&self) -> String {
        "worker".into()
    }
    fn label(&self) -> String {
        "Download worker".into()
    }
    async fn run(&self) -> Outcome {
        use skadi_store::WorkerStatusRepo;
        match self.store.latest_worker_status().await {
            Err(e) => Outcome::error(
                format!("heartbeat unreadable: {e}"),
                "The worker's heartbeat could not be read from the database: check the database check and the daemon log.",
            ),
            Ok(None) => Outcome::error(
                "no download worker has ever checked in — nothing will download",
                "Start the download worker (the skadi-downloader-worker service) and point it at the same database as the daemon.",
            ),
            Ok(Some(w)) if w.is_fresh(WORKER_SILENT_AFTER) => Outcome::ok(format!(
                "{} (v{}) last seen {}",
                w.worker_id,
                w.version,
                w.last_seen_at.to_rfc3339()
            )),
            Ok(Some(w)) => Outcome::error(
                format!(
                    "{} last checked in {} — silent for more than {} minutes",
                    w.worker_id,
                    w.last_seen_at.to_rfc3339(),
                    WORKER_SILENT_AFTER.num_minutes()
                ),
                format!(
                    "Check that the download worker {} is running (its container, and the VPN it runs behind) and read its log.",
                    w.worker_id
                ),
            ),
        }
    }
}

// ---- providers -----------------------------------------------------------------

/// One reachability check per configured indexer and download client, sorted
/// by id for a stable response.
pub struct ProviderChecks;

#[async_trait]
impl CheckSource for ProviderChecks {
    async fn checks(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
        let mut out: Vec<ProviderCheck> = Vec::new();
        for kind in ["indexers", "downloaders"] {
            for row in ctx.store.list_settings(kind).await.unwrap_or_default() {
                let name = row
                    .body
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| row.id.clone());
                out.push(ProviderCheck {
                    store: ctx.store.clone(),
                    kind,
                    setting_id: row.id.clone(),
                    name,
                });
            }
        }
        out.sort_by_key(HealthCheck::id);
        out.into_iter()
            .map(|c| Arc::new(c) as Arc<dyn HealthCheck>)
            .collect()
    }
}

/// Reachability of one configured indexer or download client, through the
/// provider's own `test()`.
pub struct ProviderCheck {
    store: Store,
    /// The settings kind: `indexers` or `downloaders`.
    kind: &'static str,
    setting_id: String,
    name: String,
}

impl ProviderCheck {
    fn remediation(&self) -> String {
        match self.kind {
            "indexers" => format!(
                "Check the URL and API key of indexer {} under Settings → Indexers, and that the daemon can reach it.",
                self.name
            ),
            _ => format!(
                "Check the URL and credentials of download client {} under Settings → Downloads, and that the daemon can reach it.",
                self.name
            ),
        }
    }
}

#[async_trait]
impl HealthCheck for ProviderCheck {
    fn id(&self) -> String {
        let singular = match self.kind {
            "indexers" => "indexer",
            "downloaders" => "downloader",
            other => other,
        };
        format!("{singular}:{}", self.name)
    }
    fn label(&self) -> String {
        match self.kind {
            "indexers" => format!("Indexer {}", self.name),
            _ => format!("Download client {}", self.name),
        }
    }
    fn timeout(&self) -> Duration {
        PROVIDER_TEST_TIMEOUT
    }
    async fn run(&self) -> Outcome {
        // Known-down indexer → answer from the cached health registry instead of
        // a live test (SKADI-T-0308). Avoids serializing dead CloudFlare trackers
        // behind FlareSolverr, which was making this endpoint take ~10s.
        if self.kind == "indexers"
            && let Some(h) = uuid::Uuid::parse_str(&self.setting_id)
                .ok()
                .map(skadi_core::IndexerId::from)
                .and_then(|iid| skadi_indexers::indexer_health().get(iid))
            && h.consecutive_failures >= CHECK_CACHED_FAILS
        {
            let reason = h
                .last_error
                .unwrap_or_else(|| "unreachable (circuit open)".into());
            return Outcome::error(reason, self.remediation());
        }
        match build_one(&self.store, self.kind, &self.setting_id).await {
            Ok(provider) => match provider.test().await {
                Ok(()) => Outcome::ok("reachable"),
                Err(e) => Outcome::error(format!("{e}"), self.remediation()),
            },
            Err(e) => Outcome::error(format!("{e}"), self.remediation()),
        }
    }
}
