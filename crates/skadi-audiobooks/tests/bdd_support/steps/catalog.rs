//! C24 audiobook catalog CRUD: authors, series, books, files, works, watchers.
use cucumber::{given, then, when};

use skadi_audiobooks::{
    AudiobooksRepo, Author, AuthorFilter, BookFilter, WatchScope, WatchersRepo, Work, WorksRepo,
};
use skadi_core::{Acquirable, AsinId, LibraryItem};

use crate::bdd_support::World;

#[given(expr = "an author {string} with ASIN {word}")]
async fn author(w: &mut World, name: String, asin: String) {
    let mut a = Author::new(&name);
    a.asin = Some(AsinId(asin));
    w.store().upsert_author(&a).await.expect("upsert author");
    w.authors.insert(name, a);
}

#[given(expr = "the book {word} is attributed to author {string}")]
async fn attributed(w: &mut World, asin: String, name: String) {
    let a = w.authors.get(&name).expect("author").clone();
    let mut b = w.books.get(&asin).expect("book").clone();
    b.author_id = Some(a.id);
    w.store().upsert_book(&b).await.expect("upsert");
    w.books.insert(asin, b);
}

#[then(expr = "the author {string} is found by ASIN {word} and by id")]
async fn author_lookup(w: &mut World, name: String, asin: String) {
    let by_asin = w
        .store()
        .get_author_by_asin(&AsinId(asin))
        .await
        .unwrap()
        .expect("by asin");
    assert_eq!(by_asin.name, name);
    let by_id = w.store().get_author(by_asin.id).await.unwrap().unwrap();
    assert_eq!(by_id, by_asin);
}

#[then(expr = "listing monitored authors yields {int}")]
async fn authors_listed(w: &mut World, n: usize) {
    let list = w
        .store()
        .list_authors(AuthorFilter {
            monitored: Some(true),
        })
        .await
        .unwrap();
    assert_eq!(list.len(), n, "{list:?}");
}

#[then(expr = "the books of author {string} are {string}")]
async fn books_of(w: &mut World, name: String, asins: String) {
    let a = w.authors.get(&name).expect("author").clone();
    let mut got: Vec<String> = w
        .store()
        .list_books_by_author(a.id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|b| b.external_ids.asin.map(|a| a.0))
        .collect();
    got.sort();
    let mut want: Vec<String> = asins.split(',').map(|s| s.trim().to_string()).collect();
    want.sort();
    assert_eq!(got, want);
}

#[when(expr = "the library is listed with monitored filter {word}")]
async fn list(w: &mut World, f: String) {
    let monitored = match f.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    w.listed = w
        .store()
        .list_books(BookFilter {
            monitored,
            limit: None,
            offset: None,
        })
        .await
        .expect("list_books");
}

#[then(expr = "the listing contains {word} with {int} file(s)")]
async fn contains(w: &mut World, asin: String, n: usize) {
    let b = w
        .listed
        .iter()
        .find(|b| b.external_ids.asin.as_ref().is_some_and(|a| a.0 == asin))
        .unwrap_or_else(|| panic!("{asin} not listed"));
    assert_eq!(b.files.len(), n);
    assert_eq!(b.acquirables().count(), n);
    assert_eq!(b.kind(), skadi_core::MediaKind::Audiobook);
}

#[then(expr = "the listing does not contain {word}")]
async fn not_contains(w: &mut World, asin: String) {
    assert!(
        !w.listed
            .iter()
            .any(|b| b.external_ids.asin.as_ref().is_some_and(|a| a.0 == asin))
    );
}

#[when(expr = "the book {word} is looked up by ASIN")]
async fn by_asin(w: &mut World, asin: String) {
    w.last_book = w
        .store()
        .get_book_by_asin(&AsinId(asin))
        .await
        .expect("get_book_by_asin");
}

#[then(expr = "the lookup returns {string} in series {string} at position {string}")]
async fn lookup_series(w: &mut World, title: String, series: String, pos: String) {
    let b = w.last_book.as_ref().expect("book");
    assert_eq!(b.title, title);
    let link = b.series.as_ref().expect("series link");
    assert_eq!(link.name, series);
    assert_eq!(link.position.as_deref(), Some(pos.as_str()));
}

#[then("the lookup returns nothing")]
async fn lookup_none(w: &mut World) {
    assert!(w.last_book.is_none());
}

