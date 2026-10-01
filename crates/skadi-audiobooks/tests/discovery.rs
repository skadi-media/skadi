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

// ---------------------------------------------------------------------------
// SKADI-T-0650: whole-catalog reads, inline language, and a page budget.
// ---------------------------------------------------------------------------

/// `n` products in `lang`, ASINs prefixed so pages and authors are told apart.
fn products(prefix: &str, n: usize, lang: &str, author: &str) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            serde_json::json!({
                "asin": format!("{prefix}{i:03}"),
                "title": format!("{prefix} {i}"),
                "language": lang,
                "authors": [{ "name": author }]
            })
        })
        .collect()
}

async fn mount_author_page(
    server: &MockServer,
    author: &str,
    page: u32,
    total: usize,
    items: Vec<serde_json::Value>,
) {
    Mock::given(method("GET"))
        .and(path("/1.0/catalog/products"))
        .and(query_param("author", author))
        .and(query_param("page", page.to_string()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "total_results": total, "products": items })),
        )
        .mount(server)
        .await;
}

/// The measured George R. R. Martin shape: the newest page is all translations
/// and the English novels sit on later pages. Reading one page ingested none of
/// them. Language must also arrive inline — no per-title detail request.
#[tokio::test]
async fn ingests_english_works_that_sit_behind_a_page_of_translations() {
    let db = TestDb::new(SQLITE_MIGRATIONS, skadi_audiobooks::POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    let server = MockServer::start().await;
    let a = "George R. R. Martin";
    mount_author_page(&server, a, 0, 61, products("DE", 50, "german", a)).await;
    mount_author_page(&server, a, 1, 61, products("EN", 11, "english", a)).await;
    let catalog = AudibleCatalogProvider::new(client()).with_base_url(server.uri());

    let (n, _) = skadi_audiobooks::ingest_author_works(&store, &catalog, a, None)
        .await
        .unwrap();

    assert_eq!(n, 11, "the eleven English works on page 1 are ingested");
    let works = skadi_audiobooks::WorksRepo::list_all_works(&store)
        .await
        .unwrap();
    assert_eq!(works.len(), 11);
    assert!(
        works
            .iter()
            .all(|w| w.language.as_deref() == Some("english")),
        "no translation is stored"
    );
    let detail_lookups = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() != "/1.0/catalog/products")
        .count();
    assert_eq!(
        detail_lookups, 0,
        "language came inline; no per-title requests"
    );
}

