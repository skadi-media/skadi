//! C24 television catalog CRUD + `LibraryItem`/`Acquirable` + unified library.
use cucumber::{then, when};

use skadi_api::LibraryProvider;
use skadi_core::{Acquirable, LibraryItem};
use skadi_tv::{Season, SeriesFilter, TelevisionLibrary, TvRepo};

use crate::bdd_support::World;

fn filter(word: &str) -> Option<bool> {
    match word {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[when(expr = "the library is listed with monitored filter {word}")]
async fn list(w: &mut World, f: String) {
    w.listed = w
        .store()
        .list_series(SeriesFilter {
            monitored: filter(&f),
            limit: None,
            offset: None,
        })
        .await
        .expect("list_series");
}

#[then(expr = "the listing contains {string} with {int} episode(s)")]
async fn contains(w: &mut World, title: String, n: usize) {
    let s = w
        .listed
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("{title} not listed"));
    assert_eq!(s.episodes.len(), n);
    assert_eq!(s.acquirables().count(), n);
    assert_eq!(s.kind(), skadi_core::MediaKind::Series);
}

#[then(expr = "the listing does not contain {string}")]
async fn not_contains(w: &mut World, title: String) {
    assert!(!w.listed.iter().any(|s| s.title == title));
}

#[when(expr = "the series {string} is looked up by its TVDB id")]
async fn by_tvdb(w: &mut World, title: String) {
    let s = w.series.get(&title).expect("series").clone();
    w.last_series = w
        .store()
        .get_series_by_tvdb(s.external_ids.tvdb.clone().unwrap())
        .await
        .expect("get_series_by_tvdb");
}

#[when(expr = "the series {string} is loaded by id")]
async fn by_id(w: &mut World, title: String) {
    let id = w.series_id(&title);
    w.last_series = w.store().get_series(id).await.expect("get_series");
}

#[then(expr = "the lookup returns {string} with {int} episode(s) and {int} season(s)")]
async fn lookup_returns(w: &mut World, title: String, eps: usize, seasons: usize) {
    let s = w.last_series.as_ref().expect("a series");
    assert_eq!(s.title, title);
    assert_eq!(s.episodes.len(), eps);
    assert_eq!(s.seasons.len(), seasons);
}

#[then("the lookup returns nothing")]
async fn lookup_none(w: &mut World) {
    assert!(w.last_series.is_none());
}

#[when(expr = "season {int} of {string} is recorded")]
async fn add_season(w: &mut World, n: u16, title: String) {
    let id = w.series_id(&title);
    w.store()
        .upsert_season(&Season::new(id, n))
        .await
        .expect("upsert_season");
}

#[when(expr = "the series {string} is deleted")]
async fn delete(w: &mut World, title: String) {
    let id = w.series_id(&title);
    w.store().delete_series(id).await.expect("delete_series");
}

#[then(expr = "no episode or season rows remain for {string}")]
async fn no_children(w: &mut World, title: String) {
    let id = w.series_id(&title);
    assert!(w.store().list_episodes(id).await.unwrap().is_empty());
    assert!(w.store().list_seasons(id).await.unwrap().is_empty());
}

#[when(expr = "a series without a TVDB id is saved")]
async fn save_without_tvdb(w: &mut World) {
    let mut s = World::new_series("No Id", 2000, true);
    s.external_ids.tvdb = None;
    w.error = w
        .store()
        .upsert_series(&s)
        .await
        .err()
        .map(|e| format!("{e:?}"));
}

#[then("the save is rejected as a validation error")]
async fn rejected(w: &mut World) {
    let e = w.error.as_deref().expect("an error");
    assert!(e.starts_with("Validation"), "{e}");
}

#[when(expr = "episode S{int}E{int} of {string} is unmonitored")]
async fn unmonitor_ep(w: &mut World, s: u16, n: u16, title: String) {
    let e = w.episode(&title, s, n);
    w.store()
        .set_episode_monitored(e.id, false)
        .await
        .expect("set_episode_monitored");
}

#[then(expr = "episode S{int}E{int} of {string} is {word} by the acquirable contract")]
async fn contract(w: &mut World, s: u16, n: u16, title: String, want: String) {
    let e = w.reload_episode(&title, s, n).await;
    assert_eq!(e.parent(), &w.series_id(&title));
    match want.as_str() {
        "wanted" => assert!(e.wanted()),
        "unwanted" => assert!(!e.wanted()),
        other => panic!("{other}"),
    }
}

#[when(expr = "season {int} of {string} is unmonitored at the repo")]
async fn unmonitor_season(w: &mut World, n: u16, title: String) {
    let id = w.series_id(&title);
    let seasons = w.store().list_seasons(id).await.unwrap();
    let season = seasons.iter().find(|s| s.number == n).expect("season row");
    w.store()
        .set_season_monitored(season.id, false)
        .await
        .expect("set_season_monitored");
}

#[then(expr = "season {int} of {string} is unmonitored but its episodes still are monitored")]
async fn season_no_cascade(w: &mut World, n: u16, title: String) {
    let id = w.series_id(&title);
    let s = w.store().get_series(id).await.unwrap().unwrap();
    assert!(!s.seasons.iter().find(|x| x.number == n).unwrap().monitored);
    assert!(
        s.episodes
            .iter()
            .filter(|e| e.season == n)
            .all(|e| e.monitored)
    );
}

#[when(expr = "the unified library lists series with monitored filter {word}")]
async fn unified(w: &mut World, f: String) {
    let lib = TelevisionLibrary::new(w.store());
    assert_eq!(lib.domain(), "television");
    assert_eq!(lib.kind(), skadi_core::MediaKind::Series);
    w.library_items = lib.items(filter(&f)).await.expect("items");
}

#[then(expr = "the library items include {string} with {int} edition row(s)")]
async fn library_includes(w: &mut World, title: String, n: usize) {
    let item = w
        .library_items
        .iter()
        .find(|i| i.title == title)
        .unwrap_or_else(|| panic!("{title} not in {:?}", w.library_items));
    assert_eq!(item.editions.len(), n, "{:?}", item.editions);
}

#[then(expr = "the library items do not include {string}")]
async fn library_excludes(w: &mut World, title: String) {
    assert!(!w.library_items.iter().any(|i| i.title == title));
}

/// C28 is movies-only (`edition_kinds` lives in `skadi-movies`); television has
/// no season/episode-type registry — `SeriesType` is a fixed enum on the series.
#[then("the television domain has an acquirable-unit kind registry")]
async fn no_kind_registry(_w: &mut World) {
    panic!(
        "skadi-tv has no acquirable-unit kind registry (no `edition_kinds`-style table or repo methods); \
         episode types are not modelled beyond `SeriesType {{Standard, Anime, Daily}}`"
    );
}
