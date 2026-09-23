//! Author-monitoring discovery integration test (SKADI-T-0132).
//!
//! A monitored author + a wiremock'd Audible catalog (author → product ASINs) +
//! a wiremock'd Audnexus (per-ASIN enrichment) → one discovery pass adds the
//! author's new book as a monitored `Missing` `Book`, which the wanted-query then
//! reports as wanted. A second pass is a no-op (idempotent).

use std::sync::Arc;

use skadi_audiobooks::{
    AudiobookWantedQuery, AudiobooksRepo, Author, AuthorDiscovery, SQLITE_MIGRATIONS,
    WantedScoring, default_audiobook_profile,
};
use skadi_core::AsinId;
use skadi_hunter::WantedQuery;
use skadi_metadata::{AudibleCatalogProvider, AudnexusProvider};
use skadi_store::{ConfigRepo, ConfigSource, SettingsRepo, Store};
use skadi_testsupport::TestDb;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ASIN: &str = "B08G9PRS1K";

async fn seed_profile_and_root(store: &Store) {
    store
        .put_setting(
            "profiles",
            &uuid::Uuid::new_v4().to_string(),
            &serde_json::json!({ "name": "test" }),
        )
        .await
        .unwrap();
    // Single library root (SKADI-T-0302): discovery derives the root from
    // `library.root`; point it at a throwaway path.
    store
        .set_config("library.root", "/audiobooks", ConfigSource::Runtime)
        .await
        .unwrap();
}

fn client() -> skadi_http::HttpClient {
    skadi_http::HttpClient::new(std::time::Duration::from_secs(5)).unwrap()
}

#[tokio::test]
async fn monitored_author_discovery_adds_a_new_book_and_is_idempotent() {
    let db = TestDb::new(SQLITE_MIGRATIONS, skadi_audiobooks::POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    seed_profile_and_root(&store).await;

    // A monitored author (with an ASIN — required for the watcher model).
    let author = {
        let mut a = Author::new("Andy Weir");
        a.asin = Some(AsinId("AUTH1".into()));
        a
    };
    store.upsert_author(&author).await.unwrap();

    // Audible catalog: the author has one product.
    let catalog_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/catalog/products"))
        .and(query_param("author", "Andy Weir"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": [
                { "asin": ASIN, "title": "Project Hail Mary", "release_date": "2021-05-04" }
            ]
        })))
        .mount(&catalog_server)
        .await;

    // Audnexus: enrichment for that ASIN.
    let audnexus_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/books/{ASIN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "asin": ASIN,
            "title": "Project Hail Mary",
            "authors": [{ "name": "Andy Weir" }],
            "release_date": "2021-05-04"
        })))
        .mount(&audnexus_server)
        .await;

    let repo: Arc<dyn AudiobooksRepo> = Arc::new(store.clone());
    let catalog =
        Arc::new(AudibleCatalogProvider::new(client()).with_base_url(catalog_server.uri()));
    let enrich = Arc::new(AudnexusProvider::new(client()).with_base_url(audnexus_server.uri()));
    let discovery = AuthorDiscovery::new(repo.clone(), store.clone(), catalog, enrich);

    // First pass: ingest stores the work + auto-creates the author watcher;
    // resolving the watcher acquires the unowned work as a monitored book.
    let (ingested, acquired) = discovery.run_once().await;
    assert_eq!(ingested, 1, "one work ingested into known-works");
    assert_eq!(acquired, 1, "one book acquired via the author watcher");

    let book = store
        .get_book_by_asin(&AsinId(ASIN.into()))
        .await
        .unwrap()
        .expect("book was added");
    assert!(book.monitored, "auto-added book is monitored");
    assert_eq!(book.title, "Project Hail Mary");
    assert_eq!(book.files.len(), 1, "one Missing BookFile");

    // The book file is now wanted (Missing on a monitored book).
    let wanted = AudiobookWantedQuery::new(
        Arc::new(store.clone()),
        WantedScoring {
            profile: default_audiobook_profile(),
        },
    )
    .wanted()
    .await
    .unwrap();
    assert!(
        wanted
            .iter()
            .any(|w| w.acquirable == book.files[0].acquirable_ref()),
        "the discovered book's file is wanted"
    );

    // Second pass: idempotent — the work is now owned, so nothing new is acquired.
    let (_ingested2, acquired2) = discovery.run_once().await;
    assert_eq!(acquired2, 0, "no duplicate acquisition on re-run");
}
