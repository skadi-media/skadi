//! Health checks + root-folder free-space diagnostics (SKADI-T-0116).
//!
//! Two read-only endpoints the Home dashboard ([[SKADI-T-0072]]) renders as
//! badges:
//!
//! - `GET /health/checks` — a flat list of `{ name, status: ok|warn|fail,
//!   detail }` covering the daemon version, DB reachability, each domain's
//!   enabled state, and every configured indexer/downloader's reachability
//!   (reusing the provider `test()` path). Provider checks run concurrently with
//!   a per-provider timeout so one slow/dead provider can't hang the endpoint.
//! - `GET /root-folders` — per root folder: path, existence, writability, and
//!   free/total bytes (via `statvfs`), so the UI can warn before a download
//!   fills the disk (a real risk on the space-limited deploy host).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;

use skadi_core::AppError;
use skadi_store::{DomainStateRepo, SettingsRepo, Store};

use crate::error::ApiError;
use crate::providers::build_one;
use crate::state::{AppState, DomainDescriptor};

/// Per-provider reachability timeout — a slow/dead provider reports `fail`
/// rather than hanging the whole endpoint. 30 s, not 10 (SKADI-T-0587): a
/// CloudFlare-fronted tracker answers through FlareSolverr, whose solve alone
/// routinely takes 10–20 s, so the shorter budget reported nine of nineteen
/// working indexers as failing.
const PROVIDER_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Consecutive failures at which an indexer's health check answers from the cached
/// health registry instead of a live `test()` (SKADI-T-0308). Live-testing a dead
/// CloudFlare tracker serializes behind the single-threaded FlareSolverr and made the
/// whole `/health/checks` page take ~10s; once the search path has marked it down this
/// many times we trust that. Matches the search circuit-breaker default.
const CHECK_CACHED_FAILS: u32 = 3;

/// One health check result, rendered as a badge by the UI.
#[derive(Serialize, Debug, Clone)]
pub struct Check {
    pub name: String,
    /// `ok` | `warn` | `fail`.
    pub status: &'static str,
    pub detail: String,
}

impl Check {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            status: "ok",
            detail: detail.into(),
        }
    }
    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            status: "fail",
            detail: detail.into(),
        }
    }
}

/// Free-space + health report for one configured root folder (SKADI-T-0232). Status is
/// the shared `skadi_core::probe_root_status`; `usable`/`problem` give the UI a single
/// badge ("missing", "not writable", …) without re-deriving it from the booleans.
#[derive(Serialize, Debug, Clone)]
pub struct RootFolderReport {
    pub id: String,
    pub path: String,
    /// Whether the path exists (as anything).
    pub exists: bool,
    /// Whether the path is a directory.
    pub is_dir: bool,
    /// Whether a probe file could be created (mount rw + permissions).
    pub writable: bool,
    /// `exists && is_dir && writable` — ready to hold a library.
    pub usable: bool,
    /// The first reason the root is unusable, or `null` when it's fine.
    pub problem: Option<String>,
    /// Bytes available to an unprivileged user; `None` if the path is missing or
    /// `statvfs` is unavailable on the platform.
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

/// Routes for the diagnostics endpoints, merged into the authed API router.
pub fn diagnostics_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health/checks", get(health_checks))
        .route("/root-folders", get(root_folders))
        .route("/root-folders/{id}/unmapped", get(unmapped_folders))
        .route("/system/status", get(system_status))
        .route("/system/task", get(system_tasks))
        .route("/log", get(system_log))
}

/// Query for `GET /log`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogQuery {
    /// How many lines to return, newest first. Capped at the ring's capacity.
    page_size: Option<usize>,
}

