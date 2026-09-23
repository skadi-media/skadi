//! Library import (SKADI-T-0074): bring existing on-disk movie files under
//! management — scan a **source** path, group + parse files, and (after the
//! operator confirms metadata matches) **restructure** each into the canonical
//! library layout under the destination root folder and register it as an
//! `Imported` movie/edition.
//!
//! Restructure semantics: read from source, write to destination — even when
//! they're the same tree. A file already at its canonical path is registered
//! as-is (no fs op); otherwise it is **hardlinked** to the canonical path (zero
//! extra disk). A fresh link is an adoption **move**: the dedicated source
//! folder is dropped afterwards (SKADI-T-0303, [`drop_adopted_source`]); an
//! in-place source is never touched. The commit **refuses to copy**: a
//! cross-device source is rejected with an error rather than silently doubling
//! disk usage — scan the library through the same mount as the root folder
//! instead. Companion files (subtitles, nfo, artwork) ride along best-effort —
//! see [`link_companions`].
//!
//! This module owns the filesystem scan/grouping + parse (pure, testable) and
//! the in-place commit; the HTTP surface lives in [`crate::http`].

use std::path::{Path, PathBuf};

use chrono::Utc;
use skadi_core::{
    AcquisitionStatus, AppError, EditionKindId, FileRef, QualityId, Result, RootFolder, TmdbId,
};
use skadi_metadata::MetadataProvider;
use skadi_quality::{QualityDefinition, default_definitions, parse, to_quality};

use crate::THEATRICAL_KIND_ID;
use crate::edition::MovieEdition;
use crate::metadata::{MovieDefaults, refresh_movie};
use crate::movie::Movie;
use crate::repo::MoviesRepo;

/// Recognized video container extensions (lowercased, no dot).
const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "mov", "wmv", "ts", "m2ts", "mpg", "mpeg", "flv", "webm",
];

/// Files smaller than this are ignored as candidates (samples/junk). 50 MiB.
const MIN_VIDEO_BYTES: u64 = 50 * 1024 * 1024;

/// How deep to recurse into a candidate folder looking for the main video file.
const MAX_DEPTH: usize = 4;

/// One scanned candidate item: an existing video file plus what we parsed from
/// its name (and its folder's name, for title/year).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCandidate {
    /// The existing video file path, recorded verbatim on import (in place).
    pub path: PathBuf,
    /// The folder or file stem used as the human/match label.
    pub display_name: String,
    /// Parsed title (best-effort).
    pub title: Option<String>,
    /// Parsed year (best-effort).
    pub year: Option<u16>,
    /// TMDB id embedded in the folder/file name (Radarr-style `{tmdb-12345}`),
    /// when present — lets the match step skip the fuzzy search and look the
    /// movie up exactly.
    pub tmdb_id: Option<u64>,
    /// Detected quality definition id (when the name carried resolution+source).
    pub quality_id: Option<QualityId>,
    /// Detected quality name for display (e.g. `Bluray-1080p`).
    pub quality_name: Option<String>,
    /// Display metadata from `movie.nfo` (Kodi/Jellyfin), when present — lets the
    /// review confirm a match at a glance (SKADI-T-0328, TV parity).
    pub nfo_title: Option<String>,
    pub nfo_year: Option<u16>,
    pub nfo_overview: Option<String>,
    pub nfo_genres: Vec<String>,
    pub nfo_rating: Option<String>,
    pub nfo_studio: Option<String>,
}

/// Extract a TMDB id embedded in a release/folder name, Radarr-style:
/// `{tmdb-12345}`, `{tmdbid-12345}`, `[tmdbid-12345]`, or a bare `tmdb-12345`
/// (case-insensitive, optional `-`/`_`/space/`:` separator). Returns `None` when
/// no `tmdb` marker is present — a bare year like `(1987)` never matches.
pub fn parse_tmdb_id(name: &str) -> Option<u64> {
    static TMDB_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)tmdb(?:id)?[-_: ]?(\d+)").unwrap());
    TMDB_RE
        .captures(name)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Sample/extra files we never treat as the main movie.
