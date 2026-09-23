//! Movies HTTP module tests (SKADI-T-0054).
//!
//! Exercises the `MoviesHttp` router directly (routes are relative, so paths
//! here are `/movies`, `/edition-kinds`, …). A real Cloacina runner is built so
//! `MoviesHttp` can be constructed, but no acquire is driven to completion here
//! — the manual-acquire happy path is covered by the binary e2e (SKADI-T-0057);
//! this file covers CRUD, the duplicate/404 paths, the domain-disabled 409, and
//! the edition-kinds registry incl. built-in delete protection.

use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::sync::Mutex;
use tower::ServiceExt;

use skadi_core::{
    AcquisitionStatus, ExternalIds, ImdbId, MediaKind, MovieEditionId, ReleaseId,
    Result as SkadiResult, TmdbId,
};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_movies::{MoviesHttp, MoviesRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS};
use skadi_store::{ConfigRepo, ConfigSource, DomainStateRepo, SettingsRepo, Store};
use skadi_testsupport::TestDb;

/// A metadata provider that returns a canned Matrix record for any lookup.
struct FakeProvider;

#[async_trait]
impl MetadataProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn search(&self, q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        // Return a canned Matrix match for Matrix-ish queries so the library
        // import scan (which searches by parsed title/year) gets a proposal.
        if q.title.to_lowercase().contains("matrix") {
            Ok(vec![MetadataMatch {
                external_ids: ExternalIds {
                    tmdb: Some(TmdbId(603)),
                    ..Default::default()
                },
                title: "The Matrix".into(),
                year: Some(1999),
                score: 0.99,
                poster_url: None,
                overview: None,
            }])
        } else {
            Ok(vec![])
        }
    }
    async fn lookup(&self, id: &ExternalId) -> SkadiResult<MetadataRecord> {
        // Echo back the id that was asked for rather than a fixed one. `imdb_id`
        // is UNIQUE, so a stub that answers "tt0133093" for every lookup makes
        // the second movie in any test collide — which is a property of the
        // stub, not of the code under test (SKADI-T-0550).
        let tmdb = match id {
            ExternalId::Tmdb(t) => t.0,
            _ => 603,
        };
        Ok(MetadataRecord {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(tmdb)),
                imdb: Some(ImdbId(format!("tt{tmdb:07}"))),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: Some("The Matrix".into()),
            overview: Some("A hacker learns the truth.".into()),
            runtime_minutes: Some(136),
            release_date: chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![],
            genres: vec!["Action".into(), "Science Fiction".into()],
            content_rating: Some("R".into()),
            ..Default::default()
        })
    }
}

struct Harness {
    http: MoviesHttp,
    store: Store,
    // Keep the isolated database alive for the whole test; its Drop tears down
    // the per-test Postgres database (or SQLite tempfile).
    _db: TestDb,
}

/// Serializes Cloacina runner construction across the parallel tests in this
/// binary. On Postgres the runner sets up a **shared** `cloacina_hunter` schema;
/// concurrent first-time `CREATE SCHEMA` calls race on `pg_namespace` ("duplicate
/// key"). Building runners one-at-a-time lets the first create it and the rest
/// find it already there.
static RUNNER_SETUP: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

