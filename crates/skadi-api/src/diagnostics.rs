//! Health checks + root-folder free-space diagnostics (SKADI-T-0116).
//!
//! The endpoints the Home dashboard ([[SKADI-T-0072]]) renders as badges:
//!
//! - `GET /health/checks` — the results of the [`crate::health_checks`]
//!   registry (SKADI-T-0679): one `{ id, label, severity, message, remediation,
//!   checked_at }` per check, plus the old `{ name, status, detail }` as a
//!   projection. It reads the stored results (SKADI-T-0680): the supervisor
//!   tick runs the checks, so one slow/dead provider cannot slow the request.
//! - `POST /health/checks/run[?id=]` — run the checks now (admin only).
//! - `GET /root-folders` — per root folder: path, existence, writability, and
//!   free/total bytes (via `statvfs`), so the UI can warn before a download
//!   fills the disk (a real risk on the space-limited deploy host).

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Serialize;

use skadi_core::AppError;
use skadi_store::{DomainStateRepo, Store};

use crate::error::ApiError;
use crate::health_checks::CheckContext;
use crate::state::AppState;

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
        .route("/health/checks/run", post(run_health_checks))
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

/// The commit `build.rs` embedded (SKADI-T-0684).
pub const BUILD_COMMIT: &str = env!("SKADI_BUILD_COMMIT");

/// When this process started, for the uptime `/system/status` reports.
static STARTED_AT: std::sync::LazyLock<chrono::DateTime<chrono::Utc>> =
    std::sync::LazyLock::new(chrono::Utc::now);

/// `GET /system/status` — Sonarr's System → Status (SKADI-T-0467): what is
/// running, since when, against which database and which library root.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    version: &'static str,
    /// The git commit this binary was built from (SKADI-T-0684), or `unknown`
    /// when the build had neither `SKADI_BUILD_COMMIT` nor a git checkout.
    commit: &'static str,
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
        commit: BUILD_COMMIT,
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

/// `GET /health/checks` — the stored results; runs no check (SKADI-T-0680). A
/// check that has not run yet is `pending`, with `checked_at: null`.
async fn health_checks(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let ctx = CheckContext::from_state(&state)?;
    Ok(Json(state.health.snapshot(&ctx).await))
}

/// Query for `POST /health/checks/run`.
#[derive(serde::Deserialize)]
struct RunChecksQuery {
    /// Run only this check. Without it, every check runs.
    id: Option<String>,
}

/// `POST /health/checks/run[?id=<check id>]` — run the checks now, store the
/// results, and return every check as `GET /health/checks` would (SKADI-T-0680).
/// Admin only: the household gate refuses a POST here to every other role. It
/// waits for the checks it runs, each within its own timeout.
async fn run_health_checks(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<RunChecksQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = CheckContext::from_state(&state)?;
    match state.health.run_now(&ctx, q.id.as_deref()).await {
        Some(results) => Ok(Json(results)),
        None => Err(ApiError(AppError::NotFound(format!(
            "no health check {:?}",
            q.id.unwrap_or_default()
        )))),
    }
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

/// How long a filesystem probe may take before health gives up on it.
pub(crate) const ROOT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Probe a root off the runtime and with a deadline (SKADI-T-0430).
///
/// A stat or a probe-file write against a hung NFS mount blocks indefinitely in
/// the kernel. `spawn_blocking` keeps it off the async runtime, but awaiting the
/// handle would still hang the caller — and this runs inside `/health/checks`,
/// so the whole endpoint would hang with it. `None` means the probe timed out;
/// the blocking thread is left to finish on its own, which it will when the
/// mount recovers.
pub(crate) async fn probe_root_bounded(path: &str) -> Option<skadi_core::RootFolderStatus> {
    let probe_path = std::path::PathBuf::from(path);
    let handle = tokio::task::spawn_blocking(move || skadi_core::probe_root_status(&probe_path));
    match tokio::time::timeout(ROOT_PROBE_TIMEOUT, handle).await {
        Ok(joined) => joined.ok(),
        Err(_) => None,
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
pub(crate) async fn library_root_path(store: &Store) -> Result<String, ApiError> {
    let view = crate::load_config_view(store).await?;
    Ok(view
        .get_path("library.root")
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/data".to_string()))
}

/// `library.root` as the operator set it (the `config` table, which the
/// `SKADI_LIBRARY_ROOT` environment variable seeds on boot), or `None` when it
/// is not set. Unlike [`library_root_path`], no registry default fills the gap:
/// the health check must tell "set to /data" from "not set" (SKADI-T-0681).
pub(crate) async fn library_root_setting(store: &Store) -> Result<Option<String>, ApiError> {
    use skadi_store::ConfigRepo;
    let entry = store.get_config("library.root").await.map_err(ApiError)?;
    Ok(entry.map(|e| e.value).filter(|v| !v.trim().is_empty()))
}

/// `(free_bytes, total_bytes)` for the filesystem backing `path`, via `statvfs`.
/// `None` if the path is missing or the platform has no `statvfs`.
#[cfg(unix)]
// `libc::statvfs` field widths differ by platform: on Linux — the only place
// skadi actually runs, since it ships as a container — these are already
// `u64` and the casts below are no-ops, so clippy calls them unnecessary. On
// macOS, where this is developed, some are narrower and the widening is
// required to compile at all. The cast is correct on both; only the lint is
// platform-specific. Same reasoning as `skadi_importer::statvfs_available`.
#[allow(clippy::unnecessary_cast)]
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
