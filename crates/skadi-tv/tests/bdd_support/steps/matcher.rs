//! C25 `EpisodeMatcher` steps — pure, in-memory.
use std::path::PathBuf;

use cucumber::{given, then, when};

use skadi_core::FileRef;
use skadi_importer::{AcquirableMatcher, CollisionPolicy, CompletedDownload};
use skadi_quality::ParsedRelease;
use skadi_tv::{Episode, EpisodeMatcher, SeriesType};

use crate::bdd_support::{Matcher, World, ep_key};

fn completed() -> CompletedDownload {
    CompletedDownload {
        handle: skadi_downloaders::DownloadHandle {
            native_id: "hash".into(),
            category: "tv".into(),
        },
        files: vec![],
        category: "tv".into(),
    }
}

fn build(w: &mut World) {
    let series = w.last_series.clone().expect("matcher series");
    let eps: Vec<Episode> = w.episodes.values().cloned().collect();
    w.matcher = Some(Matcher(EpisodeMatcher::new(series, eps)));
}

fn parse_sxxeyy(s: &str) -> (u16, u16) {
    let s = s.trim().trim_start_matches('S');
    let (a, b) = s.split_once('E').expect("SxxEyy");
    (a.parse().unwrap(), b.parse().unwrap())
}

#[given(expr = "a matcher for {string} from {int} as a/an {word} series with episodes {string}")]
async fn matcher_for(w: &mut World, title: String, year: u16, kind: String, eps: String) {
    let mut series = World::new_series(&title, year, true);
    series.series_type = SeriesType::from_str_lossy(&kind);
    w.episodes.clear();
    for tok in eps.split(',') {
        let (s, n) = parse_sxxeyy(tok);
        let mut e = Episode::missing(series.id, s, n);
        e.title = Some(format!("Episode {n}"));
        w.episodes.insert(ep_key(&title, s, n), e);
    }
    w.series.insert(title.clone(), series.clone());
    w.last_series = Some(series);
    build(w);
}

#[given(expr = "matcher episode S{int}E{int} of {string} has absolute number {int}")]
async fn set_abs(w: &mut World, s: u16, n: u16, title: String, abs: u32) {
    w.episodes
        .get_mut(&ep_key(&title, s, n))
        .expect("episode")
        .absolute_number = Some(abs);
    build(w);
}

#[given(expr = "matcher episode S{int}E{int} of {string} aired on {word}")]
async fn air(w: &mut World, s: u16, n: u16, title: String, d: String) {
    w.episodes
        .get_mut(&ep_key(&title, s, n))
        .expect("episode")
        .air_date = Some(super::common::date(&d));
    build(w);
}

#[given(expr = "matcher episode S{int}E{int} of {string} already holds {string}")]
async fn holds(w: &mut World, s: u16, n: u16, title: String, path: String) {
    w.episodes
        .get_mut(&ep_key(&title, s, n))
        .expect("episode")
        .file = Some(FileRef {
        path: PathBuf::from(path),
    });
    build(w);
}

#[when(expr = "the downloaded file {string} is matched")]
async fn match_file(w: &mut World, file: String) {
    let src = PathBuf::from(format!("/dl/{file}"));
    let m = &w.matcher.as_ref().expect("matcher").0;
    w.matches = m.match_file(&ParsedRelease::default(), &src, &completed());
}

#[then("no match is emitted")]
async fn no_match(w: &mut World) {
    assert!(w.matches.is_empty(), "{:?}", w.matches);
}

#[then(expr = "the match routes to S{int}E{int} of {string}")]
async fn routes(w: &mut World, s: u16, n: u16, title: String) {
    assert_eq!(w.matches.len(), 1, "{:?}", w.matches);
    assert_eq!(
        w.matches[0].acquirable,
        w.episode(&title, s, n).acquirable_ref()
    );
}

#[then(expr = "{int} matches are emitted covering S{int}E{int} and S{int}E{int} of {string}")]
async fn multi(w: &mut World, n: usize, s1: u16, e1: u16, s2: u16, e2: u16, title: String) {
    assert_eq!(w.matches.len(), n, "{:?}", w.matches);
    let refs: Vec<_> = w.matches.iter().map(|m| m.acquirable.clone()).collect();
    assert!(refs.contains(&w.episode(&title, s1, e1).acquirable_ref()));
    assert!(refs.contains(&w.episode(&title, s2, e2).acquirable_ref()));
}

#[then("all matches overwrite on collision and share one destination")]
async fn overwrite(w: &mut World) {
    assert!(
        w.matches
            .iter()
            .all(|m| m.on_collision == CollisionPolicy::Overwrite)
    );
    let first = &w.matches[0].dest;
    assert!(w.matches.iter().all(|m| &m.dest == first));
}

#[then(expr = "the destination contains {string}")]
async fn dest_contains(w: &mut World, part: String) {
    let d = w.matches[0].dest.to_string_lossy();
    assert!(d.contains(&part), "dest {d}");
}

#[then(expr = "the match supersedes {string}")]
async fn supersedes(w: &mut World, old: String) {
    assert_eq!(w.matches[0].supersedes, vec![PathBuf::from(old)]);
}
