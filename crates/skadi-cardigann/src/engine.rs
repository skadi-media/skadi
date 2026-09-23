//! The Cardigann search executor (SKADI-T-0257): render a definition's search
//! request, fetch it through an injected [`Fetcher`], select result rows + fields
//! (HTML via CSS selectors, JSON via paths), run the per-field filter pipeline,
//! and normalize into [`CardigannRelease`]s. The engine never touches the network
//! itself — `skadi-indexers` supplies a `Fetcher` over the shared HTTP client (+
//! FlareSolverr), so this crate stays decoupled and mockable.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use indexmap::IndexMap;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use scraper::{Html, Selector};
use serde_json::Value as Json;

/// Characters to percent-encode in a keyword before it goes into a URL: spaces +
/// the query/path-breaking set. Alphanumerics and `/`-safe punctuation pass.
const KEYWORD_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'\'');

use crate::filters::{self, FilterCtx};
use crate::model::{Definition, Field};
use crate::template::{self, TemplateContext, Value as TVal};

/// An HTTP method the engine asks the fetcher to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// A request the engine wants performed.
#[derive(Debug, Clone)]
pub struct FetchReq {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Form body for POST (`a=1&b=2`).
    pub body: Option<String>,
}

/// The fetched response.
#[derive(Debug, Clone)]
pub struct FetchResp {
    pub status: u16,
    pub final_url: String,
    pub body: String,
}

/// A failed fetch (network/solver error) — surfaced as an [`EngineError`].
#[derive(Debug, Clone)]
pub struct FetchError(pub String);

/// The injected HTTP seam. `skadi-indexers` implements this over the shared
/// bounded client + per-host FlareSolverr; tests supply a canned-response mock.
#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, req: FetchReq) -> Result<FetchResp, FetchError>;
}

/// One normalized result from a tracker search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardigannRelease {
    pub title: String,
    pub details: Option<String>,
    pub download: Option<String>,
    pub magnet: Option<String>,
    pub infohash: Option<String>,
    pub size: Option<u64>,
    pub seeders: Option<u64>,
    pub leechers: Option<u64>,
    /// RFC3339 (UTC) when parseable, else the raw value.
    pub date: Option<String>,
    /// Mapped Newznab category names (`Movies/HD`, `Audio/Audiobook`, …).
    pub categories: Vec<String>,
    pub imdb: Option<String>,
    /// Every extracted field, for the adapter to read anything bespoke.
    pub raw: IndexMap<String, String>,
}

/// Anything that aborts a search.
#[derive(Debug, Clone)]
pub enum EngineError {
    Template(String),
    Filter(String),
    Selector(String),
    Fetch(String),
    Response(String),
    /// Every configured search path was refused by the tracker (SKADI-T-0496).
    /// Carries the last status seen, which is usually 403 (a Cloudflare
    /// challenge with no solver) or 503.
    AllPathsRefused(u16),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Template(m) => write!(f, "template: {m}"),
            EngineError::Filter(m) => write!(f, "filter: {m}"),
            EngineError::Selector(m) => write!(f, "selector: {m}"),
            EngineError::Fetch(m) => write!(f, "fetch: {m}"),
            EngineError::Response(m) => write!(f, "response: {m}"),
            EngineError::AllPathsRefused(status) => write!(
                f,
                "every search path was refused by the tracker (HTTP {status}) — \
                 for a challenge-fronted tracker this usually means FlareSolverr \
                 is down or unconfigured"
            ),
        }
    }
}
impl std::error::Error for EngineError {}

/// What the domain asks the engine to search for.
#[derive(Debug, Clone, Default)]
pub struct SearchInput {
    pub keywords: String,
    /// Tracker category ids to place in `.Categories` (already mapped from Newznab).
    pub categories: Vec<String>,
    /// ID query params (`imdbid`, `season`, `ep`, …) for `.Query.*`.
    pub query: BTreeMap<String, String>,
    /// Resolved per-indexer config (`text` → `Str`, `checkbox` → `Bool`).
    pub config: BTreeMap<String, TVal>,
    /// The working site base URL; relative `search.paths` are resolved against it.
    pub base_url: String,
}

