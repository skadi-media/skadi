//! `AudiobookMatcher` — `AcquirableMatcher` for the audiobooks domain
//! (SKADI-T-0127).
//!
//! Constructed **per acquire run** with a snapshot of the run's [`Book`] and its
//! single [`BookFile`] acquirable. `match_file` then has everything in memory —
//! no async, no DB, no global state — and is exhaustively unit-tested.
//!
//! Two things make audiobooks different from movies:
//! - The importer hands `match_file` a `ParsedRelease` produced by the **movie**
//!   parser; the matcher ignores it (the target book is already known from the
//!   per-run snapshot — placement just needs the source path).
//! - An audiobook can be a **single M4B** or a **folder of MP3s**; `match_file`
//!   is called once per file, so it detects single-vs-multi from the completed
//!   download and names accordingly (rename to title vs. keep source name).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use skadi_importer::{AcquirableMatch, AcquirableMatcher, CompletedDownload, FileDisposition};
use skadi_quality::ParsedRelease;

/// Minimum title coverage for a file to be accepted as this book
/// (SKADI-T-0445); mirrors the hunter's decide-side threshold.
const MIN_TITLE_COVERAGE: f32 = 0.6;

use crate::book::Book;
use crate::book_file::BookFile;

/// Audio file extensions an audiobook is made of.
const AUDIO_EXTS: &[&str] = &[
    "m4b", "m4a", "mp3", "aac", "flac", "ogg", "opus", "mp4", "wav",
];

/// Name fragments that mark a promo cut rather than the real audiobook.
const EXCERPT_MARKERS: &[&str] = &["sample", "excerpt", "teaser", "promo", "preview"];

/// How a single audio file relates to the candidate books (SKADI-T-0309).
enum Class {
    /// Confidently the candidate at this index.
    Book(usize),
    /// A clearly-numbered book not among our candidates — a pack extra (ignore).
    Extra,
    /// Audio we cannot confidently assign — quarantine for manual review.
    Unsure,
}

/// Domain-supplied matcher built per acquire run (SKADI-T-0127), **pack-aware** per
/// SKADI-T-0309. Holds the run's primary `Book`/`BookFile` plus the other library books in
/// the same series (candidates), so a multi-book pack download fans each file out to the
/// right book instead of dumping them all under one.
pub struct AudiobookMatcher {
    /// `candidates[0]` is the run's primary book + acquirable file (the single-book
    /// fallback); sibling library books in the series follow.
    candidates: Vec<(Book, BookFile)>,
    naming: crate::naming::AudiobookNaming,
}

impl AudiobookMatcher {
    /// Build a single-book matcher (no pack siblings) with the **default** naming templates.
    #[must_use]
    pub fn new(book: Book, file: BookFile) -> Self {
        Self::with_naming(book, file, crate::naming::AudiobookNaming::default())
    }

    /// Build a single-book matcher with explicit (operator-configured) naming (SKADI-T-0228).
    #[must_use]
    pub fn with_naming(book: Book, file: BookFile, naming: crate::naming::AudiobookNaming) -> Self {
        Self::with_candidates(book, file, Vec::new(), naming)
    }

    /// Build a **pack-aware** matcher (SKADI-T-0309): the run's `(book, file)` plus sibling
    /// candidates — other library books in the same series, each with its acquirable file.
    #[must_use]
    pub fn with_candidates(
        book: Book,
        file: BookFile,
        siblings: Vec<(Book, BookFile)>,
        naming: crate::naming::AudiobookNaming,
    ) -> Self {
        let mut candidates = Vec::with_capacity(siblings.len() + 1);
        candidates.push((book, file));
        candidates.extend(siblings);
        Self { candidates, naming }
    }

    /// The run's primary `(book, file)` — the single-book fallback target.
    fn primary(&self) -> &(Book, BookFile) {
        &self.candidates[0]
    }

    /// Is this file plausibly *this* book (SKADI-T-0445)?
    ///
    /// Accepts when the file's folder + stem cover the book's title (the same
    /// measure the hunter uses on the decide side), and — importantly — also when
    /// the name says nothing at all: audiobook downloads are routinely
    /// `WoK/Chapter_01.mp3`, an abbreviation plus track numbering, and the run
    /// only reaches this matcher for a download grabbed *for* this book. Refuses
    /// only when the name clearly names a different work, which is the case the
    /// bug was about (`Andy Weir - Project Hail Mary` placed as The Way of Kings).
    fn is_this_book(&self, book: &Book, source: &Path) -> bool {
        let text = path_text(source);
        let mut wanted = vec![book.title.clone()];
        if let Some(author) = book.authors.first() {
            wanted.push(format!("{} {}", book.title, author));
        }
        if skadi_quality::title_relevance(&wanted, &text).coverage >= MIN_TITLE_COVERAGE {
            return true;
        }
        !names_a_work(&text)
    }

