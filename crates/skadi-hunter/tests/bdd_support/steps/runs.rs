//! C09/C12/C13/C14: the workflow **step bodies** (`steps::snatch` → `monitor`
//! → `import` → `notify`) run in-process against the service registry, the
//! in-flight tracker, an in-memory status sink and a throwaway SQLite store.
//! Every scenario using these is `@serial` (process-global registry/tracker).

use std::sync::Arc;

use cucumber::{given, then, when};

use skadi_core::{AcquisitionStatus, ExternalIds, MediaKind, ProfileId, Protocol};
use skadi_downloaders::DownloadStatus;
use skadi_hunter::services::reset_services;
use skadi_hunter::{
    AcquireState, HunterServices, InMemoryStatusSink, ScoringConfig, SearchSpec, StatusSink as _,
    TvScope, set_services, tracker,
};
use skadi_importer::AcquirableRef;
use skadi_indexers::{Category, release_key};
use skadi_store::{BlocklistRepo, DecisionHistoryRepo, TraceRepo};

use crate::bdd_support::World;
use crate::bdd_support::fixtures::{
    ScriptedDownloader, ScriptedIndexer, importer_into, magnet_for, rejecting_importer, release,
    standard,
};

fn kind_of(name: &str) -> MediaKind {
    match name {
        "movie" => MediaKind::Movie,
        "tv" | "series" => MediaKind::Series,
        "audiobook" => MediaKind::Audiobook,
        other => panic!("unknown domain {other}"),
    }
}

/// Drop every in-flight entry: `@serial` scenarios never overlap, and the
/// non-serial ones never touch the tracker, so a clean slate is safe.
fn clear_tracker() {
    let t = tracker();
    for m in t.snapshot() {
        t.finish(&m.acquirable_ref);
    }
}

