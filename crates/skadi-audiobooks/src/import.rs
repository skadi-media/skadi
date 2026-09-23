//! Library import (SKADI-T-0134): bring an existing on-disk audiobook tree under
//! management — scan a **source** path, parse each book into a review payload
//! (author/title/series + an extracted ASIN, if any), and (after the operator
//! confirms an Audible ASIN) **commit** each item by building the [`Book`] from
//! Audnexus and **restructuring** its audio file(s) into the canonical
//! Author/[Series/]Book layout under the destination root folder.
//!
//! Restructure semantics mirror the movies import: read from source, write to
//! destination, **hardlink-only** (cross-device is refused — a copy would double
//! disk usage). A fresh link is an adoption move: the source audio files are
//! dropped afterwards (SKADI-T-0303). An audiobook is
//! EITHER a single audio file (one `.m4b` → renamed to `<Title>.<ext>`) OR a
//! folder of audio files (MP3s → each source file name preserved). Matching is
//! ASIN-driven (Audnexus has no book-title search), so the scan extracts an ASIN
//! from the path and the operator supplies one for anything unmatched.
//!
//! This module owns the filesystem scan/parse (pure, testable) and the commit;
//! the HTTP surface lives in [`crate::http`].

use std::path::{Path, PathBuf};

use chrono::Utc;
use skadi_core::{
    AcquisitionStatus, AppError, AsinId, FileRef, ProfileId, QualityId, Result, RootFolder,
};
use skadi_metadata::MetadataProvider;
use skadi_quality::audiobook::default_audiobook_definitions;
use skadi_quality::parse_audiobook;

use crate::book::Book;
use crate::book_file::BookFile;
use crate::metadata::{BookDefaults, refresh_book};
use crate::repo::AudiobooksRepo;

/// Audio file extensions an audiobook is made of (lowercased, no dot). Mirrors
/// `crate::matcher::AUDIO_EXTS`.
const AUDIO_EXTS: &[&str] = &[
    "m4b", "m4a", "mp3", "aac", "flac", "ogg", "opus", "mp4", "wav",
];

/// Name fragments that mark a promo cut rather than the real audiobook. Mirrors
/// `crate::matcher::EXCERPT_MARKERS`.
const EXCERPT_MARKERS: &[&str] = &["sample", "excerpt", "teaser", "promo", "preview"];

/// How deep to recurse into a candidate folder looking for audio files.
const MAX_DEPTH: usize = 4;

/// One scanned candidate item: an existing audiobook (single file or folder of
/// audio files) plus what we parsed from its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCandidate {
    /// The audio source path(s) for this audiobook. Exactly one entry for a
    /// single-file book; many (the folder's audio files) for a multi-file book.
    /// Recorded verbatim on import (originals are never modified/removed).
    pub files: Vec<PathBuf>,
    /// The folder or file stem used as the human/match label.
    pub display_name: String,
    /// Parsed author (best-effort).
    pub author: Option<String>,
    /// Parsed title (best-effort).
    pub title: Option<String>,
    /// Parsed series name (best-effort).
    pub series: Option<String>,
    /// Parsed series position (best-effort).
    pub series_position: Option<String>,
    /// Audible ASIN extracted from the folder/file name, when present — lets the
    /// match step look the book up exactly via Audnexus.
    pub asin: Option<AsinId>,
    /// Whether this audiobook is a single audio file (renamed to title on
    /// commit) vs. a folder of audio files (source names kept).
    pub single_file: bool,
}

impl RawCandidate {
    /// The representative source path (the first audio file) — the stable key the
    /// UI uses to merge match results back in and the source the commit places.
    #[must_use]
    pub fn key_path(&self) -> &Path {
        // `files` is never empty by construction (a candidate is only emitted
        // when at least one audio file was found).
        &self.files[0]
    }
}

