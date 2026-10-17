//! Optional MP3-to-M4B conversion after an audiobook import (SKADI-T-0139).
//!
//! Off by default. When `import.audiobook_convert_to_m4b` is on, a background
//! worker finds audiobooks that the **acquire** path imported as something
//! other than M4B, and turns each into one chapterized `.m4b` with `ffmpeg`.
//! It is not part of the acquire workflow: the import finishes as before, and
//! the worker picks the book up afterwards, one book at a time.
//!
//! **Which books.** An edition whose file is `Imported`, not `.m4b`, and has an
//! `imported` row in the acquisition history (the acquire path writes one; the
//! library import does not). A library import's source is the operator's own
//! collection, which skadi never moves or deletes (SKADI-T-0424), so it is left
//! alone.
//!
//! **Chapters.** A `.cue` sheet that fits the files, else one chapter per file
//! (named from the file, or its title tag when the name is only a number), else
//! the single file's own chapters. A book that ends up with none gets one
//! chapter rather than failing.
//!
//! **No data loss.** The output is written under a hidden name in the book's
//! folder, checked (length, chapters), and only then linked to its canonical
//! name. The edition is re-pointed at the M4B before any original is touched.
//! A failure at any step removes the partial output and leaves the import as it
//! was.
//!
//! **Seeding stops on conversion** (Dylan, 2026-10-17). Once the edition points
//! at the M4B, the book's downloads are asked to stop with their data deleted
//! (`request_remove(id, true)`, which the worker carries out), and every
//! original in the library goes, to the recycle bin when one is set. The
//! operator seeds the M4B some other way, if at all.
//!
//! **Visible.** A running conversion shows in `/activity` at stage
//! `converting`; its start, result or failure go to the item's trace stream
//! (`convert_started`, `converted`, `convert_failed`, `convert_skipped`). A
//! failed book is tried again after a day, at most three times; a skipped book
//! (one the conversion cannot apply to) is not tried again.

pub mod ffmpeg;
pub mod plan;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use skadi_core::module::BoxFuture;
use skadi_core::{AcquisitionStatus, FileRef, MediaKind, QualityId, Worker};
use skadi_media_probe::MediaProber;
use skadi_quality::audiobook::{
    AudioBitrate, AudioFormat, default_audiobook_definitions, unknown_audiobook_id,
};
use skadi_store::{ConfigRepo, HistoryQuery, HistoryRepo, Store, TraceEvent, TraceRepo};

use crate::book::Book;
use crate::book_file::BookFile;
use crate::repo::{AudiobooksRepo, BookFilter};

/// The config key that turns the conversion on.
pub const CONVERT_KEY: &str = "import.audiobook_convert_to_m4b";
/// How often the worker looks for books to convert when nothing nudges it.
pub const DEFAULT_CONVERT_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// How long the worker waits after an import nudge, so the acquire run can
/// finish its own writes (media info, the move-mode transfer drop) first.
pub const NUDGE_DELAY: Duration = Duration::from_secs(30);
/// A failed book is tried again after this long…
pub const RETRY_AFTER: chrono::Duration = chrono::Duration::hours(24);
/// …this many times in all.
pub const MAX_ATTEMPTS: usize = 3;

const STAGE: &str = "converting";

static NUDGE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// Wake the worker: an audiobook import just landed. Cheap, and a no-op in
/// effect when the conversion is off.
pub fn nudge() {
    NUDGE.notify_one();
}

/// What one conversion did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Converted {
        path: PathBuf,
        chapters: usize,
        source: plan::ChapterSource,
        removed: usize,
        kept: usize,
    },
    /// The conversion does not apply to this book; not tried again.
    Skipped(String),
    /// The conversion failed; the import is unchanged. Tried again later.
    Failed(String),
}

/// What one pass did, for logs and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    pub enabled: bool,
    pub candidates: usize,
    pub converted: usize,
    pub failed: usize,
    pub skipped: usize,
    /// The conversion is on but ffmpeg cannot be run.
    pub ffmpeg_missing: bool,
}

/// The conversion job: finds books and converts them, one at a time.
#[derive(Clone)]
pub struct ConvertJob {
    store: Store,
    ffmpeg: PathBuf,
}

