//! C20 low-level file-management steps: the placement primitives (`place_file`,
//! `place_with_collision`), library-delete pruning, and the manual-import scan.
use cucumber::{given, then, when};
use skadi_importer::{
    CollisionPolicy, Placement, SpaceVerdict, delete_files_and_prune, place_file,
    place_with_collision, scan_files, space_check,
};

use crate::bdd_support::World;

#[given(expr = "a source file {string} with contents {string}")]
fn source_with(w: &mut World, name: String, contents: String) {
    let p = w.src(&name);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(&p, contents.as_bytes()).expect("write");
    w.sources.push(p);
}

#[given(expr = "a partial file is left at {string} from an interrupted copy")]
fn stale_partial(w: &mut World, rel: String) {
    let p = w.lib(&rel).with_extension("skadi-partial");
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(&p, b"half a copy").expect("write partial");
}

#[when(expr = "{string} is placed at library path {string} without hardlinks")]
fn place_copy(w: &mut World, name: String, rel: String) {
    let r = place_file(&w.src(&name), &w.lib(&rel), false).map(|()| Placement::Placed);
    w.place_result = Some(r.map_err(|e| e.to_string()));
}

#[when(expr = "{string} is placed at library path {string} with hardlinks")]
fn place_link(w: &mut World, name: String, rel: String) {
    let r = place_file(&w.src(&name), &w.lib(&rel), true).map(|()| Placement::Placed);
    w.place_result = Some(r.map_err(|e| e.to_string()));
}

#[when(expr = "{string} is placed at library path {string} under the {word} policy")]
fn place_policy(w: &mut World, name: String, rel: String, policy: String) {
    let policy = match policy.as_str() {
        "skip" => CollisionPolicy::Skip,
        "overwrite" => CollisionPolicy::Overwrite,
        "error" => CollisionPolicy::Error,
        other => panic!("unknown policy {other}"),
    };
    let r = place_with_collision(&w.src(&name), &w.lib(&rel), policy, true);
    w.place_result = Some(r.map_err(|e| e.to_string()));
}

#[then("the placement succeeds")]
fn placement_ok(w: &mut World) {
    let r = w.place_result.as_ref().expect("placed");
    assert!(r.is_ok(), "placement failed: {r:?}");
}

#[then(expr = "the placement reports {word}")]
fn placement_reports(w: &mut World, what: String) {
    let want = match what.as_str() {
        "placed" => Placement::Placed,
        "replaced" => Placement::Replaced,
        "skipped" => Placement::Skipped,
        other => panic!("unknown placement {other}"),
    };
    let r = w.place_result.as_ref().expect("placed");
    assert_eq!(r.as_ref().ok(), Some(&want), "got {r:?}");
}

#[then(expr = "the placement fails with an error containing {string}")]
fn placement_fails(w: &mut World, needle: String) {
    let r = w.place_result.as_ref().expect("placed");
    let e = r
        .as_ref()
        .err()
        .unwrap_or_else(|| panic!("placement succeeded: {r:?}"));
    assert!(e.contains(&needle), "error {e:?} lacks {needle:?}");
}

// ───────────────────────── delete + prune ─────────────────────────

#[given(expr = "the library holds {string}")]
fn library_holds_file(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(&p, rel.as_bytes()).expect("write");
}

#[when(expr = "the library item's files {string} are deleted")]
fn delete_item(w: &mut World, rels: String) {
    let files: Vec<_> = rels.split(',').map(|r| w.lib(r.trim())).collect();
    w.removed = delete_files_and_prune(&files, &w.library());
}

#[then(expr = "{int} file(s) was/were removed")]
fn n_removed(w: &mut World, n: usize) {
    assert_eq!(w.removed.len(), n, "{:?}", w.removed);
}

#[then("the library root still exists")]
fn root_exists(w: &mut World) {
    assert!(w.library().is_dir());
}

// ───────────────────────── manual-import scan ─────────────────────────

#[given(expr = "a download folder containing {string}")]
fn folder_with(w: &mut World, rels: String) {
    for r in rels.split(',') {
        let p = w.src(r.trim());
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, r.as_bytes()).expect("write");
    }
}