fn is_sample(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("sample") || n.contains("trailer") || n.starts_with("rarbg")
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

/// Recursively find the largest non-sample video file under `dir` (bounded).
fn largest_video_in(dir: &Path, depth: usize) -> Option<(PathBuf, u64)> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut best: Option<(PathBuf, u64)> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            if let Some(cand) = largest_video_in(&p, depth + 1)
                && best.as_ref().is_none_or(|(_, b)| cand.1 > *b)
            {
                best = Some(cand);
            }
        } else if meta.is_file() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if is_video(&p) && !is_sample(name) && meta.len() >= MIN_VIDEO_BYTES {
                let len = meta.len();
                if best.as_ref().is_none_or(|(_, b)| len > *b) {
                    best = Some((p.clone(), len));
                }
            }
        }
    }
    best
}

/// How many subdirectories the scan resolves in parallel. `largest_video_in`
/// stats many files per folder over (possibly) NFS, where latency dominates;
/// a small thread pool hides that latency, turning a serial multi-minute walk
/// of a large library into seconds (SKADI-T-0078).
const SCAN_WALK_WORKERS: usize = 16;

/// Everything we lift from a movie's NFO sidecar (Kodi/Jellyfin):
/// `<dir>/movie.nfo` or `<video>.nfo`. The exact TMDB id is authoritative for
/// matching (skips the fuzzy title search, SKADI-T-0324); the rest is display
/// metadata so the review can confirm a match at a glance (SKADI-T-0328).
#[derive(Clone, Default)]
struct MovieNfo {
    tmdb: Option<u64>,
    title: Option<String>,
    year: Option<u16>,
    overview: Option<String>,
    genres: Vec<String>,
    rating: Option<String>,
    studio: Option<String>,
}

/// Read the first useful NFO among `candidates` (in order).
fn read_nfo_from(candidates: &[PathBuf]) -> MovieNfo {
    // NFOs are KBs; skip a mislabeled multi-GB file rather than slurping it
    // (SKADI-T-0327 parity with the TV scan's cap).
    const MAX_NFO_BYTES: u64 = 1024 * 1024;
    use skadi_core::nfo::{tag, tags, uniqueid_u64};
    for nfo in candidates {
        if std::fs::metadata(nfo).map_or(true, |m| m.len() > MAX_NFO_BYTES) {
            continue;
        }
        if let Ok(xml) = std::fs::read_to_string(nfo) {
            let parsed = MovieNfo {
                tmdb: uniqueid_u64(&xml, "tmdb"),
                title: tag(&xml, "title"),
                year: tag(&xml, "year").and_then(|y| y.parse().ok()),
                overview: tag(&xml, "plot").or_else(|| tag(&xml, "outline")),
                genres: tags(&xml, "genre"),
                rating: tag(&xml, "rating"),
                studio: tag(&xml, "studio"),
            };
            // Take the first sidecar that carried anything useful.
            if parsed.tmdb.is_some() || parsed.title.is_some() {
                return parsed;
            }
        }
    }
    MovieNfo::default()
}

/// Resolve one subdirectory into a candidate: its largest non-sample video,
/// title/year from the NFO, then the *folder* name, then the file; quality from
/// the *file* name. `None` when the folder holds no usable video.
fn candidate_for_dir(
    dir: &Path,
    entry_name: &str,
    defs: &[QualityDefinition],
) -> Option<RawCandidate> {
    let (file, _) = largest_video_in(dir, 0)?;
    let folder_parsed = parse(entry_name);
    let file_name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let from_file = parse(file_name);
    let nfo = read_nfo_from(&[dir.join("movie.nfo"), file.with_extension("nfo")]);
    // NFO metadata beats name parsing; the name is the fallback.
    let title = nfo
        .title
        .clone()
        .or(folder_parsed.title)
        .or(from_file.title);
    let year = nfo.year.or(folder_parsed.year).or(from_file.year);
    // Prefer a TMDB id on the folder/file name; fall back to the movie's NFO.
    let tmdb_id = parse_tmdb_id(entry_name)
        .or_else(|| parse_tmdb_id(file_name))
        .or(nfo.tmdb);
    let (quality_id, quality_name) = quality_of(file_name, defs);
    Some(RawCandidate {
        path: file,
        display_name: entry_name.to_string(),
        title,
        year,
        tmdb_id,
        quality_id,
        quality_name,
        nfo_title: nfo.title,
        nfo_year: nfo.year,
        nfo_overview: nfo.overview,
        nfo_genres: nfo.genres,
        nfo_rating: nfo.rating,
        nfo_studio: nfo.studio,
    })
}