/// Resolve a definition's config from its `settings` defaults overlaid with user
/// overrides — a convenience for callers/tests building a [`SearchInput`].
#[must_use]
pub fn resolve_config(
    def: &Definition,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, TVal> {
    let mut cfg = BTreeMap::new();
    for s in &def.settings {
        let val = match s.kind.as_str() {
            "checkbox" => {
                let d = s
                    .default
                    .as_ref()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                TVal::Bool(d)
            }
            _ => TVal::Str(
                s.default
                    .as_ref()
                    .map(crate::filters_scalar)
                    .unwrap_or_default(),
            ),
        };
        cfg.insert(s.name.clone(), val);
    }
    for (k, v) in overrides {
        // Coerce a checkbox override to `Bool` so `eq .Config.flag .False` (a
        // bool compare) still works — a plain `Str` would silently never match.
        let is_checkbox = def
            .settings
            .iter()
            .any(|s| &s.name == k && s.kind == "checkbox");
        let val = if is_checkbox {
            TVal::Bool(matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ))
        } else {
            TVal::Str(v.clone())
        };
        cfg.insert(k.clone(), val);
    }
    cfg
}

/// Run a definition's search against `fetcher`. `now` resolves relative dates.
///
/// # Errors
/// An [`EngineError`] if a template/selector/fetch fails fatally.
pub async fn search<F: Fetcher + ?Sized>(
    def: &Definition,
    input: &SearchInput,
    fetcher: &F,
    now: DateTime<Utc>,
) -> Result<Vec<CardigannRelease>, EngineError> {
    let fctx = FilterCtx { now };
    // Keywords pass through the definition's keywordsfilters first, then are
    // URL-encoded for safe substitution into the request path/query (as
    // cardigann does) — otherwise a space (`The Matrix`) yields an invalid URL.
    let filtered = filters::apply_all(&def.search.keywordsfilters, &input.keywords, &fctx)
        .map_err(|e| EngineError::Filter(e.to_string()))?;
    let keywords = encode_keywords(&filtered);
    let base_ctx = TemplateContext {
        keywords,
        categories: input.categories.clone(),
        config: input.config.clone(),
        query: input.query.clone(),
        result: BTreeMap::new(),
    };

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Track refusals so a search where *every* path came back 4xx/5xx fails
    // instead of returning an empty result (SKADI-T-0496). Skipping a refused
    // path is right for a multi-path definition where another path works; it is
    // wrong when there is nothing left, because the caller then records an empty
    // search and marks the indexer healthy — a Cloudflare challenge with no
    // solver looked exactly like "this tracker has nothing for you".
    let mut attempted = 0usize;
    let mut refused: Option<u16> = None;
    let mut refused_count = 0usize;
    for path in &def.search.paths {
        // A malformed path template (real defs ship typos — e.g. 1337x's TV path
        // has an unbalanced paren) skips just that path, never aborts the search.
        let Ok(rendered) = render(&path.path, &base_ctx) else {
            continue;
        };
        // Relative paths (e.g. 1337x's `search/{kw}/1/`) resolve against the site.
        let path_url = if rendered.starts_with("http") || input.base_url.is_empty() {
            rendered
        } else {
            format!(
                "{}/{}",
                input.base_url.trim_end_matches('/'),
                rendered.trim_start_matches('/')
            )
        };
        let method = match path.method.as_deref() {
            Some("post") => Method::Post,
            _ => Method::Get,
        };
        // Most definitions carry the query + categories in `search.inputs`, not
        // inline in the path: render them into a query string (GET) or form body
        // (POST). A malformed input template skips the path.
        let Ok(query) = build_inputs(&def.search.inputs, &base_ctx) else {
            continue;
        };
        let headers = build_headers(&def.search.headers, &base_ctx);
        let (url, body) = if method == Method::Post {
            let body = (!query.is_empty()).then_some(query);
            (path_url, body)
        } else if query.is_empty() {
            (path_url, None)
        } else {
            let sep = if path_url.contains('?') { '&' } else { '?' };
            (format!("{path_url}{sep}{query}"), None)
        };
        let resp = fetcher
            .fetch(FetchReq {
                method,
                url,
                headers,
                body,
            })
            .await
            .map_err(|e| EngineError::Fetch(e.0))?;
        // A failed/unparseable response on one path skips just that path (a
        // multi-path def — 1337x has 4 — must not sink on one).
        attempted += 1;
        if resp.status >= 400 {
            refused = Some(resp.status);
            refused_count += 1;
            continue;
        }
        let kind = path.response.as_ref().map_or("html", |r| r.kind.as_str());
        let (Ok(doc), Ok(rows_sel)) = (
            Document::parse(&resp.body, kind),
            render(&def.search.rows.selector, &base_ctx),
        ) else {
            continue;
        };
        let Ok(rows) = doc.rows(&rows_sel) else {
            continue;
        };
        for row in rows {
            // A row that errors during extraction is skipped, not fatal.
            if let Ok(Some(rel)) = extract_row(def, &row, &base_ctx, &fctx) {
                // Dedup by (title, infohash/download).
                let key = (
                    rel.title.clone(),
                    rel.infohash.clone().or_else(|| rel.download.clone()),
                );
                if seen.insert(key) {
                    out.push(rel);
                }
            }
        }
    }
    if let Some(status) = refused
        && attempted > 0
        && refused_count == attempted
    {
        return Err(EngineError::AllPathsRefused(status));
    }
    Ok(out)
}

