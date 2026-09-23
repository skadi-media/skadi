//! TV metadata sync (SKADI-T-0266): add a series from a
//! [`SeriesMetadataProvider`] (keyless Skyhook by default) and keep it fresh.
//!
//! Mirrors `skadi-movies`'s `add_movie`/`refresh_movie`: `add_series` creates the
//! series + its seasons + episodes; `refresh_series` re-syncs descriptive fields
//! and adds newly-aired episodes **without** clobbering user/acquisition state
//! (per-episode `monitored`, status, imported file). `add_series` applies the
//! [`MonitorMode`] (T-0271) to seed each season/episode's `monitored` flag.

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, Utc};

use skadi_core::{AppError, ExternalIds, ProfileId, Result, RootFolder, SeriesId, TvdbId};
use skadi_metadata::{ImageKind, SeriesMetadata, SeriesMetadataProvider};

use crate::episode::{Episode, Season};
use crate::monitor::MonitorMode;
use crate::repo::TvRepo;
use crate::series::{Series, SeriesType};

fn image(meta: &SeriesMetadata, kind: ImageKind) -> Option<String> {
    meta.record
        .images
        .iter()
        .find(|i| i.kind == kind)
        .map(|i| i.path.clone())
}

/// Episodes of `season` that have already aired as of `today`.
fn aired_count(meta: &SeriesMetadata, season: u16, today: NaiveDate) -> u16 {
    meta.episodes
        .iter()
        .filter(|e| e.season == season && e.air_date.is_some_and(|d| d <= today))
        .count()
        .try_into()
        .unwrap_or(u16::MAX)
}

/// Apply the provider record's **descriptive** fields onto `series`, leaving the
/// user-axis fields (`monitored`/`profile`/`root_folder`/`series_type`) untouched.
fn apply_descriptive(series: &mut Series, meta: &SeriesMetadata) {
    let rec = &meta.record;
    series.title = rec.title.clone();
    series.year = rec
        .release_date
        .map(|d| u16::try_from(d.year()).unwrap_or(0));
    series.overview = rec.overview.clone();
    series.status = meta.status.clone();
    series.network = meta.network.clone();
    series.runtime_minutes = rec.runtime_minutes;
    // A lookup that reports no genres must not erase stored ones.
    if !rec.genres.is_empty() {
        series.genres = rec.genres.clone();
    }
    if rec.content_rating.is_some() {
        series.content_rating = rec.content_rating.clone();
    }
    series.poster_url = image(meta, ImageKind::Poster);
    series.backdrop_url = image(meta, ImageKind::Backdrop);
    // Carry along the cross-reference ids the source provides (tvdb stays the key).
    if rec.external_ids.tmdb.is_some() {
        series.external_ids.tmdb = rec.external_ids.tmdb.clone();
    }
    if rec.external_ids.imdb.is_some() {
        series.external_ids.imdb = rec.external_ids.imdb.clone();
    }
}

/// Whether a series is air-date numbered rather than season/episode numbered
/// (SKADI-T-0453).
///
/// Only `is_anime` was inferred before, so daily shows were typed `Standard` and
/// lost air-date naming and matching. There is no provider flag for this, so the
/// signal is the data itself: a daily show puts many episodes into one season on
/// consecutive or near-consecutive dates. A weekly drama averages seven days
/// between episodes and a daily one averages close to one, so the midpoint
/// separates them with room to spare — a weeknights-only show still averages
/// under two.
///
/// Requires a decent run of episodes before deciding: a season with three entries
/// says nothing, and guessing wrong here changes how every file is named.
fn airs_daily(meta: &skadi_metadata::SeriesMetadata) -> bool {
    const MIN_EPISODES: usize = 10;
    const MAX_MEAN_GAP_DAYS: f64 = 3.0;

    // Work within the season with the most dated episodes: gaps between seasons
    // are long by definition and would swamp the average.
    let mut by_season: std::collections::HashMap<u16, Vec<chrono::NaiveDate>> =
        std::collections::HashMap::new();
    for e in &meta.episodes {
        if let Some(d) = e.air_date {
            by_season.entry(e.season).or_default().push(d);
        }
    }
    let Some(mut dates) = by_season.into_values().max_by_key(Vec::len) else {
        return false;
    };
    if dates.len() < MIN_EPISODES {
        return false;
    }
    dates.sort_unstable();
    let span = (*dates.last().unwrap() - dates[0]).num_days() as f64;
    let mean_gap = span / (dates.len() - 1) as f64;
    mean_gap <= MAX_MEAN_GAP_DAYS
}

