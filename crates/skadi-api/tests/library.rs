//! Library + activity endpoint tests (SKADI-T-0055).

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{
    AppState, Config, DomainDescriptor, LibraryEditionDto, LibraryItemDto, LibraryProvider,
};
use skadi_core::{MediaKind, Result as SkadiResult};
use skadi_store::DomainStateRepo;
use skadi_testsupport::TestDb;

fn item(id: &str, title: &str, monitored: bool) -> LibraryItemDto {
    LibraryItemDto {
        kind: "movie".into(),
        id: id.into(),
        title: title.into(),
        year: Some(1999),
        monitored,
        editions: vec![LibraryEditionDto {
            id: format!("{id}-ed"),
            kind: "theatrical".into(),
            kind_name: None,
            status_kind: "missing".into(),
            monitored: true,
            quality: None,
            quality_name: None,
            media_info: None,
        }],
    }
}

struct FakeLib;

#[async_trait]
impl LibraryProvider for FakeLib {
    fn domain(&self) -> &str {
        "movies"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    async fn items(&self, monitored: Option<bool>) -> SkadiResult<Vec<LibraryItemDto>> {
        let all = vec![
            item("m1", "The Matrix", true),
            item("m2", "Old Yeller", false),
        ];
        Ok(match monitored {
            Some(m) => all.into_iter().filter(|i| i.monitored == m).collect(),
            None => all,
        })
    }
}

async fn state(enabled: bool) -> (Arc<AppState>, TestDb) {
    state_with(enabled, vec![Arc::new(FakeLib)]).await
}

async fn state_with(
    enabled: bool,
    providers: Vec<Arc<dyn LibraryProvider>>,
) -> (Arc<AppState>, TestDb) {
    // Postgres-default isolated DB (SQLite fallback). Caller keeps the `TestDb`
    // guard alive for the test's duration (SKADI-T-0077).
    let db = TestDb::new_store_only().await;
    if enabled {
        db.store.set_enabled("movies", true).await.unwrap();
    }
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let state = AppState::new_full(
        config,
        Some(db.store.clone()),
        vec![DomainDescriptor {
            name: "movies".into(),
            kind: MediaKind::Movie,
        }],
        providers,
    );
    (state, db)
}

/// A library with editions in a spread of statuses, for the `/wanted` view.
struct WantedLib;

fn ed(status: &str) -> LibraryEditionDto {
    LibraryEditionDto {
        id: format!("ed-{status}"),
        kind: "theatrical".into(),
        kind_name: None,
        status_kind: status.into(),
        monitored: true,
        quality: None,
        quality_name: None,
        media_info: None,
    }
}

fn item_eds(id: &str, title: &str, monitored: bool, eds: Vec<LibraryEditionDto>) -> LibraryItemDto {
    LibraryItemDto {
        kind: "movie".into(),
        id: id.into(),
        title: title.into(),
        year: Some(2020),
        monitored,
        editions: eds,
    }
}

#[async_trait]
impl LibraryProvider for WantedLib {
    fn domain(&self) -> &str {
        "movies"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    async fn items(&self, monitored: Option<bool>) -> SkadiResult<Vec<LibraryItemDto>> {
        let all = vec![
            // wanted: one missing edition (the imported one is dropped).
            item_eds(
                "m1",
                "The Matrix",
                true,
                vec![ed("missing"), ed("imported")],
            ),
            // satisfied: fully imported → not wanted.
            item_eds("m2", "Done Movie", true, vec![ed("imported")]),
            // wanted: a failed (retrying) edition.
            item_eds("m3", "Flaky Film", true, vec![ed("failed")]),
            // not wanted: unmonitored, even though it's missing.
            item_eds("m4", "Unwatched", false, vec![ed("missing")]),
            // satisfied: cutoff met.
            item_eds("m5", "Peak Film", true, vec![ed("cutoff")]),
        ];
        Ok(match monitored {
            Some(m) => all.into_iter().filter(|i| i.monitored == m).collect(),
            None => all,
        })
    }
}

async fn call(state: &Arc<AppState>, uri: &str) -> (StatusCode, serde_json::Value) {
    call_method(state, "GET", uri).await
}

/// Like [`call_method`] but also returns the response headers (for `X-Total-Count`).
async fn call_full(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let res = skadi_api::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, headers, json)
}

async fn call_method(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
) -> (StatusCode, serde_json::Value) {
    let res = skadi_api::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn library_aggregates_enabled_domains() {
    let (state, _db) = state(true).await;
    let (s, body) = call(&state, "/api/v1/library").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2);
    let first = &body[0];
    assert_eq!(first["kind"], "movie");
    assert_eq!(first["editions"][0]["status_kind"], "missing");
}

#[tokio::test]
async fn disabled_domain_contributes_nothing() {
    let (state, _db) = state(false).await;
    let (s, body) = call(&state, "/api/v1/library").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn library_filters_monitored_and_query_and_kind() {
    let (state, _db) = state(true).await;

    let (_, mon) = call(&state, "/api/v1/library?monitored=true").await;
    assert_eq!(mon.as_array().unwrap().len(), 1);
    assert_eq!(mon[0]["title"], "The Matrix");

    let (_, q) = call(&state, "/api/v1/library?q=yeller").await;
    assert_eq!(q.as_array().unwrap().len(), 1);
    assert_eq!(q[0]["title"], "Old Yeller");

    // Wrong kind → empty; right kind → all.
    let (_, none) = call(&state, "/api/v1/library?kind=series").await;
    assert_eq!(none.as_array().unwrap().len(), 0);
    let (_, all) = call(&state, "/api/v1/library?kind=movie").await;
    assert_eq!(all.as_array().unwrap().len(), 2);

    // Pagination.
    let (_, page) = call(&state, "/api/v1/library?limit=1&offset=1").await;
    assert_eq!(page.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn wanted_lists_only_unsatisfied_monitored_editions_with_a_summary() {
    let (state, _db) = state_with(true, vec![Arc::new(WantedLib)]).await;
    let (s, body) = call(&state, "/api/v1/wanted").await;
    assert_eq!(s, StatusCode::OK);

    // m1 (missing) and m3 (failed) are wanted; m2/m5 satisfied, m4 unmonitored.
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "two wanted items: {body}");
    let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"The Matrix"));
    assert!(titles.contains(&"Flaky Film"));

    // m1's imported edition is dropped — only the missing one remains.
    let m1 = items.iter().find(|i| i["title"] == "The Matrix").unwrap();
    assert_eq!(m1["editions"].as_array().unwrap().len(), 1);
    assert_eq!(m1["editions"][0]["status_kind"], "missing");

    // Summary tallies the whole wanted set.
    assert_eq!(body["summary"]["items"], 2);
    assert_eq!(body["summary"]["editions"], 2);
    assert_eq!(body["summary"]["by_status"]["missing"], 1);
    assert_eq!(body["summary"]["by_status"]["failed"], 1);
    assert!(body["summary"]["by_status"].get("imported").is_none());
}

#[tokio::test]
async fn wanted_honors_kind_query_and_pagination() {
    let (state, _db) = state_with(true, vec![Arc::new(WantedLib)]).await;

    // Wrong kind → no items (summary zero).
    let (_, none) = call(&state, "/api/v1/wanted?kind=series").await;
    assert_eq!(none["items"].as_array().unwrap().len(), 0);
    assert_eq!(none["summary"]["items"], 0);

    // q filters by title.
    let (_, q) = call(&state, "/api/v1/wanted?q=flaky").await;
    assert_eq!(q["items"].as_array().unwrap().len(), 1);
    assert_eq!(q["items"][0]["title"], "Flaky Film");
    // Summary reflects the post-q set.
    assert_eq!(q["summary"]["items"], 1);

    // Pagination over the wanted items; summary still counts the full set.
    let (_, page) = call(&state, "/api/v1/wanted?limit=1&offset=1").await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["summary"]["items"], 2);
}

