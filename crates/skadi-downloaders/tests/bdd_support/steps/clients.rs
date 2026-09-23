//! The built-in DB-queue downloader (daemon side of the worker contract), the
//! the built-in DB-queue downloader and downloader settings rows.
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use skadi_core::{DownloaderId, IndexerId};
use skadi_downloaders::{DbDownloader, DownloadHandle, DownloadStatus, DownloaderConfig};
use skadi_indexers::{Category, Release, ReleaseFetch};
use skadi_quality::ParsedRelease;
use skadi_store::{DownloadJobRepo, DownloadJobStatus, DownloadProgress};

use crate::bdd_support::{World, temp_store};

fn release(title: &str, fetch: ReleaseFetch) -> Release {
    Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch,
        size: 1_000,
        published: Utc::now(),
        seeders: Some(10),
        categories: Vec::new(),
        parsed: ParsedRelease::default(),
    }
}

// --- the built-in skadi downloader ----------------------------------------------------

#[given(
    regex = r#"^the built-in skadi downloader over a fresh queue with incomplete dir "([^"]*)" and complete dir "([^"]*)"$"#
)]
async fn skadi_downloader(w: &mut World, incomplete: String, complete: String) {
    let store = temp_store().await;
    w.downloader = Some(Box::new(DbDownloader::new(
        DownloaderId::new(),
        Arc::new(store.clone()),
        incomplete,
        complete,
    )));
    w.store = Some(store);
}

#[given(regex = r#"^a magnet release "([^"]*)" with info hash "([^"]*)"$"#)]
fn magnet_release(w: &mut World, title: String, hash: String) {
    w.release = Some(release(
        &title,
        ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{hash}&dn=x")),
    ));
}

#[given(regex = r#"^a \.torrent URL release "([^"]*)" at "([^"]*)"$"#)]
fn url_release(w: &mut World, title: String, url: String) {
    w.release = Some(release(&title, ReleaseFetch::TorrentUrl(url)));
}

#[given(regex = r#"^an NZB release "([^"]*)"$"#)]
fn nzb_release(w: &mut World, title: String) {
    w.release = Some(release(
        &title,
        ReleaseFetch::NzbUrl("http://usenet.test/x.nzb".into()),
    ));
}

#[when(regex = r"^the hunter adds the release under category (\d+)$")]
async fn add(w: &mut World, cat: u32) {
    let r = w.release.clone().expect("a release");
    match w.downloader().add(&r, &Category(cat)).await {
        Ok(h) => {
            w.handle = Some(h);
            w.add_error = None;
        }
        Err(e) => {
            w.handle = None;
            w.add_error = Some(e.to_string());
        }
    }
}

#[then(regex = r#"^the add is rejected with a validation error mentioning "([^"]*)"$"#)]
fn add_rejected(w: &mut World, needle: String) {
    let e = w.add_error.as_deref().expect("expected the add to fail");
    assert!(e.contains(&needle), "{e}");
}

#[then(regex = r#"^the add fails with an error containing "([^"]*)"$"#)]
fn add_fails(w: &mut World, needle: String) {
    let e = w.add_error.as_deref().expect("expected the add to fail");
    assert!(e.contains(&needle), "{e}");
}

#[then(
    regex = r#"^a queued job exists for the handle with source "([^"]*)" under category "([^"]*)"$"#
)]
async fn queued_job(w: &mut World, source: String, cat: String) {
    let h = w.handle().clone();
    assert_eq!(h.category, cat);
    let job = w
        .store()
        .get_download(&h.native_id)
        .await
        .unwrap()
        .expect("a job row for the handle");
    assert_eq!(job.status, DownloadJobStatus::Queued);
    assert_eq!(job.source, source);
    assert_eq!(job.category.as_deref(), Some(cat.as_str()));
}

#[then(regex = r#"^the job carries incomplete dir "([^"]*)" and complete dir "([^"]*)"$"#)]
async fn job_dirs(w: &mut World, incomplete: String, complete: String) {
    let job = w
        .store()
        .get_download(&w.handle().native_id)
        .await
        .unwrap()
        .expect("job");
    assert_eq!(job.incomplete_dir.as_deref(), Some(incomplete.as_str()));
    assert_eq!(job.complete_dir.as_deref(), Some(complete.as_str()));
}

#[then(regex = r#"^the job's acquirable ref is the release title "([^"]*)"$"#)]
async fn job_ref(w: &mut World, title: String) {
    let job = w
        .store()
        .get_download(&w.handle().native_id)
        .await
        .unwrap()
        .expect("job");
    assert_eq!(job.acquirable_ref, title);
}

// --- simulating the worker -----------------------------------------------------------

#[when(regex = r#"^the worker claims the job as "([^"]*)"$"#)]
async fn claim(w: &mut World, worker: String) {
    let job = w
        .store()
        .claim_next(&worker)
        .await
        .unwrap()
        .expect("a queued job to claim");
    assert_eq!(job.id, w.handle().native_id);
}

