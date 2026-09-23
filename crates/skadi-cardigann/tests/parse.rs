//! Parse the vendored real Jackett/Prowlarr definitions (SKADI-T-0255): a JSON-API
//! tracker (thepiratebay), HTML-scrape trackers (1337x, limetorrents), and a
//! login-based one (torrentleech). They MUST all load through the lenient parser.

use std::fs;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(name: &str) -> String {
    fs::read_to_string(fixtures().join(name)).expect("fixture present")
}

#[test]
fn every_vendored_definition_parses() {
    let yamls: Vec<String> = fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension().and_then(|x| x.to_str()) == Some("yml"))
                .then(|| fs::read_to_string(&p).unwrap())
        })
        .collect();
    assert!(
        yamls.len() >= 4,
        "expected >= 4 fixtures, found {}",
        yamls.len()
    );

    let (ok, err) = skadi_cardigann::load_all(yamls.iter().map(String::as_str));
    assert!(err.is_empty(), "definitions failed to parse: {err:?}");
    assert_eq!(ok.len(), yamls.len());
    for d in &ok {
        assert!(!d.id.is_empty() && !d.name.is_empty());
    }
}

#[test]
fn thepiratebay_json_definition_shape() {
    let d = skadi_cardigann::parse_definition(&read("thepiratebay.yml")).unwrap();
    assert_eq!(d.id, "thepiratebay");
    assert_eq!(d.privacy, "public");
    assert!(!d.needs_login());
    // Rich category map + the standard search modes.
    assert!(d.caps.categorymappings.len() > 40);
    assert!(d.caps.modes.contains_key("movie-search"));
    assert!(d.caps.modes.contains_key("book-search"));
    // A numeric category id parsed into a string.
    assert!(
        d.caps
            .categorymappings
            .iter()
            .any(|c| c.id == "102" && c.cat == "Audio/Audiobook")
    );
    // JSON response + the title field with its filter pipeline.
    assert_eq!(d.search.paths[0].response.as_ref().unwrap().kind, "json");
    let title = d.search.fields.get("title").expect("title field");
    assert_eq!(title.selector.as_deref(), Some("name"));
    assert!(!title.filters.is_empty());
}

#[test]
fn html_and_login_definitions_parse_their_blocks() {
    // HTML scrape: a CSS rows selector + an attribute-extracting field with a
    // heterogeneous filter arg list (`["/", 3]`).
    let x = skadi_cardigann::parse_definition(&read("1337x.yml")).unwrap();
    assert!(x.search.rows.selector.contains("tr"));
    assert!(
        x.search
            .fields
            .values()
            .any(|f| f.attribute.as_deref() == Some("href"))
    );

    // Login-based: the login block is present and modelled.
    let tl = skadi_cardigann::parse_definition(&read("torrentleech.yml")).unwrap();
    assert!(tl.needs_login());
    assert!(tl.login.is_some());
}
