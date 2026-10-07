//! Health-checks + root-folder diagnostics tests (SKADI-T-0116).
//!
//! `GET /health/checks` aggregates daemon/db/domain/provider checks — verified
//! incl. a dead provider reporting `fail` and a disabled domain reporting
//! `warn`. `GET /root-folders` reports free/total bytes + writability for a
//! configured root folder.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config, DomainDescriptor};
use skadi_core::MediaKind;
use skadi_testsupport::TestDb;

async fn state_with_domains(domains: Vec<DomainDescriptor>) -> (Arc<AppState>, TestDb) {
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    (
        AppState::new_with_domains(config, Some(db.store.clone()), domains),
        db,
    )
}

async fn call(
    state: &Arc<AppState>,
    method_s: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let builder = Request::builder().method(method_s).uri(uri);
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = skadi_api::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn find<'a>(checks: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    checks.as_array()?.iter().find(|c| c["name"] == name)
}

#[tokio::test]
async fn health_checks_cover_daemon_db_domain_and_a_dead_provider() {
    let (state, _db) = state_with_domains(vec![DomainDescriptor {
        name: "movies".into(),
        kind: MediaKind::Movie,
    }])
    .await;

    // A configured indexer pointing at a dead URL → its check should fail.
    let (s, _created) = call(
        &state,
        "POST",
        "/api/v1/settings/indexers",
        Some(serde_json::json!({
            "kind": "torznab",
            "name": "deadixr",
            "base_url": "http://127.0.0.1:1",
            "categories": [2000],
            "api_key": "k"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    // GET serves the stored results (SKADI-T-0680): run the checks first.
    let (s, _) = call(&state, "POST", "/api/v1/health/checks/run", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, checks) = call(&state, "GET", "/api/v1/health/checks", None).await;
    assert_eq!(s, StatusCode::OK);

    // Daemon version is always ok and names the version.
    let daemon = find(&checks, "daemon").expect("daemon check present");
    assert_eq!(daemon["status"], "ok");
    assert!(
        daemon["detail"]
            .as_str()
            .is_some_and(|d| d.contains("skadi")),
        "daemon detail names the build: {daemon:?}"
    );

    // Database reachable.
    assert_eq!(find(&checks, "database").expect("db check")["status"], "ok");

    // A disabled domain is NOT surfaced in Health (matches the sidebar/dashboard,
    // which list enabled domains only).
    assert!(
        find(&checks, "domain:movies").is_none(),
        "disabled domain omitted from health checks"
    );

    // The dead indexer is reported as a failing check.
    let ixr = find(&checks, "indexer:deadixr").expect("provider check");
    assert_eq!(ixr["status"], "fail");
    assert!(
        ixr["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "fail carries a reason: {ixr:?}"
    );

    // Enabling the domain flips its check to ok.
    let (s, _) = call(
        &state,
        "PUT",
        "/api/v1/domains/movies",
        Some(serde_json::json!({ "enabled": true })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // A newly enabled domain is listed at once, pending until it runs.
    let (_, checks) = call(&state, "GET", "/api/v1/health/checks", None).await;
    assert!(find(&checks, "domain:movies").unwrap()["checked_at"].is_null());
    let (_, checks) = call(
        &state,
        "POST",
        "/api/v1/health/checks/run?id=domain:movies",
        None,
    )
    .await;
    assert_eq!(find(&checks, "domain:movies").unwrap()["status"], "ok");
}

#[tokio::test]
async fn root_folders_report_free_space_and_writability() {
    use skadi_store::{ConfigRepo, ConfigSource};

    let (state, db) = state_with_domains(vec![]).await;
    let dir = tempfile::tempdir().unwrap();

    // Single library root (SKADI-T-0302): the report covers `library.root`, which
    // skadi owns — there is no operator root list to POST.
    db.store
        .set_config(
            "library.root",
            &dir.path().display().to_string(),
            ConfigSource::Runtime,
        )
        .await
        .unwrap();

    let (s, reports) = call(&state, "GET", "/api/v1/root-folders", None).await;
    assert_eq!(s, StatusCode::OK);
    let arr = reports.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    let r = &arr[0];
    assert_eq!(r["path"], dir.path().display().to_string());
    assert_eq!(r["exists"], true);
    assert_eq!(r["writable"], true);
    assert!(
        r["total_bytes"].as_u64().is_some_and(|t| t > 0),
        "total bytes positive: {r:?}"
    );
    assert!(
        r["free_bytes"].as_u64().is_some(),
        "free bytes present: {r:?}"
    );
}