impl ConvertJob {
    /// A job using `ffmpeg` from `PATH`.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self::with_ffmpeg(store, PathBuf::from("ffmpeg"))
    }

    /// A job using the ffmpeg binary at `ffmpeg`.
    #[must_use]
    pub fn with_ffmpeg(store: Store, ffmpeg: PathBuf) -> Self {
        Self { store, ffmpeg }
    }

    async fn config(&self) -> skadi_config::ConfigView {
        match self.store.list_config().await {
            Ok(entries) => {
                skadi_config::ConfigView::from_pairs(entries.into_iter().map(|e| (e.key, e.value)))
            }
            // Unreadable config reads as all defaults, and the default is off.
            Err(_) => skadi_config::ConfigView::from_pairs(std::iter::empty()),
        }
    }

    /// One pass: when the conversion is on, convert every eligible book in
    /// turn. Returns early when `cancel` fires.
    pub async fn run_pass(&self, cancel: &CancellationToken) -> PassReport {
        let view = self.config().await;
        let mut report = PassReport {
            enabled: view.get_bool(CONVERT_KEY).unwrap_or(false),
            ..PassReport::default()
        };
        if !report.enabled {
            return report;
        }
        let candidates = self.candidates().await;
        report.candidates = candidates.len();
        if candidates.is_empty() {
            return report;
        }
        if !ffmpeg::available(&self.ffmpeg).await {
            // Not a failure of any one book: nothing is marked, and the books
            // convert once ffmpeg is there.
            tracing::warn!(
                ffmpeg = %self.ffmpeg.display(),
                "{CONVERT_KEY} is on, but ffmpeg cannot be run; no audiobook was converted"
            );
            report.ffmpeg_missing = true;
            return report;
        }
        for (book, file) in candidates {
            if cancel.is_cancelled() {
                break;
            }
            match self.convert_tracked(&book, &file, &view, cancel).await {
                Some(Outcome::Converted { .. }) => report.converted += 1,
                Some(Outcome::Failed(_)) => report.failed += 1,
                Some(Outcome::Skipped(_)) => report.skipped += 1,
                None => {}
            }
        }
        report
    }

    /// Imported, non-M4B editions the acquire path imported, that are not
    /// skipped or out of attempts.
    async fn candidates(&self) -> Vec<(Book, BookFile)> {
        let books = match self
            .store
            .list_books(BookFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "m4b conversion: listing books failed");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for book in books {
            for file in &book.files {
                let Some((path, imported_at)) = imported_path(file) else {
                    continue;
                };
                if is_m4b(&path) {
                    continue;
                }
                let aref = file.acquirable_ref().0;
                if !self.acquired(&aref).await {
                    continue;
                }
                if !self.attempt_allowed(&aref, imported_at).await {
                    continue;
                }
                out.push((book.clone(), file.clone()));
            }
        }
        out
    }

    /// The acquire path recorded an import for this edition.
    async fn acquired(&self, aref: &str) -> bool {
        let q = HistoryQuery {
            event: Some("imported".into()),
            kind: Some("audiobook".into()),
            acquirable_ref: Some(aref.to_string()),
            ..HistoryQuery::default()
        };
        self.store
            .list_history_filtered(&q, 1, 0)
            .await
            .is_ok_and(|rows| !rows.is_empty())
    }

    /// Not skipped, fewer than [`MAX_ATTEMPTS`] failures, and the last one at
    /// least [`RETRY_AFTER`] ago — all counted since the current import.
    async fn attempt_allowed(&self, aref: &str, imported_at: DateTime<Utc>) -> bool {
        let Ok(traces) = self.store.traces_for(aref).await else {
            return false;
        };
        attempt_allowed(&traces, imported_at, Utc::now())
    }

    /// [`convert_one`](Self::convert_one) under an `/activity` entry, with
    /// the trace events. `None` when an acquire run holds the edition.
    async fn convert_tracked(
        &self,
        book: &Book,
        file: &BookFile,
        view: &skadi_config::ConfigView,
        cancel: &CancellationToken,
    ) -> Option<Outcome> {
        let aref = file.acquirable_ref().0;
        let run_id = uuid::Uuid::new_v4().to_string();
        let tracker = skadi_hunter::tracker();
        // An acquire run (an upgrade) owns this edition; try next pass.
        if !tracker.try_start(run_id.clone(), MediaKind::Audiobook, aref.clone()) {
            return None;
        }
        tracker.set_stage(&aref, STAGE);
        tracker.set_title(&aref, Some(book.title.clone()));
        let _done = FinishOnDrop(aref.clone());

        self.trace(
            &aref,
            &run_id,
            "convert_started",
            format!("converting \"{}\" to M4B", book.title),
            None,
        )
        .await;
        let outcome = self.convert_one(book, file, view, cancel).await;
        let (event, message, detail) = match &outcome {
            Outcome::Converted {
                path,
                chapters,
                source,
                removed,
                kept,
            } => (
                "converted",
                format!(
                    "converted to {} with {chapters} chapters (from {}); {removed} originals removed, {kept} kept",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("m4b"),
                    source.label()
                ),
                None,
            ),
            Outcome::Skipped(why) => ("convert_skipped", format!("not converted: {why}"), None),
            Outcome::Failed(why) => (
                "convert_failed",
                "conversion failed; the imported files are unchanged".to_string(),
                Some(why.clone()),
            ),
        };
        match &outcome {
            Outcome::Failed(why) => {
                tracing::warn!(book = %book.title, error = %why, "m4b conversion failed");
            }
            other => tracing::info!(book = %book.title, outcome = ?other, "m4b conversion"),
        }
        // A shutdown is not the book's fault: no failure is recorded.
        if !cancel.is_cancelled() {
            self.trace(&aref, &run_id, event, message, detail).await;
        }
        Some(outcome)
    }

    async fn trace(
        &self,
        aref: &str,
        run_id: &str,
        event: &str,
        message: String,
        detail: Option<String>,
    ) {
        let entry = TraceEvent {
            id: uuid::Uuid::new_v4().to_string(),
            at: Utc::now(),
            run_id: Some(run_id.to_string()),
            kind: "audiobook".into(),
            acquirable_ref: aref.to_string(),
            stage: STAGE.into(),
            event: event.into(),
            message,
            detail,
        };
        if let Err(e) = self.store.record_trace(&entry).await {
            tracing::warn!(error = %e, event, "m4b conversion: recording trace failed");
        }
    }

    /// Convert one edition. Never leaves a partial file, and never touches the
    /// originals unless the edition already points at the new M4B.
    pub async fn convert_one(
        &self,
        book: &Book,
        file: &BookFile,
        view: &skadi_config::ConfigView,
        cancel: &CancellationToken,
    ) -> Outcome {
        let Some((primary, _)) = imported_path(file) else {
            return Outcome::Skipped("the edition is not imported".into());
        };
        let naming = crate::naming::AudiobookNaming::from_view(view);
        let guards = skadi_config::import_guards(view);

        let prep = {
            let (book, file, primary) = (book.clone(), file.clone(), primary.clone());
            let reserve = guards.min_free_bytes;
            match tokio::task::spawn_blocking(move || {
                prepare(&book, &file, &primary, &naming, reserve)
            })
            .await
            {
                Ok(Ok(p)) => p,
                Ok(Err(o)) => return o,
                Err(e) => return Outcome::Failed(format!("prepare task panicked: {e}")),
            }
        };

        let work = std::env::temp_dir().join(format!("skadi-m4b-{}", uuid::Uuid::new_v4()));
        let result = self.encode(book, &prep, &work, cancel).await;
        let _ = std::fs::remove_dir_all(&work);
        let (chapters, kbps) = match result {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_file(&prep.partial);
                return Outcome::Failed(e);
            }
        };

        // The edition must still be what we converted: an upgrade that landed
        // meanwhile wins, and our output is discarded.
        match self.store.get_book_file(file.id).await {
            Ok(Some(now)) if imported_path(&now).is_some_and(|(p, _)| p == primary) => {}
            Ok(_) => {
                let _ = std::fs::remove_file(&prep.partial);
                return Outcome::Skipped("the edition changed during the conversion".into());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&prep.partial);
                return Outcome::Failed(format!("re-reading the edition: {e}"));
            }
        }

        // A hard link refuses to replace anything at the canonical name, which
        // a rename would silently do.
        if let Err(e) = std::fs::hard_link(&prep.partial, &prep.final_path) {
            let _ = std::fs::remove_file(&prep.partial);
            return Outcome::Failed(format!("placing {}: {e}", prep.final_path.display()));
        }
        let _ = std::fs::remove_file(&prep.partial);

        let final_path = prep.final_path.clone();
        let media_info = tokio::task::spawn_blocking(move || {
            skadi_media_probe::DefaultProber.probe(&final_path)
        })
        .await
        .ok()
        .flatten();

        let mut updated = file.clone();
        if let Ok(Some(now)) = self.store.get_book_file(file.id).await {
            updated = now;
        }
        let quality = m4b_quality(kbps);
        if let AcquisitionStatus::Imported { score, at, .. } = updated.status.clone() {
            updated.status = AcquisitionStatus::Imported {
                file: FileRef {
                    path: prep.final_path.clone(),
                },
                quality,
                score,
                at,
            };
        }
        updated.file = Some(FileRef {
            path: prep.final_path.clone(),
        });
        updated.quality = Some(quality);
        if media_info.is_some() {
            updated.media_info = media_info;
        }
        if let Err(e) = self.store.upsert_book_file(&updated).await {
            let _ = std::fs::remove_file(&prep.final_path);
            return Outcome::Failed(format!("recording the M4B: {e}"));
        }

        // Stop seeding the MP3 release: its torrent and its data go.
        self.stop_seeding(&file.acquirable_ref().0).await;

        let sources: Vec<PathBuf> = prep.sources.iter().map(|s| s.path.clone()).collect();
        let bin = guards.recycle_bin.clone();
        let (removed, kept) =
            tokio::task::spawn_blocking(move || retire_originals(&sources, bin.as_deref()))
                .await
                .unwrap_or((0, prep.sources.len()));

        Outcome::Converted {
            path: prep.final_path,
            chapters,
            source: prep.chapter_source,
            removed,
            kept,
        }
    }

    /// Ask the worker to stop every download of this edition and delete its
    /// data. Best-effort: a failure is logged and the conversion stands.
    async fn stop_seeding(&self, aref: &str) {
        use skadi_store::DownloadJobRepo;
        let jobs = match self.store.list_downloads().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, aref, "m4b: listing downloads to stop seeding failed");
                return;
            }
        };
        for job in jobs.iter().filter(|j| {
            j.target_ref.as_deref() == Some(aref) && j.target_kind.as_deref() == Some("audiobook")
        }) {
            match self.store.request_remove(&job.id, true).await {
                Ok(()) => {
                    tracing::info!(download = %job.id, aref, "m4b: stopped seeding the source")
                }
                Err(e) => {
                    tracing::warn!(error = %e, download = %job.id, "m4b: stop seeding failed")
                }
            }
        }
    }

    /// Write the inputs ffmpeg reads into `work`, run it into `prep.partial`,
    /// and check the result. Returns the chapter count and the audio kbps.
    async fn encode(
        &self,
        book: &Book,
        prep: &Prepared,
        work: &Path,
        cancel: &CancellationToken,
    ) -> Result<(usize, u32), String> {
        std::fs::create_dir_all(work).map_err(|e| format!("temp dir: {e}"))?;
        let tags = plan::Tags {
            title: book.title.clone(),
            author: book.authors.first().cloned(),
        };
        let meta = work.join("metadata.txt");
        std::fs::write(&meta, plan::ffmetadata(&tags, &prep.chapters))
            .map_err(|e| format!("writing metadata: {e}"))?;
        let paths: Vec<PathBuf> = prep.sources.iter().map(|s| s.path.clone()).collect();
        let (input, concat) = if paths.len() == 1 {
            (paths[0].clone(), false)
        } else {
            let list = work.join("files.txt");
            std::fs::write(&list, plan::concat_list(&paths))
                .map_err(|e| format!("writing file list: {e}"))?;
            (list, true)
        };
        let audio = if plan::can_copy(&paths) {
            plan::Audio::Copy
        } else {
            plan::Audio::Encode(plan::target_kbps(prep.source_kbps))
        };
        let inv = plan::Invocation {
            input,
            concat,
            metadata: meta,
            chapters_from_metadata: !prep.chapters.is_empty(),
            audio,
            output: prep.partial.clone(),
        };
        let title = book.title.clone();
        let aref_progress = prep.aref.clone();
        let mut last_logged = 0u8;
        ffmpeg::run(
            &self.ffmpeg,
            &plan::ffmpeg_args(&inv),
            prep.total,
            ffmpeg::timeout_for(prep.total),
            cancel,
            move |pct| {
                skadi_hunter::tracker().touch(&aref_progress);
                if pct >= last_logged.saturating_add(10) {
                    last_logged = pct - pct % 10;
                    tracing::info!(book = %title, percent = pct, "m4b conversion progress");
                }
            },
        )
        .await
        .map_err(|e| e.to_string())?;

        // Check the output, and give it one chapter if it has none.
        let partial = prep.partial.clone();
        let total = prep.total;
        let checked = tokio::task::spawn_blocking(move || check_output(&partial, total)).await;
        let (out_dur, mut chapters, out_kbps) = match checked {
            Ok(r) => r?,
            Err(e) => return Err(format!("check task panicked: {e}")),
        };
        if chapters == 0 {
            self.add_single_chapter(&tags, prep, work, out_dur, cancel)
                .await?;
            let partial = prep.partial.clone();
            chapters = tokio::task::spawn_blocking(move || {
                skadi_media_probe::chapters::chapters(&partial).map_or(0, |c| c.len())
            })
            .await
            .unwrap_or(0);
            if chapters == 0 {
                return Err("the M4B has no chapters, even after adding one".into());
            }
        }
        let kbps = match audio {
            plan::Audio::Encode(k) => k,
            plan::Audio::Copy => out_kbps.or(prep.source_kbps).unwrap_or(64),
        };
        Ok((chapters, kbps))
    }

    /// Remux `prep.partial` with one chapter over the whole book (stream copy,
    /// so it is quick and lossless).
    async fn add_single_chapter(
        &self,
        tags: &plan::Tags,
        prep: &Prepared,
        work: &Path,
        duration: Duration,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let meta = work.join("one-chapter.txt");
        let one = [plan::Chapter {
            title: tags.title.clone(),
            start: Duration::ZERO,
            end: duration,
        }];
        std::fs::write(&meta, plan::ffmetadata(tags, &one))
            .map_err(|e| format!("writing metadata: {e}"))?;
        let second = prep.partial.with_file_name(format!(
            "{}.chapter.m4b",
            prep.partial
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(".partial")
        ));
        let inv = plan::Invocation {
            input: prep.partial.clone(),
            concat: false,
            metadata: meta,
            chapters_from_metadata: true,
            audio: plan::Audio::Copy,
            output: second.clone(),
        };
        let r = ffmpeg::run(
            &self.ffmpeg,
            &plan::ffmpeg_args(&inv),
            Some(duration),
            ffmpeg::timeout_for(Some(duration)),
            cancel,
            |_| {},
        )
        .await;
        if let Err(e) = r {
            let _ = std::fs::remove_file(&second);
            return Err(format!("adding a chapter: {e}"));
        }
        std::fs::rename(&second, &prep.partial).map_err(|e| {
            let _ = std::fs::remove_file(&second);
            format!("adding a chapter: {e}")
        })
    }
}

