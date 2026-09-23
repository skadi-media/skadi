//! C24 catalog CRUD + `LibraryItem`/`Acquirable` + unified-library steps.
use cucumber::{then, when};

use skadi_api::LibraryProvider;
use skadi_core::{Acquirable, AppError, EditionKindId, LibraryItem};
use skadi_movies::{MovieEdition, MovieFilter, MoviesLibrary, MoviesRepo};

use crate::bdd_support::World;

#[when(expr = "the library is listed with monitored filter {word}")]
async fn list(w: &mut World, filter: String) {
    let monitored = match filter.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    w.listed = w
        .store()
        .list_movies(MovieFilter {
            monitored,
            limit: None,
            offset: None,
        })
        .await
        .expect("list_movies");
}

#[then(expr = "the listing contains {string}")]
async fn contains(w: &mut World, title: String) {
    assert!(
        w.listed.iter().any(|m| m.title == title),
        "listing: {:?}",
        w.listed.iter().map(|m| &m.title).collect::<Vec<_>>()
    );
}

#[then(expr = "the listing does not contain {string}")]
async fn not_contains(w: &mut World, title: String) {
    assert!(!w.listed.iter().any(|m| m.title == title));
}

#[then(expr = "every listed movie carries its editions")]
async fn hydrated(w: &mut World) {
    assert!(!w.listed.is_empty());
    for m in &w.listed {
        assert!(!m.editions.is_empty(), "{} has no editions loaded", m.title);
        assert!(m.acquirables().count() >= 1);
    }
}

#[when(expr = "the movie {string} is looked up by its TMDB id")]
async fn by_tmdb(w: &mut World, title: String) {
    let m = w.movies.get(&title).expect("movie").clone();
    w.last_movie = w
        .store()
        .get_movie_by_tmdb(m.external_ids.tmdb.clone().unwrap())
        .await
        .expect("get_movie_by_tmdb");
}

#[when(expr = "the movie {string} is loaded by id")]
async fn by_id(w: &mut World, title: String) {
    let id = w.movie_id(&title);
    w.last_movie = w.store().get_movie(id).await.expect("get_movie");
}

#[then(expr = "the lookup returns {string} with {int} edition(s)")]
async fn lookup_returns(w: &mut World, title: String, n: usize) {
    let m = w.last_movie.as_ref().expect("a movie was found");
    assert_eq!(m.title, title);
    assert_eq!(m.editions.len(), n);
    assert_eq!(m.kind(), skadi_core::MediaKind::Movie);
}

#[then("the lookup returns nothing")]
async fn lookup_none(w: &mut World) {
    assert!(w.last_movie.is_none());
}

#[when(expr = "the movie {string} is deleted")]
async fn delete(w: &mut World, title: String) {
    let id = w.movie_id(&title);
    w.store().delete_movie(id).await.expect("delete_movie");
}

#[then(expr = "no edition rows remain for {string}")]
async fn no_editions(w: &mut World, title: String) {
    let id = w.movie_id(&title);
    let left = w.store().list_editions(id).await.expect("list_editions");
    assert!(
        left.is_empty(),
        "{} orphaned edition row(s) survive the movie delete",
        left.len()
    );
}

#[when("a movie without a TMDB id is saved")]
async fn save_without_tmdb(w: &mut World) {
    let mut m = World::new_movie("No Id", 2000, true);
    m.external_ids.tmdb = None;
    w.error = w
        .store()
        .upsert_movie(&m)
        .await
        .err()
        .map(|e| e.to_string());
    w.notes.push(format!("{:?}", w.error));
}

#[then("the save is rejected as a validation error")]
async fn rejected_validation(w: &mut World) {
    let msg = w.error.as_deref().expect("an error was returned");
    let err = AppError::Validation(String::new());
    assert!(
        msg.to_lowercase().contains("tmdb") || msg.contains("Validation"),
        "expected a validation error like {err:?}, got {msg}"
    );
}

