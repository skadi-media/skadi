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
    /// audiobook root's `_review/` area, as `<download folder>/<path below it>`.
    ///
    /// It used to keep only the file's parent folder and name. A pack's
    /// `Book 5/CD1/01.mp3` and `Book 7/CD1/01.mp3` then both landed in
    /// `_review/CD1/`, and nothing could tell the books apart afterwards
    /// (SKADI-T-0763).
    fn review_dest(&self, source: &Path, completed: &CompletedDownload) -> PathBuf {
        let root = common_dir(completed);
        let mut dest = Path::new(&self.primary().0.root_folder.path).join("_review");
        if let Some(name) = root.file_name() {
            dest.push(name);
        }
        match source.strip_prefix(&root) {
            Ok(rel) if !rel.as_os_str().is_empty() => dest.push(rel),
            _ => dest.push(source.file_name().unwrap_or_default()),
        }
        dest
    }

    /// Classify one audio file against the candidate books by title-token coverage and
    /// series position, with a confidence gate (quarantine when unsure; ignore clear extras).
    ///
    /// The text is the file's own path below the download folder (SKADI-T-0763),
    /// not its parent folder plus stem: the parent of a flat pack is the pack's
    /// folder, which names the series, so in "Redwall Collection/Doomwyte.m4b"
    /// both *Doomwyte* and the book titled *Redwall* were fully covered and the
    /// file was quarantined as ambiguous. For the same reason a title made only
    /// of the series' own words is the weakest evidence: it wins only when no
    /// other candidate is covered.
    fn classify(&self, source: &Path, completed: &CompletedDownload) -> Class {
        let text = specific_text(source, completed);
        let toks = tokens(&text);
        let pos = parse_position(&text);
        let series_toks: Vec<String> = self
            .primary()
            .0
            .series
            .as_ref()
            .map(|s| tokens(&s.name))
            .unwrap_or_default();

        // (index, coverage, title words found that the series name does not have)
        let mut scored: Vec<(usize, f32, usize)> = self
            .candidates
            .iter()
            .enumerate()
            .map(|(i, (book, _))| {
                let (cov, own) = title_match(&toks, &book.title, &series_toks);
                (i, cov, own)
            })
            .collect();
        scored.sort_by(|a, b| {
            (b.2, b.1)
                .partial_cmp(&(a.2, a.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // A title with no word of its own (the first book, named after the
        // series) is covered by every file that names the series. It counts only
        // when the file names nothing else: "Redwall (Unabridged)" is the book
        // Redwall, "Eulalia! A Tale from Redwall" is not.
        let names_more = {
            let author_toks: Vec<String> = self
                .candidates
                .iter()
                .flat_map(|(b, _)| b.authors.iter().flat_map(|a| tokens(a)))
                .collect();
            toks.iter().any(|t| {
                !series_toks.contains(t)
                    && !author_toks.contains(t)
                    && !t.chars().all(|c| c.is_ascii_digit())
                    && !UNNAMING.contains(&t.as_str())
            })
        };
        let covered: Vec<&(usize, f32, usize)> = scored
            .iter()
            .filter(|x| x.1 >= MIN_TITLE_COVERAGE && (x.2 > 0 || !names_more))
            .collect();
        let bcov = scored.iter().map(|x| x.1).fold(0.0_f32, f32::max);

        let pos_hit = pos.and_then(|p| {
            self.candidates.iter().position(|(b, _)| {
                b.series
                    .as_ref()
                    .and_then(|s| s.position.as_deref())
                    .and_then(parse_f64)
                    == Some(p)
            })
        });

        match covered.as_slice() {
            // One candidate's title is covered: that book.
            [only] => return Class::Book(only.0),
            // Several are: the one with the most words of its own, if it stands alone.
            [first, second, ..] if first.2 > second.2 => return Class::Book(first.0),
            _ => {}
        }
        // Series-position match backed by at least weak title agreement, or by a
        // name that is nothing but structure ("Book 3/01.mp3").
        if let Some(pi) = pos_hit
            && (title_coverage(&toks, &self.candidates[pi].0.title) >= 0.30 || !names_a_work(&text))
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
        let distinct: HashSet<usize> = audio_files(completed)
            .filter_map(|p| match self.classify(p, completed) {
                Class::Book(i) => Some(i),
                _ => None,
            })
            .collect();
        distinct.len() >= 2
    }

    /// How many of the download's audio files confidently classify to candidate `i`.
    fn book_file_count(&self, i: usize, completed: &CompletedDownload) -> usize {
        audio_files(completed)
            .filter(|p| matches!(self.classify(p, completed), Class::Book(j) if j == i))
            .count()
    }

    /// Is this file the run's book by its own path below the download folder?
    fn is_primary_by_name(&self, source: &Path, completed: &CompletedDownload) -> bool {
        let toks = tokens(&specific_text(source, completed));
        let book = &self.primary().0;
        let series_toks = book
            .series
            .as_ref()
            .map(|s| tokens(&s.name))
            .unwrap_or_default();
        title_match(&toks, &book.title, &series_toks).0 >= MIN_TITLE_COVERAGE
    }

    /// A pack of which the library tracks only the run's book (SKADI-T-0763).
    ///
    /// With no sibling among the candidates [`is_pack`](Self::is_pack) is false,
    /// and the single-book path used to quarantine every other book of the pack
    /// into `_review` — 531 files for one grab — or, when the folders were just
    /// `Book 1` … `Book 5`, place all five books as the one that was wanted.
    /// Returns what to do with `source` when the download has that shape, `None`
    /// when it is an ordinary single-book download.
    fn lone_book_in_pack(
        &self,
        source: &Path,
        completed: &CompletedDownload,
    ) -> Option<FileDisposition> {
        let (book, file) = self.primary();

        // By name: some files are this book, others name a different work.
        // Chapter files of one book are often named too ("02 - Prologue: To
        // Kill"), so a name alone is not enough: the download must also have the
        // shape of a pack. Either each top folder is wholly this book or wholly
        // not, or the files sit flat in a folder that is called a pack and each
        // is the size of a book. Getting this wrong the other way would import a
        // book with chapters missing, so anything else falls through.
        let mine = audio_files(completed)
            .filter(|p| self.is_primary_by_name(p, completed))
            .count();
        let others_named = audio_files(completed).any(|p| {
            !self.is_primary_by_name(p, completed) && names_a_work(&specific_text(p, completed))
        });
        if mine > 0 && others_named && (self.split_by_top_folder(completed) || flat_pack(completed))
        {
            return Some(if self.is_primary_by_name(source, completed) {
                FileDisposition::Place(self.place(book, file, source, mine <= 1))
            } else {
                FileDisposition::Ignore
            });
        }

        // By number: every file sits in a top folder that is a numbered book
        // ("Book 1" … "Book 5"), and none carries this book's title.
        let top_pos = |p: &Path| top_component(p, completed).and_then(|t| book_number(&t));
        let positions: Vec<Option<f64>> = audio_files(completed).map(|p| top_pos(p)).collect();
        let distinct = {
            let mut d: Vec<f64> = positions.iter().flatten().copied().collect();
            d.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            d.dedup();
            d.len()
        };
        if mine == 0 && distinct >= 2 && positions.iter().all(Option::is_some) {
            let wanted = book
                .series
                .as_ref()
                .and_then(|s| s.position.as_deref())
                .and_then(parse_f64);
            return Some(match (wanted, top_pos(source)) {
                (Some(w), Some(p)) if w == p => {
                    let n = positions.iter().filter(|x| **x == Some(w)).count();
                    FileDisposition::Place(self.place(book, file, source, n <= 1))
                }
                // Another book of the pack, or a pack whose wanted book has no
                // number to find it by: leave it in the download.
                _ => FileDisposition::Ignore,
            });
        }
        None
    }

    /// Is every file in a top folder, and is each top folder wholly this book
    /// or wholly not, with at least one of each?
    fn split_by_top_folder(&self, completed: &CompletedDownload) -> bool {
        let mut by_top: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        for p in audio_files(completed) {
            let Some(top) = top_component(p, completed) else {
                return false;
            };
            let e = by_top.entry(top).or_default();
            if self.is_primary_by_name(p, completed) {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }
        by_top.values().all(|(m, o)| *m == 0 || *o == 0)
            && by_top.values().any(|(m, _)| *m > 0)
            && by_top.values().any(|(_, o)| *o > 0)
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

        if !self.is_pack(completed) {
            // A pack that holds the run's book among books the library does not
            // track is not a single-book download (SKADI-T-0763).
            if let Some(d) = self.lone_book_in_pack(source, completed) {
                return d;
            }
            // A single-book download (one M4B, or chapter MP3s of one book) places every
            // audio file under the run's primary book — unchanged pre-pack behaviour.
            let (book, file) = self.primary();
            // …but only if the file is plausibly that book (SKADI-T-0445). Readarr
            // refuses a download whose title doesn't match; skadi placed whatever
            // audio arrived, so a mis-grabbed "Project Hail Mary" landed as The Way
            // of Kings. Quarantine instead of ignoring: the bytes are real, they
            // just need a human to say where they go.
            if !self.is_this_book(book, source) {
                return FileDisposition::Quarantine(self.review_dest(source, completed));
            }
            let single_file = completed.files.iter().filter(|p| is_audio(p)).count() <= 1;
            return FileDisposition::Place(self.place(book, file, source, single_file));
        }

        // Multi-book pack: fan THIS file out to the book it belongs to.
        match self.classify(source, completed) {
            Class::Book(i) => {
                let (book, file) = &self.candidates[i];
                let single_file = self.book_file_count(i, completed) <= 1;
                FileDisposition::Place(self.place(book, file, source, single_file))
            }
            // A book we don't track (a pack extra) — leave it in the download.
            Class::Extra => FileDisposition::Ignore,
            // Audio we can't confidently assign — set aside for manual review (SKADI-T-0311).
            Class::Unsure => FileDisposition::Quarantine(self.review_dest(source, completed)),
        }
    }
}

/// A book-sized audio file: below this a file is a chapter, not a book.
const BOOK_SIZED_BYTES: u64 = 50 * 1024 * 1024;

/// Do the audio files sit flat in a folder that is called a pack, each the
/// size of a whole book? Reads file sizes; a file that cannot be read answers
/// no, so an unsure download keeps the single-book behaviour.
fn flat_pack(completed: &CompletedDownload) -> bool {
    let root = common_dir(completed);
    let named_a_pack = root
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(looks_like_pack);
    named_a_pack
        && audio_files(completed).count() >= 2
        && audio_files(completed).all(|p| {
            p.parent() == Some(root.as_path())
                && std::fs::metadata(p).is_ok_and(|m| m.len() >= BOOK_SIZED_BYTES)
        })
}

/// Does a release folder's name say it holds several books?
fn looks_like_pack(name: &str) -> bool {
    let lower = name.to_lowercase();
    const WORDS: &[&str] = &[
        "collection",
        "complete",
        "series",
        "saga",
        "trilogy",
        "omnibus",
        "box set",
        "boxset",
        "books ",
    ];
    WORDS.iter().any(|w| lower.contains(w))
}

/// The book number a folder name states outright: "Book 3", "Bk 3", "Volume 3",
/// "Vol. 3", "#3". A bare leading number is not enough here — `1`, `2` are as
/// often the discs of one book.
fn book_number(name: &str) -> Option<f64> {
    let lower = name.to_lowercase();
    for kw in ["book ", "bk ", "bk", "volume ", "vol ", "vol.", "#"] {
        if let Some(idx) = lower.find(kw) {
            let before_ok = idx == 0 || !lower.as_bytes()[idx - 1].is_ascii_alphabetic();
            let rest = lower[idx + kw.len()..].trim_start_matches([' ', '.', '#', '-', '_']);
            if before_ok && let Some(n) = leading_number(rest) {
                return Some(n);
            }
        }
    }
    None
}

/// The download's audio files that are not promo cuts.
fn audio_files(completed: &CompletedDownload) -> impl Iterator<Item = &PathBuf> {
    completed
        .files
        .iter()
        .filter(|p| is_audio(p) && !is_excerpt(p))
}

/// The folder every audio file of the download sits under: the release folder
/// of a pack, the book's own folder of a single-book download.
fn common_dir(completed: &CompletedDownload) -> PathBuf {
    let mut dirs = audio_files(completed).filter_map(|p| p.parent());
    let Some(first) = dirs.next() else {
        return PathBuf::new();
    };
    let mut common: Vec<_> = first.components().collect();
    for d in dirs {
        let same = common
            .iter()
            .zip(d.components())
            .take_while(|(a, b)| **a == *b)
            .count();
        common.truncate(same);
    }
    common.iter().collect()
}

/// A file's first folder below the download folder, if it has one.
fn top_component(source: &Path, completed: &CompletedDownload) -> Option<String> {
    let root = common_dir(completed);
    let rel = source.strip_prefix(&root).ok()?;
    let mut parts = rel.components();
    let first = parts.next()?;
    // The file itself is not a folder.
    parts.next()?;
    Some(first.as_os_str().to_string_lossy().into_owned())
}

/// The matching text that is the file's own: its folders below the download
/// folder, then its stem. For `Pack/Book 5 - The Empty Grave/CD1/01.mp3` that is
/// "Book 5 - The Empty Grave CD1 01"; for a file directly in the download folder
/// it is the stem alone, so the pack's own name never counts for every file.
fn specific_text(source: &Path, completed: &CompletedDownload) -> String {
    let root = common_dir(completed);
    let rel = source.strip_prefix(&root).unwrap_or(source);
    let mut parts: Vec<String> = rel
        .parent()
        .map(|d| {
            d.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    parts.push(
        source
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string(),
    );
    parts.join(" ")
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

/// Words that say nothing about which book a file is.
const UNNAMING: &[&str] = &[
    "the",
    "and",
    "of",
    "in",
    "on",
    "to",
    "an",
    "for",
    "from",
    "with",
    "by",
    "book",
    "bk",
    "vol",
    "volume",
    "part",
    "disc",
    "disk",
    "cd",
    "novel",
    "tale",
    "story",
    "audiobook",
    "unabridged",
    "abridged",
    "chapter",
    "track",
    "mp3",
    "m4b",
];

/// How well `text_tokens` name `title` within a series: `(coverage, own words found)`.
///
/// A series' books share words — "The Last Kids on Earth and the Zombie Parade"
/// and "… and the Nightmare King" differ in two — so the measure is taken on the
/// title's own words: those that are neither in the series name nor filler.
/// A title with no word of its own (the first book, named after the series) is
/// measured whole and reports no own word, which makes it the weakest match.
fn title_match(text_tokens: &[String], title: &str, series_tokens: &[String]) -> (f32, usize) {
    const FILLER: &[&str] = &[
        "the", "and", "of", "in", "on", "to", "an", "for", "from", "with",
    ];
    let own: Vec<String> = tokens(title)
        .into_iter()
        .filter(|t| !series_tokens.contains(t) && !FILLER.contains(&t.as_str()))
        .collect();
    if own.is_empty() {
        return (title_coverage(text_tokens, title), 0);
    }
    let found = own.iter().filter(|t| text_tokens.contains(t)).count();
    (found as f32 / own.len() as f32, found)
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

    // --- packs seen on the live server (SKADI-T-0763) ---

    fn series_book(sid: skadi_core::BookSeriesId, series: &str, title: &str, pos: &str) -> Book {
        let mut b = Book::new(
            ExternalIds::default(),
            title,
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        b.authors = vec!["An Author".into()];
        b.series = Some(SeriesLink {
            series_id: sid,
            name: series.into(),
            position: Some(pos.into()),
        });
        b
    }

    /// The first book of a series is often named after the series. In a flat
    /// pack every file's folder names the series, so "Redwall" used to tie with
    /// the real title and the file was quarantined.
    #[test]
    fn a_title_made_of_the_series_name_does_not_tie_with_the_real_title() {
        let sid = skadi_core::BookSeriesId::new();
        let books = [
            series_book(sid, "Redwall", "Doomwyte", "20"),
            series_book(sid, "Redwall", "Redwall", "1"),
            series_book(sid, "Redwall", "Outcast of Redwall", "8"),
            series_book(sid, "Redwall", "Mattimeo", "3"),
        ];
        let files: Vec<BookFile> = books.iter().map(|b| BookFile::missing(b.id)).collect();
        let refs: Vec<_> = files.iter().map(BookFile::acquirable_ref).collect();
        let mut pairs: Vec<(Book, BookFile)> = books.into_iter().zip(files).collect();
        let (b0, f0) = pairs.remove(0);
        let matcher = AudiobookMatcher::with_candidates(
            b0,
            f0,
            pairs,
            crate::naming::AudiobookNaming::default(),
        );
        let dir = "/dl/Redwall Collection (Audible) 17 Books - Brian Jacques";
        let srcs = vec![
            PathBuf::from(format!(
                "{dir}/Doomwyte A Novel of Redwall (Unabridged).m4b"
            )),
            PathBuf::from(format!("{dir}/Redwall (Unabridged).m4b")),
            PathBuf::from(format!("{dir}/Outcast of Redwall (Unabridged).m4b")),
            PathBuf::from(format!("{dir}/Brian Jacques - Redwall Bk03 - Mattimeo.m4b")),
            PathBuf::from(format!(
                "{dir}/Eulalia! A Tale from Redwall (Unabridged).m4b"
            )),
        ];
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        for (src, want) in srcs.iter().zip(&refs) {
            assert_eq!(
                placed(&matcher.disposition(&p, src, &c)),
                Some(want),
                "{src:?}"
            );
        }
        // A book the library does not track is not placed as "Redwall".
        assert!(
            placed(&matcher.disposition(&p, &srcs[4], &c)).is_none(),
            "Eulalia! is not a candidate"
        );
    }

    /// A pack with a folder per book: the title is two folders above the file.
    #[test]
    fn a_pack_file_is_matched_by_its_path_below_the_download_folder() {
        let sid = skadi_core::BookSeriesId::new();
        let b1 = series_book(sid, "Lockwood & Co.", "The Screaming Staircase", "1");
        let b5 = series_book(sid, "Lockwood & Co.", "The Empty Grave", "5");
        let (f1, f5) = (BookFile::missing(b1.id), BookFile::missing(b5.id));
        let (r1, r5) = (f1.acquirable_ref(), f5.acquirable_ref());
        let matcher = AudiobookMatcher::with_candidates(
            b5,
            f5,
            vec![(b1, f1)],
            crate::naming::AudiobookNaming::default(),
        );
        let dir = "/dl/Lockwood & Co. Series Book 1-5 - Jonathan Stroud";
        let srcs = vec![
            PathBuf::from(format!("{dir}/Book 1 - The Screaming Staircase/CD1/01.mp3")),
            PathBuf::from(format!("{dir}/Book 1 - The Screaming Staircase/CD2/01.mp3")),
            PathBuf::from(format!("{dir}/Book 5 - The Empty Grave/CD1/01.mp3")),
            PathBuf::from(format!("{dir}/Book 5 - The Empty Grave/CD1/02.mp3")),
        ];
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        assert_eq!(placed(&matcher.disposition(&p, &srcs[0], &c)), Some(&r1));
        assert_eq!(placed(&matcher.disposition(&p, &srcs[1], &c)), Some(&r1));
        assert_eq!(placed(&matcher.disposition(&p, &srcs[2], &c)), Some(&r5));
        assert_eq!(placed(&matcher.disposition(&p, &srcs[3], &c)), Some(&r5));
    }

    /// The library tracks one book of a pack: its folder is placed, the other
    /// books stay in the download — not 531 files in `_review`.
    #[test]
    fn the_only_tracked_book_of_a_folder_per_book_pack_is_placed_and_the_rest_left() {
        let sid = skadi_core::BookSeriesId::new();
        let book = series_book(
            sid,
            "The Last Kids on Earth",
            "The Last Kids on Earth and the Zombie Parade",
            "2",
        );
        let file = BookFile::missing(book.id);
        let want = file.acquirable_ref();
        let matcher = AudiobookMatcher::new(book, file);
        let dir = "/dl/The Last Kids on Earth Series - Books 1 - 8 - Max Brallier";
        let srcs = vec![
            PathBuf::from(format!(
                "{dir}/2 The Last Kids on Earth and the Zombie Parade/01.mp3"
            )),
            PathBuf::from(format!(
                "{dir}/2 The Last Kids on Earth and the Zombie Parade/02.mp3"
            )),
            PathBuf::from(format!(
                "{dir}/3 The Last Kids on Earth and the Nightmare King/01.mp3"
            )),
            PathBuf::from(format!("{dir}/7.5 Quint and Dirk's Hero Quest/01.mp3")),
        ];
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        assert_eq!(placed(&matcher.disposition(&p, &srcs[0], &c)), Some(&want));
        assert_eq!(placed(&matcher.disposition(&p, &srcs[1], &c)), Some(&want));
        assert_eq!(
            matcher.disposition(&p, &srcs[2], &c),
            FileDisposition::Ignore
        );
        assert_eq!(
            matcher.disposition(&p, &srcs[3], &c),
            FileDisposition::Ignore
        );
    }

    /// Folders named only `Book 1` … `Book 5`: the wanted book is found by its
    /// number. All five used to be placed as the one book that was wanted.
    #[test]
    fn a_pack_of_numbered_folders_places_only_the_wanted_number() {
        let sid = skadi_core::BookSeriesId::new();
        let book = series_book(
            sid,
            "Alcatraz",
            "Alcatraz Versus the Knights of Crystallia",
            "3",
        );
        let file = BookFile::missing(book.id);
        let want = file.acquirable_ref();
        let matcher = AudiobookMatcher::new(book, file);
        let dir = "/dl/Alcatraz vs. the Evil Librarians Books 1-5 - Brandon Sanderson";
        let srcs: Vec<PathBuf> = (1..=5)
            .flat_map(|n| (1..=2).map(move |t| PathBuf::from(format!("{dir}/Book {n}/{t:02}.mp3"))))
            .collect();
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        for src in &srcs {
            let d = matcher.disposition(&p, src, &c);
            if src.to_string_lossy().contains("/Book 3/") {
                assert_eq!(placed(&d), Some(&want), "{src:?}");
            } else {
                assert_eq!(d, FileDisposition::Ignore, "{src:?}");
            }
        }
    }

    /// One mp3 per book, flat in a folder called a series: the wanted book is
    /// placed, the others are left. The files must be the size of books.
    #[test]
    fn the_only_tracked_book_of_a_flat_pack_is_placed_when_the_files_are_book_sized() {
        let base = skadi_core::unique_temp_path("ab-flat-pack");
        let dir = base.join("Charlaine Harris Aurora Teagarden Series, Books 1-10");
        std::fs::create_dir_all(&dir).unwrap();
        let srcs: Vec<PathBuf> = [
            "01 Real Murders.mp3",
            "02 A Bone to Pick.mp3",
            "03 Three Bedrooms, One Corpse.mp3",
        ]
        .iter()
        .map(|n| {
            let p = dir.join(n);
            std::fs::File::create(&p)
                .unwrap()
                .set_len(BOOK_SIZED_BYTES + 1)
                .unwrap();
            p
        })
        .collect();
        let sid = skadi_core::BookSeriesId::new();
        let book = series_book(sid, "Aurora Teagarden", "Real Murders", "1");
        let file = BookFile::missing(book.id);
        let want = file.acquirable_ref();
        let matcher = AudiobookMatcher::new(book, file);
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        assert_eq!(placed(&matcher.disposition(&p, &srcs[0], &c)), Some(&want));
        assert_eq!(
            matcher.disposition(&p, &srcs[1], &c),
            FileDisposition::Ignore
        );
        assert_eq!(
            matcher.disposition(&p, &srcs[2], &c),
            FileDisposition::Ignore
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The guard on the rule above: one book whose chapter files have names of
    /// their own, one of them the book's title, is still one book. Reading it
    /// as a pack would import the book with chapters missing.
    #[test]
    fn a_single_book_with_named_chapters_is_not_a_pack() {
        let book = book_with(None);
        let file = BookFile::missing(book.id);
        let want = file.acquirable_ref();
        let matcher = AudiobookMatcher::new(book, file);
        // Small files that do not exist: chapters, in a folder called a series.
        let dir = "/dl/Stormlight Archive Series Book 1 - The Way of Kings";
        let srcs = vec![
            PathBuf::from(format!("{dir}/01 - The Way of Kings - Opening Credits.mp3")),
            PathBuf::from(format!("{dir}/02 - Prelude to the Stormlight Archive.mp3")),
            PathBuf::from(format!("{dir}/03 - Prologue To Kill.mp3")),
        ];
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        for src in &srcs {
            assert_eq!(
                placed(&matcher.disposition(&p, src, &c)),
                Some(&want),
                "{src:?}"
            );
        }
    }

    /// Discs of one book in folders `1`, `2`: a bare number is not a book number.
    #[test]
    fn disc_folders_with_bare_numbers_are_not_numbered_books() {
        let sid = skadi_core::BookSeriesId::new();
        let book = series_book(sid, "Dungeon Crawler Carl", "Dungeon Crawler Carl", "1");
        let file = BookFile::missing(book.id);
        let want = file.acquirable_ref();
        let matcher = AudiobookMatcher::new(book, file);
        let srcs = vec![
            PathBuf::from("/dl/DCC1/1/Track 01.mp3"),
            PathBuf::from("/dl/DCC1/2/Track 01.mp3"),
        ];
        let c = completed(srcs.clone());
        let p = ParsedRelease::default();
        assert_eq!(placed(&matcher.disposition(&p, &srcs[0], &c)), Some(&want));
        assert_eq!(placed(&matcher.disposition(&p, &srcs[1], &c)), Some(&want));
    }

    /// A quarantined file keeps the download folder and its path below it.
    #[test]
    fn a_quarantined_file_keeps_its_path_below_the_download_folder() {
        let (matcher, _refs) = dcc_pack();
        let files = vec![
            PathBuf::from("/dl/DCC Pack/Book 01 - Dungeon Crawler Carl/CD1/01.mp3"),
            PathBuf::from("/dl/DCC Pack/Book 02 - Carl's Doomsday Scenario/CD1/01.mp3"),
            PathBuf::from("/dl/DCC Pack/Extras/Bonus Interview/CD1/01.mp3"),
        ];
        let c = completed(files.clone());
        match matcher.disposition(&ParsedRelease::default(), &files[2], &c) {
            FileDisposition::Quarantine(dest) => assert_eq!(
                dest,
                PathBuf::from("/audiobooks/_review/DCC Pack/Extras/Bonus Interview/CD1/01.mp3")
            ),
            other => panic!("expected Quarantine, got {other:?}"),
        }
    }

    #[test]
    fn book_number_needs_a_keyword() {
        assert_eq!(book_number("Book 3"), Some(3.0));
        assert_eq!(book_number("Bk03 - Mattimeo"), Some(3.0));
        assert_eq!(book_number("Vol. 2"), Some(2.0));
        assert_eq!(book_number("#7.5 Hero Quest"), Some(7.5));
        assert_eq!(book_number("1"), None);
        assert_eq!(book_number("CD 2"), None);
        assert_eq!(book_number("Notebook 4"), None);
    }
}
