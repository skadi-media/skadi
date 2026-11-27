//! Storage facts of the library's files, read through the mount (SKADI-T-0773,
//! design SKADI-D-0040).
//!
//! One `stat` per file gives its size, the space it takes on disk, its link
//! count, its identity and its modification time. That is written to
//! `library_files` twice over:
//!
//! * **at import**, by [`record_file`], so a new file has its row at once;
//! * **by a sweep**, [`StorageSweepWorker`], that reads every file the domain
//!   holds again, marks the ones that are gone, and walks the domain's folder
//!   for files no record points at.
//!
//! A [`StorageSource`] per domain says which files the domain holds. The sweep
//! shares the manners of the media scan in this crate: one file at a time, off
//! the async runtime, with a time limit on every read, because the library is
//! a network share that is also serving streams.
//!
//! The sweep never changes a file. It also never concludes anything from a
//! library that is not there: a folder that is missing or empty is skipped,
//! and nothing is marked gone.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use skadi_core::{MediaKind, Result};
use skadi_store::{ConfigRepo, LibraryFileRepo, LibraryFileStat, Store};
use tokio_util::sync::CancellationToken;

/// The config key: hours between two sweeps of a domain. 0 turns the sweep off.
pub const SWEEP_INTERVAL_KEY: &str = "storage.sweep.interval_hours";

/// The config key: a file's history keeps every sample for this many days;
/// older ones are thinned to one a day. 0 keeps every sample.
pub const SAMPLES_FULL_DAYS_KEY: &str = "storage.samples.full_days";

/// The file an operator puts in the library root to prove the mount is up
/// (`library.require_root_marker`). The same name the importer checks.
const ROOT_MARKER: &str = ".skadi-root";

/// Longest one `stat` or one folder walk step may take before the sweep moves
/// on. A `hard` NFS mount retries for ever; the sweep must not.
const STAT_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest the walk of a domain's folder may take.
const WALK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A pause after this many files, so the sweep is a trickle on the share.
const PAUSE_EVERY: usize = 64;
const PAUSE: Duration = Duration::from_millis(250);

/// The wait before the first sweep after the daemon starts.
const START_DELAY: Duration = Duration::from_secs(5 * 60);

/// How often a sweep that is turned off looks at its setting again.
const OFF_RECHECK: Duration = Duration::from_secs(60 * 60);

/// A file changed less than this long ago is not an orphan yet: an import or a
/// conversion may still be writing it.
const SETTLE: Duration = Duration::from_secs(60 * 60);

/// A file with no record is an orphan when it is media, or at least this big.
/// Covers, subtitles and metadata files beside a film are not orphans.
const ORPHAN_MIN_BYTES: u64 = 50 * 1024 * 1024;

const MEDIA_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "webm", "avi", "ts", "wmv", "mpg", "mpeg", "m4b", "m4a", "mp3",
    "flac", "ogg", "opus", "aac", "wav",
];

/// One file a domain's records hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageItem {
    /// The edition, episode or book file the file belongs to.
    pub acquirable_ref: String,
    pub path: PathBuf,
}

/// A domain that owns library files.
#[async_trait]
pub trait StorageSource: Send + Sync {
    /// What this source is called, for the worker's name and the logs.
    fn name(&self) -> &'static str;

    fn kind(&self) -> MediaKind;

    /// Every file the domain's records hold, whether it is on disk or not. A
    /// book of many files gives one item per file.
    async fn files(&self) -> Result<Vec<StorageItem>>;
}

/// The word `library_files.kind` holds for a media kind: the one the history
/// and the tags use.
#[must_use]
pub fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
        MediaKind::Audiobook => "audiobook",
        MediaKind::Music => "music",
        MediaKind::Book => "book",
        MediaKind::Subtitle => "subtitle",
    }
}

/// What one `stat` gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFacts {
    pub dev: u64,
    pub ino: u64,
    pub size_bytes: u64,
    pub disk_bytes: u64,
    pub links: u64,
    pub mtime: SystemTime,
}

