//! C11 (hunter side): the runner-free `pipeline::decide` / `explain` gates —
//! title relevance, TV episode identity, movie year, blocklist, seeders,
//! category/size, profile allowed/cutoff/upgrade axes, custom formats, tally.

use std::collections::HashSet;

use cucumber::{gherkin::Step, given, then, when};

use skadi_core::{CustomFormatId, ExternalIds, MediaKind, ProfileId};
use skadi_hunter::{AcquireState, AudiobookScoring, Scoring, SearchSpec, TvScope};
use skadi_importer::AcquirableRef;
use skadi_indexers::{Category, ReleaseFetch};
use skadi_quality::{CustomFormat, CustomFormatScore, FormatMode, FormatRule};

use crate::bdd_support::World;
use crate::bdd_support::fixtures::{magnet_for, profile_of, quality_named, release, standard};

fn spec(kind: MediaKind, titles: Vec<String>, year: Option<u16>) -> SearchSpec {
    SearchSpec {
        tags: None,
        trigger: Default::default(),
        kind,
        titles,
        year,
        external_ids: ExternalIds::default(),
        categories: vec![],
        tv: None,
        series: None,
    }
}

fn install(w: &mut World, kind: MediaKind, spec: SearchSpec) {
    let (profile, defs) = standard();
    w.kind = Some(kind);
    w.profile.get_or_insert(profile);
    if w.definitions.is_empty() {
        w.definitions = defs;
    }
    w.state = Some(AcquireState::new(
        AcquirableRef(format!("item-{}", spec.titles.join("-"))),
        spec,
        ProfileId::new(),
    ));
}

#[given(expr = "a wanted movie {string} \\({int}\\) with the standard profile")]
fn wanted_movie(w: &mut World, title: String, year: u16) {
    install(
        w,
        MediaKind::Movie,
        spec(MediaKind::Movie, vec![title], Some(year)),
    );
}

#[given(expr = "a wanted movie {string} \\({int}\\) allowing only {string}")]
fn wanted_movie_allowing(w: &mut World, title: String, year: u16, names: String) {
    let (_, defs) = standard();
    let names: Vec<&str> = names.split(',').map(str::trim).collect();
    w.profile = Some(profile_of(&defs, &names));
    w.definitions = defs;
    install(
        w,
        MediaKind::Movie,
        spec(MediaKind::Movie, vec![title], Some(year)),
    );
}

#[given(expr = "a wanted movie {string} \\({int}\\) also known as {string}")]
fn wanted_movie_alias(w: &mut World, title: String, year: u16, alias: String) {
    install(
        w,
        MediaKind::Movie,
        spec(MediaKind::Movie, vec![title, alias], Some(year)),
    );
}

#[given(expr = "a wanted movie {string} with no known year")]
fn wanted_movie_no_year(w: &mut World, title: String) {
    install(
        w,
        MediaKind::Movie,
        spec(MediaKind::Movie, vec![title], None),
    );
}

fn tv_spec(title: String, tv: TvScope) -> SearchSpec {
    let mut s = spec(MediaKind::Series, vec![title], None);
    s.tv = Some(tv);
    s
}

#[given(expr = "a wanted episode {string} S{int}E{int} with the standard profile")]
fn wanted_episode(w: &mut World, title: String, season: u16, episode: u16) {
    install(
        w,
        MediaKind::Series,
        tv_spec(
            title,
            TvScope {
                season,
                episode: Some(episode),
                absolute: None,
                air_date: None,
            },
        ),
    );
}

#[given(expr = "a wanted season pack {string} S{int} with the standard profile")]
fn wanted_season(w: &mut World, title: String, season: u16) {
    install(
        w,
        MediaKind::Series,
        tv_spec(
            title,
            TvScope {
                season,
                episode: None,
                absolute: None,
                air_date: None,
            },
        ),
    );
}

#[given(expr = "a wanted anime episode {string} S{int}E{int} with absolute number {int}")]
fn wanted_anime(w: &mut World, title: String, season: u16, episode: u16, abs: u32) {
    install(
        w,
        MediaKind::Series,
        tv_spec(
            title,
            TvScope {
                season,
                episode: Some(episode),
                absolute: Some(abs),
                air_date: None,
            },
        ),
    );
}

