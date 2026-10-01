//! Contributor-role normalisation over stored data (SKADI-T-0652).

use skadi_audiobooks::{
    AudiobooksRepo, Author, AuthorFilter, Book, BookFilter, SQLITE_MIGRATIONS, Work, WorksRepo,
    normalize_contributor_roles,
};
use skadi_core::{AsinId, ExternalIds, ProfileId, RootFolder};
use skadi_testsupport::TestDb;

async fn db() -> TestDb {
    TestDb::new(SQLITE_MIGRATIONS, skadi_audiobooks::POSTGRES_MIGRATIONS).await
}

fn book(asin: &str, authors: &[&str]) -> Book {
    let mut b = Book::new(
        ExternalIds {
            asin: Some(AsinId(asin.into())),
            ..Default::default()
        },
        asin,
        ProfileId::new(),
        RootFolder::new("/audiobooks"),
    );
    b.authors = authors.iter().map(|s| (*s).to_string()).collect();
    b
}

fn author(name: &str, asin: Option<&str>) -> Author {
    let mut a = Author::new(name);
    a.asin = asin.map(|x| AsinId(x.into()));
    a
}

async fn authors_by_name(store: &skadi_store::Store) -> Vec<(String, Option<String>)> {
    let mut v: Vec<_> = store
        .list_authors(AuthorFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.name, a.asin.map(|x| x.0)))
        .collect();
    v.sort();
    v
}

/// The production cases: Dangerous Women's two editors, a translator credited as
/// an author, and an author record registered under "Gardner Dozois - editor".
#[tokio::test]
async fn cleans_books_works_and_author_records_and_is_idempotent() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_book(&book(
            "B00GXJN3U6",
            &["George R. R. Martin - editor", "Gardner Dozois - editor"],
        ))
        .await
        .unwrap();
    store
        .upsert_book(&book(
            "DARKFOREST",
            &["Cixin Liu", "Joel Martinsen - translator"],
        ))
        .await
        .unwrap();
    store
        .upsert_book(&book("CLEAN", &["Stephen King"]))
        .await
        .unwrap();
    let mut w = Work::new(AsinId("WORK1".into()), "Rogues");
    w.authors = vec![
        "George R. R. Martin - editor".into(),
        "Gardner Dozois - editor".into(),
    ];
    store.upsert_works(&[w]).await.unwrap();
    store
        .upsert_author(&author("Gardner Dozois - editor", Some("B000AQ1NTQ")))
        .await
        .unwrap();

    let first = normalize_contributor_roles(&store).await.unwrap();
    assert_eq!(first.books, 2, "Dangerous Women and The Dark Forest");
    assert_eq!(first.works, 1);
    assert_eq!(first.authors_renamed, 1);

    let dw = store
        .get_book_by_asin(&AsinId("B00GXJN3U6".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dw.authors, vec!["George R. R. Martin", "Gardner Dozois"]);
    let df = store
        .get_book_by_asin(&AsinId("DARKFOREST".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(df.authors, vec!["Cixin Liu"]);
    let works = store.list_all_works().await.unwrap();
    assert_eq!(
        works[0].authors,
        vec!["George R. R. Martin", "Gardner Dozois"]
    );
    assert_eq!(
        authors_by_name(&store).await,
        vec![("Gardner Dozois".to_string(), Some("B000AQ1NTQ".to_string()))],
        "no author record carries ' - editor'"
    );

    let second = normalize_contributor_roles(&store).await.unwrap();
    assert!(
        !second.changed_anything(),
        "a second run changes nothing: {second:?}"
    );
}

/// A role-suffixed record and a plain record for the same person are folded
/// together, keeping the one with the ASIN, and its books follow.
#[tokio::test]
async fn merges_a_role_suffixed_record_into_the_same_person() {
    let db = db().await;
    let store = db.store.clone();
    let plain = author("Jim Butcher", Some("JB"));
    let suffixed = author("Jim Butcher - editor", None);
    store.upsert_author(&plain).await.unwrap();
    store.upsert_author(&suffixed).await.unwrap();
    let mut b = book("SIDEJOBS", &["Jim Butcher"]);
    b.author_id = Some(suffixed.id);
    store.upsert_book(&b).await.unwrap();

    let r = normalize_contributor_roles(&store).await.unwrap();
    assert_eq!(r.authors_merged, 1);
    assert_eq!(
        authors_by_name(&store).await,
        vec![("Jim Butcher".to_string(), Some("JB".to_string()))]
    );
    let b = store
        .get_book_by_asin(&AsinId("SIDEJOBS".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        b.author_id,
        Some(plain.id),
        "the book follows the kept record"
    );
}

/// When only the role-suffixed record has the ASIN, it is the one kept — renamed
/// — and the ASIN-less plain record goes.
#[tokio::test]
async fn keeps_the_record_with_the_asin_when_merging() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_author(&author("Gardner Dozois", None))
        .await
        .unwrap();
    store
        .upsert_author(&author("Gardner Dozois - editor", Some("B000AQ1NTQ")))
        .await
        .unwrap();

    let r = normalize_contributor_roles(&store).await.unwrap();
    assert_eq!(r.authors_merged, 1);
    assert_eq!(
        authors_by_name(&store).await,
        vec![("Gardner Dozois".to_string(), Some("B000AQ1NTQ".to_string()))]
    );
}

/// Two records with different ASINs are two Audible identities. Renaming one
/// must not quietly merge them — that judgement belongs to SKADI-T-0653.
#[tokio::test]
async fn does_not_merge_two_different_asins() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_author(&author("Ann Leckie", Some("A1")))
        .await
        .unwrap();
    store
        .upsert_author(&author("Ann Leckie - editor", Some("A2")))
        .await
        .unwrap();

    let r = normalize_contributor_roles(&store).await.unwrap();
    assert_eq!((r.authors_merged, r.authors_renamed), (0, 1));
    assert_eq!(
        authors_by_name(&store).await,
        vec![
            ("Ann Leckie".to_string(), Some("A1".to_string())),
            ("Ann Leckie".to_string(), Some("A2".to_string())),
        ]
    );
}

/// A book whose only credit is a translator is left alone rather than emptied:
/// a background job should not destroy information.
#[tokio::test]
async fn leaves_a_book_alone_rather_than_emptying_its_authors() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_book(&book("ODD", &["Joel Martinsen - translator"]))
        .await
        .unwrap();
    let r = normalize_contributor_roles(&store).await.unwrap();
    assert_eq!((r.books, r.books_skipped_empty), (0, 1));
    let b = store
        .list_books(BookFilter::default())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(b.authors, vec!["Joel Martinsen - translator"]);
}