/// Extract an Audible ASIN embedded in a folder/file name, canonical-layout
/// style: `{asin-B08G9PRS1K}`, `[asin-B08G9PRS1K]`, or a bare `asin-B08G9PRS1K`
/// (case-insensitive, optional `-`/`_`/`:`/space separator); OR the common
/// Synology/Audiobookshelf convention of a **bare bracketed** ASIN
/// `Title [B08G9PRS1K]` or `Title [1234567890]`. An Amazon/Audible ASIN is 10
/// alphanumerics — **usually** `B0…`, but all-digit ASINs exist too
/// (SKADI-T-0150), so we match any 10 alphanumerics in an `asin-…` marker or in
/// brackets and let the metadata provider (Audnexus, ASIN-keyed) arbitrate — a
/// non-ASIN (e.g. a stray ISBN-10) simply 404s, the same as before. Returns
/// `None` when no 10-char id marker is present. Mirrors movies' `parse_tmdb_id`.
#[must_use]
pub fn parse_asin(name: &str) -> Option<AsinId> {
    // Explicit marker: `{asin-…}` / `[asin-…]` / `asin: …`.
    static MARKED_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)asin[-_: ]?([0-9a-z]{10})").unwrap());
    // Bare **bracketed** ASIN — the common Synology/Audiobookshelf convention
    // `Title [B0XXXXXXXX]`, now also all-digit `Title [1234567890]`.
    static BRACKETED_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)[\[{(]\s*([0-9a-z]{10})\s*[\]})]").unwrap()
    });
    MARKED_RE
        .captures(name)
        .or_else(|| BRACKETED_RE.captures(name))
        .and_then(|c| c.get(1))
        .map(|m| AsinId(m.as_str().to_uppercase()))
}

/// `true` if `path` has an audiobook audio extension.
fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.as_str()))
}

/// `true` if `name` looks like a promo/sample cut.
fn is_excerpt(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    EXCERPT_MARKERS.iter().any(|m| n.contains(m))
}

/// Recursively collect non-excerpt audio files under `dir` (bounded depth).
fn audio_files_in(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if meta.is_dir() {
            // Skip whole `sample`/`excerpt`/… subdirectories.
            if !is_excerpt(name) {
                audio_files_in(&p, depth + 1, out);
            }
        } else if meta.is_file() && is_audio(&p) && !is_excerpt(name) {
            out.push(p);
        }
    }
}

/// Build a candidate from a folder of audio files (multi-file or a single audio
/// file nested in a dedicated folder). `None` when the folder holds no audio.
fn candidate_for_dir(dir: &Path, entry_name: &str) -> Option<RawCandidate> {
    let mut files = Vec::new();
    audio_files_in(dir, 0, &mut files);
    if files.is_empty() {
        return None;
    }
    files.sort();
    let single_file = files.len() == 1;
    let parsed = parse_audiobook(entry_name);
    Some(RawCandidate {
        author: parsed.author,
        title: parsed.title,
        series: parsed.series,
        series_position: parsed.series_position,
        asin: parse_asin(entry_name),
        display_name: entry_name.to_string(),
        single_file,
        files,
    })
}

/// Build a candidate from a top-level single audio file parsed wholesale.
fn candidate_for_file(path: &Path, entry_name: &str) -> RawCandidate {
    let parsed = parse_audiobook(entry_name);
    RawCandidate {
        author: parsed.author,
        title: parsed.title,
        series: parsed.series,
        series_position: parsed.series_position,
        asin: parse_asin(entry_name),
        display_name: entry_name.to_string(),
        single_file: true,
        files: vec![path.to_path_buf()],
    }
}

/// Scan `root` into candidate audiobooks (pure: filesystem + parse, no network).
///
/// Each immediate subdirectory holding audio becomes one candidate (a folder of
/// MP3s, or a single nested M4B). Each top-level audio file is a single-file
/// candidate parsed wholesale. Excerpts/samples and non-audio files are skipped.
/// The result is sorted by display name for a stable order. Blocking — call from
/// `spawn_blocking` in async contexts.
pub fn scan_candidates(root: &Path) -> Result<Vec<RawCandidate>> {
    let mut out = Vec::new();
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
            if !is_excerpt(entry_name)
                && let Some(c) = candidate_for_dir(&p, entry_name)
            {
                out.push(c);
            }
        } else if meta.is_file() && is_audio(&p) && !is_excerpt(entry_name) {
            out.push(candidate_for_file(&p, entry_name));
        }
    }

    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(out)
}