fn render(tpl: &str, ctx: &TemplateContext) -> Result<String, EngineError> {
    template::render(tpl, ctx).map_err(|e| EngineError::Template(e.to_string()))
}

/// Percent-encode a keyword for URL substitution.
fn encode_keywords(kw: &str) -> String {
    utf8_percent_encode(kw, KEYWORD_ENCODE).to_string()
}

/// Render `search.inputs` into a `a=b&c=d` query/body string. Values are NOT
/// re-encoded: the `.Keywords`/`.Query.*` variables are already URL-encoded in the
/// context (as Jackett does), and the rest is literal. A `$raw` key contributes a
/// raw fragment. Returns `Err` only on a malformed input template.
fn build_inputs(
    inputs: &IndexMap<String, crate::model::Yaml>,
    ctx: &TemplateContext,
) -> Result<String, EngineError> {
    let mut parts = Vec::new();
    for (k, v) in inputs {
        let tpl = crate::filters_scalar(v);
        let rendered = render(&tpl, ctx)?;
        if k == "$raw" {
            if !rendered.is_empty() {
                parts.push(rendered);
            }
        } else {
            parts.push(format!("{k}={rendered}"));
        }
    }
    Ok(parts.join("&"))
}

/// Render `search.headers` (value is a string or single-element list) into request
/// headers. A header whose template fails to render is skipped.
fn build_headers(
    headers: &IndexMap<String, crate::model::Yaml>,
    ctx: &TemplateContext,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in headers {
        let tpl = match v {
            crate::model::Yaml::Sequence(seq) => seq.first().and_then(|f| f.as_str()),
            crate::model::Yaml::String(s) => Some(s.as_str()),
            _ => None,
        };
        if let Some(tpl) = tpl
            && let Ok(val) = render(tpl, ctx)
        {
            out.push((k.clone(), val));
        }
    }
    out
}

/// Extract one row's fields (in definition order, so `.Result.<earlier>` works) +
/// map the standard release fields.
fn extract_row(
    def: &Definition,
    row: &Row,
    base: &TemplateContext,
    fctx: &FilterCtx,
) -> Result<Option<CardigannRelease>, EngineError> {
    let mut result: IndexMap<String, String> = IndexMap::new();
    for (name, field) in &def.search.fields {
        let mut ctx = base.clone();
        ctx.result = result.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        match extract_field(field, row, &ctx, fctx)? {
            Some(v) => {
                result.insert(name.clone(), v);
            }
            None => {
                // Missing + non-optional with no default ⇒ skip the whole row
                // (it is not a valid result), mirroring cardigann.
                if !field.optional && field.default.is_none() {
                    return Ok(None);
                }
                result.insert(name.clone(), String::new());
            }
        }
    }
    let title = result.get("title").cloned().unwrap_or_default();
    if title.is_empty() {
        return Ok(None);
    }
    let rel = map_release(result, def);
    // Drop "no results" sentinels: some trackers (e.g. apibay/TPB) return a single
    // placeholder row with an all-zero infohash when nothing matched.
    if rel
        .infohash
        .as_deref()
        .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b == b'0'))
    {
        return Ok(None);
    }
    Ok(Some(rel))
}

