//! The audiobooks import-list add hook (SKADI-T-0733): a book on a list is added
//! by ASIN through `add_book`, the path `POST /books` takes, and is monitored
//! unless the list's switch is off (it starts on for an audiobook list)
//! (Dylan's decision, note n-4 on SKADI-T-0733).

use std::sync::Arc;

use async_trait::async_trait;

use skadi_api::import_lists::sync_list;
use skadi_api::library::{ImportListAddOptions, LibraryProvider};
use skadi_audiobooks::{AudiobooksLibrary, AudiobooksRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS};
use skadi_core::{AppError, AsinId, ExternalIds, MediaKind, Result as SkadiResult};
use skadi_metadata::import_list::{ImportListProvider, ListItem};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_store::{ConfigRepo, ConfigSource, ImportList, Store};
use skadi_testsupport::TestDb;

/// Knows one book; any other ASIN is not found.
struct FakeProvider;

#[async_trait]
impl MetadataProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Audiobook
    }
    async fn search(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, id: &ExternalId) -> SkadiResult<MetadataRecord> {
        match id {
            ExternalId::Asin(a) if a.0 == "B08G9PRS1K" => Ok(MetadataRecord {
                external_ids: ExternalIds {
                    asin: Some(a.clone()),
                    ..Default::default()
                },
                title: "Project Hail Mary".into(),
                authors: vec!["Andy Weir".into()],
                narrators: vec!["Ray Porter".into()],
                ..Default::default()
            }),
            other => Err(AppError::NotFound(format!("no book {other:?}"))),
        }
    }
}

struct StubList(Vec<ListItem>);

#[async_trait]
impl ImportListProvider for StubList {
    fn kind(&self) -> &'static str {
        "stub"
    }
    async fn fetch(&self, _s: &serde_json::Value) -> SkadiResult<Vec<ListItem>> {
        Ok(self.0.clone())
    }
}

fn asin(id: &str) -> ListItem {
    ListItem {
        id_kind: "asin".into(),
        external_id: id.into(),
        title: None,
        year: None,
    }
}

async fn setup() -> (TestDb, Store) {
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    store
        .set_config("library.root", "/library", ConfigSource::Runtime)
        .await
        .unwrap();
    (db, store)
}

#[tokio::test]
async fn a_book_on_a_list_is_added_monitored_once() {
    let (_db, store) = setup().await;
    let library = AudiobooksLibrary::new(store.clone()).with_provider(Arc::new(FakeProvider));
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(library)];
    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubList(vec![
        asin("B08G9PRS1K"),
        asin("B000000000"),
    ]))];
    // An audiobook list's monitored switch starts on (Dylan, 2026-10-17).
    let mut list = ImportList::new("To listen", "stub", "audiobook");
    list.settings = serde_json::json!({});
    assert!(list.add_monitored);

    let report = sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 1, "{report:?}");
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, "B000000000");

    let book = (&store as &dyn AudiobooksRepo)
        .get_book_by_asin(&AsinId("B08G9PRS1K".into()))
        .await
        .unwrap()
        .expect("added");
    assert_eq!(book.title, "Project Hail Mary");
    assert!(book.monitored, "a book on a list is added monitored");
    let files = (&store as &dyn AudiobooksRepo)
        .list_book_files(book.id)
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "one default edition, as POST /books makes");
    assert!(files[0].monitored, "and that edition is wanted");
    assert_eq!(
        book.root_folder.path.to_string_lossy(),
        "/library/audiobook",
        "the derived root"
    );

    // Already present on the next sync.
    let report = sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 0);
    assert_eq!(report.existing, 1, "{report:?}");
}

#[tokio::test]
async fn only_asin_ids_and_registered_profiles_are_accepted() {
    let (_db, store) = setup().await;
    let library = AudiobooksLibrary::new(store).with_provider(Arc::new(FakeProvider));
    let err = library
        .add_from_list("tmdb", "603", &ImportListAddOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("by asin"), "{err}");

    let opts = ImportListAddOptions {
        profile_id: Some(uuid::Uuid::new_v4().to_string()),
        ..ImportListAddOptions::default()
    };
    let err = library
        .add_from_list("asin", "B08G9PRS1K", &opts)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not a registered quality profile"),
        "{err}"
    );
}

#[tokio::test]
async fn without_a_provider_the_hook_says_so() {
    let (_db, store) = setup().await;
    let library = AudiobooksLibrary::new(store);
    let err = library
        .add_from_list("asin", "B08G9PRS1K", &ImportListAddOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no metadata provider"), "{err}");
}

#[tokio::test]
async fn a_list_with_the_switch_off_adds_the_book_unmonitored() {
    let (_db, store) = setup().await;
    let library = AudiobooksLibrary::new(store.clone()).with_provider(Arc::new(FakeProvider));
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(library)];
    let providers: Vec<Arc<dyn ImportListProvider>> =
        vec![Arc::new(StubList(vec![asin("B08G9PRS1K")]))];
    let mut list = ImportList::new("Maybe later", "stub", "audiobook");
    list.add_monitored = false;
    let report = sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 1, "{report:?}");
    let book = (&store as &dyn AudiobooksRepo)
        .get_book_by_asin(&AsinId("B08G9PRS1K".into()))
        .await
        .unwrap()
        .expect("added");
    assert!(!book.monitored, "the switch is off");
    let files = (&store as &dyn AudiobooksRepo)
        .list_book_files(book.id)
        .await
        .unwrap();
    assert!(files.iter().all(|f| !f.monitored), "no edition is wanted");
}