/// `GET /log?pageSize=n` — the daemon's recent log lines (SKADI-T-0467).
///
/// Skadi logs to stdout, so unlike Sonarr there is no log *file* to serve. This
/// reads the in-memory ring instead: enough to answer "what just happened"
/// without shelling into the host. It is bounded and does not survive a restart
/// — `docker logs` and the host journal are still the real record.
async fn system_log(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<LogQuery>,
) -> impl IntoResponse {
    let limit = q.page_size.unwrap_or(100).min(crate::logbuf::CAPACITY);
    Json(state.logs.recent(limit))
}

/// When this process started, for the uptime `/system/status` reports.
static STARTED_AT: std::sync::LazyLock<chrono::DateTime<chrono::Utc>> =
    std::sync::LazyLock::new(chrono::Utc::now);

/// `GET /system/status` — Sonarr's System → Status (SKADI-T-0467): what is
/// running, since when, against which database and which library root.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    version: &'static str,
    start_time: String,
    uptime_seconds: i64,
    /// `postgres` or `sqlite`, derived from the configured URL scheme.
    database: &'static str,
    library_root: String,
    /// The domains compiled into this build, and whether each is enabled.
    domains: Vec<DomainState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DomainState {
    name: String,
    enabled: bool,
    /// How many of this domain's workers have ended without being asked to
    /// (SKADI-T-0523). `0` for a healthy domain. Non-zero and climbing means it
    /// is crash-looping: the supervisor restarts it each tick, so `enabled` alone
    /// would show it as fine the whole time.
    #[serde(skip_serializing_if = "is_zero")]
    worker_failures: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

async fn system_status(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let url = state.config.database_url.as_str();
    let database = if url.starts_with("postgres") {
        "postgres"
    } else {
        "sqlite"
    };
    let failures = state.worker_failures.lock().await.clone();
    let mut domains = Vec::with_capacity(state.domains.len());
    for d in &state.domains {
        let enabled = store
            .get(&d.name)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.enabled);
        domains.push(DomainState {
            name: d.name.clone(),
            enabled,
            worker_failures: failures.get(&d.name).copied().unwrap_or(0),
        });
    }
    let started = *STARTED_AT;
    Ok(Json(SystemStatus {
        version: env!("CARGO_PKG_VERSION"),
        start_time: started.to_rfc3339(),
        uptime_seconds: chrono::Utc::now()
            .signed_duration_since(started)
            .num_seconds(),
        database,
        library_root: library_root_path(store).await?,
        domains,
    }))
}

/// One recurring job the daemon runs.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskInfo {
    name: &'static str,
    interval_seconds: u64,
    what: &'static str,
    /// `None` for every task today — see the note on [`system_tasks`].
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<String>,
}

/// `GET /system/task` — the recurring jobs and their cadence (SKADI-T-0467).
///
/// Reports what the daemon genuinely runs, read from the same constants the
/// workers use, so the list cannot drift from reality by being hand-maintained.
///
/// It deliberately does **not** report last/next run times: there is no task
/// registry, and housekeeping runs inside a domain's hunter tick rather than as
/// a scheduled job of its own, so nothing records when a task last ran. Emitting
/// a plausible-looking timestamp would be worse than omitting the field.
/// SKADI-T-0440 is the registry that would let this answer honestly, and would
/// also fix the related bug that with every domain disabled, nothing purges.
async fn system_tasks() -> impl IntoResponse {
    Json(vec![
        TaskInfo {
            name: "supervisor-reconcile",
            interval_seconds: crate::supervisor::DEFAULT_TICK_INTERVAL.as_secs(),
            what: "start/stop domain workers and republish providers when settings change",
            last_run: None,
        },
        TaskInfo {
            name: "rss-sweep",
            interval_seconds: skadi_hunter::DEFAULT_RSS_INTERVAL.as_secs(),
            what: "fast pass over indexer RSS feeds for newly-wanted items",
            last_run: None,
        },
        TaskInfo {
            name: "full-sweep",
            interval_seconds: 300,
            what: "search every wanted and upgradable item, per enabled domain",
            last_run: None,
        },
        TaskInfo {
            name: "housekeeping",
            interval_seconds: 300,
            what: "history purge, blocklist expiry and download-lease reclaim (runs inside a domain sweep, so it does not run with every domain disabled)",
            last_run: None,
        },
    ])
}

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

