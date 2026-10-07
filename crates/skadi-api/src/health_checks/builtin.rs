//! The checks skadi ships: daemon, database, enabled domains (and their
//! worker failures), library root, each enabled domain's folder, disk space,
//! download worker, the reachability of each configured provider, and the
//! indexer and download-client rollups (SKADI-T-0679, SKADI-T-0681).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use skadi_store::{DomainStateRepo, SettingsRepo, Store};

use super::{CheckContext, CheckResult, CheckSource, DiskProbe, HealthCheck, Outcome, Severity};
use crate::diagnostics::{
    ROOT_PROBE_TIMEOUT, library_root_path, library_root_setting, probe_root_bounded,
};
use crate::providers::build_one;
use crate::state::DomainDescriptor;

/// Per-provider reachability budget — a slow/dead provider reports an error
/// rather than hanging the whole endpoint. 30 s, not 10 (SKADI-T-0587): a
/// CloudFlare-fronted tracker answers through FlareSolverr, whose solve alone
/// routinely takes 10–20 s, so the shorter budget reported nine of nineteen
/// working indexers as failing.
const PROVIDER_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a provider result stays fresh (SKADI-T-0680). A provider probe is a
/// network round trip to a service outside skadi (through FlareSolverr for some
/// trackers), so it runs less often than the local checks. The search path's
/// circuit breaker still marks a dead indexer down between two runs.
const PROVIDER_CHECK_TTL: Duration = Duration::from_secs(5 * 60);

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

/// Used space of the library root's filesystem at which `disk-space` warns
/// (Sonarr warns on low free space too). From here to
/// [`DISK_ERROR_USED_PERCENT`] the library still works, but a season pack can
/// fill the rest.
pub const DISK_WARN_USED_PERCENT: f64 = 75.0;

/// Used space above which `disk-space` is an error: imports and downloads are
/// close to failing for lack of space.
pub const DISK_ERROR_USED_PERCENT: f64 = 90.0;

/// Unasked-for exits of a domain worker at which its `domain:<name>` check is an
/// error. One or two exits (a restart that then held) are a warning; three mean
/// the worker is crash-looping and the supervisor's restarts are not fixing it.
pub const DOMAIN_WORKER_FAILURES_ERROR: u64 = 3;

/// A provider that answers its test, but slower than this, is a warning. Well
/// above the 10–20 s that a FlareSolverr solve routinely takes (see
/// [`PROVIDER_TEST_TIMEOUT`]), and below that timeout, so "slow" means slower
/// than a working CloudFlare-fronted tracker, not "about to time out".
pub const PROVIDER_SLOW_AFTER: Duration = Duration::from_secs(20);

/// Failure rate over an indexer's recent searches
/// (`IndexerHealth::recent_failure_rate`, which needs 10 samples) at which an
/// indexer that still answers its test is a warning: it works, but one search
/// in four fails.
pub const PROVIDER_DEGRADED_FAILURE_RATE: f32 = 0.25;

/// The enabled domains, in compile order.
async fn enabled_domains(ctx: &CheckContext) -> Vec<DomainDescriptor> {
    let mut out = Vec::new();
    for d in &ctx.domains {
        let enabled = ctx
            .store
            .get(&d.name)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.enabled);
        if enabled {
            out.push(d.clone());
        }
    }
    out
}

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
        enabled_domains(ctx)
            .await
            .into_iter()
            .map(|d| {
                Arc::new(DomainCheck {
                    domain: d.name,
                    worker_failures: ctx.worker_failures.clone(),
                }) as Arc<dyn HealthCheck>
            })
            .collect()
    }
}

/// An enabled domain, and whether its worker keeps dying. The supervisor counts
/// each exit it did not ask for (`Supervisor::reap_finished`, SKADI-T-0523) and
/// restarts the worker on the next tick, so without this check a crash-looping
/// domain looked fine.
pub struct DomainCheck {
    pub domain: String,
    /// `AppState::worker_failures`: unasked-for exits per domain since the
    /// daemon started.
    pub worker_failures: Arc<tokio::sync::Mutex<HashMap<String, u64>>>,
}

