//! C27 `EpisodeStatusSink` steps.
use chrono::Utc;
use cucumber::{then, when};

use skadi_core::{AcquisitionStatus, AppError, DownloaderId, FailureReason, FileRef, ReleaseId};
use skadi_hunter::StatusSink;
use skadi_importer::AcquirableRef;
use skadi_store::HistoryRepo;
use skadi_tv::EpisodeStatusSink;

use crate::bdd_support::World;

fn sink(w: &World) -> EpisodeStatusSink {
    let s = std::sync::Arc::new(w.store());
    EpisodeStatusSink::new(s.clone(), s)
}

fn canned(kind: &str) -> AcquisitionStatus {
    match kind {
        "Missing" => AcquisitionStatus::Missing,
        "Searching" => AcquisitionStatus::Searching {
            since: Utc::now(),
            attempts: 1,
        },
        "Snatched" => AcquisitionStatus::Snatched {
            release: ReleaseId::new(),
            downloader: DownloaderId::new(),
            at: Utc::now(),
        },
        "Downloading" => AcquisitionStatus::Downloading {
            release: ReleaseId::new(),
            progress: 0.42,
        },
        "Imported" => AcquisitionStatus::Imported {
            file: FileRef {
                path: "/tv/x.mkv".into(),
            },
            quality: World::quality_named("Bluray-1080p"),
            score: 0,
            at: Utc::now(),
        },
        "Cutoff" => AcquisitionStatus::Cutoff,
        "Failed" => AcquisitionStatus::Failed {
            reason: FailureReason::ImportFailed("no placed files".into()),
            retry_at: None,
            attempts: 1,
        },
        other => panic!("unknown status {other}"),
    }
}

#[when(expr = "the hunter records {word} for episode S{int}E{int} of {string}")]
async fn record(w: &mut World, kind: String, s: u16, n: u16, title: String) {
    let r = w.episode(&title, s, n).acquirable_ref();
    sink(w)
        .set_status(&r, canned(&kind))
        .await
        .expect("set_status");
}

#[when(expr = "the hunter records {word} for episode S{int}E{int} of {string} twice")]
async fn record_twice(w: &mut World, kind: String, s: u16, n: u16, title: String) {
    let r = w.episode(&title, s, n).acquirable_ref();
    let sink = sink(w);
    sink.set_status(&r, canned(&kind)).await.expect("first");
    sink.set_status(&r, canned(&kind)).await.expect("replay");
}

#[then(expr = "the persisted status of S{int}E{int} of {string} reads back as {word}")]
async fn reads_back(w: &mut World, s: u16, n: u16, title: String, kind: String) {
    let r = w.episode(&title, s, n).acquirable_ref();
    let got = sink(w).get_status(&r).await.unwrap().expect("a status");
    assert_eq!(
        std::mem::discriminant(&got),
        std::mem::discriminant(&canned(&kind)),
        "{got:?}"
    );
}

#[then(expr = "the episode row of S{int}E{int} of {string} carries file {string} quality {string}")]
async fn row(w: &mut World, s: u16, n: u16, title: String, file: String, q: String) {
    let e = w.reload_episode(&title, s, n).await;
    assert_eq!(e.file.map(|f| f.path), Some(std::path::PathBuf::from(file)));
    assert_eq!(e.quality, Some(World::quality_named(&q)));
    assert!(Utc::now() - e.updated_at < chrono::Duration::minutes(1));
}

#[then(expr = "the latest history event is {string} labelled {string}")]
async fn latest(w: &mut World, event: String, label: String) {
    w.history = w.store().list_history(50, 0).await.unwrap();
    let h = w.history.first().expect("an entry");
    assert_eq!(h.event, event);
    assert_eq!(h.label, label);
    assert_eq!(h.kind, "episode");
}

#[then(expr = "the latest history event has reason code {string}")]
async fn reason(w: &mut World, code: String) {
    let h = w.history.first().expect("an entry");
    assert_eq!(h.reason_code.as_deref(), Some(code.as_str()));
}

#[then(expr = "the acquisition history has {int} entry/entries")]
async fn history_len(w: &mut World, n: usize) {
    w.history = w.store().list_history(50, 0).await.unwrap();
    assert_eq!(w.history.len(), n, "{:?}", w.history);
}

#[when("the hunter records Cutoff for a malformed episode ref")]
async fn malformed(w: &mut World) {
    let bad = AcquirableRef("not-a-uuid".into());
    let s = sink(w);
    let set = s.set_status(&bad, AcquisitionStatus::Cutoff).await;
    let get = s.get_status(&bad).await;
    assert!(matches!(set, Err(AppError::Validation(_))), "{set:?}");
    assert!(matches!(get, Err(AppError::Validation(_))), "{get:?}");
    w.error = Some("validation".into());
}

#[then("both the write and the read are validation errors")]
async fn both(w: &mut World) {
    assert_eq!(w.error.as_deref(), Some("validation"));
}

#[when("the hunter records Cutoff for a well-formed but unknown episode ref")]
async fn unknown(w: &mut World) {
    let ghost = AcquirableRef(uuid::Uuid::new_v4().to_string());
    let s = sink(w);
    w.error = s
        .set_status(&ghost, AcquisitionStatus::Cutoff)
        .await
        .err()
        .map(|e| e.to_string());
    w.read_status = Some(s.get_status(&ghost).await.unwrap());
}

/// SKADI-T-0455: a write that matched no row used to report success, so the
/// hunter could not tell it from a real one. It is a not-found now.
#[then("no status is read back for it and the write reports not-found")]
async fn unknown_not_found(w: &mut World) {
    assert_eq!(w.read_status, Some(None));
    let e = w.error.as_ref().expect("the write should report not-found");
    assert!(e.contains("Not found"), "{e}");
}

/// `EpisodeStatusSink` does not override `set_media_info` (the trait default
/// is a no-op) and `Episode` has no `media_info` column — probed streams for
/// TV are dropped on the floor, unlike movies and audiobooks.
#[when(expr = "the probe reports {int}x{int} video for episode S{int}E{int} of {string}")]
async fn media_info(w: &mut World, width: u32, height: u32, s: u16, n: u16, title: String) {
    let r = w.episode(&title, s, n).acquirable_ref();
    sink(w)
        .set_media_info(
            &r,
            skadi_core::MediaInfo {
                duration_secs: Some(100),
                video: Some(skadi_core::VideoInfo {
                    width,
                    height,
                    codec: Some("h264".into()),
                    profile: None,
                    dynamic_range: None,
                }),
                audio: None,
                // Languages arrived with SKADI-T-0422; not what this asserts.
                ..Default::default()
            },
        )
        .await
        .expect("set_media_info");
}

#[then(expr = "the probed media info is persisted for episode S{int}E{int} of {string}")]
async fn media_persisted(w: &mut World, s: u16, n: u16, title: String) {
    // SKADI-T-0451: episodes gained a `media_info` column and the sink stores the
    // probe's result, as movies and audiobooks already did.
    let e = w.reload_episode(&title, s, n).await;
    let info = e
        .media_info
        .as_ref()
        .unwrap_or_else(|| panic!("episode {} has no persisted media_info", e.id));
    let video = info
        .video
        .as_ref()
        .unwrap_or_else(|| panic!("no video track recorded: {info:?}"));
    assert_eq!((video.width, video.height), (1920, 1080), "{info:?}");
}