/// Removes the `/activity` entry however the conversion ends.
struct FinishOnDrop(String);

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        skadi_hunter::tracker().finish(&self.0);
    }
}

/// The imported path and import time of an `Imported` edition.
fn imported_path(file: &BookFile) -> Option<(PathBuf, DateTime<Utc>)> {
    match &file.status {
        AcquisitionStatus::Imported { file: f, at, .. } => Some((f.path.clone(), *at)),
        _ => None,
    }
}

fn is_m4b(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("m4b"))
}

/// The retry rule over an edition's trace events since its import.
fn attempt_allowed(traces: &[TraceEvent], imported_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let ours = || {
        traces
            .iter()
            .filter(move |t| t.stage == STAGE && t.at >= imported_at)
    };
    if ours().any(|t| t.event == "convert_skipped") {
        return false;
    }
    let failures: Vec<DateTime<Utc>> = ours()
        .filter(|t| t.event == "convert_failed")
        .map(|t| t.at)
        .collect();
    if failures.len() >= MAX_ATTEMPTS {
        return false;
    }
    failures
        .iter()
        .max()
        .is_none_or(|last| now - *last >= RETRY_AFTER)
}

/// The M4B tier for an AAC stream at `kbps`: the highest M4B tier at or below
/// it, else the lowest M4B tier.
#[must_use]
pub fn m4b_quality(kbps: u32) -> QualityId {
    let tier = AudioBitrate::from_kbps(kbps);
    let unknown = unknown_audiobook_id();
    let defs = default_audiobook_definitions();
    let m4b: Vec<_> = defs
        .iter()
        .filter(|d| d.id != unknown && d.format == AudioFormat::M4b)
        .collect();
    m4b.iter()
        .rfind(|d| d.bitrate <= tier)
        .or_else(|| m4b.first())
        .map_or(unknown, |d| d.id)
}

