//! Corpus ingestion / coverage tool (developer utility — not shipped).
//!
//! Reads a locally-sourced, tab-separated dump of parser test cases
//! (`<fixture>\t<method>\t<args>`) and uses it to:
//!   1. report how many real-world release names our parser handles, by category, and
//!   2. optionally emit corpus candidates (`--emit`) as JSON-lines, where the
//!      `expected` parse is computed by *our* parser (to be human-audited before
//!      being committed to the regression corpus).
//!
//! Only the release-name input strings are used — they are factual identifiers.
//! The upstream `expected`-value annotations are ignored; our expected parses
//! are our own. The input dump is git-ignored and never committed.
//!
//! Usage:
//!   cargo run -p skadi-quality --example ingest_corpus -- parser-test-cases.txt
//!   cargo run -p skadi-quality --example ingest_corpus -- parser-test-cases.txt --emit movies

use std::collections::BTreeMap;
use std::path::Path;

use skadi_quality::{ParsedRelease, parse};

/// Fixtures whose cases are relevant to the v0 movie domain.
const MOVIE_FIXTURES: &[&str] = &[
    "radarr/ParserFixture.cs",
    "radarr/QualityParserFixture.cs",
    "radarr/EditionParserFixture.cs",
    "radarr/ReleaseGroupParserFixture.cs",
    "radarr/LanguageParserFixture.cs",
    "radarr/CrapParserFixture.cs",
];

/// Pull the first double-quoted substring out of a test-case args column.
fn first_quoted(args: &str) -> Option<String> {
    let start = args.find('"')? + 1;
    let rest = &args[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Does this parse carry enough signal to count as "parsed"?
fn looks_parsed(p: &ParsedRelease) -> bool {
    p.title.is_some() && (p.year.is_some() || p.resolution.is_some() || p.source.is_some())
}

/// Category-aware success: judge each fixture on what it actually tests, so the
/// coverage number reflects the relevant field rather than a generic heuristic.
fn category_ok(fixture: &str, p: &ParsedRelease) -> bool {
    if fixture.contains("CrapParser") {
        // Crap must NOT produce a confident parse.
        !looks_parsed(p)
    } else if fixture.contains("ReleaseGroup") {
        p.group.is_some()
    } else if fixture.contains("QualityParser") {
        p.resolution.is_some() || p.source.is_some()
    } else if fixture.contains("Edition") {
        p.edition.is_some()
    } else if fixture.contains("Language") {
        !p.languages.is_empty()
    } else {
        looks_parsed(p)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: ingest_corpus <file> [--emit movies]");
        std::process::exit(2);
    };
    let emit = args.iter().any(|a| a == "--emit");
    let show_failures = args.iter().any(|a| a == "--failures");

    let text = std::fs::read_to_string(Path::new(path)).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(1);
    });

    // category -> (total, parsed)
    let mut stats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut grand_total = 0usize;
    let mut grand_parsed = 0usize;

    for line in text.lines() {
        let mut cols = line.splitn(3, '\t');
        let (Some(fixture), method, Some(raw_args)) = (cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        if !MOVIE_FIXTURES.contains(&fixture) {
            continue;
        }
        let Some(title) = first_quoted(raw_args) else {
            continue;
        };
        if title.trim().is_empty() {
            continue;
        }

        let is_crap = fixture.contains("CrapParser");
        let parsed = parse(&title);
        let ok = category_ok(fixture, &parsed);

        let entry = stats.entry(fixture.to_string()).or_default();
        entry.0 += 1;
        if ok {
            entry.1 += 1;
        }
        grand_total += 1;
        if ok {
            grand_parsed += 1;
        }

        if show_failures && !ok {
            eprintln!("FAIL [{fixture}] {title}");
        }

        if emit && !is_crap && looks_parsed(&parsed) {
            let candidate = serde_json::json!({
                "title": title,
                "expected": parsed,
                "source": format!("ingested input from local dump ({}, {}); expected derived from skadi parser, pending audit", fixture, method.unwrap_or("")),
            });
            println!("{candidate}");
        }
    }

    if !emit {
        eprintln!("== movie-relevant parser coverage ==");
        for (fixture, (total, ok)) in &stats {
            let pct = if *total > 0 { ok * 100 / total } else { 0 };
            eprintln!("  {fixture:<40} {ok:>4}/{total:<4} ({pct}%)");
        }
        eprintln!(
            "  {:<40} {grand_parsed:>4}/{grand_total:<4} ({}%)",
            "TOTAL",
            if grand_total > 0 {
                grand_parsed * 100 / grand_total
            } else {
                0
            }
        );
    }
}
