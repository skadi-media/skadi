//! C20 importer steps: completed downloads, scripted domain decisions, import /
//! preview, and outcome + filesystem assertions. Everything happens inside the
//! scenario's scratch directory.
use std::path::PathBuf;

use cucumber::{given, then, when};
use skadi_importer::{CollisionPolicy, Importer, PlannedAction};

use crate::bdd_support::{Rule, World};

#[cfg(unix)]
fn inode(p: &std::path::Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap_or_else(|e| panic!("stat {}: {e}", p.display()));
    (m.dev(), m.ino())
}

fn policy_of(s: &str) -> CollisionPolicy {
    match s {
        "skip" => CollisionPolicy::Skip,
        "overwrite" => CollisionPolicy::Overwrite,
        "error" => CollisionPolicy::Error,
        other => panic!("unknown collision policy {other:?}"),
    }
}

fn write_source(w: &mut World, name: &str, bytes: &[u8]) {
    let p = w.src(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("source parent");
    }
    std::fs::write(&p, bytes).expect("write source");
    w.sources.push(p);
}

// ───────────────────────── Given: the completed download ─────────────────────────

#[given(expr = "a completed download containing {string}")]
fn completed_download_one(w: &mut World, name: String) {
    write_source(w, &name, format!("payload of {name}").as_bytes());
}

#[given(expr = "the completed download also contains {string}")]
fn completed_download_more(w: &mut World, name: String) {
    write_source(w, &name, format!("payload of {name}").as_bytes());
}

#[given(expr = "the completed download also contains {string} of {int} bytes")]
fn completed_download_sized(w: &mut World, name: String, bytes: usize) {
    write_source(w, &name, &vec![b'x'; bytes]);
}

#[given(expr = "the completed download lists {string} which is missing on disk")]
fn completed_download_missing(w: &mut World, name: String) {
    let p = w.src(&name);
    w.sources.push(p);
}

// ───────────────────────── Given: the domain's decisions ─────────────────────────

#[given(expr = "the domain maps {string} to library path {string}")]
fn map_default(w: &mut World, file: String, rel: String) {
    let dest = w.lib(&rel);
    w.rules.push(Rule::Place {
        acquirable: format!("acq:{file}"),
        file,
        dest,
        policy: None,
        supersedes: Vec::new(),
        sidecars: Vec::new(),
    });
}

#[given(expr = "the domain maps {string} to library path {string} as acquirable {string}")]
fn map_named(w: &mut World, file: String, rel: String, acquirable: String) {
    let dest = w.lib(&rel);
    w.rules.push(Rule::Place {
        file,
        acquirable,
        dest,
        policy: None,
        supersedes: Vec::new(),
        sidecars: Vec::new(),
    });
}

#[given(expr = "the domain maps {string} to library path {string} with collision policy {word}")]
fn map_policy(w: &mut World, file: String, rel: String, policy: String) {
    let dest = w.lib(&rel);
    w.rules.push(Rule::Place {
        acquirable: format!("acq:{file}"),
        file,
        dest,
        policy: Some(policy_of(&policy)),
        supersedes: Vec::new(),
        sidecars: Vec::new(),
    });
}

#[given(expr = "the domain maps {string} to library path {string} superseding {string}")]
fn map_superseding(w: &mut World, file: String, rel: String, old_rel: String) {
    let dest = w.lib(&rel);
    let old = w.lib(&old_rel);
    w.rules.push(Rule::Place {
        acquirable: format!("acq:{file}"),
        file,
        dest,
        policy: None,
        supersedes: vec![old],
        sidecars: Vec::new(),
    });
}

#[given(expr = "the domain attaches sidecar {string} with contents {string} to {string}")]
fn attach_sidecar(w: &mut World, sidecar_rel: String, contents: String, file: String) {
    let sidecar = w.lib(&sidecar_rel);
    let rule = w
        .rules
        .iter_mut()
        .find(|r| matches!(r, Rule::Place { file: f, .. } if *f == file))
        .expect("a mapping for the file exists");
    if let Rule::Place { sidecars, .. } = rule {
        sidecars.push((sidecar, contents));
    }
}

#[given(expr = "the domain quarantines {string} into review path {string}")]
fn quarantine(w: &mut World, file: String, rel: String) {
    let dest = w.lib(&rel);
    w.rules.push(Rule::Quarantine { file, dest });
}

#[given(expr = "the domain would place {string} at library path {string} if asked")]
fn map_even_sample(w: &mut World, file: String, rel: String) {
    map_default(w, file, rel);
}