/// Everything the conversion of one edition reads, decided before ffmpeg runs.
struct Prepared {
    aref: String,
    sources: Vec<plan::Source>,
    source_kbps: Option<u32>,
    chapter_source: plan::ChapterSource,
    chapters: Vec<plan::Chapter>,
    /// Sum of the source durations, when every one is known.
    total: Option<Duration>,
    final_path: PathBuf,
    partial: PathBuf,
}

/// Gather the sources and decide the output. Blocking. `Err` carries the
/// outcome to report when the conversion cannot go ahead.
fn prepare(
    book: &Book,
    file: &BookFile,
    primary: &Path,
    naming: &crate::naming::AudiobookNaming,
    reserve: u64,
) -> Result<Prepared, Outcome> {
    let skip = |s: &str| Err(Outcome::Skipped(s.to_string()));
    let Some(dir) = primary.parent().map(Path::to_path_buf) else {
        return skip("the imported file has no folder");
    };
    let root = &book.root_folder.path;
    if !root.as_os_str().is_empty() && (!dir.starts_with(root) || dir == *root) {
        return skip("the imported file is not in a book folder under the root folder");
    }
    // Two editions in one folder: joining "all the audio here" would merge them.
    if book.files.iter().any(|f| {
        f.id != file.id && imported_path(f).is_some_and(|(p, _)| p.parent() == Some(dir.as_path()))
    }) {
        return skip("the folder holds another edition of the book");
    }

    let mut audio = Vec::new();
    let mut cues = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| Outcome::Failed(format!("reading {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') || !p.is_file() {
            continue;
        }
        if p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
        {
            cues.push(p);
        } else if crate::import::is_audio(&p) && !crate::import::is_excerpt(name) {
            audio.push(p);
        }
    }
    if audio.iter().any(|p| is_m4b(p)) {
        return skip("the folder already holds an M4B");
    }
    if !audio.iter().any(|p| p == primary) {
        return Err(Outcome::Failed(format!(
            "the imported file {} is missing",
            primary.display()
        )));
    }
    plan::sort_natural(&mut audio);

    let mut sources = Vec::with_capacity(audio.len());
    let mut source_kbps = None;
    let mut need: u64 = 0;
    for p in audio {
        need = need.saturating_add(std::fs::metadata(&p).map_or(0, |m| m.len()));
        let facts = skadi_media_probe::audio_facts(&p);
        if source_kbps.is_none() {
            source_kbps = facts.as_ref().and_then(|f| f.bitrate_kbps);
        }
        sources.push(plan::Source {
            path: p,
            duration: facts.as_ref().map(|f| f.duration).filter(|d| !d.is_zero()),
            title: facts.and_then(|f| f.title),
        });
    }
    let total = sources.iter().map(|s| s.duration).sum::<Option<Duration>>();

    // One cue sheet is a chapter list; two are a puzzle, so neither is used.
    let cue = match cues.as_slice() {
        [one] => std::fs::read(one)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
        _ => None,
    };
    let (chapter_source, chapters) = plan::plan_chapters(cue.as_deref(), &sources);

    let name = naming
        .path(
            root,
            book.authors.first().map(String::as_str),
            book.series.as_ref().map(|s| s.name.as_str()),
            book.series.as_ref().and_then(|s| s.position.as_deref()),
            &book.title,
            book.external_ids.asin.as_ref().map(|a| a.0.as_str()),
            Path::new("book.m4b"),
            true,
        )
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "book.m4b".into());
    let final_path = dir.join(&name);
    if final_path.exists() {
        return skip(&format!("{} already exists", final_path.display()));
    }
    let stem = Path::new(&name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("book")
        .to_string();
    let partial = dir.join(format!(".{stem}.partial.m4b"));
    // Ours by name: a leftover from a run the daemon did not finish.
    let _ = std::fs::remove_file(&partial);

    // The output is at most the size of the sources (AAC at or below their
    // rate), and both exist until the originals are retired.
    if let skadi_importer::SpaceVerdict::Insufficient { need, available } =
        skadi_importer::space_check(need, skadi_importer::available_space(&dir), reserve)
    {
        return Err(Outcome::Failed(format!(
            "not enough free space: needs {need} bytes, {available} free"
        )));
    }

    Ok(Prepared {
        aref: file.acquirable_ref().0,
        sources,
        source_kbps,
        chapter_source,
        chapters,
        total,
        final_path,
        partial,
    })
}

