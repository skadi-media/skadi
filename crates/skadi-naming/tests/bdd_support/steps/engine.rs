//! C21 naming-engine steps: tokens, templates, tidy, sanitize, kebab and full
//! library-path composition. Pure — no filesystem.
use std::path::{Path, PathBuf};

use cucumber::{given, then, when};
use skadi_naming::{
    kebab, render, render_component, render_library_path, sanitize_component, tidy,
};

use crate::bdd_support::World;

#[given(expr = "the token {string} is {string}")]
fn token(w: &mut World, name: String, value: String) {
    w.tokens.push((name, value));
}

#[given(expr = "the token {string} is empty")]
fn token_empty(w: &mut World, name: String) {
    w.tokens.push((name, String::new()));
}

#[given("no tokens")]
fn no_tokens(w: &mut World) {
    w.tokens.clear();
}

#[given("the movie token set the domain supplies")]
fn movie_tokens(w: &mut World) {
    // The exact token names `skadi-movies::naming::movie_path_with_templates` builds
    // (crates/skadi-movies/src/naming.rs:156-186) — nothing release-derived.
    for (k, v) in [
        ("Title", "The Matrix"),
        ("TitleKebab", "the-matrix"),
        ("Year", "1999"),
        ("TmdbTag", "{tmdb-603}"),
        ("ImdbTag", "{imdb-tt0133093}"),
        ("EditionFolder", "Theatrical"),
        ("EditionKebab", "theatrical"),
        ("EditionSuffix", ""),
        // SKADI-T-0420: derived by the domain from the source release name.
        ("Quality Full", "Bluray-1080p"),
        ("Release Group", "GRP"),
    ] {
        w.tokens.push((k.to_string(), v.to_string()));
    }
}

#[given("the episode token set the domain supplies")]
fn episode_tokens(w: &mut World) {
    // The token names `skadi-tv::naming::SeriesNaming::path` builds
    // (crates/skadi-tv/src/naming.rs:116-152); numbering is pre-padded by the domain.
    for (k, v) in [
        ("SeriesTitle", "The Wire"),
        ("SeriesTitleKebab", "the-wire"),
        ("Year", "2002"),
        ("TmdbTag", "{tmdb-1438}"),
        ("SeasonFolder", "Season 03"),
        ("Episode", "S03E05"),
        ("Absolute", ""),
        ("AirDate", ""),
        ("EpisodeTitlePart", " - straight-and-true"),
        ("QualityPart", " [1080p]"),
    ] {
        w.tokens.push((k.to_string(), v.to_string()));
    }
}

#[given(expr = "the space replacement is {string}")]
fn space(w: &mut World, s: String) {
    w.space = Some(s.chars().next().expect("one char"));
}

#[when(expr = "the template {string} is rendered")]
fn render_template(w: &mut World, template: String) {
    w.rendered = Some(render(&template, &w.tokens()));
}

#[when(expr = "the component template {string} is rendered")]
fn render_comp(w: &mut World, template: String) {
    w.rendered = Some(render_component(&template, &w.tokens(), w.space()));
}

#[when(expr = "{string} is tidied")]
fn tidy_it(w: &mut World, s: String) {
    w.rendered = Some(tidy(&s));
}

#[when(expr = "{string} is sanitized")]
fn sanitize_it(w: &mut World, s: String) {
    w.rendered = Some(sanitize_component(&s, w.space()));
    w.subject = Some(s);
}

#[when(expr = "{string} is kebab-cased")]
fn kebab_it(w: &mut World, s: String) {
    w.rendered = Some(kebab(&s));
}

#[when(
    expr = "folder template {string} and file template {string} build a path under {string} with extension {string}"
)]
fn build_path(w: &mut World, folder: String, file: String, root: String, ext: String) {
    let root = PathBuf::from(root);
    w.path = Some(render_library_path(
        &root,
        &folder,
        &file,
        &w.tokens(),
        &ext,
        w.space(),
    ));
    w.root = Some(root);
}

#[then(expr = "the result is {string}")]
fn result_is(w: &mut World, want: String) {
    assert_eq!(w.rendered(), want);
}

#[then(expr = "the path is {string}")]
fn path_is(w: &mut World, want: String) {
    assert_eq!(w.path(), Path::new(&want));
}

#[then("the path stays under the root")]
fn path_under_root(w: &mut World) {
    // A title can't be allowed to walk out of the library root (NFR-NAMING.3).
    // `starts_with` is lexical, so also refuse `..` / `.` components outright.
    let root = w.root.as_deref().expect("root");
    let p = w.path();
    let escapes = !p.starts_with(root)
        || p.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        });
    assert!(!escapes, "{} escapes {}", p.display(), root.display());
}

#[then("the result has no trailing dot or space")]
fn no_trailing_dot(w: &mut World) {
    // Windows/SMB refuse names ending in `.` or space (REQ-NAMING.10).
    let r = w.rendered();
    assert!(
        !r.ends_with('.') && !r.ends_with(' '),
        "{r:?} ends with a dot/space"
    );
}

#[then("the result is not a Windows reserved device name")]
fn not_reserved(w: &mut World) {
    let stem = w
        .rendered()
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "LPT1", "LPT2", "LPT3",
    ];
    assert!(!reserved.contains(&stem.as_str()), "{stem} is reserved");
}

#[then(expr = "the result is at most {int} bytes")]
fn max_bytes(w: &mut World, n: usize) {
    let len = w.rendered().len();
    assert!(
        len <= n,
        "{len} bytes > {n} (NAME_MAX on ext4/APFS/NFS is 255)"
    );
}

#[then("the result is in Unicode NFC form")]
fn nfc(w: &mut World) {
    // "é" as NFD (e + U+0301) must normalise to the single NFC code point so the
    // same title never yields two different on-disk names (NFR-NAMING.8).
    let r = w.rendered();
    assert!(
        !r.contains('\u{0301}'),
        "{r:?} still carries a combining accent (not NFC-normalised)"
    );
}