// ───────────────────────── Given: library state + knobs ─────────────────────────

#[given(expr = "the library already holds {string} with contents {string}")]
fn library_holds(w: &mut World, rel: String, contents: String) {
    let p = w.lib(&rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(&p, contents.as_bytes()).expect("write library file");
}

#[given(expr = "the library directory {string} is read-only")]
fn library_dir_read_only(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    std::fs::create_dir_all(&p).expect("mkdir");
    w.make_read_only(&p);
}

#[given(expr = "a stale staging file is left beside {string} from an interrupted replace")]
fn stale_staging(w: &mut World, rel: String) {
    let p = w.lib(&rel).with_extension("skadi-incoming");
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(&p, b"half-written junk").expect("write staging");
}

#[given(expr = "the importer keeps a free-space reserve of {int} bytes")]
fn reserve(w: &mut World, bytes: u64) {
    w.min_free = bytes;
}

#[given("the importer keeps an unmeetable free-space reserve")]
fn reserve_unmeetable(w: &mut World) {
    w.min_free = u64::MAX;
}

#[given(expr = "the importer rejects files smaller than {int} bytes")]
fn size_floor(w: &mut World, bytes: u64) {
    w.min_file = bytes;
}

// ───────────────────────── When ─────────────────────────

#[when("the download is imported")]
async fn import(w: &mut World) {
    let importer = w.importer();
    let outcome = importer
        .import(w.completed())
        .await
        .expect("import returns an outcome");
    w.outcome = Some(outcome);
}

#[when("the download is imported again")]
async fn import_again(w: &mut World) {
    import(w).await;
}

#[when("the import is previewed")]
async fn preview(w: &mut World) {
    let importer = w.importer();
    let plan = importer
        .preview(w.completed())
        .await
        .expect("preview returns a plan");
    w.plan = Some(plan);
}

// ───────────────────────── Then: outcome buckets ─────────────────────────

#[then(expr = "{int} file(s) is/are imported")]
fn n_imported(w: &mut World, n: usize) {
    assert_eq!(
        w.outcome().imported.len(),
        n,
        "imported: {:?}",
        w.outcome().imported
    );
}

#[then(expr = "{int} file(s) is/are reported already present")]
fn n_present(w: &mut World, n: usize) {
    assert_eq!(
        w.outcome().already_present.len(),
        n,
        "already_present: {:?}",
        w.outcome().already_present
    );
}

#[then(expr = "{int} file(s) is/are rejected")]
fn n_rejected(w: &mut World, n: usize) {
    assert_eq!(
        w.outcome().rejected.len(),
        n,
        "rejected: {:?}",
        w.outcome().rejected
    );
}

#[then(expr = "{int} file(s) is/are failed")]
fn n_failed(w: &mut World, n: usize) {
    assert_eq!(
        w.outcome().failed.len(),
        n,
        "failed: {:?}",
        w.outcome().failed
    );
}

#[then(expr = "{int} file(s) is/are quarantined")]
fn n_quarantined(w: &mut World, n: usize) {
    assert_eq!(
        w.outcome().quarantined.len(),
        n,
        "quarantined: {:?}",
        w.outcome().quarantined
    );
}

#[then(expr = "{int} file(s) is/are reported replaced")]
fn n_replaced(w: &mut World, n: usize) {
    assert_eq!(
        w.outcome().replaced.len(),
        n,
        "replaced: {:?}",
        w.outcome().replaced
    );
}

#[then("nothing is imported")]
fn nothing_imported(w: &mut World) {
    assert!(
        w.outcome().imported.is_empty(),
        "{:?}",
        w.outcome().imported
    );
}

#[then("nothing is rejected")]
fn nothing_rejected(w: &mut World) {
    assert!(
        w.outcome().rejected.is_empty(),
        "{:?}",
        w.outcome().rejected
    );
}

#[then("nothing failed")]
fn nothing_failed(w: &mut World) {
    assert!(w.outcome().failed.is_empty(), "{:?}", w.outcome().failed);
}

#[then(expr = "the import for acquirable {string} landed at library path {string}")]
fn imported_at(w: &mut World, acquirable: String, rel: String) {
    let dest = w.lib(&rel);
    assert!(
        w.outcome()
            .imported
            .iter()
            .any(|f| f.acquirable.0 == acquirable && f.file.path == dest),
        "imported: {:?}",
        w.outcome().imported
    );
}

#[then(expr = "library path {string} is reported already present for acquirable {string}")]
fn present_for(w: &mut World, rel: String, acquirable: String) {
    let dest = w.lib(&rel);
    assert!(
        w.outcome()
            .already_present
            .iter()
            .any(|f| f.acquirable.0 == acquirable && f.file.path == dest),
        "already_present: {:?}",
        w.outcome().already_present
    );
}

#[then(expr = "source {string} is rejected with reason containing {string}")]
fn rejected_with(w: &mut World, name: String, needle: String) {
    let src = w.src(&name);
    let hit = w
        .outcome()
        .rejected
        .iter()
        .find(|(p, _)| *p == src)
        .unwrap_or_else(|| panic!("{name} not in rejected: {:?}", w.outcome().rejected));
    assert!(
        hit.1.contains(&needle),
        "reason {:?} lacks {needle:?}",
        hit.1
    );
}

#[then(expr = "a rejection names the source file {string}")]
fn rejection_keyed_by_source(w: &mut World, name: String) {
    let src = w.src(&name);
    assert!(
        w.outcome().rejected.iter().any(|(p, _)| *p == src),
        "rejections are keyed by {:?}, not by the source {}",
        w.outcome()
            .rejected
            .iter()
            .map(|(p, _)| p)
            .collect::<Vec<_>>(),
        src.display()
    );
}

#[then(expr = "a rejection reason contains {string}")]
fn some_rejection_reason(w: &mut World, needle: String) {
    assert!(
        w.outcome()
            .rejected
            .iter()
            .any(|(_, r)| r.contains(&needle)),
        "no rejection reason contains {needle:?}: {:?}",
        w.outcome().rejected
    );
}

#[then(expr = "library path {string} is reported failed with reason containing {string}")]
fn failed_with(w: &mut World, rel: String, needle: String) {
    let dest = w.lib(&rel);
    let hit = w
        .outcome()
        .failed
        .iter()
        .find(|(p, _)| *p == dest)
        .unwrap_or_else(|| panic!("{rel} not in failed: {:?}", w.outcome().failed));
    assert!(
        hit.1.contains(&needle),
        "reason {:?} lacks {needle:?}",
        hit.1
    );
}

#[then(expr = "source {string} is reported failed with reason containing {string}")]
fn failed_source_with(w: &mut World, name: String, needle: String) {
    let src = w.src(&name);
    let hit = w
        .outcome()
        .failed
        .iter()
        .find(|(p, _)| *p == src)
        .unwrap_or_else(|| panic!("{name} not in failed: {:?}", w.outcome().failed));
    assert!(
        hit.1.contains(&needle),
        "reason {:?} lacks {needle:?}",
        hit.1
    );
}

#[then(expr = "library path {string} is listed as replaced")]
fn listed_replaced(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    assert!(
        w.outcome().replaced.contains(&p),
        "replaced: {:?}",
        w.outcome().replaced
    );
}

#[then(expr = "library path {string} is not listed as replaced")]
fn not_listed_replaced(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    assert!(
        !w.outcome().replaced.contains(&p),
        "replaced: {:?}",
        w.outcome().replaced
    );
}

#[then(expr = "library path {string} is listed as quarantined")]
fn listed_quarantined(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    assert!(
        w.outcome().quarantined.contains(&p),
        "quarantined: {:?}",
        w.outcome().quarantined
    );
}

#[then("every source file appears in exactly one outcome bucket")]
fn accounted(w: &mut World) {
    let o = w.outcome();
    for src in &w.sources {
        let name = src.file_name().expect("name");
        let by_dest = |p: &PathBuf| {
            w.rules.iter().any(|r| match r {
                Rule::Place { file, dest, .. } => dest == p && file.as_str() == name,
                Rule::Quarantine { file, dest } => dest == p && file.as_str() == name,
            })
        };
        let hits = o.imported.iter().filter(|f| by_dest(&f.file.path)).count()
            + o.already_present
                .iter()
                .filter(|f| by_dest(&f.file.path))
                .count()
            + o.rejected
                .iter()
                .filter(|(p, _)| p == src || by_dest(p))
                .count()
            + o.failed
                .iter()
                .filter(|(p, _)| p == src || by_dest(p))
                .count()
            + o.quarantined.iter().filter(|p| by_dest(p)).count();
        assert_eq!(hits, 1, "{} accounted {hits} times in {o:?}", src.display());
    }
}

// ───────────────────────── Then: the filesystem ─────────────────────────

#[then(expr = "library path {string} exists")]
fn lib_exists(w: &mut World, rel: String) {
    assert!(w.lib(&rel).exists(), "{rel} missing");
}

#[then(expr = "library path {string} does not exist")]
fn lib_missing(w: &mut World, rel: String) {
    assert!(!w.lib(&rel).exists(), "{rel} exists");
}

#[then(expr = "library path {string} has contents {string}")]
fn lib_contents(w: &mut World, rel: String, contents: String) {
    let got = std::fs::read(w.lib(&rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    assert_eq!(String::from_utf8_lossy(&got), contents);
}

#[then(expr = "library path {string} has the same bytes as source {string}")]
fn lib_same_bytes(w: &mut World, rel: String, name: String) {
    let got = std::fs::read(w.lib(&rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let want = std::fs::read(w.src(&name)).expect("read source");
    assert_eq!(got, want);
}

#[cfg(unix)]
#[then(expr = "library path {string} is a hardlink of source {string}")]
fn lib_is_hardlink(w: &mut World, rel: String, name: String) {
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        inode(&w.lib(&rel)),
        inode(&w.src(&name)),
        "different inodes"
    );
    let links = std::fs::metadata(w.src(&name)).expect("stat").nlink();
    assert!(links >= 2, "expected ≥2 links, got {links}");
}

#[cfg(unix)]
/// SKADI-T-0419: Sonarr's "Use Hardlinks instead of Copy".
#[given("the importer is configured to copy instead of hardlink")]
fn configure_copy(w: &mut World) {
    w.use_hardlinks = false;
}

/// SKADI-T-0419: Sonarr's "Set Permissions".
#[given(expr = "the importer is configured to set file mode {word}")]
fn configure_mode(w: &mut World, mode: String) {
    w.file_mode = u32::from_str_radix(mode.trim_start_matches("0o"), 8).ok();
}

#[then(expr = "library path {string} is not a hardlink of source {string}")]
fn lib_not_hardlink(w: &mut World, rel: String, name: String) {
    assert_ne!(inode(&w.lib(&rel)), inode(&w.src(&name)), "same inode");
}

#[then(expr = "source {string} still exists")]
fn src_exists(w: &mut World, name: String) {
    assert!(w.src(&name).exists(), "source {name} was removed");
}

#[then(expr = "no staging or partial files are left beside {string}")]
fn no_leftovers(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    let dir = p.parent().expect("parent");
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".skadi-incoming") || n.ends_with(".skadi-partial"))
                .collect()
        })
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

#[then(expr = "the library directory {string} is empty")]
fn lib_dir_empty(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    let n = std::fs::read_dir(&p).map(|rd| rd.count()).unwrap_or(0);
    assert_eq!(n, 0, "{rel} has {n} entries");
}

#[then(expr = "a recycle bin holds the replaced file {string}")]
fn recycle_bin_holds(w: &mut World, rel: String) {
    // Sonarr/Radarr "Recycling Bin" (SKADI-T-0418): a superseded file is moved
    // there instead of unlinked. Assert the three things that make it a recovery
    // mechanism rather than a differently-named delete — it is in the configured
    // bin, its bytes survived the move, and it is gone from the library.
    let bin = w.recycle_bin.clone().expect("a recycle bin was configured");
    let name = std::path::Path::new(&rel)
        .file_name()
        .expect("the replaced file has a name");
    let at = bin.join(name);
    assert!(at.exists(), "expected the replaced file at {at:?}");
    assert_eq!(
        std::fs::read_to_string(&at).unwrap(),
        "old",
        "the recycled file must keep its contents"
    );
    assert!(
        !w.lib(&rel).exists(),
        "the superseded file should no longer be in the library"
    );
}

fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
    }
    out
}

