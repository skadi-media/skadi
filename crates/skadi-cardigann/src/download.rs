//! Cardigann download-block executor (SKADI-T-0306, SKADI-T-0589): resolve a
//! release whose only fetch is a **detail-page URL** into a real link by fetching
//! that page and running the definition's `download` block.
//!
//! Some trackers don't put a magnet/`.torrent` in the search row — the search
//! field `download` is just the detail page's URL. Two block shapes cover them:
//!
//! - `download.selectors` (LimeTorrents, 1337x, TorrentDownload(s), …): an
//!   ordered list of CSS selectors tried against the detail page; the first that
//!   matches yields the magnet or `.torrent` link. Selectors may carry
//!   `{{ .Config.* }}` templates (LimeTorrents chooses magnet vs iTorrents from a
//!   setting) and a `filters` pipeline.
//! - `download.infohash` (AudioBookBay): the info-hash lives in a cell on the page
//!   and the magnet is built from it.
//!
//! Without this the HTML URL reached the downloader as a bogus `.torrent`: on
//! 2026-09-17 every decision that picked LimeTorrents failed the snatch that way.
//! This runs at **grab time** (one fetch per snatch) through the same
//! authenticated + FlareSolverr [`Fetcher`] the search uses.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use scraper::{ElementRef, Html, Selector};

use crate::engine::{EngineError, FetchReq, FetchResp, Fetcher, Method};
use crate::filters::{self, FilterCtx};
use crate::model::{Definition, Filter};
use crate::template::{TemplateContext, Value as TVal, render};

/// Resolve a detail-page `download_url` into a final link — a `magnet:` URI or an
/// absolute `.torrent` URL — via the definition's `download` block.
///
/// `config` is the indexer's resolved settings (defaults overlaid with the
/// operator's overrides), for the `{{ .Config.* }}` templates a selector may use.
///
/// Returns `Ok(None)` when the definition has no usable download block — the caller
/// then keeps the original fetch as-is (most indexers, which already yield a
/// magnet/`.torrent` in the search row, take this no-op path).
///
/// # Errors
/// [`EngineError`] on a fetch failure, or when the block runs but yields nothing
/// usable (a retryable grab error, not a silent drop).
pub async fn resolve_download<F: Fetcher + ?Sized>(
    def: &Definition,
    download_url: &str,
    fetcher: &F,
    now: DateTime<Utc>,
    config: &BTreeMap<String, TVal>,
) -> Result<Option<String>, EngineError> {
    let Some(dl) = &def.download else {
        return Ok(None);
    };
    let selectors = dl
        .selectors
        .as_ref()
        .and_then(|y| serde_yaml::from_value::<Vec<DownloadSelector>>(y.clone()).ok())
        .unwrap_or_default();
    let hash_block = dl.infohash.as_ref().and_then(|ih| ih.get("hash"));
    if selectors.is_empty() && hash_block.is_none() {
        return Ok(None);
    }

    // Fetch the detail page once (GET; download blocks rarely POST and none of
    // ours do); both shapes read from it.
    let resp = fetcher
        .fetch(FetchReq {
            method: Method::Get,
            url: download_url.to_string(),
            headers: Vec::new(),
            body: None,
        })
        .await
        .map_err(|e| EngineError::Fetch(e.0))?;

    if !selectors.is_empty() {
        let ctx = TemplateContext {
            config: config.clone(),
            ..Default::default()
        };
        if let Some(link) = first_selector_link(&selectors, &resp, &ctx, now)? {
            return Ok(Some(link));
        }
        if hash_block.is_none() {
            return Err(EngineError::Response(format!(
                "download selectors matched nothing on {download_url}"
            )));
        }
    }
    let Some(hash_block) = hash_block else {
        return Ok(None);
    };
    let selector = hash_block
        .get("selector")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if selector.is_empty() {
        return Ok(None);
    }
    // The block's filter pipeline (e.g. the `regexp ([A-Fa-f0-9]{40})` that pulls
    // the hash out of the cell text). Absent/unparseable ⇒ no filters.
    let filters_vec: Vec<Filter> = hash_block
        .get("filters")
        .and_then(|f| serde_yaml::from_value(f.clone()).ok())
        .unwrap_or_default();

    let raw = select_value(&resp.body, selector).unwrap_or_default();
    let hash = filters::apply_all(&filters_vec, &raw, &FilterCtx { now })
        .map_err(|e| EngineError::Response(format!("download infohash filter failed: {e}")))?;
    let hash = hash.trim();

    if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(EngineError::Response(format!(
            "download block did not yield a 40-char info-hash from {download_url} (got {hash:?})"
        )));
    }
    Ok(Some(format!(
        "magnet:?xt=urn:btih:{}",
        hash.to_ascii_lowercase()
    )))
}

