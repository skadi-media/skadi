//! `TvRepo` round-trip on the active backend (SQLite by default; Postgres when
//! `SKADI_TEST_DATABASE_URL` is set), proving series/seasons/episodes persist +
//! load with the `seasons`/`episodes` populated, and that delete cascades.

use skadi_core::{AcquisitionStatus, ExternalIds, ProfileId, RootFolder, TvdbId};
use skadi_testsupport::TestDb;
use skadi_tv::{Episode, Season, Series, SeriesFilter, SeriesType, TvRepo};

#[tokio::test]
async fn series_seasons_episodes_round_trip_and_cascade() {
    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
    let store = &db.store;

    // --- a series + a season + two episodes ---
    let mut s = Series::new(
        ExternalIds {
            tvdb: Some(TvdbId(121361)),
            ..Default::default()
        },
        "Game of Thrones",
        ProfileId::new(),
        RootFolder::new("/tv"),
    );
    s.year = Some(2011);
    s.series_type = SeriesType::Standard;
    store.upsert_series(&s).await.unwrap();

    let season = Season {
        episode_count: 10,
        aired_count: 10,
        ..Season::new(s.id, 1)
    };
    store.upsert_season(&season).await.unwrap();

    let mut e1 = Episode::missing(s.id, 1, 1);
    e1.title = Some("Winter Is Coming".into());
    e1.absolute_number = Some(1);
    let e2 = Episode::missing(s.id, 1, 2);
    store.upsert_episode(&e1).await.unwrap();
    store.upsert_episode(&e2).await.unwrap();

    // --- get_series populates seasons + episodes ---
    let loaded = store.get_series(s.id).await.unwrap().unwrap();
    assert_eq!(loaded.title, "Game of Thrones");
    assert_eq!(loaded.year, Some(2011));
    assert_eq!(loaded.series_type, SeriesType::Standard);
    assert_eq!(loaded.seasons.len(), 1);
    assert_eq!(loaded.seasons[0].episode_count, 10);
    assert_eq!(loaded.episodes.len(), 2);
    assert_eq!(loaded.episodes[0].number, 1);
    assert_eq!(
        loaded.episodes[0].title.as_deref(),
        Some("Winter Is Coming")
    );
    assert_eq!(loaded.episodes[0].absolute_number, Some(1));

    // --- lookup by tvdb (the TV-native key) ---
    let by_tvdb = store
        .get_series_by_tvdb(TvdbId(121361))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_tvdb.id, s.id);
    assert_eq!(by_tvdb.episodes.len(), 2);

    // --- status update + acquirable-ref decode ---
    store
        .set_episode_status(e1.id, AcquisitionStatus::Cutoff)
        .await
        .unwrap();
    let got = store
        .get_episode_by_ref(&e1.acquirable_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(got.status, AcquisitionStatus::Cutoff));

    // --- per-episode monitor toggle ---
    store.set_episode_monitored(e2.id, false).await.unwrap();
    assert!(!store.get_episode(e2.id).await.unwrap().unwrap().monitored);

    // --- monitored filter ---
    let monitored = store
        .list_series(SeriesFilter {
            monitored: Some(true),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    assert_eq!(monitored.len(), 1);
    assert!(
        store
            .list_series(SeriesFilter {
                monitored: Some(false),
                limit: None,
                offset: None,
            })
            .await
            .unwrap()
            .is_empty()
    );

    // --- delete cascades to seasons + episodes ---
    store.delete_series(s.id).await.unwrap();
    assert!(store.get_series(s.id).await.unwrap().is_none());
    assert!(store.list_episodes(s.id).await.unwrap().is_empty());
    assert!(store.list_seasons(s.id).await.unwrap().is_empty());
}

/// SKADI-T-0541: the library-import scan hides files already held, matched by
/// **inode** rather than path.
///
/// Import places library files as hardlinks to their source (SKADI-T-0424), so
/// the held copy and the one still sitting in the scan directory are the same
/// inode under two different names — the canonical library name and whatever the
/// operator called it. A path comparison misses every one of them, which is
/// exactly why the clutter appeared despite the library "knowing" about the file.
///
/// This covers the domain-specific half: that the TV repo query reaches the held
/// episode file paths, and that the identity rule then matches the source.
#[tokio::test]
async fn held_episode_files_are_identified_by_inode_not_path() {
    use std::path::PathBuf;

    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
    let store = &db.store;

    let dir = skadi_core::unique_temp_path("tv-scan-filter");
    std::fs::create_dir_all(&dir).unwrap();
    // The file as the operator has it in a scan directory…
    let source = dir.join("Show.S01E01.1080p.mkv");
    std::fs::write(&source, b"episode").unwrap();
    // …and the same bytes under the library's canonical name.
    let library = dir.join("Show (2011) - S01E01.mkv");
    std::fs::hard_link(&source, &library).unwrap();
    // A second, unrelated file that must NOT be filtered.
    let other = dir.join("Other.S01E01.1080p.mkv");
    std::fs::write(&other, b"different").unwrap();

    let mut series = Series::new(
        ExternalIds {
            tvdb: Some(TvdbId(1)),
            ..Default::default()
        },
        "Show",
        ProfileId::new(),
        RootFolder::new("/tv"),
    );
    series.monitored = true;
    store.upsert_series(&series).await.unwrap();
    // Episodes hang off a season row; without it `list_series` returns none.
    store
        .upsert_season(&Season::new(series.id, 1))
        .await
        .unwrap();
    let ep = Episode::missing(series.id, 1, 1);
    store.upsert_episode(&ep).await.unwrap();
    // The file column is written by `set_episode_status`, not by `upsert_episode`
    // — the status variant is the single source of truth for what is held.
    store
        .set_episode_status(
            ep.id,
            AcquisitionStatus::Imported {
                file: skadi_core::FileRef {
                    path: library.clone(),
                },
                quality: skadi_quality::UNKNOWN_QUALITY_ID,
                score: 0,
                at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();

    // The query the scan handler runs.
    let held: Vec<PathBuf> = store
        .list_series(SeriesFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .iter()
        .flat_map(|s| s.episodes.iter())
        .filter_map(|e| e.file.as_ref().map(|f| f.path.clone()))
        .collect();
    assert_eq!(held, vec![library.clone()], "the repo query finds the file");

    let ids = skadi_importer::file_identities(&held);
    // The differently-named hardlink is recognised…
    assert!(
        skadi_importer::file_identity(&source).is_some_and(|id| ids.contains(&id)),
        "the source shares an inode with the held library file"
    );
    // …and an unrelated file is not.
    assert!(
        skadi_importer::file_identity(&other).is_some_and(|id| !ids.contains(&id)),
        "an unrelated file must not be filtered"
    );
    // The naive check this replaces would have missed it entirely.
    assert_ne!(source, library, "names differ; only the inode matches");
}

/// `list_series` batches its seasons and episodes into two queries instead of
/// two per series (SKADI-T-0494). The batched grouping must produce **exactly**
/// what the per-series calls produce — same membership, same order — or the fix
/// trades latency for a quietly wrong library page.
#[tokio::test]
async fn a_paged_list_groups_seasons_and_episodes_exactly_as_the_single_series_path() {
    let db = TestDb::new(skadi_tv::SQLITE_MIGRATIONS, skadi_tv::POSTGRES_MIGRATIONS).await;
    let store = &db.store;

    // Three series with different shapes, including one with no episodes at all
    // and one with out-of-order inserts — the grouping must not depend on either.
    for (i, (title, tvdb, seasons, eps_per_season)) in [
        ("Game of Thrones", 121361_u64, 2_u16, 3_u16),
        ("Barren", 121362, 1, 0),
        ("Archer", 121363, 3, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let mut s = Series::new(
            ExternalIds {
                tvdb: Some(TvdbId(tvdb)),
                ..Default::default()
            },
            title,
            ProfileId::new(),
            RootFolder::new("/tv"),
        );
        s.year = Some(2011 + i as u16);
        store.upsert_series(&s).await.unwrap();
        for n in 1..=seasons {
            store.upsert_season(&Season::new(s.id, n)).await.unwrap();
            // Insert episodes highest-number-first, so any reliance on insertion
            // order rather than the ORDER BY shows up.
            for e in (1..=eps_per_season).rev() {
                store
                    .upsert_episode(&Episode::missing(s.id, n, e))
                    .await
                    .unwrap();
            }
        }
    }

    let listed = store.list_series(SeriesFilter::default()).await.unwrap();
    assert_eq!(listed.len(), 3);

    for s in &listed {
        let seasons = store.list_seasons(s.id).await.unwrap();
        let episodes = store.list_episodes(s.id).await.unwrap();
        assert_eq!(
            s.seasons.iter().map(|x| x.number).collect::<Vec<_>>(),
            seasons.iter().map(|x| x.number).collect::<Vec<_>>(),
            "seasons for {}",
            s.title
        );
        assert_eq!(
            s.episodes
                .iter()
                .map(|e| (e.season, e.number))
                .collect::<Vec<_>>(),
            episodes
                .iter()
                .map(|e| (e.season, e.number))
                .collect::<Vec<_>>(),
            "episodes for {}",
            s.title
        );
    }

    // No cross-contamination: a series' episodes are its own.
    let barren = listed.iter().find(|s| s.title == "Barren").unwrap();
    assert!(barren.episodes.is_empty());
    assert_eq!(barren.seasons.len(), 1);

    // And the same holds for a bounded page, which is the case that matters.
    let page = store
        .list_series(SeriesFilter {
            monitored: None,
            limit: Some(2),
            offset: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    for s in &page {
        let episodes = store.list_episodes(s.id).await.unwrap();
        assert_eq!(
            s.episodes.len(),
            episodes.len(),
            "page episodes for {}",
            s.title
        );
    }
}
