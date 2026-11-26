//! The storage facts of the library, for the "Library details" page
//! (SKADI-T-0774, design SKADI-D-0040).
//!
//! - `GET /library/storage/summary`: totals by kind, the orphans, the free
//!   space of the library, and where each storage sweep is.
//! - `GET /library/storage/files`: one page of the file list, sorted and
//!   filtered in the database.
//! - `GET /library/storage/orphans`: one page of the files on disk that no
//!   item holds.
//!
//! Admin only. `household::path_allowed` refuses every path it does not name
//! to a non-admin, and it names none of these.
//!
//! These read what the collectors wrote (`skadi_library_scan::storage`). They
//! never touch the disk for a file, so a slow share cannot slow the page.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use skadi_core::AppError;
use skadi_library_scan::probe;
use skadi_store::{
    LibraryFile, LibraryFileQuery, LibraryFileRepo, LibraryFileSort, LibraryOrphan,
    StorageProbeRepo, Store,
};

use crate::error::ApiError;
use crate::state::AppState;

/// The most rows one page may ask for.
pub const MAX_PAGE: i64 = 200;
/// The rows of a page when the caller does not say.
pub const DEFAULT_PAGE: i64 = 50;

/// Routes of the storage facts, merged into the authed API router.
pub fn library_storage_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/library/storage/summary", get(summary))
        .route("/library/storage/files", get(files))
        .route("/library/storage/orphans", get(orphans))
        .route("/storage/probe", get(probe_state))
        .route("/storage/probe/clear", post(probe_clear))
        .route(
            "/storage/probe/credential",
            put(probe_set_credential).delete(probe_delete_credential),
        )
}

/// The piece probe of the storage host (SKADI-T-0775), for the daemon to
/// spawn: it runs until `cancel` fires. Idle while the probe is off.
pub async fn piece_probe_loop(store: Store, cancel: tokio_util::sync::CancellationToken) {
    use skadi_core::module::Worker as _;
    Box::new(probe::PieceProbeWorker::new(store))
        .run(cancel)
        .await;
}

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

/// `GET /library/storage/summary`.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageSummary {
    /// One entry per kind that has a file row, in kind order.
    pub kinds: Vec<KindSummary>,
    pub orphans: OrphanSummary,
    /// Free and total bytes of the filesystem the library root is on. `null`
    /// when the root is not there.
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// The storage sweeps the daemon has started since it started.
    pub sweeps: Vec<SweepSummary>,
}

/// The totals of one kind. Sizes and counts leave out the files that are gone.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KindSummary {
    /// `movie`, `series` or `audiobook`.
    pub kind: String,
    pub files: i64,
    pub size_bytes: i64,
    pub disk_bytes: i64,
    /// Files with more than one hard link.
    pub linked: i64,
    /// Files with a current piece count, their bytes and their pieces. All 0
    /// until the piece probe runs.
    pub measured: i64,
    pub measured_bytes: i64,
    pub pieces: i64,
    /// Files with a current checksum. 0 until the checksum collector runs.
    pub checksummed: i64,
    /// Items whose file was not found on disk.
    pub gone: i64,
    /// When a file of this kind was last read from the library (RFC 3339).
    pub last_read_at: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrphanSummary {
    pub files: i64,
    pub size_bytes: i64,
}

/// Where one domain's storage sweep is.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SweepSummary {
    pub name: String,
    pub kind: String,
    pub running: bool,
    /// When the pass that runs now, or the last one, began (RFC 3339).
    pub started_at: Option<String>,
    /// When the last pass ended. `null` = none has ended since the daemon
    /// started.
    pub finished_at: Option<String>,
    /// Why the last pass did not run to the end; `null` when it did.
    pub skipped: Option<String>,
    /// What the last pass did. `null` before the first pass ends.
    pub last: Option<SweepCounts>,
}

#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SweepCounts {
    pub files: usize,
    pub recorded: usize,
    pub gone: usize,
    pub unreadable: usize,
    pub dropped: usize,
    pub orphans: usize,
    pub pruned: usize,
}