#[when(regex = r"^the worker reports (\d+) of (\d+) bytes with (\d+) peers$")]
async fn progress(w: &mut World, done: i64, total: i64, peers: i32) {
    w.store()
        .update_progress(
            &w.handle().native_id,
            &DownloadProgress {
                progress_bytes: done,
                total_bytes: total,
                info_hash: Some("abc".into()),
                peers: Some(peers),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// The client's own transfer state for this tick (SKADI-T-0394) — `initializing`
/// (queued behind the client's hash/init work or fetching magnet metadata),
/// `live`, `paused`, `error`.
#[when(regex = r#"^the worker reports the client state "([^"]*)"$"#)]
async fn client_state(w: &mut World, state: String) {
    let job = w
        .store()
        .get_download(&w.handle().native_id)
        .await
        .unwrap()
        .expect("job exists");
    w.store()
        .update_progress(
            &w.handle().native_id,
            &DownloadProgress {
                progress_bytes: job.progress_bytes,
                total_bytes: job.total_bytes,
                info_hash: job.info_hash,
                peers: job.peers,
                client_state: Some(state),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}
#[when(regex = r#"^the worker marks the job complete with files "([^"]*)"$"#)]
async fn complete(w: &mut World, files: String) {
    let files: Vec<String> = files.split(',').map(str::trim).map(String::from).collect();
    w.store()
        .mark_complete(&w.handle().native_id, &files)
        .await
        .unwrap();
}

#[when("the worker marks the job seeded")]
async fn seeded(w: &mut World) {
    w.store().mark_seeded(&w.handle().native_id).await.unwrap();
}

#[when(regex = r#"^the worker marks the job failed with "([^"]*)"$"#)]
async fn failed(w: &mut World, reason: String) {
    w.store()
        .mark_error(&w.handle().native_id, &reason)
        .await
        .unwrap();
}

#[when("the operator pauses the job")]
async fn pause(w: &mut World) {
    w.store()
        .request_pause(&w.handle().native_id)
        .await
        .unwrap();
}

#[when("the worker flags the job stalled")]
async fn stalled(w: &mut World) {
    w.store()
        .set_download_stalled(&w.handle().native_id, true)
        .await
        .unwrap();
}

#[when("the worker marks the job removed")]
async fn removed(w: &mut World) {
    w.store().mark_removed(&w.handle().native_id).await.unwrap();
}

// --- status / remove / test ---------------------------------------------------------------

#[when("the hunter reads the status")]
async fn status(w: &mut World) {
    let h = w.handle().clone();
    w.status = Some(w.downloader().status(&h).await.map_err(|e| e.to_string()));
}

#[when(regex = r#"^the hunter reads the status of the unknown handle "([^"]*)"$"#)]
async fn status_unknown(w: &mut World, id: String) {
    let h = DownloadHandle {
        native_id: id,
        category: "2000".into(),
    };
    w.status = Some(w.downloader().status(&h).await.map_err(|e| e.to_string()));
}

#[when(regex = r"^the hunter removes the download (keeping|deleting) its data$")]
async fn remove(w: &mut World, mode: String) {
    let h = w.handle().clone();
    w.remove_error = w
        .downloader()
        .remove(&h, mode == "deleting")
        .await
        .err()
        .map(|e| e.to_string());
}

#[when("the downloader connectivity test runs")]
async fn test(w: &mut World) {
    w.test_result = Some(w.downloader().test().await.map_err(|e| e.to_string()));
}

fn status_of(w: &World) -> &DownloadStatus {
    match w.status.as_ref().expect("a status was read") {
        Ok(s) => s,
        Err(e) => panic!("status failed: {e}"),
    }
}

#[then("the status is queued")]
fn is_queued(w: &mut World) {
    assert!(
        matches!(status_of(w), DownloadStatus::Queued),
        "{:?}",
        w.status
    );
}

#[then(regex = r"^the status is downloading at (\d+)%$")]
fn is_downloading(w: &mut World, pct: u32) {
    match status_of(w) {
        DownloadStatus::Downloading { progress } => {
            let got = (progress * 100.0).round() as u32;
            assert_eq!(got, pct, "progress {progress}");
        }
        other => panic!("expected downloading, got {other:?}"),
    }
}

#[then(regex = r#"^the status is completed with files "([^"]*)"$"#)]
fn is_completed(w: &mut World, files: String) {
    let want: Vec<PathBuf> = files.split(',').map(str::trim).map(PathBuf::from).collect();
    match status_of(w) {
        DownloadStatus::Completed { files } => assert_eq!(files, &want),
        other => panic!("expected completed, got {other:?}"),
    }
}

#[then(regex = r#"^the status is failed with a reason containing "([^"]*)"$"#)]
fn is_failed(w: &mut World, needle: String) {
    match status_of(w) {
        DownloadStatus::Failed { reason } => assert!(reason.contains(&needle), "{reason}"),
        other => panic!("expected failed, got {other:?}"),
    }
}

#[then("the status lookup reports not found")]
fn not_found(w: &mut World) {
    match w.status.as_ref().expect("a status was read") {
        Err(e) => assert!(
            e.contains("not found") || e.contains("no download job") || e.contains("no torrent"),
            "{e}"
        ),
        Ok(s) => panic!("expected not found, got {s:?}"),
    }
}

#[then(regex = r"^the queue holds a remove request for the job with delete_data (true|false)$")]
async fn remove_requested(w: &mut World, delete: String) {
    assert!(
        w.remove_error.is_none(),
        "remove failed: {:?}",
        w.remove_error
    );
    let pending = w.store().list_remove_requested().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, w.handle().native_id);
    assert_eq!(pending[0].delete_data, delete == "true");
}

#[then("the connectivity test passes")]
fn test_ok(w: &mut World) {
    assert!(matches!(w.test_result, Some(Ok(()))), "{:?}", w.test_result);
}

#[then(regex = r#"^the connectivity test fails with an error containing "([^"]*)"$"#)]
fn test_fails(w: &mut World, needle: String) {
    match &w.test_result {
        Some(Err(e)) => assert!(e.contains(&needle), "{e}"),
        other => panic!("expected the test to fail, got {other:?}"),
    }
}

#[then(regex = r"^the download is (still|no longer) in flight from the hunter's view$")]
fn in_flight(w: &mut World, yes: String) {
    let in_flight = matches!(
        status_of(w),
        DownloadStatus::Queued | DownloadStatus::Downloading { .. }
    );
    assert_eq!(in_flight, yes == "still", "{:?}", w.status);
}

#[then(regex = r#"^the handle's native id is "([^"]*)"$"#)]
fn native_id(w: &mut World, id: String) {
    assert_eq!(w.handle().native_id, id);
}

// --- settings rows -------------------------------------------------------------------------

#[given("the downloader settings row:")]
fn settings_row(w: &mut World, step: &Step) {
    let raw = step.docstring().expect("a JSON doc string");
    w.config_json = Some(serde_json::from_str(raw).expect("valid JSON"));
}

#[when("the provider factory builds the downloader")]
fn build(w: &mut World) {
    let cfg: DownloaderConfig = match serde_json::from_value(w.config_json.clone().unwrap()) {
        Ok(c) => c,
        Err(e) => {
            w.build_error = Some(format!("deserialize: {e}"));
            return;
        }
    };
    match cfg.build(DownloaderId::new(), "pw".into()) {
        Ok(d) => {
            w.downloader = Some(d);
            w.build_error = None;
        }
        Err(e) => w.build_error = Some(e.to_string()),
    }
}

#[then("the downloader builds")]
fn builds(w: &mut World) {
    assert!(w.build_error.is_none(), "build failed: {:?}", w.build_error);
}

#[then(regex = r#"^the build is rejected with a validation error mentioning "([^"]*)"$"#)]
fn rejected(w: &mut World, needle: String) {
    let e = w.build_error.as_deref().expect("expected a build failure");
    assert!(
        !e.starts_with("deserialize:"),
        "not a validation error: {e}"
    );
    assert!(e.contains(&needle), "{e}");
}

#[then(regex = r"^the settings row (is|is not) the built-in downloader$")]
fn builtin(w: &mut World, yes: String) {
    let cfg: DownloaderConfig = serde_json::from_value(w.config_json.clone().unwrap()).unwrap();
    assert_eq!(cfg.is_builtin(), yes == "is");
}

#[then(regex = r#"^the settings row resolves skadi dirs "([^"]*)" and "([^"]*)"$"#)]
fn skadi_dirs(w: &mut World, incomplete: String, complete: String) {
    let cfg: DownloaderConfig = serde_json::from_value(w.config_json.clone().unwrap()).unwrap();
    assert_eq!(cfg.skadi_dirs(), Some((incomplete, complete)));
}

#[then(regex = r#"^the stored settings row carries the field "([^"]+)"$"#)]
fn carries_field(w: &mut World, field: String) {
    let cfg: DownloaderConfig = serde_json::from_value(w.config_json.clone().unwrap()).unwrap();
    let json = serde_json::to_value(&cfg).unwrap();
    assert!(json.get(&field).is_some(), "no {field:?} in {json}");
}

#[then("the settings row is not a valid downloader kind")]
fn invalid_kind(w: &mut World) {
    // SKADI-T-0517: a row written when qBittorrent existed must be refused at the
    // deserialize step, so the settings loader skips it with a warning instead of
    // building a client that is no longer in the binary.
    let parsed: Result<DownloaderConfig, _> =
        serde_json::from_value(w.config_json.clone().unwrap());
    assert!(parsed.is_err(), "unknown kind unexpectedly parsed");
}

#[then("the settings row is a valid downloader kind")]
fn valid_kind(w: &mut World) {
    let parsed: Result<DownloaderConfig, _> =
        serde_json::from_value(w.config_json.clone().unwrap());
    assert!(parsed.is_ok(), "{:?}", parsed.err());
}
