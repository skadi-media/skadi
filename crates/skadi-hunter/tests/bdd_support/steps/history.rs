//! C13: what the hunter writes into the durable history surfaces — the
//! per-step trace stream and the grab-time decision history — plus retention.

use cucumber::{given, then, when};

use skadi_core::MediaKind;
use skadi_store::{ConfigRepo, TraceRepo};

use crate::bdd_support::World;

#[when(expr = "the hunter emits a {string} trace for {string} at stage {string}")]
async fn emit(w: &mut World, event: String, acquirable: String, stage: String) {
    let kind = w.kind();
    skadi_hunter::trace::emit(kind, &acquirable, &stage, &event, "bdd", Some("{}".into())).await;
}

#[given("no domain services are registered")]
fn no_services(w: &mut World) {
    skadi_hunter::services::reset_services();
    w.kind = Some(MediaKind::Music);
}

#[then("emitting a trace is a harmless no-op")]
async fn emit_noop(w: &mut World) {
    let kind = w.kind();
    skadi_hunter::trace::emit(kind, "x", "searching", "candidates_found", "bdd", None).await;
}

#[then(expr = "the newest trace for {string} is {string} at stage {string} in domain {string}")]
async fn newest_trace(
    w: &mut World,
    acquirable: String,
    event: String,
    stage: String,
    kind: String,
) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .traces_for(&acquirable)
        .await
        .unwrap();
    let t = rows.first().expect("a trace row");
    assert_eq!(t.event, event);
    assert_eq!(t.stage, stage);
    assert_eq!(t.kind, kind);
}

#[then(expr = "the trace stream lists {int} event(s) newest first")]
async fn trace_count(w: &mut World, n: usize) {
    let rows = w.store.as_ref().unwrap().list_traces(100, 0).await.unwrap();
    assert_eq!(rows.len(), n);
    assert!(rows.windows(2).all(|p| p[0].at >= p[1].at));
}

#[then("the hunter's history retention is operator-configurable")]
async fn retention_configurable(w: &mut World) {
    // SKADI-T-0440: retention was the compile-time constant
    // `DEFAULT_HISTORY_RETENTION_DAYS`, so *arr's settings-exposed retention
    // (REQ-HISTORY.9 / NFR-HISTORY.8) had no equivalent here. It is now
    // `history.retention_days`, re-read on each tick.
    // Asserted against the **registry**, not the stored rows: being registered is
    // what makes a key settable, and a fresh store legitimately has no row until
    // the env seeder runs.
    let spec = skadi_config::spec("history.retention_days")
        .expect("history.retention_days is a registered config key");
    assert_eq!(spec.kind, skadi_config::ValueKind::U64);
    // The default preserves the constant it replaces, so an existing install's
    // behaviour is unchanged until the operator says otherwise.
    assert_eq!(
        spec.default.parse::<i64>().ok(),
        Some(skadi_hunter::DEFAULT_HISTORY_RETENTION_DAYS),
        "the default must match the constant it replaces"
    );
    // And the store the worker reads is reachable, so a written value is picked
    // up on the next tick.
    let _ = w.store.as_ref().unwrap().list_config().await.unwrap();
}
