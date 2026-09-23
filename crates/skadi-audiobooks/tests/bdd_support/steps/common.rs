//! Shared Given steps for the audiobook features.
use chrono::Utc;
use cucumber::given;

use skadi_audiobooks::{AudiobooksRepo, SeriesLink};
use skadi_core::{AcquisitionStatus, FailureReason, FileRef, ProfileId};
use skadi_quality::QualityProfile;
use skadi_testsupport::TestDb;

use crate::bdd_support::{Db, World};

#[given("an empty audiobook library")]
async fn empty_library(w: &mut World) {
    let db = TestDb::new(
        skadi_audiobooks::SQLITE_MIGRATIONS,
        skadi_audiobooks::POSTGRES_MIGRATIONS,
    )
    .await;
    w.db = Some(Db(db));
    if w.profile.is_none() {
        w.profile = Some(two_rank_profile("MP3-64", "M4B-256", "M4B-256"));
    }
}

pub fn two_rank_profile(lo: &str, hi: &str, cutoff: &str) -> QualityProfile {
    QualityProfile {
        id: ProfileId::new(),
        name: "bdd-audiobook".into(),
        allowed: vec![World::quality_named(lo), World::quality_named(hi)],
        cutoff: World::quality_named(cutoff),
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    }
}

#[given(expr = "an audiobook profile allowing {string} and {string} with cutoff {string}")]
async fn profile(w: &mut World, lo: String, hi: String, cutoff: String) {
    w.profile = Some(two_rank_profile(&lo, &hi, &cutoff));
}

#[given("the built-in audiobook profile with upgrades allowed")]
async fn builtin_profile(w: &mut World) {
    let mut p = skadi_audiobooks::default_audiobook_profile();
    p.upgrade_allowed = true;
    w.profile = Some(p);
}

#[given("the profile disallows upgrades")]
async fn no_upgrades(w: &mut World) {
    w.profile.as_mut().expect("profile").upgrade_allowed = false;
}

fn imported(path: &str, quality: skadi_core::QualityId, score: i32) -> AcquisitionStatus {
    AcquisitionStatus::Imported {
        file: FileRef { path: path.into() },
        quality,
        score,
        at: Utc::now(),
    }
}

#[given(expr = "a monitored book {string} by {string} with ASIN {word} that is Missing")]
async fn monitored_missing(w: &mut World, title: String, author: String, asin: String) {
    w.save_book_with_file(&title, &author, &asin, true, AcquisitionStatus::Missing)
        .await;
}

#[given(expr = "an unmonitored book {string} by {string} with ASIN {word} that is Missing")]
async fn unmonitored_missing(w: &mut World, title: String, author: String, asin: String) {
    w.save_book_with_file(&title, &author, &asin, false, AcquisitionStatus::Missing)
        .await;
}

#[given(expr = "a monitored book {string} by {string} with ASIN {word} imported at {string}")]
async fn monitored_imported(w: &mut World, title: String, author: String, asin: String, q: String) {
    let status = imported("/audiobooks/x.m4b", World::quality_named(&q), 0);
    w.save_book_with_file(&title, &author, &asin, true, status)
        .await;
}

#[given(expr = "an unmonitored book {string} by {string} with ASIN {word} imported at {string}")]
async fn unmonitored_imported(
    w: &mut World,
    title: String,
    author: String,
    asin: String,
    q: String,
) {
    let status = imported("/audiobooks/x.m4b", World::quality_named(&q), 0);
    w.save_book_with_file(&title, &author, &asin, false, status)
        .await;
}

#[given(
    expr = "a monitored book {string} by {string} with ASIN {word} that failed a download with retry due {int} minutes ago"
)]
async fn failed_past(w: &mut World, title: String, author: String, asin: String, m: i64) {
    let status = AcquisitionStatus::Failed {
        reason: FailureReason::DownloadFailed("flaky".into()),
        retry_at: Some(Utc::now() - chrono::Duration::minutes(m)),
        attempts: 1,
    };
    w.save_book_with_file(&title, &author, &asin, true, status)
        .await;
}

#[given(
    expr = "a monitored book {string} by {string} with ASIN {word} that failed a download with retry due in {int} minutes"
)]
async fn failed_future(w: &mut World, title: String, author: String, asin: String, m: i64) {
    let status = AcquisitionStatus::Failed {
        reason: FailureReason::DownloadFailed("flaky".into()),
        retry_at: Some(Utc::now() + chrono::Duration::minutes(m)),
        attempts: 1,
    };
    w.save_book_with_file(&title, &author, &asin, true, status)
        .await;
}

