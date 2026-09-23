//! Real-network validation pass (SKADI-I-0036): sync the ACTUAL Jackett/Prowlarr
//! definition library from upstream and run the whole thing through the parser +
//! catalog, reporting coverage. Ignored by default (hits the network + downloads
//! the repo tarball). Run with:
//!
//!   cargo test -p skadi-indexers --test real_library -- --ignored --nocapture

use std::time::Duration;

use skadi_cardigann::Catalog;
use skadi_http::HttpClient;
use skadi_indexers::definitions::{DEFAULT_REVISION, DefinitionStore};

/// The operator's trackers (per the deploy notes) we specifically care about.
const WANTED: &[&str] = &[
    "thepiratebay",
    "1337x",
    "rutracker",
    "torrentleech",
    "nyaasi",
    "eztv",
    "limetorrents",
    "yts",
    "torrentgalaxydl",
    "badasstorrents",
];

#[tokio::test]
#[ignore = "hits the network; run with --ignored"]
async fn real_upstream_library_syncs_and_parses() {
    let dir = skadi_core::unique_temp_path("real-lib");
    let _ = std::fs::remove_dir_all(&dir);
    let http = HttpClient::new(Duration::from_secs(180)).unwrap();
    let store = DefinitionStore::new(&dir, http);

    println!("\nsyncing Prowlarr/Indexers @ {DEFAULT_REVISION} …");
    let report = store.refresh(DEFAULT_REVISION).await.expect("sync");
    println!(
        "  wrote {} definitions, {} unparseable after sync",
        report.written, report.unparseable
    );

    let (catalog, errors) = Catalog::load_dir(&dir).expect("load");
    let total = catalog.len();
    let failed = errors.len();
    let pct = if total + failed == 0 {
        0.0
    } else {
        100.0 * total as f64 / (total + failed) as f64
    };
    println!("\n=== parse coverage ===");
    println!("  parsed OK : {total}");
    println!("  warn-skip : {failed}");
    println!("  coverage  : {pct:.1}%");

    if !errors.is_empty() {
        println!("\n=== sample of skipped definitions (first 25) ===");
        for e in errors.iter().take(25) {
            let id = e.id.as_deref().unwrap_or("?");
            // first line of the parse error, trimmed
            let msg = e.message.lines().next().unwrap_or("").trim();
            println!("  - {id}: {msg}");
        }
    }

    println!("\n=== operator's trackers ===");
    for id in WANTED {
        match catalog.entry(id) {
            Some(e) => println!(
                "  ✓ {:<18} {:<13} login={} settings={} cats={}",
                e.id,
                e.privacy,
                e.needs_login,
                e.settings.len(),
                e.categories.len()
            ),
            None => println!("  ✗ {id:<18} NOT FOUND in catalog"),
        }
    }

    let _ = std::fs::remove_dir_all(&dir);

    // The library is large + churny; we don't gate on 100%, but a healthy sync
    // should parse the vast majority and include the common public trackers.
    assert!(total > 200, "expected a few hundred defs, got {total}");
    assert!(pct > 85.0, "parse coverage unexpectedly low: {pct:.1}%");
    assert!(catalog.entry("thepiratebay").is_some(), "TPB missing");
}

/// Live end-to-end search against a real public tracker (apibay/TPB) through the
/// actual `HttpFetcher` (per-indexer reqwest client) + engine, proving the full
/// network → scrape → normalize chain on a LIVE response (not a captured one).
#[tokio::test]
#[ignore = "hits the network; run with --ignored"]
async fn live_search_thepiratebay() {
    use skadi_cardigann::engine::resolve_config;
    use skadi_cardigann::{SearchInput, parse_definition, search};
    use skadi_indexers::cardigann::HttpFetcher;

    let def = parse_definition(include_str!("../bundled/thepiratebay.yml")).unwrap();
    let fetcher = HttpFetcher::new(None, None);
    let input = SearchInput {
        keywords: "ubuntu".into(),
        config: resolve_config(&def, &std::collections::BTreeMap::new()),
        ..Default::default()
    };
    let rels = search(&def, &input, &fetcher, chrono::Utc::now())
        .await
        .expect("live search");
    println!("\nTPB live search 'ubuntu': {} releases", rels.len());
    for r in rels.iter().take(5) {
        println!(
            "  - {}  ({} seeders, {} bytes, {})",
            r.title,
            r.seeders.unwrap_or(0),
            r.size.unwrap_or(0),
            r.categories.join("/"),
        );
    }
    assert!(!rels.is_empty(), "expected live TPB results for 'ubuntu'");
    assert!(rels.iter().all(|r| !r.title.is_empty()));
    assert!(rels.iter().any(|r| r.infohash.is_some()));
}

/// Live demo: find real media across the bundled public trackers. CloudFlare-gated
/// ones (1337x etc.) will come back empty without FlareSolverr — informative.
#[tokio::test]
#[ignore = "hits the network; run with --ignored"]
async fn live_find_media() {
    use skadi_cardigann::engine::resolve_config;
    use skadi_cardigann::{SearchInput, parse_definition, search};
    use skadi_indexers::cardigann::HttpFetcher;

    let cases: &[(&str, &str, &str)] = &[
        (
            "The Pirate Bay",
            include_str!("../bundled/thepiratebay.yml"),
            "Dune Part Two 2024",
        ),
        ("YTS", include_str!("../bundled/yts.yml"), "Oppenheimer"),
        ("Nyaa", include_str!("../bundled/nyaasi.yml"), "Frieren"),
        (
            "LimeTorrents",
            include_str!("../bundled/limetorrents.yml"),
            "Oppenheimer 2023",
        ),
        (
            "The Pirate Bay (audiobook)",
            include_str!("../bundled/thepiratebay.yml"),
            "Project Hail Mary audiobook",
        ),
        (
            "AudioBookBay",
            include_str!("../bundled/audiobookbay.yml"),
            "Project Hail Mary",
        ),
    ];
    for (name, yaml, query) in cases {
        let def = parse_definition(yaml).unwrap();
        let fetcher = HttpFetcher::new(None, None);
        let input = SearchInput {
            keywords: (*query).into(),
            base_url: def.links.first().cloned().unwrap_or_default(),
            config: resolve_config(&def, &std::collections::BTreeMap::new()),
            ..Default::default()
        };
        println!("\n=== {name} — \"{query}\" ===");
        match search(&def, &input, &fetcher, chrono::Utc::now()).await {
            Ok(r) if r.is_empty() => {
                println!("  (no results — likely CloudFlare-gated; needs FlareSolverr)")
            }
            Ok(r) => {
                println!("  {} results:", r.len());
                for x in r.iter().take(4) {
                    println!(
                        "   • {}  [{} seeders, {:.2} GB]",
                        x.title,
                        x.seeders.unwrap_or(0),
                        x.size.unwrap_or(0) as f64 / 1e9
                    );
                }
            }
            Err(e) => println!("  ! error: {e}"),
        }
    }
}
