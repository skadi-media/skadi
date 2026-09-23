//! Library import (SKADI-I-0047): bring existing on-disk **television** files
//! under management — scan a source path (parse-only), and (after the operator
//! confirms series matches) restructure each episode file into the canonical
//! layout under the destination root folder and mark the matching `Episode`
//! `Imported`. The TV parallel to `skadi-movies`'s `import` module.
//!
//! Restructure semantics mirror movies: a file already at its canonical path is
//! recorded as-is (no fs op, [`Placement::InPlace`]); otherwise it is
//! **hardlinked** to the canonical path ([`Placement::Linked`], hardlink-only —
//! cross-device is refused, a copy would double disk usage). A fresh
//! link is an adoption *move* — the source video is dropped afterwards
//! (SKADI-T-0303 parallel); an in-place file already *is* the canonical file and
//! is never touched. A placement/DB failure for one file is isolated and never
//! removes the source.
//!
//! This module owns the filesystem scan/parse (pure, testable) and the commit;
//! the HTTP surface lives in [`crate::http`]. The matcher (which maps a parsed
//! filename to the episode(s) it satisfies + the canonical dest) is reused from
//! [`crate::matcher`], so acquire-import and library-import can never drift.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use skadi_core::{
    AcquisitionStatus, AppError, EpisodeId, FileRef, ProfileId, QualityId, Result, RootFolder,
    TvdbId,
};
use skadi_downloaders::DownloadHandle;
use skadi_importer::{AcquirableMatcher, CompletedDownload, looks_like_sample};
use skadi_metadata::SeriesMetadataProvider;
use skadi_quality::{QualityDefinition, default_definitions, parse, parse_tv, to_quality};
use uuid::Uuid;

use crate::episode::Episode;
use crate::matcher::EpisodeMatcher;
use crate::metadata::add_series;
use crate::monitor::MonitorMode;
use crate::naming::SeriesNaming;
use crate::repo::TvRepo;

/// Recognized video container extensions (lowercased, no dot). Same set movies use.
const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "mov", "wmv", "ts", "m2ts", "mpg", "mpeg", "flv", "webm",
];

/// Files smaller than this are ignored as samples/junk. 10 MiB — lower than the
/// movies floor (50 MiB) because TV episodes (short/SD runs) are legitimately
/// small; the shared [`looks_like_sample`] name reject handles named samples.
const MIN_VIDEO_BYTES: u64 = 10 * 1024 * 1024;

/// How deep the scan recurses (series → season → file, plus slack for odd layouts).
const MAX_DEPTH: usize = 5;

/// One scanned candidate, **parse-only** (no metadata lookup, no DB). Scanning a
/// large library must be instant, so series matching is deferred to
/// `/tv/library-import/match`. Each video file is its own candidate (an episode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTvCandidate {
    /// The existing video file path, recorded verbatim (the import source).
    pub path: PathBuf,
    /// The file stem used as the human/match label.
    pub display_name: String,
    /// Parsed series title (best-effort).
    pub series_title: Option<String>,
    /// Parsed season number (`SxxEyy`); `None` for anime/daily/movie-ish names.
    pub season: Option<u16>,
    /// Parsed episode number(s); empty for a season pack or absolute/daily file.
    pub episodes: Vec<u16>,
    /// Parsed absolute episode number(s) for anime; empty otherwise.
    pub absolute: Vec<u16>,
    /// Daily-show air date as `YYYY-MM-DD`, when the release is date-keyed.
    pub air_date: Option<String>,
    /// Detected quality definition id (when the name carried resolution+source).
    pub quality_id: Option<QualityId>,
    /// Detected quality name for display (e.g. `WEBDL-1080p`).
    pub quality_name: Option<String>,
    /// The **show folder** this file lives in: the first path component under the
    /// scanned root (e.g. `12-monkeys_(2015)_{tmdb-60948}`). `None` when the file
    /// sits directly in the root. The import UI groups by this so a show's stray
    /// extras stay under the *right* show instead of spawning bogus one-file shows
    /// (SKADI-T-0324).
    pub folder: Option<String>,
    /// Exact TVDB id from the show folder's `tvshow.nfo` `<uniqueid type="tvdb">`,
    /// when present — authoritative, so the UI can match the show by id and skip
    /// the fuzzy metadata search entirely (SKADI-T-0324).
    pub nfo_tvdb_id: Option<u64>,
    /// Real series title from `tvshow.nfo` `<title>` (beats the messy folder name).
    pub nfo_title: Option<String>,
    /// Series year from `tvshow.nfo` `<year>`, when present.
    pub nfo_year: Option<u16>,
    /// Synopsis (`<plot>`) — a one-line confirm-it's-the-right-show hint.
    pub nfo_overview: Option<String>,
    /// Genres (`<genre>` tags), in order.
    pub nfo_genres: Vec<String>,
    /// Production status (`<status>`, e.g. `Ended`/`Continuing`).
    pub nfo_status: Option<String>,
    /// Network/studio (`<studio>`).
    pub nfo_network: Option<String>,
    /// Rating (`<rating>`, kept as-is for display, e.g. `8.3`).
    pub nfo_rating: Option<String>,
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Detect quality from a release-ish name via the parser + the built-in defs.
fn quality_of(name: &str, defs: &[QualityDefinition]) -> (Option<QualityId>, Option<String>) {
    let parsed = parse(name);
    match to_quality(&parsed, defs) {
        Some(q) => {
            let label = defs.iter().find(|d| d.id == q.id).map(|d| d.name.clone());
            (Some(q.id), label)
        }
        None => (None, None),
    }
}