/// `stat` one file. Blocking: call it off the async runtime.
pub fn stat_file(path: &Path) -> std::io::Result<FileFacts> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        if !m.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "not a regular file",
            ));
        }
        Ok(FileFacts {
            dev: m.dev(),
            ino: m.ino(),
            size_bytes: m.len(),
            // `st_blocks` counts 512-byte units whatever the block size is.
            disk_bytes: m.blocks() * 512,
            links: m.nlink(),
            mtime: m.modified()?,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "storage facts need a unix filesystem",
        ))
    }
}

/// How one file's `stat` ended.
#[derive(Debug)]
enum StatOutcome {
    Facts(FileFacts),
    /// The file is not there.
    Missing,
    /// It could not be read, or the read took too long. Says nothing about
    /// whether the file is there.
    Unreadable(String),
}

async fn stat_bounded(path: PathBuf, limit: Duration) -> StatOutcome {
    let job = tokio::task::spawn_blocking(move || stat_file(&path));
    match tokio::time::timeout(limit, job).await {
        Ok(Ok(Ok(facts))) => StatOutcome::Facts(facts),
        Ok(Ok(Err(e))) if e.kind() == std::io::ErrorKind::NotFound => StatOutcome::Missing,
        Ok(Ok(Err(e))) => StatOutcome::Unreadable(e.to_string()),
        Ok(Err(e)) => StatOutcome::Unreadable(e.to_string()),
        Err(_) => StatOutcome::Unreadable(format!("no answer in {} s", limit.as_secs())),
    }
}

fn to_stat(kind: &str, acquirable_ref: &str, path: &Path, f: FileFacts) -> LibraryFileStat {
    // The store keeps signed 64-bit numbers. A device or inode number above
    // i64::MAX wraps; it is only ever compared for equality.
    LibraryFileStat {
        kind: kind.to_string(),
        acquirable_ref: acquirable_ref.to_string(),
        path: path.to_string_lossy().into_owned(),
        dev: f.dev as i64,
        ino: f.ino as i64,
        size_bytes: i64::try_from(f.size_bytes).unwrap_or(i64::MAX),
        disk_bytes: i64::try_from(f.disk_bytes).unwrap_or(i64::MAX),
        links: i64::try_from(f.links).unwrap_or(i64::MAX),
        mtime: DateTime::<Utc>::from(f.mtime),
    }
}

/// Write the storage facts of one file an import just placed. Best-effort: an
/// import never fails because its storage row could not be written, and the
/// sweep finds the file later. Returns whether a row was written.
pub async fn record_file(
    repo: &dyn LibraryFileRepo,
    kind: MediaKind,
    acquirable_ref: &str,
    path: &Path,
) -> bool {
    let kind = kind_name(kind);
    match stat_bounded(path.to_path_buf(), STAT_TIMEOUT).await {
        StatOutcome::Facts(facts) => {
            let stat = to_stat(kind, acquirable_ref, path, facts);
            match repo.record_library_file_stat(&stat, Utc::now()).await {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!(?path, error = %e, "storage facts: could not store");
                    false
                }
            }
        }
        StatOutcome::Missing => {
            tracing::debug!(?path, "storage facts: imported file not found");
            false
        }
        StatOutcome::Unreadable(why) => {
            tracing::debug!(?path, why, "storage facts: imported file not read");
            false
        }
    }
}

/// Is the library there? `folder` must be a folder with something in it, and
/// `marker`, when the operator asked for one, must exist. Blocking.
fn folder_ready(folder: &Path, marker: Option<&Path>) -> std::result::Result<(), String> {
    if let Some(marker) = marker
        && !marker.exists()
    {
        return Err(format!("the marker {} is not there", marker.display()));
    }
    let mut entries =
        std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    match entries.next() {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => Err(format!("{}: {e}", folder.display())),
        None => Err(format!("{} is empty", folder.display())),
    }
}