async fn health_checks(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let checks = run_health_checks(store(&state)?, &state.domains).await;
    Ok(Json(checks))
}

async fn root_folders(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let reports = root_folder_reports(store(&state)?).await?;
    Ok(Json(reports))
}

/// `GET /root-folders/{id}/unmapped` — the root's immediate child directories that no
/// library item occupies (SKADI-T-0233): the basis of an "add existing media" view.
async fn unmapped_folders(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    // Single library root (SKADI-T-0302): the `{id}` path segment is legacy and
    // ignored — there is one root, `library.root`.
    let path = std::path::PathBuf::from(library_root_path(store).await?);

    // Immediate child directories on disk (blocking I/O off the runtime).
    let root = path.clone();
    let children = tokio::task::spawn_blocking(move || immediate_child_dirs(&root))
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("scan task panicked: {e}"))))?;

    // Folders occupied by any domain's library items (best-effort per provider).
    let mut occupied: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for provider in &state.library {
        if let Ok(folders) = provider.occupied_folders().await {
            occupied.extend(folders);
        }
    }

    let unmapped: Vec<String> = crate::library::unmapped_subfolders(&children, &occupied)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok(Json(unmapped))
}

/// The immediate child directories of `root` (absolute paths); empty if `root` is
/// missing or unreadable. Blocking.
fn immediate_child_dirs(root: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect()
}

/// Aggregate the daemon health checks. Order: daemon version, database, each
/// domain, then providers (sorted by name). Never errors — a failed sub-check
/// becomes a `fail`/`warn` entry, not an HTTP error, so the dashboard always
/// renders.
pub async fn run_health_checks(store: &Store, domains: &[DomainDescriptor]) -> Vec<Check> {
    let mut checks = vec![Check::ok(
        "daemon",
        format!("skadi {}", env!("CARGO_PKG_VERSION")),
    )];

    // Database reachability: a cheap read that touches the connection.
    match store.list_settings("profiles").await {
        Ok(_) => checks.push(Check::ok("database", "reachable")),
        Err(e) => checks.push(Check::fail("database", format!("{e}"))),
    }

    // Only enabled domains surface in Health — a disabled domain isn't a warning
    // to act on, and showing it here contradicted the sidebar/dashboard, which
    // list enabled domains only.
    for d in domains {
        let enabled = store
            .get(&d.name)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.enabled);
        if enabled {
            checks.push(Check::ok(format!("domain:{}", d.name), "enabled"));
        }
    }

    // The library root (SKADI-T-0467/0430): every import lands here, so a
    // missing, non-directory or read-only root is the single most consequential
    // misconfiguration in the stack — and it used to be invisible on the health
    // page until an import failed.
    checks.push(root_check(store).await);

    // The download worker (SKADI-T-0467). It runs out-of-process, in the VPN
    // namespace, and reaches the daemon only through the database, so its
    // heartbeat row is the only thing that says it is alive.
    checks.push(worker_check(store).await);

    checks.extend(provider_checks(store).await);
    checks
}

/// How stale a worker heartbeat may be before the worker counts as silent. The
/// worker ticks every few seconds; two minutes is well clear of a slow tick and
/// still notices a wedged or dead worker promptly.
const WORKER_SILENT_AFTER: chrono::Duration = chrono::Duration::minutes(2);