#[given(expr = "the download subfolder {string} is unreadable")]
fn subfolder_unreadable(w: &mut World, rel: String) {
    let p = w.src(&rel);
    std::fs::create_dir_all(&p).expect("mkdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        w.restore_perms.push(p);
    }
}

#[when("the download folder is scanned for manual import")]
fn scan_folder(w: &mut World) {
    w.scanned = Some(scan_files(&w.downloads()).map_err(|e| e.to_string()));
}

#[when(expr = "the download file {string} is scanned for manual import")]
fn scan_file(w: &mut World, name: String) {
    w.scanned = Some(scan_files(&w.src(&name)).map_err(|e| e.to_string()));
}

#[when("a missing path is scanned for manual import")]
fn scan_missing(w: &mut World) {
    w.scanned = Some(scan_files(&w.tmp.join("nope")).map_err(|e| e.to_string()));
}

#[then(expr = "the scan yields {string} in that order")]
fn scan_yields(w: &mut World, rels: String) {
    let want: Vec<_> = rels.split(',').map(|r| w.src(r.trim())).collect();
    let got = w
        .scanned
        .as_ref()
        .expect("scanned")
        .as_ref()
        .expect("scan ok");
    assert_eq!(*got, want);
}

#[then("the scan fails")]
fn scan_fails(w: &mut World) {
    assert!(w.scanned.as_ref().expect("scanned").is_err());
}

// ───────────────────────── pure free-space decision ─────────────────────────

#[when(expr = "a {int}-byte placement is checked against {int} available with a {int} reserve")]
fn check_space(w: &mut World, need: u64, avail: u64, reserve: u64) {
    w.notes
        .push(format!("{:?}", space_check(need, Some(avail), reserve)));
}

#[when(expr = "a {int}-byte placement is checked when free space cannot be measured")]
fn check_space_unknown(w: &mut World, need: u64) {
    w.notes.push(format!("{:?}", space_check(need, None, 0)));
}

#[then("the space verdict is that it fits")]
fn space_fits(w: &mut World) {
    assert_eq!(
        w.notes.last().map(String::as_str),
        Some(&*format!("{:?}", SpaceVerdict::Fits))
    );
}

#[then(expr = "the space verdict is insufficient with {int} available")]
fn space_insufficient(w: &mut World, available: u64) {
    let got = w.notes.last().expect("verdict");
    assert!(
        got.starts_with("Insufficient") && got.contains(&format!("available: {available}")),
        "{got}"
    );
}

#[given("the operator configures a recycle bin")]
fn configure_recycle_bin(w: &mut World) {
    // Goes through the same `ImportGuards` path production uses (SKADI-T-0418),
    // so the step exercises the settings key rather than the builder directly.
    let bin = w.tmp.join("recycle");
    let view = skadi_config::ConfigView::from_pairs([(
        "import.recycle_bin".to_string(),
        bin.display().to_string(),
    )]);
    w.recycle_bin = skadi_config::import_guards(&view).recycle_bin;
}

#[given("the operator chooses move-on-import placement")]
fn configure_move_placement(w: &mut World) {
    // Through the same `ImportGuards` path production uses (SKADI-T-0138), so the
    // step exercises the settings key rather than the builder directly.
    let view = skadi_config::ConfigView::from_pairs([(
        "import.placement".to_string(),
        "move".to_string(),
    )]);
    w.move_on_import = skadi_config::import_guards(&view).move_on_import;
}

#[given(expr = "the operator sets import placement to {string}")]
fn configure_placement(w: &mut World, mode: String) {
    let view = skadi_config::ConfigView::from_pairs([("import.placement".to_string(), mode)]);
    w.move_on_import = skadi_config::import_guards(&view).move_on_import;
}

#[then(expr = "the source file {string} is gone")]
fn source_gone(w: &mut World, name: String) {
    let p = w.src(&name);
    assert!(
        !p.exists(),
        "expected the source at {p:?} to have been moved"
    );
}

#[then(expr = "the source file {string} is still there")]
fn source_kept(w: &mut World, name: String) {
    let p = w.src(&name);
    assert!(
        p.exists(),
        "expected the source at {p:?} to be kept for seeding"
    );
}