#[cfg(unix)]
#[then(expr = "library path {string} has the operator's configured file mode")]
fn lib_has_configured_mode(w: &mut World, rel: String) {
    // SKADI-T-0419 (Sonarr/Radarr "Set Permissions"): the mode the scenario
    // configured is applied after placement.
    use std::os::unix::fs::PermissionsExt;
    let configured = w
        .file_mode
        .expect("the scenario must configure a file mode");
    let mode = std::fs::metadata(w.lib(&rel))
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, configured, "placed file has mode {mode:o}");
}

// ───────────────────────── Then: the preview plan ─────────────────────────

#[then(expr = "the plan would place {string}")]
fn plan_place(w: &mut World, rel: String) {
    let dest = w.lib(&rel);
    assert!(
        w.plan()
            .would_import
            .iter()
            .any(|p| p.dest == dest && p.action == PlannedAction::Place),
        "would_import: {:?}",
        w.plan().would_import
    );
}

#[then(expr = "the plan would replace {string}")]
fn plan_replace(w: &mut World, rel: String) {
    let dest = w.lib(&rel);
    assert!(
        w.plan()
            .would_import
            .iter()
            .any(|p| p.dest == dest && p.action == PlannedAction::Replace),
        "would_import: {:?}",
        w.plan().would_import
    );
    assert!(
        w.plan().would_replace.contains(&dest),
        "would_replace: {:?}",
        w.plan().would_replace
    );
}

