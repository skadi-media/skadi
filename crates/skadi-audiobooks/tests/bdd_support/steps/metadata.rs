//! C26 audiobook metadata sync steps (`add_book` / `refresh_book`).
use async_trait::async_trait;
use chrono::NaiveDate;
use cucumber::{given, then, when};

use skadi_audiobooks::{AudiobooksRepo, add_book, refresh_book};
use skadi_core::{AcquisitionStatus, AsinId, MediaKind, ProfileId, Result, RootFolder};
use skadi_metadata::{
    ExternalId, ImageKind, ImageRef, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord,
};

use crate::bdd_support::World;

#[derive(Debug, Clone)]
pub struct FakeProvider {
    pub record: MetadataRecord,
}

#[async_trait]
impl MetadataProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake-audnexus"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Audiobook
    }
    async fn search(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, _id: &ExternalId) -> Result<MetadataRecord> {
        Ok(self.record.clone())
    }
}

fn provider(w: &mut World) -> &mut FakeProvider {
    w.provider.get_or_insert_with(|| FakeProvider {
        record: MetadataRecord::default(),
    })
}

#[given(
    expr = "the book provider returns {string} by {string} narrated by {string} released {word}"
)]
async fn provider_returns(
    w: &mut World,
    title: String,
    author: String,
    narrator: String,
    date: String,
) {
    let p = provider(w);
    p.record.title = title;
    p.record.subtitle = Some("A Novel".into());
    p.record.authors = vec![author];
    p.record.narrators = vec![narrator];
    p.record.runtime_minutes = Some(2734);
    p.record.release_date = Some(NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap());
    p.record.images = vec![ImageRef {
        kind: ImageKind::Poster,
        path: "https://img/cover.jpg".into(),
    }];
}

#[given(expr = "the provider record places it in series {string} at position {string}")]
async fn provider_series(w: &mut World, series: String, pos: String) {
    let p = provider(w);
    p.record.series = Some(series);
    p.record.series_position = Some(pos);
}

#[given("the provider record has no series")]
async fn provider_no_series(w: &mut World) {
    let p = provider(w);
    p.record.series = None;
    p.record.series_position = None;
}

#[when(expr = "the book with ASIN {word} is added from the provider")]
async fn add(w: &mut World, asin: String) {
    let p = provider(w).clone();
    let store = w.store();
    match add_book(
        &store,
        &p,
        AsinId(asin.clone()),
        ProfileId::new(),
        RootFolder::new("/audiobooks"),
    )
    .await
    {
        Ok(b) => {
            w.files.insert(asin.clone(), b.files[0].clone());
            w.books.insert(asin, b.clone());
            w.last_book = Some(b);
            w.error = None;
        }
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[then(expr = "the library holds {string} by {string} from {int} with one Missing file")]
async fn holds(w: &mut World, title: String, author: String, year: u16) {
    let b = w.last_book.as_ref().expect("book");
    assert_eq!(b.title, title);
    assert_eq!(b.authors, vec![author]);
    assert_eq!(b.year, Some(year));
    assert!(b.monitored, "a freshly added book is monitored");
    assert!(b.cover_url.is_some());
    assert!(b.last_metadata_refresh.is_some());
    let stored = w.store().get_book(b.id).await.unwrap().expect("persisted");
    assert_eq!(stored.files.len(), 1);
    assert!(matches!(stored.files[0].status, AcquisitionStatus::Missing));
}

#[then(expr = "the book is linked to series {string} at position {string}")]
async fn linked(w: &mut World, series: String, pos: String) {
    let b = w.last_book.as_ref().expect("book");
    let link = b.series.as_ref().expect("series link");
    assert_eq!(link.name, series);
    assert_eq!(link.position.as_deref(), Some(pos.as_str()));
    let row = w
        .store()
        .get_series_by_name(&series)
        .await
        .unwrap()
        .expect("series row created");
    assert_eq!(row.id, link.series_id);
}

#[then("the book is linked to no series")]
async fn unlinked(w: &mut World) {
    assert!(w.last_book.as_ref().unwrap().series.is_none());
}

#[then(expr = "the add is rejected as a duplicate naming {string}")]
async fn duplicate(w: &mut World, title: String) {
    let e = w.error.as_deref().expect("an error");
    assert!(e.contains("already exists") && e.contains(&title), "{e}");
}

#[given(expr = "the book {word} was unmonitored by the user")]
async fn user_unmonitored(w: &mut World, asin: String) {
    let mut b = w.books.get(&asin).expect("book").clone();
    b.monitored = false;
    w.store().upsert_book(&b).await.unwrap();
    w.books.insert(asin, b);
}

#[when(expr = "the book {word} is refreshed from the provider")]
async fn refresh(w: &mut World, asin: String) {
    let p = provider(w).clone();
    let existing = w.books.get(&asin).expect("book").clone();
    let store = w.store();
    let b = refresh_book(&store, &p, AsinId(asin.clone()), Some(existing), None)
        .await
        .expect("refresh_book");
    store.upsert_book(&b).await.unwrap();
    w.last_book = Some(b);
}

#[then(
    expr = "the refreshed book is titled {string} and keeps the user's monitored flag, profile and root folder"
)]
async fn preserved(w: &mut World, title: String) {
    let b = w.last_book.as_ref().expect("book");
    let before = w.books.values().find(|x| x.id == b.id).expect("before");
    assert_eq!(b.title, title);
    assert!(!b.monitored);
    assert_eq!(b.profile, before.profile);
    assert_eq!(b.root_folder, before.root_folder);
    assert_eq!(b.added_at, before.added_at);
    let stored = w.store().get_book(b.id).await.unwrap().unwrap();
    assert_eq!(stored.files.len(), 1, "files survive a refresh");
}

#[when("a fresh book is refreshed without provider defaults")]
async fn refresh_no_defaults(w: &mut World) {
    let p = provider(w).clone();
    let store = w.store();
    w.error = refresh_book(&store, &p, AsinId("B0NEW".into()), None, None)
        .await
        .err()
        .map(|e| e.to_string());
}

#[then("the refresh is rejected as a validation error")]
async fn refresh_rejected(w: &mut World) {
    let e = w.error.as_deref().expect("an error");
    assert!(e.contains("defaults required"), "{e}");
}
