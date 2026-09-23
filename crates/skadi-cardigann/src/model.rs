//! The Cardigann tracker-definition data model (SKADI-T-0255).
//!
//! A faithful-but-lenient serde view of the Jackett/Prowlarr definition YAML
//! (schema v9–v11). Heterogeneous fields (filter `args`, `default`, category
//! `id`, `options`) keep a raw [`Yaml`] value and are interpreted later by the
//! template/filter engine (SKADI-T-0256) and the search executor (SKADI-T-0257);
//! the `login`/`download` blocks are modelled loosely here and executed in
//! SKADI-T-0258. Parsing is **lenient**: unknown keys are ignored (serde
//! default), structural sections default to empty, and only `id`/`name` are hard
//! requirements — so one malformed definition warns and is skipped, never
//! breaking a batch load.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::Deserialize;

/// A raw, not-yet-interpreted YAML value (filter args, defaults, options, …).
pub type Yaml = serde_yaml::Value;

/// One tracker definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub language: String,
    /// `public` / `private` / `semi-private`.
    #[serde(rename = "type", default)]
    pub privacy: String,
    #[serde(default)]
    pub encoding: String,
    /// Candidate base URLs (the first reachable is the site link).
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub legacylinks: Vec<String>,
    #[serde(default)]
    pub caps: Caps,
    /// User-facing config fields (api key, username/password, toggles, …).
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default)]
    pub login: Option<Login>,
    #[serde(default)]
    pub search: Search,
    #[serde(default)]
    pub download: Option<Download>,
}

impl Definition {
    /// Whether this definition needs an authenticated session (has a `login` block
    /// or is marked non-public).
    #[must_use]
    pub fn needs_login(&self) -> bool {
        self.login.is_some() || matches!(self.privacy.as_str(), "private" | "semi-private")
    }
}

/// Capabilities: tracker→Newznab category map + supported search modes/params.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caps {
    /// The list form: `- {id, cat, desc}` (most definitions).
    #[serde(default)]
    pub categorymappings: Vec<CategoryMapping>,
    /// The **map** form some definitions use instead: `{ <tracker-id>: <Newznab-cat> }`
    /// (raw — keys/values string-coerced in the engine, since ids may be ints).
    #[serde(default)]
    pub categories: Option<Yaml>,
    /// `search` / `movie-search` / `tv-search` / `music-search` / `book-search`
    /// → the params that mode accepts (`q`, `imdbid`, `tmdbid`, `season`, `ep`, …).
    #[serde(default)]
    pub modes: BTreeMap<String, Vec<String>>,
}

impl Caps {
    /// All `(tracker-id, Newznab-cat-name)` pairs, from both the list and map forms.
    #[must_use]
    pub fn category_pairs(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .categorymappings
            .iter()
            .map(|m| (m.id.clone(), m.cat.clone()))
            .collect();
        if let Some(Yaml::Mapping(map)) = &self.categories {
            for (k, v) in map {
                if let (Some(id), Some(cat)) = (yaml_scalar(k), yaml_scalar(v)) {
                    out.push((id, cat));
                }
            }
        }
        out
    }
}

/// A YAML scalar (string/number/bool) as a string, else `None`.
fn yaml_scalar(v: &Yaml) -> Option<String> {
    match v {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// One tracker-category → Newznab-category mapping.
#[derive(Debug, Clone, Deserialize)]
pub struct CategoryMapping {
    /// The tracker's own category id (numeric or string).
    #[serde(deserialize_with = "de::scalar_string")]
    pub id: String,
    /// The Newznab category name (`Movies/HD`, `Audio/Audiobook`, …).
    pub cat: String,
    #[serde(default)]
    pub desc: String,
}

/// A user-configurable setting (rendered into the indexer config form, T-0261).
#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    pub name: String,
    #[serde(default)]
    pub label: String,
    /// `text` / `password` / `checkbox` / `select` / `info`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Default value (string / bool / number) — raw until rendered.
    #[serde(default)]
    pub default: Option<Yaml>,
    /// Options for a `select` (value → label) — raw map.
    #[serde(default)]
    pub options: Option<Yaml>,
}

/// The `login` block — modelled loosely; executed in SKADI-T-0258.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Login {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub submitpath: Option<String>,
    /// `form` / `post` / `get` / `cookie` / `oneurl`.
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub form: Option<String>,
    #[serde(default)]
    pub inputs: IndexMap<String, Yaml>,
    #[serde(default)]
    pub error: Vec<Yaml>,
    #[serde(default)]
    pub test: Option<Yaml>,
}

