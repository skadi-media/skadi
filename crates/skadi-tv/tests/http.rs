//! HTTP-level tests for television's manual acquire surface (SKADI-T-0558).
//!
//! Mirrors the movies harness. Before this ticket the TV crate had no HTTP tests
//! at all, because it had no manual endpoints to test.

use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::sync::Mutex;
use tower::ServiceExt;

use skadi_api::HttpModule;
use skadi_core::{AcquisitionStatus, Result as SkadiResult, SeriesId, TvdbId};
use skadi_metadata::{MetadataMatch, MetadataQuery, SeriesMetadata, SeriesMetadataProvider};
use skadi_store::{DomainState, DomainStateRepo, Store};
use skadi_testsupport::TestDb;
use skadi_tv::{POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS, TelevisionHttp, TvRepo};

struct FakeProvider;

#[async_trait]
impl SeriesMetadataProvider for FakeProvider {
    async fn search_series(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup_series(&self, _tvdb: TvdbId) -> SkadiResult<SeriesMetadata> {
        Err(skadi_core::AppError::NotFound(
            "no metadata in tests".into(),
        ))
    }
}

struct Harness {
    http: TelevisionHttp,
    store: Store,
    _db: TestDb,
}

/// Same reason as the movies harness: on Postgres the Cloacina runner creates a
/// shared schema, and concurrent first-time `CREATE SCHEMA` calls race.
static RUNNER_SETUP: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

async fn harness() -> Harness {
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    let runner = {
        let _guard = RUNNER_SETUP.lock().await;
        Arc::new(skadi_hunter::build_runner(db.url()).await.unwrap())
    };
    let provider: Arc<dyn SeriesMetadataProvider> = Arc::new(FakeProvider);
    Harness {
        http: TelevisionHttp::new(store.clone(), provider, runner),
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

/// Enable or disable the television domain.
async fn set_domain(store: &Store, enabled: bool) {
    store
        .upsert(&DomainState {
            name: "television".into(),
            enabled,
            enabled_at: enabled.then(chrono::Utc::now),
            settings: serde_json::json!({}),
        })
        .await
        .unwrap();
}

/// A series with one episode, and the television domain enabled.
async fn seed(store: &Store) -> (SeriesId, skadi_core::EpisodeId) {
    set_domain(store, true).await;
    let mut series = skadi_tv::Series::new(
        skadi_core::ExternalIds {
            tvdb: Some(TvdbId(1234)),
            ..Default::default()
        },
        "Severance",
        skadi_core::ProfileId::new(),
        skadi_core::RootFolder::new("/library/television"),
    );
    series.year = Some(2022);
    store.upsert_series(&series).await.unwrap();
    let ep = skadi_tv::Episode::missing(series.id, 1, 1);
    store.upsert_episode(&ep).await.unwrap();
    (series.id, ep.id)
}

#[tokio::test]
async fn an_episode_of_another_series_is_not_found() {
    // Episode ids are opaque, so without the ownership check
    // `/series/{a}/episodes/{b}` would act on an episode of a different series
    // and the operator would see the effect on a page they were not looking at.
    let h = harness().await;
    let (_series, episode) = seed(&h.store).await;
    let other = SeriesId::new();
    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!("/series/{other}/episodes/{episode}/reset"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn reset_clears_a_wedged_episode_and_reports_what_it_was() {
    let h = harness().await;
    let (series, episode) = seed(&h.store).await;
    // Wedge it, the way a lost acquire run leaves an episode.
    h.store
        .set_episode_status(
            episode,
            AcquisitionStatus::Searching {
                since: chrono::Utc::now(),
                attempts: 1,
            },
        )
        .await
        .unwrap();

    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!("/series/{series}/episodes/{episode}/reset"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["reset"], true);
    assert!(
        body["previous"].as_str().unwrap().contains("Searching"),
        "the response says what was cleared, so the operator can tell a real \
         recovery from a no-op: {body}"
    );

    let after = h.store.get_episode(episode).await.unwrap().unwrap();
    assert!(matches!(after.status, AcquisitionStatus::Missing));
}

#[tokio::test]
async fn reset_is_idempotent() {
    // Resetting an already-Missing episode is the operator double-clicking a
    // recovery button; it must not error.
    let h = harness().await;
    let (series, episode) = seed(&h.store).await;
    for _ in 0..2 {
        let (s, _) = call(
            h.http.routes(),
            "POST",
            &format!("/series/{series}/episodes/{episode}/reset"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
}

#[tokio::test]
async fn acquire_is_refused_while_a_fresh_run_is_in_flight() {
    let h = harness().await;
    let (series, episode) = seed(&h.store).await;
    h.store
        .set_episode_status(
            episode,
            AcquisitionStatus::Downloading {
                release: skadi_core::ReleaseId::new(),
                progress: 0.5,
            },
        )
        .await
        .unwrap();

    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!("/series/{series}/episodes/{episode}/acquire"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "already_in_flight");
}

#[tokio::test]
async fn acquire_is_refused_while_the_domain_is_disabled() {
    let h = harness().await;
    let (series, episode) = seed(&h.store).await;
    set_domain(&h.store, false).await;

    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!("/series/{series}/episodes/{episode}/acquire"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "domain_disabled");
}

#[tokio::test]
async fn grab_link_rejects_something_that_is_not_a_link() {
    let h = harness().await;
    let (series, episode) = seed(&h.store).await;
    let (s, body) = call(
        h.http.routes(),
        "POST",
        &format!("/series/{series}/episodes/{episode}/grab-link"),
        Some(serde_json::json!({ "link": "not a magnet" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
}

/// `?view=summary` must ship less **without changing what the library wall
/// reads** (SKADI-T-0494).
///
/// `/series` was the API's worst payload — 17.5 MB on a prod-sized library —
/// because every episode carried its title, air date, scene numbers, file path,
/// media info and quality so the TV page could render a status badge from two of
/// those fields.
#[tokio::test]
async fn the_summary_view_keeps_what_the_wall_reads_and_drops_the_rest() {
    let h = harness().await;
    let (_series, episode) = seed(&h.store).await;
    // A **data-carrying** status on purpose. With a unit variant (`Missing`) the
    // full and slim forms serialise identically, so the status assertion below
    // would pass without exercising anything — which is exactly what it did
    // before this line existed.
    h.store
        .set_episode_status(
            episode,
            skadi_core::AcquisitionStatus::Failed {
                attempts: 3,
                reason: skadi_core::FailureReason::NoSuitableRelease,
                retry_at: Some(chrono::Utc::now()),
            },
        )
        .await
        .unwrap();

    let (s_full, full) = call(h.http.routes(), "GET", "/series", None).await;
    let (s_slim, slim) = call(h.http.routes(), "GET", "/series?view=summary", None).await;
    assert_eq!(s_full, StatusCode::OK);
    assert_eq!(s_slim, StatusCode::OK);

    let fe = full[0]["episodes"][0].as_object().expect("full episode");
    let se = slim[0]["episodes"][0].as_object().expect("slim episode");

    // `series_lib_status` in skadi-web reads exactly these two per episode.
    // If either disappears the badge silently changes meaning, which is the
    // failure this projection must not cause.
    // `id` too: clients type an episode as having one, so dropping it would make
    // this a different type rather than a smaller one.
    for key in ["season", "id", "number"] {
        assert_eq!(
            se.get(key),
            fe.get(key),
            "`{key}` must survive the projection"
        );
    }

    // `status` survives as its variant *name* — all `status_label` reads, and a
    // shape that helper already accepts. The nested payload (attempts, retry_at,
    // file, quality) is ~110 bytes an episode that no library list renders.
    let slim_status = se
        .get("status")
        .and_then(|v| v.as_str())
        .expect("the slim status is a bare label");
    let full_status = fe.get("status").expect("full status");
    assert!(
        full_status.is_object(),
        "the fixture must carry a data-bearing status or this proves nothing: {full_status}"
    );
    let full_label = full_status
        .as_object()
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_default();
    assert_eq!(
        slim_status, full_label,
        "the label must be the real variant name"
    );
    // `monitored` is read by the season toggles on the same page.
    assert!(se.contains_key("monitored"));

    // The weight is gone.
    for key in ["file", "media_info", "title", "air_date"] {
        assert!(
            !se.contains_key(key),
            "`{key}` is never read by a library list and must not be shipped"
        );
    }
    assert!(
        se.len() < fe.len(),
        "the projection must be smaller: slim={} full={}",
        se.len(),
        fe.len()
    );

    // Series-level fields are untouched — the page renders title, poster, year
    // and the monitored flag from them.
    assert_eq!(slim[0]["title"], full[0]["title"]);
    assert_eq!(slim[0]["id"], full[0]["id"]);
    assert_eq!(slim[0]["monitored"], full[0]["monitored"]);

    // Seasons stay whole: tens of them, not thousands.
    assert_eq!(slim[0]["seasons"], full[0]["seasons"]);
}

/// An unknown or absent `view` returns the full shape, so an old client that
/// knows nothing about this parameter is unaffected.
#[tokio::test]
async fn an_unknown_view_returns_the_full_shape() {
    let h = harness().await;
    let (_series, _episode) = seed(&h.store).await;
    let (_, full) = call(h.http.routes(), "GET", "/series", None).await;
    let (_, odd) = call(h.http.routes(), "GET", "/series?view=nonsense", None).await;
    assert_eq!(odd, full, "an unrecognised view must not silently trim");
}