fn orphan_worthy(path: &Path, size: u64) -> bool {
    size >= ORPHAN_MIN_BYTES
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| MEDIA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// What a walk of a domain's folder found.
struct Walk {
    /// Files no record holds: path, size, modification time.
    unknown: Vec<(PathBuf, u64, SystemTime)>,
    /// Every folder was read. When false, the absence of a file proves nothing.
    complete: bool,
}

/// Walk `root` for files that are not in `known`. Blocking.
fn walk_unknown(root: &Path, known: &HashSet<PathBuf>, now: SystemTime) -> std::io::Result<Walk> {
    let mut walk = Walk {
        unknown: Vec::new(),
        complete: true,
    };
    let mut stack = vec![root.to_path_buf()];
    let mut first = true;
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // The root itself must read; a folder below it that does not is
            // remembered, and the walk goes on.
            Err(e) if first => return Err(e),
            Err(e) => {
                tracing::debug!(?dir, error = %e, "storage sweep: folder not read");
                walk.complete = false;
                continue;
            }
        };
        first = false;
        for entry in entries {
            let Ok(entry) = entry else {
                walk.complete = false;
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                walk.complete = false;
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                // `@eaDir` and its kind: folders the storage host keeps for
                // itself.
                if !entry.file_name().to_string_lossy().starts_with('@') {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() || known.contains(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                walk.complete = false;
                continue;
            };
            let mtime = meta.modified().unwrap_or(now);
            let settled = now.duration_since(mtime).is_ok_and(|age| age >= SETTLE);
            if settled && orphan_worthy(&path, meta.len()) {
                walk.unknown.push((path, meta.len(), mtime));
            }
        }
    }
    Ok(walk)
}

/// What one sweep did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Why the sweep did nothing, or stopped part way. `None` = it ran to the
    /// end.
    pub skipped: Option<String>,
    /// Files the domain's records hold.
    pub files: usize,
    /// Rows written from a `stat`.
    pub recorded: usize,
    /// Files with a row that were not on disk.
    pub gone: usize,
    /// Files that could not be read; their rows were left alone.
    pub unreadable: usize,
    /// Rows removed because no record holds the file any more.
    pub dropped: usize,
    /// Files on disk with no record.
    pub orphans: usize,
    /// Orphan rows removed because the file is gone from disk.
    pub pruned: usize,
}

/// Where one domain's sweep is: for the "Library details" page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepStatus {
    /// The worker's name, e.g. `movies-storage-sweep`.
    pub name: String,
    /// The kind it sweeps, as `library_files.kind` holds it.
    pub kind: &'static str,
    /// A pass is running now.
    pub running: bool,
    /// When the pass that runs now, or the last one, began.
    pub started_at: Option<DateTime<Utc>>,
    /// When the last pass ended. `None` = no pass has ended since the daemon
    /// started.
    pub finished_at: Option<DateTime<Utc>>,
    /// What the last pass that ended did.
    pub last: Option<SweepReport>,
}

/// The sweeps of this process, by worker name. In memory: it says what runs
/// now, which no table can, and it starts empty with the daemon. The time a
/// kind was last read survives a restart in `library_files.stat_at`.
static SWEEPS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, SweepStatus>>,
> = std::sync::LazyLock::new(Default::default);