/// Health of the configured library root.
async fn root_check(store: &Store) -> Check {
    let path = match library_root_path(store).await {
        Ok(p) => p,
        Err(e) => return Check::fail("root", format!("library.root unreadable: {e:?}")),
    };
    match probe_root_bounded(&path).await {
        Some(status) => match status.problem() {
            None => Check::ok("root", format!("{path} is writable")),
            Some(problem) => Check::fail("root", format!("{path}: {problem}")),
        },
        // A health endpoint that hangs is worse than one that reports a hang
        // (SKADI-T-0430, NFR-ROOTS.1).
        None => Check::fail(
            "root",
            format!(
                "{path}: did not answer within {}s — a hung or disconnected mount is the usual cause",
                ROOT_PROBE_TIMEOUT.as_secs()
            ),
        ),
    }
}

/// How long a filesystem probe may take before health gives up on it.
const ROOT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Probe a root off the runtime and with a deadline (SKADI-T-0430).
///
/// A stat or a probe-file write against a hung NFS mount blocks indefinitely in
/// the kernel. `spawn_blocking` keeps it off the async runtime, but awaiting the
/// handle would still hang the caller — and this runs inside `/health/checks`,
/// so the whole endpoint would hang with it. `None` means the probe timed out;
/// the blocking thread is left to finish on its own, which it will when the
/// mount recovers.
async fn probe_root_bounded(path: &str) -> Option<skadi_core::RootFolderStatus> {
    let probe_path = std::path::PathBuf::from(path);
    let handle = tokio::task::spawn_blocking(move || skadi_core::probe_root_status(&probe_path));
    match tokio::time::timeout(ROOT_PROBE_TIMEOUT, handle).await {
        Ok(joined) => joined.ok(),
        Err(_) => None,
    }
}

/// Liveness of the download worker, from its heartbeat row (SKADI-T-0288).
async fn worker_check(store: &Store) -> Check {
    use skadi_store::WorkerStatusRepo;
    match store.latest_worker_status().await {
        Err(e) => Check::fail("worker", format!("heartbeat unreadable: {e}")),
        Ok(None) => Check::fail(
            "worker",
            "no download worker has ever checked in — nothing will download",
        ),
        Ok(Some(w)) if w.is_fresh(WORKER_SILENT_AFTER) => Check::ok(
            "worker",
            format!(
                "{} (v{}) last seen {}",
                w.worker_id,
                w.version,
                w.last_seen_at.to_rfc3339()
            ),
        ),
        Ok(Some(w)) => Check::fail(
            "worker",
            format!(
                "{} last checked in {} — silent for more than {} minutes",
                w.worker_id,
                w.last_seen_at.to_rfc3339(),
                WORKER_SILENT_AFTER.num_minutes()
            ),
        ),
    }
}

/// Reachability of every configured indexer/downloader, run concurrently with a
/// per-provider timeout. Results are sorted by name for a stable response.
async fn provider_checks(store: &Store) -> Vec<Check> {
    let mut targets: Vec<(&'static str, String, String)> = Vec::new();
    for kind in ["indexers", "downloaders"] {
        for row in store.list_settings(kind).await.unwrap_or_default() {
            let name = row
                .body
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| row.id.clone());
            targets.push((kind, row.id.clone(), name));
        }
    }

    let mut set = tokio::task::JoinSet::new();
    let mut cached: Vec<Check> = Vec::new();
    for (kind, id, name) in targets {
        let label = format!("{}:{}", singular(kind), name);
        // Known-down indexer → answer from the cached health registry instead of a
        // live test (SKADI-T-0308). Avoids serializing dead CloudFlare trackers behind
        // FlareSolverr, which was making this endpoint take ~10s.
        if kind == "indexers"
            && let Some(h) = uuid::Uuid::parse_str(&id)
                .ok()
                .map(skadi_core::IndexerId::from)
                .and_then(|iid| skadi_indexers::indexer_health().get(iid))
            && h.consecutive_failures >= CHECK_CACHED_FAILS
        {
            let reason = h
                .last_error
                .unwrap_or_else(|| "unreachable (circuit open)".into());
            cached.push(Check::fail(label, reason));
            continue;
        }
        let store = store.clone();
        set.spawn(async move {
            match build_one(&store, kind, &id).await {
                Ok(provider) => {
                    match tokio::time::timeout(PROVIDER_TEST_TIMEOUT, provider.test()).await {
                        Ok(Ok(())) => Check::ok(label, "reachable"),
                        Ok(Err(e)) => Check::fail(label, format!("{e}")),
                        Err(_) => Check::fail(
                            label,
                            format!("timed out after {}s", PROVIDER_TEST_TIMEOUT.as_secs()),
                        ),
                    }
                }
                Err(e) => Check::fail(label, format!("{e}")),
            }
        });
    }

    let mut checks = cached;
    while let Some(joined) = set.join_next().await {
        if let Ok(check) = joined {
            checks.push(check);
        }
    }
    checks.sort_by(|a, b| a.name.cmp(&b.name));
    checks
}

