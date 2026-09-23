//! C10: the search planner side that lives in the hunter — `SearchSpec` →
//! indexer query shape, RSS feed matching, the sweep's seed union/dedupe and
//! not-found backoff (via the `search`/`decide` step bodies).

use cucumber::{given, then, when};

use skadi_core::{AcquisitionStatus, ExternalIds, FailureReason, MediaKind, ProfileId};
use skadi_hunter::{AcquireState, SearchSpec, StatusSink as _, TvScope, load_state, tracker};
use skadi_importer::AcquirableRef;
use skadi_indexers::{Category, SearchQuery as _, normalize_title};

use crate::bdd_support::World;

#[given(expr = "a search spec for episode {string} S{int}E{int}")]
fn spec_episode(w: &mut World, title: String, season: u16, episode: u16) {
    w.state = Some(AcquireState::new(
        AcquirableRef("ep".into()),
        SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Series,
            titles: vec![title],
            year: None,
            external_ids: ExternalIds::default(),
            categories: vec![Category(5000)],
            tv: Some(TvScope {
                season,
                episode: Some(episode),
                absolute: None,
                air_date: None,
            }),
            series: None,
            tags: None,
        },
        ProfileId::new(),
    ));
}

#[given(expr = "a search spec for season pack {string} S{int}")]
fn spec_season(w: &mut World, title: String, season: u16) {
    spec_episode(w, title, season, 0);
    w.state_mut().request.tv.as_mut().unwrap().episode = None;
}