/// One entry of a `download.selectors` list.
#[derive(Debug, Clone, serde::Deserialize)]
struct DownloadSelector {
    selector: String,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default)]
    filters: Vec<Filter>,
}

/// The first selector that matches the page and yields a non-empty value, with
/// its filters applied and made absolute against the page URL. `None` when none
/// match (a definition's selectors are alternatives, so that is "not found",
/// not an error, for the caller to decide on).
fn first_selector_link(
    selectors: &[DownloadSelector],
    page: &FetchResp,
    ctx: &TemplateContext,
    now: DateTime<Utc>,
) -> Result<Option<String>, EngineError> {
    for sel in selectors {
        // A selector whose template fails to render (unknown setting) is skipped,
        // not fatal: the next alternative may still work.
        let Ok(selector) = render(&sel.selector, ctx) else {
            continue;
        };
        let Some(raw) = select_attr(&page.body, &selector, sel.attribute.as_deref()) else {
            continue;
        };
        let value = filters::apply_all(&sel.filters, &raw, &FilterCtx { now })
            .map_err(|e| EngineError::Response(format!("download selector filter failed: {e}")))?;
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let base = if page.final_url.is_empty() {
            None
        } else {
            Some(page.final_url.as_str())
        };
        return Ok(Some(absolutize(base, value)));
    }
    Ok(None)
}

/// Extract an attribute (or, without one, the text) of the first element matching
/// `selector`. Falls back to [`select_value`] for the `:contains(...)` shapes
/// `scraper` cannot parse (text only).
fn select_attr(html: &str, selector: &str, attribute: Option<&str>) -> Option<String> {
    if let Ok(sel) = Selector::parse(selector) {
        let doc = Html::parse_document(html);
        let el = doc.select(&sel).next()?;
        return match attribute {
            Some(attr) => el.value().attr(attr).map(|v| v.trim().to_string()),
            None => Some(text_of(el)),
        };
    }
    select_value(html, selector)
}

/// Make a scraped link absolute against the page it came from. Magnets and
/// absolute URLs pass through; `//host/path`, `/path` and bare relative paths
/// are resolved the way a browser would. Pure for unit testing.
#[must_use]
pub fn absolutize(base: Option<&str>, link: &str) -> String {
    let link = link.trim();
    if link.starts_with("magnet:") || link.starts_with("http://") || link.starts_with("https://") {
        return link.to_string();
    }
    let Some(base) = base else {
        return link.to_string();
    };
    let (scheme, rest) = match base.split_once("://") {
        Some(parts) => parts,
        None => return link.to_string(),
    };
    let host = rest.split('/').next().unwrap_or_default();
    if let Some(no_scheme) = link.strip_prefix("//") {
        return format!("{scheme}://{no_scheme}");
    }
    if let Some(abs_path) = link.strip_prefix('/') {
        return format!("{scheme}://{host}/{abs_path}");
    }
    // Relative to the page's directory.
    let dir = match base.rfind('/') {
        Some(i) if i > scheme.len() + 2 => &base[..i],
        _ => base,
    };
    format!("{}/{link}", dir.trim_end_matches('/'))
}