/// Build a candidate from a top-level video file parsed wholesale. The caller
/// has already confirmed it's a non-sample video over the size threshold.
fn candidate_for_file(path: &Path, entry_name: &str, defs: &[QualityDefinition]) -> RawCandidate {
    let parsed = parse(entry_name);
    let (quality_id, quality_name) = quality_of(entry_name, defs);
    // A LOOSE file in the scan root only trusts its own `<video>.nfo` sidecar —
    // a `movie.nfo` in the root would mis-tag every loose file with one movie.
    let nfo = read_nfo_from(&[path.with_extension("nfo")]);
    let tmdb_id = parse_tmdb_id(entry_name).or(nfo.tmdb);
    RawCandidate {
        path: path.to_path_buf(),
        display_name: entry_name.to_string(),
        title: parsed.title,
        year: parsed.year,
        tmdb_id,
        quality_id,
        quality_name,
        nfo_title: nfo.title,
        nfo_year: nfo.year,
        nfo_overview: nfo.overview,
        nfo_genres: nfo.genres,
        nfo_rating: nfo.rating,
        nfo_studio: nfo.studio,
    }
}

/// Scan `root` into candidate items (pure: filesystem + parse, no network).
///
/// Each immediate subdirectory becomes one candidate (its largest video file;
/// title/year parsed from the *folder* name, quality from the *file* name). Each
/// top-level video file is a candidate parsed wholesale. Samples, non-video, and
/// tiny files are skipped. Subdirectories are resolved across a bounded thread
/// pool ([`SCAN_WALK_WORKERS`]) so a large library on a high-latency mount scans
/// quickly; the result is sorted for a stable order regardless of completion
/// order. Blocking — call it from `spawn_blocking` in async contexts.
pub fn scan_candidates(root: &Path) -> Result<Vec<RawCandidate>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let defs = default_definitions();
    let mut out = Vec::new();
    let mut dirs: Vec<(PathBuf, String)> = Vec::new();
    let entries = std::fs::read_dir(root)
        .map_err(|e| AppError::Validation(format!("cannot read {}: {e}", root.display())))?;

    for entry in entries.flatten() {
        let p = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let entry_name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if entry_name.starts_with('.') {
            continue;
        }

        if meta.is_dir() {
            // Compute the owned name before moving `p` into `dirs` (the borrow
            // `entry_name` is tied to `p`).
            let name = entry_name.to_string();
            dirs.push((p, name));
        } else if meta.is_file()
            && is_video(&p)
            && !is_sample(entry_name)
            && meta.len() >= MIN_VIDEO_BYTES
        {
            out.push(candidate_for_file(&p, entry_name, &defs));
        }
    }

    // Resolve the (expensive) subdirectory walks concurrently.
    if !dirs.is_empty() {
        let workers = dirs.len().min(SCAN_WALK_WORKERS);
        let next = AtomicUsize::new(0);
        let collected = Mutex::new(Vec::<RawCandidate>::new());
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, name)) = dirs.get(i) else {
                            break;
                        };
                        if let Some(c) = candidate_for_dir(path, name, &defs) {
                            collected.lock().expect("scan collector poisoned").push(c);
                        }
                    }
                });
            }
        });
        out.extend(collected.into_inner().expect("scan collector poisoned"));
    }

    // Stable order for deterministic UIs/tests (independent of walk order).
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(out)
}

/// How a commit placed one file relative to its canonical destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The source already was the canonical path (or the same inode) — no
    /// filesystem operation was performed.
    InPlace,
    /// The source was hardlinked to the canonical path; the original file
    /// remains untouched where it was.
    Linked,
    /// A *different* file already occupied the canonical path: it was
    /// **adopted** (registered as the library file); the source stays where it
    /// is as a duplicate for the operator (SKADI-T-0328 — TV parity; the old
    /// behavior was a hard "destination already exists" error that never
    /// converged).
    AdoptedDest,
}

/// Restructure `src` into the canonical `dest`: hardlink-only, never copy.
///
/// - `src == dest` (or `dest` already exists as the same inode) → [`Placement::InPlace`].
/// - `dest` exists as a *different* file → [`Placement::AdoptedDest`] (never
///   overwritten; the caller registers the dest file and leaves the source).
/// - Cross-device → validation error telling the operator to scan through the
///   same mount as the root folder (a copy would silently double disk usage).
///
/// Blocking — call from `spawn_blocking` in async contexts.
pub fn restructure_into(src: &Path, dest: &Path) -> Result<Placement> {
    // One shared primitive (SKADI-T-0424); this was a byte-identical copy.
    Ok(match skadi_importer::adopt_into(src, dest)? {
        skadi_importer::Adoption::InPlace => Placement::InPlace,
        skadi_importer::Adoption::DestOccupied => Placement::AdoptedDest,
        skadi_importer::Adoption::Linked => Placement::Linked,
    })
}