#[when(expr = "a second Theatrical edition is added to {string}")]
async fn duplicate_edition(w: &mut World, title: String) {
    let id = w.movie_id(&title);
    let e = MovieEdition::missing(id, EditionKindId::from(skadi_movies::THEATRICAL_KIND_ID));
    w.error = w
        .store()
        .upsert_edition(&e)
        .await
        .err()
        .map(|e| e.to_string());
}

#[then("the edition write is rejected")]
async fn edition_rejected(w: &mut World) {
    assert!(
        w.error.is_some(),
        "a duplicate (movie, kind) edition was accepted"
    );
}

#[when(expr = "an Extended edition is added to {string}")]
async fn add_extended(w: &mut World, title: String) {
    let id = w.movie_id(&title);
    let e = MovieEdition::missing(
        id,
        EditionKindId::from(uuid::uuid!("00000000-0000-0000-0000-000000000002")),
    );
    w.store().upsert_edition(&e).await.expect("upsert extended");
}

#[then(expr = "the Theatrical edition of {string} is wanted by the acquirable contract")]
async fn is_wanted(w: &mut World, title: String) {
    let e = w.reload_edition(&title).await;
    assert!(e.wanted());
    assert_eq!(e.parent(), &w.movie_id(&title));
}

#[then(expr = "the Theatrical edition of {string} is not wanted by the acquirable contract")]
async fn not_wanted(w: &mut World, title: String) {
    let e = w.reload_edition(&title).await;
    assert!(!e.wanted());
}

#[when(expr = "the unified library lists movies with monitored filter {word}")]
async fn unified(w: &mut World, filter: String) {
    let monitored = match filter.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    let lib = MoviesLibrary::new(w.store());
    assert_eq!(lib.domain(), "movies");
    assert_eq!(lib.kind(), skadi_core::MediaKind::Movie);
    w.library_items = lib.items(monitored).await.expect("items");
}

#[then(
    expr = "the library items include {string} with {int} edition row(s) keyed by the Theatrical kind id"
)]
async fn library_includes(w: &mut World, title: String, n: usize) {
    let item = w
        .library_items
        .iter()
        .find(|i| i.title == title)
        .unwrap_or_else(|| panic!("{title} not in {:?}", w.library_items));
    assert_eq!(item.editions.len(), n);
    assert!(
        item.editions
            .iter()
            .any(|e| e.kind == skadi_movies::THEATRICAL_KIND_ID.to_string()),
        "{:?}",
        item.editions
    );
}

/// `movie_to_dto` (http.rs L1311–1331) emits the raw `EditionKindId` and
/// `QualityId` strings; a UI has to join the registry and the definitions
/// itself (the C23 spec's "no kind vocabulary" gap).
#[then(expr = "the library item {string} names its edition kind {string} and quality {string}")]
async fn library_names(w: &mut World, title: String, kind: String, quality: String) {
    let item = w
        .library_items
        .iter()
        .find(|i| i.title == title)
        .expect("item");
    let e = &item.editions[0];
    // SKADI-T-0454: the ids stay (clients keyed on them), and the DTO now carries
    // the names beside them so nothing has to resolve a UUID to show a row.
    assert_eq!(
        e.kind_name.as_deref(),
        Some(kind.as_str()),
        "edition kind name; raw id is {}",
        e.kind
    );
    assert_eq!(
        e.quality_name.as_deref(),
        Some(quality.as_str()),
        "quality name; raw id is {:?}",
        e.quality
    );
}

#[then(expr = "the library items do not include {string}")]
async fn library_excludes(w: &mut World, title: String) {
    assert!(!w.library_items.iter().any(|i| i.title == title));
}

#[then(expr = "the library item {string} reports status {string} and quality {string}")]
async fn library_status(w: &mut World, title: String, status: String, quality: String) {
    let item = w
        .library_items
        .iter()
        .find(|i| i.title == title)
        .expect("item");
    let e = &item.editions[0];
    assert_eq!(e.status_kind, status);
    assert_eq!(
        e.quality.as_deref(),
        Some(World::quality_named(&quality).to_string().as_str()),
        "quality is exposed as the raw QualityId"
    );
}
