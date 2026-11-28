//! The download buffer (SKADI-T-0783, COLLIERY-I-0677).
//!
//! With `SKADI_WORKER_BUFFER_DIR` set, a new download downloads and seeds in a
//! folder under the buffer, and the worker's session state lives there too. When
//! it finishes, the worker copies its files to the job's complete folder on the
//! library volume in one sequential write, and only then reports the job
//! complete. The importer hard-links from the complete folder as before.
//!
//! The copy is always a new file: never a rename or a hard link, also when the
//! buffer is on the same filesystem as the library, since a rename keeps the
//! pieces the torrent wrote. Copies run one at a time.
//!
//! Which jobs are buffer jobs is recorded on the buffer, one small file per job
//! under [`JOBS_DIR`], holding the job's complete folder. The database row is
//! not changed. A job claimed before the buffer was set has no record and
//! finishes where it is.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use skadi_store::DownloadJob;

/// Folder under the buffer that holds one record per buffer job.
pub const JOBS_DIR: &str = ".skadi-jobs";
/// Folder under the buffer that holds librqbit's session state.
pub const SESSION_DIR: &str = ".rqbit-session";
/// Suffix of the temporary file a copy is written to, beside its target.
pub const COPY_SUFFIX: &str = ".skadi-copy";
/// Read and write size of a copy.
const COPY_CHUNK: usize = 8 * 1024 * 1024;

/// One copy at a time, over every job of this worker.
static COPY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Copies running now, and the most seen at once (read by tests).
static COPIES_RUNNING: AtomicUsize = AtomicUsize::new(0);
static COPIES_PEAK: AtomicUsize = AtomicUsize::new(0);

/// The most copies that ever ran at once in this process.
#[must_use]
pub fn copies_peak() -> usize {
    COPIES_PEAK.load(Ordering::SeqCst)
}

/// The buffer folder, and the complete folder a buffer job uses when its row
/// names none (a watched-folder job).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    root: PathBuf,
    fallback_complete: PathBuf,
    limits: Limits,
}

impl Buffer {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, fallback_complete: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fallback_complete: fallback_complete.into(),
            limits: Limits::default(),
        }
    }

    /// The same buffer with `limits`.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The buffer's size cap and free-space floor.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The buffer folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where librqbit's session state lives when a buffer is set.
    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.root.join(SESSION_DIR)
    }

    fn record_path(&self, job_id: &str) -> PathBuf {
        let name: String = job_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root.join(JOBS_DIR).join(name)
    }

    /// The complete folder recorded for a buffer job; `None` for a job that is
    /// not a buffer job.
    #[must_use]
    pub fn recorded(&self, job_id: &str) -> Option<String> {
        std::fs::read_to_string(self.record_path(job_id))
            .ok()
            .map(|s| s.trim_end_matches('\n').to_string())
            .filter(|s| !s.is_empty())
    }

    /// Record `job_id` as a buffer job whose files go to `complete_dir`.
    /// Written under a temporary name and renamed, so a cut write leaves no
    /// record rather than a wrong one.
    pub fn record(&self, job_id: &str, complete_dir: &str) -> std::io::Result<()> {
        let path = self.record_path(job_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, format!("{complete_dir}\n"))?;
        std::fs::rename(&tmp, &path)
    }

    /// Drop the record of a removed job. Best effort.
    pub fn forget(&self, job_id: &str) {
        let _ = std::fs::remove_file(self.record_path(job_id));
        let _ = std::fs::remove_file(self.admitted_path(job_id));
    }

    fn admitted_path(&self, job_id: &str) -> PathBuf {
        let mut p = self.record_path(job_id).into_os_string();
        p.push(".admitted");
        PathBuf::from(p)
    }

    /// Whether a buffer job was given its room (SKADI-T-0784): it counts against
    /// the limits from then on, and is not checked again after a restart.
    #[must_use]
    pub fn is_admitted(&self, job_id: &str) -> bool {
        self.admitted_path(job_id).exists()
    }

    /// Record that a buffer job was given its room.
    pub fn admit(&self, job_id: &str) -> std::io::Result<()> {
        let path = self.admitted_path(job_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, b"")
    }

    /// Point a buffer job's folders at the buffer, in memory: its incomplete
    /// folder becomes the buffer, its complete folder the recorded one. Returns
    /// whether `job` is a buffer job; a job that is not is left as it is.
    pub fn apply(&self, job: &mut DownloadJob) -> bool {
        let Some(complete) = self.recorded(&job.id) else {
            return false;
        };
        job.incomplete_dir = Some(self.root.to_string_lossy().into_owned());
        job.complete_dir = Some(complete);
        true
    }

    /// For a job just claimed: a buffer job stays one; a new job becomes one
    /// (recorded, then [`apply`](Self::apply)); a job that already started on
    /// the library volume stays there. Returns whether `job` is a buffer job.
    pub fn adopt(&self, job: &mut DownloadJob) -> std::io::Result<bool> {
        if self.apply(job) {
            return Ok(true);
        }
        if !is_new_job(job) {
            return Ok(false);
        }
        let complete = job
            .complete_dir
            .clone()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| self.fallback_complete.to_string_lossy().into_owned());
        self.record(&job.id, &complete)?;
        Ok(self.apply(job))
    }
}