/// The `search` block.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Search {
    #[serde(default)]
    pub paths: Vec<SearchPath>,
    /// Filters applied to the keyword string before it is templated into a path.
    #[serde(default)]
    pub keywordsfilters: Vec<Filter>,
    /// Query/POST inputs (order preserved) — `name → templated value`. The query
    /// (and categories) of **most** definitions live here, not inline in the path.
    /// A `$raw` key contributes a raw query-string fragment.
    #[serde(default)]
    pub inputs: IndexMap<String, Yaml>,
    /// Custom request headers (e.g. a browser `User-Agent` for anti-bot trackers);
    /// each value is a string or a single-element list.
    #[serde(default)]
    pub headers: IndexMap<String, Yaml>,
    #[serde(default)]
    pub rows: Rows,
    /// Field-name → extraction spec (`title`, `download`, `size`, `seeders`, …),
    /// including synthetic intermediates referenced as `.Result.<name>`. **Order
    /// preserved** — a field's `text` may reference an earlier field via `.Result`.
    #[serde(default)]
    pub fields: IndexMap<String, Field>,
}

/// One search request template.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchPath {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub response: Option<Response>,
    #[serde(default)]
    pub inputs: Option<Yaml>,
    #[serde(default)]
    pub categories: Option<Yaml>,
}

/// The declared response shape of a search path.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Response {
    /// `html` (default) / `json` / `xml`.
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// How result rows are located in the response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Rows {
    /// CSS selector (HTML) or JSON path (`$.results[*]`) — may be templated.
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub count: Option<Yaml>,
    #[serde(default)]
    pub after: Option<Yaml>,
    #[serde(default)]
    pub filters: Vec<Filter>,
}

/// Extraction of one field from a row.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Field {
    #[serde(default)]
    pub selector: Option<String>,
    /// A literal/templated value used instead of a selector.
    #[serde(default)]
    pub text: Option<String>,
    /// The HTML attribute to read (`href`, `src`, …) instead of the text.
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub default: Option<Yaml>,
    /// Multiple alternative selectors (raw; resolved in T-0257).
    #[serde(default)]
    pub selectors: Option<Yaml>,
}

/// One named filter in a selector/keyword pipeline (`name` + raw `args`).
#[derive(Debug, Clone, Deserialize)]
pub struct Filter {
    pub name: String,
    /// `null` / a string / a heterogeneous list (`["/", 3]`) — interpreted in T-0256.
    #[serde(default)]
    pub args: Yaml,
}

/// The `download` block — modelled loosely; executed in SKADI-T-0258.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Download {
    #[serde(default)]
    pub selectors: Option<Yaml>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub infohash: Option<Yaml>,
    #[serde(default)]
    pub before: Option<Yaml>,
}

mod de {
    use serde::{Deserialize, Deserializer};

    /// Deserialize a YAML scalar (string / number / bool) into a `String` — used
    /// for category ids, which appear as either `100` or `"100"` across defs.
    pub fn scalar_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        match serde_yaml::Value::deserialize(d)? {
            serde_yaml::Value::String(s) => Ok(s),
            serde_yaml::Value::Number(n) => Ok(n.to_string()),
            serde_yaml::Value::Bool(b) => Ok(b.to_string()),
            other => Err(serde::de::Error::custom(format!(
                "expected a scalar category id, got {other:?}"
            ))),
        }
    }
}