impl DomainCheck {
    /// The verdict for `failures` exits of this domain's worker. Pure.
    pub fn outcome(&self, failures: u64) -> Outcome {
        if failures == 0 {
            return Outcome::ok("enabled");
        }
        let message = format!(
            "the {} worker stopped unexpectedly {failures} time{} since the daemon started; the supervisor restarts it on each tick",
            self.domain,
            if failures == 1 { "" } else { "s" },
        );
        let remediation = format!(
            "Read the daemon log for the error of the {} worker (it logs \"worker panicked\" or \"worker returned on its own\" with domain={}). Fix the cause, then restart the daemon to reset the count.",
            self.domain, self.domain
        );
        if failures >= DOMAIN_WORKER_FAILURES_ERROR {
            Outcome::error(message, remediation)
        } else {
            Outcome::warn(message, remediation)
        }
    }
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
        let failures = self
            .worker_failures
            .lock()
            .await
            .get(&self.domain)
            .copied()
            .unwrap_or(0);
        self.outcome(failures)
    }
}

/// One folder check per enabled domain: `root:<domain>`.
pub struct DomainRootChecks;

#[async_trait]
impl CheckSource for DomainRootChecks {
    async fn checks(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
        enabled_domains(ctx)
            .await
            .into_iter()
            .map(|d| {
                Arc::new(DomainRootCheck {
                    store: ctx.store.clone(),
                    domain: d,
                }) as Arc<dyn HealthCheck>
            })
            .collect()
    }
}

/// The folder of an enabled domain, `<library.root>/<subfolder>` (movie,
/// television, audiobook; SKADI-T-0302). A domain that is enabled but has no
/// folder it can write to cannot import anything.
pub struct DomainRootCheck {
    store: Store,
    domain: DomainDescriptor,
}

#[async_trait]
impl HealthCheck for DomainRootCheck {
    fn id(&self) -> String {
        format!("root:{}", self.domain.name)
    }
    fn label(&self) -> String {
        format!("Folder of {}", self.domain.name)
    }
    fn timeout(&self) -> Duration {
        ROOT_PROBE_TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self) -> Outcome {
        let root = match library_root_path(&self.store).await {
            Ok(p) => p,
            Err(e) => {
                return Outcome::error(format!("library.root unreadable: {e:?}"), ROOT_REMEDIATION);
            }
        };
        let path = Path::new(&root).join(self.domain.kind.library_subfolder());
        let shown = path.display().to_string();
        let remediation = format!(
            "Create {shown} and make it writable by the daemon. If it should already exist, check that library.root is the mounted library and that the mount is up."
        );
        match probe_root_bounded(&shown).await {
            Some(status) => match status.problem() {
                None => Outcome::ok(format!("{shown} is writable")),
                Some(problem) => Outcome::error(format!("{shown}: {problem}"), remediation),
            },
            None => Outcome::error(
                format!(
                    "{shown}: did not answer within {}s — a hung or disconnected mount is the usual cause",
                    ROOT_PROBE_TIMEOUT.as_secs()
                ),
                remediation,
            ),
        }
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

const ROOT_UNSET_REMEDIATION: &str = "Set library.root to the directory where the library is mounted: give the daemon the SKADI_LIBRARY_ROOT environment variable and restart it.";

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
        // The setting, not the resolved value: the registry default (`/data`)
        // fills an unset key, and a check of `/data` passed on any host that
        // happened to have one (SKADI-T-0681).
        let path = match library_root_setting(&self.store).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Outcome::error(
                    "library.root is not set, so skadi uses the default /data, which nobody chose as the library",
                    ROOT_UNSET_REMEDIATION,
                );
            }
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

// ---- disk space ----------------------------------------------------------------

/// Free space on the filesystem of the library root: a warning at
/// [`DISK_WARN_USED_PERCENT`], an error above [`DISK_ERROR_USED_PERCENT`].
/// Downloads and imports both land under the root, so a full root stops both.
pub struct DiskSpaceCheck {
    store: Store,
    probe: DiskProbe,
}

impl DiskSpaceCheck {
    pub fn new(store: Store, probe: DiskProbe) -> Self {
        DiskSpaceCheck { store, probe }
    }

