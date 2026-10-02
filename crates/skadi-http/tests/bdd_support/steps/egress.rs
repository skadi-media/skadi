//! C05 egress-client steps: retry/backoff on transient failures, per-attempt
//! timeouts, non-idempotent requests, and the proxy story (ambient env only).
use std::time::{Duration, Instant};

use cucumber::{given, then, when};
use wiremock::matchers::{header_regex, method, path};
use wiremock::{Mock, ResponseTemplate};

use skadi_http::{HttpClient, RetryConfig};

use crate::bdd_support::{Client, World};

// ---- client construction ---------------------------------------------------

#[given(
    expr = "an egress client with a {int} ms timeout, {int} retries and a {int} ms base backoff"
)]
async fn client(w: &mut World, timeout_ms: u64, retries: u32, backoff_ms: u64) {
    let c = HttpClient::with_config(
        Duration::from_millis(timeout_ms),
        RetryConfig {
            max_retries: retries,
            base_backoff: Duration::from_millis(backoff_ms),
        },
    )
    .expect("client");
    w.client = Some(Client(c));
}

#[given("an egress client with the default retry policy")]
async fn default_client(w: &mut World) {
    w.client = Some(Client(
        HttpClient::new(Duration::from_secs(5)).expect("client"),
    ));
}

// ---- upstream behaviour ----------------------------------------------------

#[given(expr = "the upstream answers {string} with {int} and body {string}")]
async fn upstream_ok(w: &mut World, route: String, status: u16, body: String) {
    let server = w.server().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

#[given(
    expr = "the upstream answers {string} with {int} for the first {int} calls, then 200 {string}"
)]
async fn upstream_flaky(w: &mut World, route: String, status: u16, n: u64, body: String) {
    let server = w.server().await;
    Mock::given(method("GET"))
        .and(path(route.clone()))
        .respond_with(ResponseTemplate::new(status))
        .up_to_n_times(n)
        .expect(n)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[given(expr = "the upstream always answers {string} with {int}")]
async fn upstream_always(w: &mut World, route: String, status: u16) {
    let server = w.server().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

#[given(
    expr = "the upstream answers {string} with 429 and Retry-After {int} s for the first call, then 200"
)]
async fn upstream_retry_after(w: &mut World, route: String, secs: u64) {
    let server = w.server().await;
    Mock::given(method("GET"))
        .and(path(route.clone()))
        .respond_with(
            ResponseTemplate::new(429).insert_header("retry-after", secs.to_string().as_str()),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(server)
        .await;
}

#[given(expr = "the upstream answers {string} only after a {int} ms delay")]
async fn upstream_slow(w: &mut World, route: String, delay_ms: u64) {
    let server = w.server().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("late")
                .set_delay(Duration::from_millis(delay_ms)),
        )
        .mount(server)
        .await;
}

#[given(expr = "the upstream answers {string} with JSON:")]
async fn upstream_json(w: &mut World, step: &cucumber::gherkin::Step, route: String) {
    let json = step.docstring().cloned().unwrap_or_default();
    let server = w.server().await;
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .mount(server)
        .await;
}

#[given(expr = "the upstream answers {string} only to a User-Agent containing {string}")]
async fn upstream_ua(w: &mut World, route: String, needle: String) {
    let server = w.server().await;
    Mock::given(method("GET"))
        .and(path(route))
        .and(header_regex("user-agent", &needle))
        .respond_with(ResponseTemplate::new(200).set_body_string("identified"))
        .mount(server)
        .await;
}

#[given(expr = "the upstream answers POST {string} with 503 and expects exactly {int} call(s)")]
async fn upstream_post(w: &mut World, route: String, n: u64) {
    let server = w.server().await;
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(503))
        .expect(n)
        .mount(server)
        .await;
}

// ---- requests ----------------------------------------------------------------

async fn record(w: &mut World, started: Instant, r: skadi_core::Result<String>) {
    w.elapsed = Some(started.elapsed());
    w.outcome = Some(r.map_err(|e| e.to_string()));
}

