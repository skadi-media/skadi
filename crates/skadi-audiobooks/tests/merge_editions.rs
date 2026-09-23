//! Merging the duplicate `books` rows that predate the edition model
//! (SKADI-T-0562 part 2).

use skadi_audiobooks::book::Book;
use skadi_audiobooks::book_file::BookFile;
use skadi_audiobooks::maintenance::merge_book_editions;
use skadi_audiobooks::repo::AudiobooksRepo;
use skadi_core::{ExternalIds, ProfileId, RootFolder};
use skadi_testsupport::TestDb;

async fn fresh_store() -> (TestDb, skadi_store::Store) {
    let db = TestDb::new(
        skadi_audiobooks::SQLITE_MIGRATIONS,
        skadi_audiobooks::POSTGRES_MIGRATIONS,
    )
    .await;
    let store = db.store.clone();
    (db, store)
}

fn book_named(title: &str, author: &str, added_secs: i64) -> Book {
    let mut b = Book::new(
        ExternalIds::default(),
        title,
        ProfileId::new(),
        RootFolder::new("/audiobooks"),
    );
    b.authors = vec![author.to_string()];
    b.added_at = chrono::DateTime::from_timestamp(added_secs, 0).unwrap();
    b
}

/// Two rows for one work, holding different editions, become one book with both.
#[tokio::test]
async fn duplicate_rows_fold_into_one_book_with_both_editions() {
    let (_db, store) = fresh_store().await;

    let first = book_named("Project Hail Mary", "Andy Weir", 1_000);
    let second = book_named(
        "Project Hail Mary (Dramatized Adaptation)",
        "Andy Weir",
        2_000,
    );
    store.upsert_book(&first).await.unwrap();
    store.upsert_book(&second).await.unwrap();
    store
        .upsert_book_file(&BookFile::missing_of_kind(first.id, "unabridged"))
        .await
        .unwrap();
    let moved = BookFile::discovered_of_kind(second.id, "dramatized");
    store.upsert_book_file(&moved).await.unwrap();

    // A dry run reports and changes nothing — the whole point of the two-step.
    let dry = merge_book_editions(&store, false).await.unwrap();
    assert_eq!(dry.groups, 1);
    assert_eq!(dry.moved, 1);
    assert_eq!(dry.removed, 1);
    assert_eq!(store.list_book_files(second.id).await.unwrap().len(), 1);
    assert!(store.get_book(second.id).await.unwrap().is_some());

    let report = merge_book_editions(&store, true).await.unwrap();
    assert_eq!(
        report, dry,
        "the apply must do exactly what the dry run said"
    );

    // The earlier row is canonical and now holds both editions.
    let kept = store
        .get_book(first.id)
        .await
        .unwrap()
        .expect("canonical survives");
    let mut kinds: Vec<String> = kept.files.iter().map(|f| f.kind.clone()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["dramatized", "unabridged"]);
    assert!(
        store.get_book(second.id).await.unwrap().is_none(),
        "duplicate row is gone"
    );

    // The moved edition kept its id — which is its AcquirableRef, so every row in
    // downloads/history/blocklist still points at it. This is why the merge does
    // not need a ref-rewriting pass.
    let carried = kept.files.iter().find(|f| f.kind == "dramatized").unwrap();
    assert_eq!(carried.id, moved.id);
    assert_eq!(carried.acquirable_ref(), moved.acquirable_ref());
    assert!(
        !carried.monitored,
        "a moved edition keeps its own monitored flag"
    );
}

/// Two rows holding the *same* edition kind are two files of one edition.
/// Choosing between them means deleting media, so the merge refuses.
#[tokio::test]
async fn a_kind_collision_is_reported_and_nothing_is_deleted() {
    let (_db, store) = fresh_store().await;

    let first = book_named("Dune", "Frank Herbert", 1_000);
    let second = book_named("Dune", "Frank Herbert", 2_000);
    store.upsert_book(&first).await.unwrap();
    store.upsert_book(&second).await.unwrap();
    for b in [&first, &second] {
        store
            .upsert_book_file(&BookFile::missing_of_kind(b.id, "unabridged"))
            .await
            .unwrap();
    }

    let report = merge_book_editions(&store, true).await.unwrap();
    assert_eq!(report.conflicted, 1);
    assert_eq!(report.moved, 0);
    assert_eq!(
        report.removed, 0,
        "the duplicate still owns an edition — deleting it would take the file"
    );
    assert!(store.get_book(second.id).await.unwrap().is_some());
    assert_eq!(store.list_book_files(second.id).await.unwrap().len(), 1);
}

/// A library with no duplicates is left entirely alone.
#[tokio::test]
async fn distinct_works_are_not_merged() {
    let (_db, store) = fresh_store().await;
    let a = book_named("Dune", "Frank Herbert", 1_000);
    let b = book_named("Project Hail Mary", "Andy Weir", 2_000);
    store.upsert_book(&a).await.unwrap();
    store.upsert_book(&b).await.unwrap();

    let report = merge_book_editions(&store, true).await.unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(report.groups, 0);
    assert_eq!(report.moved, 0);
    assert_eq!(report.removed, 0);
    assert!(store.get_book(a.id).await.unwrap().is_some());
    assert!(store.get_book(b.id).await.unwrap().is_some());
}

/// The canonical row is the earliest `added_at`, whatever order rows come back
/// in — a dry run and the apply that follows must agree on which row survives.
#[tokio::test]
async fn the_earliest_row_is_canonical() {
    let (_db, store) = fresh_store().await;
    let late = book_named("Dune", "Frank Herbert", 9_000);
    let early = book_named("Dune", "Frank Herbert", 1_000);
    // Inserted late-first, so a merge that just took the first row would fail.
    store.upsert_book(&late).await.unwrap();
    store.upsert_book(&early).await.unwrap();

    let report = merge_book_editions(&store, true).await.unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].canonical, early.id);
    assert!(store.get_book(early.id).await.unwrap().is_some());
    assert!(store.get_book(late.id).await.unwrap().is_none());
}
