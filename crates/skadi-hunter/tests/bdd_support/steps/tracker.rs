//! C12: the in-flight tracker's own contract (`start`/`try_start`/`set_stage`/
//! `adopt`/`finish_adopted`/`expire_adopted`/`observe_progress`/`snapshot`).
//! Process-global, so every scenario here is `@serial`.

use cucumber::{given, then, when};

use skadi_core::MediaKind;
use skadi_hunter::tracker;
use skadi_hunter::tracker::Ownership;

use crate::bdd_support::World;

#[given("an empty in-flight tracker")]
fn empty_tracker(w: &mut World) {
    let t = tracker();
    for m in t.snapshot() {
        t.finish(&m.acquirable_ref);
    }
    w.kind = Some(MediaKind::Movie);
    assert!(t.snapshot().is_empty());
}

#[given(expr = "the sweep started run {string} for {string}")]
#[when(expr = "the sweep starts run {string} for {string}")]
fn start(w: &mut World, run: String, acquirable: String) {
    let ok = tracker().try_start(run.clone(), w.kind(), acquirable.clone());
    w.last_claim = Some(ok);
    w.tracked_refs.push(acquirable.clone());
    w.run_ids.insert(acquirable, run);
}

#[when(expr = "a replayed workflow {string} enters stage {string} for {string}")]
#[given(expr = "a replayed workflow {string} entered stage {string} for {string}")]
fn adopt(w: &mut World, run: String, stage: String, acquirable: String) {
    let own = tracker().adopt(w.kind(), &acquirable, stage, Some(&run));
    w.last_ownership = Some(own);
    w.tracked_refs.push(acquirable);
}

#[when(expr = "a legacy workflow with no run id enters stage {string} for {string}")]
fn adopt_legacy(w: &mut World, stage: String, acquirable: String) {
    let own = tracker().adopt(w.kind(), &acquirable, stage, None);
    w.last_ownership = Some(own);
    w.tracked_refs.push(acquirable);
}

#[given(expr = "run {string} advances {string} to stage {string}")]
#[when(expr = "run {string} advances {string} to stage {string}")]
fn set_stage(_w: &mut World, _run: String, acquirable: String, stage: String) {
    tracker().set_stage(&acquirable, stage);
}

#[given(expr = "{string} is recorded as chosen for {string} out of {int} candidates")]
#[when(expr = "{string} is recorded as chosen for {string} out of {int} candidates")]
fn set_chosen(_w: &mut World, title: String, acquirable: String, n: usize) {
    tracker().set_chosen(&acquirable, Some(title), n);
}

#[when(expr = "the decision {string} is recorded for {string}")]
fn set_decision(_w: &mut World, decision: String, acquirable: String) {
    tracker().set_decision(&acquirable, Some(decision));
}

#[when(expr = "the run for {string} finishes")]
fn finish(_w: &mut World, acquirable: String) {
    tracker().finish(&acquirable);
}

#[when(expr = "replayed workflow {string} finishes {string}")]
fn finish_adopted(_w: &mut World, run: String, acquirable: String) {
    tracker().finish_adopted(&acquirable, Some(&run));
}

#[when(expr = "the sweep expires adopted runs idle for more than {int} hours")]
fn expire(w: &mut World, hours: i64) {
    w.expired = tracker().expire_adopted(chrono::Duration::hours(hours));
}

#[when(expr = "monitor observes {int} percent for {string}")]
fn observe(w: &mut World, pct: u32, acquirable: String) {
    let o = tracker().observe_progress(
        &acquirable,
        pct as f32 / 100.0,
        chrono::Duration::minutes(5),
    );
    w.last_flush = o.map(|o| o.flush);
}

#[then("the claim succeeds")]
fn claim_ok(w: &mut World) {
    assert_eq!(w.last_claim, Some(true));
}

#[then("the claim is refused because a run is already in flight")]
fn claim_refused(w: &mut World) {
    assert_eq!(w.last_claim, Some(false));
}

#[then(expr = "the workflow is {word} by the tracker")]
fn ownership(w: &mut World, word: String) {
    let want = match word.as_str() {
        "adopted" => Ownership::Adopted,
        "recognised" => Ownership::Mine,
        "foreign" => Ownership::Foreign,
        other => panic!("unknown ownership {other}"),
    };
    assert_eq!(w.last_ownership, Some(want));
}

#[then(expr = "the activity view lists {int} run(s)")]
fn lists(_w: &mut World, n: usize) {
    assert_eq!(tracker().snapshot().len(), n);
}

#[then(expr = "the activity view shows {string} at stage {string}")]
fn shows_stage(_w: &mut World, acquirable: String, stage: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .unwrap_or_else(|| panic!("{acquirable} not in {snap:?}"));
    assert_eq!(m.current_stage, stage);
}

#[then(expr = "the activity view shows {string} chose {string} of {int} with decision {string}")]
fn shows_chosen(_w: &mut World, acquirable: String, title: String, n: usize, decision: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .expect("tracked");
    assert_eq!(m.chosen_title.as_deref(), Some(title.as_str()));
    assert_eq!(m.candidates_considered, Some(n));
    assert_eq!(m.decision.as_deref(), Some(decision.as_str()));
}

#[given(expr = "the sweep started a {string} run {string} for {string}")]
fn start_kind(w: &mut World, kind: String, run: String, acquirable: String) {
    let kind = skadi_hunter::ActivityFilter::parse(Some(&kind), None)
        .expect("kind")
        .kinds[0];
    assert!(tracker().try_start(run.clone(), kind, acquirable.clone()));
    w.tracked_refs.push(acquirable.clone());
    w.run_ids.insert(acquirable, run);
}