fn singular(kind: &str) -> &str {
    match kind {
        "indexers" => "indexer",
        "downloaders" => "downloader",
        other => other,
    }
}

/// Build a free-space/writability report for the single library root
/// (SKADI-T-0302). skadi owns one `library.root`; there is no operator list, so
/// this returns exactly one report (the UI shows the one mount's health/space).
pub async fn root_folder_reports(store: &Store) -> Result<Vec<RootFolderReport>, ApiError> {
    let path = library_root_path(store).await?;
    // Bounded and off the runtime, for the same reason as the health check
    // (SKADI-T-0430): this endpoint backs the root-folder UI, and a hung mount
    // must not hang it. A timed-out probe reports the root as unusable with the
    // reason, which is what the operator needs to see.
    let status = probe_root_bounded(&path).await.unwrap_or_default();
    let p = Path::new(&path);
    let (free_bytes, total_bytes) = match disk_space(p) {
        Some((free, total)) => (Some(free), Some(total)),
        None => (None, None),
    };
    Ok(vec![RootFolderReport {
        id: "library-root".to_string(),
        path,
        exists: status.exists,
        is_dir: status.is_dir,
        writable: status.writable,
        usable: status.is_usable(),
        problem: status.problem().map(str::to_string),
        free_bytes,
        total_bytes,
    }])
}

/// The single `library.root` path string from the config plane (SKADI-T-0302).
async fn library_root_path(store: &Store) -> Result<String, ApiError> {
    let view = crate::load_config_view(store).await?;
    Ok(view
        .get_path("library.root")
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/data".to_string()))
}

/// `(free_bytes, total_bytes)` for the filesystem backing `path`, via `statvfs`.
/// `None` if the path is missing or the platform has no `statvfs`.
#[cfg(unix)]
pub(crate) fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;

    if !path.exists() {
        return None;
    }
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c_path` is a valid NUL-terminated path; `stat` is zero-initialized
    // and only read after a `0` (success) return.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        // `f_frsize` is the fragment size (bytes/block); `f_bavail` is blocks
        // free to an unprivileged user; `f_blocks` is total blocks.
        let frsize = stat.f_frsize as u64;
        Some((stat.f_bavail as u64 * frsize, stat.f_blocks as u64 * frsize))
    }
}

#[cfg(not(unix))]
pub(crate) fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_space_reports_for_a_real_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (free, total) = disk_space(dir.path()).expect("statvfs on a temp dir");
        assert!(total > 0, "total bytes should be positive");
        assert!(free <= total, "free must not exceed total");
    }

    #[test]
    fn disk_space_is_none_for_missing_path() {
        assert!(disk_space(Path::new("/no/such/skadi/path/xyz")).is_none());
    }

    #[test]
    fn root_status_usable_for_temp_dir_and_unusable_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(skadi_core::probe_root_status(dir.path()).is_usable());
        let missing = skadi_core::probe_root_status(Path::new("/no/such/skadi/path/xyz"));
        assert!(!missing.is_usable());
        assert_eq!(missing.problem(), Some("path does not exist"));
    }
}
