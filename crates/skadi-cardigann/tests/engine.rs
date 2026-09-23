//! End-to-end search executor tests (SKADI-T-0257): run a real definition against
//! a mock fetcher returning a **captured real response**, asserting normalized
//! releases come out — JSON mode (thepiratebay, real apibay payload) + an HTML
//! extraction case proving CSS selectors + `.Result` field ordering.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};

use skadi_cardigann::engine::resolve_config;
use skadi_cardigann::{
    FetchError, FetchReq, FetchResp, Fetcher, SearchInput, parse_definition, search,
};

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(p).expect("fixture present")
}

/// A fetcher that returns one canned body for any request (and records the URL).
struct Canned {
    body: String,
    expect_in_url: &'static str,
}

#[async_trait::async_trait]
impl Fetcher for Canned {
    async fn fetch(&self, req: FetchReq) -> Result<FetchResp, FetchError> {
        assert!(
            req.url.contains(self.expect_in_url),
            "URL '{}' missing '{}'",
            req.url,
            self.expect_in_url
        );
        Ok(FetchResp {
            status: 200,
            final_url: req.url,
            body: self.body.clone(),
        })
    }
}

#[tokio::test]
async fn thepiratebay_json_search_yields_real_releases() {
    let def = parse_definition(&fixture("thepiratebay.yml")).unwrap();
    let fetcher = Canned {
        body: fixture("responses/thepiratebay-search.json"),
        // the keyword is templated into the apibay path with the configured api url
        expect_in_url: "apibay.org/q.php?q=ubuntu",
    };
    let input = SearchInput {
        keywords: "ubuntu".into(),
        config: resolve_config(&def, &BTreeMap::new()),
        ..Default::default()
    };
    let now = Utc.with_ymd_and_hms(2026, 6, 20, 0, 0, 0).unwrap();

    let rels = search(&def, &input, &fetcher, now).await.unwrap();

    assert!(
        rels.len() > 5,
        "expected several results, got {}",
        rels.len()
    );
    // Every result has the essentials.
    for r in &rels {
        assert!(!r.title.is_empty());
        assert!(r.infohash.is_some(), "torrent needs an infohash");
        assert!(r.size.unwrap_or(0) > 0, "size parsed from bytes");
    }
    // The Ubuntu Bible e-book: category 601 → Books/EBook, unix date → RFC3339.
    let bible = rels
        .iter()
        .find(|r| r.title.contains("Ubuntu Linux Bible"))
        .expect("the e-book result");
    assert!(bible.seeders.unwrap() >= 1);
    assert_eq!(bible.categories, vec!["Books/EBook"]);
    assert!(
        bible.date.as_ref().unwrap().starts_with("2025-"),
        "{:?}",
        bible.date
    );
}

#[tokio::test]
async fn html_search_extracts_rows_fields_and_result_chain() {
    // A minimal HTML tracker definition exercising: a CSS rows selector, a
    // sub-selector field, an attribute field, a filter, and a `text:` field that
    // references an earlier field via `.Result` (order must be preserved).
    let def_yaml = r#"
id: tinytracker
name: Tiny
type: public
caps:
  categorymappings:
    - {id: 1, cat: Movies}
  modes:
    search: [q]
search:
  paths:
    - path: "https://tiny.test/search?q={{ .Keywords }}"
  rows:
    selector: "tr.r"
  fields:
    title_raw:
      selector: a.t
    title:
      text: "{{ .Result.title_raw }}"
      filters:
        - name: replace
          args: [".", " "]
    download:
      selector: a.dl
      attribute: href
    category:
      text: "1"
"#;
    let html = r#"
<html><body><table>
  <tr class="r"><td><a class="t">The.Matrix.1999</a><a class="dl" href="/get/1.torrent">dl</a></td></tr>
  <tr class="r"><td><a class="t">Dune.2021</a><a class="dl" href="/get/2.torrent">dl</a></td></tr>
</table></body></html>
"#;
    let def = parse_definition(def_yaml).unwrap();
    let fetcher = Canned {
        body: html.to_string(),
        expect_in_url: "tiny.test/search?q=matrix",
    };
    let input = SearchInput {
        keywords: "matrix".into(),
        ..Default::default()
    };
    let now = Utc::now();
    let rels = search(&def, &input, &fetcher, now).await.unwrap();

    assert_eq!(rels.len(), 2);
    // `.Result.title_raw` chained into `title`, then the `.`→` ` filter applied.
    assert_eq!(rels[0].title, "The Matrix 1999");
    assert_eq!(rels[0].download.as_deref(), Some("/get/1.torrent"));
    assert_eq!(rels[0].categories, vec!["Movies"]);
    assert_eq!(rels[1].title, "Dune 2021");
}

