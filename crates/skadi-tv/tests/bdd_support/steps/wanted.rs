//! C24 wanted / upgrade / reconcile sweep steps (`SeriesWantedQuery`).
use cucumber::{given, then, when};

use skadi_core::AcquisitionStatus;
use skadi_hunter::WantedQuery;
use skadi_tv::{SeriesWantedQuery, WantedScoring};

use crate::bdd_support::World;

fn query(w: &World) -> SeriesWantedQuery {
    SeriesWantedQuery::new(
        w.repo(),
        WantedScoring {
            profile: w.profile(),
            search_undated_episodes: w.search_undated_episodes,
            upgrade_until_format_score: w.upgrade_until_format_score,
            regrab_unplayable: false,
        },
    )
}

#[when("the wanted sweep runs")]
async fn wanted(w: &mut World) {
    w.seeds = query(w).wanted().await.expect("wanted()");
}

#[when("the upgrade sweep runs")]
async fn upgradable(w: &mut World) {
    w.seeds = query(w).upgradable().await.expect("upgradable()");
}

#[when("the stale reconcile runs")]
async fn reconcile(w: &mut World) {
    w.recovered = query(w).reconcile_stale().await.expect("reconcile_stale()");
}

#[then(expr = "{int} seed(s) is/are emitted")]
async fn n_seeds(w: &mut World, n: usize) {
    assert_eq!(
        w.seeds.len(),
        n,
        "seeds: {:?}",
        w.seeds.iter().map(|s| s.request.tv).collect::<Vec<_>>()
    );
}

#[then("no seed is emitted")]
async fn no_seed(w: &mut World) {
    assert!(
        w.seeds.is_empty(),
        "unexpected seeds: {:?}",
        w.seeds.iter().map(|s| s.request.tv).collect::<Vec<_>>()
    );
}

#[then(expr = "the seed for S{int}E{int} of {string} carries no current quality")]
async fn no_current(w: &mut World, s: u16, n: u16, title: String) {
    let seed = w.seed_for(&title, s, n);
    assert_eq!(seed.current_quality, None);
    assert_eq!(seed.current_format_score, None);
}

#[then(
    expr = "the seed for S{int}E{int} of {string} carries current quality {string} and format score {int}"
)]
async fn current(w: &mut World, s: u16, n: u16, title: String, q: String, score: i32) {
    let seed = w.seed_for(&title, s, n);
    assert_eq!(seed.current_quality, Some(World::quality_named(&q)));
    assert_eq!(seed.current_format_score, Some(score));
}

#[then(
    expr = "the seed for S{int}E{int} of {string} scopes season {int} episode {int} in TV category 5000"
)]
async fn scope(w: &mut World, s: u16, n: u16, title: String, season: u16, episode: u16) {
    let series = w.series.get(&title).expect("series").clone();
    let seed = w.seed_for(&title, s, n);
    assert_eq!(seed.request.kind, skadi_core::MediaKind::Series);
    assert_eq!(
        seed.request.categories,
        vec![skadi_indexers::Category(5000)]
    );
    let tv = seed.request.tv.expect("tv scope");
    assert_eq!((tv.season, tv.episode), (season, Some(episode)));
    assert_eq!(seed.request.external_ids.tvdb, series.external_ids.tvdb);
    assert_eq!(seed.request.year, series.year);
    assert_eq!(seed.profile, series.profile);
}

#[then(expr = "the seed for S{int}E{int} of {string} carries absolute number {int}")]
async fn absolute(w: &mut World, s: u16, n: u16, title: String, abs: u32) {
    let seed = w.seed_for(&title, s, n);
    assert_eq!(seed.request.tv.unwrap().absolute, Some(abs));
}

#[then(expr = "the seed for S{int}E{int} of {string} carries air date {word}")]
async fn air_date(w: &mut World, s: u16, n: u16, title: String, d: String) {
    let seed = w.seed_for(&title, s, n);
    assert_eq!(
        seed.request.tv.unwrap().air_date,
        Some(super::common::date(&d))
    );
}

#[then(expr = "the seed for S{int}E{int} of {string} lists titles {string}")]
async fn titles(w: &mut World, s: u16, n: u16, title: String, list: String) {
    let want: Vec<&str> = list.split(',').map(str::trim).collect();
    assert_eq!(w.seed_for(&title, s, n).request.titles, want);
}

#[then(expr = "the first seed is a season pack for season {int} of {string}")]
async fn pack_first(w: &mut World, season: u16, title: String) {
    let id = w.series_id(&title);
    let first = w.seeds.first().expect("a seed");
    assert_eq!(first.acquirable.0, format!("season-{id}-{season}"));
    let tv = first.request.tv.expect("tv scope");
    assert_eq!((tv.season, tv.episode), (season, None));
}

#[then(expr = "no season pack seed is emitted for season {int}")]
async fn no_pack(w: &mut World, season: u16) {
    assert!(
        !w.seeds.iter().any(|s| s
            .request
            .tv
            .is_some_and(|t| t.season == season && t.episode.is_none())),
        "season {season} got a pack seed"
    );
}

#[then(expr = "{int} episode(s) is/are recovered")]
async fn recovered(w: &mut World, n: usize) {
    assert_eq!(w.recovered, n);
}

#[then(expr = "episode S{int}E{int} of {string} is {word}")]
async fn status_is(w: &mut World, s: u16, n: u16, title: String, kind: String) {
    let e = w.reload_episode(&title, s, n).await;
    // The same phrase covers the monitored flag ("is monitored/unmonitored").
    match kind.as_str() {
        "monitored" => return assert!(e.monitored, "{e:?}"),
        "unmonitored" => return assert!(!e.monitored, "{e:?}"),
        _ => {}
    }
    let got = match e.status {
        AcquisitionStatus::Missing => "Missing",
        AcquisitionStatus::Searching { .. } => "Searching",
        AcquisitionStatus::Snatched { .. } => "Snatched",
        AcquisitionStatus::Downloading { .. } => "Downloading",
        AcquisitionStatus::Imported { .. } => "Imported",
        AcquisitionStatus::Cutoff => "Cutoff",
        AcquisitionStatus::Failed { .. } => "Failed",
    };
    assert_eq!(got, kind, "{:?}", e.status);
}

#[then(expr = "episode S{int}E{int} of {string} holds quality {string} and format score {int}")]
async fn holds(w: &mut World, s: u16, n: u16, title: String, q: String, score: i32) {
    let e = w.reload_episode(&title, s, n).await;
    match e.status {
        AcquisitionStatus::Imported {
            quality, score: sc, ..
        } => {
            assert_eq!(World::quality_name(quality), q);
            assert_eq!(sc, score);
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}

#[then(expr = "episode S{int}E{int} of {string} holds a quality other than {string}")]
async fn holds_not(w: &mut World, s: u16, n: u16, title: String, q: String) {
    let e = w.reload_episode(&title, s, n).await;
    match e.status {
        AcquisitionStatus::Imported { quality, .. } => assert_ne!(
            World::quality_name(quality),
            q,
            "restored import fabricated the lowest tier {q:?} (SKADI-T-0399)"
        ),
        other => panic!("expected Imported, got {other:?}"),
    }
}

#[given("the operator opts in to searching undated episodes")]
fn opt_in_undated(w: &mut World) {
    // Sonarr's "search for undated episodes" (SKADI-T-0446), off by default.
    w.search_undated_episodes = true;
}