/// Extract text for a download-block selector, honoring the jQuery `:contains("…")`
/// pseudo that `scraper` can't parse. Handles the shapes download blocks actually
/// use:
///   - a plain CSS selector (fast path),
///   - `<anchor>:contains("text")` → the matching element's text,
///   - `<anchor>:contains("text") ~ <tag>` / `+ <tag>` → a sibling cell (ABB's
///     `td:contains("Info Hash:") ~ td`),
///   - `<anchor>:contains("text") <descendant>` / `> <child>`.
fn select_value(html: &str, selector: &str) -> Option<String> {
    let doc = Html::parse_document(html);

    // Fast path: scraper parses it directly.
    if let Ok(sel) = Selector::parse(selector) {
        return doc.select(&sel).next().map(text_of);
    }

    // `:contains(...)` path.
    let (anchor_sel, needle, rest) = split_contains(selector)?;
    let anchor = Selector::parse(&anchor_sel).ok()?;
    for el in doc.select(&anchor) {
        if !el.text().collect::<String>().contains(&needle) {
            continue;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            return Some(text_of(el));
        }
        if let Some(v) = resolve_combinator(el, rest) {
            return Some(v);
        }
    }
    None
}

/// Resolve the part of a selector that follows the `:contains(...)` anchor element,
/// relative to that element. `rest` is e.g. `~ td`, `+ td`, `> td`, or `td`.
fn resolve_combinator(anchor: ElementRef, rest: &str) -> Option<String> {
    let (combinator, target) = parse_combinator(rest);
    match combinator {
        // General / adjacent sibling: walk following element siblings. Targets in
        // download blocks are simple tag names (`td`); match by name.
        '~' | '+' => {
            let mut sibs = anchor.next_siblings().filter_map(ElementRef::wrap);
            if combinator == '+' {
                let sib = sibs.next()?;
                return tag_matches(&sib, target).then(|| text_of(sib));
            }
            sibs.find(|s| tag_matches(s, target)).map(text_of)
        }
        // Descendant / child: a normal sub-selector under the anchor.
        _ => {
            let sel = Selector::parse(target).ok()?;
            anchor.select(&sel).next().map(text_of)
        }
    }
}

/// Split `before:contains("needle")after` into `(before_selector, needle, after)`.
/// `before` defaults to `*` (the contains pseudo standing alone).
fn split_contains(selector: &str) -> Option<(String, String, String)> {
    let at = selector.find(":contains(")?;
    let open = at + ":contains(".len();
    let close = open + selector[open..].find(')')?;
    let anchor = selector[..at].trim();
    let needle = selector[open..close]
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    let anchor = if anchor.is_empty() { "*" } else { anchor };
    Some((
        anchor.to_string(),
        needle,
        selector[close + 1..].trim().to_string(),
    ))
}

/// `~ td` → `('~', "td")`, `+ td` → `('+', "td")`, `> td` → `('>', "td")`,
/// `td` → `(' ', "td")` (descendant).
fn parse_combinator(rest: &str) -> (char, &str) {
    let rest = rest.trim();
    for c in ['~', '+', '>'] {
        if let Some(t) = rest.strip_prefix(c) {
            return (c, t.trim());
        }
    }
    (' ', rest)
}

/// Whether an element's tag name matches a simple `tag` target.
fn tag_matches(el: &ElementRef, target: &str) -> bool {
    el.value().name().eq_ignore_ascii_case(target.trim())
}

