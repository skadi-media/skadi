//! C27 `AudiobookStatusSink` steps.
use chrono::Utc;
use cucumber::{then, when};

use skadi_audiobooks::AudiobookStatusSink;
use skadi_core::{AcquisitionStatus, AppError, DownloaderId, FailureReason, FileRef, ReleaseId};
use skadi_hunter::StatusSink;
use skadi_importer::AcquirableRef;
use skadi_store::HistoryRepo;

use crate::bdd_support::World;

fn sink(w: &World) -> AudiobookStatusSink {
    let s = std::sync::Arc::new(w.store());
    AudiobookStatusSink::new(s.clone(), s)
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
                path: "/audiobooks/x.m4b".into(),
            },
            quality: World::quality_named("M4B-256"),
            score: 0,
            at: Utc::now(),
        },
        "Cutoff" => AcquisitionStatus::Cutoff,
        "Failed" => AcquisitionStatus::Failed {
            reason: FailureReason::NoSuitableRelease,
            retry_at: None,
            attempts: 1,
        },
        other => panic!("unknown status {other}"),
    }
}

#[when(expr = "the hunter records {word} for the file of {word}")]
async fn record(w: &mut World, kind: String, asin: String) {
    let r = w.files.get(&asin).expect("file").acquirable_ref();
    sink(w)
        .set_status(&r, canned(&kind))
        .await
        .expect("set_status");
}

#[when(expr = "the hunter records {word} for the file of {word} twice")]
async fn record_twice(w: &mut World, kind: String, asin: String) {
    let r = w.files.get(&asin).expect("file").acquirable_ref();
    let s = sink(w);
    s.set_status(&r, canned(&kind)).await.expect("first");
    s.set_status(&r, canned(&kind)).await.expect("replay");
}

#[then(expr = "the persisted status of {word} reads back as {word}")]
async fn reads_back(w: &mut World, asin: String, kind: String) {
    let r = w.files.get(&asin).expect("file").acquirable_ref();
    let got = sink(w).get_status(&r).await.unwrap().expect("a status");
    assert_eq!(
        std::mem::discriminant(&got),
        std::mem::discriminant(&canned(&kind)),
        "{got:?}"
    );
}

#[then(expr = "the file row of {word} carries file {string} quality {string}")]
async fn row(w: &mut World, asin: String, file: String, q: String) {
    let f = w.reload_file(&asin).await;
    assert_eq!(f.file.map(|x| x.path), Some(std::path::PathBuf::from(file)));
    assert_eq!(f.quality, Some(World::quality_named(&q)));
    assert!(Utc::now() - f.updated_at < chrono::Duration::minutes(1));
}

#[then(expr = "the latest history event is {string} labelled {string}")]
async fn latest(w: &mut World, event: String, label: String) {
    w.history = w.store().list_history(50, 0).await.unwrap();
    let h = w.history.first().expect("an entry");
    assert_eq!(h.event, event);
    assert_eq!(h.label, label);
    assert_eq!(h.kind, "audiobook");
}

#[then(expr = "the latest history event has detail {string}")]
async fn detail(w: &mut World, d: String) {
    let h = w.history.first().expect("an entry");
    assert_eq!(h.detail.as_deref(), Some(d.as_str()));
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

#[when("the hunter records Cutoff for a malformed file ref")]
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

#[when("the hunter records Cutoff for a well-formed but unknown file ref")]
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

#[when(expr = "the probe reports a {int} kbps {word} stream for {word}")]
async fn media_info(w: &mut World, kbps: u32, codec: String, asin: String) {
    let r = w.files.get(&asin).expect("file").acquirable_ref();
    sink(w)
        .set_media_info(
            &r,
            skadi_core::MediaInfo {
                duration_secs: Some(36_000),
                video: None,
                audio: Some(skadi_core::AudioInfo {
                    codec: Some(codec),
                    channels: Some(2),
                    bitrate_kbps: Some(kbps),
                    sample_rate_hz: Some(44_100),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("set_media_info");
}

#[then(expr = "the file of {word} shows a {int} kbps audio stream")]
async fn shows(w: &mut World, asin: String, kbps: u32) {
    let f = w.reload_file(&asin).await;
    assert_eq!(
        f.media_info.unwrap().audio.unwrap().bitrate_kbps,
        Some(kbps)
    );
}