/// How a commit placed a file relative to its canonical destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The source already was the canonical path (or the same inode) — no
    /// filesystem operation was performed.
    InPlace,
    /// The source was hardlinked to the canonical path; a fresh link is an
    /// adoption move — the source file is dropped afterwards (SKADI-T-0303).
    Linked,
    /// A *different* file already occupied the canonical path: it was adopted
    /// (registered as the library file); the source stays where it is
    /// (SKADI-T-0328, movies/TV parity — was a hard error).
    AdoptedDest,
}

/// Place one source audio file at `dest` — **hardlink-only** (parity with the
/// movies/TV library imports, SKADI-T-0324 review): a fresh link is an adoption
/// move whose source is dropped afterwards, so a silent copy fallback would turn
/// a cross-device import into copy-then-delete (2x disk churn). Cross-device ⇒
/// a validation error telling the operator to scan through the same mount.
/// `dest` already existing as the *same* inode (or `src == dest`) is an
/// in-place no-op; a *different* file at `dest` ⇒ [`Placement::AdoptedDest`]
/// (never overwritten). Blocking.
fn place_one(src: &Path, dest: &Path) -> Result<Placement> {
    // One shared primitive (SKADI-T-0424); this was a byte-identical copy.
    Ok(match skadi_importer::adopt_into(src, dest)? {
        skadi_importer::Adoption::InPlace => Placement::InPlace,
        skadi_importer::Adoption::DestOccupied => Placement::AdoptedDest,
        skadi_importer::Adoption::Linked => Placement::Linked,
    })
}

/// After an adoption hardlink succeeds, drop the source audio files so the import
/// is a net **move**, not a copy (SKADI-T-0303). Each canonical path shares its
/// source's inode, so no bytes are lost; this removes the old audio files and
/// prunes the now-empty source directories. Best-effort — failures are logged,
/// never fatal (the library files are already placed).
///
/// MUST NOT be called for a completed-**download** import — that source is the
/// still-seeding torrent (the acquire path uses `skadi_importer::DefaultImporter`,
/// never `commit_item`). Blocking — call from `spawn_blocking`.
pub fn drop_adopted_sources(files: &[PathBuf]) {
    let mut dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for f in files {
        if let Some(d) = f.parent() {
            dirs.insert(d.to_path_buf());
        }
        if let Err(e) = std::fs::remove_file(f) {
            tracing::warn!(file = %f.display(), error = %e, "adoption: source file not removed");
        }
    }
    // Prune deepest dirs first; each prune walks up while the directory is empty.
    for d in dirs.into_iter().rev() {
        prune_empty_dirs(&d);
    }
}

/// Remove `dir` and its ancestors while they are empty, stopping at the first
/// non-empty (or unreadable) directory. Best-effort.
fn prune_empty_dirs(dir: &Path) {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        let empty = std::fs::read_dir(&d)
            .map(|mut rd| rd.next().is_none())
            .unwrap_or(false);
        if !empty || std::fs::remove_dir(&d).is_err() {
            break;
        }
        cur = d.parent().map(Path::to_path_buf);
    }
}

/// Restructure all of a candidate's audio `files` into the canonical layout under
/// `root` for the resolved `book`, hardlinking each (originals untouched).
///
/// Returns `(primary_dest, placement)` where `primary_dest` is the canonical path
/// of the first file (recorded on the `BookFile`). Blocking. Mirrors movies'
/// "restructure BEFORE the DB write" ordering so a refused placement leaves no
/// half-imported row.
fn restructure_book(
    root: &Path,
    book: &Book,
    files: &[PathBuf],
) -> Result<(PathBuf, Placement, Vec<PathBuf>)> {
    let single_file = files.len() == 1;
    let author = book.authors.first().map(String::as_str);
    let series = book.series.as_ref().map(|s| s.name.as_str());
    let series_position = book.series.as_ref().and_then(|s| s.position.as_deref());
    let asin = book.external_ids.asin.as_ref().map(|a| a.0.as_str());

    let mut primary: Option<(PathBuf, Placement)> = None;
    // Only sources that got a FRESH link are safe to drop: an AdoptedDest
    // source's bytes exist nowhere else, and an exact-path InPlace source *is*
    // the canonical file (SKADI-T-0328 — per-file precision instead of dropping
    // every source off the primary's placement).
    let mut linked_srcs: Vec<PathBuf> = Vec::new();
    for src in files {
        let dest = crate::naming::canonical_audiobook_path(
            root,
            author,
            series,
            series_position,
            &book.title,
            asin,
            src,
            single_file,
        );
        let placement = place_one(src, &dest)?;
        if placement == Placement::Linked {
            linked_srcs.push(src.clone());
        }
        if primary.is_none() {
            primary = Some((dest, placement));
        }
    }
    primary
        .map(|(dest, placement)| (dest, placement, linked_srcs))
        .ok_or_else(|| AppError::Validation("audiobook has no audio files to place".into()))
}