#[given(expr = "a search spec for movie {string} \\({int}\\) with tmdb id {int}")]
fn spec_movie(w: &mut World, title: String, year: u16, tmdb: u64) {
    let ids = ExternalIds {
        tmdb: Some(skadi_core::TmdbId(tmdb)),
        ..Default::default()
    };
    w.state = Some(AcquireState::new(
        AcquirableRef("mv".into()),
        SearchSpec {
            trigger: Default::default(),
            kind: MediaKind::Movie,
            titles: vec![title],
            year: Some(year),
            external_ids: ids,
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        ProfileId::new(),
    ));
}

#[then(expr = "the indexer query carries season {int} and episode {int}")]
fn query_season_ep(w: &mut World, season: u16, ep: u16) {
    let q = w.state.as_ref().unwrap().request.query();
    let params = q.extra_params();
    assert!(
        params.contains(&("season", season.to_string())),
        "{params:?}"
    );
    assert!(params.contains(&("ep", ep.to_string())), "{params:?}");
}

#[then(expr = "the indexer query carries season {int} and no episode")]
fn query_season_only(w: &mut World, season: u16) {
    let q = w.state.as_ref().unwrap().request.query();
    let params = q.extra_params();
    assert!(
        params.contains(&("season", season.to_string())),
        "{params:?}"
    );
    assert!(!params.iter().any(|(k, _)| *k == "ep"), "{params:?}");
}

#[then(expr = "the indexer query is scoped to category {int}")]
fn query_category(w: &mut World, cat: u32) {
    let q = w.state.as_ref().unwrap().request.query();
    assert_eq!(q.categories(), &[Category(cat)]);
}

#[then(expr = "the indexer query carries year {int} and tmdb id {int}")]
fn query_ids(w: &mut World, year: u16, tmdb: u64) {
    let q = w.state.as_ref().unwrap().request.query();
    assert_eq!(q.year(), Some(year));
    assert_eq!(q.external_ids().tmdb.as_ref().map(|t| t.0), Some(tmdb));
    assert_eq!(q.mode(), skadi_indexers::SearchMode::Auto);
}

#[then(expr = "the search spec round-trips through the workflow context")]
fn spec_roundtrip(w: &mut World) {
    let st = w.state.as_ref().unwrap();
    let ctx = st.into_context().unwrap();
    assert_eq!(load_state(&ctx).unwrap(), *st);
}

// --- RSS title matching ----------------------------------------------------

#[then(expr = "the RSS feed entry {string} matches the wanted title {string}")]
fn rss_matches(_w: &mut World, feed_title: String, wanted: String) {
    // Exercise the same keying the fast pass uses, not a re-implementation of it
    // (SKADI-T-0431): a feed entry offers several keys, and the wanted title only
    // has to match one.
    let parsed = skadi_quality::parse(&feed_title);
    let keys = skadi_hunter::feed_title_keys(&feed_title, parsed.title.as_deref());
    assert!(
        keys.contains(&normalize_title(&wanted)),
        "keys {keys:?} do not contain {wanted:?}"
    );
}

#[then(expr = "the RSS feed entry {string} does not match the wanted title {string}")]
fn rss_no_match(_w: &mut World, feed_title: String, wanted: String) {
    let parsed = skadi_quality::parse(&feed_title);
    let keys = skadi_hunter::feed_title_keys(&feed_title, parsed.title.as_deref());
    assert!(
        !keys.contains(&normalize_title(&wanted)),
        "keys {keys:?} unexpectedly contain {wanted:?}"
    );
}

// --- not-found backoff through the step bodies (needs the registry) --------

#[given(expr = "a first-acquisition run {string} for {string} searching {string}")]
fn first_run(w: &mut World, run: String, acquirable: String, title: String) {
    let kind = w.kind();
    let st = AcquireState::new(
        AcquirableRef(acquirable.clone()),
        SearchSpec {
            trigger: Default::default(),
            kind,
            titles: vec![title],
            year: Some(2020),
            external_ids: ExternalIds::default(),
            categories: vec![Category(2000)],
            tv: None,
            series: None,
            tags: None,
        },
        ProfileId::new(),
    );
    w.tracked_refs.push(acquirable);
    w.states.insert(run.clone(), st.clone());
    w.contexts.insert(run, st.into_context().unwrap());
}

#[given(expr = "{string} already failed not-found {int} time(s)")]
async fn prior_failures(w: &mut World, acquirable: String, attempts: u32) {
    w.status
        .as_ref()
        .unwrap()
        .set_status(
            &AcquirableRef(acquirable),
            AcquisitionStatus::Failed {
                reason: FailureReason::NoSuitableRelease,
                retry_at: Some(chrono::Utc::now()),
                attempts,
            },
        )
        .await
        .unwrap();
}

#[when(expr = "run {string} searches and decides")]
async fn search_decide(w: &mut World, run: String) {
    let ctx = w.contexts.get_mut(&run).expect("run");
    let res = match skadi_hunter::steps::search(ctx).await {
        Ok(()) => skadi_hunter::steps::decide(ctx).await,
        Err(e) => Err(e),
    };
    w.outcome = Some(res.map_err(|e| format!("{e:?}")));
    if let Ok(st) = load_state(ctx) {
        w.states.insert(run, st);
    }
}

#[then(
    expr = "the status of {string} is Failed not-found with {int} attempt(s) and a re-check in about {int} hours"
)]
async fn not_found_status(w: &mut World, acquirable: String, attempts: u32, hours: i64) {
    let s = w
        .status
        .as_ref()
        .unwrap()
        .get_status(&AcquirableRef(acquirable))
        .await
        .unwrap();
    match s {
        Some(AcquisitionStatus::Failed {
            reason: FailureReason::NoSuitableRelease,
            retry_at: Some(at),
            attempts: a,
        }) => {
            assert_eq!(a, attempts);
            let h = (at - chrono::Utc::now()).num_minutes() as f64 / 60.0;
            assert!(
                (h - hours as f64).abs() < 0.1,
                "re-check in {h:.2}h, expected {hours}h"
            );
        }
        other => panic!("expected Failed(NoSuitableRelease), got {other:?}"),
    }
}

#[then(expr = "{string} is not left in flight")]
fn not_in_flight(_w: &mut World, acquirable: String) {
    // The in-process search/decide never registered it (start_acquire owns the
    // tracker entry, and these scenarios drive the step bodies directly).
    assert!(!tracker().is_active(&acquirable));
}