/// Sidecar subtitle extensions (lowercase, no dot).
const SUBTITLE_EXTS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "vtt"];
/// Artwork extensions (lowercase, no dot).
const ARTWORK_EXTS: &[&str] = &["jpg", "jpeg", "png", "tbn", "webp"];
/// Generic artwork stems media centers recognize; linked verbatim into the
/// movie folder (one level above the edition folder).
const GENERIC_ART_STEMS: &[&str] = &[
    "poster",
    "fanart",
    "folder",
    "cover",
    "banner",
    "clearlogo",
    "logo",
    "disc",
    "landscape",
    "thumb",
    "backdrop",
];
/// Subtitle subdirectories scene releases commonly use.
const SUB_DIRS: &[&str] = &["subs", "subtitles"];

/// Bring companion files (subtitles, nfo, artwork) along with the video into
/// its canonical home — hardlink-only via [`restructure_into`], best-effort
/// (failures are logged and skipped; the movie file is the contract).
///
/// Rules:
/// - Files whose name starts with the video's stem plus a separator
///   (`Movie.2019.en.srt`, `Movie.2019-poster.jpg`, `Movie.2019.nfo`) are
///   always taken and renamed to the canonical stem — safe even in shared
///   dump folders.
/// - When the source folder is *dedicated* (exactly one real video), generic
///   companions are taken too: `movie.nfo` → `<stem>.nfo`; generic artwork
///   (`poster.jpg`, `fanart.jpg`, …) verbatim into the movie folder; loose
///   subtitle files and `Subs/`/`Subtitles/` contents → `<stem>.<orig>.<ext>`.
///
/// Returns the canonical paths placed (linked or already in place). Blocking.
pub fn link_companions(video_src: &Path, video_dest: &Path) -> Vec<PathBuf> {
    let (Some(src_dir), Some(dest_dir)) = (video_src.parent(), video_dest.parent()) else {
        return vec![];
    };
    // Generic artwork lands at the movie-folder level (above the edition dir).
    let movie_dir = dest_dir.parent().unwrap_or(dest_dir);
    let canon_stem = match video_dest.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return vec![],
    };
    let src_stem = match video_src.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return vec![],
    };

    let entries: Vec<_> = match std::fs::read_dir(src_dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return vec![],
    };

    // Dedicated = this folder holds exactly one real (non-sample) video.
    let real_videos = entries
        .iter()
        .filter(|e| {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            is_video(&p)
                && !is_sample(name)
                && e.metadata()
                    .map(|m| m.len() >= MIN_VIDEO_BYTES)
                    .unwrap_or(false)
        })
        .count();
    let dedicated = real_videos == 1;

    let mut out = Vec::new();
    let mut place = |src: &Path, dest: PathBuf| match restructure_into(src, &dest) {
        // AdoptedDest: something else already sits at the companion's dest —
        // that's not OUR link, don't count it.
        Ok(Placement::AdoptedDest) => {}
        Ok(_) => out.push(dest),
        Err(e) => {
            tracing::warn!(src = %src.display(), dest = %dest.display(), error = %e,
                    "skipping companion file");
        }
    };

    for entry in &entries {
        let p = entry.path();
        if p == *video_src || !entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || is_video(&p) {
            continue;
        }
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        // Stem-prefix match: rename the remainder onto the canonical stem.
        let prefix_matched = name.len() > src_stem.len()
            && name
                .get(..src_stem.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(&src_stem))
            && matches!(name.as_bytes()[src_stem.len()], b'.' | b'-' | b'_' | b' ');
        if prefix_matched {
            let rest = &name[src_stem.len()..];
            place(&p, dest_dir.join(format!("{canon_stem}{rest}")));
            continue;
        }

        if !dedicated {
            continue;
        }
        let lower = name.to_lowercase();
        if lower == "movie.nfo" {
            place(&p, dest_dir.join(format!("{canon_stem}.nfo")));
        } else if ARTWORK_EXTS.contains(&ext.as_str())
            && GENERIC_ART_STEMS.contains(&stem.to_lowercase().as_str())
        {
            place(&p, movie_dir.join(name));
        } else if SUBTITLE_EXTS.contains(&ext.as_str()) {
            let tag = crate::naming::sanitize_path_component(&stem);
            place(&p, dest_dir.join(format!("{canon_stem}.{tag}.{ext}")));
        }
    }

    // Subs/ or Subtitles/ one level down (dedicated folders only).
    if dedicated {
        for entry in &entries {
            let p = entry.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !p.is_dir() || !SUB_DIRS.contains(&name.to_lowercase().as_str()) {
                continue;
            }
            let Ok(subs) = std::fs::read_dir(&p) else {
                continue;
            };
            for sub in subs.flatten() {
                let sp = sub.path();
                let ext = sp
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if !sp.is_file() || !SUBTITLE_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                let stem = sp.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                let tag = crate::naming::sanitize_path_component(stem);
                place(&sp, dest_dir.join(format!("{canon_stem}.{tag}.{ext}")));
            }
        }
    }

    out
}

