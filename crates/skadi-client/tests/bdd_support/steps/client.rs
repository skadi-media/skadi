//! Steps driving `skadi_client::Client` against the recorded-response daemon.
use std::time::Duration;

use cucumber::gherkin::Step;
use cucumber::{given, then, when};

use skadi_client::ApiError;

use crate::bdd_support::World;

#[given(expr = "a daemon that answers {int} with body {string}")]
async fn daemon_answers(w: &mut World, status: u16, body: String) {
    // Cucumber keeps `\"` escapes literal inside `{string}`; the features write JSON.
    let body = body.replace("\\\"", "\"");
    {
        let mut c = w.canned.lock().unwrap();
        c.status = status;
        c.body = body;
        c.hang = false;
    }
    w.daemon().await;
}

#[given(expr = "a daemon that answers {int} with a non-JSON body")]
async fn daemon_answers_text(w: &mut World, status: u16) {
    {
        let mut c = w.canned.lock().unwrap();
        c.status = status;
        c.content_type = "text/plain".into();
        c.body = "<html>not json</html>".into();
    }
    w.daemon().await;
}

#[given("a daemon that never answers")]
async fn daemon_hangs(w: &mut World) {
    w.canned.lock().unwrap().hang = true;
    w.daemon().await;
}

#[given("no daemon is listening")]
async fn no_daemon(w: &mut World) {
    // Bind then drop: the port is free again and refuses connections.
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().unwrap().port();
    drop(l);
    w.base_url = Some(format!("http://127.0.0.1:{port}"));
}

#[given(expr = "a client configured with token {string}")]
async fn with_token(w: &mut World, token: String) {
    w.token = Some(token);
}

#[given("a client configured without a token")]
async fn without_token(w: &mut World) {
    w.token = None;
}

#[given("the daemon base URL has a trailing slash")]
async fn trailing_slash(w: &mut World) {
    let url = w.daemon().await;
    w.base_url = Some(format!("{url}/"));
}

/// Dispatch a client method by name. Add a line here when a client method
/// gains a scenario; unknown names fail the step loudly.
async fn dispatch(
    client: &skadi_client::Client,
    method: &str,
    step: Option<&Step>,
) -> Result<serde_json::Value, ApiError> {
    let body = step
        .and_then(|s| s.docstring().cloned())
        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
        .unwrap_or(serde_json::json!({}));
    match method {
        "health" => client.health().await,
        "health_checks" => client.health_checks().await,
        "domains" => client.domains().await,
        "enable movies" => client.set_domain_enabled("movies", true).await,
        "list_settings profiles" => client.list_settings("profiles").await,
        "create_setting profiles" => client.create_setting("profiles", &body).await,
        "delete_setting profiles abc" => client.delete_setting("profiles", "abc").await,
        "test_setting indexers abc" => client.test_setting("indexers", "abc").await,
        "list_movies monitored" => client.list_movies(Some(true)).await,
        "list_movies" => client.list_movies(None).await,
        "get_movie m1" => client.get_movie("m1").await,
        "add_movie 603" => client.add_movie(603, None, None, true).await,
        "library movies monitored" => client.library(Some("movie"), Some(true)).await,
        "activity" => client.activity().await,
        "list_blocklist" => client.list_blocklist(None).await,
        "root_folders" => client.root_folders().await,
        other => panic!("no dispatch for client method {other:?}"),
    }
}

#[when(expr = "the client calls {string}")]
async fn call(w: &mut World, method: String) {
    let client = w.client().await;
    let r = dispatch(&client, &method, None).await;
    w.result = Some(r);
}

#[when(expr = "the client calls {string} with body:")]
async fn call_with_body(w: &mut World, step: &Step, method: String) {
    let client = w.client().await;
    let r = dispatch(&client, &method, Some(step)).await;
    w.result = Some(r);
}

#[when(expr = "the client calls {string} and is given {int} seconds")]
async fn call_bounded(w: &mut World, method: String, secs: u64) {
    let client = w.client().await;
    let r = tokio::time::timeout(Duration::from_secs(secs), dispatch(&client, &method, None))
        .await
        .unwrap_or_else(|_| {
            Err(ApiError::Network(format!(
                "scenario gave up after {secs}s: the client has no request timeout"
            )))
        });
    w.notes.push(format!("bounded:{}", r.is_err()));
    w.result = Some(r);
}