/// Add a series to the library by TVDB id: fetch metadata, persist the series +
/// every season + every episode with the `monitor` mode applied (T-0271), and
/// return it loaded. Idempotent — re-adding an existing series returns it unchanged.
pub async fn add_series(
    repo: &dyn TvRepo,
    provider: &dyn SeriesMetadataProvider,
    tvdb: TvdbId,
    profile: ProfileId,
    root: RootFolder,
    monitor: MonitorMode,
) -> Result<Series> {
    if let Some(existing) = repo.get_series_by_tvdb(TvdbId(tvdb.0)).await? {
        return Ok(existing);
    }
    let meta = provider.lookup_series(TvdbId(tvdb.0)).await?;

    let mut series = Series::new(
        ExternalIds {
            tvdb: Some(tvdb),
            ..Default::default()
        },
        meta.record.title.clone(),
        profile,
        root,
    );
    apply_descriptive(&mut series, &meta);
    series.series_type = if meta.is_anime {
        SeriesType::Anime
    } else if airs_daily(&meta) {
        SeriesType::Daily
    } else {
        SeriesType::Standard
    };
    // `None` mode adds the series itself unmonitored; otherwise it's in the library.
    series.monitored = monitor != MonitorMode::None;
    series.last_metadata_refresh = Some(Utc::now());
    repo.upsert_series(&series).await?;

    let today = Utc::now().date_naive();
    // First/last non-special season number drive the firstSeason/lastSeason modes.
    let season_nums: Vec<u16> = meta
        .seasons
        .iter()
        .map(|s| s.number)
        .filter(|n| *n != 0)
        .collect();
    let first_season = season_nums.iter().copied().min().unwrap_or(1);
    let last_season = season_nums.iter().copied().max().unwrap_or(1);
    let monitors = |season: u16, number: u16, air: Option<chrono::NaiveDate>| {
        monitor.monitors(season, number, air, today, first_season, last_season)
    };

    for sm in &meta.seasons {
        let mut season = Season::new(series.id, sm.number);
        season.episode_count = sm.episode_count;
        season.aired_count = aired_count(&meta, sm.number, today);
        // A season is monitored if the mode would monitor any of its episodes.
        season.monitored = meta
            .episodes
            .iter()
            .filter(|e| e.season == sm.number)
            .any(|e| monitors(e.season, e.number, e.air_date));
        repo.upsert_season(&season).await?;
    }
    for em in &meta.episodes {
        let mut ep = Episode::missing(series.id, em.season, em.number);
        ep.absolute_number = em.absolute;
        ep.title = em.title.clone();
        ep.air_date = em.air_date;
        ep.monitored = monitors(em.season, em.number, em.air_date);
        repo.upsert_episode(&ep).await?;
    }

    repo.get_series(series.id)
        .await?
        .ok_or_else(|| AppError::Internal("series vanished immediately after add".into()))
}

