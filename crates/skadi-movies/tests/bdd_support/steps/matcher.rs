//! C25 `MovieMatcher` steps — pure, in-memory, no DB.
use std::path::{Path, PathBuf};

use cucumber::{given, then, when};

use skadi_core::{EditionKindId, FileRef};
use skadi_importer::{AcquirableMatcher, CollisionPolicy, CompletedDownload};
use skadi_movies::{EditionKind, Movie, MovieEdition, MovieMatcher};

use crate::bdd_support::{Matcher, World};

/// The six built-in kinds exactly as the migration seeds them.
pub fn builtin_kinds() -> Vec<EditionKind> {
    let k = |n: u128, name: &str, tag: &str, pats: &[&str]| EditionKind {
        id: EditionKindId::from(uuid::Uuid::from_u128(n)),
        name: name.into(),
        normalized_tag: tag.into(),
        match_patterns: pats.iter().map(|s| s.to_string()).collect(),
        builtin: true,
    };
    vec![
        k(1, "Theatrical", "Theatrical", &["theatrical"]),
        k(
            2,
            "Extended",
            "Extended",
            &["extended", "extended cut", "extended edition"],
        ),
        k(
            3,
            "Director's Cut",
            "Directors Cut",
            &["director's cut", "directors cut", "director.cut"],
        ),
        k(
            4,
            "Ultimate Cut",
            "Ultimate Cut",
            &["ultimate cut", "ultimate edition", "ultimate.cut"],
        ),
        k(5, "IMAX", "IMAX", &["imax"]),
        k(6, "Remastered", "Remastered", &["remastered", "remaster"]),
    ]
}

fn completed() -> CompletedDownload {
    CompletedDownload {
        handle: skadi_downloaders::DownloadHandle {
            native_id: "h".into(),
            category: "2000".into(),
        },
        files: vec![],
        category: "2000".into(),
    }
}

fn build(w: &mut World) {
    let movie = w.last_movie.clone().expect("matcher movie");
    let editions: Vec<MovieEdition> = w.editions.values().cloned().collect();
    let kinds = if w.kinds.is_empty() {
        builtin_kinds()
    } else {
        w.kinds.clone()
    };
    w.matcher = Some(Matcher(MovieMatcher::new(movie, editions, kinds)));
}

#[given(expr = "a matcher for {string} from {int} with editions {string}")]
async fn matcher_for(w: &mut World, title: String, year: u16, editions: String) {
    let mut movie = World::new_movie(&title, year, true);
    movie.external_ids.tmdb = Some(skadi_core::TmdbId(603));
    w.editions.clear();
    for name in editions.split(',').map(str::trim) {
        let kind = builtin_kinds()
            .into_iter()
            .find(|k| k.name == name)
            .unwrap_or_else(|| panic!("no builtin kind {name:?}"));
        w.editions
            .insert(name.to_string(), MovieEdition::missing(movie.id, kind.id));
    }
    w.last_movie = Some(movie);
    build(w);
}

#[given(
    expr = "a custom kind {string} tagged {string} matching pattern {string} with its own edition"
)]
async fn custom_kind(w: &mut World, name: String, tag: String, pattern: String) {
    let kind = EditionKind {
        id: EditionKindId::new(),
        name: name.clone(),
        normalized_tag: tag,
        match_patterns: vec![pattern],
        builtin: false,
    };
    let movie_id = w.last_movie.as_ref().map(|m: &Movie| m.id).expect("movie");
    w.editions
        .insert(name, MovieEdition::missing(movie_id, kind.id));
    let mut kinds = vec![kind];
    kinds.extend(builtin_kinds());
    w.kinds = kinds;
    build(w);
}

#[given(expr = "the Theatrical edition already holds {string}")]
async fn holds(w: &mut World, path: String) {
    w.editions.get_mut("Theatrical").expect("theatrical").file = Some(FileRef {
        path: PathBuf::from(path),
    });
    build(w);
}

#[when(expr = "the release {string} is matched from download path {string}")]
async fn match_release(w: &mut World, release: String, source: String) {
    let parsed = skadi_quality::parse(&release);
    let m = &w.matcher.as_ref().expect("matcher").0;
    w.matches = m.match_file(&parsed, Path::new(&source), &completed());
}

#[when(expr = "a release whose parsed edition is {string} is matched from download path {string}")]
async fn match_edition(w: &mut World, edition: String, source: String) {
    let parsed = skadi_quality::ParsedRelease {
        edition: Some(edition),
        ..Default::default()
    };
    let m = &w.matcher.as_ref().expect("matcher").0;
    w.matches = m.match_file(&parsed, Path::new(&source), &completed());
}

#[when(expr = "a {int}-byte file named {string} is matched")]
async fn match_small_file(w: &mut World, bytes: usize, name: String) {
    let dir = w.tmp();
    let src = dir.join(&name);
    std::fs::write(&src, vec![0u8; bytes]).expect("write");
    let parsed = skadi_quality::parse(&name);
    let m = &w.matcher.as_ref().expect("matcher").0;
    w.matches = m.match_file(&parsed, &src, &completed());
}

#[then("no match is emitted")]
async fn no_match(w: &mut World) {
    assert!(w.matches.is_empty(), "matches: {:?}", w.matches);
}

#[then(expr = "the match routes to the {string} edition")]
async fn routes(w: &mut World, name: String) {
    assert_eq!(w.matches.len(), 1, "matches: {:?}", w.matches);
    let e = w.editions.get(&name).expect("edition");
    assert_eq!(w.matches[0].acquirable, e.acquirable_ref());
}

#[then(expr = "the destination is {string}")]
async fn dest_is(w: &mut World, dest: String) {
    assert_eq!(w.matches[0].dest, PathBuf::from(dest));
}

#[then(expr = "the destination contains {string}")]
async fn dest_contains(w: &mut World, part: String) {
    let d = w.matches[0].dest.to_string_lossy();
    assert!(d.contains(&part), "dest {d}");
}

#[then("the destination has no filesystem-unsafe characters")]
async fn dest_safe(w: &mut World) {
    let d = w.matches[0].dest.to_string_lossy().to_string();
    let rel = d.trim_start_matches("/movies/");
    assert!(
        !rel.contains(['\\', ':', '*', '?', '"', '<', '>', '|']),
        "dest {d}"
    );
}

#[then(expr = "the match supersedes {string} and overwrites on collision")]
async fn supersedes(w: &mut World, old: String) {
    assert_eq!(w.matches[0].supersedes, vec![PathBuf::from(old)]);
    assert_eq!(w.matches[0].on_collision, CollisionPolicy::Overwrite);
}

#[then("the match carries an NFO sidecar")]
async fn nfo(w: &mut World) {
    assert!(
        w.matches[0]
            .sidecars
            .iter()
            .any(|(p, _)| p.extension().is_some_and(|e| e == "nfo")),
        "sidecars: {:?}",
        w.matches[0].sidecars
    );
}