/// Trimmed concatenated text of an element.
fn text_of(el: ElementRef) -> String {
    el.text().collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{FetchError, FetchResp};

    /// Serves one canned body for any URL.
    struct OneBody(&'static str);

    #[async_trait::async_trait]
    impl Fetcher for OneBody {
        async fn fetch(&self, req: FetchReq) -> Result<FetchResp, FetchError> {
            Ok(FetchResp {
                status: 200,
                final_url: req.url,
                body: self.0.to_string(),
            })
        }
    }

    // A trimmed AudioBookBay detail page: the Info Hash sits in the td *following*
    // the `Info Hash:` label cell, and the gated `.torrent` link is a separate row.
    const ABB_PAGE: &str = r#"
<html><head><title>Soulbrand</title></head><body>
<h1>Soulbrand (Weapons and Wielders #3)</h1>
<table><tbody>
  <tr><td>Format:</td><td>M4B</td></tr>
  <tr><td>Bitrate:</td><td>128 kbps</td></tr>
  <tr><td>Info Hash:</td><td>cc9cfe0cdd66d98bffffbff2d9d71798622cd6f5</td></tr>
  <tr><td>Torrent Download</td><td><a href="/downld0?downfs=login">Download</a></td></tr>
</tbody></table>
</body></html>
"#;

    fn abb_def() -> Definition {
        let yaml = r#"
id: audiobookbay
name: AudioBook Bay
type: public
links: [https://audiobookbay.lu/]
caps:
  modes: {search: [q]}
search:
  rows:
    selector: div.post
download:
  infohash:
    hash:
      selector: 'td:contains("Info Hash:") ~ td'
      filters:
        - name: regexp
          args: ([A-Fa-f0-9]{40})
"#;
        crate::parse_definition(yaml).unwrap()
    }

    #[tokio::test]
    async fn resolves_abb_detail_page_to_magnet() {
        let magnet = resolve_download(
            &abb_def(),
            "https://audiobookbay.lu/abss/soulebrand-weapons-and-wielders-3-andrew-rowe/",
            &OneBody(ABB_PAGE),
            Utc::now(),
            &BTreeMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            magnet,
            Some("magnet:?xt=urn:btih:cc9cfe0cdd66d98bffffbff2d9d71798622cd6f5".to_string())
        );
    }

    #[tokio::test]
    async fn no_download_block_is_a_noop() {
        let def = crate::parse_definition(
            "id: x\nname: X\ntype: public\nlinks: [https://x/]\ncaps:\n  modes: {search: [q]}\nsearch:\n  rows:\n    selector: tr\n",
        )
        .unwrap();
        let out = resolve_download(
            &def,
            "https://x/page",
            &OneBody(""),
            Utc::now(),
            &BTreeMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(out, None);
    }

    #[tokio::test]
    async fn missing_hash_on_page_is_a_retryable_error() {
        let out = resolve_download(
            &abb_def(),
            "https://x/page",
            &OneBody("<html><body>no hash here</body></html>"),
            Utc::now(),
            &BTreeMap::new(),
        )
        .await;
        assert!(
            out.is_err(),
            "a page without an info-hash should error, not drop"
        );
    }

    #[test]
    fn contains_sibling_selector_extracts_cell() {
        let v = select_value(ABB_PAGE, r#"td:contains("Info Hash:") ~ td"#);
        assert_eq!(
            v.as_deref(),
            Some("cc9cfe0cdd66d98bffffbff2d9d71798622cd6f5")
        );
    }

    #[test]
    fn plain_selector_fast_path() {
        assert_eq!(
            select_value(ABB_PAGE, "h1").as_deref(),
            Some("Soulbrand (Weapons and Wielders #3)")
        );
    }

    // A trimmed LimeTorrents detail page (2026-09-17): both the iTorrents
    // `.torrent` link and the magnet carry the `csprite_dltorrent` class.
    const LIME_PAGE: &str = r#"
<html><body>
<div class="torrentinfo">
  <a class="csprite_dltorrent" href="http://itorrents.org/torrent/4251B2662B77B5FB71828F5B8F2C405C2F40FA06.torrent?title=x">Download torrent</a>
  <a class="csprite_dltorrent" href="magnet:?xt=urn:btih:4251B2662B77B5FB71828F5B8F2C405C2F40FA06&dn=Into+the+Badlands">Magnet</a>
  <a href="/torrent-13267114.html">Details</a>
</div>
</body></html>
"#;

    fn lime_def() -> Definition {
        let yaml = r#"
id: limetorrents
name: LimeTorrents
type: public
links: [https://www.limetorrents.fun/]
settings:
  - name: primarydownloadlink
    type: select
    default: "magnet:"
    options:
      "//itorrents.": iTorrents
      "magnet:": magnet
  - name: fallbackdownloadlink
    type: select
    default: "//itorrents."
    options:
      "//itorrents.": iTorrents
      "magnet:": magnet
caps:
  modes: {search: [q]}
search:
  rows:
    selector: tr
download:
  selectors:
    - selector: a.csprite_dltorrent[href*="{{ .Config.primarydownloadlink }}"]
      attribute: href
      filters:
        - name: replace
          args: ["http://itorrents.org/", "https://itorrents.net/"]
    - selector: a.csprite_dltorrent[href*="{{ .Config.fallbackdownloadlink }}"]
      attribute: href
      filters:
        - name: replace
          args: ["http://itorrents.org/", "https://itorrents.net/"]
"#;
        crate::parse_definition(yaml).unwrap()
    }

    /// The production failure (SKADI-T-0589): with the definition's default
    /// setting the first selector picks the magnet off the detail page.
    #[tokio::test]
    async fn selectors_pick_the_magnet_with_the_default_setting() {
        let def = lime_def();
        let config = crate::engine::resolve_config(&def, &BTreeMap::new());
        let out = resolve_download(
            &def,
            "https://www.limetorrents.fun/Into-the-Badlands-torrent-13267114.html",
            &OneBody(LIME_PAGE),
            Utc::now(),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(
            out.as_deref(),
            Some(
                "magnet:?xt=urn:btih:4251B2662B77B5FB71828F5B8F2C405C2F40FA06&dn=Into+the+Badlands"
            )
        );
    }

    /// An operator override steers the first selector to the `.torrent`, and the
    /// selector's filter pipeline rewrites the dead host.
    #[tokio::test]
    async fn selectors_honour_config_overrides_and_filters() {
        let def = lime_def();
        let overrides = BTreeMap::from([(
            "primarydownloadlink".to_string(),
            "//itorrents.".to_string(),
        )]);
        let config = crate::engine::resolve_config(&def, &overrides);
        let out = resolve_download(
            &def,
            "https://www.limetorrents.fun/x.html",
            &OneBody(LIME_PAGE),
            Utc::now(),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(
            out.as_deref(),
            Some(
                "https://itorrents.net/torrent/4251B2662B77B5FB71828F5B8F2C405C2F40FA06.torrent?title=x"
            )
        );
    }

    /// A page with neither link is a retryable error, never a silent pass-through
    /// of the HTML URL.
    #[tokio::test]
    async fn selectors_matching_nothing_is_an_error() {
        let def = lime_def();
        let config = crate::engine::resolve_config(&def, &BTreeMap::new());
        let out = resolve_download(
            &def,
            "https://www.limetorrents.fun/x.html",
            &OneBody("<html><body>Just a moment...</body></html>"),
            Utc::now(),
            &config,
        )
        .await;
        assert!(out.is_err());
    }

    #[test]
    fn absolutize_resolves_like_a_browser() {
        let base = Some("https://tracker.example/dir/page.html");
        assert_eq!(
            absolutize(base, "magnet:?xt=urn:btih:abc"),
            "magnet:?xt=urn:btih:abc"
        );
        assert_eq!(
            absolutize(base, "https://other/x.torrent"),
            "https://other/x.torrent"
        );
        assert_eq!(
            absolutize(base, "//cdn.example/x.torrent"),
            "https://cdn.example/x.torrent"
        );
        assert_eq!(
            absolutize(base, "/dl/x.torrent"),
            "https://tracker.example/dl/x.torrent"
        );
        assert_eq!(
            absolutize(base, "x.torrent"),
            "https://tracker.example/dir/x.torrent"
        );
        assert_eq!(absolutize(None, "/dl/x.torrent"), "/dl/x.torrent");
    }
}