/// Build a candidate from one video file: episode info via [`parse_tv`], quality
/// via the movie-ish [`parse`] (the quality tokens are container-agnostic).
fn candidate_for_file(path: &Path, defs: &[QualityDefinition]) -> RawTvCandidate {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_string();
    let p = parse_tv(name);
    let (quality_id, quality_name) = quality_of(name, defs);
    RawTvCandidate {
        path: path.to_path_buf(),
        display_name: stem,
        series_title: p.title,
        season: p.season,
        episodes: p.episodes,
        // ParsedRelease carries anime absolute numbers as u32; narrow for the DTO.
        absolute: p
            .absolute
            .into_iter()
            .filter_map(|a| u16::try_from(a).ok())
            .collect(),
        air_date: p.air_date,
        quality_id,
        quality_name,
        folder: None,      // filled by `scan_tv_candidates` (needs the scan root)
        nfo_tvdb_id: None, // ↓ from the folder's tvshow.nfo
        nfo_title: None,
        nfo_year: None,
        nfo_overview: None,
        nfo_genres: Vec::new(),
        nfo_status: None,
        nfo_network: None,
        nfo_rating: None,
    }
}

/// Recursively collect candidate video files under `dir` (bounded by [`MAX_DEPTH`]).
fn walk_videos(
    dir: &Path,
    depth: usize,
    defs: &[QualityDefinition],
    out: &mut Vec<RawTvCandidate>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // unreadable subdir — skip, don't fail the whole scan
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        // `std::fs::metadata` FOLLOWS symlinks (DirEntry::metadata does not) —
        // symlinked season folders / episode files used to scan to zero
        // candidates silently (SKADI-T-0327). `MAX_DEPTH` bounds any link cycle.
        let Ok(meta) = std::fs::metadata(&p) else {
            continue; // broken symlink / vanished — skip
        };
        if meta.is_dir() {
            walk_videos(&p, depth + 1, defs, out);
        } else if meta.is_file()
            && is_video(&p)
            && !looks_like_sample(&p)
            && meta.len() >= MIN_VIDEO_BYTES
        {
            out.push(candidate_for_file(&p, defs));
        }
    }
}

/// How many show folders the scan walks in parallel. Over NFS, per-file stat
/// latency dominates a large library; a worker pool turns a serial multi-minute
/// walk into seconds (mirrors the movies scan, SKADI-T-0078/T-0324).
const SCAN_WALK_WORKERS: usize = 16;

/// Everything we lift from a show folder's `tvshow.nfo`: the exact TVDB id + real
/// title/year for matching, plus display metadata (synopsis, genres, status,
/// network, rating) so an operator can confirm a match at a glance. A
/// missing/garbled NFO yields all-default (fall back to folder + filename parse).
#[derive(Clone, Default)]
struct TvshowNfo {
    tvdb: Option<u64>,
    title: Option<String>,
    year: Option<u16>,
    overview: Option<String>,
    genres: Vec<String>,
    status: Option<String>,
    network: Option<String>,
    rating: Option<String>,
}

/// NFO files are KBs; a mislabeled multi-GB "tvshow.nfo" must not be slurped
/// whole by `read_to_string` — 16 scan workers × huge allocations (SKADI-T-0327).
const MAX_NFO_BYTES: u64 = 1024 * 1024;

fn read_tvshow_nfo(dir: &Path) -> TvshowNfo {
    let nfo = dir.join("tvshow.nfo");
    if std::fs::metadata(&nfo).map_or(true, |m| m.len() > MAX_NFO_BYTES) {
        return TvshowNfo::default();
    }
    let Ok(xml) = std::fs::read_to_string(&nfo) else {
        return TvshowNfo::default();
    };
    use skadi_core::nfo::{tag, tags, uniqueid_u64};
    TvshowNfo {
        tvdb: uniqueid_u64(&xml, "tvdb"),
        title: tag(&xml, "title"),
        year: tag(&xml, "year").and_then(|y| y.parse().ok()),
        overview: tag(&xml, "plot").or_else(|| tag(&xml, "outline")),
        genres: tags(&xml, "genre"),
        status: tag(&xml, "status"),
        network: tag(&xml, "studio").or_else(|| tag(&xml, "network")),
        rating: tag(&xml, "rating"),
    }
}

/// Scan `root` into candidate episode files (pure: filesystem + parse, no
/// network, no DB). Each top-level **show folder** is walked **concurrently** (a
/// bounded worker pool — NFS latency, not CPU, is the bottleneck), its videos
/// parsed with [`parse_tv`] and tagged with the folder + its `tvshow.nfo` hints.
/// Loose videos directly in the root are candidates with no folder. Sorted by
/// path for a stable order. Blocking — call from `spawn_blocking` in async code.
pub fn scan_tv_candidates(root: &Path) -> Result<Vec<RawTvCandidate>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let entries = std::fs::read_dir(root)
        .map_err(|e| AppError::Validation(format!("cannot read {}: {e}", root.display())))?;
    let defs = default_definitions();

    // Split the root into show folders (walked concurrently below) + loose videos.
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut out: Vec<RawTvCandidate> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        // Follow symlinked show folders too (SKADI-T-0327).
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            dirs.push(p);
        } else if meta.is_file()
            && is_video(&p)
            && !looks_like_sample(&p)
            && meta.len() >= MIN_VIDEO_BYTES
        {
            // A loose video directly in the root: no show folder, no NFO.
            out.push(candidate_for_file(&p, &defs));
        }
    }

    // Walk each show folder concurrently, tagging its files with the folder name +
    // its `tvshow.nfo` hints (read once per folder, inside the worker).
    if !dirs.is_empty() {
        let workers = dirs.len().min(SCAN_WALK_WORKERS);
        let next = AtomicUsize::new(0);
        let collected = Mutex::new(Vec::<RawTvCandidate>::new());
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(dir) = dirs.get(i) else { break };
                        let mut cands = Vec::new();
                        walk_videos(dir, 0, &defs, &mut cands);
                        if cands.is_empty() {
                            continue;
                        }
                        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                        let nfo = read_tvshow_nfo(dir);
                        for c in &mut cands {
                            c.folder = folder.clone();
                            c.nfo_tvdb_id = nfo.tvdb;
                            c.nfo_title = nfo.title.clone();
                            c.nfo_year = nfo.year;
                            c.nfo_overview = nfo.overview.clone();
                            c.nfo_genres = nfo.genres.clone();
                            c.nfo_status = nfo.status.clone();
                            c.nfo_network = nfo.network.clone();
                            c.nfo_rating = nfo.rating.clone();
                        }
                        collected
                            .lock()
                            .expect("scan collector poisoned")
                            .extend(cands);
                    }
                });
            }
        });
        out.extend(collected.into_inner().expect("scan collector poisoned"));
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// How a commit placed one episode file relative to its canonical destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The source already was the canonical path (or the same inode) — no fs op.
    InPlace,
    /// The source was hardlinked (copy fallback) to the canonical path; a fresh
    /// link, so the source is an adoption move and is dropped afterwards.
    Linked,
}