async fn harness() -> Harness {
    // Postgres-default (per-test isolated DB) when SKADI_TEST_DATABASE_URL is
    // set; SQLite tempfile otherwise. See `skadi-testsupport` (SKADI-T-0077).
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();

    let runner = {
        let _guard = RUNNER_SETUP.lock().await;
        Arc::new(skadi_hunter::build_runner(db.url()).await.unwrap())
    };
    let provider: Arc<dyn MetadataProvider> = Arc::new(FakeProvider);
    let http = MoviesHttp::new(store.clone(), provider, runner);
    Harness {
        http,
        store,
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

use skadi_api::HttpModule;

/// Register a profile + point `library.root` at `/library` (SKADI-T-0302: the
/// root is derived, not operator-chosen). Returns `(profile_id, derived_movie_root)`
/// where the derived root is `<library.root>/movie`.
async fn register_profile_and_root(store: &Store) -> (String, String) {
    let profile_id = uuid::Uuid::new_v4().to_string();
    store
        .put_setting(
            "profiles",
            &profile_id,
            &serde_json::json!({ "name": "test" }),
        )
        .await
        .unwrap();
    store
        .set_config("library.root", "/library", ConfigSource::Runtime)
        .await
        .unwrap();
    (profile_id, "/library/movie".to_string())
}

#[tokio::test]
async fn movie_crud_lifecycle() {
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    // Empty.
    let (s, body) = call(routes(), "GET", "/movies", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);

    // Create (explicit profile + root, both registered).
    let req = serde_json::json!({
        "tmdb_id": 603,
        "profile": profile_id,
        "root_folder": root_path,
    });
    let (s, created) = call(routes(), "POST", "/movies", Some(req.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(created["title"], "The Matrix");
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["editions"].as_array().unwrap().len(), 1);
    // Domain is disabled in this harness, so no search was started.
    assert_eq!(created["search_started"], false);

    // Duplicate tmdb → 409.
    let (s, _) = call(routes(), "POST", "/movies", Some(req)).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // List has one.
    let (s, body) = call(routes(), "GET", "/movies", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Get.
    let (s, got) = call(routes(), "GET", &format!("/movies/{id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["id"], id);
    assert_eq!(got["monitored"], true);

    // Patch monitored=false.
    let (s, patched) = call(
        routes(),
        "PATCH",
        &format!("/movies/{id}"),
        Some(serde_json::json!({ "monitored": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(patched["monitored"], false);

    // monitored filter.
    let (_, mon) = call(routes(), "GET", "/movies?monitored=true", None).await;
    assert_eq!(mon.as_array().unwrap().len(), 0);
    let (_, unmon) = call(routes(), "GET", "/movies?monitored=false", None).await;
    assert_eq!(unmon.as_array().unwrap().len(), 1);

    // Delete → 204, then 404.
    let (s, _) = call(routes(), "DELETE", &format!("/movies/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(routes(), "DELETE", &format!("/movies/{id}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// SKADI-T-0612: the member on the request decides what the list, detail and
/// stream routes show. A kid whose ceiling is PG never learns an R film exists
/// (empty list, 404 detail, 404 stream); a member with no ceiling sees it; a kid
/// whose ceiling is R sees it too.
#[tokio::test]
async fn household_policy_hides_titles_over_the_ceiling() {
    use skadi_api::household::{MaxRating, Member, Policy, Role};
    let h = harness().await;
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;
    let req =
        serde_json::json!({ "tmdb_id": 603, "profile": profile_id, "root_folder": root_path });
    let (s, created) = call(h.http.routes(), "POST", "/movies", Some(req)).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(created["content_rating"], "R");
    let id = created["id"].as_str().unwrap().to_string();
    let edition = created["editions"][0]["id"].as_str().unwrap().to_string();

    let as_member = |role: Role, movie_ceiling: Option<&str>| {
        let member = Member {
            role,
            policy: Policy {
                max_rating: MaxRating {
                    movie: movie_ceiling.map(str::to_string),
                    series: None,
                },
                ..Policy::default()
            },
            ..Member::open_mode_admin()
        };
        h.http.routes().layer(axum::Extension(member))
    };

    // Kid, ceiling PG: the film is not there.
    let (s, list) = call(as_member(Role::Kid, Some("PG")), "GET", "/movies", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 0, "hidden from the list");
    let (s, _) = call(
        as_member(Role::Kid, Some("PG")),
        "GET",
        &format!("/movies/{id}"),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "detail is a 404, not a 403: the kid never learns it exists"
    );
    let (s, _) = call(
        as_member(Role::Kid, Some("PG")),
        "GET",
        &format!("/movies/{id}/editions/{edition}/video"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "the bytes are hidden too");

    // Kid, ceiling R: visible.
    let (_, list) = call(as_member(Role::Kid, Some("R")), "GET", "/movies", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // Member with no ceiling: visible.
    let (s, got) = call(
        as_member(Role::Member, None),
        "GET",
        &format!("/movies/{id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["id"], id);
    // Admin: always.
    let (_, list) = call(as_member(Role::Admin, Some("G")), "GET", "/movies", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

/// SKADI-T-0617: a non-admin's page is cut from the policy-filtered list. With
/// two films and a kid allowed only the second, `?limit=1` must return that
/// film with a total of 1 — the old code paged the store first, returned the
/// hidden first film's empty page, and the app took it for the last page.
#[tokio::test]
async fn household_pages_after_filtering_for_non_admins() {
    use skadi_api::household::{MaxRating, Member, Policy, Role};
    let h = harness().await;
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;
    let mut ids = Vec::new();
    for tmdb in [603u64, 604] {
        let req = serde_json::json!({ "tmdb_id": tmdb, "profile": profile_id, "root_folder": root_path });
        let (s, created) = call(h.http.routes(), "POST", "/movies", Some(req)).await;
        assert_eq!(s, StatusCode::CREATED);
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    let kid = Member {
        role: Role::Kid,
        policy: Policy {
            max_rating: MaxRating { movie: Some("PG".into()), series: None },
            allowed_items: vec![ids[1].clone()],
            ..Policy::default()
        },
        ..Member::open_mode_admin()
    };
    let routes = || h.http.routes().layer(axum::Extension(kid.clone()));
    let (s, page) = call(routes(), "GET", "/movies?limit=1&offset=0", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page.as_array().unwrap().len(), 1, "the first page holds the one visible film");
    assert_eq!(page[0]["id"], ids[1]);
    let (_, rest) = call(routes(), "GET", "/movies?limit=1&offset=1", None).await;
    assert_eq!(rest.as_array().unwrap().len(), 0, "nothing beyond it");
    // The admin still pages the store directly.
    let (_, all) = call(h.http.routes(), "GET", "/movies?limit=1&offset=1", None).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn add_movie_defaults_and_validation() {
    let h = harness().await;
    let routes = || h.http.routes();

    // With nothing registered, adding fails with a clear validation error.
    let (s, body) = call(
        routes(),
        "POST",
        "/movies",
        Some(serde_json::json!({ "tmdb_id": 603 })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no profile registered: {body}");

    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    // An unregistered profile id is rejected (the root is no longer operator-set,
    // so there's nothing to validate there — SKADI-T-0302).
    let (s, _) = call(
        routes(),
        "POST",
        "/movies",
        Some(serde_json::json!({
            "tmdb_id": 603,
            "profile": uuid::Uuid::new_v4().to_string(),
        })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown profile rejected");

    // Omitting the profile falls back to the first registered one; the root is
    // derived as `<library.root>/movie` (root_path == "/library/movie").
    let (s, created) = call(
        routes(),
        "POST",
        "/movies",
        Some(serde_json::json!({ "tmdb_id": 603 })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "defaults applied: {created}");
    assert_eq!(created["profile"], profile_id);
    assert_eq!(created["root_folder"]["path"], root_path);
    assert_eq!(created["search_started"], false, "domain disabled");
}

#[tokio::test]
async fn acquire_conflicts_when_edition_already_in_flight() {
    use skadi_core::AcquisitionStatus;

    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;
    h.store.set_enabled("movies", true).await.unwrap();

    let (s, created) = call(
        routes(),
        "POST",
        "/movies",
        Some(serde_json::json!({
            "tmdb_id": 603,
            "profile": profile_id,
            "root_folder": root_path,
            // No search here: this test drives the status by hand.
            "search": false,
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let movie_id = created["id"].as_str().unwrap().to_string();
    let edition_id = created["editions"][0]["id"].as_str().unwrap().to_string();

    // Put the edition in flight, then try a second manual acquire.
    let movie = h
        .store
        .get_movie(skadi_core::MovieId::from(
            uuid::Uuid::parse_str(&movie_id).unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    let mut edition = movie.editions[0].clone();
    edition.status = AcquisitionStatus::Searching {
        since: chrono::Utc::now(),
        attempts: 1,
    };
    h.store.upsert_edition(&edition).await.unwrap();

    let (s, body) = call(
        routes(),
        "POST",
        &format!("/movies/{movie_id}/editions/{edition_id}/acquire"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["error"], "already_in_flight");
}

#[tokio::test]
async fn get_missing_movie_is_404() {
    let h = harness().await;
    let (s, _) = call(
        h.http.routes(),
        "GET",
        &format!("/movies/{}", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn acquire_requires_enabled_domain() {
    let h = harness().await;
    // Domain disabled (default) → 409 regardless of whether the ids exist.
    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!(
            "/movies/{}/editions/{}/acquire",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body["error"], "domain_disabled");

    // Enable the domain → now an unknown movie surfaces as 404.
    h.store.set_enabled("movies", true).await.unwrap();
    let (s, _) = call(
        h.http.routes(),
        "POST",
        &format!(
            "/movies/{}/editions/{}/acquire",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn edition_kinds_registry_crud_and_builtin_protection() {
    let h = harness().await;
    let routes = || h.http.routes();

    // Built-ins are seeded.
    let (s, kinds) = call(routes(), "GET", "/edition-kinds", None).await;
    assert_eq!(s, StatusCode::OK);
    let arr = kinds.as_array().unwrap();
    assert!(arr.len() >= 6, "expected built-in kinds, got {}", arr.len());
    let builtin_id = arr.iter().find(|k| k["builtin"] == true).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Deleting a built-in is refused (Validation → 400).
    let (s, _) = call(
        routes(),
        "DELETE",
        &format!("/edition-kinds/{builtin_id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Create a custom kind.
    let (s, created) = call(
        routes(),
        "POST",
        "/edition-kinds",
        Some(serde_json::json!({
            "name": "Special Edition",
            "normalized_tag": "Special Edition",
            "match_patterns": ["special edition", "se"]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(created["builtin"], false);
    let id = created["id"].as_str().unwrap().to_string();

    // Update it.
    let (s, updated) = call(
        routes(),
        "PUT",
        &format!("/edition-kinds/{id}"),
        Some(serde_json::json!({
            "name": "Special Edition",
            "normalized_tag": "Special Edition",
            "match_patterns": ["special edition"]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(updated["match_patterns"].as_array().unwrap().len(), 1);

    // Delete the custom kind → 204.
    let (s, _) = call(routes(), "DELETE", &format!("/edition-kinds/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Unknown kind → 404.
    let (s, _) = call(
        routes(),
        "GET",
        &format!("/edition-kinds/{}", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Full library-import flow through the HTTP boundary (SKADI-T-0074):
/// scan an on-disk tree → proposed match with high confidence → commit →
/// the file is restructured (hardlinked) to its canonical path under the root
/// folder and the movie shows as `Imported` pointing at the *canonical* file;
/// the original is left untouched on disk.
#[tokio::test]
async fn library_import_scan_then_commit_in_place() {
    let h = harness().await;
    let routes = || h.http.routes();

    // Seed a profile so the commit's default-profile resolution succeeds (the
    // import UX no longer sends one — SKADI-T-0304).
    register_profile_and_root(&h.store).await;

    // A sample on-disk library outside any managed root.
    let media = skadi_core::unique_temp_path("libimport");
    let movie_dir = media.join("The Matrix (1999)");
    std::fs::create_dir_all(&movie_dir).unwrap();
    let file = movie_dir.join("The.Matrix.1999.1080p.BluRay.x264.mkv");
    {
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(64 * 1024 * 1024).unwrap(); // past the 50 MiB sample threshold
    }
    // Capture the source inode so we can prove the canonical path is the same
    // inode after adoption MOVES it (SKADI-T-0303).
    #[cfg(unix)]
    let src_inode = {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(&file).unwrap();
        (m.dev(), m.ino())
    };

    // Adopt into `<library.root>/movie` (SKADI-T-0302). Point the root at `media`
    // so the canonical tree lands under `media/movie` (same FS → hardlink works).
    h.store
        .set_config(
            "library.root",
            &media.display().to_string(),
            ConfigSource::Runtime,
        )
        .await
        .unwrap();

    // 1) Scan → one parse-only candidate (no metadata lookup at this step).
    let (s, scan) = call(
        routes(),
        "POST",
        "/library-import/scan",
        Some(serde_json::json!({ "path": media.display().to_string() })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cands = scan.as_array().unwrap();
    assert_eq!(cands.len(), 1, "one candidate");
    let cand = &cands[0];
    assert_eq!(cand["parsed_title"], "The Matrix");
    assert_eq!(cand["parsed_year"], 1999);
    assert_eq!(cand["quality_name"], "Bluray-1080p");
    assert!(cand.get("proposed").is_none(), "scan is parse-only");
    let scanned_path = cand["path"].as_str().unwrap().to_string();
    assert_eq!(scanned_path, file.to_string_lossy());

    // 1b) Match the page → high-confidence Matrix proposal, not yet in library.
    let (s, matched) = call(
        routes(),
        "POST",
        "/library-import/match",
        Some(serde_json::json!({ "items": [{
            "path": scanned_path,
            "title": cand["parsed_title"],
            "year": cand["parsed_year"],
        }] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let mrow = &matched.as_array().unwrap()[0];
    assert_eq!(mrow["path"], scanned_path.as_str());
    assert_eq!(mrow["proposed"]["tmdb_id"], 603);
    assert_eq!(mrow["confidence"], "high");
    assert_eq!(mrow["already_in_library"], false);

    // 2) Commit the confirmed item: restructured into the canonical layout.
    let (s, result) = call(
        routes(),
        "POST",
        "/library-import/commit",
        Some(serde_json::json!({
            // No profile/root: derived + defaulted server-side (SKADI-T-0304/0302).
            "items": [{
                "path": scanned_path,
                "tmdb_id": 603,
                "quality_id": cand["quality_id"],
            }],
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(result["imported"], 1);
    assert_eq!(result["skipped"], 0);
    assert_eq!(result["linked"], 1, "hardlinked to the canonical path");
    assert_eq!(result["in_place"], 0);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);

    // 3) The movie now lists as Imported, pointing at the canonical path
    //    (`<root>/the-matrix_(1999)_{tmdb-603}/theatrical/the-matrix_(1999).mkv`),
    //    which is a hardlink of the original.
    let canonical = media.join(
        "movie/the-matrix_(1999)_{tmdb-603}_{imdb-tt0000603}/theatrical/the-matrix_(1999).mkv",
    );
    let (_, movies) = call(routes(), "GET", "/movies", None).await;
    let movies = movies.as_array().unwrap();
    assert_eq!(movies.len(), 1);
    let m = &movies[0];
    assert_eq!(m["title"], "The Matrix");
    let edition = &m["editions"][0];
    let imported = &edition["status"]["Imported"];
    assert!(imported.is_object(), "edition status is Imported");
    assert_eq!(
        imported["file"]["path"],
        canonical.to_string_lossy().as_ref()
    );
    assert!(canonical.exists(), "canonical file placed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let b = std::fs::metadata(&canonical).unwrap();
        assert_eq!(
            src_inode,
            (b.dev(), b.ino()),
            "hardlink, not copy (inode preserved across the move)"
        );
    }

    // 4) Reorg-via-link is a MOVE (SKADI-T-0303): the dedicated source folder is
    //    gone (its now-empty parent pruned), the data living on at the canonical
    //    path under `media/movie`.
    assert!(!file.exists(), "source file removed (moved)");
    assert!(!movie_dir.exists(), "dedicated source folder removed");

    // 4b) A re-scan of the same root no longer lists the file (SKADI-T-0320):
    //     it is now in the library, and matched by **inode** rather than path —
    //     the canonical name differs from the one it was scanned under, so a path
    //     comparison would still show it and invite a re-import.
    let (s, rescan) = call(
        routes(),
        "POST",
        "/library-import/scan",
        Some(serde_json::json!({ "path": media.display().to_string() })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        rescan.as_array().unwrap().is_empty(),
        "already-imported file should be filtered out: {rescan}"
    );

    // …unless the operator explicitly asks to see them.
    let (_, rescan_all) = call(
        routes(),
        "POST",
        "/library-import/scan",
        Some(serde_json::json!({
            "path": media.display().to_string(),
            "include_imported": true,
        })),
    )
    .await;
    assert_eq!(
        rescan_all.as_array().unwrap().len(),
        1,
        "include_imported returns it"
    );

    // 5) A re-match now reports the candidate as already in the library.
    let (_, rematch) = call(
        routes(),
        "POST",
        "/library-import/match",
        Some(serde_json::json!({ "items": [{
            "path": scanned_path,
            "title": cand["parsed_title"],
            "year": cand["parsed_year"],
        }] })),
    )
    .await;
    assert_eq!(rematch[0]["already_in_library"], true);

    // 6) Committing it again is a no-op skip.
    let (_, again) = call(
        routes(),
        "POST",
        "/library-import/commit",
        Some(serde_json::json!({
            "items": [{ "path": scanned_path, "tmdb_id": 603, "quality_id": cand["quality_id"] }],
        })),
    )
    .await;
    assert_eq!(again["imported"], 0);
    assert_eq!(again["skipped"], 1);

    std::fs::remove_dir_all(&media).ok();
}

#[tokio::test]
async fn reset_recovers_a_wedged_edition() {
    // SKADI-T-0112: an edition stuck in a non-terminal acquire state (e.g. a
    // daemon crash lost the run) can be forced back to Missing via the reset
    // endpoint — no manual DB surgery.
    let h = harness().await;
    let routes = || h.http.routes();
    let (profile_id, root_path) = register_profile_and_root(&h.store).await;

    let (s, created) = call(
        routes(),
        "POST",
        "/movies",
        Some(
            serde_json::json!({ "tmdb_id": 603, "profile": profile_id, "root_folder": root_path }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let mid = created["id"].as_str().unwrap().to_string();
    let eid = created["editions"][0]["id"].as_str().unwrap().to_string();

    // Wedge it: drive the edition to Downloading directly through the repo.
    let edition_id: MovieEditionId = uuid::Uuid::parse_str(&eid).unwrap().into();
    h.store
        .set_edition_status(
            edition_id,
            AcquisitionStatus::Downloading {
                release: ReleaseId::new(),
                progress: 0.0,
            },
        )
        .await
        .unwrap();

    // Reset → 200, status Missing.
    let (s, body) = call(
        routes(),
        "POST",
        &format!("/movies/{mid}/editions/{eid}/reset"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["status"], "Missing");

    // The edition really is Missing now (re-acquirable).
    let (_, got) = call(routes(), "GET", &format!("/movies/{mid}"), None).await;
    assert_eq!(got["editions"][0]["status"], "Missing");

    // Reset on a foreign movie id is a 404.
    let (s, _) = call(
        routes(),
        "POST",
        &format!("/movies/{}/editions/{eid}/reset", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// SKADI-T-0184: the `POST /movies/quality/test` endpoint runs the real decision
/// engine over a supplied title and returns the full explanation — the HTTP ↔
/// decision-engine integration point. Sets up the domain's live scoring (a
/// profile that rewards an "x265" custom format) and asserts the breakdown.
#[tokio::test]
async fn test_release_endpoint_explains_a_title_via_the_live_engine() {
    use skadi_core::{CustomFormatId, ProfileId};
    use skadi_hunter::services::{ScoringConfig, reset_services};
    use skadi_hunter::{HunterServices, InMemoryStatusSink, set_services};
    use skadi_quality::{
        CustomFormat, CustomFormatScore, FormatRule, QualityProfile, default_definitions,
    };

    // A matcher is required to build HunterServices but the test endpoint never
    // imports, so it's a no-op.
    struct NoMatcher;
    impl skadi_importer::AcquirableMatcher for NoMatcher {
        fn match_file(
            &self,
            _: &skadi_quality::ParsedRelease,
            _: &std::path::Path,
            _: &skadi_importer::CompletedDownload,
        ) -> Vec<skadi_importer::AcquirableMatch> {
            vec![]
        }
    }

    let h = harness().await;
    let defs = default_definitions();
    let lo = defs.iter().find(|d| d.name == "Bluray-720p").unwrap().id;
    let hi = defs.iter().find(|d| d.name == "Bluray-1080p").unwrap().id;
    let fmt = CustomFormatId::new();
    let profile = QualityProfile {
        id: ProfileId::new(),
        name: "test".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![CustomFormatScore {
            format: fmt,
            score: 150,
            mode: skadi_quality::FormatMode::Preferred,
        }],
        min_format_score: 0,
    };
    let registry = vec![CustomFormat {
        id: fmt,
        name: "x265".into(),
        rules: vec![FormatRule::Codec("x265".into())],
    }];

    let svc = Arc::new(HunterServices {
        kind: MediaKind::Movie,
        store: h.store.clone(),
        status: Arc::new(InMemoryStatusSink::new()),
        indexers: vec![],
        downloaders: vec![],
        importer: Arc::new(skadi_importer::DefaultImporter::new(NoMatcher)),
        importer_factory: None,
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs.clone(),
            profile,
            formats: registry,
            min_seeders: 0,
            audiobook: None,
        },
    });
    reset_services();
    set_services(svc);

    // An x265 1080p title → accepted, format-boosted, full breakdown.
    let (status, body) = call(
        h.http.routes(),
        "POST",
        "/movies/quality/test",
        Some(serde_json::json!({ "title": "The.Matrix.1999.1080p.BluRay.x265-GRP" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], true, "body: {body}");
    assert_eq!(body["quality"], "Bluray-1080p");
    assert_eq!(body["quality_rank"], 1);
    assert_eq!(body["format_score"], 150);
    assert_eq!(body["matched_formats"][0]["name"], "x265");
    assert_eq!(body["matched_formats"][0]["score"], 150);
    assert_eq!(body["decision"], "Accept");

    // An empty title is a 400.
    let (status, _) = call(
        h.http.routes(),
        "POST",
        "/movies/quality/test",
        Some(serde_json::json!({ "title": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    reset_services();
}

/// SKADI-T-0162: a movie cannot be misfiled into another domain's tree.
///
/// The ticket described the risk as an add-form root picker offering the
/// audiobooks root. SKADI-T-0302 removed the picker and made each domain derive
/// `<library.root>/<kind subfolder>`, but `PATCH /movies/{id}` still honoured an
/// operator-supplied `root_folder` — the last way left to point a movie at the
/// audiobooks tree. `create_movie` already ignored it; this was the inconsistency.
#[tokio::test]
async fn patch_cannot_move_a_movie_out_of_its_domain_tree() {
    let h = harness().await;
    let (_profile, movie_root) = register_profile_and_root(&h.store).await;
    let (s, created) = call(
        h.http.routes(),
        "POST",
        "/movies",
        Some(serde_json::json!({ "tmdb_id": 603, "title": "The Matrix", "year": 1999 })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let derived = created["root_folder"]["path"].as_str().unwrap().to_string();
    assert_eq!(
        derived, movie_root,
        "the root is derived from the media kind, not chosen"
    );

    // Ask to move it into the audiobooks tree — accepted for wire compatibility,
    // but not applied.
    let (s, patched) = call(
        h.http.routes(),
        "PATCH",
        &format!("/movies/{id}"),
        Some(serde_json::json!({ "root_folder": "/mnt/storage/audiobooks" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "an old client sending it must not fail");
    assert_eq!(
        patched["root_folder"]["path"].as_str(),
        Some(derived.as_str()),
        "the root is unchanged"
    );

    // And it is still unchanged when read back, not just in the response.
    let (_, got) = call(h.http.routes(), "GET", &format!("/movies/{id}"), None).await;
    assert_eq!(got["root_folder"]["path"].as_str(), Some(derived.as_str()));
}

/// Tag membership round-trips through PATCH and filters the list (SKADI-T-0550).
#[tokio::test]
async fn tags_round_trip_through_patch_and_filter_the_list() {
    let h = harness().await;
    register_profile_and_root(&h.store).await;

    let mut ids = Vec::new();
    for (tmdb, title) in [
        (603u64, "The Matrix"),
        (604, "Reloaded"),
        (605, "Revolutions"),
    ] {
        let (s, created) = call(
            h.http.routes(),
            "POST",
            "/movies",
            Some(serde_json::json!({
                "tmdb_id": tmdb,
                "title": title,
                "year": 1999,
            })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{created}");
        ids.push(created["id"].as_str().unwrap().to_string());
    }

    // Tag two of the three.
    for (i, tags) in [(0usize, vec!["anime"]), (1, vec!["anime", "uhd"])] {
        let (s, _) = call(
            h.http.routes(),
            "PATCH",
            &format!("/movies/{}", ids[i]),
            Some(serde_json::json!({ "tags": tags })),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }

    let (s, body) = call(h.http.routes(), "GET", "/movies?tags=anime", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2, "{body}");

    let (s, body) = call(h.http.routes(), "GET", "/movies?tags=uhd", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");

    // Any, not all — picking two tags means "either bucket", which is what an
    // operator selecting from a list expects (and what Sonarr does).
    let (s, body) = call(h.http.routes(), "GET", "/movies?tags=anime,uhd", None).await;
    assert_eq!(body.as_array().unwrap().len(), 2, "{body}");
    assert_eq!(s, StatusCode::OK);

    // Omitting `tags` from a PATCH leaves them alone; `[]` clears them. Those
    // are different edits, and conflating them would make every unrelated PATCH
    // silently untag the item.
    let (s, _) = call(
        h.http.routes(),
        "PATCH",
        &format!("/movies/{}", ids[0]),
        Some(serde_json::json!({ "monitored": false })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, body) = call(h.http.routes(), "GET", "/movies?tags=anime", None).await;
    assert_eq!(
        body.as_array().unwrap().len(),
        2,
        "a PATCH that omits tags must not clear them: {body}"
    );

    let (s, _) = call(
        h.http.routes(),
        "PATCH",
        &format!("/movies/{}", ids[0]),
        Some(serde_json::json!({ "tags": [] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, body) = call(h.http.routes(), "GET", "/movies?tags=anime", None).await;
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");

    // A filter that parses to nothing is refused rather than silently showing
    // the whole library, which would look like it worked.
    let (s, _) = call(h.http.routes(), "GET", "/movies?tags=", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Deleting an item takes its tag membership with it (SKADI-T-0560).
///
/// Without this the rows outlive the item, and a later item that happened to
/// reuse the id would inherit them — the silent orphaning *arr does and the
/// vision calls out as something skadi does not.
#[tokio::test]
async fn deleting_a_movie_clears_its_tags() {
    use skadi_store::ItemTagRepo;
    let h = harness().await;
    register_profile_and_root(&h.store).await;

    let (s, created) = call(
        h.http.routes(),
        "POST",
        "/movies",
        Some(serde_json::json!({ "tmdb_id": 603, "title": "The Matrix", "year": 1999 })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (s, _) = call(
        h.http.routes(),
        "PATCH",
        &format!("/movies/{id}"),
        Some(serde_json::json!({ "tags": ["anime"] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.store.tags_for("movie", &id).await.unwrap(), vec!["anime"]);

    let (s, _) = call(h.http.routes(), "DELETE", &format!("/movies/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(
        h.store.tags_for("movie", &id).await.unwrap().is_empty(),
        "the membership rows must go with the item"
    );
}