#[given(expr = "a registered {word} domain with an in-memory status sink")]
async fn registered_domain(w: &mut World, domain: String) {
    clear_tracker();
    let kind = kind_of(&domain);
    w.kind = Some(kind);
    let tmp = w
        .tmp
        .get_or_insert_with(|| Arc::new(tempfile::tempdir().expect("tmp")))
        .clone();
    let url = format!("sqlite://{}", tmp.path().join("skadi.db").display());
    let store = skadi_store::Store::connect(&url).expect("store");
    store.run_migrations().await.expect("migrations");
    let status = Arc::new(InMemoryStatusSink::new());
    let downloader = w
        .downloader
        .get_or_insert_with(|| {
            ScriptedDownloader::new(Protocol::Torrent, vec![DownloadStatus::Queued])
        })
        .clone();
    let (profile, defs) = standard();
    let library = tmp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let services = Arc::new(HunterServices {
        kind,
        store: store.clone(),
        status: status.clone(),
        indexers: vec![ScriptedIndexer::serving(kind, vec![])],
        downloaders: vec![downloader as Arc<dyn skadi_downloaders::Downloader>],
        importer: if w.reject_imports {
            rejecting_importer()
        } else {
            importer_into(library, "item-1")
        },
        importer_factory: None,
        notifiers: w
            .notifiers
            .iter()
            .map(|n| n.clone() as Arc<dyn skadi_notify::Notifier>)
            .collect(),
        scoring: ScoringConfig {
            definitions: defs,
            profile,
            formats: vec![],
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(services.clone());
    w.services = Some(services);
    w.status = Some(status);
    w.store = Some(store);
}

fn run_state(w: &World, acquirable: &str, title: &str) -> AcquireState {
    let kind = w.kind();
    let tv = (kind == MediaKind::Series).then_some(TvScope {
        season: 3,
        episode: acquirable.rsplit('E').next().and_then(|e| e.parse().ok()),
        absolute: None,
        air_date: None,
    });
    let r = release(kind, title, Some(20), magnet_for(title));
    let mut st = AcquireState::new(
        AcquirableRef(acquirable.to_string()),
        SearchSpec {
            trigger: Default::default(),
            kind,
            titles: vec![title.split('.').next().unwrap_or(title).to_string()],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(if kind == MediaKind::Series {
                5000
            } else {
                2000
            })],
            tv,
            series: None,
            tags: None,
        },
        ProfileId::new(),
    );
    st.candidates = vec![r.clone()];
    st.chosen = Some(r);
    st
}

#[given(expr = "a run {string} for {string} that chose {string}")]
fn run_for(w: &mut World, run: String, acquirable: String, title: String) {
    let mut st = run_state(w, &acquirable, &title);
    st.run_id = Some(run.clone());
    w.tracked_refs.push(acquirable.clone());
    w.states.insert(run.clone(), st.clone());
    w.contexts.insert(run, st.into_context().expect("context"));
}

#[given(expr = "run {string} was launched by the sweep")]
fn owned_by_sweep(w: &mut World, run: String) {
    let st = w.states.get(&run).expect("run").clone();
    tracker().start(run.clone(), st.request.kind, st.acquirable.0.clone());
}

#[given(expr = "run {string} is an upgrade run")]
fn upgrade_run(w: &mut World, run: String) {
    let q = w.services.as_ref().unwrap().scoring.profile.allowed[0];
    let st = w.states.get_mut(&run).expect("run");
    st.current_quality = Some(q);
    w.contexts.insert(run, st.clone().into_context().unwrap());
}

#[given(expr = "run {string} is a manual grab")]
fn manual_run(w: &mut World, run: String) {
    let st = w.states.get_mut(&run).expect("run");
    st.manual = true;
    w.contexts.insert(run, st.clone().into_context().unwrap());
}

#[given(expr = "run {string} completed with a media file on disk")]
fn completed_on_disk(w: &mut World, run: String) {
    let tmp = w.tmp.as_ref().unwrap().clone();
    let src = tmp.path().join(format!("src-{run}"));
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("Movie.2020.1080p.BluRay.x264-GRP.mkv");
    std::fs::write(&file, vec![0u8; 4096]).unwrap();
    let st = w.states.get_mut(&run).expect("run");
    st.handle = Some(skadi_downloaders::DownloadHandle {
        native_id: "h-1".into(),
        category: "2000".into(),
    });
    st.completed_paths = Some(vec![file]);
    w.contexts.insert(run, st.clone().into_context().unwrap());
}

#[given(expr = "{string} is already Imported")]
async fn already_imported(w: &mut World, acquirable: String) {
    let q = w.services.as_ref().unwrap().scoring.profile.cutoff;
    w.status
        .as_ref()
        .unwrap()
        .set_status(
            &AcquirableRef(acquirable),
            AcquisitionStatus::Imported {
                file: skadi_core::FileRef {
                    path: "Movie (2020)/Movie.mkv".into(),
                },
                quality: q,
                score: 0,
                at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
}

async fn drive(w: &mut World, run: &str, action: &str) {
    let ctx = w.contexts.get_mut(run).expect("run context");
    let res = match action {
        "snatches" => skadi_hunter::steps::snatch(ctx).await,
        "monitors" => skadi_hunter::steps::monitor(ctx).await,
        "imports" => skadi_hunter::steps::import(ctx).await,
        "notifies" => skadi_hunter::steps::notify(ctx).await,
        other => panic!("unknown step {other}"),
    };
    w.outcome = Some(res.map_err(|e| format!("{e:?}")));
    if let Ok(st) = skadi_hunter::load_state(ctx) {
        w.states.insert(run.to_string(), st);
    }
}

#[when(expr = "run {string} {word}")]
async fn run_step(w: &mut World, run: String, action: String) {
    drive(w, &run, &action).await;
}

#[when(expr = "run {string} runs snatch through notify")]
async fn run_all(w: &mut World, run: String) {
    for s in ["snatches", "monitors", "imports", "notifies"] {
        drive(w, &run, s).await;
        if w.outcome.as_ref().unwrap().is_err() {
            break;
        }
    }
}

#[then("the step succeeds")]
fn step_ok(w: &mut World) {
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
}

#[then(expr = "the step fails mentioning {string}")]
fn step_err(w: &mut World, msg: String) {
    match &w.outcome {
        Some(Err(e)) => assert!(e.contains(&msg), "{e}"),
        other => panic!("expected failure, got {other:?}"),
    }
}

#[then(expr = "run {string} ended superseded")]
fn superseded(w: &mut World, run: String) {
    let st = w.states.get(&run).expect("run");
    assert!(
        st.terminal_failure,
        "run should be flagged terminal: {st:?}"
    );
    assert!(
        st.handle.is_none(),
        "a superseded run must hold no transfer"
    );
}

#[then(expr = "run {string} is flagged terminal")]
fn terminal(w: &mut World, run: String) {
    assert!(w.states.get(&run).expect("run").terminal_failure);
}

#[then(expr = "run {string} is not flagged terminal")]
fn not_terminal(w: &mut World, run: String) {
    assert!(!w.states.get(&run).expect("run").terminal_failure);
}

async fn status_of(w: &World, acquirable: &str) -> Option<AcquisitionStatus> {
    w.status
        .as_ref()
        .unwrap()
        .get_status(&AcquirableRef(acquirable.to_string()))
        .await
        .unwrap()
}

#[then(expr = "the status of {string} is {word}")]
async fn status_is(w: &mut World, acquirable: String, variant: String) {
    let s = status_of(w, &acquirable).await;
    let ok = matches!(
        (variant.as_str(), &s),
        ("Missing", Some(AcquisitionStatus::Missing))
            | ("Searching", Some(AcquisitionStatus::Searching { .. }))
            | ("Snatched", Some(AcquisitionStatus::Snatched { .. }))
            | ("Downloading", Some(AcquisitionStatus::Downloading { .. }))
            | ("Imported", Some(AcquisitionStatus::Imported { .. }))
            | ("Failed", Some(AcquisitionStatus::Failed { .. }))
            | ("unset", None)
    );
    assert!(ok, "status of {acquirable}: {s:?}, expected {variant}");
}

#[then(expr = "the status of {string} is Failed with a retry backoff of about {int} minutes")]
async fn failed_backoff(w: &mut World, acquirable: String, minutes: i64) {
    match status_of(w, &acquirable).await {
        Some(AcquisitionStatus::Failed {
            retry_at: Some(at), ..
        }) => {
            let mins = (at - chrono::Utc::now()).num_minutes();
            assert!(
                (minutes - 2..=minutes).contains(&mins),
                "retry_at is {mins} minutes out, expected about {minutes}"
            );
        }
        other => panic!("expected Failed{{retry_at}}, got {other:?}"),
    }
}

#[then(expr = "the status of {string} is Failed and immediately retryable")]
async fn failed_immediate(w: &mut World, acquirable: String) {
    match status_of(w, &acquirable).await {
        Some(AcquisitionStatus::Failed { retry_at, .. }) => {
            let due = retry_at.is_none_or(|at| at <= chrono::Utc::now());
            assert!(
                due,
                "Sonarr/Radarr re-search immediately after a failed grab (Redownload Failed); retry_at is {retry_at:?}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[then(expr = "the status of {string} is Downloading at {int} percent")]
async fn downloading_at(w: &mut World, acquirable: String, pct: u32) {
    match status_of(w, &acquirable).await {
        Some(AcquisitionStatus::Downloading { progress, .. }) => {
            assert_eq!((progress * 100.0).round() as u32, pct);
        }
        other => panic!("expected Downloading, got {other:?}"),
    }
}

fn key_for(w: &World, title: &str) -> String {
    release_key(&release(w.kind(), title, Some(1), magnet_for(title)))
}

#[then(expr = "{string} is blocklisted")]
async fn is_blocklisted(w: &mut World, title: String) {
    let key = key_for(w, &title);
    let store = w.store.as_ref().unwrap();
    assert!(
        store.is_blocked(&key).await.unwrap(),
        "{title} should be blocklisted"
    );
}

#[then(expr = "{string} is not blocklisted")]
async fn not_blocklisted(w: &mut World, title: String) {
    let key = key_for(w, &title);
    let store = w.store.as_ref().unwrap();
    assert!(
        !store.is_blocked(&key).await.unwrap(),
        "{title} must stay grabbable"
    );
}

#[then(expr = "the blocklist entry for {string} records reason {string}")]
async fn blocklist_reason(w: &mut World, title: String, reason: String) {
    let key = key_for(w, &title);
    let rows = w.store.as_ref().unwrap().list_blocklist().await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.release_key == key)
        .unwrap_or_else(|| panic!("no blocklist row for {title}"));
    assert!(
        row.reason.as_deref().unwrap_or("").contains(&reason),
        "reason {:?} lacks {reason:?}",
        row.reason
    );
}

#[then(expr = "the blocklist entry for {string} expires")]
async fn blocklist_expires(w: &mut World, title: String) {
    let key = key_for(w, &title);
    let rows = w.store.as_ref().unwrap().list_blocklist().await.unwrap();
    let row = rows.iter().find(|r| r.release_key == key).expect("row");
    assert!(
        row.expires_at.is_some(),
        "an automatic blocklist entry should carry the policy TTL (REQ-BLOCKLIST.8); it is permanent"
    );
}

#[then(expr = "the client removed the transfer")]
fn client_removed(w: &mut World) {
    assert!(w.downloader.as_ref().unwrap().removes() >= 1);
}

#[then(expr = "the client still holds the transfer")]
fn client_kept(w: &mut World) {
    assert_eq!(w.downloader.as_ref().unwrap().removes(), 0);
}

#[then(expr = "the stall clock for {string} has not started")]
fn stall_clock_not_started(_w: &mut World, acquirable: String) {
    // The transfer IS under watch — that is what bounds it by the 18 h lifetime
    // cap, so a genuine zombie still ends — but `live_since` is unset, so the 3 h
    // stall timeout cannot fire while the client has it merely queued
    // (SKADI-T-0394).
    let watch = tracker().transfer_watch(&acquirable);
    assert!(
        watch.is_none_or(|w| w.live_since.is_none()),
        "the transfer is only queued in the client (not live), yet the stall clock is running: {watch:?}"
    );
}

#[then(expr = "the transfer watch for {string} shows best progress {int} percent")]
fn watch_best(_w: &mut World, acquirable: String, pct: u32) {
    let watch = tracker().transfer_watch(&acquirable).expect("watch");
    assert_eq!((watch.best_progress * 100.0).round() as u32, pct);
}

#[then(expr = "the tracker shows {string} at stage {string}")]
fn tracker_stage(_w: &mut World, acquirable: String, stage: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .unwrap_or_else(|| panic!("{acquirable} not tracked: {snap:?}"));
    assert_eq!(m.current_stage, stage);
}

#[then(expr = "the tracker entry for {string} belongs to run {string}")]
fn tracker_owner(_w: &mut World, acquirable: String, run: String) {
    assert_eq!(
        tracker().run_id_for(&acquirable).as_deref(),
        Some(run.as_str())
    );
}

#[then(expr = "the tracker entry for {string} is adopted")]
fn tracker_adopted(_w: &mut World, acquirable: String) {
    let snap = tracker().snapshot();
    let m = snap
        .iter()
        .find(|m| m.acquirable_ref == acquirable)
        .expect("tracked");
    assert!(m.adopted);
}

#[then(expr = "{string} is no longer tracked")]
fn not_tracked(_w: &mut World, acquirable: String) {
    assert!(!tracker().is_active(&acquirable));
}

#[then(expr = "the sweep cannot start another run for {string}")]
fn sweep_blocked(w: &mut World, acquirable: String) {
    let kind = w.kind();
    assert!(!tracker().try_start("sweep-probe", kind, acquirable.clone()));
}

#[then(expr = "the trace for {string} has event {string}")]
async fn trace_has(w: &mut World, acquirable: String, event: String) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .traces_for(&acquirable)
        .await
        .unwrap();
    assert!(
        rows.iter().any(|t| t.event == event),
        "no {event} trace; got {:?}",
        rows.iter().map(|t| t.event.clone()).collect::<Vec<_>>()
    );
}

#[then(expr = "the trace for {string} has no event {string}")]
async fn trace_lacks(w: &mut World, acquirable: String, event: String) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .traces_for(&acquirable)
        .await
        .unwrap();
    assert!(!rows.iter().any(|t| t.event == event));
}

#[then(expr = "the decision history for {string} records kind {string}")]
async fn decision_kind(w: &mut World, acquirable: String, kind: String) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .decisions_for(&acquirable)
        .await
        .unwrap();
    let row = rows.first().expect("a decision row");
    assert_eq!(row.kind, kind, "decision row {row:?}");
}

#[then(expr = "the decision history for {string} explains the grab with a release key")]
async fn decision_key(w: &mut World, acquirable: String) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .decisions_for(&acquirable)
        .await
        .unwrap();
    let row = rows.first().expect("a decision row");
    assert!(row.release_key.is_some());
    assert!(row.decision.is_some());
    assert!(
        row.explanation.contains("\"accepted\":true"),
        "{}",
        row.explanation
    );
}

#[then(expr = "the decision history for {string} is empty")]
async fn decision_empty(w: &mut World, acquirable: String) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .decisions_for(&acquirable)
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
}

#[then(expr = "notifier {int} saw a Grabbed event exactly once")]
fn grabbed_once(w: &mut World, idx: usize) {
    let seen = w.notifiers[idx - 1].kinds_seen();
    let n = seen
        .iter()
        .filter(|k| matches!(k, skadi_notify::NotificationKind::Grabbed))
        .count();
    assert_eq!(n, 1, "{seen:?}");
}

#[given("the domain importer matches nothing")]
async fn importer_rejects(w: &mut World) {
    w.reject_imports = true;
    let domain = match w.kind() {
        MediaKind::Series => "tv",
        MediaKind::Audiobook => "audiobook",
        _ => "movie",
    };
    registered_domain(w, domain.to_string()).await;
}
