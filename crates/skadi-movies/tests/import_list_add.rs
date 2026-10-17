//! The movies import-list add hook with IMDb ids (SKADI-T-0733): an IMDb list
//! item resolves to a TMDB id, then takes the TMDB path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::Utc;

use skadi_api::import_lists::sync_list;
use skadi_api::library::{ImportListAddOptions, LibraryProvider};
use skadi_core::{AppError, ExternalIds, ImdbId, MediaKind, Result as SkadiResult, TmdbId};
use skadi_metadata::import_list::{ImportListProvider, ListItem};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_movies::{MoviesLibrary, MoviesRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS};
use skadi_store::{
    ConfigRepo, ConfigSource, ImportList, ImportListExclusion, ImportListRepo, SettingsRepo, Store,
};
use skadi_testsupport::TestDb;

/// Knows two films by IMDb id; anything else is not found, as TMDB's `/find`
/// answers. Counts IMDb lookups.
#[derive(Default)]
struct FakeProvider {
    imdb_lookups: AtomicUsize,
}

fn known(imdb: &str) -> Option<(u64, &'static str)> {
    match imdb {
        "tt0133093" => Some((603, "The Matrix")),
        "tt0088763" => Some((105, "Back to the Future")),
        _ => None,
    }
}

fn record(tmdb: u64, imdb: &str, title: &str) -> MetadataRecord {
    MetadataRecord {
        external_ids: ExternalIds {
            tmdb: Some(TmdbId(tmdb)),
            imdb: Some(ImdbId(imdb.into())),
            ..Default::default()
        },
        title: title.into(),
        release_date: chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
        ..Default::default()
    }
}

#[async_trait]
impl MetadataProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn search(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, id: &ExternalId) -> SkadiResult<MetadataRecord> {
        match id {
            ExternalId::Imdb(i) => {
                self.imdb_lookups.fetch_add(1, Ordering::SeqCst);
                let (tmdb, title) = known(&i.0)
                    .ok_or_else(|| AppError::NotFound(format!("no movie for {}", i.0)))?;
                Ok(record(tmdb, &i.0, title))
            }
            ExternalId::Tmdb(t) => match t.0 {
                603 => Ok(record(603, "tt0133093", "The Matrix")),
                105 => Ok(record(105, "tt0088763", "Back to the Future")),
                n => Err(AppError::NotFound(format!("no movie {n}"))),
            },
            other => Err(AppError::Validation(format!("{other:?}"))),
        }
    }
}

struct StubList(Vec<ListItem>);

#[async_trait]
impl ImportListProvider for StubList {
    fn kind(&self) -> &'static str {
        "imdb_list"
    }
    async fn fetch(&self, _s: &serde_json::Value) -> SkadiResult<Vec<ListItem>> {
        Ok(self.0.clone())
    }
}

fn imdb(id: &str) -> ListItem {
    ListItem {
        id_kind: "imdb".into(),
        external_id: id.into(),
        title: None,
        year: None,
    }
}

async fn setup() -> (TestDb, Store) {
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    store
        .put_setting(
            "profiles",
            &uuid::Uuid::new_v4().to_string(),
            &serde_json::json!({ "name": "test" }),
        )
        .await
        .unwrap();
    store
        .set_config("library.root", "/library", ConfigSource::Runtime)
        .await
        .unwrap();
    (db, store)
}

fn a_list() -> ImportList {
    let mut l = ImportList::new("Sci-fi", "imdb_list", "movie");
    l.settings = serde_json::json!({"list_id": "ls012345678"});
    l
}

#[tokio::test]
async fn an_imdb_list_adds_its_movies_by_tmdb_id() {
    let (_db, store) = setup().await;
    let provider = Arc::new(FakeProvider::default());
    let library = MoviesLibrary::new(store.clone()).with_provider(provider.clone());
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(library)];
    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubList(vec![
        imdb("tt0133093"),
        imdb("tt0088763"),
        imdb("tt0000001"),
    ]))];

    let report = sync_list(&a_list(), &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 2, "{report:?}");
    assert_eq!(report.failed.len(), 1, "{report:?}");
    let (id, why) = &report.failed[0];
    assert_eq!(id, "tt0000001");
    assert!(why.contains("IMDb id tt0000001 matches no movie"), "{why}");

    let matrix = (&store as &dyn MoviesRepo)
        .get_movie_by_tmdb(TmdbId(603))
        .await
        .unwrap()
        .expect("added by its TMDB id");
    assert_eq!(matrix.title, "The Matrix");
    assert!(!matrix.monitored, "the list's add_monitored is off");

    // The second sync finds both in the library by IMDb id, without a lookup.
    let before = provider.imdb_lookups.load(Ordering::SeqCst);
    let report = sync_list(&a_list(), &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 0);
    assert_eq!(report.existing, 2, "{report:?}");
    assert_eq!(
        provider.imdb_lookups.load(Ordering::SeqCst) - before,
        1,
        "only the unknown id is looked up again"
    );
}

#[tokio::test]
async fn an_exclusion_by_tmdb_id_holds_for_an_imdb_list() {
    // The movie page's "Exclude from lists" posts the TMDB id.
    let (_db, store) = setup().await;
    store
        .add_exclusion(&ImportListExclusion {
            id: uuid::Uuid::new_v4().to_string(),
            target_domain: "movie".into(),
            id_kind: "tmdb".into(),
            external_id: "603".into(),
            title: Some("The Matrix".into()),
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    let library =
        MoviesLibrary::new(store.clone()).with_provider(Arc::new(FakeProvider::default()));
    let outcome = library
        .add_from_list("imdb", "tt0133093", &ImportListAddOptions::default())
        .await
        .unwrap();
    // Counted as excluded, not as already in the library.
    assert_eq!(outcome, skadi_api::library::ListAdd::Excluded);
    assert!(
        (&store as &dyn MoviesRepo)
            .get_movie_by_tmdb(TmdbId(603))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_malformed_imdb_id_or_another_kind_is_a_validation_error() {
    let (_db, store) = setup().await;
    let library = MoviesLibrary::new(store).with_provider(Arc::new(FakeProvider::default()));
    let opts = ImportListAddOptions::default();
    let err = library
        .add_from_list("imdb", "ls123", &opts)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Validation(_)), "{err:?}");
    let err = library
        .add_from_list("tvdb", "81189", &opts)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("tmdb or imdb"), "{err}");
}