    /// Build the placement(s) for one source file under `book`/`file`.
    fn place(
        &self,
        book: &Book,
        file: &BookFile,
        source: &Path,
        single_file: bool,
    ) -> Vec<AcquirableMatch> {
        let dest = self.naming.path(
            &book.root_folder.path,
            book.authors.first().map(String::as_str),
            book.series.as_ref().map(|s| s.name.as_str()),
            book.series.as_ref().and_then(|s| s.position.as_deref()),
            &book.title,
            book.external_ids.asin.as_ref().map(|a| a.0.as_str()),
            source,
            single_file,
        );
        vec![AcquirableMatch::new(file.acquirable_ref(), dest)]
    }

    /// Where an unplaceable file is set aside for manual review (SKADI-T-0311): under the
    /// audiobook root's `_review/` area, preserving the source's parent folder + file name.
    fn review_dest(&self, source: &Path) -> PathBuf {
        let parent = source
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        Path::new(&self.primary().0.root_folder.path)
            .join("_review")
            .join(parent)
            .join(name)
    }

    /// Classify one audio file against the candidate books by title-token coverage and
    /// series position, with a confidence gate (quarantine when unsure; ignore clear extras).
    fn classify(&self, source: &Path) -> Class {
        let toks = tokens(&path_text(source));
        let pos = parse_position(&path_text(source));

        let mut scored: Vec<(usize, f32)> = self
            .candidates
            .iter()
            .enumerate()
            .map(|(i, (book, _))| (i, title_coverage(&toks, &book.title)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (bi, bcov) = scored[0];
        let runner = scored.get(1).map_or(0.0, |x| x.1);

        let pos_hit = pos.and_then(|p| {
            self.candidates.iter().position(|(b, _)| {
                b.series
                    .as_ref()
                    .and_then(|s| s.position.as_deref())
                    .and_then(parse_f64)
                    == Some(p)
            })
        });

        // Strong, unambiguous title match.
        if bcov >= 0.60 && (bcov - runner) >= 0.20 {
            return Class::Book(bi);
        }
        // Series-position match backed by at least weak title agreement.
        if let Some(pi) = pos_hit
            && title_coverage(&toks, &self.candidates[pi].0.title) >= 0.30
        {
            return Class::Book(pi);
        }
        // A clearly-numbered book matching no candidate position (and no good title) is a pack
        // extra — a book we don't track. Leave it in the download rather than quarantine it.
        if pos.is_some() && pos_hit.is_none() && bcov < 0.40 {
            return Class::Extra;
        }
        Class::Unsure
    }

    /// Does this download span ≥2 distinct candidate books (a real multi-book pack)?
    fn is_pack(&self, completed: &CompletedDownload) -> bool {
        let distinct: HashSet<usize> = completed
            .files
            .iter()
            .filter(|p| is_audio(p) && !is_excerpt(p))
            .filter_map(|p| match self.classify(p) {
                Class::Book(i) => Some(i),
                _ => None,
            })
            .collect();
        distinct.len() >= 2
    }

    /// How many of the download's audio files confidently classify to candidate `i`.
    fn book_file_count(&self, i: usize, completed: &CompletedDownload) -> usize {
        completed
            .files
            .iter()
            .filter(|p| is_audio(p) && !is_excerpt(p))
            .filter(|p| matches!(self.classify(p), Class::Book(j) if j == i))
            .count()
    }
}

impl AcquirableMatcher for AudiobookMatcher {
    fn match_file(
        &self,
        parsed: &ParsedRelease,
        source: &Path,
        completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        match self.disposition(parsed, source, completed) {
            FileDisposition::Place(m) => m,
            FileDisposition::Quarantine(_) | FileDisposition::Ignore => vec![],
        }
    }

    fn disposition(
        &self,
        _parsed: &ParsedRelease,
        source: &Path,
        completed: &CompletedDownload,
    ) -> FileDisposition {
        // Only audio files; never cover art / nfo / promo cuts.
        if !is_audio(source) || is_excerpt(source) {
            return FileDisposition::Ignore;
        }

        // A single-book download (one M4B, or chapter MP3s of one book) places every audio
        // file under the run's primary book — unchanged pre-pack behaviour (no regression).
        if !self.is_pack(completed) {
            let (book, file) = self.primary();
            // …but only if the file is plausibly that book (SKADI-T-0445). Readarr
            // refuses a download whose title doesn't match; skadi placed whatever
            // audio arrived, so a mis-grabbed "Project Hail Mary" landed as The Way
            // of Kings. Quarantine instead of ignoring: the bytes are real, they
            // just need a human to say where they go.
            if !self.is_this_book(book, source) {
                return FileDisposition::Quarantine(self.review_dest(source));
            }
            let single_file = completed.files.iter().filter(|p| is_audio(p)).count() <= 1;
            return FileDisposition::Place(self.place(book, file, source, single_file));
        }

        // Multi-book pack: fan THIS file out to the book it belongs to.
        match self.classify(source) {
            Class::Book(i) => {
                let (book, file) = &self.candidates[i];
                let single_file = self.book_file_count(i, completed) <= 1;
                FileDisposition::Place(self.place(book, file, source, single_file))
            }
            // A book we don't track (a pack extra) — leave it in the download.
            Class::Extra => FileDisposition::Ignore,
            // Audio we can't confidently assign — set aside for manual review (SKADI-T-0311).
            Class::Unsure => FileDisposition::Quarantine(self.review_dest(source)),
        }
    }
}

/// The matching text for a file: its parent folder name + file stem. Folder-per-book packs
/// carry the book identity on the folder; flat packs carry it on the filename.
fn path_text(source: &Path) -> String {
    let parent = source
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let stem = source.file_stem().and_then(|n| n.to_str()).unwrap_or("");
    format!("{parent} {stem}")
}

/// Significant lowercase alphanumeric tokens (length ≥ 2).
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// Fraction of `title`'s tokens present in `text_tokens` (0..=1).
fn title_coverage(text_tokens: &[String], title: &str) -> f32 {
    let tt = tokens(title);
    if tt.is_empty() {
        return 0.0;
    }
    let present = tt.iter().filter(|t| text_tokens.contains(t)).count();
    present as f32 / tt.len() as f32
}

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Parse a book/volume position from file text: "Book 3", "Vol. 2", "#4", or a leading
/// "03 - …" / "03_…". `None` if no position is evident.
fn parse_position(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    for kw in ["book ", "volume ", "vol ", "vol.", "#"] {
        if let Some(idx) = lower.find(kw) {
            let rest = lower[idx + kw.len()..].trim_start_matches([' ', '.', '#', '-', '_']);
            if let Some(n) = leading_number(rest) {
                return Some(n);
            }
        }
    }
    leading_number(lower.trim_start())
}

fn leading_number(s: &str) -> Option<f64> {
    let num: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if num.is_empty() {
        None
    } else {
        num.parse::<f64>().ok()
    }
}

// --- helpers ---

/// `true` if `source` has an audiobook audio extension.
fn is_audio(source: &Path) -> bool {
    source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.as_str()))
}

