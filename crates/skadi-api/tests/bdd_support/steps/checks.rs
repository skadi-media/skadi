//! Health check model, cache, warning and config-sanity steps (C34,
//! COLLIERY-I-0294) for `C34-health/checks.feature`.
//!
//! The fixtures that the code of today can build are real: fake providers and a
//! fake gluetun (wiremock), a backdated worker heartbeat, recorded domain-worker
//! failures. The fixtures that still need a seam the code does not have panic and name
//! the task that adds it (`todo_seam`).
//!
//! Checks are looked up by `id`, falling back to `name` (the pre-T-0679 field), in
//! a bare array or in an object with a `checks` array.
use std::time::Duration;

use cucumber::{given, then, when};
use diesel::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use skadi_store::{ConfigRepo, SettingsRepo, WorkerStatusRepo};

use crate::bdd_support::{EnvGuard, Fake, World};

// ---- helpers -------------------------------------------------------------------

/// The check list of the last reply.
fn checks(w: &World) -> Vec<serde_json::Value> {
    let r = w.reply();
    let list = r.json.as_array().or_else(|| r.json["checks"].as_array());
    list.cloned()
        .unwrap_or_else(|| panic!("the reply is not a list of health checks: {}", r.text))
}

/// The check `id` of the last reply (matched on `id`, then on `name`).
fn check(w: &World, id: &str) -> serde_json::Value {
    checks(w)
        .into_iter()
        .find(|c| c["id"] == id || (c.get("id").is_none() && c["name"] == id))
        .unwrap_or_else(|| panic!("no health check {id:?} in {}", w.reply().text))
}

fn text<'a>(check: &'a serde_json::Value, field: &str) -> &'a str {
    check[field]
        .as_str()
        .unwrap_or_else(|| panic!("health check has no string {field:?}: {check}"))
}

fn checked_at(check: &serde_json::Value) -> chrono::DateTime<chrono::Utc> {
    let raw = text(check, "checked_at");
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("checked_at {raw:?} is not RFC 3339: {e}"))
        .with_timezone(&chrono::Utc)
}

/// A fixture that needs a seam the code does not have yet. The task that turns
/// the scenario `@passing` replaces the panic with the real fixture.
fn todo_seam(task: &str, what: &str) -> ! {
    panic!("fixture not built yet ({task}): {what}")
}

/// Store an indexer setting that points at `base_url`; remember its id by name.
async fn store_indexer(w: &mut World, name: &str, base_url: &str) {
    let body = serde_json::json!({
        "kind": "torznab",
        "name": name,
        "base_url": base_url,
        "categories": [2000],
        "api_key": "k",
    });
    let r = w
        .call(
            "POST",
            "/api/v1/settings/indexers",
            Some((body.to_string(), "application/json")),
        )
        .await;
    assert_eq!(r.status, 201, "seeding indexer {name} failed: {}", r.text);
    let id = r.json["id"].as_str().expect("id").to_string();
    w.ids.insert(name.to_string(), id);
}

// ---- running the checks --------------------------------------------------------

/// `POST /health/checks/run` (SKADI-T-0680): runs every check now and stores the
/// results that `GET /health/checks` then serves.
#[given("the health checks have run")]
#[when("the health checks have run")]
async fn checks_have_run(w: &mut World) {
    let r = w.call("POST", "/api/v1/health/checks/run", None).await;
    assert!(
        r.status == 200,
        "POST /health/checks/run answered {}: {}",
        r.status,
        r.text
    );
}

/// SKADI-T-0680: run the refresh that the supervisor tick runs, against the
/// cache of the scenario's `AppState` (`w.api()`), without an HTTP request.
#[when("the supervisor refreshes the health checks")]
async fn supervisor_refreshes(w: &mut World) {
    w.api().await.refresh_health().await;
}

#[when(expr = "{int} ms pass")]
async fn time_passes(_w: &mut World, ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

// ---- fake providers ------------------------------------------------------------

#[given(expr = "an indexer {string} whose server counts its requests")]
async fn counting_indexer(w: &mut World, name: String) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/xml")
                .set_body_string(r#"<?xml version="1.0"?><caps><searching/></caps>"#),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    w.fakes.insert(name.clone(), Fake::new(server));
    store_indexer(w, &name, &uri).await;
}

/// Accepts the connection, then holds the reply for longer than any probe budget.
#[given(expr = "an indexer {string} whose server accepts connections but never answers")]
async fn hanging_indexer(w: &mut World, name: String) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(600)))
        .mount(&server)
        .await;
    let uri = server.uri();
    w.fakes.insert(name.clone(), Fake::new(server));
    store_indexer(w, &name, &uri).await;
}