async fn summary(State(state): State<Arc<AppState>>) -> Result<Json<StorageSummary>, ApiError> {
    let store = store(&state)?;
    let mut kinds = Vec::new();
    for t in store.library_totals_by_kind().await? {
        let last = store.library_last_stat_at(&t.kind).await?;
        kinds.push(KindSummary {
            last_read_at: last.map(|t| t.to_rfc3339()),
            kind: t.kind,
            files: t.files,
            size_bytes: t.size_bytes,
            disk_bytes: t.disk_bytes,
            linked: t.linked,
            measured: t.measured,
            measured_bytes: t.measured_bytes,
            pieces: t.pieces,
            checksummed: t.checksummed,
            gone: t.gone,
        });
    }
    let (orphan_files, orphan_bytes) = store.library_orphan_totals().await?;
    let root = crate::diagnostics::library_root_path(store).await?;
    // One `statvfs`, off the async runtime: the root is a network share.
    let space = tokio::task::spawn_blocking(move || {
        crate::diagnostics::disk_space(std::path::Path::new(&root))
    })
    .await
    .ok()
    .flatten();
    let sweeps = skadi_library_scan::storage::sweep_statuses()
        .into_iter()
        .map(|s| SweepSummary {
            name: s.name,
            kind: s.kind.to_string(),
            running: s.running,
            started_at: s.started_at.map(|t| t.to_rfc3339()),
            finished_at: s.finished_at.map(|t| t.to_rfc3339()),
            skipped: s.last.as_ref().and_then(|r| r.skipped.clone()),
            last: s.last.map(|r| SweepCounts {
                files: r.files,
                recorded: r.recorded,
                gone: r.gone,
                unreadable: r.unreadable,
                dropped: r.dropped,
                orphans: r.orphans,
                pruned: r.pruned,
            }),
        })
        .collect();
    Ok(Json(StorageSummary {
        kinds,
        orphans: OrphanSummary {
            files: orphan_files,
            size_bytes: orphan_bytes,
        },
        free_bytes: space.map(|(free, _)| free),
        total_bytes: space.map(|(_, total)| total),
        sweeps,
    }))
}

/// Query of `GET /library/storage/files`.
#[derive(Deserialize, Default)]
struct FilesQuery {
    /// `movie`, `series` or `audiobook`. Absent = every kind.
    kind: Option<String>,
    /// `present` (the default), `gone` or `all`.
    state: Option<String>,
    /// `size` (the default), `disk`, `links`, `path` or `mtime`.
    sort: Option<String>,
    /// `desc` or `asc`. The default is `desc`, but `asc` for `path`.
    dir: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn bad(what: &str, got: &str, allowed: &str) -> ApiError {
    ApiError(AppError::Validation(format!(
        "{what} `{got}` is not known; use one of {allowed}"
    )))
}

fn page(limit: Option<i64>, offset: Option<i64>) -> (i64, i64) {
    (
        limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE),
        offset.unwrap_or(0).max(0),
    )
}

impl FilesQuery {
    fn parse(self) -> Result<LibraryFileQuery, ApiError> {
        let sort = match self.sort.as_deref().unwrap_or("size") {
            "size" => LibraryFileSort::Size,
            "disk" => LibraryFileSort::Disk,
            "links" => LibraryFileSort::Links,
            "path" => LibraryFileSort::Path,
            "mtime" => LibraryFileSort::Mtime,
            other => return Err(bad("sort", other, "size, disk, links, path, mtime")),
        };
        let descending = match self.dir.as_deref() {
            None => sort != LibraryFileSort::Path,
            Some("desc") => true,
            Some("asc") => false,
            Some(other) => return Err(bad("dir", other, "asc, desc")),
        };
        let gone = match self.state.as_deref().unwrap_or("present") {
            "present" => Some(false),
            "gone" => Some(true),
            "all" => None,
            other => return Err(bad("state", other, "present, gone, all")),
        };
        let (limit, offset) = page(self.limit, self.offset);
        Ok(LibraryFileQuery {
            kind: self.kind.filter(|k| !k.trim().is_empty()),
            gone,
            sort,
            descending,
            limit,
            offset,
        })
    }
}

