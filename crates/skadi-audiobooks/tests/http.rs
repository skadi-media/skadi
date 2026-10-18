//! Audiobooks HTTP module tests (SKADI-T-0131).
//!
//! Exercises the `AudiobooksHttp` router directly (routes are relative, so paths
//! here are `/books`, `/authors`, …). A real Cloacina runner is built so
//! `AudiobooksHttp` can be constructed.
//!
//! Provider-mock approach: `AudiobooksHttp` stores a **concrete**
//! `Arc<AudnexusProvider>` (its author routes need inherent methods not on the
//! generic `MetadataProvider` trait). Rather than a boxed test-only constructor,
//! we build a real `AudnexusProvider` pointed at a `wiremock` server via
//! `with_base_url` and script the Audnexus endpoints the add/lookup paths hit
//! (`GET /books/{asin}`, `GET /authors/{asin}`, `GET /authors?name=`). This
//! exercises the real lookup path end-to-end with no network.
//!
//! Coverage: list books (empty), 404 on unknown book, add-by-ASIN happy path
//! (with profile/root defaults + validation), get/patch/delete, the
//! domain-disabled 409 + 404 ordering on acquire, the in-flight conflict, reset,
//! the book/author lookup previews, and add-author.

use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tokio::sync::Mutex;
use tower::ServiceExt;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use skadi_api::HttpModule;
use skadi_audiobooks::{AudiobooksHttp, AudiobooksRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS};
use skadi_core::{AcquisitionStatus, BookFileId, BookId};
use skadi_http::HttpClient;
use skadi_metadata::AudnexusProvider;
use skadi_store::{ConfigRepo, ConfigSource, DomainStateRepo, SettingsRepo, Store};
use skadi_testsupport::TestDb;

/// Canonical Audnexus book JSON for the test ASIN.
fn book_json() -> serde_json::Value {
    json!({
        "asin": "B003ITRL7G",
        "title": "The Way of Kings",
        "subtitle": "Book One of the Stormlight Archive",
        "authors": [{ "asin": "A1", "name": "Brandon Sanderson" }],
        "narrators": [{ "name": "Michael Kramer" }, { "name": "Kate Reading" }],
        "seriesPrimary": { "asin": "S1", "name": "Stormlight Archive", "position": "1" },
        "runtimeLengthMin": 2734,
        "image": "https://img/wok.jpg",
        "releaseDate": "2010-08-31T00:00:00.000Z",
        "formatType": "unabridged",
        "summary": "Roshar is a world of stone and storms.",
        "language": "english"
    })
}

struct Harness {
    http: AudiobooksHttp,
    store: Store,
    // Keep the mock server + isolated database alive for the whole test.
    _server: MockServer,
    _db: TestDb,
}

/// Serializes Cloacina runner construction across the parallel tests in this
/// binary (shared `cloacina_hunter` schema on Postgres). Mirrors the movies
/// test harness.
static RUNNER_SETUP: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

async fn harness() -> Harness {
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();

    let runner = {
        let _guard = RUNNER_SETUP.lock().await;
        Arc::new(skadi_hunter::build_runner(db.url()).await.unwrap())
    };

    // A scripted Audnexus server: the book lookup + author lookup/search the
    // add/lookup routes hit.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/books/B003ITRL7G"))
        .respond_with(ResponseTemplate::new(200).set_body_json(book_json()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/authors/A1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "asin": "A1",
            "name": "Brandon Sanderson",
            "description": "Author of the Stormlight Archive.",
            "image": "https://img/bs.jpg"
        })))
        .mount(&server)
        .await;
    // The lookup handler enriches each candidate (SKADI-T-0152), so A2 needs a
    // resolvable author record too — with a bio so it isn't dropped as junk.
    Mock::given(method("GET"))
        .and(path("/authors/A2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "asin": "A2",
            "name": "Brandon Mull",
            "description": "Author of Fablehaven.",
            "image": "https://img/bm.jpg"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/authors"))
        .and(query_param("name", "brandon"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "asin": "A1", "name": "Brandon Sanderson" },
            { "asin": "A1", "name": "Brandon Sanderson" },
            { "asin": "A2", "name": "Brandon Mull" }
        ])))
        .mount(&server)
        .await;

    let provider = Arc::new(
        AudnexusProvider::new(HttpClient::new(Duration::from_secs(5)).unwrap())
            .with_base_url(server.uri()),
    );
    let http = AudiobooksHttp::new(store.clone(), provider, None, runner);
    Harness {
        http,
        store,
        _server: server,
        _db: db,
    }
}