#[then(expr = "the server of indexer {string} has received {int} requests")]
async fn indexer_hits(w: &mut World, name: String, n: usize) {
    let fake = w
        .fakes
        .get(&name)
        .unwrap_or_else(|| panic!("no fake server {name:?}"));
    let got = fake
        .server()
        .received_requests()
        .await
        .unwrap_or_default()
        .len();
    assert_eq!(got, n, "requests that reached indexer {name}");
}

/// A gluetun control server: `/v1/vpn/status` and `/v1/publicip/ip`.
/// `SKADI_GLUETUN_CONTROL_URL` points at it until the world drops (`@serial`).
#[given(expr = "a gluetun whose tunnel is {string} with the exit IP {string}")]
async fn fake_gluetun(w: &mut World, status: String, exit_ip: String) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/vpn/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": status
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "public_ip": exit_ip,
            "country": "Testland",
        })))
        .mount(&server)
        .await;
    w.env_guards
        .push(EnvGuard::set("SKADI_GLUETUN_CONTROL_URL", &server.uri()));
    w.fakes.insert("gluetun".into(), Fake::new(server));
}

// ---- configuration -------------------------------------------------------------

#[given("no indexer is configured")]
async fn no_indexers(w: &mut World) {
    clear_settings(w, "indexers").await;
}

#[given("no download client is configured")]
async fn no_downloaders(w: &mut World) {
    clear_settings(w, "downloaders").await;
}

async fn clear_settings(w: &mut World, kind: &str) {
    let store = w.store().await;
    for row in store.list_settings(kind).await.expect("list settings") {
        store.delete_setting(kind, &row.id).await.expect("delete");
    }
    assert!(store.list_settings(kind).await.expect("list").is_empty());
}

#[given("the library root is not set")]
async fn root_not_set(w: &mut World) {
    let store = w.store().await;
    store.delete_config("library.root").await.expect("delete");
    assert!(
        store
            .get_config("library.root")
            .await
            .expect("get")
            .is_none(),
        "library.root is still set"
    );
}

/// `statvfs` reads the real disk, so the world gives the disk-space check a
/// probe with a fixed fill level (`AppState::disk_probe`, SKADI-T-0681).
#[given(expr = "the library root's filesystem is {int} % full")]
async fn disk_full(w: &mut World, percent: u32) {
    w.disk_used_percent = Some(percent);
    w.reset_api();
}

/// Records `failed` failed and then `total - failed` successful searches of the
/// indexer into the search-health registry, the way the search path does. The
/// last search succeeded, so only the failure rate says it is degraded.
#[given(expr = "{int} of the last {int} searches of indexer {string} failed")]
async fn indexer_search_history(w: &mut World, failed: u32, total: u32, name: String) {
    let id = w
        .ids
        .get(&name)
        .unwrap_or_else(|| panic!("no indexer {name:?}"));
    let iid = skadi_core::IndexerId::from(uuid::Uuid::parse_str(id).expect("indexer id"));
    let health = skadi_indexers::indexer_health();
    for _ in 0..failed {
        health.record_failure(iid, "HTTP 503 from the tracker");
    }
    for _ in failed..total {
        health.record_success(iid);
    }
}

// ---- workers -------------------------------------------------------------------

/// The failure counts that `Supervisor::reap_finished` publishes into
/// `AppState::worker_failures` (SKADI-T-0523).
#[given(expr = "the supervisor has recorded {int} failures of the {string} domain worker")]
async fn worker_failures(w: &mut World, n: u64, domain: String) {
    w.api().await.worker_failures.lock().await.insert(domain, n);
}

#[given(expr = "the download worker {string} last heartbeated {int} minutes ago")]
async fn stale_heartbeat(w: &mut World, id: String, minutes: i64) {
    let store = w.store().await;
    store
        .heartbeat_worker(&id, "0.0.0-test")
        .await
        .expect("heartbeat");
    let at = chrono::Utc::now() - chrono::Duration::minutes(minutes);
    let at_text = at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    store
        .with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::sql_query(format!(
                        "UPDATE worker_status SET last_seen_at = now() - interval '{minutes} minutes' \
                         WHERE worker_id = $1"
                    ))
                    .bind::<diesel::sql_types::Text, _>(&id)
                    .execute(pg)
                },
                |sq| {
                    diesel::sql_query(
                        "UPDATE worker_status SET last_seen_at = ? WHERE worker_id = ?",
                    )
                    .bind::<diesel::sql_types::Text, _>(&at_text)
                    .bind::<diesel::sql_types::Text, _>(&id)
                    .execute(sq)
                },
            )
            .map_err(|e| skadi_core::AppError::Internal(e.to_string()))?;
            Ok(())
        })
        .await
        .expect("backdate heartbeat");
    let seen = store
        .latest_worker_status()
        .await
        .expect("read heartbeat")
        .expect("a heartbeat row");
    assert!(
        !seen.is_fresh(chrono::Duration::minutes(minutes - 1)),
        "heartbeat was not backdated: {seen:?}"
    );
}