/// Import one confirmed item: build the [`Book`] from Audnexus for `asin`,
/// restructure its audio file(s) into the canonical layout under `root_folder`
/// (hardlink; in-place no-op when already canonical), and persist the book with
/// one `Imported` [`BookFile`] pointing at the **canonical** primary path.
///
/// Returns `Ok(None)` when a book with this ASIN already exists (skip, like
/// movies' duplicate guard). Originals are never modified or removed.
#[allow(clippy::too_many_arguments)]
pub async fn commit_item(
    repo: &dyn AudiobooksRepo,
    provider: &dyn MetadataProvider,
    files: Vec<PathBuf>,
    asin: AsinId,
    profile: ProfileId,
    root_folder: RootFolder,
    quality_id: Option<QualityId>,
) -> Result<Option<(Book, Placement)>> {
    if files.is_empty() {
        return Err(AppError::Validation(
            "commit_item: no audio files for this item".into(),
        ));
    }

    // A book with this ASIN may already exist. Only SKIP when we genuinely
    // already have a real copy (Imported/Cutoff); a book stuck in Failed /
    // Missing / Downloading with a valid local file in hand should ADOPT the
    // file and become Imported, not be silently skipped (SKADI-T-0352).
    if let Some(mut existing) = repo.get_book_by_asin(&asin).await? {
        let already_have = existing.files.iter().any(|f| {
            matches!(
                f.status,
                AcquisitionStatus::Imported { .. } | AcquisitionStatus::Cutoff
            )
        });
        if already_have {
            return Ok(None);
        }
        let placement =
            place_and_mark_imported(repo, &mut existing, files, &root_folder, quality_id).await?;
        return Ok(Some((existing, placement)));
    }

    let mut book = refresh_book(
        repo,
        provider,
        asin,
        None,
        Some(BookDefaults {
            profile,
            root_folder: root_folder.clone(),
        }),
    )
    .await?;

    let placement =
        place_and_mark_imported(repo, &mut book, files, &root_folder, quality_id).await?;
    Ok(Some((book, placement)))
}

