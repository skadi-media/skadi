//! C21 parity steps for Sonarr/Radarr naming features (SKADI-T-0420).
use cucumber::then;
use skadi_naming::{ColonStyle, render, sanitize_component_with, validate_template};

use crate::bdd_support::World;

#[then(expr = "the format modifier {string} pads to {string}")]
fn format_modifier(w: &mut World, template: String, want: String) {
    // Sonarr's `{season:00}` / `{absolute:000}`: a run of zeros is the minimum
    // width. Applied only to a value that parses as a number — padding a title
    // would be nonsense, and mangling it silently would be worse than ignoring
    // the modifier.
    assert_eq!(render(&template, &w.tokens()), want);
}

#[then(expr = "the template {string} is reported invalid mentioning {string}")]
fn validate(w: &mut World, template: String, needle: String) {
    // Validated at **save** time: a typo'd token renders to a plausible-looking
    // wrong path, so the operator would otherwise find out when files land
    // somewhere unexpected.
    let known: Vec<&str> = w.tokens.iter().map(|(k, _)| k.as_str()).collect();
    let err =
        validate_template(&template, &known).expect_err("the template should be rejected on save");
    let text = err.to_string();
    assert!(
        text.to_lowercase().contains(&needle.to_lowercase()),
        "error {text:?} does not mention {needle:?}"
    );
}

#[then(expr = "the rendered name contains the quality {string}")]
fn quality_token(w: &mut World, want: String) {
    let r = w.rendered();
    assert!(r.contains(&want), "{r:?} lacks the quality {want:?}");
}

#[then(expr = "the rendered name contains the release group {string}")]
fn release_group_token(w: &mut World, want: String) {
    let r = w.rendered();
    assert!(r.contains(&want), "{r:?} lacks the release group {want:?}");
}

#[then(expr = "the colon renders as {string} under the {string} colon style")]
fn colon_style(w: &mut World, want: String, style: String) {
    // A colon must always go (illegal on Windows, awkward over SMB), but *what it
    // becomes* changes how the title reads, which is the operator's call.
    let subject = w.subject.clone().expect("a sanitize step ran first");
    let style = ColonStyle::from_str_or_default(&style);
    assert_eq!(sanitize_component_with(&subject, w.space(), style), want);
}