#[when(expr = "the book {word} is deleted")]
async fn delete(w: &mut World, asin: String) {
    let id = w.book_id(&asin);
    w.store().delete_book(id).await.expect("delete_book");
}

#[then(expr = "no file rows remain for {word}")]
async fn no_files(w: &mut World, asin: String) {
    let id = w.book_id(&asin);
    let left = w.store().list_book_files(id).await.unwrap();
    assert!(left.is_empty(), "{} orphaned file row(s)", left.len());
}

#[when("a second file is added to the same book")]
async fn second_file(w: &mut World) {
    let (asin, _) = w.files.iter().next().expect("a book");
    let id = w.book_id(&asin.clone());
    let f = skadi_audiobooks::BookFile::missing(id);
    w.error = w
        .store()
        .upsert_book_file(&f)
        .await
        .err()
        .map(|e| e.to_string());
}

#[then("the file write is rejected")]
async fn file_rejected(w: &mut World) {
    assert!(w.error.is_some(), "a second file per book was accepted");
}

#[then(expr = "the file of {word} is {word} by the acquirable contract")]
async fn contract(w: &mut World, asin: String, want: String) {
    let f = w.reload_file(&asin).await;
    assert_eq!(f.parent(), &w.book_id(&asin));
    match want.as_str() {
        "wanted" => assert!(f.wanted()),
        "unwanted" => assert!(!f.wanted()),
        other => panic!("{other}"),
    }
}

#[given(expr = "a known work {word} {string} by author ASIN {word} in series ASIN {word}")]
async fn known_work(w: &mut World, asin: String, title: String, author: String, series: String) {
    let mut work = Work::new(AsinId(asin), title);
    work.author_asin = Some(AsinId(author));
    work.series_asin = Some(AsinId(series));
    work.language = Some("English".into());
    w.store().upsert_work(&work).await.expect("upsert_work");
    w.works.push(work);
}

#[then(expr = "the works of author ASIN {word} are {string}")]
async fn works_of_author(w: &mut World, author: String, list: String) {
    let mut got: Vec<String> = w
        .store()
        .list_works_by_author(&AsinId(author))
        .await
        .unwrap()
        .into_iter()
        .map(|x| x.asin.0)
        .collect();
    got.sort();
    let mut want: Vec<String> = list.split(',').map(|s| s.trim().to_string()).collect();
    want.sort();
    assert_eq!(got, want);
}

#[when(expr = "a {word} watcher is set on {word}")]
async fn set_watcher(w: &mut World, scope: String, key: String) {
    w.store()
        .set_watcher(WatchScope::parse(&scope).expect("scope"), &key)
        .await
        .expect("set_watcher");
}

#[when(expr = "the {word} watcher on {word} is cleared")]
async fn clear_watcher(w: &mut World, scope: String, key: String) {
    w.store()
        .clear_watcher(WatchScope::parse(&scope).expect("scope"), &key)
        .await
        .expect("clear_watcher");
}

#[then(expr = "{word} is {word} watched at {word} scope")]
async fn is_watched(w: &mut World, key: String, flag: String, scope: String) {
    let got = w
        .store()
        .is_watched(WatchScope::parse(&scope).expect("scope"), &key)
        .await
        .unwrap();
    assert_eq!(got, flag == "currently", "{key} watched={got}");
}

#[then(expr = "{int} watcher(s) is/are listed")]
async fn watchers(w: &mut World, n: usize) {
    w.watchers = w.store().list_watchers().await.unwrap();
    assert_eq!(w.watchers.len(), n, "{:?}", w.watchers);
}

/// The audiobooks crate ships no `LibraryProvider` (movies and television do),
/// so `GET /library` never includes books.
#[then("the audiobook domain contributes items to the unified library")]
async fn no_library_provider(_w: &mut World) {
    panic!(
        "skadi-audiobooks implements HttpModule but no skadi_api::LibraryProvider \
         (grep 'LibraryProvider for' finds only skadi-movies and skadi-tv)"
    );
}

/// Readarr models editions (narration / abridgement / publisher) under one book;
/// Skadi has no audiobook edition-kind registry — every ASIN is a separate `Book`.
#[then("audiobook editions such as Full Cast or Booktrack are modelled as acquirable-unit kinds")]
async fn no_edition_kinds(_w: &mut World) {
    panic!(
        "no audiobook edition-kind registry: a Full Cast / Booktrack / Dramatized edition is a \
         second monitored `books` row with its own ASIN (SKADI-T-0400)"
    );
}
