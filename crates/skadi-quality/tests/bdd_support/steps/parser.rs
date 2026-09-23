//! C11: the clean-room release-title parsers (`parse`, `parse_tv`,
//! `parse_audiobook`), the taxonomy, and the embedded regression corpora.

use cucumber::{gherkin::Step, given, then, when};

use skadi_quality::{Resolution, Source, parse, parse_audiobook, parse_tv};

use crate::bdd_support::World;

#[given(expr = "the release title {string}")]
fn set_title(w: &mut World, t: String) {
    w.title = t;
}

#[when("it is parsed as a movie")]
fn parse_movie(w: &mut World) {
    w.parsed = Some(parse(&w.title));
}

#[when("it is parsed as a TV episode")]
fn parse_episode(w: &mut World) {
    w.parsed = Some(parse_tv(&w.title));
}

#[when("it is parsed as an audiobook")]
fn parse_book(w: &mut World) {
    w.parsed = Some(parse_audiobook(&w.title));
}

fn p(w: &World) -> &skadi_quality::ParsedRelease {
    w.parsed.as_ref().expect("parsed")
}

#[then(expr = "the title is {string}")]
fn title_is(w: &mut World, t: String) {
    assert_eq!(p(w).title.as_deref(), Some(t.as_str()), "{:?}", p(w));
}

#[then(expr = "the year is {int}")]
fn year_is(w: &mut World, y: u16) {
    assert_eq!(p(w).year, Some(y), "{:?}", p(w));
}

#[then("there is no year")]
fn no_year(w: &mut World) {
    assert_eq!(p(w).year, None, "{:?}", p(w));
}

#[then(expr = "the resolution is {string}")]
fn resolution_is(w: &mut World, r: String) {
    assert_eq!(p(w).resolution.as_deref(), Some(r.as_str()), "{:?}", p(w));
}

#[then("there is no resolution")]
fn no_resolution(w: &mut World) {
    assert_eq!(p(w).resolution, None, "{:?}", p(w));
}

#[then(expr = "the source is {string}")]
fn source_is(w: &mut World, s: String) {
    assert_eq!(p(w).source.as_deref(), Some(s.as_str()), "{:?}", p(w));
}

#[then("there is no source")]
fn no_source(w: &mut World) {
    assert_eq!(p(w).source, None, "{:?}", p(w));
}

#[then(expr = "the codec is {string}")]
fn codec_is(w: &mut World, c: String) {
    assert_eq!(p(w).codec.as_deref(), Some(c.as_str()), "{:?}", p(w));
}

#[then(expr = "the group is {string}")]
fn group_is(w: &mut World, g: String) {
    assert_eq!(p(w).group.as_deref(), Some(g.as_str()), "{:?}", p(w));
}

#[then(expr = "the edition is {string}")]
fn edition_is(w: &mut World, e: String) {
    assert_eq!(p(w).edition.as_deref(), Some(e.as_str()), "{:?}", p(w));
}

#[then(expr = "the modifiers are {string}")]
fn modifiers_are(w: &mut World, m: String) {
    let want: Vec<String> = m
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(p(w).modifiers, want, "{:?}", p(w));
}

#[then(expr = "the languages are {string}")]
fn languages_are(w: &mut World, l: String) {
    let want: Vec<String> = l
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(p(w).languages, want, "{:?}", p(w));
}

#[then(expr = "it is season {int} episode {int}")]
fn season_episode(w: &mut World, s: u16, e: u16) {
    assert_eq!(p(w).season, Some(s), "{:?}", p(w));
    assert_eq!(p(w).episodes, vec![e], "{:?}", p(w));
}

#[then(expr = "it is season {int} episodes {string}")]
fn season_episodes(w: &mut World, s: u16, eps: String) {
    let want: Vec<u16> = eps.split(',').map(|e| e.trim().parse().unwrap()).collect();
    assert_eq!(p(w).season, Some(s), "{:?}", p(w));
    assert_eq!(p(w).episodes, want, "{:?}", p(w));
}

#[then(expr = "it is a full season {int} pack")]
fn full_season(w: &mut World, s: u16) {
    assert_eq!(p(w).season, Some(s), "{:?}", p(w));
    assert!(p(w).full_season && p(w).episodes.is_empty(), "{:?}", p(w));
}

#[then(expr = "it covers seasons {int} to {int}")]
fn covers_seasons(w: &mut World, from: u16, to: u16) {
    // A multi-season pack (`S01-S04`) should expose every season it carries
    // (Sonarr parses `SeasonNumbers`). `ParsedRelease` has a single `season`.
    let json = serde_json::to_value(p(w)).unwrap();
    let seasons = json.get("seasons").and_then(|v| v.as_array()).cloned();
    assert!(
        seasons.is_some_and(|s| s.len() == usize::from(to - from + 1)),
        "no multi-season field; parse is season {:?} full_season {}",
        p(w).season,
        p(w).full_season
    );
}

#[then(expr = "it is absolute episode {int}")]
fn absolute(w: &mut World, n: u32) {
    assert_eq!(p(w).absolute, vec![n], "{:?}", p(w));
    assert_eq!(p(w).season, None);
}

