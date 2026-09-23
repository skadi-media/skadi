//! Typed indexer configuration (`IndexerConfig`), the settings-row shape the
//! daemon stores and the provider factory builds from.
use std::sync::Arc;

use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use skadi_core::IndexerId;
use skadi_indexers::IndexerConfig;

use crate::bdd_support::{World, fast_http, fixtures};

#[given("the indexer settings row:")]
fn settings_row(w: &mut World, step: &Step) {
    let raw = step.docstring().expect("a JSON doc string");
    w.config_json = Some(serde_json::from_str(raw).expect("valid JSON"));
}

fn parsed(w: &World) -> Result<IndexerConfig, String> {
    serde_json::from_value(w.config_json.clone().expect("settings row")).map_err(|e| e.to_string())
}

#[when("the provider factory builds the indexer")]
fn build(w: &mut World) {
    let cfg = match parsed(w) {
        Ok(c) => c,
        Err(e) => {
            w.build_error = Some(format!("deserialize: {e}"));
            return;
        }
    };
    let result = if cfg.cardigann_definition_id().is_some() {
        let mut def =
            skadi_cardigann::parse_definition(&fixtures::definition("public_json")).unwrap();
        def.links = vec!["https://synth.test/".into()];
        let def = Arc::new(def);
        cfg.build_cardigann(IndexerId::new(), def, None, None, None)
    } else {
        cfg.build(IndexerId::new(), "key".into(), fast_http())
    };
    match result {
        Ok(ix) => {
            w.indexer = Some(ix);
            w.build_error = None;
        }
        Err(e) => w.build_error = Some(e.to_string()),
    }
}

#[then("the indexer builds")]
fn builds(w: &mut World) {
    assert!(w.build_error.is_none(), "build failed: {:?}", w.build_error);
    assert!(w.indexer.is_some());
}

#[then(regex = r#"^the build is rejected with a validation error mentioning "([^"]*)"$"#)]
fn rejected(w: &mut World, needle: String) {
    let err = w.build_error.as_deref().expect("expected a build failure");
    assert!(
        err.contains("validation") || err.contains("Validation") || err.contains(&needle),
        "not a validation error: {err}"
    );
    assert!(err.contains(&needle), "error {err:?} lacks {needle:?}");
}

#[then("the settings row is rejected as an unknown kind")]
fn unknown_kind(w: &mut World) {
    let err = w.build_error.as_deref().expect("expected a failure");
    assert!(err.starts_with("deserialize:"), "{err}");
}

#[then(regex = r#"^the settings row round-trips with name "([^"]*)"$"#)]
fn round_trips(w: &mut World, name: String) {
    let cfg = parsed(w).expect("parses");
    assert_eq!(cfg.name(), name);
    let json = serde_json::to_value(&cfg).unwrap();
    let back: IndexerConfig = serde_json::from_value(json).unwrap();
    assert_eq!(back, cfg);
}

#[then(regex = r"^the settings row (is|is not) a built-in indexer$")]
fn builtin(w: &mut World, yes: String) {
    let cfg = parsed(w).expect("parses");
    assert_eq!(cfg.is_builtin(), yes == "is");
}

/// Sonarr/Radarr per-indexer settings the row should be able to carry.
#[then(regex = r#"^the stored settings row carries the per-indexer field "([^"]+)"$"#)]
fn carries_field(w: &mut World, field: String) {
    let cfg = parsed(w).expect("parses");
    let json = serde_json::to_value(&cfg).unwrap();
    assert!(
        json.get(&field).is_some(),
        "settings row has no {field:?} field: {json}"
    );
}
