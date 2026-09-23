//! C25 `AudiobookMatcher` steps — pure, in-memory.
use std::path::PathBuf;

use cucumber::{given, then, when};

use skadi_audiobooks::{AudiobookMatcher, Book, BookFile, SeriesLink};
use skadi_importer::{AcquirableMatcher, CompletedDownload, FileDisposition};
use skadi_quality::ParsedRelease;

use crate::bdd_support::{Matcher, World};

fn completed(files: &[PathBuf]) -> CompletedDownload {
    CompletedDownload {
        handle: skadi_downloaders::DownloadHandle {
            native_id: "h".into(),
            category: "3030".into(),
        },
        files: files.to_vec(),
        category: "3030".into(),
    }
}

fn book(title: &str, author: &str, asin: &str, series: Option<(&str, &str)>) -> Book {
    let mut b = World::new_book(title, author, asin, true);
    b.series = series.map(|(name, pos)| SeriesLink {
        series_id: skadi_core::BookSeriesId::new(),
        name: name.into(),
        position: Some(pos.into()),
    });
    b
}

#[given(expr = "a matcher for the book {string} by {string} with ASIN {word}")]
async fn matcher_single(w: &mut World, title: String, author: String, asin: String) {
    let b = book(&title, &author, &asin, None);
    let f = BookFile::missing(b.id);
    w.files.insert(asin.clone(), f.clone());
    w.books.insert(asin, b.clone());
    w.matcher = Some(Matcher(AudiobookMatcher::new(b, f)));
}

#[given(
    expr = "a matcher for the book {string} by {string} with ASIN {word} in series {string} at position {string}"
)]
async fn matcher_series(
    w: &mut World,
    title: String,
    author: String,
    asin: String,
    series: String,
    pos: String,
) {
    let b = book(&title, &author, &asin, Some((&series, &pos)));
    let f = BookFile::missing(b.id);
    w.files.insert(asin.clone(), f.clone());
    w.books.insert(asin, b.clone());
    w.matcher = Some(Matcher(AudiobookMatcher::new(b, f)));
}

/// Primary + siblings share one series; entries are `ASIN|Title|position`.
#[given(expr = "a pack matcher over series {string} by {string} with books {string}")]
async fn matcher_pack(w: &mut World, series: String, author: String, books: String) {
    let sid = skadi_core::BookSeriesId::new();
    let mut all: Vec<(Book, BookFile)> = Vec::new();
    for entry in books.split(';') {
        let parts: Vec<&str> = entry.split('|').map(str::trim).collect();
        let (asin, title, pos) = (parts[0], parts[1], parts[2]);
        let mut b = World::new_book(title, &author, asin, true);
        b.series = Some(SeriesLink {
            series_id: sid,
            name: series.clone(),
            position: Some(pos.into()),
        });
        let f = BookFile::missing(b.id);
        w.files.insert(asin.into(), f.clone());
        w.books.insert(asin.into(), b.clone());
        all.push((b, f));
    }
    let (primary_book, primary_file) = all.remove(0);
    w.matcher = Some(Matcher(AudiobookMatcher::with_candidates(
        primary_book,
        primary_file,
        all,
        skadi_audiobooks::naming::AudiobookNaming::default(),
    )));
}

#[given(expr = "the completed download contains {string}")]
async fn download_contains(w: &mut World, files: String) {
    w.download_files = files.split(';').map(|f| PathBuf::from(f.trim())).collect();
}

#[when(expr = "the downloaded file {string} is matched")]
async fn match_file(w: &mut World, file: String) {
    let src = PathBuf::from(file);
    let files = if w.download_files.is_empty() {
        vec![src.clone()]
    } else {
        w.download_files.clone()
    };
    let m = &w.matcher.as_ref().expect("matcher").0;
    let c = completed(&files);
    w.disposition = Some(m.disposition(&ParsedRelease::default(), &src, &c));
    w.matches = m.match_file(&ParsedRelease::default(), &src, &c);
}

#[then("no match is emitted")]
async fn no_match(w: &mut World) {
    assert!(w.matches.is_empty(), "{:?}", w.matches);
}

#[then(expr = "the match routes to the file of {word}")]
async fn routes(w: &mut World, asin: String) {
    assert_eq!(w.matches.len(), 1, "{:?}", w.matches);
    assert_eq!(
        w.matches[0].acquirable,
        w.files.get(&asin).expect("file").acquirable_ref()
    );
}

#[then(expr = "the destination is {string}")]
async fn dest_is(w: &mut World, dest: String) {
    assert_eq!(w.matches[0].dest, PathBuf::from(dest));
}

#[then("the file is ignored")]
async fn ignored(w: &mut World) {
    assert_eq!(w.disposition, Some(FileDisposition::Ignore));
}

#[then("the file is quarantined for review")]
async fn quarantined(w: &mut World) {
    match &w.disposition {
        Some(FileDisposition::Quarantine(dest)) => {
            assert!(dest.to_string_lossy().contains("/_review/"), "{dest:?}");
        }
        other => panic!("expected Quarantine, got {other:?}"),
    }
}
