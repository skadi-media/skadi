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

#[when(expr = "run {string} advances {string} to stage {string}")]
fn set_stage(_w: &mut World, _run: String, acquirable: String, stage: String) {
    tracker().set_stage(&acquirable, stage);
}

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

#[then(expr = "the activity view entry for {string} carries the release size and ETA")]
fn shows_progress_fields(_w: &mut World, acquirable: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .expect("tracked");
    let json = serde_json::to_value(m).unwrap();
    assert!(
        json.get("size_bytes").is_some() && json.get("eta_seconds").is_some(),
        "the *arr queue row shows size / time-left; RunMeta has only {:?}",
        json.as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
    );
}

#[then(expr = "the activity view entry for {string} carries a human-readable title")]
fn shows_title(_w: &mut World, acquirable: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .expect("tracked");
    let json = serde_json::to_value(m).unwrap();
    assert!(
        json.get("title").is_some(),
        "queue rows need the item title, not just the opaque ref; RunMeta has {:?}",
        json.as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
    );
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

#[then(expr = "the activity view can be filtered to {string} runs")]
fn filtered_view(_w: &mut World, _kind: String) {
    // REQ-QUEUE.5: filtering/pagination by kind/stage. The tracker exposes only
    // `snapshot()`; `GET /activity` returns it whole.
    let has_filter = false;
    assert!(
        has_filter,
        "no per-kind/stage filter on the in-flight snapshot (REQ-QUEUE.5)"
    );
}