fn extract_field(
    field: &Field,
    row: &Row,
    ctx: &TemplateContext,
    fctx: &FilterCtx,
) -> Result<Option<String>, EngineError> {
    // `text:` renders a template directly; otherwise read a selector/attribute.
    // A malformed field template/selector yields no value (the field then falls
    // back to optional/default), rather than failing the row.
    let raw = if let Some(text) = &field.text {
        render(text, ctx).ok()
    } else if let Some(sel) = &field.selector {
        match render(sel, ctx) {
            Ok(sel) => row.select(&sel, field.attribute.as_deref())?,
            Err(_) => None,
        }
    } else {
        // No selector and no text → the row's own value.
        row.select("", field.attribute.as_deref())?
    };
    let Some(val) = raw else {
        return Ok(field.default.as_ref().map(crate::filters_scalar));
    };
    // A filter failure (split index out of range, unparseable date, …) falls back
    // to the field's default/empty rather than dropping the whole row.
    match filters::apply_all(&field.filters, &val, fctx) {
        Ok(filtered) => Ok(Some(filtered)),
        Err(_) => Ok(Some(
            field
                .default
                .as_ref()
                .map(crate::filters_scalar)
                .unwrap_or_default(),
        )),
    }
}

/// Map the extracted field map onto the standard release shape.
fn map_release(result: IndexMap<String, String>, def: &Definition) -> CardigannRelease {
    let get = |k: &str| result.get(k).filter(|s| !s.is_empty()).cloned();
    let date = get("date").map(|d| normalize_date(&d));
    let categories = get("category")
        .map(|c| map_category(def, &c))
        .unwrap_or_default();
    CardigannRelease {
        title: get("title").unwrap_or_default(),
        details: get("details"),
        download: get("download").or_else(|| get("link")),
        magnet: get("magnet").or_else(|| get("magneturl")),
        infohash: get("infohash"),
        size: get("size").and_then(|s| parse_size(&s)),
        seeders: get("seeders").and_then(|s| s.parse().ok()),
        leechers: get("leechers").and_then(|s| s.parse().ok()),
        date,
        categories,
        imdb: get("imdbid").or_else(|| get("imdb")),
        raw: result,
    }
}

/// A unix-seconds string → RFC3339; otherwise passed through (filters may already
/// have produced an RFC3339 value).
fn normalize_date(d: &str) -> String {
    if let Ok(secs) = d.trim().parse::<i64>()
        && let Some(dt) = Utc.timestamp_opt(secs, 0).single()
    {
        return dt.to_rfc3339();
    }
    d.to_string()
}

/// Map a tracker category id to its Newznab category name(s), via either the
/// `categorymappings` list or the `categories` map form.
fn map_category(def: &Definition, tracker_id: &str) -> Vec<String> {
    def.caps
        .category_pairs()
        .into_iter()
        .filter(|(id, _)| id == tracker_id)
        .map(|(_, cat)| cat)
        .collect()
}

/// Parse a size value: a bare byte count, or a human string (`1.2 GB`, `700 MiB`).
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Ok(bytes) = s.parse::<u64>() {
        return Some(bytes);
    }
    let (num, unit): (String, String) = s
        .chars()
        .partition(|c| c.is_ascii_digit() || *c == '.' || *c == ',');
    let num: f64 = num.replace(',', "").trim().parse().ok()?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "b" | "" => 1.0,
        "kb" | "kib" | "k" => 1024.0,
        "mb" | "mib" | "m" => 1024.0 * 1024.0,
        "gb" | "gib" | "g" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "tib" | "t" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((num * mult) as u64)
}

// --- Document abstraction (HTML via CSS / JSON via path) ---------------------

enum Document {
    Html(Html),
    Json(Json),
}

/// A located result row.
enum Row<'a> {
    Html(scraper::ElementRef<'a>),
    Json(&'a Json),
}

impl Document {
    fn parse(body: &str, kind: &str) -> Result<Document, EngineError> {
        match kind {
            "json" => serde_json::from_str(body)
                .map(Document::Json)
                .map_err(|e| EngineError::Response(format!("invalid JSON: {e}"))),
            _ => Ok(Document::Html(Html::parse_document(body))),
        }
    }

    /// Locate result rows by selector (CSS for HTML; a JSON path for JSON).
    fn rows(&self, selector: &str) -> Result<Vec<Row<'_>>, EngineError> {
        match self {
            Document::Html(doc) => {
                // An unparseable (jQuery-flavored) selector degrades to no rows
                // rather than aborting the search.
                let Some(sel) = lenient_css(selector) else {
                    return Ok(Vec::new());
                };
                Ok(doc.select(&sel).map(Row::Html).collect())
            }
            Document::Json(root) => {
                let node = json_path(root, selector);
                Ok(match node {
                    Some(Json::Array(items)) => items.iter().map(Row::Json).collect(),
                    Some(v) => vec![Row::Json(v)],
                    None => Vec::new(),
                })
            }
        }
    }
}