#[given(expr = "a wanted daily episode {string} S{int}E{int} aired {string}")]
fn wanted_daily(w: &mut World, title: String, season: u16, episode: u16, date: String) {
    let d = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").expect("YYYY-MM-DD");
    install(
        w,
        MediaKind::Series,
        tv_spec(
            title,
            TvScope {
                season,
                episode: Some(episode),
                absolute: None,
                air_date: Some(d),
            },
        ),
    );
}

#[given(expr = "a wanted audiobook {string} by {string}")]
fn wanted_audiobook(w: &mut World, title: String, author: String) {
    let s = spec(
        MediaKind::Audiobook,
        vec![title.clone(), format!("{title} {author}")],
        None,
    );
    let defs = skadi_quality::default_audiobook_definitions();
    let allowed: Vec<_> = defs.iter().map(|d| d.id).collect();
    w.profile = Some(skadi_quality::QualityProfile {
        id: ProfileId::new(),
        name: "audiobook".into(),
        cutoff: *allowed.last().unwrap(),
        allowed,
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    });
    w.kind = Some(MediaKind::Audiobook);
    w.state = Some(AcquireState::new(
        AcquirableRef(format!("book-{title}")),
        s,
        ProfileId::new(),
    ));
}

#[given(expr = "the audiobook is book {int} of the series {string}")]
fn audiobook_series(w: &mut World, _n: u16, series: String) {
    let st = w.state_mut();
    st.request.titles.push(series.clone());
    st.request.series = Some(series);
}

#[given(expr = "the item already holds {string}")]
fn holds_quality(w: &mut World, name: String) {
    let q = quality_named(&w.definitions, &name);
    w.current_quality = Some(q);
    w.state_mut().current_quality = Some(q);
}

#[given(expr = "the item already holds {string} with format score {int}")]
fn holds_quality_score(w: &mut World, name: String, score: i32) {
    holds_quality(w, name);
    w.current_format_score = Some(score);
    w.state_mut().current_format_score = Some(score);
}

#[given(expr = "the profile requires at least {int} seeders")]
fn min_seeders(w: &mut World, n: u32) {
    w.min_seeders = n;
}

#[given(expr = "reachability is preferred over quality from {int}p up")]
fn prefer_reachability(w: &mut World, height: u16) {
    w.reachability = Some((true, height));
}

#[given("quality is preferred over reachability")]
fn prefer_quality(w: &mut World) {
    w.reachability = Some((false, 720));
}

#[given(expr = "the profile's minimum format score is {int}")]
fn min_format_score(w: &mut World, n: i32) {
    w.profile.as_mut().expect("profile").min_format_score = n;
}

#[given(expr = "the profile does not allow upgrades")]
fn no_upgrades(w: &mut World) {
    w.profile.as_mut().expect("profile").upgrade_allowed = false;
}

fn add_format(w: &mut World, name: String, rule: FormatRule, score: i32, mode: FormatMode) {
    let id = CustomFormatId::new();
    w.formats.push(CustomFormat {
        id,
        name,
        rules: vec![rule],
    });
    w.profile
        .as_mut()
        .expect("profile")
        .formats
        .push(CustomFormatScore {
            format: id,
            score,
            mode,
        });
}

#[given(expr = "a custom format {string} matching title regex {string} scoring {int}")]
fn format_preferred(w: &mut World, name: String, re: String, score: i32) {
    add_format(
        w,
        name,
        FormatRule::TitleRegex(re),
        score,
        FormatMode::Preferred,
    );
}

#[given(expr = "a required custom format {string} matching title regex {string}")]
fn format_required(w: &mut World, name: String, re: String) {
    add_format(w, name, FormatRule::TitleRegex(re), 0, FormatMode::Required);
}

#[given(expr = "an ignored custom format {string} matching title regex {string}")]
fn format_ignored(w: &mut World, name: String, re: String) {
    add_format(w, name, FormatRule::TitleRegex(re), 0, FormatMode::Ignored);
}