#[tokio::test]
async fn wanted_is_empty_for_a_disabled_domain() {
    let (state, _db) = state_with(false, vec![Arc::new(WantedLib)]).await;
    let (s, body) = call(&state, "/api/v1/wanted").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["summary"]["items"], 0);
}

#[tokio::test]
async fn search_all_accepts_and_pokes_the_sweep_trigger() {
    let (state, _db) = state(true).await;

    // A subscriber stands in for a running worker: it must observe the poke.
    let watcher = skadi_hunter::trigger::subscribe();
    assert!(
        !watcher.has_changed().unwrap(),
        "no request before the call"
    );

    let (s, body) = call_method(&state, "POST", "/api/v1/search-all").await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "sweep requested");

    // The endpoint bumped the trigger every worker watches.
    assert!(
        watcher.has_changed().unwrap(),
        "POST /search-all should poke the sweep trigger"
    );
}

#[tokio::test]
async fn history_endpoint_filters_paginates_reports_total_and_counts() {
    use skadi_store::{HistoryEntry, HistoryRepo};

    let (state, db) = state(true).await;
    let mk = |id: &str, secs: i64, kind: &str, ev: &str| HistoryEntry {
        id: id.into(),
        at: chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap(),
        kind: kind.into(),
        acquirable_ref: "ref-1".into(),
        label: id.into(),
        event: ev.into(),
        detail: None,
        reason_code: None,
    };
    for e in [
        mk("g1", 0, "movie", "grabbed"),
        mk("i1", 10, "movie", "imported"),
        mk("f1", 20, "audiobook", "failed"),
    ] {
        db.store.record_history(&e).await.unwrap();
    }

    // Filter by event → one row, total header reflects the filtered count.
    let (s, hdrs, body) = call_full(&state, "GET", "/api/v1/history?event=imported").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["event"], "imported");
    assert_eq!(hdrs.get("x-total-count").unwrap(), "1");

    // Filter by kind.
    let (_, _, body) = call_full(&state, "GET", "/api/v1/history?kind=audiobook").await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], "f1");

    // No filter → all three; total header = 3.
    let (_, hdrs, body) = call_full(&state, "GET", "/api/v1/history").await;
    assert_eq!(body.as_array().unwrap().len(), 3);
    assert_eq!(hdrs.get("x-total-count").unwrap(), "3");

    // Pagination caps the page but the total stays the full match count.
    let (_, hdrs, body) = call_full(&state, "GET", "/api/v1/history?limit=1").await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(hdrs.get("x-total-count").unwrap(), "3");
    assert_eq!(body[0]["id"], "f1", "newest first");

    // A malformed timestamp is a 400, not a silent ignore.
    let (s, _, _) = call_full(&state, "GET", "/api/v1/history?since=not-a-date").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Counts endpoint tallies by event.
    let (s, body) = call(&state, "/api/v1/history/counts").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["total"], 3);
    assert_eq!(body["grabbed"], 1);
    assert_eq!(body["imported"], 1);
    assert_eq!(body["failed"], 1);

    let _ = db; // hold the TestDb guard to the end
}

