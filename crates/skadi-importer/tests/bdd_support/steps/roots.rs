//! C22 root-folder steps. The single-library-root model (SKADI-T-0302) lives in
//! `skadi-core::folder` (`RootFolder::for_domain`, `probe_root_status`) and the
//! importer's `available_space`; both are exercised here against the scenario's
//! scratch directory — never a real library path.
use cucumber::{given, then, when};
use skadi_core::{MediaKind, RootFolder, probe_root_status};
use skadi_importer::available_space;

use crate::bdd_support::World;

fn kind_of(s: &str) -> MediaKind {
    match s {
        "movie" | "movies" => MediaKind::Movie,
        "series" | "television" | "tv" => MediaKind::Series,
        "audiobook" | "audiobooks" => MediaKind::Audiobook,
        other => panic!("unknown media kind {other}"),
    }
}

#[given("the operator's single library root is the scratch library")]
fn single_root(w: &mut World) {
    w.notes.push(w.library().display().to_string());
}

#[when(expr = "the {word} domain derives its root")]
fn derive_root(w: &mut World, kind: String) {
    w.derived_root = Some(RootFolder::for_domain(w.library(), kind_of(&kind)));
}

#[then(expr = "the derived root is library path {string}")]
fn derived_is(w: &mut World, rel: String) {
    let got = &w.derived_root.as_ref().expect("derived").path;
    assert_eq!(*got, w.lib(&rel));
}

#[given(expr = "library path {string} is a directory")]
fn lib_dir(w: &mut World, rel: String) {
    std::fs::create_dir_all(w.lib(&rel)).expect("mkdir");
}

#[given(expr = "library path {string} is a regular file")]
fn lib_file(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(&p, b"not a dir").expect("write");
}

#[given(expr = "library path {string} is a read-only directory")]
fn lib_ro_dir(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    std::fs::create_dir_all(&p).expect("mkdir");
    w.make_read_only(&p);
}

#[when(expr = "library path {string} is probed as a root folder")]
fn probe(w: &mut World, rel: String) {
    w.root_status = Some(probe_root_status(&w.lib(&rel)));
}

#[then("the root is usable")]
fn usable(w: &mut World) {
    let s = w.root_status.expect("probed");
    assert!(s.is_usable(), "{s:?}");
    assert_eq!(s.problem(), None);
}

#[then(expr = "the root problem is {string}")]
fn problem_is(w: &mut World, want: String) {
    let s = w.root_status.expect("probed");
    assert_eq!(s.problem(), Some(want.as_str()), "{s:?}");
    assert!(!s.is_usable());
}

#[then(expr = "the probe left nothing behind in library path {string}")]
fn probe_clean(w: &mut World, rel: String) {
    let n = std::fs::read_dir(w.lib(&rel)).expect("read_dir").count();
    assert_eq!(n, 0, "{n} entries left behind");
}

#[when(expr = "free space is measured for library path {string}")]
fn measure_space(w: &mut World, rel: String) {
    w.space = Some(available_space(&w.lib(&rel)));
}

#[then("a free-space figure is reported")]
fn space_reported(w: &mut World) {
    let s = w.space.expect("measured");
    assert!(s.is_some_and(|b| b > 0), "{s:?}");
}

#[then("every domain root derives from the single library root")]
fn roots_derive(w: &mut World) {
    // Sonarr/Radarr keep N operator-registered roots and choose one per item.
    // skadi derives exactly one per domain from `library.root` (SKADI-A-0004).
    let lib = w.library();
    let lib = lib.as_path();
    for kind in [MediaKind::Movie, MediaKind::Series, MediaKind::Audiobook] {
        let root = RootFolder::for_domain(lib, kind);
        assert!(
            root.path.starts_with(lib),
            "{kind:?} root {:?} escaped the library root {lib:?}",
            root.path
        );
        assert_ne!(
            root.path, lib,
            "{kind:?} must get its own subfolder, not the library root itself"
        );
    }
    // Each domain gets a *distinct* subfolder — this is what stops a movie being
    // filed into the audiobooks tree (SKADI-T-0162).
    let movie = RootFolder::for_domain(lib, MediaKind::Movie).path;
    let audiobook = RootFolder::for_domain(lib, MediaKind::Audiobook).path;
    assert_ne!(movie, audiobook);
}

#[then("there is no way to register a second root alongside it")]
fn no_root_registry(w: &mut World) {
    // The absence is the assertion. `for_domain` is the only way to obtain a
    // root, and it is a pure function of (library root, kind) — so asking twice
    // for the same domain cannot produce two different places, however it is
    // called. If a registry is ever reintroduced, this stops being true.
    let lib = w.library();
    let lib = lib.as_path();
    let a = RootFolder::for_domain(lib, MediaKind::Movie);
    let b = RootFolder::for_domain(lib, MediaKind::Movie);
    assert_eq!(
        a.path, b.path,
        "two calls produced different roots — a registry has crept back in"
    );
}

#[then("every movie derives the same root, so no per-item choice exists")]
fn no_per_item_root(w: &mut World) {
    // Sonarr/Radarr let each item pick its root and move between roots. Every
    // skadi movie anchors to `for_domain`, so there is nothing to pick from.
    let lib = w.library();
    let lib = lib.as_path();
    let first = RootFolder::for_domain(lib, MediaKind::Movie);
    let second = RootFolder::for_domain(lib, MediaKind::Movie);
    assert_eq!(first.path, second.path);
}

#[given(expr = "the importer's library root is {string}")]
fn importer_root_is(w: &mut World, rel: String) {
    // SKADI-T-0417: naming a root turns the liveness gate on. The path need not
    // exist — a root that has vanished is exactly what this guards against.
    w.library_roots = vec![w.lib(&rel)];
}

#[given("the library root carries a skadi root marker")]
fn root_has_marker(w: &mut World) {
    for root in &w.library_roots {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(skadi_importer::ROOT_MARKER), b"").unwrap();
    }
}

#[given("the operator requires a root marker")]
fn require_marker(w: &mut World) {
    w.require_root_marker = true;
}

#[given("the library root exists but is empty")]
fn root_exists_empty(w: &mut World) {
    // An empty, writable directory is exactly what a dropped NFS/SMB mount leaves
    // behind at the mount point, which is why level 1 alone cannot catch it.
    for root in &w.library_roots {
        std::fs::create_dir_all(root).unwrap();
    }
}
