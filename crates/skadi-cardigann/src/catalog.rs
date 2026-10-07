//! The definition catalog (SKADI-T-0260): load a directory of Cardigann YAML
//! definitions (warn-skipping the unparseable), index them by id, and summarize
//! each for the add-tracker picker. Pure — filesystem only; the network sync that
//! populates the directory lives in `skadi-indexers`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use crate::LoadError;
use crate::model::Definition;

/// One user-facing setting, summarized for the config form.
#[derive(Debug, Clone, Serialize)]
pub struct SettingSummary {
    pub name: String,
    pub label: String,
    /// `text` / `password` / `checkbox` / `select`.
    pub kind: String,
    pub default: Option<String>,
    /// Help text shown under the input (SKADI-T-0699): the text of the
    /// definition's `info_<name>` row, else a standard text for a common
    /// setting (cookie, user agent, login). `None` when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// A browsable summary of a definition for the add-tracker picker.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub language: String,
    /// `public` / `private` / `semi-private`.
    pub privacy: String,
    pub needs_login: bool,
    /// Real input settings (the non-`info` rows) the user must fill in.
    pub settings: Vec<SettingSummary>,
    /// Distinct Newznab category names the tracker exposes.
    pub categories: Vec<String>,
    /// Supported search modes (`search`, `movie-search`, …).
    pub search_modes: Vec<String>,
}

/// Summarize a definition for the catalog.
#[must_use]
pub fn summarize(def: &Definition) -> CatalogEntry {
    let mut categories: Vec<String> = def
        .caps
        .category_pairs()
        .into_iter()
        .map(|(_, cat)| cat)
        .collect();
    categories.sort();
    categories.dedup();
    let mut search_modes: Vec<String> = def.caps.modes.keys().cloned().collect();
    search_modes.sort();

    CatalogEntry {
        id: def.id.clone(),
        name: def.name.clone(),
        description: def.description.clone(),
        language: def.language.clone(),
        privacy: if def.privacy.is_empty() {
            "public".into()
        } else {
            def.privacy.clone()
        },
        needs_login: def.needs_login(),
        settings: def
            .settings
            .iter()
            // `info` / `info_*` rows are display-only notes, not inputs.
            .filter(|s| !s.kind.starts_with("info"))
            .map(|s| SettingSummary {
                name: s.name.clone(),
                label: s.label.clone(),
                kind: if s.kind.is_empty() {
                    "text".into()
                } else {
                    s.kind.clone()
                },
                default: s.default.as_ref().map(crate::filters_scalar),
                help: setting_help(def, &s.name),
            })
            .collect(),
        categories,
        search_modes,
    }
}

/// Standard help for settings that many definitions share. Upstream marks some
/// of them with a text-less `info_cookie` / `info_useragent` row and lets the
/// client supply the words; these are skadi's.
const COMMON_HELP: &[(&str, &str)] = &[
    (
        "cookie",
        "The cookie of a browser session that is logged in to the site. Log in with your browser, open the developer tools, and copy the Cookie header of a request to the site.",
    ),
    (
        "useragent",
        "The User-Agent of the browser that the cookie comes from. The site can refuse the cookie with a different User-Agent.",
    ),
    ("username", "The user name of your account on the site."),
    (
        "password",
        "The password of your account on the site. Skadi keeps it encrypted.",
    ),
    ("apikey", "The API key from your profile page on the site."),
];

/// The help text for the setting `name` of `def`: the text of an `info` row
/// named `info_<name>` (or `info_<prefix>` for a prefix of `name`, as
/// `info_download` explains `downloadlink` and `downloadlink2`), else
/// [`COMMON_HELP`]. HTML in the definition text becomes plain text.
fn setting_help(def: &Definition, name: &str) -> Option<String> {
    let info_text = |s: &crate::model::Setting| {
        s.default
            .as_ref()
            .map(crate::filters_scalar)
            .map(|t| plain_text(&t))
            .filter(|t| !t.is_empty())
    };
    let infos = || {
        def.settings
            .iter()
            .filter(|s| s.kind.starts_with("info"))
            .filter_map(|s| Some((s.name.strip_prefix("info_")?, s)))
    };
    infos()
        .find(|(about, _)| *about == name)
        .and_then(|(_, s)| info_text(s))
        .or_else(|| {
            infos()
                .filter(|(about, _)| !about.is_empty() && name.starts_with(about))
                .find_map(|(_, s)| info_text(s))
        })
        .or_else(|| {
            COMMON_HELP
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, h)| (*h).to_string())
        })
}