/// A **real, complex** HTML definition (1337x) end-to-end against crafted markup:
/// jQuery-flavored selectors (`tr:has(...)`, `:contains(...)`) that `scraper`
/// can't parse MUST degrade gracefully (coarser match + optional-field fallback),
/// never abort the search. Exercises the `.Result` field chain + filter pipeline
/// + relative-URL handling + dedup across the def's 4 search paths.
#[tokio::test]
async fn real_1337x_html_definition_end_to_end() {
    let def = parse_definition(&fixture("1337x.yml")).unwrap();
    // td.coll-1 holds the torrent link (title/download); coll-2/3/4 = seed/leech/size.
    let html = r#"
<html><body><table>
  <tr class="head"><td>Name</td><td>SE</td><td>LE</td><td>Size</td></tr>
  <tr>
    <td class="coll-1 name"><a href="/sub/Movies/1/">Movies</a><a href="/torrent/123/The-Matrix-1999-1080p/">The.Matrix.1999.1080p.BluRay</a></td>
    <td class="coll-2 seeds">42</td><td class="coll-3 leeches">5</td><td class="coll-4 size">1.5 GB</td>
    <td class="coll-5 user"><a href="/user/neo/">neo</a></td>
  </tr>
  <tr>
    <td class="coll-1 name"><a href="/torrent/456/Dune-2021-2160p/">Dune.2021.2160p.UHD</a></td>
    <td class="coll-2 seeds">10</td><td class="coll-3 leeches">2</td><td class="coll-4 size">700 MB</td>
    <td class="coll-5 user"><a href="/user/paul/">paul</a></td>
  </tr>
</table></body></html>
"#;
    let fetcher = Canned {
        body: html.to_string(),
        expect_in_url: "", // the def issues 4 paths; accept any
    };
    let input = SearchInput {
        keywords: "matrix".into(),
        base_url: "https://1337x.test".into(),
        config: resolve_config(&def, &BTreeMap::new()),
        ..Default::default()
    };

    let rels = search(&def, &input, &fetcher, Utc::now()).await.unwrap();

    // Two unique results despite 4 search paths × 2 rows (deduped by title+link);
    // the header row is dropped (no torrent link → no title).
    assert_eq!(rels.len(), 2, "deduped to the 2 real rows: {rels:?}");
    let titles: Vec<&str> = rels.iter().map(|r| r.title.as_str()).collect();
    assert!(titles.iter().any(|t| t.contains("Matrix")), "{titles:?}");
    assert!(titles.iter().any(|t| t.contains("Dune")), "{titles:?}");
    let matrix = rels.iter().find(|r| r.title.contains("Matrix")).unwrap();
    assert_eq!(matrix.seeders, Some(42));
    assert_eq!(matrix.size, Some(1_610_612_736)); // 1.5 GiB
    // Relative download href absolutized against the base.
    assert!(
        matrix
            .download
            .as_deref()
            .unwrap()
            .contains("/torrent/123/"),
        "{:?}",
        matrix.download
    );
}

/// The query lives in `search.inputs` (the form 95% of real defs use, e.g. your
/// AudioBookBay), NOT inline in the path — the engine MUST render inputs into the
/// request URL, else it searches the bare path and finds nothing.
#[tokio::test]
async fn search_inputs_become_query_params() {
    let def_yaml = r#"
id: inputtracker
name: Input
type: public
caps:
  categorymappings: [{id: "1", cat: Audio/Audiobook}]
  modes: {search: [q]}
search:
  paths:
    - path: "/"
      response: {type: json}
  inputs:
    s: "{{ .Keywords }}"
    cat: "1"
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    download: {selector: link}
"#;
    let def = parse_definition(def_yaml).unwrap();
    let fetcher = Canned {
        body: r#"[{"name":"Some Audiobook 64kbps","link":"magnet:?xt=urn:btih:DEAD"}]"#.into(),
        // the inputs were appended to the bare "/" path as a query string
        expect_in_url: "/?s=matrix&cat=1",
    };
    let input = SearchInput {
        keywords: "matrix".into(),
        base_url: "https://abb.test".into(),
        config: resolve_config(&def, &BTreeMap::new()),
        ..Default::default()
    };
    let rels = search(&def, &input, &fetcher, Utc::now()).await.unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].title, "Some Audiobook 64kbps");
}

/// The `caps.categories` **map** form (used by ~6% of real defs, e.g. eztv) is
/// parsed into category pairs, not just the `categorymappings` list.
#[test]
fn caps_categories_map_form_is_parsed() {
    let def = parse_definition(
        "id: e\nname: E\ncaps:\n  categories:\n    1: TV\n    5070: TV/Anime\n  modes: {search: [q]}\n",
    )
    .unwrap();
    let pairs = def.caps.category_pairs();
    assert!(pairs.iter().any(|(id, cat)| id == "1" && cat == "TV"));
    assert!(
        pairs
            .iter()
            .any(|(id, cat)| id == "5070" && cat == "TV/Anime")
    );
}

/// A checkbox config override MUST resolve to a `Bool` (not `Str`), so a
/// definition's `eq .Config.flag .False` template still works (regression).
#[test]
fn checkbox_overrides_resolve_to_bool() {
    use skadi_cardigann::template::Value;
    let def = parse_definition(
        "id: t\nname: T\nsettings:\n  - {name: flag, type: checkbox, default: true}\n",
    )
    .unwrap();
    // Default (no override) → Bool(true).
    let d = resolve_config(&def, &BTreeMap::new());
    assert_eq!(d.get("flag"), Some(&Value::Bool(true)));
    // User override "false" → Bool(false), NOT Str("false").
    let o = resolve_config(&def, &BTreeMap::from([("flag".into(), "false".into())]));
    assert_eq!(o.get("flag"), Some(&Value::Bool(false)));
}