#[given(
    expr = "a monitored book {string} by {string} with ASIN {word} that failed with no suitable release"
)]
async fn failed_terminal(w: &mut World, title: String, author: String, asin: String) {
    let status = AcquisitionStatus::Failed {
        reason: FailureReason::NoSuitableRelease,
        retry_at: None,
        attempts: 3,
    };
    w.save_book_with_file(&title, &author, &asin, true, status)
        .await;
}

#[given(
    expr = "a monitored book {string} by {string} with ASIN {word} that has been Downloading for {int} minutes"
)]
async fn downloading(w: &mut World, title: String, author: String, asin: String, m: i64) {
    let (_, mut f) = w
        .save_book_with_file(&title, &author, &asin, true, AcquisitionStatus::Missing)
        .await;
    f.status = AcquisitionStatus::Downloading {
        release: skadi_core::ReleaseId::new(),
        progress: 0.0,
    };
    f.updated_at = Utc::now() - chrono::Duration::minutes(m);
    w.store().upsert_book_file(&f).await.expect("upsert");
}

#[given(expr = "the book {word} belongs to series {string} at position {string}")]
async fn in_series(w: &mut World, asin: String, series: String, pos: String) {
    let mut b = w.books.get(&asin).expect("book").clone();
    let s = match w.store().get_series_by_name(&series).await.unwrap() {
        Some(s) => s,
        None => {
            let s = skadi_audiobooks::Series::new(&series);
            w.store().upsert_series(&s).await.unwrap();
            s
        }
    };
    b.series = Some(SeriesLink {
        series_id: s.id,
        name: s.name,
        position: Some(pos),
    });
    w.store().upsert_book(&b).await.expect("upsert");
    w.books.insert(asin, b);
}

#[given(expr = "the file of {word} was imported at {string} to a library file on disk")]
async fn imported_to_disk(w: &mut World, asin: String, q: String) {
    let dir = w.tmp();
    let path = dir.join(format!("{asin}.m4b"));
    std::fs::write(&path, b"audio").expect("write");
    let f = w.files.get(&asin).expect("file").clone();
    w.store()
        .set_book_file_status(
            f.id,
            imported(path.to_str().unwrap(), World::quality_named(&q), 7),
        )
        .await
        .expect("set Imported");
    w.paths.insert(asin, path);
}

#[given(expr = "the file of {word} was then overwritten to Missing")]
async fn overwritten_missing(w: &mut World, asin: String) {
    let f = w.files.get(&asin).expect("file").clone();
    w.store()
        .set_book_file_status(f.id, AcquisitionStatus::Missing)
        .await
        .expect("set Missing");
}

#[given(expr = "the file of {word} was then overwritten to an import failure due for retry")]
async fn overwritten_failed(w: &mut World, asin: String) {
    let f = w.files.get(&asin).expect("file").clone();
    w.store()
        .set_book_file_status(
            f.id,
            AcquisitionStatus::Failed {
                reason: FailureReason::ImportFailed("no placed files".into()),
                retry_at: Some(Utc::now() - chrono::Duration::minutes(1)),
                attempts: 3,
            },
        )
        .await
        .expect("set Failed");
}

#[given(expr = "the file of {word} was then overwritten to Downloading {int} minutes ago")]
async fn overwritten_downloading(w: &mut World, asin: String, m: i64) {
    let mut row = w.reload_file(&asin).await;
    row.status = AcquisitionStatus::Downloading {
        release: skadi_core::ReleaseId::new(),
        progress: 0.4,
    };
    row.updated_at = Utc::now() - chrono::Duration::minutes(m);
    w.store().upsert_book_file(&row).await.expect("upsert");
}

#[given(expr = "the library file of {word} was deleted")]
async fn file_deleted(w: &mut World, asin: String) {
    let p = w.paths.get(&asin).expect("path").clone();
    std::fs::remove_file(p).expect("remove");
}

#[given(
    expr = "a monitored book {string} by {string} with ASIN {word} whose Missing file still points at a library file with no recorded quality"
)]
async fn demoted_no_quality(w: &mut World, title: String, author: String, asin: String) {
    let dir = w.tmp();
    let path = dir.join(format!("{asin}.m4b"));
    std::fs::write(&path, b"audio").expect("write");
    let (_, mut f) = w
        .save_book_with_file(&title, &author, &asin, true, AcquisitionStatus::Missing)
        .await;
    f.file = Some(FileRef { path: path.clone() });
    f.quality = None;
    f.format_score = 7;
    w.store().upsert_book_file(&f).await.expect("upsert");
    w.paths.insert(asin, path);
}