/// What `place_episode` found at the destination — a **typed** outcome instead of
/// the old string-matched "destination already exists" error (SKADI-T-0327): a
/// different file already occupying the canonical path is a real state the caller
/// must decide about (adopt it), not a stringly-typed failure.
enum PlaceOutcome {
    Placed(Placement),
    /// `dest` holds a *different* file (different inode). Never overwritten.
    DestOccupied,
}

/// Place `src` into the canonical `dest`, mirroring movies' restructure rules:
/// **hardlink-only** — a fresh link is an adoption move whose source is dropped
/// afterwards, so a silent copy fallback would turn cross-device imports into
/// copy-then-delete (2x disk churn, and `Linked` would be a lie). Cross-device ⇒
/// a validation error telling the operator to scan through the same mount
/// (parity with movies' `restructure_into`, SKADI-T-0324 review). `src == dest`
/// or an already-identical inode ⇒ `InPlace` (no-op); a *different* file already
/// at `dest` ⇒ [`PlaceOutcome::DestOccupied`] (never overwritten). Blocking.
fn place_episode(src: &Path, dest: &Path) -> Result<PlaceOutcome> {
    // One shared primitive (SKADI-T-0424); this was a byte-identical copy.
    Ok(match skadi_importer::adopt_into(src, dest)? {
        skadi_importer::Adoption::InPlace => PlaceOutcome::Placed(Placement::InPlace),
        skadi_importer::Adoption::DestOccupied => PlaceOutcome::DestOccupied,
        skadi_importer::Adoption::Linked => PlaceOutcome::Placed(Placement::Linked),
    })
}

/// One confirmed item to import: an existing file, the series it belongs to
/// (TVDB id), and the detected quality (falls back to the lowest when absent).
/// `season`/`episode` are an **explicit manual mapping** (SKADI-T-0324): when both
/// are set, the file is placed at that episode directly, bypassing the filename
/// re-parse — for files the parser couldn't resolve and the operator picked.
#[derive(Clone, Debug)]
pub struct CommitTarget {
    pub path: PathBuf,
    pub tvdb_id: u64,
    pub quality_id: Option<QualityId>,
    pub season: Option<u16>,
    pub episode: Option<u16>,
}

/// Aggregate result of a commit batch (the HTTP `CommitResult` mirrors it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitOutcome {
    pub imported: usize,
    pub skipped: usize,
    /// Of `imported`: hardlinked to their canonical path (source dropped).
    pub linked: usize,
    /// Of `imported`: already at their canonical path — nothing to move.
    pub in_place: usize,
    /// Files left in place because they couldn't be matched to a TVDB episode
    /// (custom/absolute numbering, specials, misplaced files) or the episode was
    /// already imported. **Benign** — the source is untouched, not an error
    /// (SKADI-T-0324). Each entry is `path: reason`.
    pub unmatched: Vec<String>,
    pub errors: Vec<String>,
}

/// A minimal completed-download stand-in for the matcher (it ignores everything
/// but the source path on a library-import call).
fn import_completed() -> CompletedDownload {
    CompletedDownload {
        handle: DownloadHandle {
            native_id: "library-import".into(),
            category: "tv".into(),
        },
        files: vec![],
        category: "tv".into(),
    }
}

fn decode_episode_id(s: &str) -> Result<EpisodeId> {
    Uuid::parse_str(s)
        .map(EpisodeId::from)
        .map_err(|e| AppError::Validation(format!("invalid episode ref: {e}")))
}