#[then(expr = "it aired on {string}")]
fn aired(w: &mut World, d: String) {
    assert_eq!(p(w).air_date.as_deref(), Some(d.as_str()), "{:?}", p(w));
}

#[then("it carries no TV markers")]
fn no_tv(w: &mut World) {
    let x = p(w);
    assert!(
        x.season.is_none()
            && x.episodes.is_empty()
            && x.absolute.is_empty()
            && x.air_date.is_none()
            && !x.full_season,
        "{x:?}"
    );
}

#[then(expr = "the author is {string}")]
fn author(w: &mut World, a: String) {
    assert_eq!(p(w).author.as_deref(), Some(a.as_str()), "{:?}", p(w));
}

#[then(expr = "the audio format is {string} at {int} kbps")]
fn audio(w: &mut World, f: String, kbps: u32) {
    assert_eq!(p(w).audio_format.as_deref(), Some(f.as_str()), "{:?}", p(w));
    assert_eq!(p(w).bitrate_kbps, Some(kbps), "{:?}", p(w));
}

#[then(expr = "the series is {string} position {string}")]
fn series(w: &mut World, s: String, pos: String) {
    assert_eq!(p(w).series.as_deref(), Some(s.as_str()), "{:?}", p(w));
    assert_eq!(
        p(w).series_position.as_deref(),
        Some(pos.as_str()),
        "{:?}",
        p(w)
    );
}

#[then("it is a multi-book pack")]
fn book_pack(w: &mut World) {
    assert!(p(w).book_pack, "{:?}", p(w));
}

#[then(expr = "it is {word}")]
fn abridged(w: &mut World, word: String) {
    let want = match word.as_str() {
        "abridged" => Some(true),
        "unabridged" => Some(false),
        other => panic!("unknown abridgement {other}"),
    };
    assert_eq!(p(w).abridged, want, "{:?}", p(w));
}

// --- taxonomy ------------------------------------------------------------

#[then(expr = "the token {string} maps to resolution {string}")]
fn res_token(_w: &mut World, token: String, want: String) {
    let r = Resolution::from_token(&token).unwrap_or_else(|| panic!("{token} unmapped"));
    assert_eq!(format!("{r:?}"), want);
}

#[then(expr = "the token {string} maps to source {string}")]
fn src_token(_w: &mut World, token: String, want: String) {
    let s = Source::from_token(&token).unwrap_or_else(|| panic!("{token} unmapped"));
    assert_eq!(format!("{s:?}"), want);
}

#[then("resolutions order SD < 480p < 576p < 720p < 1080p < 2160p")]
fn res_order(_w: &mut World) {
    use Resolution::*;
    assert!(Sd < R480p && R480p < R576p && R576p < R720p && R720p < R1080p && R1080p < R2160p);
}

// --- corpus ---------------------------------------------------------------

#[when(expr = "the {word} corpus is replayed")]
fn replay_corpus(w: &mut World, which: String) {
    let report = match which.as_str() {
        "movie" => skadi_quality::corpus::run(&skadi_quality::corpus::seed(), parse),
        "audiobook" => {
            skadi_quality::corpus::run(&skadi_quality::corpus::audiobook_seed(), parse_audiobook)
        }
        other => panic!("unknown corpus {other}"),
    };
    w.corpus_report = Some(report);
}

#[then(expr = "every one of its at least {int} entries parses as expected")]
fn corpus_clean(w: &mut World, min: usize) {
    let r = w.corpus_report.as_ref().unwrap();
    assert!(r.total >= min, "corpus has only {} entries", r.total);
    assert!(r.is_clean(), "{}", r.summary());
}

#[then(expr = "the corpus holds at least {int} entries")]
fn corpus_size(w: &mut World, min: usize) {
    let r = w.corpus_report.as_ref().unwrap();
    assert!(
        r.total >= min,
        "REQ-DECIDE.13 / SKADI-T-0013 target a bulk corpus; the seed has {} entries",
        r.total
    );
}

#[then("a corpus of titles parses as expected:")]
fn corpus_table(_w: &mut World, step: &Step) {
    let table = step.table().expect("table");
    let header = &table.rows[0];
    let col = |n: &str| {
        header
            .iter()
            .position(|h| h == n)
            .unwrap_or_else(|| panic!("column {n}"))
    };
    let (ti, yi, ri, si) = (col("title"), col("year"), col("resolution"), col("source"));
    let mut bad = Vec::new();
    for row in &table.rows[1..] {
        let p = parse(&row[ti]);
        let year = if row[yi].is_empty() {
            None
        } else {
            row[yi].parse::<u16>().ok()
        };
        let res = (!row[ri].is_empty()).then(|| row[ri].clone());
        let src = (!row[si].is_empty()).then(|| row[si].clone());
        if p.year != year || p.resolution != res || p.source != src {
            bad.push(format!(
                "{}: got year {:?} res {:?} src {:?}",
                row[ti], p.year, p.resolution, p.source
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