/// Restructure `files` into `book`'s canonical layout, persist the book, and
/// mark its file Imported — updating an existing non-imported file row in place
/// (Failed/Missing adoption) or creating one for a fresh book. Shared by the
/// new-book and existing-book paths of [`commit_item`] (SKADI-T-0352).
async fn place_and_mark_imported(
    repo: &dyn AudiobooksRepo,
    book: &mut Book,
    files: Vec<PathBuf>,
    root_folder: &RootFolder,
    quality_id: Option<QualityId>,
) -> Result<Placement> {
    // Restructure into the canonical layout BEFORE any DB write, so a refused
    // placement (collision) leaves no half-imported book row.
    let (primary_dest, placement) = {
        let root = root_folder.path.clone();
        let book = book.clone();
        tokio::task::spawn_blocking(move || {
            let (primary_dest, placement, linked_srcs) = restructure_book(&root, &book, &files)?;
            // Adoption is a reorg-via-link MOVE (SKADI-T-0303): once the canonical
            // hardlinks are in place, drop the sources — but only the ones that
            // actually got a fresh link (per-file precision, SKADI-T-0328).
            if !linked_srcs.is_empty() {
                drop_adopted_sources(&linked_srcs);
            }
            Ok::<_, AppError>((primary_dest, placement))
        })
        .await
        .map_err(|e| AppError::Internal(format!("restructure task panicked: {e}")))??
    };

    repo.upsert_book(book).await?;

    // Quality is required by the Imported status. The imported file's container
    // is ground truth (SKADI-T-0148): reconcile any caller-supplied (title-parsed)
    // quality against the actual file's format, falling back to the lowest
    // built-in definition when undetected so the row is always valid.
    let defs = default_audiobook_definitions();
    let file_fmt = primary_dest
        .extension()
        .and_then(|e| e.to_str())
        .and_then(skadi_quality::AudioFormat::from_token);
    let quality = match file_fmt {
        // Unassessed ⇒ the ladder's explicit Unknown row, not `defs[0]` (which is
        // whatever tier happens to sort first): `upgradable()` special-cases
        // Unknown so an adopted book is not re-grabbed (SKADI-T-0399).
        Some(fmt) => skadi_quality::reconcile_quality_with_format(quality_id, fmt, &defs)
            .or(quality_id)
            .unwrap_or_else(skadi_quality::audiobook::unknown_audiobook_id),
        None => quality_id.unwrap_or_else(skadi_quality::audiobook::unknown_audiobook_id),
    };

    let imported = AcquisitionStatus::Imported {
        file: FileRef {
            path: primary_dest.clone(),
        },
        quality,
        score: 0,
        at: Utc::now(),
    };

    // Adopt onto an existing non-imported file row if there is one (so a Failed
    // book flips to Imported instead of gaining a second file); else create one.
    if let Some(existing_file) = book.files.iter_mut().find(|f| {
        !matches!(
            f.status,
            AcquisitionStatus::Imported { .. } | AcquisitionStatus::Cutoff
        )
    }) {
        existing_file.status = imported;
        existing_file.file = Some(FileRef { path: primary_dest });
        existing_file.quality = Some(quality);
        repo.upsert_book_file(existing_file).await?;
    } else {
        let mut file = BookFile::missing(book.id);
        file.status = imported;
        file.file = Some(FileRef { path: primary_dest });
        file.quality = Some(quality);
        repo.upsert_book_file(&file).await?;
        book.files.push(file);
    }

    Ok(placement)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"audio").unwrap();
    }

    #[test]
    fn parses_asin_from_canonical_and_loose_names() {
        assert_eq!(
            parse_asin("Project Hail Mary {asin-B08G9PRS1K}"),
            Some(AsinId("B08G9PRS1K".into()))
        );
        assert_eq!(
            parse_asin("Project Hail Mary [asin-B08G9PRS1K]"),
            Some(AsinId("B08G9PRS1K".into()))
        );
        assert_eq!(
            parse_asin("project.hail.mary.asin-b08g9prs1k.m4b"),
            Some(AsinId("B08G9PRS1K".into())),
            "case-insensitive, normalized to upper"
        );
        // Bare bracketed Audible ASIN (Synology/Audiobookshelf convention).
        assert_eq!(
            parse_asin("A Game of Thrones [B002UZZ93G]"),
            Some(AsinId("B002UZZ93G".into())),
            "bare bracketed ASIN starting with B"
        );
        // All-digit ASINs exist on Audible too (SKADI-T-0150): a bare bracketed
        // 10-digit id is matched as a candidate ASIN (Audnexus arbitrates).
        assert_eq!(
            parse_asin("A Little Hatred [1478916591]"),
            Some(AsinId("1478916591".into())),
            "bare bracketed all-digit ASIN"
        );
        assert_eq!(
            parse_asin("Some Book {asin-1234567890}"),
            Some(AsinId("1234567890".into())),
            "marked all-digit ASIN"
        );
        // No asin marker — a bare year/10-char token must not match.
        assert_eq!(parse_asin("Project Hail Mary (2021)"), None);
        assert_eq!(parse_asin("Stormlight Archive 1234567890"), None);
    }

    #[test]
    fn scan_single_file_book_at_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Andy Weir - Project Hail Mary {asin-B08G9PRS1K}.m4b"));

        let cands = scan_candidates(root).unwrap();
        assert_eq!(cands.len(), 1);
        let c = &cands[0];
        assert!(c.single_file, "a single top-level file is single_file");
        assert_eq!(c.files.len(), 1);
        assert_eq!(c.asin, Some(AsinId("B08G9PRS1K".into())));
        assert_eq!(c.author.as_deref(), Some("Andy Weir"));
        assert!(c.title.as_deref().unwrap().contains("Project Hail Mary"));
    }

    #[test]
    fn scan_multi_file_folder_book() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let folder = root.join("The Way of Kings {asin-B003ITRL7G}");
        touch(&folder.join("Chapter_01.mp3"));
        touch(&folder.join("Chapter_02.mp3"));
        touch(&folder.join("cover.jpg")); // non-audio, ignored
        touch(&folder.join("Sample.mp3")); // excerpt, ignored

        let cands = scan_candidates(root).unwrap();
        assert_eq!(cands.len(), 1);
        let c = &cands[0];
        assert!(!c.single_file, "a folder of mp3s is multi-file");
        assert_eq!(
            c.files.len(),
            2,
            "two real chapters; cover + sample dropped"
        );
        assert_eq!(c.asin, Some(AsinId("B003ITRL7G".into())));
    }

    #[test]
    fn scan_skips_excerpt_only_folders_and_errors_on_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A folder whose only audio is a sample → no candidate.
        touch(&root.join("Some Promo/teaser.mp3"));
        let cands = scan_candidates(root).unwrap();
        assert!(cands.is_empty());

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
    #[cfg(unix)]
    fn restructure_single_file_renames_to_title_and_hardlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("audiobooks");
        let src = dir.path().join("dump/Andy Weir - Project Hail Mary.m4b");
        touch(&src);

        let mut book = Book::new(
            skadi_core::ExternalIds {
                asin: Some(AsinId("B08G9PRS1K".into())),
                ..Default::default()
            },
            "Project Hail Mary",
            ProfileId::new(),
            RootFolder::new(root.to_string_lossy().into_owned()),
        );
        book.authors = vec!["Andy Weir".into()];

        let (dest, placement, _) =
            restructure_book(&root, &book, std::slice::from_ref(&src)).unwrap();
        assert_eq!(placement, Placement::Linked);
        let expected =
            root.join("andy-weir/project-hail-mary_{asin-B08G9PRS1K}/project-hail-mary.m4b");
        assert_eq!(dest, expected, "single file renamed to title");
        assert!(src.exists(), "original preserved");
        assert_eq!(inode_of(&src), inode_of(&dest), "hardlink, not copy");
    }

    #[test]
    #[cfg(unix)]
    fn restructure_multi_file_keeps_source_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("audiobooks");
        let a = dir.path().join("dump/WoK/Chapter_01.mp3");
        let b = dir.path().join("dump/WoK/Chapter_02.mp3");
        touch(&a);
        touch(&b);

        let mut book = Book::new(
            skadi_core::ExternalIds {
                asin: Some(AsinId("B003".into())),
                ..Default::default()
            },
            "The Way of Kings",
            ProfileId::new(),
            RootFolder::new(root.to_string_lossy().into_owned()),
        );
        book.authors = vec!["Brandon Sanderson".into()];

        let (dest, _, _) = restructure_book(&root, &book, &[a.clone(), b.clone()]).unwrap();
        let book_dir = root.join("brandon-sanderson/the-way-of-kings_{asin-B003}");
        assert_eq!(dest, book_dir.join("Chapter_01.mp3"), "source name kept");
        assert!(book_dir.join("Chapter_02.mp3").exists());
        assert!(a.exists() && b.exists(), "originals preserved");
    }

    #[test]
    fn restructure_same_path_is_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("audiobooks");
        // A source already at its canonical path.
        let f = root.join("andy-weir/project-hail-mary_{asin-B08G9PRS1K}/project-hail-mary.m4b");
        touch(&f);
        let mut book = Book::new(
            skadi_core::ExternalIds {
                asin: Some(AsinId("B08G9PRS1K".into())),
                ..Default::default()
            },
            "Project Hail Mary",
            ProfileId::new(),
            RootFolder::new(root.to_string_lossy().into_owned()),
        );
        book.authors = vec!["Andy Weir".into()];
        let (_, placement, _) = restructure_book(&root, &book, &[f]).unwrap();
        assert_eq!(placement, Placement::InPlace);
    }
}