// --- SKADI-T-0653: repairing stored author links ---

use skadi_audiobooks::repair_author_links;

#[tokio::test]
async fn clears_a_works_attribution_to_an_author_it_does_not_name() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_author(&author("George R.R. Martin", Some("B0DNQBC8G7")))
        .await
        .unwrap();
    let mut fat = Work::new(AsinId("B08J1D4ZH5".into()), "The Fat Cat Lotto Method");
    fat.authors = vec!["George R. Martin III".into()];
    fat.author_asins = vec![Some(AsinId("B00PUVY1AE".into()))];
    fat.author_asin = Some(AsinId("B0DNQBC8G7".into()));
    let mut real = Work::new(AsinId("B09SKXD5DW".into()), "The Rise of the Dragon");
    real.authors = vec!["George R. R. Martin".into()];
    real.author_asins = vec![Some(AsinId("B0DNQBC8G7".into()))];
    real.author_asin = Some(AsinId("B0DNQBC8G7".into()));
    store.upsert_works(&[fat, real]).await.unwrap();

    let r = repair_author_links(&store).await.unwrap();
    assert_eq!(r.works_unattributed, 1);
    let fat = store
        .get_work(&AsinId("B08J1D4ZH5".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fat.author_asin, None,
        "Martin III's book no longer counts as his"
    );
    let real = store
        .get_work(&AsinId("B09SKXD5DW".into()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        real.author_asin.is_some(),
        "a work that names him keeps its link"
    );

    assert_eq!(
        repair_author_links(&store).await.unwrap(),
        skadi_audiobooks::LinkReport::default(),
        "idempotent"
    );
}

#[tokio::test]
async fn links_books_only_to_an_unambiguous_author() {
    let db = db().await;
    let store = db.store.clone();
    let king = author("Stephen King", Some("KING"));
    store.upsert_author(&king).await.unwrap();
    store
        .upsert_author(&author("George R. R. Martin", Some("B000APIGH4")))
        .await
        .unwrap();
    store
        .upsert_author(&author("George R.R. Martin", Some("B0DNQBC8G7")))
        .await
        .unwrap();
    store
        .upsert_book(&book("IT", &["Stephen King"]))
        .await
        .unwrap();
    store
        .upsert_book(&book("AGOT", &["George R. R. Martin"]))
        .await
        .unwrap();

    let r = repair_author_links(&store).await.unwrap();
    assert_eq!((r.books_linked, r.books_ambiguous), (1, 1));
    let it = store
        .get_book_by_asin(&AsinId("IT".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(it.author_id, Some(king.id));
    let agot = store
        .get_book_by_asin(&AsinId("AGOT".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        agot.author_id, None,
        "two records match: left unlinked, not guessed"
    );
}

// --- SKADI-T-0656: the repair judges by ASIN, as discovery does ---

fn attributed(asin: &str, names: &[(&str, Option<&str>)], to: &str) -> Work {
    let mut w = Work::new(AsinId(asin.into()), asin);
    w.authors = names.iter().map(|(n, _)| (*n).to_string()).collect();
    w.author_asins = names
        .iter()
        .map(|(_, a)| a.map(|x| AsinId(x.into())))
        .collect();
    w.author_asin = Some(AsinId(to.into()));
    w
}

async fn author_of(store: &skadi_store::Store, asin: &str) -> Option<String> {
    store
        .get_work(&AsinId(asin.into()))
        .await
        .unwrap()
        .unwrap()
        .author_asin
        .map(|a| a.0)
}

/// The production cases: the name differs from the registered record, but the
/// contributor ASIN is the author's. Discovery credits these; the repair used to
/// clear them at every start.
#[tokio::test]
async fn keeps_a_work_whose_stored_asin_is_the_authors_whatever_the_name() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_author(&author("Derek Kunsken", Some("DK")))
        .await
        .unwrap();
    store
        .upsert_author(&author("Dashiell Hammett", Some("DH")))
        .await
        .unwrap();
    store
        .upsert_works(&[
            attributed("W1", &[("Derek Künsken", Some("DK"))], "DK"),
            attributed("W2", &[("Hammett Dashiell", Some("DH"))], "DH"),
        ])
        .await
        .unwrap();

    let r = repair_author_links(&store).await.unwrap();
    assert_eq!(r.works_unattributed, 0);
    assert_eq!(author_of(&store, "W1").await.as_deref(), Some("DK"));
    assert_eq!(author_of(&store, "W2").await.as_deref(), Some("DH"));
}

#[tokio::test]
async fn still_clears_a_work_crediting_somebody_else_by_asin() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_author(&author("Ian Smith", Some("IS")))
        .await
        .unwrap();
    store
        .upsert_works(&[
            // Somebody else's ASIN, a different name.
            attributed("W1", &[("Ian E. Smith", Some("OTHER"))], "IS"),
            // The same name under a different ASIN is somebody else too.
            attributed("W2", &[("Ian Smith", Some("OTHER"))], "IS"),
            // The matching name with no ASIN still counts, as in discovery.
            attributed("W3", &[("Ian Smith", None)], "IS"),
        ])
        .await
        .unwrap();

    let r = repair_author_links(&store).await.unwrap();
    assert_eq!(r.works_unattributed, 2);
    assert_eq!(author_of(&store, "W1").await, None);
    assert_eq!(author_of(&store, "W2").await, None);
    assert_eq!(author_of(&store, "W3").await.as_deref(), Some("IS"));
}

/// A work stored before ASINs were kept has no evidence to judge by. It is left
/// alone — judging it by name is exactly the disagreement this fixes.
#[tokio::test]
async fn leaves_a_work_without_stored_asins_alone() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_author(&author("Fritz Leiber", Some("FL")))
        .await
        .unwrap();
    let mut legacy = Work::new(AsinId("W1".into()), "Swords");
    legacy.authors = vec!["Fritz Leiber Jr.".into()];
    legacy.author_asin = Some(AsinId("FL".into()));
    store.upsert_works(&[legacy]).await.unwrap();

    let r = repair_author_links(&store).await.unwrap();
    assert_eq!(r.works_unattributed, 0);
    assert_eq!(author_of(&store, "W1").await.as_deref(), Some("FL"));
}

#[tokio::test]
async fn stores_contributor_asins_and_the_normaliser_keeps_them_aligned() {
    let db = db().await;
    let store = db.store.clone();
    store
        .upsert_works(&[attributed(
            "W1",
            &[
                ("Ellen Datlow - editor", Some("ED")),
                ("Stephen King", Some("SK")),
            ],
            "SK",
        )])
        .await
        .unwrap();
    let w = store.get_work(&AsinId("W1".into())).await.unwrap().unwrap();
    assert_eq!(w.author_asins.len(), 2, "round-trips through the store");

    normalize_contributor_roles(&store).await.unwrap();
    let w = store.get_work(&AsinId("W1".into())).await.unwrap().unwrap();
    assert_eq!(w.authors, vec!["Stephen King"]);
    assert_eq!(
        w.author_asins,
        vec![Some(AsinId("SK".into()))],
        "the editor's ASIN went with the editor"
    );
}
