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
            })
            .collect(),
        categories,
        search_modes,
    }
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
}
