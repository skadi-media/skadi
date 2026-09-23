//! C24 wanted / upgrade / reconcile sweep steps (`AudiobookWantedQuery`).
use cucumber::{then, when};

use skadi_audiobooks::{AudiobookWantedQuery, WantedScoring};
use skadi_core::AcquisitionStatus;
use skadi_hunter::WantedQuery;

use crate::bdd_support::World;

fn query(w: &World) -> AudiobookWantedQuery {
    AudiobookWantedQuery::new(
        w.repo(),
        WantedScoring {
            profile: w.profile(),
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

fn seed_asins(w: &World) -> Vec<String> {
    w.seeds
        .iter()
        .filter_map(|s| s.request.external_ids.asin.as_ref().map(|a| a.0.clone()))
        .collect()
}

#[then(expr = "{int} seed(s) is/are emitted")]
async fn n_seeds(w: &mut World, n: usize) {
    assert_eq!(w.seeds.len(), n, "seeds for ASINs {:?}", seed_asins(w));
}

#[then("no seed is emitted")]
async fn no_seed(w: &mut World) {
    assert!(
        w.seeds.is_empty(),
        "unexpected seeds for {:?}",
        seed_asins(w)
    );
}

#[then(expr = "the seed for {word} carries no current quality")]
async fn no_current(w: &mut World, asin: String) {
    let s = w.seed_for(&asin);
    assert_eq!(s.current_quality, None);
    assert_eq!(s.current_format_score, None);
}

#[then(expr = "the seed for {word} carries current quality {string} and format score {int}")]
async fn current(w: &mut World, asin: String, q: String, score: i32) {
    let s = w.seed_for(&asin);
    assert_eq!(s.current_quality, Some(World::quality_named(&q)));
    assert_eq!(s.current_format_score, Some(score));
}

#[then(expr = "the seed for {word} searches the audiobook category with year {int}")]
async fn category(w: &mut World, asin: String, year: u16) {
    let s = w.seed_for(&asin);
    assert_eq!(s.request.kind, skadi_core::MediaKind::Audiobook);
    assert_eq!(s.request.categories, vec![skadi_indexers::Category(3030)]);
    assert_eq!(s.request.year, Some(year));
    assert_eq!(
        s.request.external_ids.asin.as_ref().map(|a| a.0.as_str()),
        Some(asin.as_str())
    );
}

#[then(expr = "the seed for {word} lists titles {string}")]
async fn titles(w: &mut World, asin: String, list: String) {
    let want: Vec<&str> = list.split(',').map(str::trim).collect();
    assert_eq!(w.seed_for(&asin).request.titles, want);
}

#[then(expr = "the seed for {word} carries series {string}")]
async fn series_hint(w: &mut World, asin: String, series: String) {
    assert_eq!(
        w.seed_for(&asin).request.series.as_deref(),
        Some(series.as_str())
    );
}

#[then(expr = "{int} file(s) is/are recovered")]
async fn recovered(w: &mut World, n: usize) {
    assert_eq!(w.recovered, n);
}

#[then(expr = "the file of {word} is {word}")]
async fn status_is(w: &mut World, asin: String, kind: String) {
    let f = w.reload_file(&asin).await;
    let got = match f.status {
        AcquisitionStatus::Missing => "Missing",
        AcquisitionStatus::Searching { .. } => "Searching",
        AcquisitionStatus::Snatched { .. } => "Snatched",
        AcquisitionStatus::Downloading { .. } => "Downloading",
        AcquisitionStatus::Imported { .. } => "Imported",
        AcquisitionStatus::Cutoff => "Cutoff",
        AcquisitionStatus::Failed { .. } => "Failed",
    };
    assert_eq!(got, kind, "{:?}", f.status);
}

#[then(expr = "the file of {word} holds quality {string} and format score {int}")]
async fn holds(w: &mut World, asin: String, q: String, score: i32) {
    let f = w.reload_file(&asin).await;
    match f.status {
        AcquisitionStatus::Imported {
            quality, score: s, ..
        } => {
            assert_eq!(quality, World::quality_named(&q));
            assert_eq!(s, score);
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}