/// `GET /library/storage/files`.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageFilesPage {
    /// How many files the filters match in all.
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub files: Vec<StorageFileDto>,
}

/// The storage facts of one library file.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageFileDto {
    pub id: String,
    pub kind: String,
    /// The edition, episode or book file the file belongs to.
    pub acquirable_ref: String,
    pub path: String,
    pub size_bytes: i64,
    /// Space used on disk.
    pub disk_bytes: i64,
    /// Hard link count.
    pub links: i64,
    pub mtime: String,
    /// Extents, when measured. `piecesAt` is `null` while the count is not
    /// current: never measured, or the file changed since.
    pub pieces: Option<i64>,
    pub pieces_at: Option<String>,
    pub checksum: Option<String>,
    pub checksum_at: Option<String>,
    /// When the size and the links were last read.
    pub stat_at: String,
    /// Set when the file was not found on disk.
    pub gone_at: Option<String>,
}

impl From<LibraryFile> for StorageFileDto {
    fn from(f: LibraryFile) -> Self {
        Self {
            id: f.id,
            kind: f.kind,
            acquirable_ref: f.acquirable_ref,
            path: f.path,
            size_bytes: f.size_bytes,
            disk_bytes: f.disk_bytes,
            links: f.links,
            mtime: f.mtime.to_rfc3339(),
            pieces: f.pieces,
            pieces_at: f.pieces_at.map(|t| t.to_rfc3339()),
            checksum: f.checksum,
            checksum_at: f.checksum_at.map(|t| t.to_rfc3339()),
            stat_at: f.stat_at.to_rfc3339(),
            gone_at: f.gone_at.map(|t| t.to_rfc3339()),
        }
    }
}

async fn files(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FilesQuery>,
) -> Result<Json<StorageFilesPage>, ApiError> {
    let query = q.parse()?;
    let (rows, total) = store(&state)?.list_library_files(&query).await?;
    Ok(Json(StorageFilesPage {
        total,
        limit: query.limit,
        offset: query.offset,
        files: rows.into_iter().map(StorageFileDto::from).collect(),
    }))
}

/// Query of `GET /library/storage/orphans`.
#[derive(Deserialize, Default)]
struct OrphansQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

/// `GET /library/storage/orphans`: largest first.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageOrphansPage {
    pub total: i64,
    pub size_bytes: i64,
    pub limit: i64,
    pub offset: i64,
    pub orphans: Vec<StorageOrphanDto>,
}

/// A file under a library folder that no item holds.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageOrphanDto {
    pub path: String,
    pub size_bytes: i64,
    pub mtime: String,
    /// The first and the newest pass that met the file.
    pub first_seen: String,
    pub last_seen: String,
}

impl From<LibraryOrphan> for StorageOrphanDto {
    fn from(o: LibraryOrphan) -> Self {
        Self {
            path: o.path,
            size_bytes: o.size_bytes,
            mtime: o.mtime.to_rfc3339(),
            first_seen: o.first_seen.to_rfc3339(),
            last_seen: o.last_seen.to_rfc3339(),
        }
    }
}

async fn orphans(
    State(state): State<Arc<AppState>>,
    Query(q): Query<OrphansQuery>,
) -> Result<Json<StorageOrphansPage>, ApiError> {
    let store = store(&state)?;
    let (limit, offset) = page(q.limit, q.offset);
    let (total, size_bytes) = store.library_orphan_totals().await?;
    let rows = store.list_library_orphans(limit, offset).await?;
    Ok(Json(StorageOrphansPage {
        total,
        size_bytes,
        limit,
        offset,
        orphans: rows.into_iter().map(StorageOrphanDto::from).collect(),
    }))
}