#[given(expr = "a custom format {string} matching source {string} scoring {int}")]
fn format_source(w: &mut World, name: String, source: String, score: i32) {
    add_format(
        w,
        name,
        FormatRule::Source(source),
        score,
        FormatMode::Preferred,
    );
}

fn push_candidate(w: &mut World, title: &str, seeders: Option<u32>) -> usize {
    let kind = w.kind();
    let r = release(kind, title, seeders, magnet_for(title));
    let st = w.state_mut();
    st.candidates.push(r);
    st.candidates.len() - 1
}

#[given(expr = "the candidate releases:")]
fn candidates(w: &mut World, step: &Step) {
    let table = step.table().expect("a table of candidates");
    let header = &table.rows[0];
    let col = |name: &str| header.iter().position(|h| h == name);
    let (ti, si, zi, ci, pi) = (
        col("title").expect("title column"),
        col("seeders"),
        col("size"),
        col("category"),
        col("published_hours_ago"),
    );
    for row in &table.rows[1..] {
        let seeders = si.and_then(|i| row[i].parse::<u32>().ok());
        let idx = push_candidate(w, &row[ti], seeders.or(Some(10)));
        let c = &mut w.state_mut().candidates[idx];
        if let Some(size) = zi.and_then(|i| row[i].parse::<u64>().ok()) {
            c.size = size;
        }
        if let Some(cat) = ci.and_then(|i| row[i].parse::<u32>().ok()) {
            c.categories = vec![Category(cat)];
        }
        if let Some(h) = pi.and_then(|i| row[i].parse::<i64>().ok()) {
            c.published = chrono::Utc::now() - chrono::Duration::hours(h);
        }
    }
}

#[given(expr = "a candidate {string} with {int} seeders")]
fn candidate_seeders(w: &mut World, title: String, seeders: u32) {
    push_candidate(w, &title, Some(seeders));
}

#[given(expr = "a usenet candidate {string}")]
fn candidate_usenet(w: &mut World, title: String) {
    let kind = w.kind();
    let r = release(
        kind,
        &title,
        None,
        ReleaseFetch::NzbUrl(format!("https://nzb.example/{title}")),
    );
    w.state_mut().candidates.push(r);
}

#[given(expr = "a candidate {string} of {int} bytes")]
fn candidate_sized(w: &mut World, title: String, size: u64) {
    let idx = push_candidate(w, &title, Some(10));
    w.state_mut().candidates[idx].size = size;
}

#[given(expr = "a candidate {string} tagged category {int}")]
fn candidate_category(w: &mut World, title: String, cat: u32) {
    let idx = push_candidate(w, &title, Some(10));
    w.state_mut().candidates[idx].categories = vec![Category(cat)];
}

#[given(expr = "{string} is on the blocklist")]
fn blocklisted(w: &mut World, title: String) {
    let kind = w.kind();
    let r = release(kind, &title, Some(1), magnet_for(&title));
    w.blocklisted.insert(skadi_indexers::release_key(&r));
}

fn scoring<'a>(w: &'a World, ab: Option<&'a AudiobookScoring>) -> Scoring<'a> {
    Scoring {
        indexer_flags: &[],
        definitions: &w.definitions,
        profile: w.profile(),
        formats: &w.formats,
        min_seeders: w.min_seeders,
        blocklisted: &w.blocklisted,
        audiobook: ab,
        current_quality: w.current_quality,
        current_format_score: w.current_format_score,
        current_unplayable: false,
    }
}

fn audiobook_scoring(w: &World) -> Option<AudiobookScoring> {
    (w.kind() == MediaKind::Audiobook).then(|| AudiobookScoring {
        definitions: skadi_quality::default_audiobook_definitions(),
        allow_abridged: false,
    })
}

#[when("the hunter decides")]
fn decide(w: &mut World) {
    let ab = audiobook_scoring(w);
    let mut state = w.state.take().expect("wanted item");
    let result = {
        let sc = scoring(w, ab.as_ref());
        // Quality-first unless a scenario opted into reachability (SKADI-T-0598).
        let policy = match w.reachability {
            Some((prefer_seeders, height)) => skadi_hunter::ReachabilityPolicy {
                prefer_seeders,
                floor: skadi_hunter::floor_for_height(height),
            },
            None => skadi_hunter::ReachabilityPolicy {
                prefer_seeders: false,
                floor: skadi_quality::Resolution::R720p,
            },
        };
        skadi_hunter::decide_with(&mut state, &sc, policy).map_err(|e| e.to_string())
    };
    w.state = Some(state);
    w.outcome = Some(result);
}