    /// The verdict for a filesystem with `free` of `total` bytes. Pure.
    pub fn outcome(path: &str, free: u64, total: u64) -> Outcome {
        if total == 0 {
            return Outcome::error(
                format!("{path}: the filesystem reports a size of 0"),
                "Check that the library mount is up: an unmounted or broken network mount can report no size.",
            );
        }
        let used = 100.0 * (total.saturating_sub(free)) as f64 / total as f64;
        let message = format!(
            "{path}: {used:.0} % used, {} free of {}",
            human_bytes(free),
            human_bytes(total)
        );
        let remediation = "Free space on the library's filesystem (delete or move media, or remove finished downloads that no longer seed), or make the filesystem bigger.";
        if used > DISK_ERROR_USED_PERCENT {
            Outcome::error(message, remediation)
        } else if used >= DISK_WARN_USED_PERCENT {
            Outcome::warn(message, remediation)
        } else {
            Outcome::ok(message)
        }
    }
}

/// Bytes as GiB/TiB with one decimal (`1.5 TiB`), or MiB below one GiB.
fn human_bytes(n: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    let n = n as f64;
    if n >= TIB {
        format!("{:.1} TiB", n / TIB)
    } else if n >= GIB {
        format!("{:.1} GiB", n / GIB)
    } else {
        format!("{:.0} MiB", n / MIB)
    }
}

#[async_trait]
impl HealthCheck for DiskSpaceCheck {
    fn id(&self) -> String {
        "disk-space".into()
    }
    fn label(&self) -> String {
        "Disk space".into()
    }
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
        // `statvfs` on a hung mount blocks in the kernel: off the runtime and
        // with a deadline, like the root probe (SKADI-T-0430).
        let probe = self.probe.clone();
        let at = PathBuf::from(&path);
        let handle = tokio::task::spawn_blocking(move || probe(&at));
        match tokio::time::timeout(ROOT_PROBE_TIMEOUT, handle).await {
            Ok(Ok(Some((free, total)))) => Self::outcome(&path, free, total),
            Ok(Ok(None)) | Ok(Err(_)) => Outcome::error(
                format!(
                    "{path}: the free space cannot be read (the path is missing or not mounted)"
                ),
                "Fix the library root first (see the Library root check); the free space is measured there.",
            ),
            Err(_) => Outcome::error(
                format!(
                    "{path}: the free space did not answer within {}s — a hung or disconnected mount is the usual cause",
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
    fn ttl(&self) -> Duration {
        PROVIDER_CHECK_TTL
    }
    async fn run(&self) -> Outcome {
        // The search history of an indexer, read before the test: the test
        // itself records an outcome into it.
        let history = (self.kind == "indexers")
            .then(|| {
                uuid::Uuid::parse_str(&self.setting_id)
                    .ok()
                    .map(skadi_core::IndexerId::from)
                    .and_then(|iid| skadi_indexers::indexer_health().get(iid))
            })
            .flatten();
        // Known-down indexer → answer from the cached health registry instead of
        // a live test (SKADI-T-0308). Avoids serializing dead CloudFlare trackers
        // behind FlareSolverr, which was making this endpoint take ~10s.
        if let Some(h) = &history
            && h.consecutive_failures >= CHECK_CACHED_FAILS
        {
            let reason = h
                .last_error
                .clone()
                .unwrap_or_else(|| "unreachable (circuit open)".into());
            return Outcome::error(reason, self.remediation());
        }
        let started = Instant::now();
        let tested = match build_one(&self.store, self.kind, &self.setting_id).await {
            Ok(provider) => provider.test().await.map_err(|e| format!("{e}")),
            Err(e) => Err(format!("{e}")),
        };
        self.outcome(tested, started.elapsed(), history.as_ref())
    }
}

impl ProviderCheck {
    /// The verdict for a test that answered `tested` after `elapsed`, given the
    /// indexer's recent search history (`None` for a download client, or an
    /// indexer that has not searched yet). Pure.
    ///
    /// A provider that answers is ok, unless it answers slowly
    /// ([`PROVIDER_SLOW_AFTER`]) or its recent searches fail: the last one
    /// (a failure streak below the circuit breaker), or one in four of the
    /// recent window ([`PROVIDER_DEGRADED_FAILURE_RATE`]). Those are warnings:
    /// the provider works, but not well.
    pub fn outcome(
        &self,
        tested: Result<(), String>,
        elapsed: Duration,
        history: Option<&skadi_indexers::IndexerHealth>,
    ) -> Outcome {
        if let Err(e) = tested {
            return Outcome::error(e, self.remediation());
        }
        if elapsed > PROVIDER_SLOW_AFTER {
            return Outcome::warn(
                format!(
                    "reachable, but slow: the test took {}s (more than {}s)",
                    elapsed.as_secs(),
                    PROVIDER_SLOW_AFTER.as_secs()
                ),
                format!(
                    "{} is slow to answer. Check its load and the route to it (the VPN, FlareSolverr for a CloudFlare-fronted tracker).",
                    self.label()
                ),
            );
        }
        if let Some(h) = history {
            let degraded = format!(
                "{} answers its test, but its searches fail. Read the reason on the Indexers page and in the daemon log; a tracker that is overloaded or rate-limits skadi is the usual cause.",
                self.label()
            );
            if h.consecutive_failures > 0 {
                let reason = h.last_error.as_deref().unwrap_or("no reason recorded");
                return Outcome::warn(
                    format!(
                        "reachable, but its last {} search{} failed: {reason}",
                        h.consecutive_failures,
                        if h.consecutive_failures == 1 {
                            ""
                        } else {
                            "es"
                        }
                    ),
                    degraded,
                );
            }
            if let Some(rate) = h.recent_failure_rate()
                && rate >= PROVIDER_DEGRADED_FAILURE_RATE
            {
                let n = u32::from(h.recent_len);
                let failed = (rate * n as f32).round() as u32;
                return Outcome::warn(
                    format!("reachable, but {failed} of its last {n} searches failed"),
                    degraded,
                );
            }
        }
        Outcome::ok("reachable")
    }
}

// ---- rollups ---------------------------------------------------------------------

/// The summary of every configured indexer (`indexers`) or download client
/// (`download-clients`): an error when there is none — nothing can be searched
/// or downloaded — and otherwise the worst state over the
/// `indexer:*` / `downloader:*` checks: ok when each one is ok, an error when
/// none works, and a warning in between. This was worked out in the browser for
/// the dashboard's indexer chip (SKADI-T-0348); now every client reads it.
pub struct ProviderRollup {
    id: &'static str,
    label: &'static str,
    prefix: &'static str,
    /// What one member is called: `indexer`, `download client`.
    noun: &'static str,
    /// The same with its article: `an indexer`, `a download client`.
    a_noun: &'static str,
    /// The settings page that lists them, as the web UI labels it.
    page: &'static str,
}

impl ProviderRollup {
    pub const INDEXERS: ProviderRollup = ProviderRollup {
        id: "indexers",
        label: "Indexers",
        prefix: "indexer:",
        noun: "indexer",
        a_noun: "an indexer",
        page: "Settings → Indexers",
    };

    pub const DOWNLOAD_CLIENTS: ProviderRollup = ProviderRollup {
        id: "download-clients",
        label: "Download clients",
        prefix: "downloader:",
        noun: "download client",
        a_noun: "a download client",
        page: "Settings → Downloads",
    };
}

#[async_trait]
impl HealthCheck for ProviderRollup {
    fn id(&self) -> String {
        self.id.into()
    }
    fn label(&self) -> String {
        self.label.into()
    }
    async fn run(&self) -> Outcome {
        // Never called: a rollup is summarized from its members.
        Outcome::ok("summary")
    }
    fn rollup_prefix(&self) -> Option<&'static str> {
        Some(self.prefix)
    }
    fn summarize(&self, members: &[&CheckResult]) -> Option<Outcome> {
        let total = members.len();
        if total == 0 {
            let what = if self.id == "indexers" {
                "nothing can be searched"
            } else {
                "nothing can be downloaded"
            };
            return Some(Outcome::error(
                format!("no {} is configured, so {what}", self.noun),
                format!("Add {} under {}.", self.a_noun, self.page),
            ));
        }
        let count = |s: Severity| members.iter().filter(|r| r.severity == s).count();
        let (ok, warn, error, pending) = (
            count(Severity::Ok),
            count(Severity::Warn),
            count(Severity::Error),
            count(Severity::Pending),
        );
        if pending == total {
            return None;
        }
        let checked = total - pending;
        let reachable = ok + warn;
        let mut message = if ok == total {
            format!("{total} reachable")
        } else {
            format!("{reachable}/{checked} reachable")
        };
        if warn > 0 {
            message.push_str(&format!(", {warn} slow or degraded"));
        }
        if pending > 0 {
            message.push_str(&format!(", {pending} not checked yet"));
        }
        let remediation = format!(
            "Open {} for the reason of each {} that is not ok.",
            self.page, self.noun
        );
        Some(if error == checked {
            Outcome::error(message, remediation)
        } else if error > 0 || warn > 0 {
            Outcome::warn(message, remediation)
        } else {
            Outcome::ok(message)
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn store() -> Option<(tempfile::TempDir, Store)> {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("t.db").display());
        // No SQLite backend in this build: the BDD scenarios cover these checks
        // against a real store.
        let store = Store::connect(&url).ok()?;
        Some((dir, store))
    }

    fn result(id: &str, severity: Severity) -> CheckResult {
        CheckResult {
            id: id.into(),
            label: id.into(),
            severity,
            message: String::new(),
            remediation: None,
            checked_at: (severity != Severity::Pending).then(Utc::now),
        }
    }

    #[test]
    fn disk_space_warns_from_75_percent_and_errors_above_90() {
        let total = 1000u64 << 30;
        let at = |used: u64| DiskSpaceCheck::outcome("/lib", total / 100 * (100 - used), total);
        assert_eq!(at(50).severity, Severity::Ok);
        assert_eq!(at(74).severity, Severity::Ok);
        assert_eq!(at(75).severity, Severity::Warn);
        let eighty = at(80);
        assert_eq!(eighty.severity, Severity::Warn);
        assert!(eighty.message.contains("80 % used"), "{}", eighty.message);
        assert!(eighty.remediation.is_some());
        assert_eq!(at(90).severity, Severity::Warn);
        assert_eq!(at(91).severity, Severity::Error);
        assert_eq!(at(100).severity, Severity::Error);
        assert_eq!(
            DiskSpaceCheck::outcome("/lib", 0, 0).severity,
            Severity::Error,
            "a filesystem with no size is not a healthy one"
        );
    }

    #[test]
    fn a_domain_worker_that_keeps_dying_is_an_error() {
        let check = DomainCheck {
            domain: "movies".into(),
            worker_failures: Arc::default(),
        };
        assert_eq!(check.outcome(0), Outcome::ok("enabled"));
        let once = check.outcome(1);
        assert_eq!(once.severity, Severity::Warn);
        assert!(once.message.contains("1 time "), "{}", once.message);
        let three = check.outcome(DOMAIN_WORKER_FAILURES_ERROR);
        assert_eq!(three.severity, Severity::Error);
        assert!(three.message.contains("3 times"), "{}", three.message);
        assert!(three.remediation.unwrap().contains("movies"));
    }

    #[tokio::test]
    async fn a_domain_check_reads_the_supervisor_failure_count() {
        let failures: Arc<tokio::sync::Mutex<HashMap<String, u64>>> = Arc::default();
        failures.lock().await.insert("movies".into(), 4);
        let check = DomainCheck {
            domain: "movies".into(),
            worker_failures: failures,
        };
        assert_eq!(check.run().await.severity, Severity::Error);
    }

    #[test]
    fn a_provider_that_answers_slowly_or_fails_its_searches_is_a_warning() {
        let Some((_dir, store)) = store() else {
            return;
        };
        let check = ProviderCheck {
            store,
            kind: "indexers",
            setting_id: "x".into(),
            name: "ix".into(),
        };
        let fast = Duration::from_millis(50);
        assert_eq!(check.outcome(Ok(()), fast, None), Outcome::ok("reachable"));

        let down = check.outcome(Err("refused".into()), fast, None);
        assert_eq!(down.severity, Severity::Error);
        assert_eq!(down.message, "refused");

        let slow = check.outcome(Ok(()), PROVIDER_SLOW_AFTER + Duration::from_secs(1), None);
        assert_eq!(slow.severity, Severity::Warn);
        assert!(slow.message.contains("slow"), "{}", slow.message);

        let reg = skadi_indexers::IndexerHealthRegistry::default();
        let id = skadi_core::IndexerId::from(uuid::Uuid::new_v4());
        for _ in 0..3 {
            reg.record_failure(id, "HTTP 503");
        }
        for _ in 0..7 {
            reg.record_success(id);
        }
        let flaky = check.outcome(Ok(()), fast, reg.get(id).as_ref());
        assert_eq!(flaky.severity, Severity::Warn);
        assert!(
            flaky.message.contains("3 of its last 10"),
            "{}",
            flaky.message
        );

        reg.record_failure(id, "timed out");
        let streak = check.outcome(Ok(()), fast, reg.get(id).as_ref());
        assert_eq!(streak.severity, Severity::Warn);
        assert!(streak.message.contains("timed out"), "{}", streak.message);

        let calm = skadi_indexers::IndexerHealthRegistry::default();
        for _ in 0..9 {
            calm.record_success(id);
        }
        calm.record_failure(id, "once");
        calm.record_success(id);
        assert_eq!(
            check.outcome(Ok(()), fast, calm.get(id).as_ref()),
            Outcome::ok("reachable"),
            "one failure in eleven is below the degraded rate"
        );
    }

    #[test]
    fn the_rollup_is_an_error_with_no_member_and_the_worst_state_otherwise() {
        let r = ProviderRollup::INDEXERS;
        let none = r.summarize(&[]).expect("an empty rollup is known now");
        assert_eq!(none.severity, Severity::Error);
        assert!(none.remediation.unwrap().contains("Settings → Indexers"));
        let dl = ProviderRollup::DOWNLOAD_CLIENTS.summarize(&[]).unwrap();
        assert!(
            dl.remediation
                .unwrap()
                .contains("Add a download client under Settings → Downloads")
        );

        let ok = result("indexer:a", Severity::Ok);
        let warn = result("indexer:b", Severity::Warn);
        let bad = result("indexer:c", Severity::Error);
        let pending = result("indexer:d", Severity::Pending);

        assert_eq!(
            r.summarize(&[&ok, &ok]).unwrap(),
            Outcome::ok("2 reachable")
        );
        let mixed = r.summarize(&[&ok, &bad]).unwrap();
        assert_eq!(mixed.severity, Severity::Warn);
        assert_eq!(mixed.message, "1/2 reachable");
        assert_eq!(r.summarize(&[&ok, &warn]).unwrap().severity, Severity::Warn);
        assert_eq!(
            r.summarize(&[&bad, &bad]).unwrap().severity,
            Severity::Error
        );
        assert_eq!(r.summarize(&[&pending]), None, "nothing has run yet");
        let partly = r.summarize(&[&ok, &pending]).unwrap();
        assert_eq!(partly.severity, Severity::Ok);
        assert!(
            partly.message.contains("1 not checked yet"),
            "{}",
            partly.message
        );
    }

    #[test]
    fn resolve_summarizes_a_rollup_over_the_stored_member_results() {
        let checks: Vec<Arc<dyn HealthCheck>> =
            vec![Arc::new(ProviderRollup::INDEXERS), Arc::new(DaemonCheck)];
        let stored = HashMap::new();
        let out = super::super::resolve(&checks, &stored);
        assert_eq!(out[0].id, "indexers");
        assert_eq!(out[0].severity, Severity::Error, "no indexer at all");
        assert!(out[0].checked_at.is_some());
        assert_eq!(out[1].severity, Severity::Pending);
    }
}