#[tokio::test]
async fn history_detail_and_blocklist_and_search() {
    use skadi_store::{
        BlocklistRepo, DecisionEntry, DecisionHistoryRepo, HistoryEntry, HistoryRepo,
    };

    let (state, db) = state(true).await;

    // A failed history row for an item, plus the decision that recorded the
    // grabbed release's stable key.
    db.store
        .record_history(&HistoryEntry {
            id: "h1".into(),
            at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            kind: "movie".into(),
            acquirable_ref: "ed-7".into(),
            label: "The Matrix".into(),
            event: "failed".into(),
            detail: Some("ImportFailed".into()),
            reason_code: Some("import_failed".into()),
        })
        .await
        .unwrap();
    db.store
        .record_decision(&DecisionEntry {
            id: "d1".into(),
            at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            kind: "movie".into(),
            acquirable_ref: "ed-7".into(),
            title: "The Matrix 1080p".into(),
            quality: Some("Bluray-1080p".into()),
            decision: Some("Accept".into()),
            format_score: 0,
            explanation: "{}".into(),
            release_key: Some("btih:dead".into()),
        })
        .await
        .unwrap();

    // Detail: found, with the structured reason code.
    let (s, body) = call(&state, "/api/v1/history/h1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["id"], "h1");
    assert_eq!(body["acquirable_ref"], "ed-7");
    assert_eq!(body["event"], "failed");
    assert_eq!(body["reason_code"], "import_failed");

    // The list filters on the structured reason code.
    let (_, _, filtered) =
        call_full(&state, "GET", "/api/v1/history?reason_code=import_failed").await;
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    assert_eq!(filtered[0]["id"], "h1");
    let (_, _, none) =
        call_full(&state, "GET", "/api/v1/history?reason_code=download_failed").await;
    assert_eq!(none.as_array().unwrap().len(), 0);

    // Detail: 404 for unknown id.
    let (s, _) = call(&state, "/api/v1/history/nope").await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Blocklist-and-search: blocklists the grabbed release_key + reports a sweep.
    let (s, body) = call_method(&state, "POST", "/api/v1/history/h1/blocklist-and-search").await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(body["blocklisted"], true);
    assert_eq!(body["release_key"], "btih:dead");
    assert_eq!(body["sweep_requested"], true);

    // The release is now actually on the blocklist.
    assert!(db.store.is_blocked("btih:dead").await.unwrap());

    // For an item with no decision on record, it still kicks the sweep but
    // blocklists nothing.
    db.store
        .record_history(&HistoryEntry {
            id: "h2".into(),
            at: chrono::DateTime::from_timestamp(1_700_000_100, 0).unwrap(),
            kind: "movie".into(),
            acquirable_ref: "ed-none".into(),
            label: "No Decision".into(),
            event: "failed".into(),
            detail: None,
            reason_code: None,
        })
        .await
        .unwrap();
    let (s, body) = call_method(&state, "POST", "/api/v1/history/h2/blocklist-and-search").await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(body["blocklisted"], false);
    assert_eq!(body["sweep_requested"], true);

    let _ = db;
}

#[tokio::test]
async fn activity_reflects_the_in_flight_tracker() {
    let (state, _db) = state(true).await;

    let key = "ref-activity-test-0055";
    skadi_hunter::tracker().finish(key);

    let (s, before) = call(&state, "/api/v1/activity").await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !before
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["acquirable_ref"] == key)
    );

    skadi_hunter::tracker().start("run-xyz", MediaKind::Movie, key);
    let (_, after) = call(&state, "/api/v1/activity").await;
    let entry = after
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["acquirable_ref"] == key)
        .expect("seeded run present");
    assert_eq!(entry["run_id"], "run-xyz");
    assert_eq!(entry["current_stage"], "running");
    assert!(entry["started_at"].is_string());

    skadi_hunter::tracker().finish(key);
}