/// Commit confirmed items into the library. Items are grouped by `tvdb_id`; each
/// series is **added on import** if not already present (via [`add_series`] with
/// [`MonitorMode::All`]), then every file in the group is matched to its
/// episode(s) and restructured into the canonical layout.
///
/// Idempotent: a file whose target episode is already `Imported` is counted
/// `skipped`. Best-effort and non-destructive: one file's placement/DB failure is
/// isolated into `errors` and never aborts the batch nor touches the source. The
/// source video is dropped only after a fresh `Linked` placement (an adoption
/// move); an `InPlace` file already *is* the canonical file and is left alone.
pub async fn commit_items(
    repo: &dyn TvRepo,
    provider: &dyn SeriesMetadataProvider,
    items: Vec<CommitTarget>,
    profile: ProfileId,
    root: RootFolder,
    naming: SeriesNaming,
) -> CommitOutcome {
    let mut out = CommitOutcome::default();

    // Group by series so each TVDB id is added/loaded once.
    let mut groups: BTreeMap<u64, Vec<CommitTarget>> = BTreeMap::new();
    for it in items {
        groups.entry(it.tvdb_id).or_default().push(it);
    }

    // Per-path `(linked, bad, first fresh-link dest)` aggregate across ALL items,
    // for the source-drop + companion decision after the loop (see `commit_one`'s
    // return contract).
    #[allow(clippy::type_complexity)]
    let mut drops: BTreeMap<PathBuf, (bool, bool, Option<PathBuf>)> = BTreeMap::new();

    for (tvdb, group) in groups {
        // Add-on-import: ensure the series (+ its episodes) exists, then load it.
        // A NEWLY created series starts **unmonitored** and is armed only after at
        // least one file actually places — otherwise a commit whose files all fail
        // would leave a monitored, all-Missing show and the hunter would start
        // downloading the entire series (SKADI-T-0324 review). Episodes keep their
        // `All` per-episode flags; the hunter gates on `series.monitored`.
        let mut newly_added = false;
        let series = match repo.get_series_by_tvdb(TvdbId(tvdb)).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                match add_series(
                    repo,
                    provider,
                    TvdbId(tvdb),
                    profile,
                    root.clone(),
                    MonitorMode::All,
                )
                .await
                {
                    Ok(mut s) => {
                        newly_added = true;
                        s.monitored = false;
                        if let Err(e) = repo.upsert_series(&s).await {
                            tracing::warn!(tvdb, error = %e, "import: could not disarm new series");
                        }
                        s
                    }
                    Err(e) => {
                        for it in &group {
                            out.errors.push(format!("{}: {e}", it.path.display()));
                        }
                        continue;
                    }
                }
            }
            Err(e) => {
                for it in &group {
                    out.errors.push(format!("{}: {e}", it.path.display()));
                }
                continue;
            }
        };

        let episodes = series.episodes.clone();
        let series_for_arm = series.clone();
        let matcher = EpisodeMatcher::with_naming(series, episodes.clone(), naming.clone());
        let completed = import_completed();

        let imported_before = out.imported;
        for it in group {
            let (linked, bad, dest) =
                commit_one(repo, &matcher, &episodes, &completed, &it, &mut out).await;
            let e = drops.entry(it.path.clone()).or_insert((false, false, None));
            e.0 |= linked;
            e.1 |= bad;
            if e.2.is_none() {
                e.2 = dest;
            }
        }

        // Arm the hunter only once the group has really landed something.
        if newly_added && out.imported > imported_before {
            let mut s = series_for_arm;
            s.monitored = true;
            if let Err(e) = repo.upsert_series(&s).await {
                tracing::warn!(tvdb, error = %e, "import: could not arm imported series");
            }
        }
    }

    // Adoption move, decided per **path** once every item touching it has run: a
    // fresh link with nothing bad (no error, no unmatched intent) brings the
    // video's sidecars along (subs, per-episode NFO/artwork — SKADI-T-0328) and
    // then drops the source video + the sidecars that actually linked. A manual
    // one-file→two-episodes mapping is two items sharing a path — dropping
    // inline after the first would strand the second (SKADI-T-0324 review). An
    // InPlace-only path never set `linked`, so it is never removed.
    for (path, (linked, bad, dest)) in drops {
        if linked && !bad {
            if let Some(dest) = dest {
                let src = path.clone();
                let companions =
                    tokio::task::spawn_blocking(move || link_episode_companions(&src, &dest))
                        .await
                        .unwrap_or_default();
                for c in companions {
                    drop_source(&c).await;
                }
            }
            drop_source(&path).await;
        }
    }

    out
}

/// Drop the adoption source after a successful fresh link (best-effort). Only the
/// single video file — never the season folder, which holds sibling episodes.
async fn drop_source(src: &Path) {
    let s = src.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = std::fs::remove_file(&s) {
            tracing::warn!(file = %s.display(), error = %e, "adoption: source file not removed");
        }
    })
    .await;
}

/// Companion sidecar extensions an episode brings along (SKADI-T-0328): subs,
/// per-episode NFO/artwork. Season folders are shared, so ONLY files prefixed by
/// the video's stem are taken — never loose folder artwork.
const COMPANION_EXTS: &[&str] = &[
    "srt", "ass", "ssa", "sub", "idx", "vtt", "nfo", "jpg", "jpeg", "png",
];

