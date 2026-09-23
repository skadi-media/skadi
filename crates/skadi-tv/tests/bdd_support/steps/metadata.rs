//! C26 television metadata sync steps (`add_series` / `refresh_series`).
use async_trait::async_trait;
use chrono::NaiveDate;
use cucumber::{given, then, when};

use skadi_core::{AcquisitionStatus, FileRef, ProfileId, Result, RootFolder, TvdbId};
use skadi_metadata::{
    EpisodeMeta, ImageKind, ImageRef, MetadataMatch, MetadataQuery, MetadataRecord, SeasonMeta,
    SeriesMetadata, SeriesMetadataProvider,
};
use skadi_tv::{MonitorMode, SeriesType, TvRepo, add_series, refresh_series};

use crate::bdd_support::{World, ep_key};

#[derive(Debug, Clone)]
pub struct FakeProvider {
    pub meta: SeriesMetadata,
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

fn provider(w: &mut World) -> &mut FakeProvider {
    w.provider.get_or_insert_with(|| FakeProvider {
        meta: SeriesMetadata {
            record: MetadataRecord::default(),
            status: None,
            network: None,
            is_anime: false,
            seasons: vec![],
            episodes: vec![],
        },
    })
}

fn rebuild_seasons(meta: &mut SeriesMetadata) {
    let mut nums: Vec<u16> = meta.episodes.iter().map(|e| e.season).collect();
    nums.sort_unstable();
    nums.dedup();
    meta.seasons = nums
        .into_iter()
        .map(|n| SeasonMeta {
            number: n,
            name: None,
            episode_count: meta
                .episodes
                .iter()
                .filter(|e| e.season == n)
                .count()
                .try_into()
                .unwrap(),
            air_date: meta
                .episodes
                .iter()
                .filter(|e| e.season == n)
                .find_map(|e| e.air_date),
        })
        .collect();
}

#[given(expr = "the series provider returns {string} first aired {word} on {string}")]
async fn provider_returns(w: &mut World, title: String, aired: String, network: String) {
    let p = provider(w);
    p.meta.record.title = title;
    p.meta.record.release_date = Some(super::common::date(&aired));
    p.meta.record.overview = Some("A show.".into());
    p.meta.record.runtime_minutes = Some(60);
    p.meta.record.images = vec![ImageRef {
        kind: ImageKind::Poster,
        path: "https://x/poster.jpg".into(),
    }];
    p.meta.status = Some("Continuing".into());
    p.meta.network = Some(network);
}

#[given(expr = "the provider lists episode S{int}E{int} {string} airing {word}")]
async fn provider_episode(w: &mut World, s: u16, n: u16, title: String, air: String) {
    let p = provider(w);
    p.meta.episodes.push(EpisodeMeta {
        season: s,
        number: n,
        absolute: None,
        title: Some(title),
        air_date: Some(super::common::date(&air)),
        overview: None,
    });
    rebuild_seasons(&mut p.meta);
}

#[given(expr = "the provider lists episode S{int}E{int} {string} with absolute number {int}")]
async fn provider_episode_abs(w: &mut World, s: u16, n: u16, title: String, abs: u32) {
    let p = provider(w);
    p.meta.episodes.push(EpisodeMeta {
        season: s,
        number: n,
        absolute: Some(abs),
        title: Some(title),
        air_date: NaiveDate::from_ymd_opt(2020, 1, 1),
        overview: None,
    });
    rebuild_seasons(&mut p.meta);
}

#[given(expr = "the provider no longer lists episode S{int}E{int}")]
async fn provider_drop(w: &mut World, s: u16, n: u16) {
    let p = provider(w);
    p.meta
        .episodes
        .retain(|e| !(e.season == s && e.number == n));
    rebuild_seasons(&mut p.meta);
}

#[given("the provider flags the series as anime")]
async fn provider_anime(w: &mut World) {
    provider(w).meta.is_anime = true;
}

#[given(expr = "the provider describes the series as a daily show on {string}")]
async fn provider_daily(w: &mut World, network: String) {
    let p = provider(w);
    p.meta.network = Some(network);
    p.meta.record.overview = Some("Airs every weeknight.".into());
    // Actually describe a daily show (SKADI-T-0453): the type is inferred from
    // air-date cadence, because no provider exposes a flag for it. A network name
    // and a prose overview are not something to type a library on.
    p.meta.episodes.clear();
    let start = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date");
    for i in 0..20i64 {
        // Weeknights: skip the weekend, so the mean gap is ~1.4 days.
        let d = start + chrono::Duration::days(i + (i / 5) * 2);
        p.meta.episodes.push(EpisodeMeta {
            season: 2024,
            number: (i + 1) as u16,
            absolute: None,
            title: Some(format!("Episode {}", i + 1)),
            air_date: Some(d),
            overview: None,
        });
    }
    rebuild_seasons(&mut p.meta);
}

#[when(expr = "{string} is added by TVDB id {int} with monitor mode {word}")]
async fn add(w: &mut World, title: String, tvdb: u64, mode: String) {
    let p = provider(w).clone();
    let store = w.store();
    let s = add_series(
        &store,
        &p,
        TvdbId(tvdb),
        ProfileId::new(),
        RootFolder::new("/tv"),
        MonitorMode::from_str_lossy(&mode),
    )
    .await
    .expect("add_series");
    for e in &s.episodes {
        w.episodes
            .insert(ep_key(&title, e.season, e.number), e.clone());
    }
    w.series.insert(title, s.clone());
    w.last_series = Some(s);
}

#[then(expr = "the series {string} is {word} and typed {word}")]
async fn series_flags(w: &mut World, title: String, monitored: String, kind: String) {
    let s = w.last_series.as_ref().expect("series");
    assert_eq!(s.title, title);
    assert_eq!(s.monitored, monitored == "monitored");
    assert_eq!(
        s.series_type,
        SeriesType::from_str_lossy(&kind),
        "{:?}",
        s.series_type
    );
    assert!(s.last_metadata_refresh.is_some());
}

#[then(expr = "the series {string} has {int} episode(s) and {int} season(s)")]
async fn counts(w: &mut World, title: String, eps: usize, seasons: usize) {
    let id = w.series_id(&title);
    let s = w.store().get_series(id).await.unwrap().unwrap();
    assert_eq!(s.episodes.len(), eps, "{:?}", s.episodes);
    assert_eq!(s.seasons.len(), seasons);
}

#[then(expr = "season {int} of {string} is {word}")]
async fn season_flag(w: &mut World, n: u16, title: String, flag: String) {
    let id = w.series_id(&title);
    let s = w.store().get_series(id).await.unwrap().unwrap();
    let season = s.seasons.iter().find(|x| x.number == n).expect("season");
    assert_eq!(season.monitored, flag == "monitored");
}

#[then(expr = "the series {string} carries network {string} and a poster")]
async fn descriptive(w: &mut World, title: String, network: String) {
    let s = w.last_series.as_ref().expect("series");
    assert_eq!(s.title, title);
    assert_eq!(s.network.as_deref(), Some(network.as_str()));
    assert!(s.poster_url.is_some());
}

#[when(expr = "{string} is added again by TVDB id {int} with monitor mode {word}")]
async fn add_again(w: &mut World, title: String, tvdb: u64, mode: String) {
    let before = w.series_id(&title);
    add(w, title.clone(), tvdb, mode).await;
    assert_eq!(
        w.series_id(&title),
        before,
        "the existing series is returned"
    );
}

#[then(expr = "only one series row exists for {string}")]
async fn single_row(w: &mut World, title: String) {
    let n = w
        .store()
        .list_series(skadi_tv::SeriesFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.title == title)
        .count();
    assert_eq!(n, 1);
}

#[given(expr = "episode S{int}E{int} of {string} was unmonitored by the user")]
async fn user_unmonitored(w: &mut World, s: u16, n: u16, title: String) {
    let e = w.episode(&title, s, n);
    w.store().set_episode_monitored(e.id, false).await.unwrap();
}

#[given(expr = "episode S{int}E{int} of {string} was imported at {string}")]
async fn user_imported(w: &mut World, s: u16, n: u16, title: String, q: String) {
    let e = w.episode(&title, s, n);
    w.store()
        .set_episode_status(
            e.id,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/tv/x.mkv".into(),
                },
                quality: World::quality_named(&q),
                score: 0,
                at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
}

#[when(expr = "the series {string} is refreshed from the provider")]
async fn refresh(w: &mut World, title: String) {
    let p = provider(w).clone();
    let id = w.series_id(&title);
    let store = w.store();
    let s = refresh_series(&store, &p, id)
        .await
        .expect("refresh_series");
    for e in &s.episodes {
        w.episodes
            .insert(ep_key(&title, e.season, e.number), e.clone());
    }
    w.last_series = Some(s);
}

#[then(expr = "episode S{int}E{int} of {string} is still Imported at {string}")]
async fn still_imported(w: &mut World, s: u16, n: u16, title: String, q: String) {
    let e = w.reload_episode(&title, s, n).await;
    match e.status {
        AcquisitionStatus::Imported { quality, .. } => {
            assert_eq!(World::quality_name(quality), q)
        }
        other => panic!("refresh clobbered status: {other:?}"),
    }
}

#[then(expr = "episode S{int}E{int} of {string} is titled {string}")]
async fn titled(w: &mut World, s: u16, n: u16, title: String, ep_title: String) {
    let e = w.reload_episode(&title, s, n).await;
    assert_eq!(e.title.as_deref(), Some(ep_title.as_str()));
}

/// SKADI-T-0447: an episode the provider dropped that still holds a file must be
/// kept — the file is real whatever the provider now says — and unmonitored so it
/// stops being swept.
#[then(expr = "episode S{int}E{int} of {string} still exists but is unmonitored")]
async fn kept_unmonitored(w: &mut World, s: u16, n: u16, title: String) {
    let id = w.series_id(&title);
    let series = w.store().get_series(id).await.unwrap().unwrap();
    let ep = series
        .episodes
        .iter()
        .find(|e| e.season == s && e.number == n)
        .unwrap_or_else(|| panic!("S{s}E{n} should have been kept, not pruned"));
    assert!(
        !ep.monitored,
        "a dropped episode with a file is unmonitored"
    );
    assert!(ep.file.is_some(), "its file reference survives the refresh");
}