/// The per-pass budget counts **pages**, a partly-read author resumes at the page
/// it stopped on rather than starting again, and authors are visited in a stable
/// order rather than whatever a HashMap yields.
#[tokio::test]
async fn the_pass_budget_counts_pages_and_a_cut_off_author_resumes() {
    let db = TestDb::new(SQLITE_MIGRATIONS, skadi_audiobooks::POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    for (name, asin) in [("Alpha", "AA"), ("Bravo", "BB"), ("Charlie", "CC")] {
        let mut author = Author::new(name);
        author.asin = Some(AsinId(asin.into()));
        store.upsert_author(&author).await.unwrap();
    }
    let server = MockServer::start().await;
    // Alpha: three pages (50, 50, 5). Bravo and Charlie: one short page each.
    mount_author_page(
        &server,
        "Alpha",
        0,
        105,
        products("A0", 50, "english", "Alpha"),
    )
    .await;
    mount_author_page(
        &server,
        "Alpha",
        1,
        105,
        products("A1", 50, "english", "Alpha"),
    )
    .await;
    mount_author_page(
        &server,
        "Alpha",
        2,
        105,
        products("A2", 5, "english", "Alpha"),
    )
    .await;
    mount_author_page(
        &server,
        "Bravo",
        0,
        3,
        products("B0", 3, "english", "Bravo"),
    )
    .await;
    mount_author_page(
        &server,
        "Charlie",
        0,
        2,
        products("C0", 2, "english", "Charlie"),
    )
    .await;

    let audnexus = MockServer::start().await;
    let repo: Arc<dyn AudiobooksRepo> = Arc::new(store.clone());
    let catalog = Arc::new(AudibleCatalogProvider::new(client()).with_base_url(server.uri()));
    let enrich = Arc::new(AudnexusProvider::new(client()).with_base_url(audnexus.uri()));
    let discovery = AuthorDiscovery::new(repo, store.clone(), catalog, enrich);

    // Which (author, page) each request asked for, in order.
    let asked = |reqs: &[wiremock::Request]| -> Vec<(String, String)> {
        reqs.iter()
            .filter(|r| r.url.path() == "/1.0/catalog/products")
            .map(|r| {
                let q: std::collections::HashMap<_, _> = r.url.query_pairs().collect();
                (q["author"].to_string(), q["page"].to_string())
            })
            .collect()
    };

    // Pass 1, two pages: both go to Alpha (first by name), which is not finished.
    discovery.ingest_once(2).await;
    let after1 = server.received_requests().await.unwrap();
    assert_eq!(
        asked(&after1),
        vec![("Alpha".into(), "0".into()), ("Alpha".into(), "1".into())],
        "the budget is two pages, not two authors"
    );

    // Pass 2: Alpha resumes at page 2 (not page 0), finishes, then Bravo.
    discovery.ingest_once(2).await;
    let after2 = server.received_requests().await.unwrap();
    assert_eq!(
        asked(&after2[after1.len()..]),
        vec![("Alpha".into(), "2".into()), ("Bravo".into(), "0".into())],
        "a cut-off author resumes where it stopped"
    );

    // Pass 3 starts at Charlie — the rotation moves on rather than restarting.
    discovery.ingest_once(1).await;
    let after3 = server.received_requests().await.unwrap();
    assert_eq!(
        asked(&after3[after2.len()..]),
        vec![("Charlie".into(), "0".into())],
        "the next pass picks up at the next author"
    );

    let works = skadi_audiobooks::WorksRepo::list_all_works(&store)
        .await
        .unwrap();
    assert_eq!(
        works.len(),
        105 + 3 + 2,
        "every work from every page was kept"
    );
}

/// **Live** check against the real Audible catalog (SKADI-T-0650, SKADI-T-0652).
/// `#[ignore]`d because it needs the network; run it deliberately with
/// `cargo test -p skadi-audiobooks --test discovery -- --ignored live_`.
///
/// Runs the shipped code path — `ingest_author_works` with a real
/// `AudibleCatalogProvider` — into a test database. The lab stack was the planned
/// place for this, but on 2026-09-30 its network could not reach Audible.
#[tokio::test]
#[ignore = "hits the live Audible catalog"]
async fn live_george_r_r_martin_ingests_the_english_ice_and_fire_novels() {
    let db = TestDb::new(SQLITE_MIGRATIONS, skadi_audiobooks::POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    let catalog = AudibleCatalogProvider::new(
        skadi_http::HttpClient::new(std::time::Duration::from_secs(30)).unwrap(),
    );
    let grrm = AsinId("B000APIGH4".into());

    let (n, _) =
        skadi_audiobooks::ingest_author_works(&store, &catalog, "George R. R. Martin", Some(&grrm))
            .await
            .unwrap();
    let works = skadi_audiobooks::WorksRepo::list_all_works(&store)
        .await
        .unwrap();
    eprintln!("ingested {n} English works");

    // English editions of A Song of Ice and Fire #1–#5, observed on Audible
    // pages 1 and 2 on 2026-09-30. None were on page 0, the only page read before.
    for (pos, asin, title) in [
        ("1", "B002UZZ93G", "A Game of Thrones"),
        ("2", "B002UZKIBO", "A Clash of Kings"),
        ("3", "B0036NQ9Z8", "A Storm of Swords"),
        ("4", "B006LPIVL8", "A Feast for Crows"),
        ("5", "B0057POQJE", "A Dance with Dragons"),
    ] {
        let w = works
            .iter()
            .find(|w| w.asin.0 == asin)
            .unwrap_or_else(|| panic!("#{pos} {title} ({asin}) was not ingested"));
        assert_eq!(w.series_position.as_deref(), Some(pos), "{title}");
        assert_eq!(w.language.as_deref(), Some("english"), "{title}");
    }
    assert!(
        works
            .iter()
            .all(|w| w.language.as_deref() == Some("english")),
        "no translation was stored"
    );

    // SKADI-T-0652: Dangerous Women's editors arrive as plain names.
    if let Some(dw) = works.iter().find(|w| w.asin.0 == "B00GXJN3U6") {
        assert_eq!(dw.authors, vec!["George R. R. Martin", "Gardner Dozois"]);
    }
    let suffixed: Vec<&String> = works
        .iter()
        .flat_map(|w| &w.authors)
        .filter(|a| a.contains(" - editor"))
        .collect();
    assert!(suffixed.is_empty(), "role suffixes survived: {suffixed:?}");
}
