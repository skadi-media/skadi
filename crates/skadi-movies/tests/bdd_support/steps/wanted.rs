//! C24 wanted / upgrade / reconcile sweep steps (`MovieWantedQuery`).
use cucumber::{then, when};

use skadi_core::AcquisitionStatus;
use skadi_hunter::WantedQuery;
use skadi_movies::{MovieWantedQuery, WantedScoring};

use crate::bdd_support::World;

fn query(w: &World) -> MovieWantedQuery {
    MovieWantedQuery::new(
        w.repo(),
        WantedScoring {
            profile: w.profile(),
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
    assert_eq!(w.seeds.len(), n, "seeds: {:?}", w.seeds);
}

#[then("no seed is emitted")]
async fn no_seed(w: &mut World) {
    assert!(w.seeds.is_empty(), "unexpected seeds: {:?}", w.seeds);
}

#[then(expr = "the seed for {string} carries no current quality")]
async fn no_current(w: &mut World, title: String) {
    let s = w.seed_for(&title);
    assert_eq!(s.current_quality, None);
    assert_eq!(s.current_format_score, None);
}

#[then(expr = "the seed for {string} carries current quality {string} and format score {int}")]
async fn current(w: &mut World, title: String, quality: String, score: i32) {
    let s = w.seed_for(&title);
    assert_eq!(s.current_quality, Some(World::quality_named(&quality)));
    assert_eq!(s.current_format_score, Some(score));
}

#[then(expr = "the seed for {string} searches category {int} with year {int}")]
async fn category_year(w: &mut World, title: String, cat: u32, year: u16) {
    let s = w.seed_for(&title);
    assert_eq!(s.request.kind, skadi_core::MediaKind::Movie);
    assert_eq!(s.request.categories, vec![skadi_indexers::Category(cat)]);
    assert_eq!(s.request.year, Some(year));
    assert!(s.request.tv.is_none(), "movies carry no TV scope");
}

#[then(expr = "the seed for {string} lists titles {string}")]
async fn titles(w: &mut World, title: String, list: String) {
    let want: Vec<&str> = list.split(',').map(str::trim).collect();
    let s = w.seed_for(&title);
    assert_eq!(s.request.titles, want);
}

#[then(expr = "the seed for {string} carries the movie's TMDB id")]
async fn tmdb(w: &mut World, title: String) {
    let m = w.movies.get(&title).expect("movie").clone();
    let s = w.seed_for(&title);
    assert_eq!(s.request.external_ids.tmdb, m.external_ids.tmdb);
    assert_eq!(s.profile, m.profile);
}

#[then(expr = "{int} edition(s) is/are recovered")]
async fn recovered(w: &mut World, n: usize) {
    assert_eq!(w.recovered, n);
}

#[then(expr = "the Theatrical edition of {string} is {word}")]
async fn status_is(w: &mut World, title: String, kind: String) {
    let e = w.reload_edition(&title).await;
    let got = match e.status {
        AcquisitionStatus::Missing => "Missing",
        AcquisitionStatus::Searching { .. } => "Searching",
        AcquisitionStatus::Snatched { .. } => "Snatched",
        AcquisitionStatus::Downloading { .. } => "Downloading",
        AcquisitionStatus::Imported { .. } => "Imported",
        AcquisitionStatus::Cutoff => "Cutoff",
        AcquisitionStatus::Failed { .. } => "Failed",
    };
    assert_eq!(got, kind, "status of {title}: {:?}", e.status);
}

#[then(expr = "the Theatrical edition of {string} holds quality {string} and format score {int}")]
async fn holds(w: &mut World, title: String, quality: String, score: i32) {
    let e = w.reload_edition(&title).await;
    match e.status {
        AcquisitionStatus::Imported {
            quality: q,
            score: s,
            ..
        } => {
            assert_eq!(World::quality_name(q), quality);
            assert_eq!(s, score);
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}

#[then(expr = "the Theatrical edition of {string} holds a quality other than {string}")]
async fn holds_not(w: &mut World, title: String, quality: String) {
    let e = w.reload_edition(&title).await;
    match e.status {
        AcquisitionStatus::Imported { quality: q, .. } => {
            assert_ne!(
                World::quality_name(q),
                quality,
                "restored import fabricated the lowest tier {quality:?} (SKADI-T-0399)"
            );
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}