async fn call(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = router.oneshot(request).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

/// Register a profile + point `library.root` at `/library` (SKADI-T-0302: the
/// root is derived, not operator-chosen). Returns `(profile_id, derived_root)`
/// where the derived audiobook root is `<library.root>/audiobook`.
async fn register_profile_and_root(store: &Store) -> (String, String) {
    let profile_id = uuid::Uuid::new_v4().to_string();
    store
        .put_setting("profiles", &profile_id, &json!({ "name": "test" }))
        .await
        .unwrap();
    store
        .set_config("library.root", "/library", ConfigSource::Runtime)
        .await
        .unwrap();
    (profile_id, "/library/audiobook".to_string())
}

#[tokio::test]
async fn book_crud_lifecycle() {
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    // Empty.
    let (s, body) = call(routes(), "GET", "/books", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);

    // Create (explicit profile + root, both registered).
    let req = json!({
        "asin": "B003ITRL7G",
        "profile": profile_id,
        "root": root_path,
    });
    let (s, created) = call(routes(), "POST", "/books", Some(req.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "create: {created}");
    assert_eq!(created["title"], "The Way of Kings");
    assert_eq!(created["year"], 2010);
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["files"].as_array().unwrap().len(), 1);
    // Domain disabled in this harness → no search started.
    assert_eq!(created["search_started"], false);

    // Duplicate ASIN → 409.
    let (s, _) = call(routes(), "POST", "/books", Some(req)).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // List has one, with its sort keys (SKADI-T-0737).
    let (s, body) = call(routes(), "GET", "/books", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["title"], "The Way of Kings");
    assert_eq!(body[0]["sort_title"], "way of kings");
    assert_eq!(body[0]["sort_authors"], json!(["sanderson brandon"]));
    assert_eq!(body[0]["sort_series"], "stormlight archive");

    // Get.
    let (s, got) = call(routes(), "GET", &format!("/books/{id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["id"], id);
    assert_eq!(got["monitored"], true);

    // Patch monitored=false.
    let (s, patched) = call(
        routes(),
        "PATCH",
        &format!("/books/{id}"),
        Some(json!({ "monitored": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(patched["monitored"], false);

    // monitored filter.
    let (_, mon) = call(routes(), "GET", "/books?monitored=true", None).await;
    assert_eq!(mon.as_array().unwrap().len(), 0);
    let (_, unmon) = call(routes(), "GET", "/books?monitored=false", None).await;
    assert_eq!(unmon.as_array().unwrap().len(), 1);

    // Delete → 204, then 404.
    let (s, _) = call(routes(), "DELETE", &format!("/books/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(routes(), "DELETE", &format!("/books/{id}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// SKADI-T-0741: a kid allowed a book still sees it after the book is
/// deleted and added again. The delete moves the allowance from the old id to
/// the ASIN, and the re-added book (a new id) matches the ASIN.
#[tokio::test]
async fn a_kid_keeps_a_book_that_is_deleted_and_added_again() {
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;
    let req = json!({ "asin": "B003ITRL7G", "profile": profile_id, "root": root_path });
    let (s, created) = call(routes(), "POST", "/books", Some(req.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "create: {created}");
    let old_id = created["id"].as_str().unwrap().to_string();

    let kid = json!({
        "id": "kid-1",
        "name": "Robin",
        "role": "kid",
        "policy": { "kinds": ["audiobook"], "allowed_books": [old_id] },
        "created_at": "2026-01-01T00:00:00Z",
    });
    h.store
        .put_setting(skadi_api::household::MEMBERS_KIND, "kid-1", &kid)
        .await
        .unwrap();

    let (s, _) = call(routes(), "DELETE", &format!("/books/{old_id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let row = h
        .store
        .get_setting(skadi_api::household::MEMBERS_KIND, "kid-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.body["policy"]["allowed_books"], json!(["B003ITRL7G"]));

    let (s, again) = call(routes(), "POST", "/books", Some(req)).await;
    assert_eq!(s, StatusCode::CREATED, "re-add: {again}");
    let new_id = again["id"].as_str().unwrap().to_string();
    assert_ne!(new_id, old_id);

    let member: skadi_api::household::Member = serde_json::from_value(row.body).unwrap();
    let as_kid = || routes().layer(axum::Extension(member.clone()));
    let (s, _) = call(as_kid(), "GET", &format!("/books/{new_id}"), None).await;
    assert_eq!(s, StatusCode::OK, "the kid sees the re-added book");
    let (s, list) = call(as_kid(), "GET", "/books", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn add_book_defaults_and_validation() {
    let h = harness().await;
    let routes = || h.http.routes();

    let (profile_id, _root_path) = register_profile_and_root(&h.store).await;

    // An unregistered profile id is rejected. The root is derived now
    // (`<library.root>/audiobook`), so there's nothing to validate there —
    // SKADI-T-0302.
    let (s, _) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({
            "asin": "B003ITRL7G",
            "profile": uuid::Uuid::new_v4().to_string(),
        })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown profile rejected");

    // Omitting the profile binds to the built-in audiobook profile (audiobooks
    // rank via the built-in ladder, not a movie `profiles` row — SKADI-T-0142);
    // the root is derived as `<library.root>/audiobook`.
    let (s, created) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({ "asin": "B003ITRL7G" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "defaults applied: {created}");
    assert_eq!(
        created["profile"],
        skadi_audiobooks::audiobook_builtin_profile_id()
            .into_uuid()
            .to_string()
    );
    // It does NOT bind to the (movie-shaped) registered profile row.
    assert_ne!(created["profile"], profile_id);
    assert_eq!(created["root_folder"]["path"], "/library/audiobook");
    assert_eq!(created["search_started"], false, "domain disabled");
}

#[tokio::test]
async fn get_missing_book_is_404() {
    let h = harness().await;
    let (s, _) = call(
        h.http.routes(),
        "GET",
        &format!("/books/{}", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn acquire_requires_enabled_domain_then_404() {
    let h = harness().await;
    // Domain disabled (default) → 409 regardless of whether the ids exist.
    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!(
            "/books/{}/files/{}/acquire",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["error"], "domain_disabled");

    // Enable the domain → now an unknown book surfaces as 404.
    h.store.set_enabled("audiobooks", true).await.unwrap();
    let (s, _) = call(
        h.http.routes(),
        "POST",
        &format!(
            "/books/{}/files/{}/acquire",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn acquire_conflicts_when_file_already_in_flight() {
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;
    h.store.set_enabled("audiobooks", true).await.unwrap();

    let (s, created) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({
            "asin": "B003ITRL7G",
            "profile": profile_id,
            "root": root_path,
            "search": false,
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let book_id = created["id"].as_str().unwrap().to_string();
    let file_id = created["files"][0]["id"].as_str().unwrap().to_string();

    // Put the file in flight, then try a second manual acquire.
    let fid: BookFileId = uuid::Uuid::parse_str(&file_id).unwrap().into();
    h.store
        .set_book_file_status(
            fid,
            AcquisitionStatus::Searching {
                since: chrono::Utc::now(),
                attempts: 1,
            },
        )
        .await
        .unwrap();

    let (s, body) = call(
        routes(),
        "POST",
        &format!("/books/{book_id}/files/{file_id}/acquire"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["error"], "already_in_flight");
}

#[tokio::test]
async fn reset_recovers_a_wedged_file() {
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    let (s, created) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({ "asin": "B003ITRL7G", "profile": profile_id, "root": root_path })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let bid = created["id"].as_str().unwrap().to_string();
    let fid = created["files"][0]["id"].as_str().unwrap().to_string();

    // Wedge it: drive the file to Downloading directly through the repo.
    let file_id: BookFileId = uuid::Uuid::parse_str(&fid).unwrap().into();
    h.store
        .set_book_file_status(
            file_id,
            AcquisitionStatus::Downloading {
                release: skadi_core::ReleaseId::new(),
                progress: 0.0,
            },
        )
        .await
        .unwrap();

    // Reset → 200, status Missing.
    let (s, body) = call(
        routes(),
        "POST",
        &format!("/books/{bid}/files/{fid}/reset"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["status"], "Missing");

    // The file really is Missing now.
    let (_, got) = call(routes(), "GET", &format!("/books/{bid}"), None).await;
    assert_eq!(got["files"][0]["status"], "Missing");

    // Reset on a foreign book id is a 404.
    let (s, _) = call(
        routes(),
        "POST",
        &format!("/books/{}/files/{fid}/reset", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn releases_without_providers_is_config_error() {
    // With no `HunterServices` registered for audiobooks, the interactive search
    // surfaces a clean config error rather than panicking. (The full scored-list
    // shape is covered by the movies parity test + the binary e2e.)
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;
    let (_, created) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({ "asin": "B003ITRL7G", "profile": profile_id, "root": root_path })),
    )
    .await;
    let bid = created["id"].as_str().unwrap().to_string();
    let fid = created["files"][0]["id"].as_str().unwrap().to_string();

    let (s, _) = call(
        routes(),
        "GET",
        &format!("/books/{bid}/files/{fid}/releases"),
        None,
    )
    .await;
    // 500-class: AppError::Config maps to an internal error in skadi-api.
    assert!(
        s.is_server_error() || s == StatusCode::SERVICE_UNAVAILABLE,
        "expected a server-side config error, got {s}"
    );
}

#[tokio::test]
async fn book_lookup_preview_returns_record() {
    let h = harness().await;
    let (s, rec) = call(
        h.http.routes(),
        "GET",
        "/books/lookup?asin=B003ITRL7G",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "lookup: {rec}");
    assert_eq!(rec["title"], "The Way of Kings");
    assert_eq!(rec["series"], "Stormlight Archive");
}

#[tokio::test]
async fn author_add_lookup_and_crud() {
    let h = harness().await;
    let routes = || h.http.routes();

    // Empty.
    let (s, body) = call(routes(), "GET", "/authors", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);

    // Search candidates by name (Audnexus author search).
    let (s, results) = call(routes(), "GET", "/authors/lookup?name=brandon", None).await;
    assert_eq!(s, StatusCode::OK, "lookup: {results}");
    let arr = results.as_array().unwrap();
    assert_eq!(arr.len(), 2, "deduped to 2 distinct authors: {results}");
    assert_eq!(arr[0]["asin"], "A1");
    // Cards are enriched with a portrait + bio snippet (SKADI-T-0152).
    assert_eq!(arr[0]["image"], "https://img/bs.jpg");
    assert_eq!(arr[0]["description"], "Author of the Stormlight Archive.");

    // Add by ASIN (resolves via lookup_author).
    let (s, created) = call(routes(), "POST", "/authors", Some(json!({ "asin": "A1" }))).await;
    assert_eq!(s, StatusCode::CREATED, "add author: {created}");
    assert_eq!(created["name"], "Brandon Sanderson");
    let aid = created["id"].as_str().unwrap().to_string();

    // Idempotent: a second add returns the same author (201, same id).
    let (s, again) = call(routes(), "POST", "/authors", Some(json!({ "asin": "A1" }))).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(again["id"], aid);

    // List has one; get works.
    let (_, list) = call(routes(), "GET", "/authors", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // Surname first (SKADI-T-0737).
    assert_eq!(list[0]["sort_name"], "sanderson brandon");
    let (s, got) = call(routes(), "GET", &format!("/authors/{aid}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["id"], aid);
    // Know-only by default now (SKADI-I-0018): adding an author records + ingests
    // their catalog but acquires nothing until a watcher is applied.
    assert_eq!(got["monitored"], false, "added authors start know-only");

    // Patch monitored=true is still honored (legacy field; migrated to a watcher).
    let (s, patched) = call(
        routes(),
        "PATCH",
        &format!("/authors/{aid}"),
        Some(json!({ "monitored": true })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "patch author: {patched}");
    assert_eq!(patched["monitored"], true);

    // Delete → 204, then 404.
    let (s, _) = call(routes(), "DELETE", &format!("/authors/{aid}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(routes(), "DELETE", &format!("/authors/{aid}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_books_by_author_filter() {
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    // Add the book; it carries no author_id yet (Audnexus authors are separate
    // entities resolved on demand), so filtering by a random author yields none.
    let (s, _) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({ "asin": "B003ITRL7G", "profile": profile_id, "root": root_path })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, by_author) = call(
        routes(),
        "GET",
        &format!("/books?author={}", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(by_author.as_array().unwrap().len(), 0);
}

/// Browse a body of work (SKADI-T-0159): `GET /audiobooks/works?author=` returns
/// every known work ordered by series position, flagged owned (with a `book_id`)
/// and watched (covered by any scope). Seeds three works — one owned, one covered
/// by a series watcher, one missing — plus the owned library book.
#[tokio::test]
async fn works_browse_marks_owned_and_watched() {
    use skadi_audiobooks::{WatchScope, WatchersRepo, Work, WorksRepo};
    use skadi_core::AsinId;

    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    // Own "The Way of Kings" (#1, asin B003ITRL7G) via the real add path.
    let (s, _) = call(
        routes(),
        "POST",
        "/books",
        Some(json!({ "asin": "B003ITRL7G", "profile": profile_id, "root": root_path })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    // Seed the known-works catalog for author A1: the owned #1, an unowned #2
    // (Words of Radiance) in series S1, and an unowned #3 in series S1.
    let mk = |asin: &str, title: &str, pos: &str| {
        let mut w = Work::new(AsinId(asin.to_string()), title);
        w.author_asin = Some(AsinId("A1".to_string()));
        w.series_name = Some("Stormlight Archive".to_string());
        w.series_asin = Some(AsinId("S1".to_string()));
        w.series_position = Some(pos.to_string());
        w.language = Some("english".to_string());
        w
    };
    // A German edition of #2 — must be hidden from the English-only catalog.
    let mut german = Work::new(AsinId("BGER2".into()), "Sturmklänge");
    german.author_asin = Some(AsinId("A1".into()));
    german.series_name = Some("Die Sturmlicht-Chroniken".into());
    german.series_asin = Some(AsinId("S1".into()));
    german.series_position = Some("2".into());
    german.language = Some("german".into());
    h.store
        .upsert_works(&[
            mk("B003ITRL7G", "The Way of Kings", "1"),
            mk("BWOR2", "Words of Radiance", "2"),
            mk("BOATH3", "Oathbringer", "3"),
            german,
        ])
        .await
        .unwrap();
    // Watch the series → the unowned entries become "watched".
    h.store.set_watcher(WatchScope::Series, "S1").await.unwrap();

    let (s, body) = call(routes(), "GET", "/audiobooks/works?author=A1", None).await;
    assert_eq!(s, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(
        rows.len(),
        3,
        "English works only — the German edition is filtered out"
    );
    assert!(
        !rows.iter().any(|r| r["title"] == "Sturmklänge"),
        "German edition must not appear"
    );

    // Ordered by series position: #1, #2, #3.
    assert_eq!(rows[0]["title"], "The Way of Kings");
    assert_eq!(rows[1]["title"], "Words of Radiance");
    assert_eq!(rows[2]["title"], "Oathbringer");

    // #1 is owned (has a book_id); the others are not.
    assert_eq!(rows[0]["owned"], true);
    assert!(rows[0]["book_id"].is_string());
    assert_eq!(rows[1]["owned"], false);
    assert_eq!(rows[1]["book_id"], serde_json::Value::Null);

    // The series watcher covers every entry in S1.
    assert_eq!(rows[0]["watched"], true);
    assert_eq!(rows[1]["watched"], true);
    assert_eq!(rows[2]["watched"], true);

    // Missing the selector entirely → 400.
    let (s, _) = call(routes(), "GET", "/audiobooks/works", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// Keep the unused-import linter honest: `BookId` is referenced via the parse in
// the conflict/reset tests through `BookFileId`; this assertion documents the id
// round-trip the routes rely on.
#[test]
fn book_id_parses_from_uuid_string() {
    let u = uuid::Uuid::new_v4();
    let id = BookId::from(u);
    assert_eq!(id.to_string(), u.to_string());
}

// --- bulk (SKADI-T-0696) ---

/// A book under `root` with one file in `status` (its `file` set when imported).
async fn seed_book(store: &Store, asin: &str, root: &str, status: AcquisitionStatus) -> String {
    use skadi_audiobooks::{Book, BookFile};
    let book = Book::new(
        skadi_core::ExternalIds {
            asin: Some(skadi_core::AsinId(asin.into())),
            ..Default::default()
        },
        format!("Book {asin}"),
        skadi_core::ProfileId::new(),
        skadi_core::RootFolder::new(root),
    );
    store.upsert_book(&book).await.unwrap();
    let mut file = BookFile::missing(book.id);
    if let AcquisitionStatus::Imported { file: f, .. } = &status {
        file.file = Some(f.clone());
    }
    file.status = status;
    store.upsert_book_file(&file).await.unwrap();
    book.id.to_string()
}

fn imported_at(path: std::path::PathBuf) -> AcquisitionStatus {
    AcquisitionStatus::Imported {
        file: skadi_core::FileRef { path },
        quality: skadi_core::QualityId::new(),
        score: 0,
        at: chrono::Utc::now(),
    }
}

async fn bulk(h: &Harness, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    call(h.http.routes(), "POST", "/books/bulk", Some(body)).await
}

#[tokio::test]
async fn bulk_unmonitors_and_monitors_many_books_in_one_request() {
    let h = harness().await;
    let mut ids = Vec::new();
    for n in 0..20 {
        ids.push(
            seed_book(
                &h.store,
                &format!("B{n:09}"),
                "/library/audiobook",
                AcquisitionStatus::Missing,
            )
            .await,
        );
    }
    let (s, report) = bulk(&h, json!({ "ids": ids, "action": "unmonitor" })).await;
    assert_eq!(s, StatusCode::OK, "{report}");
    assert_eq!(report["done"].as_array().unwrap().len(), 20);
    let (_, unmon) = call(h.http.routes(), "GET", "/books?monitored=false", None).await;
    assert_eq!(unmon.as_array().unwrap().len(), 20);

    let ghost = BookId::new().to_string();
    let (s, report) = bulk(&h, json!({ "ids": [ids[0], ghost], "action": "monitor" })).await;
    assert_eq!(s, StatusCode::OK, "{report}");
    assert_eq!(report["done"], json!([ids[0]]));
    assert_eq!(report["not_found"], json!([ghost]));
}

#[tokio::test]
async fn bulk_refuses_a_malformed_request() {
    let h = harness().await;
    for body in [
        json!({ "ids": [], "action": "monitor" }),
        json!({ "ids": ["x"], "action": "search" }),
        json!({ "ids": [BookId::new().to_string()], "action": "search", "delete_files": true }),
    ] {
        let (s, got) = bulk(&h, body.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body} → {got}");
    }
}

#[tokio::test]
async fn bulk_search_starts_runs_for_missing_files_only() {
    let h = harness().await;
    let missing = seed_book(
        &h.store,
        "B000000001",
        "/library/audiobook",
        AcquisitionStatus::Missing,
    )
    .await;
    let owned = seed_book(
        &h.store,
        "B000000002",
        "/library/audiobook",
        imported_at("/library/audiobook/x.m4b".into()),
    )
    .await;
    let busy = seed_book(
        &h.store,
        "B000000003",
        "/library/audiobook",
        AcquisitionStatus::Searching {
            since: chrono::Utc::now(),
            attempts: 1,
        },
    )
    .await;
    let body = json!({ "ids": [missing, owned, busy], "action": "search" });
    let (s, got) = bulk(&h, body.clone()).await;
    assert_eq!(s, StatusCode::CONFLICT, "{got}");
    assert_eq!(got["error"], "domain_disabled");

    h.store.set_enabled("audiobooks", true).await.unwrap();
    let (s, report) = bulk(&h, body).await;
    assert_eq!(s, StatusCode::OK, "{report}");
    assert_eq!(report["searches_started"], 1);
    assert_eq!(report["done"], json!([missing]));
    assert_eq!(report["skipped"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn bulk_delete_removes_rows_and_only_asked_for_files() {
    let h = harness().await;
    let root = std::env::temp_dir().join(format!("skadi-ab-bulk-{}", uuid::Uuid::new_v4()));
    let mut files = Vec::new();
    let mut ids = Vec::new();
    for (asin, name) in [("B000000011", "One"), ("B000000012", "Two")] {
        let dir = root.join("Author").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("book.m4b");
        std::fs::write(&f, b"audio").unwrap();
        ids.push(
            seed_book(
                &h.store,
                asin,
                root.to_str().unwrap(),
                imported_at(f.clone()),
            )
            .await,
        );
        files.push(f);
    }
    let (s, report) = bulk(
        &h,
        json!({ "ids": [ids[0]], "action": "delete", "delete_files": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{report}");
    assert!(!files[0].exists(), "the book file went");
    assert!(
        !root.join("Author/One").exists(),
        "the emptied folder is pruned"
    );
    assert!(root.join("Author").exists(), "a folder still in use stays");

    let (s, report) = bulk(&h, json!({ "ids": [ids[1]], "action": "delete" })).await;
    assert_eq!(s, StatusCode::OK, "{report}");
    assert!(files[1].exists(), "no delete_files → the file stays");
    let (_, left) = call(h.http.routes(), "GET", "/books", None).await;
    assert_eq!(left.as_array().unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(&root);
}

// --- multi-file audiobooks (SKADI-T-0738) ---

/// Like `call`, but the raw response: status, headers and body bytes.
async fn call_raw(
    router: Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let res = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

/// `n` MPEG-1 Layer III frames (64 kbps, 44.1 kHz, mono: 208 bytes each,
/// 26.1 ms each) filled with `fill`.
fn mp3_frames(n: usize, fill: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(n * 208);
    for _ in 0..n {
        v.extend_from_slice(&[0xFF, 0xFB, 0x50, 0xC0]);
        v.resize(v.len() + 204, fill);
    }
    v
}

/// A book folder with three MP3 parts, each wrapped in the tags a rip
/// carries, seeded as a book whose recorded file is part 1. Returns the root,
/// the book id, the file id and the bare frames of each part in order.
async fn seed_three_part_book(h: &Harness) -> (std::path::PathBuf, String, String, Vec<Vec<u8>>) {
    let root = std::env::temp_dir().join(format!("skadi-ab-parts-{}", uuid::Uuid::new_v4()));
    let dir = root.join("author").join("book");
    std::fs::create_dir_all(&dir).unwrap();
    let frames = vec![mp3_frames(200, 1), mp3_frames(300, 2), mp3_frames(100, 3)];
    for (i, audio) in frames.iter().enumerate() {
        // ID3v2 with a 50-byte body, then the frames, then an ID3v1 block.
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x32".to_vec();
        bytes.resize(60, 0);
        bytes.extend(audio);
        let mut tag = vec![0u8; 128];
        tag[0..3].copy_from_slice(b"TAG");
        bytes.extend(&tag);
        std::fs::write(dir.join(format!("Book-Part{}.mp3", i + 1)), &bytes).unwrap();
    }
    std::fs::write(dir.join("cover.jpg"), b"not audio").unwrap();
    let book_id = seed_book(
        &h.store,
        "B000000021",
        root.to_str().unwrap(),
        imported_at(dir.join("Book-Part1.mp3")),
    )
    .await;
    let files = h
        .store
        .list_book_files(BookId::from(uuid::Uuid::parse_str(&book_id).unwrap()))
        .await
        .unwrap();
    let fid = files[0].id.to_string();
    (root, book_id, fid, frames)
}

#[tokio::test]
async fn audio_joins_the_parts_of_a_multi_file_book() {
    let h = harness().await;
    let (root, book_id, fid, frames) = seed_three_part_book(&h).await;
    let joined: Vec<u8> = frames.concat();

    let (s, headers, body) = call_raw(
        h.http.routes(),
        &format!("/books/{book_id}/files/{fid}/audio"),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "audio/mpeg");
    assert_eq!(headers["accept-ranges"], "bytes");
    assert_eq!(headers["content-length"], joined.len().to_string());
    assert!(headers.contains_key("last-modified"));
    assert_eq!(
        body, joined,
        "the body is the frames of every part, no tags"
    );

    // A range that starts in part 1 and ends in part 3.
    let (from, to) = (frames[0].len() - 10, frames[0].len() + frames[1].len() + 9);
    let (s, headers, body) = call_raw(
        h.http.routes(),
        &format!("/books/{book_id}/files/{fid}/audio"),
        &[("range", &format!("bytes={from}-{to}"))],
    )
    .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers["content-range"],
        format!("bytes {from}-{to}/{}", joined.len())
    );
    assert_eq!(body, &joined[from..=to]);

    // The tail, as a resume would ask for it.
    let (s, _, body) = call_raw(
        h.http.routes(),
        &format!("/books/{book_id}/files/{fid}/audio"),
        &[("range", "bytes=-50")],
    )
    .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, &joined[joined.len() - 50..]);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn chapters_of_a_multi_file_book_are_its_parts() {
    let h = harness().await;
    let (root, book_id, fid, _) = seed_three_part_book(&h).await;
    let (s, chapters) = call(
        h.http.routes(),
        "GET",
        &format!("/books/{book_id}/files/{fid}/chapters"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let chapters = chapters.as_array().unwrap();
    assert_eq!(chapters.len(), 3);
    let titles: Vec<&str> = chapters
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Book-Part1", "Book-Part2", "Book-Part3"]);
    assert_eq!(chapters[0]["index"], 0);
    assert_eq!(chapters[0]["start_s"], 0.0);
    // 200 frames of 1152 samples at 44.1 kHz is 5.22 s; 300 more is 13.06 s.
    let end0 = chapters[0]["end_s"].as_f64().unwrap();
    assert!((end0 - 5.22).abs() < 0.2, "{end0}");
    assert_eq!(chapters[1]["start_s"], chapters[0]["end_s"]);
    let end1 = chapters[1]["end_s"].as_f64().unwrap();
    assert!((end1 - 13.06).abs() < 0.3, "{end1}");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn delete_with_files_removes_every_part() {
    let h = harness().await;
    let (root, book_id, _, _) = seed_three_part_book(&h).await;
    let (s, _) = call(
        h.http.routes(),
        "DELETE",
        &format!("/books/{book_id}?delete_files=true"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let dir = root.join("author").join("book");
    for i in 1..=3 {
        assert!(
            !dir.join(format!("Book-Part{i}.mp3")).exists(),
            "part {i} went with the book"
        );
    }
    // The cover is not audio and not the book's to delete; the folder stays for it.
    assert!(dir.join("cover.jpg").exists());
    let _ = std::fs::remove_dir_all(&root);
}
