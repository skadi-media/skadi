//! C27 `MovieStatusSink` steps.
use chrono::Utc;
use cucumber::{then, when};

use skadi_core::{AcquisitionStatus, AppError, DownloaderId, FailureReason, FileRef, ReleaseId};
use skadi_hunter::StatusSink;
use skadi_importer::AcquirableRef;
use skadi_movies::MovieStatusSink;
use skadi_store::HistoryRepo;

use crate::bdd_support::World;

fn sink(w: &World) -> MovieStatusSink {
    let s = std::sync::Arc::new(w.store());
    MovieStatusSink::new(s.clone(), s)
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
                path: "/movies/x.mkv".into(),
            },
            quality: World::quality_named("Bluray-1080p"),
            score: 0,
            at: Utc::now(),
        },
        "Cutoff" => AcquisitionStatus::Cutoff,
        "Failed" => AcquisitionStatus::Failed {
            reason: FailureReason::DownloadFailed("client rejected".into()),
            retry_at: None,
            attempts: 1,
        },
        other => panic!("unknown status {other}"),
    }
}

#[when(expr = "the hunter records {word} for the edition of {string}")]
async fn record(w: &mut World, kind: String, title: String) {
    let r = w.editions.get(&title).expect("edition").acquirable_ref();
    sink(w)
        .set_status(&r, canned(&kind))
        .await
        .expect("set_status");
}

#[when(expr = "the hunter records {word} for the edition of {string} twice")]
async fn record_twice(w: &mut World, kind: String, title: String) {
    let r = w.editions.get(&title).expect("edition").acquirable_ref();
    let s = sink(w);
    s.set_status(&r, canned(&kind)).await.expect("first");
    s.set_status(&r, canned(&kind)).await.expect("replay");
}

#[when(
    expr = "the hunter records Imported for {string} at {string} with score {int} to file {string}"
)]
async fn record_imported(w: &mut World, title: String, quality: String, score: i32, file: String) {
    let r = w.editions.get(&title).expect("edition").acquirable_ref();
    sink(w)
        .set_status(
            &r,
            AcquisitionStatus::Imported {
                file: FileRef { path: file.into() },
                quality: World::quality_named(&quality),
                score,
                at: Utc::now(),
            },
        )
        .await
        .expect("set_status");
}

#[then(expr = "the persisted status of {string} reads back as {word}")]
async fn reads_back(w: &mut World, title: String, kind: String) {
    let r = w.editions.get(&title).expect("edition").acquirable_ref();
    let got = sink(w)
        .get_status(&r)
        .await
        .expect("get_status")
        .expect("a status");
    assert_eq!(
        std::mem::discriminant(&got),
        std::mem::discriminant(&canned(&kind)),
        "got {got:?}"
    );
}

#[then(expr = "the edition row of {string} carries file {string} quality {string} score {int}")]
async fn row_columns(w: &mut World, title: String, file: String, quality: String, score: i32) {
    let e = w.reload_edition(&title).await;
    assert_eq!(e.file.map(|f| f.path), Some(std::path::PathBuf::from(file)));
    assert_eq!(e.quality, Some(World::quality_named(&quality)));
    assert_eq!(e.format_score, score);
}

#[then(expr = "the edition row of {string} was touched within the last minute")]
async fn touched(w: &mut World, title: String) {
    let e = w.reload_edition(&title).await;
    assert!(Utc::now() - e.updated_at < chrono::Duration::minutes(1));
}

#[when("the hunter records Cutoff for a malformed edition ref")]
async fn malformed(w: &mut World) {
    let bad = AcquirableRef("not-a-uuid".into());
    let s = sink(w);
    let set = s.set_status(&bad, AcquisitionStatus::Cutoff).await;
    let get = s.get_status(&bad).await;
    w.error = Some(format!("{set:?} / {get:?}"));
    assert!(matches!(set, Err(AppError::Validation(_))));
    assert!(matches!(get, Err(AppError::Validation(_))));
}

#[then("both the write and the read are validation errors")]
async fn both_validation(w: &mut World) {
    assert!(w.error.as_deref().unwrap().contains("Validation"));
}

#[when("the hunter records Cutoff for a well-formed but unknown edition ref")]
async fn unknown_ref(w: &mut World) {
    let ghost = AcquirableRef(uuid::Uuid::new_v4().to_string());
    let s = sink(w);
    w.error = s
        .set_status(&ghost, AcquisitionStatus::Cutoff)
        .await
        .err()
        .map(|e| e.to_string());
    w.read_status = Some(s.get_status(&ghost).await.expect("get_status"));
}

#[then("no status is read back for it")]
async fn none_back(w: &mut World) {
    assert_eq!(w.read_status, Some(None));
}

/// SKADI-T-0455: a write matching no row is reported, not silently successful.
#[then("the write reports not-found without touching any row")]
async fn unknown_not_found(w: &mut World) {
    let e = w.error.as_ref().expect("the write should report not-found");
    assert!(e.contains("Not found"), "{e}");
}

#[then(expr = "the acquisition history has {int} entry/entries")]
async fn history_len(w: &mut World, n: usize) {
    w.history = w.store().list_history(50, 0).await.expect("history");
    assert_eq!(w.history.len(), n, "{:?}", w.history);
}

#[then(expr = "the latest history event is {string} labelled {string}")]
async fn latest_event(w: &mut World, event: String, label: String) {
    w.history = w.store().list_history(50, 0).await.expect("history");
    let h = w.history.first().expect("an entry");
    assert_eq!(h.event, event);
    assert_eq!(h.label, label);
    assert_eq!(h.kind, "movie");
}

#[then(expr = "the latest history event has detail {string}")]
async fn latest_detail(w: &mut World, detail: String) {
    let h = w.history.first().expect("an entry");
    assert_eq!(h.detail.as_deref(), Some(detail.as_str()));
}

#[then(expr = "the latest history event has reason code {string}")]
async fn latest_reason(w: &mut World, code: String) {
    let h = w.history.first().expect("an entry");
    assert_eq!(h.reason_code.as_deref(), Some(code.as_str()));
}

#[when(expr = "the probe reports {int}x{int} video for {string}")]
async fn media_info(w: &mut World, width: u32, height: u32, title: String) {
    let r = w.editions.get(&title).expect("edition").acquirable_ref();
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

#[then(expr = "the edition of {string} shows resolution tier {string}")]
async fn tier(w: &mut World, title: String, want: String) {
    let e = w.reload_edition(&title).await;
    assert_eq!(e.media_info.unwrap().video.unwrap().resolution_tier(), want);
}
