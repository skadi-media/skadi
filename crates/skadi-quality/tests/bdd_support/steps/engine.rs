//! C11: quality definitions, profiles (`decide_id`), custom-format scoring
//! (`score_breakdown`) and title relevance.

use cucumber::{given, then, when};

use skadi_core::{CustomFormatId, ProfileId};
use skadi_quality::{
    CustomFormat, CustomFormatScore, Decision, FormatMode, FormatRule, QualityProfile, ReleaseMeta,
    default_definitions, default_profiles, parse, score_breakdown, standard_profile,
    title_relevance, to_quality,
};

use crate::bdd_support::World;

fn q(w: &World, name: &str) -> skadi_core::QualityId {
    w.definitions
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no built-in quality {name:?}"))
        .id
}

#[given("the built-in quality definitions")]
fn defs(w: &mut World) {
    w.definitions = default_definitions();
}

#[given("the standard profile")]
fn standard(w: &mut World) {
    if w.definitions.is_empty() {
        w.definitions = default_definitions();
    }
    w.profile = Some(standard_profile(&w.definitions));
}

#[given(expr = "a profile allowing {string} with cutoff {string}")]
fn profile_allowing(w: &mut World, names: String, cutoff: String) {
    if w.definitions.is_empty() {
        w.definitions = default_definitions();
    }
    let allowed: Vec<_> = names.split(',').map(|n| q(w, n.trim())).collect();
    w.profile = Some(QualityProfile {
        id: ProfileId::new(),
        name: names.clone(),
        allowed,
        cutoff: q(w, &cutoff),
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    });
}

#[given("upgrades are disabled")]
fn no_upgrades(w: &mut World) {
    w.profile.as_mut().unwrap().upgrade_allowed = false;
}

#[given(expr = "the library holds {string}")]
fn holds(w: &mut World, name: String) {
    w.current = Some(q(w, &name));
}

#[given("the library holds a file of unknown quality")]
fn holds_unknown(w: &mut World) {
    w.current = Some(skadi_core::QualityId::new());
}

#[when(expr = "a {string} candidate is judged")]
fn judge(w: &mut World, name: String) {
    let cand = q(w, &name);
    w.decision = Some(w.profile.as_ref().unwrap().decide_id(cand, w.current));
}

#[then(expr = "the verdict is {word}")]
fn verdict(w: &mut World, word: String) {
    let want = match word.as_str() {
        "Accept" => Decision::Accept,
        "Upgrade" => Decision::Upgrade,
        "Reject" => Decision::Reject,
        "MeetsCutoff" => Decision::MeetsCutoff,
        other => panic!("unknown verdict {other}"),
    };
    assert_eq!(w.decision, Some(want));
}

#[when("the title is classified")]
fn classify(w: &mut World) {
    if w.definitions.is_empty() {
        w.definitions = default_definitions();
    }
    let p = parse(&w.title);
    w.classified = to_quality(&p, &w.definitions).map(|q| q.id);
}

#[then(expr = "it classifies as {string}")]
fn classifies(w: &mut World, name: String) {
    let want = q(w, &name);
    let got = w.classified.and_then(|id| {
        w.definitions
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
    });
    assert_eq!(
        w.classified,
        Some(want),
        "classified as {got:?}, wanted {name}"
    );
}

#[then("it does not classify to any quality")]
fn unclassified(w: &mut World) {
    assert_eq!(w.classified, None);
}

#[then(expr = "the definitions are ranked with {string} below {string}")]
fn ranked(w: &mut World, lo: String, hi: String) {
    let pos = |n: &str| w.definitions.iter().position(|d| d.name == n).unwrap();
    assert!(pos(&lo) < pos(&hi));
}

#[then("definition ids are stable across calls")]
fn stable_ids(w: &mut World) {
    let again = default_definitions();
    assert_eq!(
        w.definitions.iter().map(|d| d.id).collect::<Vec<_>>(),
        again.iter().map(|d| d.id).collect::<Vec<_>>()
    );
}

#[then(expr = "the built-in profiles include {string}")]
fn builtin_profiles(w: &mut World, names: String) {
    let profiles = default_profiles(&w.definitions);
    for n in names.split(',').map(str::trim) {
        assert!(profiles.iter().any(|p| p.name == n), "missing profile {n}");
    }
}

#[then(expr = "the standard profile allows {int} qualities with cutoff {string}")]
fn standard_shape(w: &mut World, n: usize, cutoff: String) {
    let p = w.profile.as_ref().unwrap();
    assert_eq!(p.allowed.len(), n);
    assert_eq!(p.cutoff, q(w, &cutoff));
    assert!(p.upgrade_allowed);
}

// --- custom formats ---------------------------------------------------------

fn add(w: &mut World, name: &str, rules: Vec<FormatRule>, score: i32, mode: FormatMode) {
    let id = CustomFormatId::new();
    w.formats.push(CustomFormat {
        id,
        name: name.to_string(),
        rules,
    });
    w.scores.push(CustomFormatScore {
        format: id,
        score,
        mode,
    });
}

fn rule(kind: &str, value: &str) -> FormatRule {
    match kind {
        "title regex" => FormatRule::TitleRegex(value.to_string()),
        "resolution" => FormatRule::Resolution(value.to_string()),
        "source" => FormatRule::Source(value.to_string()),
        "codec" => FormatRule::Codec(value.to_string()),
        "edition" => FormatRule::Edition(value.to_string()),
        "language" => FormatRule::Language(value.to_string()),
        "indexer flag" => FormatRule::IndexerFlag(value.to_string()),
        "size at most GB" => FormatRule::SizeBetween {
            min: None,
            max: Some(value.parse::<u64>().unwrap() * 1024 * 1024 * 1024),
        },
        other => panic!("unknown rule kind {other}"),
    }
}