#[when(expr = "the download row for {string} is {int} of {int} bytes with {int} seconds left")]
fn download_row(w: &mut World, acquirable: String, done: i64, total: i64, eta: i64) {
    // The row as the hunter leaves it at snatch (SKADI-T-0689): keyed by the
    // release title, with the item it was grabbed for as `target_ref`.
    let release = tracker()
        .snapshot()
        .into_iter()
        .find(|m| m.acquirable_ref == acquirable)
        .and_then(|m| m.chosen_title)
        .expect("a chosen release");
    let now = chrono::Utc::now();
    w.download_rows.push(skadi_store::DownloadJob {
        id: format!("dl-{acquirable}"),
        acquirable_ref: release,
        source: "magnet:?xt=urn:btih:c12".into(),
        category: None,
        status: skadi_store::DownloadJobStatus::Downloading,
        info_hash: None,
        progress_bytes: done,
        total_bytes: total,
        down_speed_bps: Some(1_000),
        up_speed_bps: None,
        uploaded_bytes: None,
        peers: None,
        peers_seen: None,
        eta_seconds: Some(eta),
        files: vec![],
        error: None,
        worker_id: None,
        delete_data: false,
        incomplete_dir: None,
        complete_dir: None,
        created_at: now,
        updated_at: now,
        completed_at: None,
        lease_expires_at: None,
        client_state: None,
        target_kind: Some("movie".into()),
        target_ref: Some(acquirable),
        import_state: None,
        import_error: None,
    });
}

#[then(
    expr = "the activity view entry for {string} carries size {int}, downloaded {int} and ETA {int}"
)]
fn shows_progress_fields(w: &mut World, acquirable: String, size: i64, done: i64, eta: i64) {
    // What `GET /activity` serves: the snapshot with the download rows merged
    // in one pass.
    let mut snap = tracker().snapshot();
    assert!(skadi_hunter::tracker::wants_downloads(&snap));
    skadi_hunter::tracker::merge_downloads(&mut snap, &w.download_rows);
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .expect("tracked");
    let json = serde_json::to_value(m).unwrap();
    assert_eq!(json["size_bytes"], size, "{json}");
    assert_eq!(json["downloaded_bytes"], done, "{json}");
    assert_eq!(json["eta_seconds"], eta, "{json}");
    assert_eq!(json["download_id"], format!("dl-{acquirable}"), "{json}");
}

#[when(
    expr = "the run for {string} is titled from a search for {string} season {int} episode {int}"
)]
fn titled(_w: &mut World, acquirable: String, title: String, season: u16, episode: u16) {
    let spec = skadi_hunter::SearchSpec {
        kind: MediaKind::Series,
        trigger: Default::default(),
        titles: vec![title, "Alias".into()],
        year: None,
        external_ids: Default::default(),
        categories: vec![],
        tv: Some(skadi_hunter::TvScope {
            season,
            episode: Some(episode),
            absolute: None,
            air_date: None,
        }),
        series: None,
        tags: None,
    };
    tracker().set_title(&acquirable, spec.display_title());
}

#[then(expr = "the activity view entry for {string} carries the title {string}")]
fn shows_title(_w: &mut World, acquirable: String, title: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .expect("tracked");
    let json = serde_json::to_value(m).unwrap();
    assert_eq!(json["title"], title, "{json}");
}

fn filtered(kind: Option<&str>, stage: Option<&str>) -> Vec<String> {
    let f = skadi_hunter::ActivityFilter::parse(kind, stage).expect("filter");
    let mut refs: Vec<String> = tracker()
        .snapshot_filtered(&f)
        .into_iter()
        .map(|m| m.acquirable_ref)
        .collect();
    refs.sort();
    refs
}

#[then(expr = "the activity view filtered to kind {string} lists only {string}")]
fn filtered_kind(_w: &mut World, kind: String, only: String) {
    assert_eq!(filtered(Some(&kind), None), vec![only]);
}

#[then(expr = "the activity view filtered to stage {string} lists only {string}")]
fn filtered_stage(_w: &mut World, stage: String, only: String) {
    assert_eq!(filtered(None, Some(&stage)), vec![only]);
}

#[then(expr = "the activity view filtered to kind {string} and stage {string} lists nothing")]
fn filtered_both(_w: &mut World, kind: String, stage: String) {
    assert!(filtered(Some(&kind), Some(&stage)).is_empty());
}

#[then(expr = "the tracker still holds {string} for run {string}")]
fn still_holds(_w: &mut World, acquirable: String, run: String) {
    assert_eq!(
        tracker().run_id_for(&acquirable).as_deref(),
        Some(run.as_str())
    );
}

#[then(expr = "{string} is not in flight")]
fn not_active(_w: &mut World, acquirable: String) {
    assert!(!tracker().is_active(&acquirable));
}

#[then(expr = "nothing was expired")]
fn nothing_expired(w: &mut World) {
    assert!(w.expired.is_empty(), "{:?}", w.expired);
}

#[then("the progress is flushed to the status sink")]
fn flushed(w: &mut World) {
    assert_eq!(w.last_flush, Some(true));
}

#[then("the progress is throttled")]
fn throttled(w: &mut World) {
    assert_eq!(w.last_flush, Some(false));
}

#[then(expr = "the transfer watch for {string} has best progress {int} percent")]
fn watch_best(_w: &mut World, acquirable: String, pct: u32) {
    let watch = tracker().transfer_watch(&acquirable).expect("watch");
    assert_eq!((watch.best_progress * 100.0).round() as u32, pct);
}
