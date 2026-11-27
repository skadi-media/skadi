//! The storage facts API (SKADI-T-0774): totals, the paged file list, the
//! orphans, and the admin gate.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config};
use skadi_store::{LibraryFileRepo, LibraryFileStat, Store};
use skadi_testsupport::TestDb;

const GB: i64 = 1_000_000_000;

fn config() -> Config {
    Config {
        database_url: "sqlite://:memory:".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: Some("operator-token".into()),
    }
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"));
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = skadi_api::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn t(minute: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap() + Duration::minutes(minute)
}

async fn file(store: &Store, kind: &str, path: &str, size: i64, links: i64) -> String {
    let stat = LibraryFileStat {
        kind: kind.into(),
        acquirable_ref: format!("ref-{path}"),
        path: path.into(),
        dev: 1,
        ino: path.len() as i64,
        size_bytes: size,
        disk_bytes: size,
        links,
        mtime: t(0),
    };
    store
        .record_library_file_stat(&stat, t(1))
        .await
        .unwrap()
        .id
}

/// Seven files of three kinds, one of them gone, and two orphans.
async fn seeded() -> (Arc<AppState>, TestDb) {
    let db = TestDb::new_store_only().await;
    let s = &db.store;
    file(s, "movie", "/lib/movie/a.mkv", 12 * GB, 2).await;
    file(s, "movie", "/lib/movie/b.mkv", 2 * GB, 1).await;
    file(s, "series", "/lib/tv/c.mkv", 10 * GB, 1).await;
    file(s, "series", "/lib/tv/d.mkv", 20 * GB, 1).await;
    let e = file(s, "series", "/lib/tv/e.mkv", GB, 1).await;
    s.mark_library_file_gone(&e, t(2)).await.unwrap();
    file(s, "audiobook", "/lib/book/f.m4b", GB / 2, 1).await;
    file(s, "audiobook", "/lib/book/g.m4b", GB / 4, 1).await;
    s.record_library_orphan("/lib/tv/old.mkv", 3 * GB, t(0), t(3))
        .await
        .unwrap();
    s.record_library_orphan("/lib/movie/stray.mp4", GB, t(0), t(3))
        .await
        .unwrap();
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    (state, db)
}

fn paths(page: &serde_json::Value, list: &str) -> Vec<String> {
    page[list]
        .as_array()
        .expect("a list")
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_summary_gives_totals_by_kind_and_the_orphans() {
    let (state, _db) = seeded().await;
    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/library/storage/summary",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let kinds = body["kinds"].as_array().unwrap();
    assert_eq!(
        kinds
            .iter()
            .map(|k| k["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["audiobook", "movie", "series"]
    );
    let movie = &kinds[1];
    assert_eq!(movie["files"], 2);
    assert_eq!(movie["sizeBytes"], 14 * GB);
    assert_eq!(movie["diskBytes"], 14 * GB);
    assert_eq!(movie["linked"], 1);
    assert_eq!(movie["gone"], 0);
    // Nothing is measured before the probe and the checksum collector run.
    assert_eq!(movie["measured"], 0);
    assert_eq!(movie["checksummed"], 0);
    assert!(
        movie["lastReadAt"]
            .as_str()
            .unwrap()
            .starts_with("2026-11-01T12:01")
    );
    let series = &kinds[2];
    // The gone file counts only as gone.
    assert_eq!(series["files"], 2);
    assert_eq!(series["sizeBytes"], 30 * GB);
    assert_eq!(series["gone"], 1);
    assert_eq!(body["orphans"]["files"], 2);
    assert_eq!(body["orphans"]["sizeBytes"], 4 * GB);
    assert!(body["sweeps"].is_array());
}

#[tokio::test]
async fn the_file_list_is_sorted_by_size_and_paged_in_the_database() {
    let (state, _db) = seeded().await;
    let (st, page) = call(
        &state,
        "GET",
        "/api/v1/library/storage/files?limit=2",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{page}");
    assert_eq!(
        page["total"], 6,
        "the files on disk; the gone one is left out"
    );
    assert_eq!((&page["limit"], &page["offset"]), (&2.into(), &0.into()));
    assert_eq!(
        paths(&page, "files"),
        vec!["/lib/tv/d.mkv", "/lib/movie/a.mkv"]
    );
    let first = &page["files"][0];
    assert_eq!(first["kind"], "series");
    assert_eq!(first["sizeBytes"], 20 * GB);
    assert_eq!(first["links"], 1);
    assert!(first["pieces"].is_null() && first["piecesAt"].is_null());
    assert!(first["checksumAt"].is_null());
    assert!(first["goneAt"].is_null());

    let (_, page) = call(
        &state,
        "GET",
        "/api/v1/library/storage/files?limit=2&offset=2",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(
        paths(&page, "files"),
        vec!["/lib/tv/c.mkv", "/lib/movie/b.mkv"]
    );
}

#[tokio::test]
async fn the_file_list_filters_and_sorts_as_asked() {
    let (state, _db) = seeded().await;
    let get = |uri: &'static str| {
        let state = state.clone();
        async move {
            let (st, page) = call(&state, "GET", uri, "operator-token", None).await;
            assert_eq!(st, StatusCode::OK, "{uri}: {page}");
            page
        }
    };
    let page = get("/api/v1/library/storage/files?kind=audiobook").await;
    assert_eq!(page["total"], 2);
    assert_eq!(
        paths(&page, "files"),
        vec!["/lib/book/f.m4b", "/lib/book/g.m4b"]
    );

    let page = get("/api/v1/library/storage/files?state=gone").await;
    assert_eq!(paths(&page, "files"), vec!["/lib/tv/e.mkv"]);
    assert!(page["files"][0]["goneAt"].is_string());

    let page = get("/api/v1/library/storage/files?state=all").await;
    assert_eq!(page["total"], 7);

    let page = get("/api/v1/library/storage/files?sort=path&limit=1").await;
    assert_eq!(
        paths(&page, "files"),
        vec!["/lib/book/f.m4b"],
        "path sorts A to Z"
    );

    let page = get("/api/v1/library/storage/files?sort=size&dir=asc&limit=1").await;
    assert_eq!(paths(&page, "files"), vec!["/lib/book/g.m4b"]);

    let page = get("/api/v1/library/storage/files?sort=links&limit=1").await;
    assert_eq!(paths(&page, "files"), vec!["/lib/movie/a.mkv"]);

    // A page is never larger than the cap, whatever is asked.
    let page = get("/api/v1/library/storage/files?limit=100000").await;
    assert_eq!(page["limit"], skadi_api::library_storage::MAX_PAGE);
}

#[tokio::test]
async fn an_unknown_sort_state_or_direction_is_a_400_that_names_the_choices() {
    let (state, _db) = seeded().await;
    for uri in [
        "/api/v1/library/storage/files?sort=pieces",
        "/api/v1/library/storage/files?state=missing",
        "/api/v1/library/storage/files?dir=up",
    ] {
        let (st, body) = call(&state, "GET", uri, "operator-token", None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("use one of"),
            "{uri}: {body}"
        );
    }
}

#[tokio::test]
async fn the_orphans_are_listed_largest_first_with_their_total() {
    let (state, _db) = seeded().await;
    let (st, page) = call(
        &state,
        "GET",
        "/api/v1/library/storage/orphans?limit=1",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 2);
    assert_eq!(page["sizeBytes"], 4 * GB);
    assert_eq!(paths(&page, "orphans"), vec!["/lib/tv/old.mkv"]);
    assert!(page["orphans"][0]["firstSeen"].is_string());
}

/// Tested with real non-admin accounts: the admin passes every gate, so it
/// can never show that one is missing.
#[tokio::test]
async fn every_storage_endpoint_refuses_a_non_admin() {
    let (state, _db) = seeded().await;
    let mut tokens = Vec::new();
    for (name, role) in [
        ("Robin", "member"),
        ("Sam", "contributor"),
        ("Jamie", "kid"),
    ] {
        let (st, created) = call(
            &state,
            "POST",
            "/api/v1/members",
            "operator-token",
            Some(serde_json::json!({"name": name, "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        tokens.push((role, created["token"].as_str().unwrap().to_string()));
    }
    for path in [
        "/api/v1/library/storage/summary",
        "/api/v1/library/storage/files",
        "/api/v1/library/storage/orphans",
    ] {
        for (role, token) in &tokens {
            let (st, body) = call(&state, "GET", path, token, None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{path} as {role}: {body}");
            assert!(
                body.get("files").is_none() && body.get("kinds").is_none(),
                "{path} as {role} must carry no data: {body}"
            );
        }
        let (st, _) = call(&state, "GET", path, "operator-token", None).await;
        assert_eq!(st, StatusCode::OK, "{path} as the admin");
    }
}

// ---------------------------------------------------------------------------
// The piece probe (SKADI-T-0775)
// ---------------------------------------------------------------------------

const SECRET: &str = "swordfish-do-not-echo";

#[tokio::test]
async fn the_probe_is_off_until_set_up_and_its_credential_is_never_sent_back() {
    use skadi_store::{ConfigRepo, ConfigSource};
    let (state, db) = seeded().await;
    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/storage/probe",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "off");
    assert_eq!(body["enabled"], false);
    assert!(body["credential"].is_null());

    // On, with nothing set: it says what is missing.
    db.store
        .set_config(
            "storage.probe.pieces.enabled",
            "true",
            ConfigSource::Runtime,
        )
        .await
        .unwrap();
    let (_, body) = call(
        &state,
        "GET",
        "/api/v1/storage/probe",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(body["state"], "not_set_up");
    assert_eq!(body["missing"].as_array().unwrap().len(), 4, "{body}");

    // The credential goes in; only its kind comes out, here and after.
    let (st, body) = call(
        &state,
        "PUT",
        "/api/v1/storage/probe/credential",
        "operator-token",
        Some(serde_json::json!({"kind": "password", "secret": SECRET})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["credential"], "password");
    assert!(!body.to_string().contains(SECRET), "{body}");
    let (_, body) = call(
        &state,
        "GET",
        "/api/v1/storage/probe",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(body["credential"], "password");
    assert!(!body.to_string().contains(SECRET), "{body}");
    assert_eq!(body["missing"].as_array().unwrap().len(), 3);
    // Nor does the config surface carry it.
    let (_, config) = call(&state, "GET", "/api/v1/config", "operator-token", None).await;
    assert!(!config.to_string().contains(SECRET));

    let (st, body) = call(
        &state,
        "PUT",
        "/api/v1/storage/probe/credential",
        "operator-token",
        Some(serde_json::json!({"kind": "token", "secret": SECRET})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        !body.to_string().contains(SECRET),
        "an error must not echo the secret"
    );

    let (st, body) = call(
        &state,
        "DELETE",
        "/api/v1/storage/probe/credential",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body["credential"].is_null());
}

#[tokio::test]
async fn a_failed_probe_shows_its_reason_and_an_admin_clears_it() {
    use skadi_store::StorageProbeRepo;
    let (state, db) = seeded().await;
    db.store
        .fail_storage_probe(
            "pieces",
            "the storage host refused the key of user skadi",
            t(5),
        )
        .await
        .unwrap();
    let (_, body) = call(
        &state,
        "GET",
        "/api/v1/storage/probe",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(body["state"], "failed");
    assert_eq!(
        body["reason"],
        "the storage host refused the key of user skadi"
    );
    assert!(body["failedAt"].is_string());
    assert!(body["clearedAt"].is_null());

    // The health check carries the reason and the way out.
    let (st, checks) = call(
        &state,
        "POST",
        "/api/v1/health/checks/run?id=storage-piece-probe",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{checks}");
    let check = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "storage-piece-probe")
        .expect("the probe's check");
    assert_eq!(check["severity"], "error", "{check}");
    assert!(
        check["message"]
            .as_str()
            .unwrap()
            .contains("refused the key")
    );
    assert!(check["remediation"].as_str().unwrap().contains("Clear"));

    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/storage/probe/clear",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "off", "cleared; the flag is still off here");
    assert!(body["clearedAt"].is_string());
    assert!(
        body["clearedBy"].as_str().is_some_and(|by| !by.is_empty()),
        "{body}"
    );
    assert_eq!(
        body["reason"], "the storage host refused the key of user skadi",
        "the reason stays as a record"
    );
}

#[tokio::test]
async fn the_probe_endpoints_refuse_a_non_admin() {
    use skadi_store::StorageProbeRepo;
    let (state, db) = seeded().await;
    db.store
        .fail_storage_probe("pieces", "host key changed", t(5))
        .await
        .unwrap();
    let mut tokens = Vec::new();
    for (name, role) in [
        ("Robin", "member"),
        ("Sam", "contributor"),
        ("Jamie", "kid"),
    ] {
        let (st, created) = call(
            &state,
            "POST",
            "/api/v1/members",
            "operator-token",
            Some(serde_json::json!({"name": name, "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        tokens.push((role, created["token"].as_str().unwrap().to_string()));
    }
    let credential = serde_json::json!({"kind": "password", "secret": SECRET});
    for (role, token) in &tokens {
        for (method, path, body) in [
            ("GET", "/api/v1/storage/probe", None),
            ("POST", "/api/v1/storage/probe/clear", None),
            (
                "PUT",
                "/api/v1/storage/probe/credential",
                Some(credential.clone()),
            ),
            ("DELETE", "/api/v1/storage/probe/credential", None),
        ] {
            let (st, answer) = call(&state, method, path, token, body).await;
            assert_eq!(
                st,
                StatusCode::FORBIDDEN,
                "{method} {path} as {role}: {answer}"
            );
        }
    }
    // None of it happened: still failed, no credential.
    let (_, body) = call(
        &state,
        "GET",
        "/api/v1/storage/probe",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(body["state"], "failed");
    assert!(body["credential"].is_null());
}

// ---------------------------------------------------------------------------
// History (SKADI-T-0776)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_history_gives_a_row_per_day_and_kind_and_a_file_gives_its_samples() {
    let (state, db) = seeded().await;
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let last_week = (Utc::now() - Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();
    let long_ago = (Utc::now() - Duration::days(400))
        .format("%Y-%m-%d")
        .to_string();
    for day in [&long_ago, &last_week, &today] {
        db.store
            .record_library_daily_totals(day, Utc::now())
            .await
            .unwrap();
    }
    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/library/storage/history",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let days = body["days"].as_array().unwrap();
    assert_eq!(
        days.len(),
        6,
        "two days in the last 90, three kinds each: {body}"
    );
    assert_eq!(days[0]["day"], last_week.as_str(), "oldest first");
    assert_eq!(days[0]["kind"], "audiobook");
    assert_eq!(days[1]["kind"], "movie");
    assert_eq!(days[1]["sizeBytes"], 14 * GB);
    assert_eq!(days[5]["day"], today.as_str());

    let (_, body) = call(
        &state,
        "GET",
        "/api/v1/library/storage/history?days=1",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(body["days"].as_array().unwrap().len(), 3, "today only");
    let (_, body) = call(
        &state,
        "GET",
        "/api/v1/library/storage/history?days=100000",
        "operator-token",
        None,
    )
    .await;
    assert_eq!(
        body["days"].as_array().unwrap().len(),
        9,
        "capped at two years: {body}"
    );

    // One file: its changes, oldest first.
    let (_, page) = call(
        &state,
        "GET",
        "/api/v1/library/storage/files?sort=links&limit=1",
        "operator-token",
        None,
    )
    .await;
    let id = page["files"][0]["id"].as_str().unwrap().to_string();
    db.store
        .record_library_file_pieces(&id, 321, t(9))
        .await
        .unwrap();
    let (st, samples) = call(
        &state,
        "GET",
        &format!("/api/v1/library/storage/files/{id}/samples"),
        "operator-token",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{samples}");
    let samples = samples.as_array().unwrap();
    assert_eq!(samples.len(), 2);
    assert!(samples[0]["pieces"].is_null());
    assert_eq!(samples[1]["pieces"], 321);
    assert_eq!(samples[1]["sizeBytes"], 12 * GB);

    // A non-admin gets neither.
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        "operator-token",
        Some(serde_json::json!({"name": "Robin", "role": "member"})),
    )
    .await;
    let member = created["token"].as_str().unwrap().to_string();
    for path in [
        "/api/v1/library/storage/history".to_string(),
        format!("/api/v1/library/storage/files/{id}/samples"),
    ] {
        let (st, _) = call(&state, "GET", &path, &member, None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{path}");
    }
}
