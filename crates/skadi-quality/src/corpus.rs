//! The release-title regression corpus and its harness.
//!
//! The corpus is the load-bearing artifact of the quality layer (DRAFT): a
//! versioned set of real-world release titles paired with their expected
//! [`ParsedRelease`]. It is built *before* the parser and is the contract the
//! parser is held to — every parser revision must pass it, and behavior changes
//! are deliberate corpus edits.
//!
//! Entries are stored as JSON-lines under `corpus/` (one JSON object per line,
//! blank lines and `#` comments ignored) and embedded at build time. Each entry
//! records its `source`/provenance so we can demonstrate the corpus is authored
//! from public release-naming conventions, never copied from *arr test suites.

use serde::{Deserialize, Serialize};

use crate::parsed::ParsedRelease;

/// The embedded seed corpus (a small starter set; bulk growth is SKADI-T-0013).
const SEED_JSONL: &str = include_str!("../corpus/seed.jsonl");

/// The embedded **audiobook** seed corpus (SKADI-T-0122), tested against
/// [`parse_audiobook`](crate::parser::parse_audiobook).
const AUDIOBOOK_SEED_JSONL: &str = include_str!("../corpus/audiobook_seed.jsonl");

/// The systematic expansion (SKADI-T-0444): one block per parse axis, so a
/// failure names the axis it broke rather than just a title.
const EXPANDED_JSONL: &str = include_str!("../corpus/expanded.jsonl");

/// The television expansion (SKADI-T-0444), tested against
/// [`parse_tv`](crate::parser::parse_tv) rather than `parse` — the movie parser
/// deliberately leaves `SxxExx` in the work title, so running a TV name through
/// it proves nothing about episode identity.
const TV_EXPANDED_JSONL: &str = include_str!("../corpus/tv_expanded.jsonl");

/// Titles the parser gets **wrong** today (SKADI-T-0444, tracked by
/// SKADI-T-0549). Held separately and asserted to keep failing — see
/// [`known_gaps`].
const KNOWN_GAPS_JSONL: &str = include_str!("../corpus/known_gaps.jsonl");

/// Television titles `parse_tv` gets wrong today (SKADI-T-0444).
const TV_KNOWN_GAPS_JSONL: &str = include_str!("../corpus/tv_known_gaps.jsonl");

/// One corpus record: a title, its expected parse, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusEntry {
    pub title: String,
    pub expected: ParsedRelease,
    /// Provenance — how this entry was sourced (public list, scene archive,
    /// authored example). Never `*arr` fixtures.
    pub source: String,
}

/// A single corpus mismatch.
#[derive(Clone, Debug)]
pub struct Failure {
    pub title: String,
    pub expected: ParsedRelease,
    pub actual: ParsedRelease,
}

/// The result of running a parser across the corpus.
#[derive(Debug)]
pub struct Report {
    pub total: usize,
    pub passed: usize,
    pub failures: Vec<Failure>,
}

impl Report {
    /// Whether every entry parsed as expected.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }

    /// A human-readable summary, listing failing titles.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "corpus: {}/{} passed ({} failed)",
            self.passed,
            self.total,
            self.failures.len()
        );
        for f in &self.failures {
            out.push_str(&format!("\n  FAIL: {}", f.title));
        }
        out
    }
}

/// Parse the JSON-lines corpus text into entries (ignoring blank/`#` lines).
pub fn parse_corpus(text: &str) -> Result<Vec<CorpusEntry>, String> {
    let mut entries = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry: CorpusEntry =
            serde_json::from_str(line).map_err(|e| format!("corpus line {}: {e}", i + 1))?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Load the embedded video corpus: the original seed plus the systematic
/// expansion (SKADI-T-0444). One list, because callers want "the contract the
/// parser is held to", not a choice of which half to enforce.
pub fn seed() -> Vec<CorpusEntry> {
    let mut entries = parse_corpus(SEED_JSONL).expect("embedded seed corpus must be valid");
    entries.extend(parse_corpus(EXPANDED_JSONL).expect("embedded expanded corpus must be valid"));
    entries
}

/// The television corpus (SKADI-T-0444). Run against
/// [`parse_tv`](crate::parser::parse_tv).
pub fn tv_seed() -> Vec<CorpusEntry> {
    parse_corpus(TV_EXPANDED_JSONL).expect("embedded tv corpus must be valid")
}

/// Titles the parser is known to get wrong (SKADI-T-0444).
///
/// Deliberately **not** part of [`seed`]: including them would leave the build
/// red, and excluding them entirely would let a known bug rot untested. They are
/// asserted to keep failing instead, so fixing the parser breaks that assertion
/// and tells whoever fixed it to promote these entries into the real corpus.
pub fn known_gaps() -> Vec<CorpusEntry> {
    parse_corpus(KNOWN_GAPS_JSONL).expect("embedded known-gaps corpus must be valid")
}

/// Television titles the parser is known to get wrong (SKADI-T-0444). Held
/// apart from [`known_gaps`] so each corpus is paired with exactly one parser.
pub fn tv_known_gaps() -> Vec<CorpusEntry> {
    parse_corpus(TV_KNOWN_GAPS_JSONL).expect("embedded tv known-gaps corpus must be valid")
}

/// Load the embedded audiobook seed corpus (SKADI-T-0122).
pub fn audiobook_seed() -> Vec<CorpusEntry> {
    parse_corpus(AUDIOBOOK_SEED_JSONL).expect("embedded audiobook corpus must be valid")
}

/// Run `parser` over every entry and collect a [`Report`].
pub fn run<F>(entries: &[CorpusEntry], parser: F) -> Report
where
    F: Fn(&str) -> ParsedRelease,
{
    let mut passed = 0;
    let mut failures = Vec::new();
    for entry in entries {
        let actual = parser(&entry.title);
        if actual == entry.expected {
            passed += 1;
        } else {
            failures.push(Failure {
                title: entry.title.clone(),
                expected: entry.expected.clone(),
                actual,
            });
        }
    }
    Report {
        total: entries.len(),
        passed,
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_corpus_loads_and_is_nonempty() {
        let entries = seed();
        assert!(!entries.is_empty(), "seed corpus should have entries");
        // Every entry records provenance.
        assert!(entries.iter().all(|e| !e.source.trim().is_empty()));
    }

    #[test]
    fn harness_detects_mismatches() {
        let entries = seed();
        // A no-op parser returns empty parses, so every non-empty expected fails.
        let report = run(&entries, |_| ParsedRelease::default());
        assert_eq!(report.total, entries.len());
        assert!(
            report.passed < report.total,
            "a no-op parser must fail real entries — proves comparison works"
        );
        assert!(!report.is_clean());
        assert!(report.summary().contains("passed"));
    }

    #[test]
    fn harness_reports_clean_for_an_oracle_parser() {
        // An "oracle" that returns each title's expected parse passes everything,
        // proving the pass path and that fixtures are self-consistent.
        let entries = seed();
        let by_title: std::collections::HashMap<&str, &ParsedRelease> = entries
            .iter()
            .map(|e| (e.title.as_str(), &e.expected))
            .collect();
        let report = run(&entries, |t| by_title[t].clone());
        assert!(report.is_clean(), "{}", report.summary());
        assert_eq!(report.passed, entries.len());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = "# a comment\n\n{\"title\":\"X\",\"expected\":{},\"source\":\"authored\"}\n";
        let entries = parse_corpus(text).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "X");
    }
}