#[then(expr = "the plan would remove {string}")]
fn plan_remove(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    assert!(
        w.plan().would_replace.contains(&p),
        "would_replace: {:?}",
        w.plan().would_replace
    );
}

#[then(expr = "the plan would reject source {string} with reason containing {string}")]
fn plan_reject_source(w: &mut World, name: String, needle: String) {
    let src = w.src(&name);
    let hit = w
        .plan()
        .would_reject
        .iter()
        .find(|(p, _)| *p == src)
        .unwrap_or_else(|| panic!("{name} not in would_reject: {:?}", w.plan().would_reject));
    assert!(
        hit.1.contains(&needle),
        "reason {:?} lacks {needle:?}",
        hit.1
    );
}

#[then(expr = "the plan would reject library path {string} with reason containing {string}")]
fn plan_reject_dest(w: &mut World, rel: String, needle: String) {
    let dest = w.lib(&rel);
    let hit = w
        .plan()
        .would_reject
        .iter()
        .find(|(p, _)| *p == dest)
        .unwrap_or_else(|| panic!("{rel} not in would_reject: {:?}", w.plan().would_reject));
    assert!(
        hit.1.contains(&needle),
        "reason {:?} lacks {needle:?}",
        hit.1
    );
}

/// SKADI-T-0428: an occupied Skip destination is reported as already-present by
/// the preview, matching what `import` reports, rather than as a rejection.
#[then(expr = "the plan reports library path {string} as already present")]
fn plan_already_present(w: &mut World, rel: String) {
    let dest = w.lib(&rel);
    assert!(
        w.plan().already_present.contains(&dest),
        "{rel} not in already_present: {:?}",
        w.plan().already_present
    );
    assert!(
        !w.plan().would_reject.iter().any(|(p, _)| *p == dest),
        "{rel} should not also be a rejection"
    );
}