#[when(expr = "the client GETs {string}")]
async fn get(w: &mut World, route: String) {
    let url = format!("{}{}", w.server().await.uri(), route);
    let started = Instant::now();
    let r = w.client().get_text(&url).await;
    record(w, started, r).await;
}

#[when(expr = "the client GETs {string} as JSON")]
async fn get_json(w: &mut World, route: String) {
    let url = format!("{}{}", w.server().await.uri(), route);
    let started = Instant::now();
    let r = w
        .client()
        .get_json::<serde_json::Value>(&url)
        .await
        .map(|v| v.to_string());
    record(w, started, r).await;
}

#[when(expr = "the client GETs {string} as bytes")]
async fn get_bytes(w: &mut World, route: String) {
    let url = format!("{}{}", w.server().await.uri(), route);
    let started = Instant::now();
    let r = w
        .client()
        .get_bytes(&url)
        .await
        .map(|b| format!("{} bytes", b.len()));
    record(w, started, r).await;
}

#[when(expr = "the client GETs the closed port {string}")]
async fn get_closed(w: &mut World, url: String) {
    let started = Instant::now();
    let r = w.client().get_text(&url).await;
    record(w, started, r).await;
}

#[when(expr = "the client POSTs {string} through the raw client")]
async fn post_raw(w: &mut World, route: String) {
    let url = format!("{}{}", w.server().await.uri(), route);
    let started = Instant::now();
    let resp = w.client().raw().post(&url).send().await;
    let r = match resp {
        Ok(r) => Ok(format!("HTTP {}", r.status())),
        Err(e) => Err(skadi_core::AppError::Network(e.to_string())),
    };
    record(w, started, r).await;
}

// ---- outcomes ----------------------------------------------------------------

#[then(expr = "the request succeeds with body {string}")]
async fn ok_body(w: &mut World, body: String) {
    assert_eq!(w.outcome.as_ref().expect("a request"), &Ok(body));
}

#[then(expr = "the request succeeds with {string}")]
async fn ok_with(w: &mut World, s: String) {
    assert_eq!(w.outcome.as_ref().expect("a request"), &Ok(s));
}

#[then("the request succeeds with JSON:")]
async fn ok_json(w: &mut World, step: &cucumber::gherkin::Step) {
    let want: serde_json::Value =
        serde_json::from_str(&step.docstring().cloned().unwrap_or_default()).expect("valid JSON");
    match w.outcome.as_ref().expect("a request") {
        Ok(body) => {
            let got: serde_json::Value = serde_json::from_str(body).expect("JSON body");
            assert_eq!(got, want);
        }
        Err(e) => panic!("expected JSON, got error {e}"),
    }
}

#[then("the request fails with a network error")]
async fn network_error(w: &mut World) {
    match w.outcome.as_ref().expect("a request") {
        Err(e) => assert!(e.starts_with("Network error:"), "not a Network error: {e}"),
        Ok(b) => panic!("expected an error, got body {b:?}"),
    }
}

/// SKADI-T-0502: a 404 is the upstream answering, not a transport failure.
#[then("the request fails with a not-found error")]
async fn not_found_error(w: &mut World) {
    match w.outcome.as_ref().expect("a request") {
        Err(e) => assert!(e.starts_with("Not found:"), "not a NotFound error: {e}"),
        Ok(b) => panic!("expected an error, got body {b:?}"),
    }
}

#[then(expr = "the error mentions {string}")]
async fn error_mentions(w: &mut World, needle: String) {
    match w.outcome.as_ref().expect("a request") {
        Err(e) => assert!(e.contains(&needle), "{e:?} lacks {needle:?}"),
        Ok(b) => panic!("expected an error, got body {b:?}"),
    }
}

#[then(expr = "the request took at least {int} ms")]
async fn took_at_least(w: &mut World, ms: u64) {
    let took = w.elapsed.expect("a request");
    assert!(
        took >= Duration::from_millis(ms),
        "took {took:?}, expected >= {ms} ms"
    );
}