/// Re-sync an existing series from the provider: refresh descriptive fields + the
/// season counts, and **add newly-aired episodes**, while preserving per-episode
/// acquisition state (status/file/quality) and the `monitored` flags.
pub async fn refresh_series(
    repo: &dyn TvRepo,
    provider: &dyn SeriesMetadataProvider,
    id: SeriesId,
) -> Result<Series> {
    let mut series = repo
        .get_series(id)
        .await?
        .ok_or_else(|| AppError::Validation(format!("series {id} not found")))?;
    let tvdb = series
        .external_ids
        .tvdb
        .clone()
        .ok_or_else(|| AppError::Validation("series has no tvdb id to refresh from".into()))?;

    let meta = provider.lookup_series(tvdb).await?;

    // Snapshot pre-refresh children before mutating `series`.
    let existing_seasons: HashMap<u16, Season> = series
        .seasons
        .iter()
        .map(|s| (s.number, s.clone()))
        .collect();
    let existing_eps: HashMap<(u16, u16), Episode> = series
        .episodes
        .iter()
        .map(|e| ((e.season, e.number), e.clone()))
        .collect();

    apply_descriptive(&mut series, &meta);
    series.last_metadata_refresh = Some(Utc::now());
    repo.upsert_series(&series).await?;

    let today = Utc::now().date_naive();
    for sm in &meta.seasons {
        let mut season = existing_seasons
            .get(&sm.number)
            .cloned()
            .unwrap_or_else(|| {
                // Brand-new season: monitored unless it's Specials — those stay
                // opt-in via the season toggle (SKADI-T-0389).
                let mut s = Season::new(series.id, sm.number);
                s.monitored = sm.number != 0;
                s
            });
        season.episode_count = sm.episode_count;
        season.aired_count = aired_count(&meta, sm.number, today);
        repo.upsert_season(&season).await?;
    }
    for em in &meta.episodes {
        let mut ep = existing_eps
            .get(&(em.season, em.number))
            .cloned()
            .unwrap_or_else(|| {
                // Newly-listed episode: inherit its season's monitored flag so a
                // switched-off season (Specials by default) doesn't re-arm itself
                // every time the provider lists another episode (SKADI-T-0389).
                let mut e = Episode::missing(series.id, em.season, em.number);
                e.monitored = existing_seasons
                    .get(&em.season)
                    .map_or(em.season != 0, |s| s.monitored);
                e
            });
        // Refresh descriptive bits; never touch id/status/monitored/file/quality.
        ep.absolute_number = em.absolute.or(ep.absolute_number);
        if em.title.is_some() {
            ep.title = em.title.clone();
        }
        if em.air_date.is_some() {
            ep.air_date = em.air_date;
        }
        repo.upsert_episode(&ep).await?;
    }

    // Prune episodes the provider no longer lists (SKADI-T-0447). Without this the
    // refresh only ever grew the catalogue: a mis-listed episode, a provider
    // renumbering a season, or a special reclassified out of the run left a row
    // that was monitored, undated and unacquirable — searched on every sweep with
    // nothing to find.
    //
    // Sonarr's rule, and the safe one: an episode that **holds a file** is never
    // deleted. The file is real whatever the provider now says, and deleting the
    // row would orphan a library file with nothing pointing at it. Those are
    // unmonitored instead, so they stop being swept but stay visible.
    let listed: std::collections::HashSet<(u16, u16)> =
        meta.episodes.iter().map(|e| (e.season, e.number)).collect();
    for (key, ep) in &existing_eps {
        if listed.contains(key) {
            continue;
        }
        if ep.file.is_some() {
            if ep.monitored {
                tracing::info!(
                    series = %series.id,
                    season = ep.season,
                    episode = ep.number,
                    "provider no longer lists this episode, but it holds a file; unmonitoring instead of deleting"
                );
                repo.set_episode_monitored(ep.id, false).await?;
            }
            continue;
        }
        tracing::info!(
            series = %series.id,
            season = ep.season,
            episode = ep.number,
            "pruning episode the provider no longer lists"
        );
        repo.delete_episode(ep.id).await?;
    }

    repo.get_series(id)
        .await?
        .ok_or_else(|| AppError::Internal("series vanished during refresh".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use skadi_core::{AcquisitionStatus, FileRef, QualityId};
    use skadi_metadata::{
        EpisodeMeta, ImageRef, MetadataMatch, MetadataQuery, MetadataRecord, SeasonMeta,
    };
    use skadi_testsupport::TestDb;
    use std::path::PathBuf;

    struct FakeProvider {
        meta: SeriesMetadata,
    }

    #[async_trait]
    impl SeriesMetadataProvider for FakeProvider {
        async fn search_series(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
            Ok(vec![])
        }
        async fn lookup_series(&self, _tvdb: TvdbId) -> Result<SeriesMetadata> {
            Ok(self.meta.clone())
        }
    }

    fn got_meta() -> SeriesMetadata {
        SeriesMetadata {
            record: MetadataRecord {
                external_ids: ExternalIds {
                    tvdb: Some(TvdbId(121361)),
                    tmdb: Some(skadi_core::TmdbId(1399)),
                    ..Default::default()
                },
                title: "Game of Thrones".into(),
                overview: Some("Nine noble families fight for control.".into()),
                runtime_minutes: Some(60),
                release_date: NaiveDate::from_ymd_opt(2011, 4, 17),
                images: vec![ImageRef {
                    kind: ImageKind::Poster,
                    path: "https://x/poster.jpg".into(),
                }],
                ..Default::default()
            },
            status: Some("Ended".into()),
            network: Some("HBO".into()),
            is_anime: false,
            seasons: vec![SeasonMeta {
                number: 1,
                name: Some("Winter is Coming".into()),
                episode_count: 2,
                air_date: NaiveDate::from_ymd_opt(2011, 4, 17),
            }],
            episodes: vec![
                EpisodeMeta {
                    season: 1,
                    number: 1,
                    title: Some("Winter Is Coming".into()),
                    air_date: NaiveDate::from_ymd_opt(2011, 4, 17),
                    ..Default::default()
                },
                EpisodeMeta {
                    season: 1,
                    number: 2,
                    title: Some("The Kingsroad".into()),
                    air_date: NaiveDate::from_ymd_opt(2011, 4, 24),
                    ..Default::default()
                },
            ],
        }
    }

    #[tokio::test]
    async fn add_then_refresh_preserves_state_and_adds_new_episodes() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let provider = FakeProvider { meta: got_meta() };

        // --- add ---
        let s = add_series(
            store,
            &provider,
            TvdbId(121361),
            ProfileId::new(),
            RootFolder::new("/tv"),
            MonitorMode::All,
        )
        .await
        .unwrap();
        assert_eq!(s.title, "Game of Thrones");
        assert_eq!(s.year, Some(2011));
        assert_eq!(s.network.as_deref(), Some("HBO"));
        assert_eq!(s.external_ids.tmdb.map(|t| t.0), Some(1399));
        assert_eq!(s.poster_url.as_deref(), Some("https://x/poster.jpg"));
        assert_eq!(s.seasons.len(), 1);
        assert_eq!(s.seasons[0].episode_count, 2);
        assert_eq!(s.episodes.len(), 2);

        // Re-adding is idempotent (same id, no dupes).
        let again = add_series(
            store,
            &provider,
            TvdbId(121361),
            ProfileId::new(),
            RootFolder::new("/tv"),
            MonitorMode::All,
        )
        .await
        .unwrap();
        assert_eq!(again.id, s.id);
        assert_eq!(again.episodes.len(), 2);

        // Mark E1 imported + unmonitor E2 — user/acquisition state we must preserve.
        let e1 = s.episodes.iter().find(|e| e.number == 1).unwrap();
        store
            .set_episode_status(
                e1.id,
                AcquisitionStatus::Imported {
                    file: FileRef {
                        path: PathBuf::from("got.s01e01.mkv"),
                    },
                    quality: QualityId::new(),
                    score: 100,
                    at: Utc::now(),
                },
            )
            .await
            .unwrap();
        let e2 = s.episodes.iter().find(|e| e.number == 2).unwrap();
        store.set_episode_monitored(e2.id, false).await.unwrap();

        // --- refresh with a newly-aired E3 + a changed E2 title ---
        let mut meta2 = got_meta();
        meta2.seasons[0].episode_count = 3;
        meta2.episodes[1].title = Some("The Kingsroad (Revised)".into());
        meta2.episodes.push(EpisodeMeta {
            season: 1,
            number: 3,
            title: Some("Lord Snow".into()),
            air_date: NaiveDate::from_ymd_opt(2011, 5, 1),
            ..Default::default()
        });
        let provider2 = FakeProvider { meta: meta2 };

        let refreshed = refresh_series(store, &provider2, s.id).await.unwrap();
        assert_eq!(refreshed.episodes.len(), 3, "E3 was added");
        assert_eq!(refreshed.seasons[0].episode_count, 3);

        let r1 = refreshed.episodes.iter().find(|e| e.number == 1).unwrap();
        assert!(
            matches!(r1.status, AcquisitionStatus::Imported { .. }),
            "E1 import state preserved across refresh"
        );
        let r2 = refreshed.episodes.iter().find(|e| e.number == 2).unwrap();
        assert!(!r2.monitored, "E2 unmonitored flag preserved");
        assert_eq!(
            r2.title.as_deref(),
            Some("The Kingsroad (Revised)"),
            "descriptive title refreshed"
        );
        let r3 = refreshed.episodes.iter().find(|e| e.number == 3).unwrap();
        assert!(matches!(r3.status, AcquisitionStatus::Missing));
        assert!(r3.monitored);
    }

    #[tokio::test]
    async fn add_series_applies_monitor_mode() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let provider = FakeProvider { meta: got_meta() };

        // Pilot mode: only S01E01 monitored.
        let s = add_series(
            store,
            &provider,
            TvdbId(121361),
            ProfileId::new(),
            RootFolder::new("/tv"),
            MonitorMode::Pilot,
        )
        .await
        .unwrap();
        let e1 = s.episodes.iter().find(|e| e.number == 1).unwrap();
        let e2 = s.episodes.iter().find(|e| e.number == 2).unwrap();
        assert!(e1.monitored, "pilot monitors S01E01");
        assert!(!e2.monitored, "pilot does not monitor S01E02");
        assert!(s.monitored, "series itself is monitored");
    }

    #[tokio::test]
    async fn add_series_none_mode_adds_unmonitored() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;
        let provider = FakeProvider { meta: got_meta() };
        let s = add_series(
            store,
            &provider,
            TvdbId(121361),
            ProfileId::new(),
            RootFolder::new("/tv"),
            MonitorMode::None,
        )
        .await
        .unwrap();
        assert!(!s.monitored, "None mode adds the series unmonitored");
        assert!(s.episodes.iter().all(|e| !e.monitored));
    }

    /// SKADI-T-0389: specials are opt-in. `All` leaves S00 unmonitored at
    /// add-time, a refresh that lists new specials keeps them off, a refresh that
    /// lists a new regular season arms it, and opting in via the season toggle
    /// makes later-listed specials inherit that choice.
    #[tokio::test]
    async fn specials_are_opt_in_at_add_and_refresh() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store = &db.store;

        let special = |number: u16| EpisodeMeta {
            season: 0,
            number,
            title: Some(format!("Special {number}")),
            air_date: NaiveDate::from_ymd_opt(2012, 1, 1),
            ..Default::default()
        };
        let mut meta = got_meta();
        meta.seasons.push(SeasonMeta {
            number: 0,
            name: Some("Specials".into()),
            episode_count: 1,
            air_date: NaiveDate::from_ymd_opt(2012, 1, 1),
        });
        meta.episodes.push(special(1));
        let provider = FakeProvider { meta: meta.clone() };

        // --- add with All: regular episodes armed, specials (episode + season row) not ---
        let s = add_series(
            store,
            &provider,
            TvdbId(121361),
            ProfileId::new(),
            RootFolder::new("/tv"),
            MonitorMode::All,
        )
        .await
        .unwrap();
        assert!(
            s.episodes
                .iter()
                .filter(|e| e.season == 1)
                .all(|e| e.monitored)
        );
        let sp1 = s.episodes.iter().find(|e| e.season == 0).unwrap();
        assert!(!sp1.monitored, "All does not arm specials");
        let s00 = s.seasons.iter().find(|x| x.number == 0).unwrap();
        assert!(!s00.monitored, "Specials season row starts off");

        // --- refresh lists another special + a brand-new S02 ---
        let mut meta2 = meta.clone();
        meta2.seasons[1].episode_count = 2;
        meta2.episodes.push(special(2));
        meta2.seasons.push(SeasonMeta {
            number: 2,
            name: None,
            episode_count: 1,
            air_date: NaiveDate::from_ymd_opt(2012, 4, 1),
        });
        meta2.episodes.push(EpisodeMeta {
            season: 2,
            number: 1,
            title: Some("The North Remembers".into()),
            air_date: NaiveDate::from_ymd_opt(2012, 4, 1),
            ..Default::default()
        });
        let r = refresh_series(
            store,
            &FakeProvider {
                meta: meta2.clone(),
            },
            s.id,
        )
        .await
        .unwrap();
        let sp2 = r
            .episodes
            .iter()
            .find(|e| e.season == 0 && e.number == 2)
            .unwrap();
        assert!(
            !sp2.monitored,
            "newly-listed special inherits the off Specials season"
        );
        let s02 = r.seasons.iter().find(|x| x.number == 2).unwrap();
        assert!(s02.monitored, "new regular season is monitored");
        let s02e01 = r
            .episodes
            .iter()
            .find(|e| e.season == 2 && e.number == 1)
            .unwrap();
        assert!(
            s02e01.monitored,
            "new regular season's episode is monitored"
        );

        // --- opt in (what the season-toggle endpoint does: season row + cascade),
        // and the next newly-listed special follows that choice ---
        store.set_season_monitored(s00.id, true).await.unwrap();
        for e in r.episodes.iter().filter(|e| e.season == 0) {
            store.set_episode_monitored(e.id, true).await.unwrap();
        }

        let mut meta3 = meta2;
        meta3.seasons[1].episode_count = 3;
        meta3.episodes.push(special(3));
        let r = refresh_series(store, &FakeProvider { meta: meta3 }, s.id)
            .await
            .unwrap();
        let sp3 = r
            .episodes
            .iter()
            .find(|e| e.season == 0 && e.number == 3)
            .unwrap();
        assert!(
            sp3.monitored,
            "after opt-in, newly-listed specials are monitored"
        );
    }
}