#[then(expr = "the plan would quarantine {string}")]
fn plan_quarantine(w: &mut World, rel: String) {
    let p = w.lib(&rel);
    assert!(
        w.plan().would_quarantine.contains(&p),
        "would_quarantine: {:?}",
        w.plan().would_quarantine
    );
}

#[then("the plan imports nothing")]
fn plan_nothing(w: &mut World) {
    assert!(
        w.plan().would_import.is_empty(),
        "{:?}",
        w.plan().would_import
    );
}

#[then("the library root contains no files")]
fn library_untouched(w: &mut World) {
    let files = walk(&w.library());
    assert!(files.is_empty(), "library has files: {files:?}");
}

#[then(expr = "a failure reason contains {string}")]
fn some_failure_reason(w: &mut World, needle: String) {
    assert!(
        w.outcome().failed.iter().any(|(_, r)| r.contains(&needle)),
        "no failure reason contains {needle:?}: {:?}",
        w.outcome().failed
    );
}

#[given(expr = "the operator configures a minimum free space of {int} bytes in settings")]
fn operator_min_free_space(w: &mut World, bytes: u64) {
    // Sonarr/Radarr "Minimum Free Space" (Media Management), now carried by the
    // `import.min_free_mb` config key and applied by every domain importer
    // (SKADI-T-0416). The step reads bytes; the settings key is in MB, so this
    // goes through the same `ImportGuards` conversion production uses.
    let view = skadi_config::ConfigView::from_pairs([(
        "import.min_free_mb".to_string(),
        (bytes / (1024 * 1024)).to_string(),
    )]);
    w.min_free = skadi_config::import_guards(&view).min_free_bytes;
}

#[given(expr = "the operator configures a minimum file size of {int} bytes in settings")]
fn operator_min_file_size(w: &mut World, bytes: u64) {
    let view = skadi_config::ConfigView::from_pairs([(
        "import.min_file_bytes".to_string(),
        bytes.to_string(),
    )]);
    w.min_file = skadi_config::import_guards(&view).min_file_bytes;
}
