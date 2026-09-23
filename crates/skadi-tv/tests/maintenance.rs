//! Probe-based grading for television (SKADI-T-0557).

use skadi_core::{AcquisitionStatus, FileRef, MediaInfo, VideoInfo};
use skadi_testsupport::TestDb;
use skadi_tv::{Episode, Series, TvRepo};

/// A prober answering from a fixed map, so the test controls what each file
/// "is" without needing real media on disk.
struct StubProber(std::collections::HashMap<String, MediaInfo>);

impl skadi_media_probe::MediaProber for StubProber {
    fn probe(&self, path: &std::path::Path) -> Option<MediaInfo> {
        self.0.get(path.to_str().unwrap_or_default()).cloned()
    }
}

fn hd() -> MediaInfo {
    MediaInfo {
        video: Some(VideoInfo {
            width: 1920,
            height: 1080,
            codec: Some("h264".into()),
            profile: None,
            dynamic_range: None,
        }),
        ..Default::default()
    }
}

/// A series with `n` imported episodes at `quality`, files under `/tv/`.
async fn seed(store: &skadi_store::Store, n: u16, quality: skadi_core::QualityId) -> Vec<String> {
    let series = Series::new(
        skadi_core::ExternalIds {
            tvdb: Some(skadi_core::TvdbId(99)),
            ..Default::default()
        },
        "Show",
        skadi_core::ProfileId::new(),
        skadi_core::RootFolder::new("/library/television"),
    );
    store.upsert_series(&series).await.unwrap();
    let mut paths = Vec::new();
    for i in 1..=n {
        let mut ep = Episode::missing(series.id, 1, i);
        let path = format!("/tv/show-s01e{i:02}.mkv");
        ep.status = AcquisitionStatus::Imported {
            file: FileRef {
                path: std::path::PathBuf::from(&path),
            },
            quality,
            score: 0,
            at: chrono::Utc::now(),
        };
        store.upsert_episode(&ep).await.unwrap();
        paths.push(path);
    }
    paths
}

#[tokio::test]
async fn probing_grades_unknown_episodes_and_leaves_assessed_ones_alone() {
    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
    let defs = skadi_quality::default_definitions();
    let paths = seed(&db.store, 3, skadi_quality::UNKNOWN_QUALITY_ID).await;

    let map = paths.iter().map(|p| (p.clone(), hd())).collect();
    let report =
        skadi_tv::maintenance::grade_unknown_by_probe(&db.store, &StubProber(map), &defs, true)
            .await
            .unwrap();
    assert_eq!(report.scanned, 3);
    assert_eq!(report.graded, 3);

    // Re-running grades nothing: every row is now assessed, so the pass skips
    // them. This is what makes the walk resumable without a checkpoint — an
    // interrupted run picks up where it stopped because "still Unknown" *is*
    // the remaining-work marker.
    let again = skadi_tv::maintenance::grade_unknown_by_probe(
        &db.store,
        &StubProber(std::collections::HashMap::new()),
        &defs,
        true,
    )
    .await
    .unwrap();
    assert_eq!(again.scanned, 0, "nothing left to do");
    assert_eq!(again.graded, 0);
    assert_eq!(again.kept, 3, "all three are now assessed and skipped");
}

#[tokio::test]
async fn an_unprobeable_episode_stays_unknown() {
    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
    let defs = skadi_quality::default_definitions();
    seed(&db.store, 2, skadi_quality::UNKNOWN_QUALITY_ID).await;

    // Empty map: nothing can be read — missing, unreadable, or a format the
    // prober does not know.
    let report = skadi_tv::maintenance::grade_unknown_by_probe(
        &db.store,
        &StubProber(std::collections::HashMap::new()),
        &defs,
        true,
    )
    .await
    .unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(report.graded, 0);
    assert_eq!(
        report.unknown, 2,
        "an unprobeable file must stay Unknown, never take a guessed tier"
    );
}

#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
    let defs = skadi_quality::default_definitions();
    let paths = seed(&db.store, 1, skadi_quality::UNKNOWN_QUALITY_ID).await;
    let map = paths.iter().map(|p| (p.clone(), hd())).collect();

    let report =
        skadi_tv::maintenance::grade_unknown_by_probe(&db.store, &StubProber(map), &defs, false)
            .await
            .unwrap();
    assert_eq!(report.graded, 1, "the report says what a real run would do");

    let series = db
        .store
        .list_series(skadi_tv::SeriesFilter::default())
        .await
        .unwrap();
    let AcquisitionStatus::Imported { quality, .. } = &series[0].episodes[0].status else {
        panic!("expected Imported")
    };
    assert!(
        skadi_quality::is_unknown_quality(*quality),
        "a dry run must not write"
    );
}