#[then(expr = "the request took less than {int} ms")]
async fn took_less(w: &mut World, ms: u64) {
    let took = w.elapsed.expect("a request");
    assert!(
        took < Duration::from_millis(ms),
        "took {took:?}, expected < {ms} ms"
    );
}

#[then("the upstream saw exactly the expected number of calls")]
async fn verify(w: &mut World) {
    // `MockServer` verifies `.expect(n)` on drop; do it eagerly for a clear message.
    w.server.take().expect("a server").0.verify().await;
}

// ---- proxy (ambient env; @serial) ------------------------------------------

#[given(expr = "the ambient HTTP proxy is {string}")]
async fn ambient_proxy(w: &mut World, proxy: String) {
    for key in ["HTTP_PROXY", "http_proxy", "NO_PROXY", "no_proxy"] {
        w.env_touched
            .push((key.to_string(), std::env::var(key).ok()));
    }
    // SAFETY: `@serial` scenario — no other scenario builds a client concurrently.
    unsafe {
        std::env::set_var("HTTP_PROXY", &proxy);
        std::env::set_var("http_proxy", &proxy);
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }
}

#[given("an egress client built under that proxy with 2 retries and a 1 ms base backoff")]
async fn client_under_proxy(w: &mut World) {
    // reqwest snapshots the proxy env when the client is built.
    let c = HttpClient::with_config(
        Duration::from_millis(2_000),
        RetryConfig {
            max_retries: 2,
            base_backoff: Duration::from_millis(1),
        },
    )
    .expect("client");
    w.client = Some(Client(c));
    w.restore_env();
}

/// The parity behaviour: Prowlarr/Sonarr configure the proxy explicitly
/// (Settings → General → Proxy) and route indexer traffic through it. skadi's
/// shared client has no proxy option at all — only the cardigann fetcher builds
/// its own proxied `reqwest::Client` (`crates/skadi-indexers/src/cardigann.rs:53`).
/// SKADI-T-0522: the proxy is a client setting, not ambient process environment,
/// so torznab/metadata/notifier egress can be routed through gluetun like
/// cardigann already was.
#[then(expr = "the client can be configured to egress via the proxy {string}")]
async fn explicit_proxy(_w: &mut World, proxy: String) {
    use std::time::Duration;
    // The proxy named in the scenario must at least build a client.
    skadi_http::HttpClient::with_proxy(
        Duration::from_secs(5),
        skadi_http::RetryConfig::default(),
        &proxy,
        &["nas.local".to_string()],
    )
    .expect("a valid proxy URL builds a client");

    // Then prove the traffic really goes to the proxy, with one we run here: a
    // local mock answers the proxied request for a host that does not exist.
    // A direct request could only fail, so an answer means it went via the
    // proxy. This used to send the request to the scenario's proxy itself
    // ("http://gluetun:8888"), which needed DNS for that name on whatever
    // machine ran the suite — and once hung a release build for over two hours
    // in CI — while proving nothing: going direct fails the same way.
    let local_proxy = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::path("/x"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("via proxy"))
        .mount(&local_proxy)
        .await;
    let client = skadi_http::HttpClient::with_proxy(
        Duration::from_secs(5),
        skadi_http::RetryConfig::default(),
        &local_proxy.uri(),
        &[],
    )
    .expect("client");
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        client.get_text("http://example.invalid/x"),
    )
    .await
    .expect("a proxied request must not hang")
    .expect("the proxy answers for a host that cannot be reached directly");
    assert_eq!(body, "via proxy");

    // A malformed proxy is a config error, not a silent fall-back to direct
    // egress — silently ignoring it is how traffic leaks past a VPN.
    let bad = skadi_http::HttpClient::with_proxy(
        Duration::from_secs(5),
        skadi_http::RetryConfig::default(),
        "not a url",
        &[],
    );
    assert!(
        matches!(bad, Err(skadi_core::AppError::Config(_))),
        "a malformed proxy must be a Config error, not a silent fall-back"
    );
}