/// `true` if `source` looks like a promo/sample cut (by name or a `sample`
/// directory). Size-based rejection is deliberately omitted: a real low-bitrate
/// chapter can be tiny, so dropping small files would lose legitimate audio.
fn is_excerpt(source: &Path) -> bool {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if EXCERPT_MARKERS.iter().any(|m| name.contains(m)) {
        return true;
    }
    source.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| EXCERPT_MARKERS.iter().any(|m| s.eq_ignore_ascii_case(m)))
    })
}

/// Does this file text actually *name* a work, or is it just structural noise?
///
/// `WoK/Chapter_01.mp3` and `DCC1/01 - Track.mp3` carry no title — an
/// abbreviation and numbering — so they are no evidence either way and the run's
/// book stands. `Andy Weir - Project Hail Mary.m4b` names a work, so a title
/// mismatch there is real (SKADI-T-0445). Structural words and pure numbers are
/// dropped; two or more remaining word tokens count as a name.
fn names_a_work(text: &str) -> bool {
    const STRUCTURAL: &[&str] = &[
        "chapter",
        "chapters",
        "track",
        "tracks",
        "part",
        "parts",
        "disc",
        "disk",
        "cd",
        "book",
        "audiobook",
        "unabridged",
        "abridged",
        "prologue",
        "epilogue",
        "intro",
        "outro",
        "credits",
        "opening",
        "ending",
        "side",
        "vol",
        "volume",
        "file",
        "final",
        "the",
        "and",
        "of",
        "a",
        "an",
    ];
    tokens(text)
        .iter()
        .filter(|t| {
            t.len() > 1
                && !t.chars().all(|c| c.is_ascii_digit())
                && !STRUCTURAL.contains(&t.as_str())
        })
        .count()
        >= 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use skadi_core::{AsinId, ExternalIds, ProfileId, RootFolder};
    use skadi_downloaders::DownloadHandle;

    use crate::author::SeriesLink;
    use crate::book::Book;
    use crate::book_file::BookFile;

    fn book_with(series: Option<SeriesLink>) -> Book {
        let mut b = Book::new(
            ExternalIds {
                asin: Some(AsinId("B003".into())),
                ..Default::default()
            },
            "The Way of Kings",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        b.authors = vec!["Brandon Sanderson".into()];
        b.series = series;
        b
    }

    fn completed(files: Vec<PathBuf>) -> CompletedDownload {
        CompletedDownload {
            handle: DownloadHandle {
                native_id: "h".into(),
                category: "3030".into(),
            },
            files,
            category: "3030".into(),
        }
    }

    #[test]
    fn single_m4b_renames_to_title_under_author_folder() {
        let book = book_with(None);
        let file = BookFile::missing(book.id);
        let want_ref = file.acquirable_ref();
        let matcher = AudiobookMatcher::new(book, file);

        let src = PathBuf::from("/dl/Brandon Sanderson - The Way of Kings.m4b");
        let out = matcher.match_file(
            &ParsedRelease::default(),
            &src,
            &completed(vec![src.clone()]),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].acquirable, want_ref);
        assert_eq!(
            out[0].dest,
            PathBuf::from(
                "/audiobooks/brandon-sanderson/the-way-of-kings_{asin-B003}/the-way-of-kings.m4b"
            )
        );
    }

    #[test]
    fn multi_file_mp3_keeps_names_and_uses_series_folder() {
        let book = book_with(Some(SeriesLink {
            series_id: skadi_core::BookSeriesId::new(),
            name: "Stormlight Archive".into(),
            position: Some("1".into()),
        }));
        let file = BookFile::missing(book.id);
        let matcher = AudiobookMatcher::new(book, file);

        let files = vec![
            PathBuf::from("/dl/WoK/Chapter_01.mp3"),
            PathBuf::from("/dl/WoK/Chapter_02.mp3"),
        ];
        let out = matcher.match_file(
            &ParsedRelease::default(),
            &files[0],
            &completed(files.clone()),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].dest,
            PathBuf::from(
                "/audiobooks/brandon-sanderson/stormlight-archive/1_-_the-way-of-kings_{asin-B003}/Chapter_01.mp3"
            )
        );
    }

    #[test]
    fn excerpts_and_non_audio_are_rejected() {
        let book = book_with(None);
        let file = BookFile::missing(book.id);
        let matcher = AudiobookMatcher::new(book, file);
        let c = completed(vec![]);

        // Promo cut by name.
        assert!(
            matcher
                .match_file(
                    &ParsedRelease::default(),
                    Path::new("/dl/The Way of Kings - Sample.mp3"),
                    &c
                )
                .is_empty()
        );
        // `sample` directory.
        assert!(
            matcher
                .match_file(
                    &ParsedRelease::default(),
                    Path::new("/dl/Sample/ch1.mp3"),
                    &c
                )
                .is_empty()
        );
        // Non-audio (cover art, nfo).
        assert!(
            matcher
                .match_file(&ParsedRelease::default(), Path::new("/dl/cover.jpg"), &c)
                .is_empty()
        );
        assert!(
            matcher
                .match_file(&ParsedRelease::default(), Path::new("/dl/info.nfo"), &c)
                .is_empty()
        );
    }

    // --- pack-aware (SKADI-T-0309) ---

    fn dcc(sid: skadi_core::BookSeriesId, title: &str, pos: &str) -> Book {
        let mut b = Book::new(
            ExternalIds::default(),
            title,
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        b.authors = vec!["Matt Dinniman".into()];
        b.series = Some(SeriesLink {
            series_id: sid,
            name: "Dungeon Crawler Carl".into(),
            position: Some(pos.into()),
        });
        b
    }

    fn placed(d: &FileDisposition) -> Option<&skadi_importer::AcquirableRef> {
        match d {
            FileDisposition::Place(m) => Some(&m[0].acquirable),
            _ => None,
        }
    }

    /// Build a 3-book DCC pack matcher; returns `(matcher, [ref1, ref2, ref3])`.
    fn dcc_pack() -> (AudiobookMatcher, [skadi_importer::AcquirableRef; 3]) {
        let sid = skadi_core::BookSeriesId::new();
        let b1 = dcc(sid, "Dungeon Crawler Carl", "1");
        let b2 = dcc(sid, "Carl's Doomsday Scenario", "2");
        let b3 = dcc(sid, "The Dungeon Anarchist's Cookbook", "3");
        let (f1, f2, f3) = (
            BookFile::missing(b1.id),
            BookFile::missing(b2.id),
            BookFile::missing(b3.id),
        );
        let refs = [
            f1.acquirable_ref(),
            f2.acquirable_ref(),
            f3.acquirable_ref(),
        ];
        let matcher = AudiobookMatcher::with_candidates(
            b1,
            f1,
            vec![(b2, f2), (b3, f3)],
            crate::naming::AudiobookNaming::default(),
        );
        (matcher, refs)
    }

    #[test]
    fn series_pack_fans_each_file_to_its_book() {
        let (matcher, refs) = dcc_pack();
        let files = vec![
            PathBuf::from("/dl/DCC/Book 01 - Dungeon Crawler Carl.m4b"),
            PathBuf::from("/dl/DCC/Book 02 - Carl's Doomsday Scenario.m4b"),
            PathBuf::from("/dl/DCC/Book 03 - The Dungeon Anarchist's Cookbook.m4b"),
        ];
        let c = completed(files.clone());
        let p = ParsedRelease::default();
        assert_eq!(
            placed(&matcher.disposition(&p, &files[0], &c)),
            Some(&refs[0])
        );
        assert_eq!(
            placed(&matcher.disposition(&p, &files[1], &c)),
            Some(&refs[1])
        );
        assert_eq!(
            placed(&matcher.disposition(&p, &files[2], &c)),
            Some(&refs[2])
        );
    }

    #[test]
    fn pack_ignores_untracked_extra_and_quarantines_ambiguous() {
        let (matcher, _refs) = dcc_pack();
        let files = vec![
            PathBuf::from("/dl/DCC/Book 01 - Dungeon Crawler Carl.m4b"),
            PathBuf::from("/dl/DCC/Book 02 - Carl's Doomsday Scenario.m4b"),
            // Book 4 is not in the library → a pack extra, left in the download.
            PathBuf::from("/dl/DCC/Book 04 - The Gate of the Feral Gods.m4b"),
            // Unplaceable audio → quarantined for manual review.
            PathBuf::from("/dl/DCC/Bonus Interview.m4b"),
        ];
        let c = completed(files.clone());
        let p = ParsedRelease::default();
        assert_eq!(
            matcher.disposition(&p, &files[2], &c),
            FileDisposition::Ignore
        );
        match matcher.disposition(&p, &files[3], &c) {
            FileDisposition::Quarantine(dest) => {
                assert!(
                    dest.to_str().unwrap().contains("/_review/"),
                    "quarantine dest should be under the review area, got {dest:?}"
                );
            }
            other => panic!("expected Quarantine, got {other:?}"),
        }
    }

    #[test]
    fn single_book_in_a_known_series_is_not_treated_as_a_pack() {
        // One book, chapter MP3s — even though it's in a series, only one candidate book is
        // present, so it must keep the prior single-book behaviour (all under that book).
        let sid = skadi_core::BookSeriesId::new();
        let b1 = dcc(sid, "Dungeon Crawler Carl", "1");
        let f1 = BookFile::missing(b1.id);
        let r1 = f1.acquirable_ref();
        let matcher = AudiobookMatcher::with_candidates(
            b1,
            f1,
            vec![],
            crate::naming::AudiobookNaming::default(),
        );
        let files = vec![
            PathBuf::from("/dl/DCC1/Chapter_01.mp3"),
            PathBuf::from("/dl/DCC1/Chapter_02.mp3"),
        ];
        let c = completed(files.clone());
        let p = ParsedRelease::default();
        assert_eq!(placed(&matcher.disposition(&p, &files[0], &c)), Some(&r1));
        assert_eq!(placed(&matcher.disposition(&p, &files[1], &c)), Some(&r1));
    }
}