fn sweeps() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<String, SweepStatus>> {
    // A panic while the lock was held leaves the map usable: it holds plain
    // values, each written whole.
    SWEEPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Every sweep this process has started, in name order.
#[must_use]
pub fn sweep_statuses() -> Vec<SweepStatus> {
    sweeps().values().cloned().collect()
}

/// Reads the storage facts of every file a domain holds, on a schedule.
pub struct StorageSweepWorker {
    source: Arc<dyn StorageSource>,
    store: Store,
    name: String,
    /// The domain's folder, when a test sets one; else it comes from
    /// `library.root`.
    root: Option<PathBuf>,
    start_delay: Duration,
    pause: Duration,
    stat_timeout: Duration,
}

impl StorageSweepWorker {
    #[must_use]
    pub fn new(source: Arc<dyn StorageSource>, store: Store) -> Self {
        Self {
            name: format!("{}-storage-sweep", source.name()),
            source,
            store,
            root: None,
            start_delay: START_DELAY,
            pause: PAUSE,
            stat_timeout: STAT_TIMEOUT,
        }
    }

    /// Sweep this folder, whatever `library.root` says. Tests only.
    #[must_use]
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Shorten the waits. Tests only.
    #[must_use]
    pub fn with_pauses(mut self, start_delay: Duration, pause: Duration) -> Self {
        self.start_delay = start_delay;
        self.pause = pause;
        self
    }

    async fn config(&self, key: &str) -> Option<String> {
        let stored = self.store.get_config(key).await.ok().flatten();
        stored
            .map(|e| e.value)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| skadi_config::default_for(key).ok().map(str::to_string))
    }

    /// The domain's folder and, when the operator asked for one, the marker
    /// that proves the mount is up.
    async fn folder(&self) -> (PathBuf, Option<PathBuf>) {
        if let Some(root) = &self.root {
            return (root.clone(), None);
        }
        let library: PathBuf = self
            .config("library.root")
            .await
            .unwrap_or_else(|| "/data".into())
            .into();
        let marker = self
            .config("library.require_root_marker")
            .await
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
            .then(|| library.join(ROOT_MARKER));
        (library.join(self.source.kind().library_subfolder()), marker)
    }

    async fn ready(&self, folder: &Path, marker: Option<&Path>) -> std::result::Result<(), String> {
        let (folder, marker) = (folder.to_path_buf(), marker.map(Path::to_path_buf));
        let job = tokio::task::spawn_blocking(move || folder_ready(&folder, marker.as_deref()));
        match tokio::time::timeout(self.stat_timeout, job).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the library did not answer".into()),
        }
    }

    /// Hours between sweeps; `None` = the sweep is off.
    async fn interval(&self) -> Option<Duration> {
        let hours: u64 = self
            .config(SWEEP_INTERVAL_KEY)
            .await
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(24);
        (hours > 0).then(|| Duration::from_secs(hours * 3600))
    }

    /// Make the sweep known to [`sweep_statuses`] before its first pass.
    fn announce(&self) {
        sweeps()
            .entry(self.name.clone())
            .or_insert_with(|| SweepStatus {
                name: self.name.clone(),
                kind: kind_name(self.source.kind()),
                ..SweepStatus::default()
            });
    }

    /// One pass over the domain. Never fails: what went wrong is in the
    /// report and the log.
    pub async fn sweep_once(&self, cancel: &CancellationToken) -> SweepReport {
        self.announce();
        if let Some(status) = sweeps().get_mut(&self.name) {
            status.running = true;
            status.started_at = Some(Utc::now());
        }
        let report = self.pass(cancel).await;
        if report.skipped.is_none() {
            self.keep_history().await;
        }
        if let Some(status) = sweeps().get_mut(&self.name) {
            status.running = false;
            status.finished_at = Some(Utc::now());
            status.last = Some(report.clone());
        }
        report
    }

    /// After a pass that ran to the end: write today's totals for the trend,
    /// and thin the old per-file history. Best-effort; the next pass tries
    /// again.
    async fn keep_history(&self) {
        let now = Utc::now();
        let day = now.format("%Y-%m-%d").to_string();
        if let Err(e) = self.store.record_library_daily_totals(&day, now).await {
            tracing::warn!(error = %e, "storage sweep: daily totals not written");
        }
        let full_days: i64 = self
            .config(SAMPLES_FULL_DAYS_KEY)
            .await
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(30);
        if full_days > 0 {
            let before = now - chrono::Duration::days(full_days);
            match self.store.thin_library_file_samples(before).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "storage sweep: old history thinned"),
                Err(e) => tracing::warn!(error = %e, "storage sweep: history not thinned"),
            }
        }
    }

    async fn pass(&self, cancel: &CancellationToken) -> SweepReport {
        let mut report = SweepReport::default();
        let kind = kind_name(self.source.kind());
        let (folder, marker) = self.folder().await;
        if let Err(why) = self.ready(&folder, marker.as_deref()).await {
            report.skipped = Some(why);
            return report;
        }
        let started = Utc::now();
        let items = match self.source.files().await {
            Ok(items) => items,
            Err(e) => {
                report.skipped = Some(format!("could not list the records: {e}"));
                return report;
            }
        };
        report.files = items.len();

        // 1. Every file the records hold: read it, or mark it gone.
        for (n, item) in items.iter().enumerate() {
            if cancel.is_cancelled() {
                report.skipped = Some("stopped".into());
                return report;
            }
            if n > 0 && n % PAUSE_EVERY == 0 {
                tokio::time::sleep(self.pause).await;
            }
            match stat_bounded(item.path.clone(), self.stat_timeout).await {
                StatOutcome::Facts(facts) => {
                    let stat = to_stat(kind, &item.acquirable_ref, &item.path, facts);
                    match self.store.record_library_file_stat(&stat, Utc::now()).await {
                        Ok(_) => report.recorded += 1,
                        Err(e) => {
                            tracing::warn!(path = ?item.path, error = %e,
                                "storage sweep: could not store");
                            report.unreadable += 1;
                        }
                    }
                }
                StatOutcome::Missing => {
                    // A missing file is only news while the library is there.
                    // If the mount dropped under the sweep, every file is
                    // "missing": stop, and mark nothing more.
                    if let Err(why) = self.ready(&folder, marker.as_deref()).await {
                        report.skipped = Some(format!("the library went away: {why}"));
                        return report;
                    }
                    let path = item.path.to_string_lossy();
                    match self.store.library_file_by_path(&path).await {
                        Ok(Some(row)) => {
                            if let Err(e) =
                                self.store.mark_library_file_gone(&row.id, Utc::now()).await
                            {
                                tracing::warn!(path = ?item.path, error = %e,
                                    "storage sweep: could not mark gone");
                            }
                            report.gone += 1;
                        }
                        // No row yet: there is nothing to say about a file
                        // that was never measured.
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(path = ?item.path, error = %e,
                                "storage sweep: could not read the row");
                        }
                    }
                }
                StatOutcome::Unreadable(why) => {
                    tracing::debug!(path = ?item.path, why, "storage sweep: file not read");
                    report.unreadable += 1;
                }
            }
        }

        // 2. Rows whose file no record holds any more (the item was removed,
        // or its file was replaced): remove them. If the file is still on
        // disk, the walk below finds it as an orphan.
        let known: HashSet<PathBuf> = items.into_iter().map(|i| i.path).collect();
        match self.store.library_files_of_kind(kind).await {
            Ok(rows) => {
                for row in rows {
                    if !known.contains(Path::new(&row.path))
                        && self
                            .store
                            .delete_library_file(&row.id)
                            .await
                            .unwrap_or(false)
                    {
                        report.dropped += 1;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "storage sweep: could not list the rows"),
        }

        // 3. Files on disk that no record holds.
        let walk_root = folder.clone();
        let job = tokio::task::spawn_blocking(move || {
            walk_unknown(&walk_root, &known, SystemTime::now())
        });
        let walk = match tokio::time::timeout(WALK_TIMEOUT, job).await {
            Ok(Ok(Ok(walk))) => walk,
            Ok(Ok(Err(e))) => {
                report.skipped = Some(format!("could not walk {}: {e}", folder.display()));
                return report;
            }
            Ok(Err(e)) => {
                report.skipped = Some(format!("could not walk {}: {e}", folder.display()));
                return report;
            }
            Err(_) => {
                report.skipped = Some(format!("the walk of {} took too long", folder.display()));
                return report;
            }
        };
        for (path, size, mtime) in &walk.unknown {
            let recorded = self
                .store
                .record_library_orphan(
                    &path.to_string_lossy(),
                    i64::try_from(*size).unwrap_or(i64::MAX),
                    DateTime::<Utc>::from(*mtime),
                    Utc::now(),
                )
                .await;
            match recorded {
                Ok(true) => report.orphans += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(?path, error = %e, "storage sweep: orphan not stored"),
            }
        }
        // An orphan this pass did not meet is gone from disk, but only a
        // walk that read every folder can say so.
        if walk.complete {
            match self
                .store
                .prune_library_orphans(&folder.to_string_lossy(), started)
                .await
            {
                Ok(n) => report.pruned = n,
                Err(e) => tracing::warn!(error = %e, "storage sweep: orphans not pruned"),
            }
        }
        report
    }
}