/// One gigabyte, as the limits count it (decimal, like disk sizes).
pub const GB: u64 = 1_000_000_000;

/// The buffer's limits (SKADI-T-0784): a size cap on what the buffer jobs hold,
/// and a floor of free space on the buffer's filesystem. The first one reached
/// applies. `None` turns a limit off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub cap_bytes: Option<u64>,
    pub floor_bytes: Option<u64>,
}

/// Default size cap: 100 GB.
pub const DEFAULT_CAP_GB: f64 = 100.0;
/// Default free-space floor: 20 GB.
pub const DEFAULT_FLOOR_GB: f64 = 20.0;

impl Default for Limits {
    fn default() -> Self {
        Self {
            cap_bytes: gb_limit(DEFAULT_CAP_GB),
            floor_bytes: gb_limit(DEFAULT_FLOOR_GB),
        }
    }
}

impl Limits {
    /// No limits at all.
    #[must_use]
    pub fn none() -> Self {
        Self {
            cap_bytes: None,
            floor_bytes: None,
        }
    }

    /// From the values of `SKADI_WORKER_BUFFER_MAX_GB` and
    /// `SKADI_WORKER_BUFFER_MIN_FREE_GB`: unset, empty or unreadable gives the
    /// default; `0` turns the limit off.
    #[must_use]
    pub fn from_settings(cap_gb: Option<&str>, floor_gb: Option<&str>) -> Self {
        let read = |raw: Option<&str>, default: f64| {
            raw.map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(default)
        };
        Self {
            cap_bytes: gb_limit(read(cap_gb, DEFAULT_CAP_GB)),
            floor_bytes: gb_limit(read(floor_gb, DEFAULT_FLOOR_GB)),
        }
    }
}

fn gb_limit(gb: f64) -> Option<u64> {
    (gb > 0.0).then_some((gb * GB as f64) as u64)
}

/// A size for a log line or a reason: `12.3 GB`, or `4.5 MB` under a GB.
#[must_use]
pub fn gb(bytes: u64) -> String {
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

/// One buffer job, as the room check sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub job_id: String,
    /// The torrent's size: what the job holds on the buffer once finished.
    pub size: u64,
    /// What it has still to download.
    pub remaining: u64,
    /// When it started seeding; `Some` only for a seeding copy, the only kind
    /// that may be evicted.
    pub seeding_since: Option<chrono::DateTime<chrono::Utc>>,
}

/// What the buffer holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Sum of the sizes of the buffer jobs.
    pub used: u64,
    /// Bytes the buffer jobs still have to download.
    pub to_come: u64,
    /// Free space on the buffer's filesystem.
    pub free: u64,
    pub seeding: usize,
    pub downloading: usize,
}

/// The buffer jobs among `jobs` (already passed through [`Buffer::apply`]):
/// those that were given room and still have a buffer copy.
#[must_use]
pub fn held_jobs(buffer: &Buffer, jobs: &[DownloadJob]) -> Vec<Held> {
    use skadi_store::DownloadJobStatus as S;
    jobs.iter()
        .filter(|j| !matches!(j.status, S::Seeded | S::Removed | S::BufferTooSmall))
        .filter(|j| buffer.recorded(&j.id).is_some() && buffer.is_admitted(&j.id))
        .map(|j| {
            let size = j.total_bytes.max(0) as u64;
            let done = j.progress_bytes.clamp(0, j.total_bytes.max(0)) as u64;
            let still_downloading = matches!(
                j.status,
                S::Queued | S::Downloading | S::Stalled | S::Paused
            );
            Held {
                job_id: j.id.clone(),
                size,
                remaining: if still_downloading { size - done } else { 0 },
                seeding_since: (j.status == S::Completed)
                    .then(|| j.completed_at.unwrap_or(j.updated_at)),
            }
        })
        .collect()
}