/// `text` without its HTML tags: a `<br>` (or `</br>`, `<li>`) becomes a
/// space, other tags go, the common entities are decoded, and runs of white
/// space become one space.
fn plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) => {
                let tag = rest[start + 1..start + end]
                    .trim_start_matches('/')
                    .to_ascii_lowercase();
                if tag.starts_with("br") || tag.starts_with("li") || tag.starts_with('p') {
                    out.push(' ');
                }
                rest = &rest[start + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An in-memory index of definitions keyed by id, plus their catalog summaries.
#[derive(Debug, Default)]
pub struct Catalog {
    defs: BTreeMap<String, Arc<Definition>>,
    entries: BTreeMap<String, CatalogEntry>,
}

impl Catalog {
    /// Build from parsed definitions (e.g. a bundled set).
    #[must_use]
    pub fn from_definitions(defs: impl IntoIterator<Item = Definition>) -> Self {
        let mut c = Catalog::default();
        for d in defs {
            c.insert(d);
        }
        c
    }

    fn insert(&mut self, def: Definition) {
        let id = def.id.clone();
        self.entries.insert(id.clone(), summarize(&def));
        self.defs.insert(id, Arc::new(def));
    }

    /// Load every `*.yml`/`*.yaml` under `dir` (recursively), **warn-skipping**
    /// definitions that fail to parse. A later id wins over an earlier duplicate
    /// (so the sync can layer newer schema versions over older).
    ///
    /// # Errors
    /// Propagates filesystem errors reading `dir`; per-definition parse failures
    /// are collected, not returned as errors.
    pub fn load_dir(dir: &Path) -> std::io::Result<(Catalog, Vec<LoadError>)> {
        let mut catalog = Catalog::default();
        let mut errors = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d)? {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let is_yaml = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "yml" || e == "yaml");
                if !is_yaml {
                    continue;
                }
                let text = std::fs::read_to_string(&path)?;
                match crate::parse_definition(&text) {
                    Ok(def) => catalog.insert(def),
                    Err(e) => errors.push(e),
                }
            }
        }
        Ok((catalog, errors))
    }

    /// The parsed definition for `id` (for building an indexer).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<Definition>> {
        self.defs.get(id).cloned()
    }

    /// The catalog summary for `id`.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.get(id)
    }

    /// All catalog entries, id-sorted.
    #[must_use]
    pub fn list(&self) -> Vec<&CatalogEntry> {
        self.entries.values().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_summarizes_fixture_directory() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let (cat, errs) = Catalog::load_dir(Path::new(dir)).unwrap();
        assert!(errs.is_empty(), "fixtures should all parse: {errs:?}");
        assert!(cat.len() >= 6, "loaded {} defs", cat.len());

        let tpb = cat.entry("thepiratebay").expect("tpb in catalog");
        assert_eq!(tpb.privacy, "public");
        assert!(!tpb.needs_login);
        assert!(tpb.categories.iter().any(|c| c == "Audio/Audiobook"));
        assert!(tpb.search_modes.iter().any(|m| m == "movie-search"));
        // `apiurl` is a real text setting; the `info_*` rows are excluded.
        assert!(
            tpb.settings
                .iter()
                .any(|s| s.name == "apiurl" && s.kind == "text")
        );
        assert!(tpb.settings.iter().all(|s| s.kind != "info"));
        // The full definition is retrievable for building an indexer.
        assert!(cat.get("thepiratebay").is_some());
    }

    #[test]
    fn private_tracker_flagged_needs_login() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let (cat, _) = Catalog::load_dir(Path::new(dir)).unwrap();
        let tl = cat.entry("torrentleech").expect("torrentleech in catalog");
        assert!(tl.needs_login);
    }

    #[test]
    fn a_setting_takes_its_help_from_its_info_row_as_plain_text() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let (cat, _) = Catalog::load_dir(Path::new(dir)).unwrap();
        let tl = cat.entry("torrentleech").expect("torrentleech in catalog");
        let help = |name: &str| {
            tl.settings
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.help.clone())
        };
        let token = help("alt2fatoken").expect("alt2fatoken has an info row");
        assert!(token.contains("Alt 2FA Token"), "{token}");
        assert!(!token.contains('<'), "tags are stripped: {token}");
        // No info row: the standard text.
        assert!(help("username").is_some_and(|h| h.contains("user name")));
        assert!(help("password").is_some_and(|h| h.contains("encrypted")));
    }

    #[test]
    fn an_info_row_named_for_a_prefix_explains_each_matching_setting() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let (cat, _) = Catalog::load_dir(Path::new(dir)).unwrap();
        let x = cat.entry("1337x").expect("1337x in catalog");
        for name in ["downloadlink", "downloadlink2"] {
            let s = x.settings.iter().find(|s| s.name == name).expect(name);
            assert!(
                s.help.as_deref().is_some_and(|h| h.contains("magnet")),
                "{name}: {:?}",
                s.help
            );
        }
        let sort = x.settings.iter().find(|s| s.name == "sort").expect("sort");
        assert_eq!(sort.help, None, "no info row, no common text");
    }

    #[test]
    fn plain_text_drops_tags_and_decodes_entities() {
        assert_eq!(
            plain_text("Only <b>Other</b>.</br>Add 8000 &amp; more"),
            "Only Other. Add 8000 & more"
        );
        assert_eq!(plain_text("<ol><li>One</li><li>Two</li></ol>"), "One Two");
        assert_eq!(plain_text("a < b"), "a < b");
    }
}