/// Hardlink `src` video's stem-prefixed sidecars next to `dest` (renamed to the
/// canonical stem, suffix preserved: `Show.S01E05.en.srt` → `<dest stem>.en.srt`).
/// Best-effort: failures are logged and skipped — the episode file is the
/// contract. Returns the companion SOURCES that were successfully linked (safe
/// to drop with the video on adoption). Blocking.
fn link_episode_companions(src: &Path, dest: &Path) -> Vec<PathBuf> {
    let (Some(src_dir), Some(src_stem), Some(dest_parent), Some(dest_stem)) = (
        src.parent(),
        src.file_stem().and_then(|s| s.to_str()),
        dest.parent(),
        dest.file_stem().and_then(|s| s.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(src_dir) else {
        return Vec::new();
    };
    let mut linked = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p == src {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let ext_ok = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .is_some_and(|e| COMPANION_EXTS.contains(&e.as_str()));
        if !ext_ok || !name.starts_with(src_stem) {
            continue;
        }
        // `Show.S01E05.en.srt` → suffix `.en.srt` appended to the dest stem.
        let suffix = &name[src_stem.len()..];
        let dest_name = format!("{dest_stem}{suffix}");
        let dest_path = dest_parent.join(dest_name);
        if dest_path.exists() {
            continue; // never overwrite; also skips re-runs
        }
        match std::fs::hard_link(&p, &dest_path) {
            Ok(()) => linked.push(p),
            Err(e) => {
                tracing::warn!(src = %p.display(), dest = %dest_path.display(), error = %e,
                    "skipping companion file");
            }
        }
    }
    linked
}

/// How `place_and_mark` handled one (file, episode) pair.
enum MarkOutcome {
    /// Actually placed the source (`Linked`/`InPlace`) and recorded `Imported`.
    Placed(Placement),
    /// Episode already `Imported` — idempotent re-run, nothing to do.
    AlreadyImported,
    /// Episode has an acquisition in flight (`Snatched`/`Downloading`) — the
    /// import must not clobber it; the two pipelines would fight (SKADI-T-0327).
    ActiveAcquisition,
    /// A *different* file already occupies the canonical dest while the DB said
    /// not-imported: the dest file was **adopted** (recorded `Imported` at dest);
    /// the source stays where it is. Re-runs then converge to `AlreadyImported`
    /// instead of looping on "already exists" forever (SKADI-T-0327).
    AdoptedDest,
}

/// Idempotent place + mark for one already-resolved episode: re-fetch its current
/// status (so a re-run skips), hardlink `src` to `dest`, write `Imported`.
/// Shared by the matcher path and the manual-override path.
async fn place_and_mark(
    repo: &dyn TvRepo,
    src: &Path,
    dest: PathBuf,
    ep_id: EpisodeId,
    quality: QualityId,
) -> Result<MarkOutcome> {
    let episode = repo
        .get_episode(ep_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("episode {ep_id} not found")))?;
    match episode.status {
        AcquisitionStatus::Imported { .. } => return Ok(MarkOutcome::AlreadyImported),
        AcquisitionStatus::Snatched { .. } | AcquisitionStatus::Downloading { .. } => {
            return Ok(MarkOutcome::ActiveAcquisition);
        }
        _ => {}
    }
    let file_path = dest.clone();
    let (s, d) = (src.to_path_buf(), dest);
    let outcome = tokio::task::spawn_blocking(move || place_episode(&s, &d))
        .await
        .map_err(|e| AppError::Internal(format!("placement task panicked: {e}")))??;
    let (placement, adopted) = match outcome {
        PlaceOutcome::Placed(p) => (Some(p), false),
        // Dest occupied by a different file while the DB says not-imported:
        // adopt what's there — the DB now matches disk, and the source is left
        // untouched for the operator.
        PlaceOutcome::DestOccupied => (None, true),
    };
    let status = AcquisitionStatus::Imported {
        file: FileRef { path: file_path },
        quality,
        score: 0,
        at: Utc::now(),
    };
    repo.set_episode_status(ep_id, status).await?;
    Ok(match (placement, adopted) {
        (Some(p), _) => MarkOutcome::Placed(p),
        (None, _) => MarkOutcome::AdoptedDest,
    })
}

/// Import one file. With a manual `(season, episode)` override it places the file
/// at exactly that episode; otherwise the `EpisodeMatcher` resolves the episode(s)
/// from the filename. Places + persists each. Updates `out` in place; never
/// propagates an error.
///
/// Returns `(linked, bad)` for the **source-drop decision**, which the caller
/// makes per *path* after every item touching that path has run — a manual
/// mapping of one file to two episodes is two items with the same path, and
/// dropping after the first would make the second fail on a vanished source
/// (SKADI-T-0324 review). `linked` = this item produced a fresh hardlink;
/// `bad` = anything other than clean success/skip happened (error or benign
/// unmatched) — either blocks the drop so the source is never removed while any
/// intent for it is unsatisfied.
async fn commit_one(
    repo: &dyn TvRepo,
    matcher: &EpisodeMatcher,
    episodes: &[Episode],
    completed: &CompletedDownload,
    item: &CommitTarget,
    out: &mut CommitOutcome,
) -> (bool, bool, Option<PathBuf>) {
    let name = item
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    // Server-side sanity: the commit DTO's path is client-supplied — never
    // hardlink-and-drop something that isn't a plausible episode video
    // (SKADI-T-0327). A *missing* file deliberately falls through: a re-run
    // after a successful adoption (source dropped) must still reach the
    // already-imported skip, and a genuinely absent source fails placement
    // with a clear ENOENT anyway.
    if !is_video(&item.path) {
        out.errors.push(format!(
            "{}: not a recognized video file — refusing to import",
            item.path.display()
        ));
        return (false, true, None);
    }
    if let Ok(m) = std::fs::metadata(&item.path)
        && m.len() < MIN_VIDEO_BYTES
    {
        out.errors.push(format!(
            "{}: below the {} MiB size floor — refusing to import",
            item.path.display(),
            MIN_VIDEO_BYTES / (1024 * 1024)
        ));
        return (false, true, None);
    }

    // Quality: trust the client's id when it sent one; otherwise re-derive it
    // from the filename server-side. The old floor-default recorded SDTV for
    // any un-graded file — a fabricated fact the upgrade logic would act on
    // (SKADI-T-0327).
    let defs = default_definitions();
    let quality = item
        .quality_id
        .or_else(|| quality_of(name, &defs).0)
        // The T-0327 comment above was aspirational: the floor default was still
        // here, so 18,449 adopted episodes on prod carried SDTV and every one of
        // them looked "below cutoff" to `upgradable()`. Record UNKNOWN instead —
        // the sweep skips it until the file is actually graded (SKADI-T-0399).
        .unwrap_or(skadi_quality::UNKNOWN_QUALITY_ID);

    // One (file, episode) placement's bookkeeping, shared by both paths below.
    // Returns (linked, bad) deltas for this single placement.
    fn tally(out: &mut CommitOutcome, path: &Path, res: Result<MarkOutcome>) -> (bool, bool) {
        match res {
            Ok(MarkOutcome::Placed(Placement::Linked)) => {
                out.imported += 1;
                out.linked += 1;
                (true, false)
            }
            Ok(MarkOutcome::Placed(Placement::InPlace)) => {
                out.imported += 1;
                out.in_place += 1;
                (false, false)
            }
            Ok(MarkOutcome::AdoptedDest) => {
                // The canonical file already on disk was recorded; this source
                // stays put as a duplicate for the operator to deal with.
                out.imported += 1;
                out.in_place += 1;
                out.unmatched.push(format!(
                    "{}: a file already at the canonical path was adopted — this source \
                     was left in place as a duplicate",
                    path.display()
                ));
                (false, true)
            }
            Ok(MarkOutcome::AlreadyImported) => {
                out.skipped += 1;
                (false, false)
            }
            Ok(MarkOutcome::ActiveAcquisition) => {
                out.unmatched.push(format!(
                    "{}: this episode has a download in flight — left alone so the \
                     pipelines don't fight",
                    path.display()
                ));
                (false, true)
            }
            Err(err) => {
                out.errors.push(format!("{}: {err}", path.display()));
                (false, true)
            }
        }
    }

    // Manual override (SKADI-T-0324): the operator mapped this file to an explicit
    // (season, episode). Place it there directly, bypassing the filename parse.
    if let (Some(s), Some(e)) = (item.season, item.episode) {
        let Some(ep) = episodes.iter().find(|x| x.season == s && x.number == e) else {
            out.unmatched.push(format!(
                "{}: S{s:02}E{e:02} is not an episode of this series",
                item.path.display()
            ));
            return (false, true, None);
        };
        let resolution = parse_tv(name).resolution;
        let dest = matcher.dest_for(&item.path, ep, resolution.as_deref());
        let res = place_and_mark(repo, &item.path, dest.clone(), ep.id, quality).await;
        let (l, b) = tally(out, &item.path, res);
        return (l, b, l.then_some(dest));
    }

    // Auto path: the matcher resolves the episode(s) from the filename.
    let parsed = parse_tv(name);
    let matches = matcher.match_file(&parsed, &item.path, completed);
    if matches.is_empty() {
        // Benign: the parsed SxxEyy has no counterpart in this series (custom
        // numbering, specials, a misplaced file). Leave it in place, don't error.
        out.unmatched
            .push(format!("{}: no matching episode", item.path.display()));
        return (false, true, None);
    }

    let mut linked = false;
    let mut bad = false;
    let mut link_dest: Option<PathBuf> = None;
    for m in matches {
        let ep_id = match decode_episode_id(&m.acquirable.0) {
            Ok(id) => id,
            Err(e) => {
                bad = true;
                out.errors.push(format!("{}: {e}", item.path.display()));
                continue;
            }
        };
        let res = place_and_mark(repo, &item.path, m.dest.clone(), ep_id, quality).await;
        let (l, b) = tally(out, &item.path, res);
        if l && link_dest.is_none() {
            link_dest = Some(m.dest.clone());
        }
        linked |= l;
        bad |= b;
    }
    (linked, bad, link_dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::NaiveDate;
    use skadi_core::{ExternalIds, TmdbId};
    use skadi_metadata::{
        EpisodeMeta, ImageKind, ImageRef, MetadataMatch, MetadataQuery, MetadataRecord, SeasonMeta,
        SeriesMetadata,
    };
    use skadi_testsupport::TestDb;

    use crate::episode::Episode;

    fn write_file(path: &Path, bytes: u64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let f = std::fs::File::create(path).unwrap();
        f.set_len(bytes).unwrap();
    }

    #[test]
    fn scan_parses_episode_files_and_skips_samples() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_file(
            &root.join("Game of Thrones/Season 01/Game.of.Thrones.S01E01.1080p.WEB-DL.x265.mkv"),
            12 * 1024 * 1024,
        );
        write_file(
            &root.join("Game of Thrones/Season 01/Game.of.Thrones.S01E02.1080p.WEB-DL.x265.mkv"),
            12 * 1024 * 1024,
        );
        // A named sample + a tiny file must be skipped.
        write_file(
            &root.join("Game of Thrones/Season 01/Game.of.Thrones.S01E03.sample.mkv"),
            12 * 1024 * 1024,
        );
        write_file(&root.join("junk.mkv"), 1024);

        let cands = scan_tv_candidates(root).unwrap();
        assert_eq!(cands.len(), 2, "two real episodes; sample + junk skipped");

        let e1 = cands
            .iter()
            .find(|c| c.episodes == vec![1])
            .expect("S01E01 candidate");
        assert_eq!(e1.series_title.as_deref(), Some("Game of Thrones"));
        assert_eq!(e1.season, Some(1));
        assert_eq!(e1.episodes, vec![1]);

        assert!(
            cands.iter().any(|c| c.episodes == vec![2]),
            "S01E02 present"
        );
    }

    #[test]
    fn scan_errors_on_missing_root() {
        let err = scan_tv_candidates(Path::new("/no/such/dir/here")).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    // --- commit tests ---

    struct FakeProvider {
        meta: SeriesMetadata,
    }

    #[async_trait]
    impl SeriesMetadataProvider for FakeProvider {
        async fn search_series(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
            Ok(vec![])
        }
        async fn lookup_series(&self, _tvdb: TvdbId) -> Result<SeriesMetadata> {
            Ok(self.meta.clone())
        }
    }

    fn got_meta() -> SeriesMetadata {
        SeriesMetadata {
            record: MetadataRecord {
                external_ids: ExternalIds {
                    tvdb: Some(TvdbId(121361)),
                    tmdb: Some(TmdbId(1399)),
                    ..Default::default()
                },
                title: "Game of Thrones".into(),
                release_date: NaiveDate::from_ymd_opt(2011, 4, 17),
                images: vec![ImageRef {
                    kind: ImageKind::Poster,
                    path: "https://x/poster.jpg".into(),
                }],
                ..Default::default()
            },
            status: Some("Ended".into()),
            network: Some("HBO".into()),
            is_anime: false,
            seasons: vec![SeasonMeta {
                number: 1,
                name: Some("Season 1".into()),
                episode_count: 2,
                air_date: NaiveDate::from_ymd_opt(2011, 4, 17),
            }],
            episodes: vec![
                EpisodeMeta {
                    season: 1,
                    number: 1,
                    title: Some("Winter Is Coming".into()),
                    air_date: NaiveDate::from_ymd_opt(2011, 4, 17),
                    ..Default::default()
                },
                EpisodeMeta {
                    season: 1,
                    number: 2,
                    title: Some("The Kingsroad".into()),
                    air_date: NaiveDate::from_ymd_opt(2011, 4, 24),
                    ..Default::default()
                },
            ],
        }
    }

    /// Find the loaded episode for `(season, number)`.
    async fn episode(store: &skadi_store::Store, season: u16, number: u16) -> Episode {
        let s = store
            .get_series_by_tvdb(TvdbId(121361))
            .await
            .unwrap()
            .unwrap();
        s.episodes
            .into_iter()
            .find(|e| e.season == season && e.number == number)
            .unwrap()
    }

    #[tokio::test]
    async fn commit_links_marks_imported_and_drops_source_then_is_idempotent() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = db.store.clone();
        let provider = FakeProvider { meta: got_meta() };

        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        let src = dir
            .path()
            .join("dump/Game.of.Thrones.S01E01.1080p.WEB-DL.x265-GRP.mkv");
        write_file(&src, 12 * 1024 * 1024);

        let out = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src.clone(),
                tvdb_id: 121361,
                quality_id: None,
                season: None,
                episode: None,
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;

        assert_eq!(out.imported, 1, "{:?}", out.errors);
        assert_eq!(out.linked, 1);
        assert_eq!(out.in_place, 0);
        assert_eq!(out.skipped, 0);
        assert!(out.errors.is_empty(), "{:?}", out.errors);

        // (a) The episode became Imported, pointing at a real file under the library.
        let e1 = episode(&store, 1, 1).await;
        let dest = match &e1.status {
            AcquisitionStatus::Imported { file, .. } => file.path.clone(),
            other => panic!("expected Imported, got {other:?}"),
        };
        assert!(dest.exists(), "canonical episode file exists");
        assert!(dest.starts_with(&lib), "placed under the library root");
        // (c) A fresh link is an adoption move — the source was dropped.
        assert!(!src.exists(), "source dropped on Linked");

        // (b) Re-running commit on the already-imported episode skips, not errors.
        let again = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src.clone(),
                tvdb_id: 121361,
                quality_id: None,
                season: None,
                episode: None,
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(again.skipped, 1);
        assert_eq!(again.imported, 0);
        assert!(again.errors.is_empty(), "{:?}", again.errors);
    }

    #[tokio::test]
    async fn commit_manual_override_places_at_explicit_episode() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = db.store.clone();
        let provider = FakeProvider { meta: got_meta() };

        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        // A filename the parser can't resolve to an episode — the operator maps it.
        let src = dir.path().join("dump/random_clip_no_markers.mkv");
        write_file(&src, 12 * 1024 * 1024);

        let out = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src.clone(),
                tvdb_id: 121361,
                quality_id: None,
                season: Some(1),
                episode: Some(2),
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;

        assert_eq!(out.imported, 1, "{:?}", out.errors);
        assert_eq!(out.linked, 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        // The explicitly-picked episode (S01E02) became Imported, file placed.
        let e2 = episode(&store, 1, 2).await;
        let dest = match &e2.status {
            AcquisitionStatus::Imported { file, .. } => file.path.clone(),
            other => panic!("expected Imported, got {other:?}"),
        };
        assert!(dest.exists(), "placed at the picked episode");
        assert!(!src.exists(), "source dropped on Linked");

        // A bad override (episode not in the series) errors, never panics.
        let src2 = dir.path().join("dump/another.mkv");
        write_file(&src2, 12 * 1024 * 1024);
        let bad = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src2,
                tvdb_id: 121361,
                quality_id: None,
                season: Some(9),
                episode: Some(9),
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(bad.imported, 0);
        assert!(bad.errors.is_empty(), "not a hard error");
        assert_eq!(
            bad.unmatched.len(),
            1,
            "out-of-range override is left in place, benign"
        );
    }

    #[tokio::test]
    async fn commit_two_manual_items_same_path_places_both_then_drops_once() {
        // One file, two explicit episodes (two CommitItems sharing the path) — the
        // source must survive until BOTH placements land (SKADI-T-0324 review).
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = db.store.clone();
        let provider = FakeProvider { meta: got_meta() };

        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        let src = dir.path().join("dump/double_bill_no_markers.mkv");
        write_file(&src, 12 * 1024 * 1024);

        let mk = |season, episode| CommitTarget {
            path: src.clone(),
            tvdb_id: 121361,
            quality_id: None,
            season: Some(season),
            episode: Some(episode),
        };
        let out = commit_items(
            &store,
            &provider,
            vec![mk(1, 1), mk(1, 2)],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;

        assert_eq!(out.imported, 2, "{:?}", out.errors);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        for n in [1u16, 2] {
            let ep = episode(&store, 1, n).await;
            let dest = match &ep.status {
                AcquisitionStatus::Imported { file, .. } => file.path.clone(),
                other => panic!("S01E{n:02}: expected Imported, got {other:?}"),
            };
            assert!(dest.exists(), "S01E{n:02} placed");
        }
        assert!(!src.exists(), "source dropped after BOTH placements");
    }

    #[tokio::test]
    async fn commit_failure_leaves_new_series_unarmed() {
        // A commit whose files all fail must not leave a monitored all-Missing
        // series behind (the hunter would download the whole show) — the series is
        // created disarmed and armed only after a successful placement.
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = db.store.clone();
        let provider = FakeProvider { meta: got_meta() };

        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();

        // Source path does not exist → placement fails; nothing lands.
        let out = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: dir.path().join("dump/ghost.mkv"),
                tvdb_id: 121361,
                quality_id: None,
                season: Some(1),
                episode: Some(1),
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(out.imported, 0);

        let s = store
            .get_series_by_tvdb(TvdbId(121361))
            .await
            .unwrap()
            .expect("series was still created (add-on-import)");
        assert!(!s.monitored, "failed import must not arm the hunter");

        // A later successful commit for the same series arms it… only for a NEW
        // series; an existing one keeps its operator-set state. Verify the arm
        // path with a fresh series id instead: re-commit successfully here and
        // confirm the still-existing series stays untouched.
        let src = dir.path().join("dump/real.mkv");
        write_file(&src, 12 * 1024 * 1024);
        let ok = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src,
                tvdb_id: 121361,
                quality_id: None,
                season: Some(1),
                episode: Some(1),
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(ok.imported, 1, "{:?}", ok.errors);
        let s = store
            .get_series_by_tvdb(TvdbId(121361))
            .await
            .unwrap()
            .unwrap();
        assert!(
            !s.monitored,
            "existing series' monitored state is never flipped by a later import"
        );
    }

    #[tokio::test]
    async fn commit_adopts_occupied_dest_and_converges() {
        // A different file already sits at the canonical dest while the DB says
        // the episode is NOT imported (e.g. a prior half-finished import). The
        // commit adopts the dest file (DB now matches disk), leaves the source
        // untouched, and a re-run converges to a clean skip instead of looping
        // on "already exists" forever (SKADI-T-0327).
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = db.store.clone();
        let provider = FakeProvider { meta: got_meta() };

        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        let src = dir.path().join("dump/other_copy_no_markers.mkv");
        write_file(&src, 12 * 1024 * 1024);

        let target = || CommitTarget {
            path: src.clone(),
            tvdb_id: 121361,
            quality_id: None,
            season: Some(1),
            episode: Some(1),
        };
        let profile = ProfileId::new();

        // First commit places normally — use its dest as the "occupier", then
        // reset the episode to Missing to simulate the divergent state.
        let first = commit_items(
            &store,
            &provider,
            vec![target()],
            profile,
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(first.imported, 1, "{:?}", first.errors);
        let e1 = episode(&store, 1, 1).await;
        let dest = match &e1.status {
            AcquisitionStatus::Imported { file, .. } => file.path.clone(),
            other => panic!("expected Imported, got {other:?}"),
        };
        store
            .set_episode_status(e1.id, AcquisitionStatus::Missing)
            .await
            .unwrap();

        // A NEW source file for the same episode: dest occupied + DB Missing.
        let src2 = dir.path().join("dump/second_copy_no_markers.mkv");
        write_file(&src2, 12 * 1024 * 1024);
        let out = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src2.clone(),
                tvdb_id: 121361,
                quality_id: None,
                season: Some(1),
                episode: Some(1),
            }],
            profile,
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(out.imported, 1, "adopted the dest: {:?}", out.errors);
        assert_eq!(out.in_place, 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.unmatched.len(), 1, "duplicate-source note surfaced");
        assert!(src2.exists(), "the duplicate source is left untouched");
        let e1 = episode(&store, 1, 1).await;
        assert!(
            matches!(&e1.status, AcquisitionStatus::Imported { file, .. } if file.path == dest),
            "episode re-recorded as Imported at the existing dest"
        );

        // Convergence: running the same item again is now a clean skip.
        let again = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src2.clone(),
                tvdb_id: 121361,
                quality_id: None,
                season: Some(1),
                episode: Some(1),
            }],
            profile,
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(again.skipped, 1);
        assert!(again.errors.is_empty() && again.unmatched.is_empty());
    }

    #[tokio::test]
    async fn commit_in_place_does_not_drop_source() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = db.store.clone();
        let provider = FakeProvider { meta: got_meta() };

        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).unwrap();
        let src = dir
            .path()
            .join("dump/Game.of.Thrones.S01E01.1080p.WEB-DL.x265-GRP.mkv");
        write_file(&src, 12 * 1024 * 1024);

        // First import → Linked; capture the canonical dest.
        let out = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src,
                tvdb_id: 121361,
                quality_id: None,
                season: None,
                episode: None,
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(out.linked, 1, "{:?}", out.errors);

        let e1 = episode(&store, 1, 1).await;
        let dest = match &e1.status {
            AcquisitionStatus::Imported { file, .. } => file.path.clone(),
            other => panic!("expected Imported, got {other:?}"),
        };
        // Reset to Missing so the idempotency skip doesn't short-circuit the re-commit.
        store
            .set_episode_status(e1.id, AcquisitionStatus::Missing)
            .await
            .unwrap();

        // Re-create a *parseable* source already sharing the canonical inode (what a
        // library scanned through the same mount as the root folder looks like). The
        // canonical kebab filename itself doesn't re-parse, so hardlink the placed file
        // back to a scene-style name and commit that.
        let src2 = dir
            .path()
            .join("dump2/Game.of.Thrones.S01E01.1080p.WEB-DL.x265-GRP.mkv");
        std::fs::create_dir_all(src2.parent().unwrap()).unwrap();
        std::fs::hard_link(&dest, &src2).unwrap();

        // src2 != dest but they share an inode ⇒ InPlace, source untouched.
        let out = commit_items(
            &store,
            &provider,
            vec![CommitTarget {
                path: src2.clone(),
                tvdb_id: 121361,
                quality_id: None,
                season: None,
                episode: None,
            }],
            ProfileId::new(),
            RootFolder::new(&lib),
            SeriesNaming::default(),
        )
        .await;
        assert_eq!(out.imported, 1, "{:?}", out.errors);
        assert_eq!(out.in_place, 1);
        assert_eq!(out.linked, 0);
        assert!(dest.exists(), "an InPlace file is never dropped");
        assert!(src2.exists(), "the same-inode source is never dropped");
    }
}