impl skadi_core::module::Worker for StorageSweepWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(
        self: Box<Self>,
        cancel: CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        Box::pin(async move {
            let kind = kind_name(self.source.kind());
            self.announce();
            let mut wait = self.start_delay;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
                let Some(interval) = self.interval().await else {
                    wait = OFF_RECHECK;
                    continue;
                };
                let report = self.sweep_once(&cancel).await;
                match &report.skipped {
                    Some(why) => tracing::warn!(kind, why, "storage sweep: not completed"),
                    None => tracing::info!(
                        kind,
                        files = report.files,
                        recorded = report.recorded,
                        gone = report.gone,
                        unreadable = report.unreadable,
                        dropped = report.dropped,
                        orphans = report.orphans,
                        pruned = report.pruned,
                        "storage sweep: done"
                    ),
                }
                wait = interval;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeSource {
        items: Mutex<Vec<StorageItem>>,
    }

    #[async_trait]
    impl StorageSource for FakeSource {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        async fn files(&self) -> Result<Vec<StorageItem>> {
            Ok(self.items.lock().unwrap().clone())
        }
    }

    struct Lib {
        root: PathBuf,
        store: Store,
        source: Arc<FakeSource>,
    }

    impl Lib {
        async fn new() -> Self {
            let base = skadi_core::unique_temp_path("storage-sweep");
            let root = base.join("movie");
            std::fs::create_dir_all(&root).unwrap();
            let store =
                Store::connect(&format!("sqlite://{}", base.join("t.db").display())).unwrap();
            store.run_migrations().await.unwrap();
            Self {
                root,
                store,
                source: Arc::new(FakeSource {
                    items: Mutex::new(Vec::new()),
                }),
            }
        }

        /// Write a file, old enough to count as settled.
        fn file(&self, rel: &str, bytes: usize) -> PathBuf {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, vec![7u8; bytes]).unwrap();
            let old = SystemTime::now() - Duration::from_secs(2 * 3600);
            let f = std::fs::File::options().write(true).open(&path).unwrap();
            f.set_modified(old).unwrap();
            path
        }

        fn hold(&self, acquirable_ref: &str, path: &Path) {
            self.source.items.lock().unwrap().push(StorageItem {
                acquirable_ref: acquirable_ref.into(),
                path: path.to_path_buf(),
            });
        }

        fn worker(&self) -> StorageSweepWorker {
            StorageSweepWorker::new(self.source.clone(), self.store.clone())
                .with_root(&self.root)
                .with_pauses(Duration::ZERO, Duration::ZERO)
        }

        async fn sweep(&self) -> SweepReport {
            self.worker().sweep_once(&CancellationToken::new()).await
        }

        async fn orphans(&self) -> Vec<String> {
            let rows = self.store.list_library_orphans(100, 0).await.unwrap();
            let mut paths: Vec<String> = rows.into_iter().map(|o| o.path).collect();
            paths.sort();
            paths
        }
    }

    #[tokio::test]
    async fn an_import_writes_the_row_with_what_the_mount_gives() {
        let lib = Lib::new().await;
        let path = lib.file("A (2020)/a.mkv", 5000);
        assert!(record_file(&lib.store, MediaKind::Movie, "ed-1", &path).await);
        let row = lib
            .store
            .library_file_by_path(&path.to_string_lossy())
            .await
            .unwrap()
            .expect("the row");
        assert_eq!(
            (row.kind.as_str(), row.acquirable_ref.as_str()),
            ("movie", "ed-1")
        );
        assert_eq!(row.size_bytes, 5000);
        assert_eq!(row.links, 1);
        assert!(row.ino != 0);
        assert!(row.gone_at.is_none());
    }

    #[tokio::test]
    async fn an_import_whose_file_is_not_there_writes_nothing_and_does_not_fail() {
        let lib = Lib::new().await;
        let path = lib.root.join("nope.mkv");
        assert!(!record_file(&lib.store, MediaKind::Movie, "ed-1", &path).await);
    }

    #[tokio::test]
    async fn the_sweep_adds_a_missing_row_and_updates_a_changed_one() {
        let lib = Lib::new().await;
        let path = lib.file("A (2020)/a.mkv", 1000);
        lib.hold("ed-1", &path);

        let report = lib.sweep().await;
        assert_eq!(report.skipped, None);
        assert_eq!((report.files, report.recorded), (1, 1));
        let key = path.to_string_lossy().into_owned();
        let row = lib.store.library_file_by_path(&key).await.unwrap().unwrap();
        assert_eq!(row.size_bytes, 1000);

        // Nothing changed: the same row, no new sample.
        lib.sweep().await;
        assert_eq!(
            lib.store.library_file_samples(&row.id).await.unwrap().len(),
            1
        );

        // A hard link appears and the file grows: the row follows, with one
        // sample.
        std::fs::hard_link(&path, lib.root.join("A (2020)/link.mkv")).unwrap();
        lib.hold("ed-1", &lib.root.join("A (2020)/link.mkv"));
        std::fs::write(&path, vec![1u8; 3000]).unwrap();
        lib.sweep().await;
        let after = lib.store.library_file_by_path(&key).await.unwrap().unwrap();
        assert_eq!(after.id, row.id);
        assert_eq!((after.size_bytes, after.links), (3000, 2));
        assert_eq!(
            lib.store.library_file_samples(&row.id).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_on_disk_is_marked_gone_and_its_row_kept() {
        let lib = Lib::new().await;
        let a = lib.file("A (2020)/a.mkv", 1000);
        let b = lib.file("B (2021)/b.mkv", 1000);
        lib.hold("ed-a", &a);
        lib.hold("ed-b", &b);
        lib.sweep().await;

        std::fs::remove_file(&a).unwrap();
        let report = lib.sweep().await;
        assert_eq!(report.skipped, None);
        assert_eq!((report.gone, report.dropped), (1, 0));
        let row = lib
            .store
            .library_file_by_path(&a.to_string_lossy())
            .await
            .unwrap()
            .expect("the row stays");
        assert!(row.gone_at.is_some());

        // Back again: found.
        lib.file("A (2020)/a.mkv", 1000);
        lib.sweep().await;
        let row = lib
            .store
            .library_file_by_path(&a.to_string_lossy())
            .await
            .unwrap()
            .unwrap();
        assert!(row.gone_at.is_none());
    }

    /// The guard that matters most: a library that is not mounted looks like
    /// a library where every file was deleted.
    #[tokio::test]
    async fn a_folder_that_is_missing_or_empty_is_skipped_and_nothing_is_marked_gone() {
        let lib = Lib::new().await;
        let a = lib.file("A (2020)/a.mkv", 1000);
        lib.hold("ed-a", &a);
        lib.sweep().await;
        let key = a.to_string_lossy().into_owned();

        // Empty: what an unmounted share leaves behind.
        std::fs::remove_dir_all(&lib.root).unwrap();
        std::fs::create_dir_all(&lib.root).unwrap();
        let report = lib.sweep().await;
        assert!(report.skipped.is_some(), "an empty folder must be skipped");
        assert_eq!(report.gone, 0);
        let row = lib.store.library_file_by_path(&key).await.unwrap().unwrap();
        assert!(row.gone_at.is_none(), "nothing is marked gone");

        // Missing altogether.
        std::fs::remove_dir_all(&lib.root).unwrap();
        let report = lib.sweep().await;
        assert!(report.skipped.is_some(), "a missing folder must be skipped");
        let row = lib.store.library_file_by_path(&key).await.unwrap().unwrap();
        assert!(row.gone_at.is_none());
    }

    #[tokio::test]
    async fn a_marker_the_operator_asked_for_must_be_there() {
        let lib = Lib::new().await;
        lib.file("A (2020)/a.mkv", 1000);
        let marker = lib.root.parent().unwrap().join(ROOT_MARKER);
        assert!(folder_ready(&lib.root, Some(&marker)).is_err());
        std::fs::write(&marker, b"").unwrap();
        assert!(folder_ready(&lib.root, Some(&marker)).is_ok());
    }

    #[tokio::test]
    async fn files_no_record_holds_are_orphans_until_claimed_or_gone() {
        let lib = Lib::new().await;
        let held = lib.file("A (2020)/a.mkv", 1000);
        lib.hold("ed-a", &held);
        let stray = lib.file("A (2020)/old-copy.mkv", 1000);
        let other = lib.file("Stray (1999)/s.mp4", 1000);
        // Not orphans: a small file that is not media, the storage host's own
        // folder, and a file still being written.
        lib.file("A (2020)/poster.jpg", 1000);
        lib.file("A (2020)/@eaDir/thumb.mkv", 1000);
        std::fs::write(lib.root.join("A (2020)/incoming.mkv"), b"new").unwrap();

        let report = lib.sweep().await;
        assert_eq!(report.skipped, None);
        assert_eq!(report.orphans, 2);
        let mut want = vec![
            stray.to_string_lossy().into_owned(),
            other.to_string_lossy().into_owned(),
        ];
        want.sort();
        assert_eq!(lib.orphans().await, want);

        // A second pass meets them again: still two rows, first_seen kept.
        let first = lib.store.list_library_orphans(10, 0).await.unwrap();
        lib.sweep().await;
        let second = lib.store.list_library_orphans(10, 0).await.unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(first[0].first_seen, second[0].first_seen);
        assert!(second[0].last_seen >= first[0].last_seen);

        // One is claimed by a record, the other is deleted from disk.
        lib.hold("ed-old", &stray);
        std::fs::remove_file(&other).unwrap();
        let report = lib.sweep().await;
        assert_eq!(report.pruned, 1);
        assert!(lib.orphans().await.is_empty());
    }

    #[tokio::test]
    async fn a_row_whose_record_is_gone_is_dropped_and_its_file_becomes_an_orphan() {
        let lib = Lib::new().await;
        let a = lib.file("A (2020)/a.mkv", 1000);
        lib.hold("ed-a", &a);
        lib.sweep().await;

        // The item is removed from the library; its file stays on disk.
        lib.source.items.lock().unwrap().clear();
        let report = lib.sweep().await;
        assert_eq!(report.dropped, 1);
        assert_eq!(
            lib.store
                .library_file_by_path(&a.to_string_lossy())
                .await
                .unwrap(),
            None
        );
        assert_eq!(lib.orphans().await, vec![a.to_string_lossy().into_owned()]);
    }

    #[tokio::test]
    async fn a_pass_is_reported_with_its_times_and_what_it_did() {
        let lib = Lib::new().await;
        let path = lib.file("A (2020)/a.mkv", 1000);
        lib.hold("ed-1", &path);
        let before = Utc::now();
        lib.sweep().await;
        // Other tests sweep under the same name at the same time, so only
        // what every pass of this fake source has in common is asserted.
        let status = sweep_statuses()
            .into_iter()
            .find(|s| s.name == "fake-storage-sweep")
            .expect("the sweep is listed");
        assert_eq!(status.kind, "movie");
        assert!(status.started_at.is_some_and(|t| t >= before));
        assert!(status.finished_at.is_some_and(|t| t >= before));
        assert!(status.last.is_some());
    }

    #[tokio::test]
    async fn a_completed_pass_writes_the_days_totals_and_a_skipped_one_does_not() {
        let lib = Lib::new().await;
        let path = lib.file("A (2020)/a.mkv", 1000);
        lib.hold("ed-1", &path);
        lib.sweep().await;
        let days = lib.store.library_daily_totals("2000-01-01").await.unwrap();
        assert_eq!(days.len(), 1, "one kind, one day");
        assert_eq!(days[0].day, Utc::now().format("%Y-%m-%d").to_string());
        assert_eq!(
            (
                days[0].totals.kind.as_str(),
                days[0].totals.files,
                days[0].totals.size_bytes
            ),
            ("movie", 1, 1000)
        );

        // The library goes away: the pass is skipped, and the day keeps the
        // totals of the last good pass, not a library of nothing.
        std::fs::remove_dir_all(&lib.root).unwrap();
        assert!(lib.sweep().await.skipped.is_some());
        let days = lib.store.library_daily_totals("2000-01-01").await.unwrap();
        assert_eq!(days[0].totals.files, 1);
    }

    #[test]
    fn what_counts_as_an_orphan() {
        assert!(orphan_worthy(Path::new("/l/a.MKV"), 10));
        assert!(orphan_worthy(Path::new("/l/part.mp3"), 10));
        assert!(!orphan_worthy(Path::new("/l/cover.jpg"), 10));
        assert!(!orphan_worthy(Path::new("/l/a.en.srt"), 10));
        // Big enough to matter whatever it is.
        assert!(orphan_worthy(Path::new("/l/dump.rar"), ORPHAN_MIN_BYTES));
    }

    #[tokio::test]
    async fn cancellation_stops_the_worker() {
        use skadi_core::module::Worker as _;
        let lib = Lib::new().await;
        let worker = Box::new(lib.worker());
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(worker.run(cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("the worker must stop on cancel")
            .unwrap();
    }
}