/// After an adoption hardlink succeeds, drop the source so the import is a net
/// **move**, not a copy (SKADI-T-0303). The canonical path shares the source's
/// inode, so no bytes are lost; this just removes the old location.
///
/// - A *dedicated* source folder (exactly one real video, like a scanned release
///   folder) is removed wholesale — its companions go with it — and now-empty
///   ancestor directories are pruned.
/// - A loose video in a *shared* folder removes only that video plus its
///   stem-prefixed companions; the shared folder and unrelated files are kept.
///
/// Best-effort: failures are logged, never fatal (the library file is already
/// safely placed). MUST NOT be called for a completed-**download** import — that
/// source is the still-seeding torrent and has to stay (the acquire path uses
/// `skadi_importer::DefaultImporter`, never `commit_item`, so it never reaches here).
/// Blocking — call from `spawn_blocking`.
pub fn drop_adopted_source(video_src: &Path) {
    let Some(src_dir) = video_src.parent() else {
        return;
    };
    let entries: Vec<_> = match std::fs::read_dir(src_dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return,
    };
    let real_videos = entries
        .iter()
        .filter(|e| {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            is_video(&p)
                && !is_sample(name)
                && e.metadata()
                    .map(|m| m.len() >= MIN_VIDEO_BYTES)
                    .unwrap_or(false)
        })
        .count();

    if real_videos == 1 {
        // Dedicated release folder: take the whole thing.
        if let Err(e) = std::fs::remove_dir_all(src_dir) {
            tracing::warn!(dir = %src_dir.display(), error = %e,
                "adoption: source folder not removed");
            return;
        }
        prune_empty_ancestors(src_dir);
    } else {
        // Shared folder: only this video + its stem-prefixed companions.
        let src_stem = video_src
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if let Err(e) = std::fs::remove_file(video_src) {
            tracing::warn!(file = %video_src.display(), error = %e,
                "adoption: source file not removed");
        }
        for entry in &entries {
            let p = entry.path();
            if p == *video_src || !p.is_file() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let prefix_matched = name.len() > src_stem.len()
                && name
                    .get(..src_stem.len())
                    .is_some_and(|h| h.eq_ignore_ascii_case(&src_stem))
                && matches!(name.as_bytes()[src_stem.len()], b'.' | b'-' | b'_' | b' ');
            if prefix_matched {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Remove now-empty ancestor directories of `start`, walking upward and stopping
/// at the first non-empty (or unreadable) directory. Best-effort — the natural
/// "stop at non-empty" guard keeps it from ever reaching a populated library root.
fn prune_empty_ancestors(start: &Path) {
    let mut dir = start.parent();
    while let Some(d) = dir {
        let empty = std::fs::read_dir(d)
            .map(|mut rd| rd.next().is_none())
            .unwrap_or(false);
        if !empty {
            break; // non-empty or unreadable → stop
        }
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Import one confirmed item: sync metadata for `tmdb`, restructure the file at
/// `path` into the canonical layout under `root_folder` (see
/// [`restructure_into`]), and create the movie + a Theatrical edition in
/// `Imported` status pointing at the **canonical** path.
///
/// Returns `Ok(None)` when a movie with this TMDB id already exists (skip).
pub async fn commit_item(
    repo: &dyn MoviesRepo,
    provider: &dyn MetadataProvider,
    path: PathBuf,
    tmdb: TmdbId,
    profile: skadi_core::ProfileId,
    root_folder: RootFolder,
    quality_id: Option<QualityId>,
) -> Result<Option<(Movie, Placement)>> {
    if repo.get_movie_by_tmdb(tmdb.clone()).await?.is_some() {
        return Ok(None);
    }

    let mut movie = refresh_movie(
        provider,
        tmdb,
        None,
        Some(MovieDefaults {
            profile,
            root_folder,
        }),
    )
    .await?;

    // Restructure into the canonical layout BEFORE any DB write, so a refused
    // placement (cross-device, collision) leaves no half-imported movie row.
    let dest = crate::naming::canonical_movie_path(
        &movie.root_folder.path,
        &movie.title,
        movie.year,
        movie.external_ids.tmdb.as_ref().map(|t| t.0),
        movie.external_ids.imdb.as_ref().map(|i| i.0.as_str()),
        None, // library import registers the Theatrical edition
        &path,
    );
    let placement = {
        let (src, dst) = (path.clone(), dest.clone());
        tokio::task::spawn_blocking(move || {
            let placement = restructure_into(&src, &dst)?;
            // Companions ride along best-effort; the movie file is the contract.
            let companions = link_companions(&src, &dst);
            if !companions.is_empty() {
                tracing::debug!(count = companions.len(), src = %src.display(), "linked companion files");
            }
            // Adoption is a reorg-via-link MOVE (SKADI-T-0303): once the canonical
            // hardlink + companions are in place, drop the source. Only for a fresh
            // link — an InPlace source already *is* the canonical file.
            if placement == Placement::Linked {
                drop_adopted_source(&src);
            }
            Ok::<_, AppError>(placement)
        })
        .await
        .map_err(|e| AppError::Internal(format!("restructure task panicked: {e}")))??
    };

    // Write the Kodi/Jellyfin `.nfo` next to the placed video, best-effort — the
    // library file is the contract, the NFO rides along (SKADI-T-0298).
    {
        let nfo_path = crate::nfo::nfo_path_for(&dest);
        let xml = crate::nfo::movie_nfo_xml(&movie);
        let _ = tokio::task::spawn_blocking(move || std::fs::write(&nfo_path, xml)).await;
    }

    repo.upsert_movie(&movie).await?;

    // Theatrical kind (deterministic id, registry fallback by tag).
    let kind_id = EditionKindId::from(THEATRICAL_KIND_ID);
    let kind_id = if repo.get_edition_kind(kind_id).await?.is_some() {
        kind_id
    } else {
        repo.get_edition_kind_by_tag("Theatrical")
            .await?
            .ok_or_else(|| {
                AppError::Internal("Theatrical edition kind missing from registry".into())
            })?
            .id
    };

    // Quality is required by the Imported status; when undetected record UNKNOWN
    // rather than the ladder's lowest tier. The old SDTV floor was a fabricated
    // fact: 1,354 adopted editions on prod read as "below cutoff", so enabling
    // upgrades re-grabbed the whole library (SKADI-T-0399). Unknown is skipped by
    // `upgradable()` until a probe/backfill grades the file.
    let quality = quality_id.unwrap_or(skadi_quality::UNKNOWN_QUALITY_ID);

    let mut edition = MovieEdition::missing(movie.id, kind_id);
    edition.status = AcquisitionStatus::Imported {
        file: FileRef { path: dest.clone() },
        quality,
        score: 0,
        at: Utc::now(),
    };
    edition.file = Some(FileRef { path: dest });
    edition.quality = Some(quality);
    repo.upsert_edition(&edition).await?;
    movie.editions.push(edition);

    Ok(Some((movie, placement)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, bytes: u64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let f = std::fs::File::create(path).unwrap();
        // Sparse file of the requested size — cheap, and `metadata().len()`
        // reports `bytes` so the size threshold logic is exercised.
        f.set_len(bytes).unwrap();
    }

    #[test]
    fn scan_groups_folders_and_top_level_files_and_skips_samples() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A movie folder with a main file + a sample (sample must be ignored).
        write_file(
            &root.join("The Matrix (1999)/The.Matrix.1999.1080p.BluRay.x264.mkv"),
            60 * 1024 * 1024,
        );
        write_file(&root.join("The Matrix (1999)/sample.mkv"), 10 * 1024 * 1024);
        // A top-level file.
        write_file(
            &root.join("Rogue One 2016 2160p UHD BluRay x265.mkv"),
            60 * 1024 * 1024,
        );
        // A tiny file that should be ignored.
        write_file(&root.join("junk.mkv"), 1024);

        let cands = scan_candidates(root).unwrap();
        assert_eq!(cands.len(), 2, "two real candidates, sample+junk skipped");

        let matrix = cands
            .iter()
            .find(|c| c.display_name.contains("Matrix"))
            .unwrap();
        assert_eq!(matrix.title.as_deref(), Some("The Matrix"));
        assert_eq!(matrix.year, Some(1999));
        assert!(
            matrix
                .path
                .ends_with("The.Matrix.1999.1080p.BluRay.x264.mkv")
        );
        assert_eq!(matrix.quality_name.as_deref(), Some("Bluray-1080p"));

        let rogue = cands
            .iter()
            .find(|c| c.display_name.contains("Rogue"))
            .unwrap();
        assert_eq!(rogue.year, Some(2016));
        assert_eq!(rogue.quality_name.as_deref(), Some("Bluray-2160p"));
    }

    #[test]
    fn scan_errors_on_missing_root() {
        let err = scan_candidates(Path::new("/no/such/dir/here")).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[cfg(unix)]
    fn inode_of(p: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(p).unwrap();
        (m.dev(), m.ino())
    }

    #[test]
    fn restructure_same_path_is_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("Movie (2020)/Movie (2020).mkv");
        write_file(&f, 1024);
        assert_eq!(restructure_into(&f, &f).unwrap(), Placement::InPlace);
    }

    #[test]
    #[cfg(unix)]
    fn restructure_hardlinks_and_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dump/Movie.2020.1080p.mkv");
        let dest = dir.path().join("library/Movie (2020)/Movie (2020).mkv");
        write_file(&src, 4096);

        assert_eq!(restructure_into(&src, &dest).unwrap(), Placement::Linked);
        assert!(src.exists(), "original stays put");
        assert!(dest.exists());
        assert_eq!(inode_of(&src), inode_of(&dest), "hardlink, not copy");

        // Re-running is idempotent: dest exists as the same inode.
        assert_eq!(restructure_into(&src, &dest).unwrap(), Placement::InPlace);
    }

    #[test]
    fn drop_adopted_source_removes_dedicated_folder_and_prunes(/* SKADI-T-0303 */) {
        let dir = tempfile::tempdir().unwrap();
        // A dedicated release folder (one real video + companions) under an
        // otherwise-empty `incoming/`.
        let folder = dir.path().join("incoming/The.Movie.2020.1080p");
        let video = folder.join("The.Movie.2020.1080p.mkv");
        write_file(&video, MIN_VIDEO_BYTES + 1);
        write_file(&folder.join("The.Movie.2020.en.srt"), 10);
        write_file(&folder.join("poster.jpg"), 10);

        drop_adopted_source(&video);

        assert!(!folder.exists(), "dedicated folder removed wholesale");
        assert!(
            !dir.path().join("incoming").exists(),
            "now-empty parent pruned"
        );
    }

    #[test]
    fn drop_adopted_source_shared_dir_keeps_siblings() {
        let dir = tempfile::tempdir().unwrap();
        // A shared dir holding two real videos: only the adopted one + its
        // stem-prefixed companion go; the other movie is untouched.
        let shared = dir.path().join("shared");
        let video = shared.join("Movie.A.2020.mkv");
        let companion = shared.join("Movie.A.2020.en.srt");
        let other = shared.join("Movie.B.2021.mkv");
        write_file(&video, MIN_VIDEO_BYTES + 1);
        write_file(&companion, 10);
        write_file(&other, MIN_VIDEO_BYTES + 1);

        drop_adopted_source(&video);

        assert!(!video.exists(), "adopted video removed");
        assert!(!companion.exists(), "its stem-prefixed companion removed");
        assert!(other.exists(), "the other movie is untouched");
        assert!(
            shared.exists(),
            "shared dir kept (still has the other movie)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn companions_in_dedicated_folder_are_linked_and_renamed() {
        let dir = tempfile::tempdir().unwrap();
        // A dedicated release folder: one real video + companions.
        let src_dir = dir.path().join("dump/The.Matrix.1999.1080p");
        let video = src_dir.join("The.Matrix.1999.1080p.mkv");
        write_file(&video, 60 * 1024 * 1024);
        write_file(&src_dir.join("The.Matrix.1999.1080p.en.srt"), 1024);
        write_file(&src_dir.join("movie.nfo"), 512);
        write_file(&src_dir.join("poster.jpg"), 2048);
        write_file(&src_dir.join("Subs/French.srt"), 1024);
        // Noise that must be left behind.
        write_file(&src_dir.join("RARBG.txt"), 64);

        let dest = dir
            .path()
            .join("movies/The_Matrix_(1999)_{tmdb-603}/Theatrical/The_Matrix_(1999).mkv");
        restructure_into(&video, &dest).unwrap();

        let placed = link_companions(&video, &dest);
        let edition_dir = dest.parent().unwrap();
        let movie_dir = edition_dir.parent().unwrap();
        assert!(edition_dir.join("The_Matrix_(1999).en.srt").exists());
        assert!(edition_dir.join("The_Matrix_(1999).nfo").exists());
        assert!(edition_dir.join("The_Matrix_(1999).French.srt").exists());
        assert!(
            movie_dir.join("poster.jpg").exists(),
            "generic artwork at the movie-folder level"
        );
        assert!(!edition_dir.join("RARBG.txt").exists(), "noise skipped");
        assert_eq!(placed.len(), 4);
        // Hardlinks, originals untouched.
        assert_eq!(
            inode_of(&src_dir.join("The.Matrix.1999.1080p.en.srt")),
            inode_of(&edition_dir.join("The_Matrix_(1999).en.srt"))
        );
        assert!(src_dir.join("movie.nfo").exists());
    }

    #[test]
    #[cfg(unix)]
    fn shared_folder_takes_only_stem_matched_companions() {
        let dir = tempfile::tempdir().unwrap();
        // Two real videos in one folder → NOT dedicated.
        let src_dir = dir.path().join("dump");
        let video = src_dir.join("The.Matrix.1999.mkv");
        write_file(&video, 60 * 1024 * 1024);
        write_file(&src_dir.join("Other.Movie.2001.mkv"), 60 * 1024 * 1024);
        write_file(&src_dir.join("The.Matrix.1999.en.srt"), 1024);
        write_file(&src_dir.join("poster.jpg"), 2048); // ambiguous — must stay

        let dest = dir
            .path()
            .join("movies/The_Matrix_(1999)_{tmdb-603}/Theatrical/The_Matrix_(1999).mkv");
        restructure_into(&video, &dest).unwrap();

        let placed = link_companions(&video, &dest);
        let edition_dir = dest.parent().unwrap();
        let movie_dir = edition_dir.parent().unwrap();
        assert!(edition_dir.join("The_Matrix_(1999).en.srt").exists());
        assert!(
            !movie_dir.join("poster.jpg").exists(),
            "generic artwork not vacuumed from a shared folder"
        );
        assert_eq!(placed.len(), 1);
    }

    #[test]
    fn restructure_adopts_a_different_file_at_dest_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dump/a.mkv");
        let dest = dir.path().join("library/Movie (2020)/Movie (2020).mkv");
        write_file(&src, 1024);
        write_file(&dest, 2048);

        // A different file at the canonical path is adopted, never overwritten
        // (SKADI-T-0328); both files survive.
        assert_eq!(
            restructure_into(&src, &dest).unwrap(),
            Placement::AdoptedDest
        );
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 2048, "untouched");
        assert!(src.exists(), "source left in place");
    }

    #[test]
    fn parses_tmdb_id_from_radarr_style_names() {
        assert_eq!(parse_tmdb_id("The Matrix (1999) {tmdb-603}"), Some(603));
        assert_eq!(parse_tmdb_id("The Matrix (1999) {tmdbid-603}"), Some(603));
        assert_eq!(parse_tmdb_id("The Matrix (1999) [tmdbid-603]"), Some(603));
        assert_eq!(parse_tmdb_id("the.matrix.1999.tmdb-603.mkv"), Some(603));
        assert_eq!(parse_tmdb_id("Movie {TMDB 12345}"), Some(12345));
        // No tmdb marker — a bare year must not be mistaken for an id.
        assert_eq!(parse_tmdb_id("The Matrix (1999)"), None);
        assert_eq!(parse_tmdb_id("-batteries-not-included_(1987)"), None);
    }
}
