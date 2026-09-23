//! C09: the runner-free pipeline stages (`search` → `snatch` → `monitor` →
//! `import` → `notify`) driven with in-memory fakes.

use std::sync::Arc;

use cucumber::{gherkin::Step, given, then, when};

use skadi_core::{ExternalIds, MediaKind, ProfileId, Protocol};
use skadi_downloaders::DownloadStatus;
use skadi_hunter::{AcquireState, SearchSpec};
use skadi_importer::AcquirableRef;
use skadi_indexers::{Category, ReleaseFetch};
use skadi_notify::NotificationKind;

use crate::bdd_support::World;
use crate::bdd_support::fixtures::{
    RecordingNotifier, ScriptedDownloader, ScriptedIndexer, importer_into, magnet_for,
    rejecting_importer, release,
};

fn ensure_state(w: &mut World, kind: MediaKind) -> &mut AcquireState {
    if w.state.is_none() {
        w.kind = Some(kind);
        w.state = Some(AcquireState::new(
            AcquirableRef("item-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            ProfileId::new(),
        ));
    }
    w.state.as_mut().unwrap()
}

// --- search --------------------------------------------------------------

#[given(expr = "an acquire run for a movie")]
fn movie_run(w: &mut World) {
    ensure_state(w, MediaKind::Movie);
}

#[given(expr = "an indexer serving movies with:")]
fn indexer_with(w: &mut World, step: &Step) {
    let titles: Vec<String> = step
        .table()
        .expect("titles")
        .rows
        .iter()
        .skip(1)
        .map(|r| r[0].clone())
        .collect();
    let rels = titles
        .iter()
        .map(|t| release(MediaKind::Movie, t, Some(5), magnet_for(t)))
        .collect();
    w.indexers
        .push(ScriptedIndexer::serving(MediaKind::Movie, rels));
}

#[given(expr = "an indexer serving movies that is down")]
fn indexer_down(w: &mut World) {
    w.indexers.push(ScriptedIndexer::failing(MediaKind::Movie));
}

#[given(expr = "an indexer that only serves audiobooks")]
fn indexer_other_kind(w: &mut World) {
    w.indexers
        .push(ScriptedIndexer::serving(MediaKind::Audiobook, vec![]));
}

#[when("the hunter searches")]
async fn search(w: &mut World) {
    let mut state = w.state.take().expect("run");
    let res = skadi_hunter::search(&mut state, &w.indexers)
        .await
        .map_err(|e| e.to_string());
    w.state = Some(state);
    w.outcome = Some(res);
}

#[then(expr = "{int} candidates are collected")]
fn candidates_collected(w: &mut World, n: usize) {
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
    assert_eq!(w.state.as_ref().unwrap().candidates.len(), n);
}

#[then(expr = "the search fails mentioning {string}")]
fn search_fails(w: &mut World, msg: String) {
    match &w.outcome {
        Some(Err(e)) => assert!(e.contains(&msg), "error {e:?} lacks {msg:?}"),
        other => panic!("expected a search failure, got {other:?}"),
    }
}

// --- snatch --------------------------------------------------------------

#[given(expr = "the run chose the magnet release {string}")]
fn chose_magnet(w: &mut World, title: String) {
    let st = ensure_state(w, MediaKind::Movie);
    let r = release(MediaKind::Movie, &title, Some(5), magnet_for(&title));
    st.candidates = vec![r.clone()];
    st.chosen = Some(r);
}

#[given(expr = "the run chose the nzb release {string}")]
fn chose_nzb(w: &mut World, title: String) {
    let st = ensure_state(w, MediaKind::Movie);
    let r = release(
        MediaKind::Movie,
        &title,
        None,
        ReleaseFetch::NzbUrl(format!("https://nzb.example/{title}")),
    );
    st.candidates = vec![r.clone()];
    st.chosen = Some(r);
}

fn statuses_from(step: &Step) -> Vec<DownloadStatus> {
    step.table()
        .expect("statuses")
        .rows
        .iter()
        .skip(1)
        .map(|r| match r[0].as_str() {
            "Queued" => DownloadStatus::Queued,
            "Completed" => DownloadStatus::Completed {
                files: vec![std::path::PathBuf::from("/tmp/done.mkv")],
            },
            "Failed" => DownloadStatus::Failed {
                reason: "tracker says no".into(),
            },
            s if s.starts_with("Downloading") => {
                let pct: f32 = s
                    .trim_start_matches("Downloading")
                    .trim()
                    .trim_end_matches('%')
                    .parse()
                    .expect("percent");
                DownloadStatus::Downloading {
                    progress: pct / 100.0,
                }
            }
            other => panic!("unknown status {other}"),
        })
        .collect()
}

#[given(expr = "a torrent client that will report:")]
fn torrent_client(w: &mut World, step: &Step) {
    w.downloader = Some(ScriptedDownloader::new(
        Protocol::Torrent,
        statuses_from(step),
    ));
}

#[given("a torrent client")]
fn torrent_client_plain(w: &mut World) {
    w.downloader = Some(ScriptedDownloader::new(
        Protocol::Torrent,
        vec![DownloadStatus::Queued],
    ));
}

#[given("a usenet client")]
fn usenet_client(w: &mut World) {
    w.downloader = Some(ScriptedDownloader::new(
        Protocol::Usenet,
        vec![DownloadStatus::Queued],
    ));
}

#[given("a torrent client that refuses every add")]
fn refusing_client(w: &mut World) {
    w.downloader = Some(ScriptedDownloader::refusing(Protocol::Torrent));
}

fn downloaders(w: &World) -> Vec<Arc<dyn skadi_downloaders::Downloader>> {
    w.downloader
        .iter()
        .map(|d| d.clone() as Arc<dyn skadi_downloaders::Downloader>)
        .collect()
}

#[when("the hunter snatches")]
async fn snatch(w: &mut World) {
    let mut state = w.state.take().expect("run");
    let ds = downloaders(w);
    let res = skadi_hunter::snatch(&mut state, &ds, &w.indexers)
        .await
        .map_err(|e| e.to_string());
    w.state = Some(state);
    w.outcome = Some(res);
}

#[when("the hunter snatches again")]
async fn snatch_again(w: &mut World) {
    snatch(w).await;
}

#[then(expr = "the client holds {int} transfer(s)")]
fn client_holds(w: &mut World, n: usize) {
    assert_eq!(w.downloader.as_ref().expect("client").adds(), n);
}

#[then("the run records a download handle")]
fn has_handle(w: &mut World) {
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
    assert!(w.state.as_ref().unwrap().handle.is_some());
}

#[then(expr = "the stage fails mentioning {string}")]
fn stage_fails(w: &mut World, msg: String) {
    match &w.outcome {
        Some(Err(e)) => assert!(e.contains(&msg), "error {e:?} lacks {msg:?}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

// --- monitor -------------------------------------------------------------

#[given("the transfer was handed to the client")]
async fn handed_over(w: &mut World) {
    snatch(w).await;
    assert!(
        w.state.as_ref().unwrap().handle.is_some(),
        "{:?}",
        w.outcome
    );
}

#[when(expr = "the hunter monitors with a budget of {int} poll(s)")]
async fn monitor(w: &mut World, polls: u32) {
    let mut state = w.state.take().expect("run");
    let ds = downloaders(w);
    let res = skadi_hunter::monitor(
        &mut state,
        &ds,
        std::time::Duration::from_millis(1),
        polls,
        None,
    )
    .await
    .map_err(|e| format!("{e:?}"));
    w.state = Some(state);
    w.outcome = Some(res);
}

#[then(expr = "the run records {int} completed file(s)")]
fn completed_files(w: &mut World, n: usize) {
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
    let paths = w.state.as_ref().unwrap().completed_paths.clone().unwrap();
    assert_eq!(paths.len(), n);
}

#[then("monitor asks to be retried later")]
fn monitor_retryable(w: &mut World) {
    match &w.outcome {
        Some(Err(e)) => assert!(
            e.starts_with("Network"),
            "expected a retryable Network error, got {e}"
        ),
        other => panic!("expected a retryable error, got {other:?}"),
    }
}

#[then("monitor reports a hard download failure")]
fn monitor_hard(w: &mut World) {
    match &w.outcome {
        Some(Err(e)) => assert!(
            e.starts_with("Internal"),
            "expected a terminal Internal error, got {e}"
        ),
        other => panic!("expected a hard failure, got {other:?}"),
    }
}

#[when("the transfer is cancelled")]
async fn cancel(w: &mut World) {
    let ds = downloaders(w);
    skadi_hunter::pipeline::cancel_transfer(w.state.as_ref().unwrap(), &ds).await;
}

#[then(expr = "the client removed {int} transfer(s)")]
fn removed(w: &mut World, n: usize) {
    assert_eq!(w.downloader.as_ref().expect("client").removes(), n);
}

// --- import --------------------------------------------------------------

#[given(expr = "the completed download holds {int} media file(s)")]
fn completed_download(w: &mut World, n: usize) {
    let tmp = Arc::new(tempfile::tempdir().expect("tmp"));
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut files = Vec::new();
    for i in 0..n {
        let p = src.join(format!("Movie.2020.1080p.BluRay.x264-GRP.part{i}.mkv"));
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        files.push(p);
    }
    w.tmp = Some(tmp);
    let st = ensure_state(w, MediaKind::Movie);
    st.handle = Some(skadi_downloaders::DownloadHandle {
        native_id: "h-1".into(),
        category: "2000".into(),
    });
    st.completed_paths = Some(files);
}

#[when("the hunter imports into the library")]
async fn import_ok(w: &mut World) {
    let lib = w.tmp.as_ref().unwrap().path().join("library");
    let importer = importer_into(lib, "item-1");
    let mut state = w.state.take().unwrap();
    let res = skadi_hunter::import(&mut state, importer.as_ref())
        .await
        .map_err(|e| e.to_string());
    w.state = Some(state);
    w.outcome = Some(res);
}

#[when("the hunter imports with a matcher that places nothing")]
async fn import_reject(w: &mut World) {
    let importer = rejecting_importer();
    let mut state = w.state.take().unwrap();
    let res = skadi_hunter::import(&mut state, importer.as_ref())
        .await
        .map_err(|e| e.to_string());
    w.state = Some(state);
    w.outcome = Some(res);
}

#[then(expr = "{int} file(s) are placed in the library")]
fn placed(w: &mut World, n: usize) {
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
    let out = w.state.as_ref().unwrap().outcome.clone().unwrap();
    assert_eq!(out.imported.len(), n, "{out:?}");
}

#[then("the import fails because nothing was placed")]
fn import_failed(w: &mut World) {
    match &w.outcome {
        Some(Err(e)) => assert!(e.contains("no placed files"), "{e}"),
        other => panic!("expected an import failure, got {other:?}"),
    }
}

#[then("a second import of the same files counts as already imported")]
async fn reimport(w: &mut World) {
    import_ok(w).await;
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
    let out = w.state.as_ref().unwrap().outcome.clone().unwrap();
    assert!(
        !out.imported.is_empty(),
        "already-present files are counted as imported: {out:?}"
    );
}

// --- notify --------------------------------------------------------------

#[given(expr = "a notifier subscribed to {string}")]
fn notifier(w: &mut World, kinds: String) {
    let kinds = kinds
        .split(',')
        .map(|k| match k.trim() {
            "Grabbed" => NotificationKind::Grabbed,
            "Imported" => NotificationKind::Imported,
            "Failed" => NotificationKind::Failed,
            "Upgraded" => NotificationKind::Upgraded,
            other => panic!("unknown kind {other}"),
        })
        .collect();
    w.notifiers.push(RecordingNotifier::wanting(kinds));
}

#[given("a notifier whose webhook always fails")]
fn failing_notifier(w: &mut World) {
    w.notifiers.push(RecordingNotifier::failing());
}

fn notifiers(w: &World) -> Vec<Arc<dyn skadi_notify::Notifier>> {
    w.notifiers
        .iter()
        .map(|n| n.clone() as Arc<dyn skadi_notify::Notifier>)
        .collect()
}

#[when("the hunter announces the import")]
async fn announce_import(w: &mut World) {
    let st = ensure_state(w, MediaKind::Movie).clone();
    let ns = notifiers(w);
    let res = skadi_hunter::notify(&st, &ns)
        .await
        .map_err(|e| e.to_string());
    w.outcome = Some(res);
}

#[when("the hunter announces the grab")]
async fn announce_grab(w: &mut World) {
    let st = ensure_state(w, MediaKind::Movie).clone();
    let ns = notifiers(w);
    let res = skadi_hunter::notify_grabbed(&st, &ns)
        .await
        .map_err(|e| e.to_string());
    w.outcome = Some(res);
}

#[then(expr = "notifier {int} received {string}")]
fn notifier_received(w: &mut World, idx: usize, kinds: String) {
    let seen = w.notifiers[idx - 1].kinds_seen();
    let want: Vec<String> = kinds
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let got: Vec<String> = seen.iter().map(|k| format!("{k:?}")).collect();
    assert_eq!(got, want);
}

#[then(expr = "notifier {int} received nothing")]
fn notifier_nothing(w: &mut World, idx: usize) {
    assert!(w.notifiers[idx - 1].kinds_seen().is_empty());
}

#[then("the notification stage succeeds")]
fn notify_ok(w: &mut World) {
    assert!(w.outcome.as_ref().unwrap().is_ok(), "{:?}", w.outcome);
}