/// Every list endpoint the UI renders carries the paging envelope
/// (SKADI-T-0494 / SKADI-T-0468).
///
/// The ticket recorded "only `/history` sends X-Total-Count", found by P6. That
/// is no longer true — all of them do — and this pins it, so a list endpoint
/// added without paging fails here rather than being discovered when a page
/// takes seconds on a prod-sized library.
///
/// `X-Total-Count` is the whole point: without it a client cannot render page
/// controls, so it pages blind and either shows no pager or a wrong one.
#[tokio::test]
async fn every_list_endpoint_sends_a_total_count() {
    let (state, _db) = state(true).await;
    for uri in [
        "/api/v1/library",
        "/api/v1/wanted",
        "/api/v1/history",
        "/api/v1/traces",
        "/api/v1/decisions",
        "/api/v1/downloads",
        "/api/v1/blocklist",
    ] {
        let (status, headers, _body) = call_full(&state, "GET", uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(
            headers.contains_key("x-total-count"),
            "{uri} has no X-Total-Count, so a client cannot page it"
        );
    }
}

/// The bound must reach the response, not just be accepted and ignored.
#[tokio::test]
async fn a_limit_actually_limits() {
    let (state, _db) = state(true).await;
    for uri in [
        "/api/v1/library?limit=1",
        "/api/v1/wanted?limit=1",
        "/api/v1/history?limit=1",
        "/api/v1/traces?limit=1",
        "/api/v1/decisions?limit=1",
        "/api/v1/downloads?limit=1",
    ] {
        let (status, _h, body) = call_full(&state, "GET", uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        // The shape differs per endpoint (bare array vs `{items: []}`), so count
        // whichever array the response actually carries.
        let n = body
            .as_array()
            .map(Vec::len)
            .or_else(|| body.get("items").and_then(|i| i.as_array()).map(Vec::len))
            .unwrap_or_else(|| panic!("{uri}: no array in {body}"));
        assert!(n <= 1, "{uri} returned {n} rows for limit=1");
    }
}

/// Each `/downloads` row says when it was enqueued, in a fixed-width UTC form
/// that sorts as a string — the web "Added" sort compares it lexically
/// (SKADI-T-0686).
#[tokio::test]
async fn each_download_row_carries_a_sortable_created_at() {
    use skadi_store::{DownloadJobRepo, NewDownloadJob};
    let (state, db) = state(true).await;
    let job = db
        .store
        .enqueue(&NewDownloadJob {
            acquirable_ref: "ref-added-0686".into(),
            source: "magnet:?xt=urn:btih:0686".into(),
            category: None,
            incomplete_dir: None,
            complete_dir: None,
        })
        .await
        .unwrap();

    let (status, body) = call(&state, "/api/v1/downloads").await;
    assert_eq!(status, StatusCode::OK);
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == job.id.as_str())
        .expect("the enqueued job is listed");
    let created = row["created_at"].as_str().expect("created_at is a string");
    assert_eq!(
        created,
        job.created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    // `YYYY-MM-DDTHH:MM:SS.mmmZ`: always 24 characters, always UTC.
    assert_eq!(created.len(), 24, "{created}");
    assert!(created.ends_with('Z'), "{created}");
}

/// `/downloads` lists recent failures next to the live transfers, with their
/// message, but bounded: at most 50, the most recently updated ones
/// (SKADI-T-0687). The 7-day age bound is pinned in skadi-store
/// (`recent_errored_downloads_are_bounded_by_age_and_count`).
#[tokio::test]
async fn downloads_lists_the_recent_failures_bounded_to_fifty() {
    use skadi_store::{DownloadJobRepo, NewDownloadJob};
    let (state, db) = state(true).await;
    let live = db
        .store
        .enqueue(&NewDownloadJob {
            acquirable_ref: "ref-live-0687".into(),
            source: "magnet:?xt=urn:btih:live0687".into(),
            category: None,
            incomplete_dir: None,
            complete_dir: None,
        })
        .await
        .unwrap();
    let mut failed = Vec::new();
    for i in 0..55 {
        let job = db
            .store
            .enqueue(&NewDownloadJob {
                acquirable_ref: format!("ref-err-0687-{i}"),
                source: format!("magnet:?xt=urn:btih:err0687{i}"),
                category: None,
                incomplete_dir: None,
                complete_dir: None,
            })
            .await
            .unwrap();
        db.store
            .mark_error(&job.id, &format!("tracker said no ({i})"))
            .await
            .unwrap();
        failed.push(db.store.get_download(&job.id).await.unwrap().unwrap());
    }
    // The store's order: newest update first, ties by id.
    failed.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    let expected: std::collections::BTreeSet<String> =
        failed.iter().take(50).map(|j| j.id.clone()).collect();

    let (status, body) = call(&state, "/api/v1/downloads").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert!(rows.iter().any(|r| r["id"] == live.id.as_str()));
    let errored: Vec<&serde_json::Value> = rows.iter().filter(|r| r["status"] == "error").collect();
    assert_eq!(errored.len(), 50, "at most 50 failures are served");
    let served: std::collections::BTreeSet<String> = errored
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(served, expected, "the 50 most recent failures");
    for r in &errored {
        let msg = r["error"].as_str().expect("an errored row has its message");
        assert!(msg.starts_with("tracker said no"), "{msg}");
    }
}