#[given(expr = "a custom format {string} with {word} {word} {string} scoring {int}")]
fn format_two_word(w: &mut World, name: String, k1: String, k2: String, value: String, score: i32) {
    add(
        w,
        &name,
        vec![rule(&format!("{k1} {k2}"), &value)],
        score,
        FormatMode::Preferred,
    );
}

#[given(expr = "a custom format {string} with {word} {string} scoring {int}")]
fn format_one_word(w: &mut World, name: String, kind: String, value: String, score: i32) {
    add(
        w,
        &name,
        vec![rule(&kind, &value)],
        score,
        FormatMode::Preferred,
    );
}

#[given(expr = "a custom format {string} with source {string} and codec {string} scoring {int}")]
fn format_and(w: &mut World, name: String, source: String, codec: String, score: i32) {
    add(
        w,
        &name,
        vec![FormatRule::Source(source), FormatRule::Codec(codec)],
        score,
        FormatMode::Preferred,
    );
}

#[given(expr = "a custom format {string} with a maximum size of {int} GB scoring {int}")]
fn format_size(w: &mut World, name: String, gb: u64, score: i32) {
    add(
        w,
        &name,
        vec![rule("size at most GB", &gb.to_string())],
        score,
        FormatMode::Preferred,
    );
}

#[given(expr = "a required custom format {string} with title regex {string}")]
fn format_required(w: &mut World, name: String, re: String) {
    add(
        w,
        &name,
        vec![FormatRule::TitleRegex(re)],
        0,
        FormatMode::Required,
    );
}

#[given(expr = "an ignored custom format {string} with title regex {string}")]
fn format_ignored(w: &mut World, name: String, re: String) {
    add(
        w,
        &name,
        vec![FormatRule::TitleRegex(re)],
        0,
        FormatMode::Ignored,
    );
}

#[given(expr = "a custom format {string} with no rules scoring {int}")]
fn format_empty(w: &mut World, name: String, score: i32) {
    add(w, &name, vec![], score, FormatMode::Preferred);
}

#[when(expr = "the release of {int} GB with flags {string} is scored")]
fn score_with(w: &mut World, gb: u64, flags: String) {
    let p = parse(&w.title);
    let flags: Vec<String> = flags
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let meta = ReleaseMeta {
        size_bytes: Some(gb * 1024 * 1024 * 1024),
        indexer_flags: &flags,
    };
    w.breakdown = Some(score_breakdown(&w.scores, &w.formats, &w.title, &p, &meta));
}

#[when("the release is scored")]
fn score(w: &mut World) {
    score_with(w, 4, String::new());
}

#[then(expr = "the aggregate format score is {int}")]
fn total(w: &mut World, n: i32) {
    let b = w.breakdown.as_ref().unwrap();
    assert_eq!(b.total, n, "{b:?}");
}

#[then(expr = "the matched formats are {string}")]
fn matched(w: &mut World, names: String) {
    let b = w.breakdown.as_ref().unwrap();
    let mut got: Vec<String> = b.matched.iter().map(|m| m.name.clone()).collect();
    got.sort();
    let mut want: Vec<String> = names
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    want.sort();
    assert_eq!(got, want, "{b:?}");
}

#[then(expr = "the required format {string} is reported missing")]
fn missing_required(w: &mut World, name: String) {
    let b = w.breakdown.as_ref().unwrap();
    assert!(b.missing_required.contains(&name), "{b:?}");
}

#[then(expr = "the ignored format {string} is reported present")]
fn present_ignored(w: &mut World, name: String) {
    let b = w.breakdown.as_ref().unwrap();
    assert!(b.present_ignored.contains(&name), "{b:?}");
}

#[then(expr = "the title regex {string} is rejected at validation")]
fn invalid_regex(_w: &mut World, re: String) {
    assert!(FormatRule::TitleRegex(re).validate().is_err());
}

#[then(expr = "the profile's score floor of {int} accepts {int} and rejects {int}")]
fn score_floor(w: &mut World, min: i32, ok: i32, bad: i32) {
    let mut p = w.profile.clone().unwrap();
    p.min_format_score = min;
    assert!(p.accepts_format_score(ok));
    assert!(!p.accepts_format_score(bad));
}

// --- relevance ------------------------------------------------------------

#[when(expr = "{string} is scored against the wanted title {string}")]
fn relevance(w: &mut World, release: String, wanted: String) {
    w.relevance = Some(title_relevance(&[wanted], &release));
}

#[when(expr = "{string} is scored against the wanted title {string} by {string}")]
fn relevance_author(w: &mut World, release: String, wanted: String, author: String) {
    w.relevance = Some(title_relevance(
        &[wanted.clone(), format!("{wanted} {author}")],
        &release,
    ));
}

#[then(expr = "the title coverage is {float}")]
fn coverage(w: &mut World, c: f32) {
    let r = w.relevance.as_ref().unwrap();
    assert!((r.coverage - c).abs() < 0.01, "{r:?}");
}

#[then(expr = "the precision is below {float}")]
fn precision_below(w: &mut World, p: f32) {
    let r = w.relevance.as_ref().unwrap();
    assert!(r.precision < p, "{r:?}");
}

#[then(expr = "the author is {word} in the release")]
fn author_hit(w: &mut World, word: String) {
    let r = w.relevance.as_ref().unwrap();
    match word.as_str() {
        "present" => assert!(r.author_tokens > 0 && r.author_hits > 0, "{r:?}"),
        "absent" => assert!(r.author_tokens > 0 && r.author_hits == 0, "{r:?}"),
        other => panic!("{other}"),
    }
}
