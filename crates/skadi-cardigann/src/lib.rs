//! `skadi-cardigann` — a native Rust engine for Jackett/Prowlarr **Cardigann**
//! tracker definitions (SKADI-I-0036). This crate owns definition parsing +
//! (in later tasks) the template/filter engine, the login/search flow executor,
//! and the scrape→normalize chain. It is deliberately **dependency-light** (no
//! `skadi-indexers`/HTTP): the engine returns raw [`model::Definition`]s and (later)
//! `CardigannRelease`s through an injected fetcher, so `skadi-indexers` can adapt
//! them to its `Indexer` trait without a dependency cycle.
//!
//! This file (T-0255) provides the data model + a **lenient** parser: a single
//! malformed definition produces a [`LoadError`] and is skipped, never failing a
//! batch [`load_all`].

pub mod catalog;
pub mod download;
pub mod engine;
pub mod filters;
pub mod login;
pub mod model;
pub mod template;

pub use download::resolve_download;
pub use login::{LoginOutcome, login};

pub use catalog::{Catalog, CatalogEntry, SettingSummary};
pub use engine::{
    CardigannRelease, EngineError, FetchError, FetchReq, FetchResp, Fetcher, Method, SearchInput,
    search,
};

/// Render a YAML scalar (string / number / bool) to a `String`; non-scalars → "".
/// Shared by the config + field-default paths.
pub(crate) fn filters_scalar(v: &model::Yaml) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

pub use model::{
    Caps, CategoryMapping, Definition, Download, Field, Filter, Login, Response, Rows, Search,
    SearchPath, Setting, Yaml,
};

use serde::Deserialize;

/// A definition that failed to parse — carries the offending `id` (best-effort)
/// and a human-readable reason, so a batch load can warn-and-skip it.
#[derive(Debug, Clone)]
pub struct LoadError {
    /// The definition `id` if it could be recovered from the raw YAML.
    pub id: Option<String>,
    pub message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.id {
            Some(id) => write!(f, "definition '{id}': {}", self.message),
            None => write!(f, "definition: {}", self.message),
        }
    }
}

impl std::error::Error for LoadError {}

/// Parse one Cardigann definition from YAML. On failure, the `id` is recovered
/// (best-effort) for diagnostics.
///
/// # Errors
/// Returns a [`LoadError`] if the YAML is malformed or violates the model
/// (e.g. a field value of the wrong shape) — the caller warn-skips it.
pub fn parse_definition(yaml: &str) -> Result<Definition, LoadError> {
    serde_yaml::from_str::<Definition>(yaml).map_err(|e| LoadError {
        id: recover_id(yaml),
        message: e.to_string(),
    })
}

/// Parse many definitions, **partitioning** into the ones that loaded and the
/// ones that failed — the lenient batch entry point for the definition sync
/// (SKADI-T-0260). Never fails as a whole.
#[must_use]
pub fn load_all<'a, I>(defs: I) -> (Vec<Definition>, Vec<LoadError>)
where
    I: IntoIterator<Item = &'a str>,
{
    let mut ok = Vec::new();
    let mut err = Vec::new();
    for yaml in defs {
        match parse_definition(yaml) {
            Ok(d) => ok.push(d),
            Err(e) => err.push(e),
        }
    }
    (ok, err)
}

/// Pull just the `id` from a raw definition, for error context.
fn recover_id(yaml: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct IdOnly {
        id: String,
    }
    serde_yaml::from_str::<IdOnly>(yaml).ok().map(|x| x.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recover_id_from_malformed_body() {
        // Valid `id`, but `caps` is the wrong shape → parse fails, id recovered.
        let yaml = "id: badtracker\nname: Bad\ncaps: 42\nsearch: {}\n";
        let err = parse_definition(yaml).unwrap_err();
        assert_eq!(err.id.as_deref(), Some("badtracker"));
    }

    #[test]
    fn load_all_partitions_good_and_bad() {
        let good = "id: t\nname: T\ncaps: {}\nsearch:\n  rows:\n    selector: tr\n";
        let bad = "id: b\nname: B\nsettings: not-a-list\n";
        let (ok, err) = load_all([good, bad]);
        assert_eq!(ok.len(), 1);
        assert_eq!(err.len(), 1);
        assert_eq!(ok[0].id, "t");
        assert_eq!(err[0].id.as_deref(), Some("b"));
    }

    #[test]
    fn minimal_definition_parses() {
        let yaml = "id: x\nname: X\n";
        let d = parse_definition(yaml).unwrap();
        assert_eq!(d.id, "x");
        assert!(d.caps.categorymappings.is_empty());
        assert!(!d.needs_login());
    }
}