/// `GET /storage/probe`: the piece probe, with no secret in it.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeDto {
    /// `storage.probe.pieces.enabled`.
    pub enabled: bool,
    /// `off`, `not_set_up`, `failed` or `on`.
    pub state: &'static str,
    /// The settings the probe cannot run without and does not have.
    pub missing: Vec<String>,
    /// Why the probe stopped. Kept after a clear, as a record.
    pub reason: Option<String>,
    pub failed_at: Option<String>,
    pub cleared_at: Option<String>,
    pub cleared_by: Option<String>,
    /// `password` or `key` when a credential is stored; never the secret.
    pub credential: Option<&'static str>,
    /// The storage host's public key, pinned at the first connection.
    pub host_key: Option<String>,
}

async fn probe_dto(store: &Store) -> Result<ProbeDto, ApiError> {
    let o = probe::overview(store).await?;
    let state = if o.failed {
        "failed"
    } else if !o.enabled {
        "off"
    } else if !o.missing.is_empty() {
        "not_set_up"
    } else {
        "on"
    };
    Ok(ProbeDto {
        enabled: o.enabled,
        state,
        missing: o.missing.iter().map(|m| (*m).to_string()).collect(),
        reason: o.reason,
        failed_at: o.failed_at.map(|t| t.to_rfc3339()),
        cleared_at: o.cleared_at.map(|t| t.to_rfc3339()),
        cleared_by: o.cleared_by,
        credential: o.credential,
        host_key: o.host_key,
    })
}

async fn probe_state(State(state): State<Arc<AppState>>) -> Result<Json<ProbeDto>, ApiError> {
    Ok(Json(probe_dto(store(&state)?).await?))
}

/// `POST /storage/probe/clear`: an operator says the cause is fixed. The probe
/// runs again at its next tick. Clearing a probe that has not failed changes
/// nothing.
async fn probe_clear(
    State(state): State<Arc<AppState>>,
    member: Option<axum::Extension<crate::household::Member>>,
) -> Result<Json<ProbeDto>, ApiError> {
    let store = store(&state)?;
    let by = crate::household::member_or_admin(member).name;
    if store
        .clear_storage_probe(probe::PROBE, &by, chrono::Utc::now())
        .await?
    {
        tracing::info!(by, "piece probe: failure cleared");
    }
    Ok(Json(probe_dto(store).await?))
}

/// Body of `PUT /storage/probe/credential`.
#[derive(Deserialize)]
struct CredentialBody {
    /// `password` or `key`.
    kind: String,
    /// The password, or the private key in OpenSSH or PEM form.
    secret: String,
    /// The passphrase of an encrypted key.
    #[serde(default)]
    passphrase: Option<String>,
}

/// `PUT /storage/probe/credential`: store how the probe logs in. The answer is
/// the probe's state; it never carries the secret back.
async fn probe_set_credential(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CredentialBody>,
) -> Result<Json<ProbeDto>, ApiError> {
    let store = store(&state)?;
    if body.secret.trim().is_empty() {
        return Err(ApiError(AppError::Validation("the secret is empty".into())));
    }
    let credential = match body.kind.as_str() {
        "password" => probe::ProbeCredential::Password {
            secret: body.secret,
        },
        "key" => probe::ProbeCredential::Key {
            secret: body.secret,
            passphrase: body.passphrase.filter(|p| !p.is_empty()),
        },
        other => return Err(bad("kind", other, "password, key")),
    };
    probe::save_credential(store, &credential).await?;
    tracing::info!(kind = credential.kind(), "piece probe: credential stored");
    Ok(Json(probe_dto(store).await?))
}

/// `DELETE /storage/probe/credential`.
async fn probe_delete_credential(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ProbeDto>, ApiError> {
    let store = store(&state)?;
    probe::delete_credential(store).await?;
    Ok(Json(probe_dto(store).await?))
}