#[cfg(test)]
mod daily_tests {
    use super::*;
    use skadi_metadata::{EpisodeMeta, MetadataRecord, SeriesMetadata};

    fn meta(season: u16, dates: &[(i32, u32, u32)]) -> SeriesMetadata {
        SeriesMetadata {
            record: MetadataRecord::default(),
            status: None,
            network: None,
            is_anime: false,
            seasons: vec![],
            episodes: dates
                .iter()
                .enumerate()
                .map(|(i, (y, m, d))| EpisodeMeta {
                    season,
                    number: (i + 1) as u16,
                    absolute: None,
                    title: None,
                    air_date: chrono::NaiveDate::from_ymd_opt(*y, *m, *d),
                    overview: None,
                })
                .collect(),
        }
    }

    fn run(start: (i32, u32, u32), step_days: i64, n: i64) -> Vec<(i32, u32, u32)> {
        let d0 = chrono::NaiveDate::from_ymd_opt(start.0, start.1, start.2).unwrap();
        (0..n)
            .map(|i| {
                let d = d0 + chrono::Duration::days(i * step_days);
                (
                    chrono::Datelike::year(&d),
                    chrono::Datelike::month(&d),
                    chrono::Datelike::day(&d),
                )
            })
            .collect()
    }

    #[test]
    fn a_nightly_run_is_daily_and_a_weekly_one_is_not() {
        // SKADI-T-0453: no provider exposes a "daily" flag, so the signal is the
        // air-date cadence — which is what actually distinguishes the two.
        assert!(airs_daily(&meta(2024, &run((2024, 1, 1), 1, 20))));
        assert!(!airs_daily(&meta(1, &run((2024, 1, 1), 7, 20))));
    }

    #[test]
    fn a_short_run_is_never_typed_daily() {
        // Three consecutive episodes say nothing, and guessing wrong changes how
        // every file in the series is named.
        assert!(!airs_daily(&meta(1, &run((2024, 1, 1), 1, 3))));
        assert!(!airs_daily(&meta(1, &[])));
    }

    #[test]
    fn season_gaps_do_not_drag_the_average() {
        // Two nightly seasons a year apart: judged within the bigger season, not
        // across the gap, which would otherwise look weekly-or-worse.
        let mut m = meta(2024, &run((2024, 1, 1), 1, 20));
        let older = meta(2023, &run((2023, 1, 1), 1, 12));
        m.episodes.extend(older.episodes);
        assert!(airs_daily(&m));
    }
}
