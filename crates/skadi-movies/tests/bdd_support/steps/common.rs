//! Shared Given steps: the isolated library, the quality profile, and movie /
//! edition fixtures used by every C24–C28 feature.
use chrono::Utc;
use cucumber::given;

use skadi_core::{AcquisitionStatus, FailureReason, FileRef, ProfileId};
use skadi_movies::MoviesRepo;
use skadi_quality::QualityProfile;
use skadi_testsupport::TestDb;

use crate::bdd_support::{Db, World};

#[given("an empty movies library")]
async fn empty_library(w: &mut World) {
    let db = TestDb::new(
        skadi_movies::SQLITE_MIGRATIONS,
        skadi_movies::POSTGRES_MIGRATIONS,
    )
    .await;
    w.db = Some(Db(db));
    if w.profile.is_none() {
        w.profile = Some(two_rank_profile(
            "Bluray-720p",
            "Bluray-1080p",
            "Bluray-1080p",
        ));
    }
}

pub fn two_rank_profile(lo: &str, hi: &str, cutoff: &str) -> QualityProfile {
    QualityProfile {
        id: ProfileId::new(),
        name: "bdd".into(),
        allowed: vec![World::quality_named(lo), World::quality_named(hi)],
        cutoff: World::quality_named(cutoff),
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    }
}

#[given(expr = "a quality profile allowing {string} and {string} with cutoff {string}")]
async fn profile(w: &mut World, lo: String, hi: String, cutoff: String) {
    w.profile = Some(two_rank_profile(&lo, &hi, &cutoff));
}

#[given("the profile disallows upgrades")]
async fn no_upgrades(w: &mut World) {
    w.profile.as_mut().expect("profile").upgrade_allowed = false;
}

#[given(expr = "the profile re-checks imports until format score {int}")]
async fn format_cutoff(w: &mut World, score: i32) {
    w.upgrade_until_format_score = score;
}

#[given(expr = "a monitored movie {string} from {int} with a Missing Theatrical edition")]
async fn monitored_missing(w: &mut World, title: String, year: u16) {
    w.save_movie_with_edition(&title, year, true, AcquisitionStatus::Missing)
        .await;
}

#[given(expr = "an unmonitored movie {string} from {int} with a Missing Theatrical edition")]
async fn unmonitored_missing(w: &mut World, title: String, year: u16) {
    w.save_movie_with_edition(&title, year, false, AcquisitionStatus::Missing)
        .await;
}

fn imported(path: &str, quality: skadi_core::QualityId, score: i32) -> AcquisitionStatus {
    AcquisitionStatus::Imported {
        file: FileRef { path: path.into() },
        quality,
        score,
        at: Utc::now(),
    }
}

#[given(
    expr = "a monitored movie {string} from {int} with a Theatrical edition imported at {string}"
)]
async fn monitored_imported(w: &mut World, title: String, year: u16, quality: String) {
    let status = imported("/movies/x.mkv", World::quality_named(&quality), 0);
    w.save_movie_with_edition(&title, year, true, status).await;
}

#[given(
    expr = "a monitored movie {string} from {int} with a Theatrical edition imported at {string} with format score {int}"
)]
async fn monitored_imported_scored(
    w: &mut World,
    title: String,
    year: u16,
    quality: String,
    score: i32,
) {
    let status = imported("/movies/x.mkv", World::quality_named(&quality), score);
    w.save_movie_with_edition(&title, year, true, status).await;
}

#[given(
    expr = "an unmonitored movie {string} from {int} with a Theatrical edition imported at {string}"
)]
async fn unmonitored_imported(w: &mut World, title: String, year: u16, quality: String) {
    let status = imported("/movies/x.mkv", World::quality_named(&quality), 0);
    w.save_movie_with_edition(&title, year, false, status).await;
}

/// What `import.rs::commit` records for an adopted file whose quality could not
/// be parsed from its name: `UNKNOWN_QUALITY_ID` (SKADI-T-0399). Rows written
/// before that fix carry `default_definitions()[0]` (SDTV) and are re-graded by
/// the `backfill_unassessed_quality` maintenance pass.
#[given(
    expr = "a monitored movie {string} from {int} with a Theatrical edition adopted at the unassessed default quality"
)]
async fn adopted_unassessed(w: &mut World, title: String, year: u16) {
    let status = imported("/movies/adopted.mkv", skadi_quality::UNKNOWN_QUALITY_ID, 0);
    w.save_movie_with_edition(&title, year, true, status).await;
}