/// Sum up `held` with the free space of the buffer.
#[must_use]
pub fn usage(held: &[Held], free: u64) -> Usage {
    Usage {
        used: held.iter().map(|h| h.size).sum(),
        to_come: held.iter().map(|h| h.remaining).sum(),
        free,
        seeding: held.iter().filter(|h| h.seeding_since.is_some()).count(),
        downloading: held.iter().filter(|h| h.seeding_since.is_none()).count(),
    }
}

/// One line on the buffer's use and limits, for the log.
#[must_use]
pub fn status_line(u: &Usage, limits: Limits) -> String {
    let cap = limits
        .cap_bytes
        .map_or_else(|| "no cap".to_string(), |c| format!("cap {}", gb(c)));
    let floor = limits
        .floor_bytes
        .map_or_else(|| "no floor".to_string(), |f| format!("floor {}", gb(f)));
    format!(
        "buffer holds {} ({cap}), {} still to download, {} free ({floor}); {} seeding, {} downloading",
        gb(u.used),
        gb(u.to_come),
        gb(u.free),
        u.seeding,
        u.downloading
    )
}

/// The room check of a new buffer download (SKADI-T-0784).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Room {
    /// It fits once these seeding copies are evicted (oldest seed first;
    /// empty when it fits as it is).
    Start { evict: Vec<String> },
    /// It does not fit now, but will once the downloads in the buffer finish
    /// and can be evicted. The reason is for the log.
    Wait { reason: String },
    /// It can never fit: bigger than the cap, or than the buffer with every
    /// buffer job gone. The reason says what to change.
    Never { reason: String },
}

/// Decide whether a new download of `size` bytes fits in the buffer. Pure.
///
/// The cap counts the sizes of the buffer jobs; the floor counts the free space
/// left once every buffer download has finished. The first limit reached
/// applies. Only seeding copies are evicted, the oldest seed first; nothing
/// still downloading or being copied.
#[must_use]
pub fn check_room(size: u64, held: &[Held], free: u64, limits: Limits) -> Room {
    let u = usage(held, free);
    let fix = "Raise the cap (SKADI_WORKER_BUFFER_MAX_GB), free space on the buffer, \
               or point SKADI_WORKER_BUFFER_DIR at a larger folder, then resume the download.";
    if let Some(cap) = limits.cap_bytes
        && size > cap
    {
        return Room::Never {
            reason: format!(
                "Too big for the download buffer: the download is {}, the buffer's cap is {}. {fix}",
                gb(size),
                gb(cap)
            ),
        };
    }
    if let Some(floor) = limits.floor_bytes
        && size.saturating_add(floor) > free.saturating_add(u.used)
    {
        return Room::Never {
            reason: format!(
                "Too big for the download buffer: the download is {}, and the buffer would have {} \
                 free with every download in it gone, under its free-space floor of {} \
                 (SKADI_WORKER_BUFFER_MIN_FREE_GB). {fix}",
                gb(size),
                gb(free.saturating_add(u.used).saturating_sub(size)),
                gb(floor)
            ),
        };
    }
    let need_cap = limits
        .cap_bytes
        .map_or(0, |cap| u.used.saturating_add(size).saturating_sub(cap));
    let need_floor = limits.floor_bytes.map_or(0, |floor| {
        floor
            .saturating_add(u.to_come)
            .saturating_add(size)
            .saturating_sub(free)
    });
    let need = need_cap.max(need_floor);
    if need == 0 {
        return Room::Start { evict: Vec::new() };
    }
    let mut seeds: Vec<&Held> = held.iter().filter(|h| h.seeding_since.is_some()).collect();
    seeds.sort_by_key(|h| h.seeding_since);
    let mut freed = 0u64;
    let mut evict = Vec::new();
    for h in seeds {
        if freed >= need {
            break;
        }
        freed = freed.saturating_add(h.size);
        evict.push(h.job_id.clone());
    }
    if freed >= need {
        return Room::Start { evict };
    }
    Room::Wait {
        reason: format!(
            "waiting for room in the download buffer: the download is {}, {} more is needed \
             than the seeding copies free; {}",
            gb(size),
            gb(need - freed),
            status_line(&u, limits)
        ),
    }
}