impl Row<'_> {
    /// Read a field from this row by selector (+ optional attribute). An empty
    /// selector reads the row itself.
    fn select(
        &self,
        selector: &str,
        attribute: Option<&str>,
    ) -> Result<Option<String>, EngineError> {
        match self {
            Row::Html(el) => {
                let target = if selector.is_empty() {
                    Some(*el)
                } else {
                    // Unparseable selectors degrade to "no match" (the field is
                    // then optional/defaulted), never an error.
                    match lenient_css(selector) {
                        Some(sel) => el.select(&sel).next(),
                        None => None,
                    }
                };
                Ok(target.map(|t| match attribute {
                    Some(attr) => t.value().attr(attr).unwrap_or_default().to_string(),
                    None => t.text().collect::<String>().trim().to_string(),
                }))
            }
            Row::Json(v) => {
                let node = if selector.is_empty() {
                    Some(*v)
                } else {
                    json_path(v, selector)
                };
                Ok(node.map(json_scalar))
            }
        }
    }
}

/// Parse a CSS selector, falling back to a **jQuery-pseudo-stripped** form when
/// the original is unparseable — cardigann definitions use jQuery extensions
/// (`:has(...)`, `:contains(...)`, `:not(:contains(...))`) that `scraper`'s
/// standard-CSS engine rejects. Stripping them yields a coarser match (e.g.
/// `tr:has(a)` → `tr`); over-matched rows are then filtered by field extraction
/// (a row with no title is dropped). `None` only if even the stripped form fails.
fn lenient_css(selector: &str) -> Option<Selector> {
    if let Ok(s) = Selector::parse(selector) {
        return Some(s);
    }
    let stripped = strip_jquery_pseudos(selector);
    Selector::parse(&stripped).ok()
}

/// Remove `:has(...)` / `:contains(...)` / `:not(...)` with balanced parens.
fn strip_jquery_pseudos(selector: &str) -> String {
    const PSEUDOS: &[&str] = &[":has(", ":contains(", ":not("];
    let mut s = selector.to_string();
    'again: loop {
        for p in PSEUDOS {
            if let Some(at) = s.find(p) {
                let open = at + p.len() - 1;
                if let Some(close) = matching_paren(&s, open) {
                    s.replace_range(at..=close, "");
                    continue 'again;
                }
            }
        }
        break;
    }
    let t = s.trim();
    if t.is_empty() {
        "*".to_string()
    } else {
        t.to_string()
    }
}

/// Index of the `)` matching the `(` at `open` (byte index), if any.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Resolve a cardigann JSON path (`$`, `key`, `$.a.b`, `$[0].c`) against `value`.
fn json_path<'a>(value: &'a Json, path: &str) -> Option<&'a Json> {
    let path = path.trim();
    if path.is_empty() || path == "$" {
        return Some(value);
    }
    let path = path.strip_prefix('$').unwrap_or(path);
    let mut cur = value;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        // Handle a trailing `[i]` index on a key, or a bare `[i]`.
        let (key, idx) = split_index(seg);
        if !key.is_empty() {
            cur = cur.get(key)?;
        }
        if let Some(i) = idx {
            cur = cur.get(i)?;
        }
    }
    Some(cur)
}

/// Split `name[3]` → (`name`, Some(3)); `name` → (`name`, None); `[3]` → (``, Some(3)).
fn split_index(seg: &str) -> (&str, Option<usize>) {
    if let Some(open) = seg.find('[')
        && seg.ends_with(']')
    {
        let key = &seg[..open];
        let idx = seg[open + 1..seg.len() - 1].parse().ok();
        return (key, idx);
    }
    (seg, None)
}

/// Render a JSON scalar to a string (numbers/bools become their text).
fn json_scalar(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}