#[then(expr = "it chooses {string}")]
fn chooses(w: &mut World, title: String) {
    let out = w.outcome.clone().expect("decide ran");
    assert!(out.is_ok(), "decide failed: {out:?}");
    let chosen = w
        .state
        .as_ref()
        .unwrap()
        .chosen
        .as_ref()
        .map(|r| r.title.clone());
    assert_eq!(chosen.as_deref(), Some(title.as_str()));
}

#[then("no release is chosen")]
fn nothing_chosen(w: &mut World) {
    let out = w.outcome.clone().expect("decide ran");
    let chosen = w
        .state
        .as_ref()
        .unwrap()
        .chosen
        .as_ref()
        .map(|r| r.title.clone());
    assert!(
        out.is_err() && chosen.is_none(),
        "expected no suitable release, got {out:?} / chosen {chosen:?}"
    );
}

#[then(expr = "the tally rejected {int} as {string}")]
fn tally_rejected(w: &mut World, n: usize, gate: String) {
    let tally = w.state.as_ref().unwrap().tally.clone().expect("tally");
    let got = tally.rejected.get(&gate).copied().unwrap_or(0);
    assert_eq!(got, n, "tally {tally:?}");
}

#[then(expr = "the tally considered {int} candidates")]
fn tally_considered(w: &mut World, n: usize) {
    let tally = w.state.as_ref().unwrap().tally.clone().expect("tally");
    assert_eq!(tally.considered, n);
}

#[then(expr = "the tally summary is {string}")]
fn tally_summary(w: &mut World, s: String) {
    let tally = w.state.as_ref().unwrap().tally.clone().expect("tally");
    assert_eq!(tally.summary(), s);
}

fn explain_for(w: &World, title: &str) -> skadi_hunter::ReleaseExplanation {
    let ab = audiobook_scoring(w);
    let sc = scoring(w, ab.as_ref());
    let r = w
        .state
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .find(|r| r.title == title)
        .unwrap_or_else(|| panic!("no candidate titled {title:?}"));
    skadi_hunter::explain(r, &sc)
}

#[then(expr = "{string} is explained as accepted at {string}")]
fn explained_accepted(w: &mut World, title: String, quality: String) {
    let e = explain_for(w, &title);
    assert!(e.accepted, "{e:?}");
    assert_eq!(e.quality.as_deref(), Some(quality.as_str()), "{e:?}");
}

#[then(expr = "{string} is explained as rejected because {string}")]
fn explained_rejected(w: &mut World, title: String, why: String) {
    let e = explain_for(w, &title);
    assert!(!e.accepted, "{e:?}");
    assert!(
        e.reason.contains(&why),
        "reason {:?} lacks {why:?}",
        e.reason
    );
}

#[then(expr = "the decision for {string} is {string}")]
fn decision_label(w: &mut World, title: String, label: String) {
    let e = explain_for(w, &title);
    assert_eq!(e.decision.as_deref(), Some(label.as_str()), "{e:?}");
}

#[then(expr = "{string} has format score {int}")]
fn format_score(w: &mut World, title: String, score: i32) {
    let e = explain_for(w, &title);
    assert_eq!(e.format_score, score, "{e:?}");
}

#[then(expr = "the chosen release's title relevance is at least {float}")]
fn chosen_relevance(w: &mut World, min: f32) {
    let tally = w.state.as_ref().unwrap().tally.clone().expect("tally");
    let r = tally.chosen_relevance.expect("chosen relevance recorded");
    assert!(r >= min, "relevance {r}");
}

/// Keep the unused-import lint honest when a scenario never touches sets.
#[allow(dead_code)]
fn _touch(_: &HashSet<String>) {}

#[given(expr = "the show premiered in {int}")]
fn show_year(w: &mut World, year: u16) {
    w.state_mut().request.year = Some(year);
}
