//! Quality recording on adoption vs regular import (SKADI-T-0399 review).
//!
//! The domain adoption commits (`skadi-movies::import::commit_item`,
//! `skadi-tv::import::commit_one`, `skadi-audiobooks::import::place_and_mark_imported`)
//! derive quality from the **file name only** and fall back to
//! `default_definitions()[0]` (SDTV) when nothing parses; they never probe the file.
//! The regular hunter import (`skadi-hunter::steps::import`) probes the placed file and
//! reconciles the parse with the real resolution. These steps exercise the shared
//! `skadi-quality` primitives both paths use, so the gap is provable in-process.
use cucumber::{given, then, when};
use skadi_core::VideoInfo;
use skadi_quality::{default_definitions, parse, reconcile_with_probe, to_quality};

use crate::bdd_support::World;

#[given(expr = "an existing library file named {string}")]
fn library_file_named(w: &mut World, name: String) {
    w.notes.push(name);
}

#[when("its quality is derived from the file name the way adoption does")]
fn derive_like_adoption(w: &mut World) {
    let name = w.notes.last().expect("file name").clone();
    let parsed = parse(&name);
    let defs = default_definitions();
    w.quality_name = to_quality(&parsed, &defs).map(|q| {
        defs.iter()
            .find(|d| d.id == q.id)
            .map(|d| d.name.clone())
            .unwrap_or_default()
    });
    w.parsed = Some(parsed);
}

#[when(expr = "the file is probed at {int}x{int} and reconciled with the name")]
fn probe_and_reconcile(w: &mut World, width: u32, height: u32) {
    let name = w.notes.last().expect("file name").clone();
    let tier = VideoInfo {
        width,
        height,
        codec: None,
        profile: None,
        dynamic_range: None,
    }
    .resolution_tier();
    let parsed = reconcile_with_probe(&parse(&name), Some(tier), None);
    let defs = default_definitions();
    w.quality_name = to_quality(&parsed, &defs).map(|q| {
        defs.iter()
            .find(|d| d.id == q.id)
            .map(|d| d.name.clone())
            .unwrap_or_default()
    });
    w.parsed = Some(parsed);
}

#[then("no quality can be parsed from the name")]
fn no_quality(w: &mut World) {
    assert_eq!(w.quality_name, None, "parsed {:?}", w.parsed);
}

#[then(expr = "the quality is {string}")]
fn quality_is(w: &mut World, want: String) {
    assert_eq!(
        w.quality_name.as_deref(),
        Some(want.as_str()),
        "parsed {:?}",
        w.parsed
    );
}

#[then(expr = "the lowest built-in definition the adoption fallback records is {string}")]
fn lowest_is(_w: &mut World, want: String) {
    // This is exactly `quality_id.unwrap_or_else(|| default_definitions()[0].id)` in
    // movies/import.rs:761, tv/import.rs:776, audiobooks (its own defs) — the origin
    // of the 18.4k SDTV rows in SKADI-T-0399.
    assert_eq!(default_definitions()[0].name, want);
}

#[then("an explicit Unknown quality tier exists for unassessed files")]
fn unknown_tier_exists(_w: &mut World) {
    // SKADI-T-0399 plan: adoption must record *Unknown*, which the upgrade sweep
    // skips — not the lowest real tier, which it treats as "below cutoff".
    let defs = default_definitions();
    assert!(
        defs.iter().any(|d| d.name.eq_ignore_ascii_case("unknown")),
        "no Unknown definition among {:?}",
        defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );
}

#[then("the probed resolution is reflected in the parse")]
fn probed_resolution_reflected(w: &mut World) {
    let p = w.parsed.as_ref().expect("parsed");
    assert!(p.resolution.is_some(), "resolution missing: {p:?}");
}

#[then("a quality is derived from the probe alone")]
fn quality_from_probe_alone(w: &mut World) {
    // A container reveals pixels but not the *source* (BluRay vs WEB vs HDTV), and
    // `to_quality` requires both — so a probe can never back-fill an adopted file's
    // quality by itself. Back-filling SKADI-T-0399 needs a resolution-only lookup
    // (e.g. "1080p, unknown source") in the quality model.
    assert!(
        w.quality_name.is_some(),
        "to_quality is None for {:?}: resolution known, source unknown",
        w.parsed
    );
}
