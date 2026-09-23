//! Shared Given steps: the isolated library, the profile, series and episode
//! fixtures used by every C24–C27 television feature.
use chrono::{NaiveDate, Utc};
use cucumber::given;

use skadi_core::{AcquisitionStatus, FailureReason, FileRef, ProfileId};
use skadi_quality::QualityProfile;
use skadi_testsupport::TestDb;
use skadi_tv::{Episode, SeriesType, TvRepo};

use crate::bdd_support::{Db, World};

#[given("an empty television library")]
async fn empty_library(w: &mut World) {
    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
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

#[given(expr = "a monitored series {string} from {int}")]
async fn monitored_series(w: &mut World, title: String, year: u16) {
    w.save_series(&title, year, true).await;
}

#[given(expr = "an unmonitored series {string} from {int}")]
async fn unmonitored_series(w: &mut World, title: String, year: u16) {
    w.save_series(&title, year, false).await;
}

#[given(expr = "{string} is an anime series")]
async fn anime(w: &mut World, title: String) {
    let mut s = w.series.get(&title).expect("series").clone();
    s.series_type = SeriesType::Anime;
    w.store().upsert_series(&s).await.expect("upsert");
    w.series.insert(title, s);
}

#[given(expr = "{string} is a daily series")]
async fn daily(w: &mut World, title: String) {
    let mut s = w.series.get(&title).expect("series").clone();
    s.series_type = SeriesType::Daily;
    w.store().upsert_series(&s).await.expect("upsert");
    w.series.insert(title, s);
}

pub fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_else(|_| panic!("bad date {s}"))
}

fn base_ep(w: &World, title: &str, season: u16, number: u16, air: Option<&str>) -> Episode {
    let mut e = Episode::missing(w.series_id(title), season, number);
    e.air_date = air.map(date);
    e
}

fn imported(path: &str, quality: skadi_core::QualityId, score: i32) -> AcquisitionStatus {
    AcquisitionStatus::Imported {
        file: FileRef { path: path.into() },
        quality,
        score,
        at: Utc::now(),
    }
}

#[given(expr = "{string} has episode S{int}E{int} aired on {word} that is Missing")]
async fn ep_missing(w: &mut World, title: String, s: u16, n: u16, air: String) {
    let e = base_ep(w, &title, s, n, Some(&air));
    w.save_episode(&title, e).await;
}

#[given(
    expr = "{string} has episodes S{int}E{int} through E{int} aired on {word} that are Missing"
)]
async fn eps_missing(w: &mut World, title: String, s: u16, from: u16, to: u16, air: String) {
    for n in from..=to {
        let e = base_ep(w, &title, s, n, Some(&air));
        w.save_episode(&title, e).await;
    }
}

#[given(expr = "{string} has episode S{int}E{int} with no air date that is Missing")]
async fn ep_no_air(w: &mut World, title: String, s: u16, n: u16) {
    let e = base_ep(w, &title, s, n, None);
    w.save_episode(&title, e).await;
}

#[given(expr = "{string} has an unmonitored episode S{int}E{int} aired on {word}")]
async fn ep_unmonitored(w: &mut World, title: String, s: u16, n: u16, air: String) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.monitored = false;
    w.save_episode(&title, e).await;
}

#[given(expr = "{string} has episode S{int}E{int} aired on {word} with absolute number {int}")]
async fn ep_absolute(w: &mut World, title: String, s: u16, n: u16, air: String, abs: u32) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.absolute_number = Some(abs);
    w.save_episode(&title, e).await;
}

#[given(expr = "{string} has episode S{int}E{int} aired on {word} imported at {string}")]
async fn ep_imported(w: &mut World, title: String, s: u16, n: u16, air: String, q: String) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = imported("/tv/x.mkv", World::quality_named(&q), 0);
    w.save_episode(&title, e).await;
}

#[given(
    expr = "{string} has episode S{int}E{int} aired on {word} imported at {string} with format score {int}"
)]
async fn ep_imported_scored(
    w: &mut World,
    title: String,
    s: u16,
    n: u16,
    air: String,
    q: String,
    score: i32,
) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = imported("/tv/x.mkv", World::quality_named(&q), score);
    w.save_episode(&title, e).await;
}

/// What `import.rs::commit` records for an adopted file whose name yields no
/// quality: `UNKNOWN_QUALITY_ID` (SKADI-T-0399). Rows written before that fix
/// carry SDTV and are re-graded by the `backfill_unassessed_quality` pass.
#[given(
    expr = "{string} has episode S{int}E{int} aired on {word} adopted at the unassessed default quality"
)]
async fn ep_adopted(w: &mut World, title: String, s: u16, n: u16, air: String) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = imported("/tv/adopted.mkv", skadi_quality::UNKNOWN_QUALITY_ID, 0);
    w.save_episode(&title, e).await;
}