/// SKADI-T-0683: the heartbeat row has no egress field yet. The task adds it
/// (the worker reports the IP it sees) and writes it here.
#[given(expr = "the download worker {string} heartbeated just now with the egress IP {string}")]
async fn heartbeat_with_egress(w: &mut World, id: String, egress: String) {
    w.store()
        .await
        .heartbeat_worker(&id, "0.0.0-test")
        .await
        .expect("heartbeat");
    todo_seam(
        "SKADI-T-0683",
        &format!("record the egress IP {egress} on the heartbeat of {id}"),
    );
}

// ---- assertions ----------------------------------------------------------------

#[then("every health check has an id, a label, a severity, a message and a checked_at time")]
async fn every_check_shaped(w: &mut World) {
    let all = checks(w);
    assert!(!all.is_empty(), "no health checks at all");
    for c in &all {
        for field in ["id", "label", "severity", "message"] {
            assert!(
                c[field].as_str().is_some_and(|s| !s.is_empty()),
                "check lacks {field:?}: {c}"
            );
        }
        checked_at(c);
        assert!(
            c.get("remediation").is_some(),
            "check lacks remediation: {c}"
        );
    }
}

#[then(expr = "every health check severity is one of {string}")]
async fn every_severity_in(w: &mut World, allowed: String) {
    let allowed: Vec<&str> = allowed.split(',').map(str::trim).collect();
    for c in checks(w) {
        let s = text(&c, "severity");
        assert!(
            allowed.contains(&s),
            "severity {s:?} not in {allowed:?}: {c}"
        );
    }
}

#[then(expr = "the health check {string} has severity {string}")]
async fn has_severity(w: &mut World, id: String, severity: String) {
    let c = check(w, &id);
    assert_eq!(c["severity"], severity, "check: {c}");
}

#[then(expr = "the health check {string} has a remediation")]
async fn has_remediation(w: &mut World, id: String) {
    let c = check(w, &id);
    assert!(
        c["remediation"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty()),
        "check has no remediation: {c}"
    );
}

#[then(expr = "every health check with severity {string} has a remediation")]
async fn every_severity_has_remediation(w: &mut World, severity: String) {
    let matching: Vec<_> = checks(w)
        .into_iter()
        .filter(|c| c["severity"] == severity)
        .collect();
    assert!(!matching.is_empty(), "no check has severity {severity:?}");
    for c in matching {
        assert!(
            c["remediation"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "a {severity} check has no remediation: {c}"
        );
    }
}

#[then(expr = "the health check {string} message contains {string}")]
async fn message_contains(w: &mut World, id: String, needle: String) {
    let c = check(w, &id);
    assert!(text(&c, "message").contains(&needle), "check: {c}");
}

#[then(expr = "the health check {string} remediation contains {string}")]
async fn remediation_contains(w: &mut World, id: String, needle: String) {
    let c = check(w, &id);
    assert!(text(&c, "remediation").contains(&needle), "check: {c}");
}

#[then(expr = "the request took less than {int} ms")]
async fn took_less_than(w: &mut World, ms: u64) {
    let took = w.elapsed.expect("no request has been made yet");
    assert!(
        took < Duration::from_millis(ms),
        "the request took {took:?}, budget {ms} ms"
    );
}

/// Pending: the check is listed but has never run, so `checked_at` is `null`.
#[then(expr = "the health check {string} is pending")]
async fn is_pending(w: &mut World, id: String) {
    let c = check(w, &id);
    assert!(
        c.get("checked_at").is_some_and(serde_json::Value::is_null),
        "a check that has never run has `checked_at: null`: {c}"
    );
}

#[then(expr = "the health check {string} has a checked_at time")]
async fn has_checked_at(w: &mut World, id: String) {
    checked_at(&check(w, &id));
}

#[then(expr = "the checked_at time of the health check {string} is remembered as {string}")]
async fn remember_checked_at(w: &mut World, id: String, name: String) {
    let at = checked_at(&check(w, &id));
    w.times.insert(name, at);
}

#[then(expr = "the health check {string} was checked after {string}")]
async fn checked_after(w: &mut World, id: String, name: String) {
    let at = checked_at(&check(w, &id));
    let before = *w
        .times
        .get(&name)
        .unwrap_or_else(|| panic!("no time remembered as {name:?}"));
    assert!(
        at > before,
        "checked_at {at} is not after {name} ({before})"
    );
}
