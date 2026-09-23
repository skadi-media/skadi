//! The OpenAPI document is hand-maintained, so it is only worth anything if it
//! cannot drift from the routers (SKADI-T-0472).
//!
//! This walks the source of every crate that contributes to `/api/v1`, extracts
//! each `.route("<path>", get(..).post(..))` literal, and asserts the resulting
//! `(method, path)` set is exactly the one `openapi::ROUTES` declares. Adding a
//! route without documenting it fails here, and so does documenting a route that
//! no longer exists.
//!
//! Source scanning rather than router introspection because axum does not expose
//! its registered routes at runtime — there is nothing to ask.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Crates whose routers are merged into `/api/v1`. A new domain crate must be
/// added here, or its routes go undocumented and unnoticed.
const ROUTER_CRATES: &[&str] = &["skadi-api", "skadi-movies", "skadi-tv", "skadi-audiobooks"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> has a workspace root two levels up")
        .to_path_buf()
}

/// `(METHOD, path)` for every `.route(...)` literal in the source.
fn routes_in_source() -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for krate in ROUTER_CRATES {
        let src = workspace_root().join("crates").join(krate).join("src");
        let mut files = Vec::new();
        collect_rs(&src, &mut files);
        assert!(!files.is_empty(), "no sources found under {src:?}");
        for file in files {
            let text = std::fs::read_to_string(&file).expect("readable source");
            for (idx, _) in text.match_indices(".route(") {
                let rest = &text[idx + ".route(".len()..];
                // The literal path, then the method calls up to the end of the
                // `.route(...)` argument list — a line-based scan would miss the
                // multi-line `.route("/x", get(a).post(b))` form.
                let Some(open) = rest.find('"') else { continue };
                let Some(close) = rest[open + 1..].find('"') else {
                    continue;
                };
                let path = &rest[open + 1..open + 1 + close];
                if !path.starts_with('/') {
                    continue;
                }
                let tail = &rest[open + 1 + close..];
                let end = tail.find(')').map_or(tail.len(), |_| arg_end(tail));
                for method in ["get", "post", "put", "patch", "delete"] {
                    if tail[..end].contains(&format!("{method}(")) {
                        found.insert((method.to_uppercase(), path.to_string()));
                    }
                }
            }
        }
    }
    found
}

/// Index just past the `)` that closes the `.route(` argument list, counting
/// nesting so `get(handler)` does not end the scan early.
fn arg_end(tail: &str) -> usize {
    let mut depth = 1usize;
    for (i, ch) in tail.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    tail.len()
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_openapi_document_covers_exactly_the_routes_that_are_served() {
    let served = routes_in_source();
    let documented: BTreeSet<(String, String)> = skadi_api::openapi::ROUTES
        .iter()
        .map(|(m, p, _, _)| ((*m).to_string(), (*p).to_string()))
        .collect();

    let undocumented: Vec<_> = served.difference(&documented).collect();
    let stale: Vec<_> = documented.difference(&served).collect();

    assert!(
        undocumented.is_empty(),
        "routes served but missing from openapi::ROUTES — add a line for each:\n{undocumented:#?}"
    );
    assert!(
        stale.is_empty(),
        "routes documented but no longer served — remove them from openapi::ROUTES:\n{stale:#?}"
    );
}