#[given(
    expr = "{string} has episode S{int}E{int} aired on {word} that failed a download with retry due {int} minutes ago"
)]
async fn ep_failed_past(w: &mut World, title: String, s: u16, n: u16, air: String, m: i64) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = AcquisitionStatus::Failed {
        reason: FailureReason::DownloadFailed("flaky".into()),
        retry_at: Some(Utc::now() - chrono::Duration::minutes(m)),
        attempts: 1,
    };
    w.save_episode(&title, e).await;
}

#[given(
    expr = "{string} has episode S{int}E{int} aired on {word} that failed a download with retry due in {int} minutes"
)]
async fn ep_failed_future(w: &mut World, title: String, s: u16, n: u16, air: String, m: i64) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = AcquisitionStatus::Failed {
        reason: FailureReason::DownloadFailed("flaky".into()),
        retry_at: Some(Utc::now() + chrono::Duration::minutes(m)),
        attempts: 1,
    };
    w.save_episode(&title, e).await;
}

#[given(
    expr = "{string} has episode S{int}E{int} aired on {word} that failed with no suitable release"
)]
async fn ep_failed_terminal(w: &mut World, title: String, s: u16, n: u16, air: String) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = AcquisitionStatus::Failed {
        reason: FailureReason::NoSuitableRelease,
        retry_at: None,
        attempts: 3,
    };
    w.save_episode(&title, e).await;
}

#[given(
    expr = "{string} has episode S{int}E{int} aired on {word} that has been Downloading for {int} minutes"
)]
async fn ep_downloading(w: &mut World, title: String, s: u16, n: u16, air: String, m: i64) {
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.status = AcquisitionStatus::Downloading {
        release: skadi_core::ReleaseId::new(),
        progress: 0.0,
    };
    e.updated_at = Utc::now() - chrono::Duration::minutes(m);
    w.save_episode(&title, e).await;
}

#[given(
    expr = "episode S{int}E{int} of {string} was imported at {string} to a library file on disk"
)]
async fn ep_imported_to_disk(w: &mut World, s: u16, n: u16, title: String, q: String) {
    let dir = w.tmp();
    let path = dir.join(format!("{}.S{s:02}E{n:02}.mkv", title.replace(' ', ".")));
    std::fs::write(&path, b"x").expect("write library file");
    let e = w.episode(&title, s, n);
    w.store()
        .set_episode_status(
            e.id,
            imported(path.to_str().unwrap(), World::quality_named(&q), 7),
        )
        .await
        .expect("set Imported");
    w.files
        .insert(crate::bdd_support::ep_key(&title, s, n), path);
}

#[given(expr = "episode S{int}E{int} of {string} was then overwritten to Missing")]
async fn ep_overwritten_missing(w: &mut World, s: u16, n: u16, title: String) {
    let e = w.episode(&title, s, n);
    w.store()
        .set_episode_status(e.id, AcquisitionStatus::Missing)
        .await
        .expect("set Missing");
}

#[given(
    expr = "episode S{int}E{int} of {string} was then overwritten to Searching {int} minutes ago"
)]
async fn ep_overwritten_searching(w: &mut World, s: u16, n: u16, title: String, m: i64) {
    let mut row = w.reload_episode(&title, s, n).await;
    row.status = AcquisitionStatus::Searching {
        since: Utc::now(),
        attempts: 1,
    };
    row.updated_at = Utc::now() - chrono::Duration::minutes(m);
    w.store().upsert_episode(&row).await.expect("upsert");
}

#[given(expr = "the library file of episode S{int}E{int} of {string} was deleted")]
async fn ep_file_deleted(w: &mut World, s: u16, n: u16, title: String) {
    let p = w
        .files
        .get(&crate::bdd_support::ep_key(&title, s, n))
        .expect("library file")
        .clone();
    std::fs::remove_file(p).expect("remove");
}

#[given(
    expr = "{string} has a Missing episode S{int}E{int} aired on {word} that still points at a library file with no recorded quality"
)]
async fn ep_demoted_no_quality(w: &mut World, title: String, s: u16, n: u16, air: String) {
    let dir = w.tmp();
    let path = dir.join(format!("{}.S{s:02}E{n:02}.mkv", title.replace(' ', ".")));
    std::fs::write(&path, b"x").expect("write library file");
    let mut e = base_ep(w, &title, s, n, Some(&air));
    e.file = Some(FileRef { path: path.clone() });
    e.quality = None;
    w.save_episode(&title, e).await;
    w.files
        .insert(crate::bdd_support::ep_key(&title, s, n), path);
}