/// Free space on the filesystem of `path` (`statvfs`), for the floor.
pub fn free_space(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a valid out-pointer.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// Blocking: delete a buffer job's folder `folder` (SKADI-T-0784), and nothing
/// else. Refuses, and deletes nothing, unless `folder` is strictly below
/// `root`, names no `..`, and is a real folder (not a link). Returns whether
/// something was deleted.
pub fn delete_buffer_copy(root: &Path, folder: &Path) -> std::io::Result<bool> {
    use std::path::Component;
    let below = folder.starts_with(root)
        && folder != root
        && folder
            .strip_prefix(root)
            .is_ok_and(|rel| rel.components().all(|c| matches!(c, Component::Normal(_))));
    if !below {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not below the buffer {}; not deleted",
                folder.display(),
                root.display()
            ),
        ));
    }
    match std::fs::symlink_metadata(folder) {
        Ok(m) if m.is_dir() => {
            std::fs::remove_dir_all(folder)?;
            Ok(true)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a folder; not deleted", folder.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The buffer named by a `SKADI_WORKER_BUFFER_DIR` value: `None` when it is
/// unset or empty.
#[must_use]
pub fn buffer_root(raw: Option<&str>) -> Option<PathBuf> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Whether a claimed job has not started yet: no torrent added (no info-hash)
/// and nothing downloaded. Such a job can go to the buffer; any other one may
/// have data on the library volume and finishes there.
#[must_use]
pub fn is_new_job(job: &DownloadJob) -> bool {
    job.info_hash.is_none() && job.progress_bytes <= 0
}

/// Give the buffer's session folder the state of the old one, once: when the
/// buffer has no session yet and the old folder has one, its files are copied
/// across, so the torrents that seed on the library volume are not checked again
/// in full. Returns how many files were copied.
pub fn seed_session_dir(buffer_session: &Path, old_session: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(buffer_session)?;
    if buffer_session == old_session || std::fs::read_dir(buffer_session)?.next().is_some() {
        return Ok(0);
    }
    let Ok(entries) = std::fs::read_dir(old_session) else {
        return Ok(0);
    };
    let mut copied = 0;
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_file() {
            std::fs::copy(e.path(), buffer_session.join(e.file_name()))?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// How a file reached the complete folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    /// Written now.
    Copied,
    /// Already there from an earlier run: a separate file of the same size.
    AlreadyThere,
}

/// The temporary name a copy to `dest` is written under, beside it.
#[must_use]
pub fn temp_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dest.with_file_name(format!(".{name}{COPY_SUFFIX}"))
}

fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Blocking: copy `src` to `dest` as a new file written in one pass.
///
/// The bytes go to a temporary name beside `dest` ([`temp_path`]), are flushed
/// to disk, checked by size, then renamed into place, so `dest` is never a half
/// file. A temporary file left by a cut copy is written again from the start. A
/// `dest` that is already a separate file of the right size is a copy an earlier
/// run finished, and is kept. A `dest` that is the same file as `src` (a hard
/// link) is replaced by a copy.
///
/// This reads and writes by hand on purpose: `std::fs::copy` and `std::io::copy`
/// may clone or reflink the source, which keeps its pieces.
pub fn copy_file(src: &Path, dest: &Path) -> std::io::Result<CopyOutcome> {
    let src_meta = std::fs::metadata(src)?;
    let len = src_meta.len();
    if let Ok(m) = std::fs::symlink_metadata(dest)
        && m.is_file()
        && m.len() == len
        && !same_file(&m, &src_meta)
    {
        return Ok(CopyOutcome::AlreadyThere);
    }
    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no folder")
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = temp_path(dest);
    let result = (|| {
        let mut input = std::fs::File::open(src)?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        let mut buf = vec![0u8; COPY_CHUNK];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
        out.sync_all()?;
        drop(out);
        let written = std::fs::metadata(&tmp)?.len();
        if written != len {
            return Err(std::io::Error::other(format!(
                "copy of {} is {written} bytes, the source is {len}",
                src.display()
            )));
        }
        std::fs::rename(&tmp, dest)?;
        // Make the rename durable; not every filesystem allows a folder fsync.
        if let Ok(d) = std::fs::File::open(parent) {
            let _ = d.sync_all();
        }
        Ok(CopyOutcome::Copied)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Blocking: copy each finished file of a buffer job from under `buffer_root`
/// to the same path under `complete_dir`, and return the paths to report. Stops
/// at the first failure, which names the file; the buffer copies stay.
pub fn copy_into_complete(
    files: &[String],
    buffer_root: &str,
    complete_dir: &str,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::with_capacity(files.len());
    for abs in files {
        let dest = crate::map_to_complete(abs, buffer_root, complete_dir)
            .ok_or_else(|| anyhow::anyhow!("{abs} is not under the buffer {buffer_root}"))?;
        copy_file(Path::new(abs), Path::new(&dest))
            .map_err(|e| anyhow::anyhow!("copying {abs} to {dest}: {e}"))?;
        out.push(dest);
    }
    Ok(out)
}

/// Copy a finished buffer job's files to the library volume
/// ([`copy_into_complete`]), one job at a time over the whole worker.
pub async fn copy_to_library(
    files: Vec<String>,
    buffer_root: String,
    complete_dir: String,
) -> anyhow::Result<Vec<String>> {
    let _turn = COPY_LOCK.lock().await;
    tokio::task::spawn_blocking(move || {
        let now = COPIES_RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
        COPIES_PEAK.fetch_max(now, Ordering::SeqCst);
        let r = copy_into_complete(&files, &buffer_root, &complete_dir);
        COPIES_RUNNING.fetch_sub(1, Ordering::SeqCst);
        r
    })
    .await
    .map_err(|e| anyhow::anyhow!("copy task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn tmp(tag: &str) -> PathBuf {
        let p = skadi_core::unique_temp_path(tag);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn job(id: &str) -> DownloadJob {
        DownloadJob {
            id: id.into(),
            acquirable_ref: "Some Show S01".into(),
            source: "magnet:?xt=urn:btih:0000000000000000000000000000000000000000".into(),
            category: None,
            status: skadi_store::DownloadJobStatus::Downloading,
            info_hash: None,
            progress_bytes: 0,
            total_bytes: 0,
            down_speed_bps: None,
            up_speed_bps: None,
            uploaded_bytes: None,
            peers: None,
            peers_seen: None,
            eta_seconds: None,
            files: Vec::new(),
            error: None,
            worker_id: Some("w".into()),
            delete_data: false,
            incomplete_dir: Some("/lib/downloads/incomplete".into()),
            complete_dir: Some("/lib/downloads/complete".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            completed_at: None,
            lease_expires_at: None,
            stalled_at: None,
            client_state: None,
            target_kind: None,
            target_ref: None,
            import_state: None,
            import_error: None,
            priority: 0,
        }
    }

    #[test]
    fn an_empty_setting_is_no_buffer() {
        assert_eq!(buffer_root(None), None);
        assert_eq!(buffer_root(Some("")), None);
        assert_eq!(buffer_root(Some("  ")), None);
        assert_eq!(buffer_root(Some("/buffer")), Some(PathBuf::from("/buffer")));
    }

    #[test]
    fn a_new_job_moves_to_the_buffer_and_stays_there() {
        let root = tmp("buf-adopt");
        let b = Buffer::new(&root, "/lib/downloads/complete");
        let mut j = job("job-1");
        assert!(b.adopt(&mut j).unwrap());
        assert_eq!(
            j.incomplete_dir.as_deref(),
            Some(root.to_string_lossy().as_ref())
        );
        assert_eq!(j.complete_dir.as_deref(), Some("/lib/downloads/complete"));

        // After a restart the row has an info-hash and progress; the record
        // keeps it in the buffer.
        let mut again = job("job-1");
        again.info_hash = Some("ab".repeat(20));
        again.progress_bytes = 10;
        assert!(b.adopt(&mut again).unwrap());
        assert_eq!(again.incomplete_dir, j.incomplete_dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_job_that_started_on_the_library_volume_stays_there() {
        let root = tmp("buf-old");
        let b = Buffer::new(&root, "/lib/downloads/complete");
        let mut j = job("job-old");
        j.info_hash = Some("cd".repeat(20));
        j.progress_bytes = 1000;
        assert!(!b.adopt(&mut j).unwrap());
        assert_eq!(
            j.incomplete_dir.as_deref(),
            Some("/lib/downloads/incomplete")
        );
        assert!(b.recorded("job-old").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_watched_folder_job_copies_to_the_download_folder() {
        let root = tmp("buf-watch");
        let b = Buffer::new(&root, "/lib/downloads/complete");
        let mut j = job("job-watch");
        j.complete_dir = None;
        assert!(b.adopt(&mut j).unwrap());
        assert_eq!(j.complete_dir.as_deref(), Some("/lib/downloads/complete"));
        b.forget("job-watch");
        assert!(b.recorded("job-watch").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_copy_is_a_new_file_never_a_link() {
        let base = tmp("buf-copy");
        let src = base.join("buffer/Show/ep.mkv");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        // Same filesystem as the source: still a copy.
        let dest = base.join("lib/complete/Show/ep.mkv");
        assert_eq!(copy_file(&src, &dest).unwrap(), CopyOutcome::Copied);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let (s, d) = (
            std::fs::metadata(&src).unwrap(),
            std::fs::metadata(&dest).unwrap(),
        );
        assert_ne!(s.ino(), d.ino(), "a new file, not a hard link");
        assert_eq!(d.nlink(), 1);
        assert!(!temp_path(&dest).exists(), "no temporary file is left");
        // The source stays, for seeding.
        assert!(src.exists());
        // A second run keeps the finished copy.
        assert_eq!(copy_file(&src, &dest).unwrap(), CopyOutcome::AlreadyThere);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_hard_link_at_the_target_is_replaced_by_a_copy() {
        let base = tmp("buf-link");
        let src = base.join("src.bin");
        std::fs::write(&src, b"0123456789").unwrap();
        let dest = base.join("out/src.bin");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::hard_link(&src, &dest).unwrap();
        assert_eq!(copy_file(&src, &dest).unwrap(), CopyOutcome::Copied);
        assert_ne!(
            std::fs::metadata(&src).unwrap().ino(),
            std::fs::metadata(&dest).unwrap().ino()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A worker restart in the middle of a copy: the cut temporary file, or a
    /// short file at the target, is written again in full.
    #[test]
    fn a_cut_copy_is_written_again() {
        let base = tmp("buf-cut");
        let src = base.join("src.bin");
        let data = vec![7u8; 100_000];
        std::fs::write(&src, &data).unwrap();
        let dest = base.join("out/src.bin");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(temp_path(&dest), &data[..400]).unwrap();
        assert_eq!(copy_file(&src, &dest).unwrap(), CopyOutcome::Copied);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!temp_path(&dest).exists());

        std::fs::write(&dest, &data[..10]).unwrap();
        assert_eq!(copy_file(&src, &dest).unwrap(), CopyOutcome::Copied);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_failed_copy_reports_the_reason_and_keeps_the_buffer_copy() {
        let base = tmp("buf-fail");
        let buffer = base.join("buffer");
        let src = buffer.join("Show/ep.mkv");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"data").unwrap();
        let complete = base.join("complete");
        std::fs::create_dir_all(&complete).unwrap();
        // A file where the target's folder should be: the copy cannot be made.
        std::fs::write(complete.join("Show"), b"in the way").unwrap();
        let err = copy_into_complete(
            &[src.to_string_lossy().into_owned()],
            &buffer.to_string_lossy(),
            &complete.to_string_lossy(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("ep.mkv"), "{err}");
        assert!(src.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_into_complete_mirrors_paths_below_the_buffer() {
        let base = tmp("buf-mirror");
        let buffer = base.join("buffer");
        let a = buffer.join("Show S01/a.mkv");
        let b = buffer.join("Show S01/Subs/b.srt");
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bb").unwrap();
        let complete = base.join("complete");
        let got = copy_into_complete(
            &[
                a.to_string_lossy().into_owned(),
                b.to_string_lossy().into_owned(),
            ],
            &buffer.to_string_lossy(),
            &complete.to_string_lossy(),
        )
        .unwrap();
        assert_eq!(
            got,
            vec![
                complete
                    .join("Show S01/a.mkv")
                    .to_string_lossy()
                    .into_owned(),
                complete
                    .join("Show S01/Subs/b.srt")
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
        assert_eq!(
            std::fs::read(complete.join("Show S01/Subs/b.srt")).unwrap(),
            b"bb"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn copies_run_one_at_a_time() {
        let base = tmp("buf-serial");
        let buffer = base.join("buffer");
        std::fs::create_dir_all(&buffer).unwrap();
        let mut tasks = Vec::new();
        for i in 0..4 {
            let f = buffer.join(format!("job{i}/file.bin"));
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, vec![i as u8; 4_000_000]).unwrap();
            tasks.push(tokio::spawn(copy_to_library(
                vec![f.to_string_lossy().into_owned()],
                buffer.to_string_lossy().into_owned(),
                base.join("complete").to_string_lossy().into_owned(),
            )));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        assert_eq!(copies_peak(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_buffer_session_starts_from_the_old_one_once() {
        let base = tmp("buf-session");
        let old = base.join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("session.json"), b"{}").unwrap();
        std::fs::write(old.join("abc.bitv"), b"\x01").unwrap();
        let b = Buffer::new(base.join("buffer"), "/c");
        assert_eq!(seed_session_dir(&b.session_dir(), &old).unwrap(), 2);
        assert!(b.session_dir().join("session.json").exists());
        // Not again: the buffer's own state wins from then on.
        std::fs::write(old.join("session.json"), b"{\"newer\":1}").unwrap();
        assert_eq!(seed_session_dir(&b.session_dir(), &old).unwrap(), 0);
        assert_eq!(
            std::fs::read(b.session_dir().join("session.json")).unwrap(),
            b"{}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- Limits and eviction (SKADI-T-0784) ----

    fn seed(id: &str, size: u64, mins_ago: i64) -> Held {
        Held {
            job_id: id.into(),
            size,
            remaining: 0,
            seeding_since: Some(chrono::Utc::now() - chrono::Duration::minutes(mins_ago)),
        }
    }

    fn downloading(id: &str, size: u64, remaining: u64) -> Held {
        Held {
            job_id: id.into(),
            size,
            remaining,
            seeding_since: None,
        }
    }

    fn lim(cap_gb: Option<u64>, floor_gb: Option<u64>) -> Limits {
        Limits {
            cap_bytes: cap_gb.map(|g| g * GB),
            floor_bytes: floor_gb.map(|g| g * GB),
        }
    }

    #[test]
    fn the_limits_default_to_100_and_20_gb_and_0_turns_one_off() {
        let d = Limits::from_settings(None, None);
        assert_eq!(d.cap_bytes, Some(100 * GB));
        assert_eq!(d.floor_bytes, Some(20 * GB));
        assert_eq!(d, Limits::default());
        let set = Limits::from_settings(Some("250"), Some("0"));
        assert_eq!(set.cap_bytes, Some(250 * GB));
        assert_eq!(set.floor_bytes, None);
        let junk = Limits::from_settings(Some("lots"), Some(" "));
        assert_eq!(junk, Limits::default());
        assert_eq!(
            Limits::from_settings(Some("1.5"), None).cap_bytes,
            Some(1_500_000_000)
        );
    }

    #[test]
    fn a_download_that_fits_starts_without_evicting() {
        let held = [seed("a", 10 * GB, 60)];
        assert_eq!(
            check_room(20 * GB, &held, 500 * GB, lim(Some(100), Some(20))),
            Room::Start { evict: vec![] }
        );
    }

    #[test]
    fn the_cap_evicts_the_oldest_seeds_first_and_never_a_download() {
        let held = [
            seed("newer", 30 * GB, 10),
            downloading("busy", 40 * GB, 5 * GB),
            seed("oldest", 20 * GB, 120),
        ];
        // 90 held; 25 more needs 15 freed under the 100 cap: the oldest seed.
        assert_eq!(
            check_room(25 * GB, &held, 500 * GB, lim(Some(100), None)),
            Room::Start {
                evict: vec!["oldest".into()]
            }
        );
        // 45 more needs 35: both seeds, oldest first; the download stays.
        assert_eq!(
            check_room(45 * GB, &held, 500 * GB, lim(Some(100), None)),
            Room::Start {
                evict: vec!["oldest".into(), "newer".into()]
            }
        );
        // 60 more needs 50, the seeds free 50 — fits; 61 must wait for the
        // download to finish and become a seed.
        match check_room(61 * GB, &held, 500 * GB, lim(Some(100), None)) {
            Room::Wait { reason } => assert!(reason.contains("61.0 GB"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_floor_counts_what_downloads_still_need() {
        // 50 free, one download still needs 25, floor 20: 5 left for new data.
        let held = [
            downloading("busy", 30 * GB, 25 * GB),
            seed("old", 10 * GB, 30),
        ];
        assert_eq!(
            check_room(5 * GB, &held, 50 * GB, lim(None, Some(20))),
            Room::Start { evict: vec![] }
        );
        assert_eq!(
            check_room(12 * GB, &held, 50 * GB, lim(None, Some(20))),
            Room::Start {
                evict: vec!["old".into()]
            }
        );
        assert!(matches!(
            check_room(20 * GB, &held, 50 * GB, lim(None, Some(20))),
            Room::Wait { .. }
        ));
    }

    #[test]
    fn the_first_limit_reached_applies() {
        let held = [seed("old", 10 * GB, 30)];
        // Cap 100 is far; the floor bites: 25 free, floor 20, 10 wanted.
        assert_eq!(
            check_room(10 * GB, &held, 25 * GB, lim(Some(100), Some(20))),
            Room::Start {
                evict: vec!["old".into()]
            }
        );
        // Plenty free; the cap bites: 10 held, cap 15, 10 wanted.
        assert_eq!(
            check_room(10 * GB, &held, 500 * GB, lim(Some(15), Some(20))),
            Room::Start {
                evict: vec!["old".into()]
            }
        );
    }

    #[test]
    fn a_download_that_can_never_fit_says_what_to_change() {
        match check_room(150 * GB, &[], 500 * GB, lim(Some(100), Some(20))) {
            Room::Never { reason } => {
                assert!(reason.contains("150.0 GB"), "{reason}");
                assert!(reason.contains("100.0 GB"), "{reason}");
                assert!(reason.contains("SKADI_WORKER_BUFFER_MAX_GB"), "{reason}");
                assert!(reason.contains("resume"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        // Bigger than the buffer with everything in it gone, under the floor.
        let held = [seed("old", 10 * GB, 30)];
        match check_room(40 * GB, &held, 40 * GB, lim(None, Some(20))) {
            Room::Never { reason } => {
                assert!(
                    reason.contains("SKADI_WORKER_BUFFER_MIN_FREE_GB"),
                    "{reason}"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn held_jobs_are_the_admitted_buffer_jobs_with_a_copy() {
        use skadi_store::DownloadJobStatus as S;
        let root = tmp("buf-held");
        let b = Buffer::new(&root, "/c");
        let mut seeding = job("seeding");
        seeding.status = S::Completed;
        seeding.total_bytes = 100;
        seeding.progress_bytes = 100;
        seeding.completed_at = Some(chrono::Utc::now());
        let mut busy = job("busy");
        busy.total_bytes = 100;
        busy.progress_bytes = 40;
        let mut gone = job("gone");
        gone.status = S::Seeded;
        gone.total_bytes = 100;
        let waiting = job("waiting"); // recorded, not admitted
        let library = job("library"); // not a buffer job
        for j in [&seeding, &busy, &gone, &waiting] {
            b.record(&j.id, "/c").unwrap();
        }
        for j in [&seeding, &busy, &gone] {
            b.admit(&j.id).unwrap();
        }
        let held = held_jobs(&b, &[seeding, busy, gone, waiting, library]);
        assert_eq!(held.len(), 2, "{held:?}");
        let u = usage(&held, 1000);
        assert_eq!(
            (u.used, u.to_come, u.seeding, u.downloading),
            (200, 60, 1, 1)
        );
        assert!(status_line(&u, Limits::default()).contains("cap 100.0 GB"));
        // A removed job's records go.
        b.forget("busy");
        assert!(!b.is_admitted("busy"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_buffer_copy_is_deleted_only_below_the_buffer() {
        let base = tmp("buf-delete");
        let root = base.join("buffer");
        let library = base.join("library/complete/Show");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("ep.mkv"), b"library copy").unwrap();
        let copy = root.join("Show");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(copy.join("ep.mkv"), b"buffer copy").unwrap();

        // Outside the buffer, the buffer itself, and `..` are refused.
        assert!(delete_buffer_copy(&root, &library).is_err());
        assert!(delete_buffer_copy(&root, &root).is_err());
        assert!(delete_buffer_copy(&root, &root.join("../library/complete/Show")).is_err());
        assert!(library.join("ep.mkv").exists(), "the library copy stays");

        assert!(delete_buffer_copy(&root, &copy).unwrap());
        assert!(!copy.exists());
        assert!(root.exists());
        assert_eq!(
            std::fs::read(library.join("ep.mkv")).unwrap(),
            b"library copy"
        );
        // Already gone is not an error.
        assert!(!delete_buffer_copy(&root, &copy).unwrap());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn free_space_reads_the_filesystem() {
        let root = tmp("buf-free");
        assert!(free_space(&root).unwrap() > 0);
        assert!(free_space(&root.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