/// Check ffmpeg's output: it reads as audio, is about as long as the sources,
/// and how many chapters it has. Returns `(duration, chapters, kbps)`.
fn check_output(
    partial: &Path,
    total: Option<Duration>,
) -> Result<(Duration, usize, Option<u32>), String> {
    let facts = skadi_media_probe::audio_facts(partial)
        .ok_or_else(|| "the output does not read as audio".to_string())?;
    if facts.duration.is_zero() {
        return Err("the output has no audio".into());
    }
    // Probed lengths of VBR MP3s are estimates, so allow some slack; a
    // truncated encode is far shorter than this.
    if let Some(total) = total
        && facts.duration < total.mul_f64(0.9)
    {
        return Err(format!(
            "the output is {}s long, the sources {}s",
            facts.duration.as_secs(),
            total.as_secs()
        ));
    }
    let chapters = skadi_media_probe::chapters::chapters(partial).map_or(0, |c| c.len());
    Ok((facts.duration, chapters, facts.bitrate_kbps))
}

/// Retire the originals after the edition points at the M4B. Returns
/// `(removed, kept)`; a file is kept only when it cannot be removed. Blocking,
/// best-effort.
///
/// A file with another hard link is a second name for bytes the seeding
/// download also holds; that download is stopped with its data in the same
/// step, so the name is simply removed. A file that is the only copy goes to
/// the recycle bin when one is set, else it is deleted.
fn retire_originals(sources: &[PathBuf], bin: Option<&Path>) -> (usize, usize) {
    let mut removed = 0;
    let mut kept = 0;
    for p in sources {
        let gone = match bin {
            Some(bin) if link_count(p) <= 1 => skadi_importer::recycle(p, bin).is_some(),
            _ => std::fs::remove_file(p).is_ok(),
        };
        if gone {
            removed += 1;
        } else {
            kept += 1;
        }
    }
    (removed, kept)
}

#[cfg(unix)]
fn link_count(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).map_or(1, |m| m.nlink())
}

#[cfg(not(unix))]
fn link_count(_: &Path) -> u64 {
    1
}

/// The daemon-side worker: a pass on an interval, and soon after each import.
pub struct AudiobookConvertWorker {
    job: ConvertJob,
    interval: Duration,
}

impl AudiobookConvertWorker {
    #[must_use]
    pub fn new(job: ConvertJob, interval: Duration) -> Self {
        Self { job, interval }
    }
}

impl Worker for AudiobookConvertWorker {
    fn name(&self) -> &str {
        "audiobooks-m4b-convert"
    }

    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.tick().await; // skip the immediate first tick
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {}
                    () = NUDGE.notified() => {
                        tokio::select! {
                            () = cancel.cancelled() => break,
                            () = tokio::time::sleep(NUDGE_DELAY) => {}
                        }
                    }
                }
                let report = self.job.run_pass(&cancel).await;
                if report.candidates > 0 {
                    tracing::info!(?report, "m4b conversion pass");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