// --- what the daemon saw ---------------------------------------------------

#[then(expr = "the daemon saw {word} {string}")]
async fn saw(w: &mut World, method: String, path: String) {
    let s = w.last_seen();
    assert_eq!(
        (s.method.as_str(), s.path.as_str()),
        (method.as_str(), path.as_str())
    );
}

#[then(expr = "the daemon saw the header {string} with value {string}")]
async fn saw_header(w: &mut World, name: String, value: String) {
    let s = w.last_seen();
    let got = s
        .headers
        .iter()
        .find(|(k, _)| k == &name.to_ascii_lowercase())
        .map(|(_, v)| v.clone());
    assert_eq!(
        got.as_deref(),
        Some(value.as_str()),
        "headers: {:?}",
        s.headers
    );
}

#[then(expr = "the daemon saw no {string} header")]
async fn saw_no_header(w: &mut World, name: String) {
    let s = w.last_seen();
    assert!(
        !s.headers
            .iter()
            .any(|(k, _)| k == &name.to_ascii_lowercase()),
        "headers: {:?}",
        s.headers
    );
}

#[then(expr = "the daemon saw the JSON body field {string} equal to {string}")]
async fn saw_body_field(w: &mut World, field: String, value: String) {
    let s = w.last_seen();
    let json: serde_json::Value = serde_json::from_str(&s.body).expect("JSON body");
    let expected: serde_json::Value =
        serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value.clone()));
    assert_eq!(json[&field], expected, "body: {}", s.body);
}

// --- what the client returned ---------------------------------------------

#[then("the call succeeds")]
async fn succeeds(w: &mut World) {
    assert!(w.result().is_ok(), "got {:?}", w.result());
}

#[then(expr = "the call returns the JSON field {string} equal to {string}")]
async fn returns_field(w: &mut World, field: String, value: String) {
    let v = w.result().as_ref().expect("ok result");
    let expected: serde_json::Value =
        serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value.clone()));
    assert_eq!(v[&field], expected, "value: {v}");
}

#[then("the call returns JSON null")]
async fn returns_null(w: &mut World) {
    assert_eq!(w.result().as_ref().expect("ok"), &serde_json::Value::Null);
}

#[then(expr = "the call fails with {word}")]
async fn fails_with(w: &mut World, variant: String) {
    let err = match w.result() {
        Ok(v) => panic!("expected {variant} error, got Ok({v})"),
        Err(e) => e,
    };
    let ok = match variant.as_str() {
        "Unauthorized" => matches!(err, ApiError::Unauthorized),
        "NotFound" => matches!(err, ApiError::NotFound(_)),
        "Status" => matches!(err, ApiError::Status { .. }),
        "Network" => matches!(err, ApiError::Network(_)),
        "Decode" => matches!(err, ApiError::Decode(_)),
        other => panic!("unknown ApiError variant {other}"),
    };
    assert!(ok, "expected {variant}, got {err:?}");
}

#[then(expr = "the error text contains {string}")]
async fn error_contains(w: &mut World, needle: String) {
    let text = match w.result() {
        Ok(v) => panic!("expected an error, got Ok({v})"),
        Err(e) => e.to_string(),
    };
    assert!(text.contains(&needle), "error {text:?} lacks {needle:?}");
}

#[then(expr = "the error carries the status {int}")]
async fn error_status(w: &mut World, code: u16) {
    match w.result() {
        Err(ApiError::Status { code: c, .. }) => assert_eq!(*c, code),
        other => panic!("expected Status error, got {other:?}"),
    }
}

#[then("the client gave up on its own before the scenario deadline")]
async fn gave_up_itself(w: &mut World) {
    let text = match w.result() {
        Ok(v) => panic!("expected an error, got Ok({v})"),
        Err(e) => e.to_string(),
    };
    assert!(
        !text.contains("scenario gave up"),
        "the client has no request timeout: {text}"
    );
}