#[given(
    expr = "a monitored movie {string} from {int} whose Theatrical edition failed a download with retry due {int} minutes ago"
)]
async fn failed_retry_past(w: &mut World, title: String, year: u16, minutes: i64) {
    let status = AcquisitionStatus::Failed {
        reason: FailureReason::DownloadFailed("flaky".into()),
        retry_at: Some(Utc::now() - chrono::Duration::minutes(minutes)),
        attempts: 1,
    };
    w.save_movie_with_edition(&title, year, true, status).await;
}

#[given(
    expr = "a monitored movie {string} from {int} whose Theatrical edition failed a download with retry due in {int} minutes"
)]
async fn failed_retry_future(w: &mut World, title: String, year: u16, minutes: i64) {
    let status = AcquisitionStatus::Failed {
        reason: FailureReason::DownloadFailed("flaky".into()),
        retry_at: Some(Utc::now() + chrono::Duration::minutes(minutes)),
        attempts: 1,
    };
    w.save_movie_with_edition(&title, year, true, status).await;
}

#[given(
    expr = "a monitored movie {string} from {int} whose Theatrical edition failed with no suitable release"
)]
async fn failed_terminal(w: &mut World, title: String, year: u16) {
    let status = AcquisitionStatus::Failed {
        reason: FailureReason::NoSuitableRelease,
        retry_at: None,
        attempts: 3,
    };
    w.save_movie_with_edition(&title, year, true, status).await;
}

/// Import the edition to a real temp file (so `reconcile_stale` can stat it).
#[given(
    expr = "the Theatrical edition of {string} was imported at {string} to a library file on disk"
)]
async fn imported_to_disk(w: &mut World, title: String, quality: String) {
    let dir = w.tmp();
    let path = dir.join(format!("{}.mkv", title.replace(' ', "_")));
    std::fs::write(&path, b"x").expect("write library file");
    let e = w.editions.get(&title).expect("edition").clone();
    w.store()
        .set_edition_status(
            e.id,
            imported(path.to_str().unwrap(), World::quality_named(&quality), 7),
        )
        .await
        .expect("set Imported");
    w.files.insert(title, path);
}

#[given(expr = "the edition of {string} was then overwritten to Missing")]
async fn overwritten_missing(w: &mut World, title: String) {
    let e = w.editions.get(&title).expect("edition").clone();
    w.store()
        .set_edition_status(e.id, AcquisitionStatus::Missing)
        .await
        .expect("set Missing");
}

#[given(expr = "the edition of {string} was then overwritten to Searching {int} minutes ago")]
async fn overwritten_searching(w: &mut World, title: String, minutes: i64) {
    let mut row = w.reload_edition(&title).await;
    row.status = AcquisitionStatus::Searching {
        since: Utc::now(),
        attempts: 1,
    };
    row.updated_at = Utc::now() - chrono::Duration::minutes(minutes);
    w.store().upsert_edition(&row).await.expect("upsert");
}

#[given(expr = "the library file of {string} was deleted")]
async fn file_deleted(w: &mut World, title: String) {
    let p = w.files.get(&title).expect("library file").clone();
    std::fs::remove_file(p).expect("remove library file");
}

#[given(
    expr = "a monitored movie {string} from {int} whose Theatrical edition has been Downloading for {int} minutes"
)]
async fn downloading_for(w: &mut World, title: String, year: u16, minutes: i64) {
    let (_, mut e) = w
        .save_movie_with_edition(&title, year, true, AcquisitionStatus::Missing)
        .await;
    e.status = AcquisitionStatus::Downloading {
        release: skadi_core::ReleaseId::new(),
        progress: 0.0,
    };
    e.updated_at = Utc::now() - chrono::Duration::minutes(minutes);
    w.store().upsert_edition(&e).await.expect("upsert");
}

/// A row whose `file_path` points at a real file but whose `quality_id` column
/// is empty (adopted before quality was recorded) — the `restore_demoted_import`
/// fallback path.
#[given(
    expr = "a monitored movie {string} from {int} whose Missing Theatrical edition still points at a library file with no recorded quality"
)]
async fn demoted_no_quality(w: &mut World, title: String, year: u16) {
    let dir = w.tmp();
    let path = dir.join(format!("{}.mkv", title.replace(' ', "_")));
    std::fs::write(&path, b"x").expect("write library file");
    let (_, mut e) = w
        .save_movie_with_edition(&title, year, true, AcquisitionStatus::Missing)
        .await;
    e.file = Some(FileRef { path: path.clone() });
    e.quality = None;
    w.store().upsert_edition(&e).await.expect("upsert");
    w.files.insert(title, path);
}
