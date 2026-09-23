//! Generic HTTP steps shared by every C29–C36 feature: daemon setup, the
//! credential the client presents, sending requests, and asserting on the
//! status / error envelope / body / headers of the last reply.
use cucumber::gherkin::Step;
use cucumber::{given, then, when};

use crate::bdd_support::{World, kind_for};
use skadi_api::DomainDescriptor;

// --- daemon setup ---------------------------------------------------------

#[given(expr = "a daemon protected by API token {string}")]
async fn protected_daemon(w: &mut World, token: String) {
    w.token = Some(token);
    w.reset_api();
}

#[given("a daemon running in open mode")]
async fn open_daemon(w: &mut World) {
    w.token = None;
    w.reset_api();
}

#[given(expr = "the daemon compiles in the {string} domain")]
async fn compiled_domain(w: &mut World, name: String) {
    w.domains.push(DomainDescriptor {
        kind: kind_for(&name),
        name,
    });
    w.reset_api();
}

#[given(expr = "the {string} domain is enabled")]
async fn domain_enabled(w: &mut World, name: String) {
    use skadi_store::DomainStateRepo;
    if !w.domains.iter().any(|d| d.name == name) {
        w.domains.push(DomainDescriptor {
            kind: kind_for(&name),
            name: name.clone(),
        });
        w.reset_api();
    }
    w.store()
        .await
        .set_enabled(&name, true)
        .await
        .expect("enable");
}

/// SKADI-T-0463: backups default to `/data/backups`, which is right in the
/// container and unwritable here. Point them at the world's temp dir.
#[given("backups are written to a temporary directory")]
async fn backups_to_tmp(w: &mut World) {
    use skadi_store::{ConfigRepo, ConfigSource};
    let dir = w.tmp().join("backups");
    w.store()
        .await
        .set_config(
            "backup.dir",
            &dir.display().to_string(),
            ConfigSource::Runtime,
        )
        .await
        .expect("set backup.dir");
}

/// SKADI-T-0475: readiness is only true once the supervisor has published a
/// provider set. This stands in for that reconcile without running a supervisor.
#[given("the supervisor has published providers")]
async fn providers_published(w: &mut World) {
    w.api()
        .await
        .providers_ready
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

// --- what the client presents ---------------------------------------------

#[given(expr = "the client presents token {string}")]
#[when(expr = "the client presents token {string}")]
async fn presents_token(w: &mut World, token: String) {
    w.presented = Some(format!("Bearer {token}"));
}

#[given(expr = "the client presents the authorization header {string}")]
async fn presents_header(w: &mut World, value: String) {
    w.presented = Some(value);
}

#[given("the client presents no token")]
async fn presents_nothing(w: &mut World) {
    w.presented = None;
}

#[given(expr = "the client presents the header {string} with value {string}")]
async fn extra_header(w: &mut World, name: String, value: String) {
    w.extra_headers.push((name, value));
}

// --- requests -------------------------------------------------------------

#[when(expr = "the client requests {word} {string}")]
async fn request(w: &mut World, method: String, path: String) {
    w.call(&method, &path, None).await;
}

#[when(expr = "the client sends {word} {string} with body:")]
async fn request_with_body(w: &mut World, step: &Step, method: String, path: String) {
    let body = step.docstring().cloned().unwrap_or_default();
    // Fail fast on a typo in the feature: bodies here are always JSON.
    serde_json::from_str::<serde_json::Value>(&body).expect("scenario body must be JSON");
    w.call(&method, &path, Some((body, "application/json")))
        .await;
}

#[when(expr = "the client sends {word} {string} with the raw body {string}")]
async fn request_raw(w: &mut World, method: String, path: String, raw: String) {
    w.call(&method, &path, Some((raw, "application/json")))
        .await;
}

#[when(expr = "the created id is remembered as {string}")]
#[then(expr = "the created id is remembered as {string}")]
async fn remember_id(w: &mut World, name: String) {
    let id = w
        .field("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("reply has no `id`: {}", w.reply().text))
        .to_string();
    w.ids.insert(name, id);
}

// --- assertions -----------------------------------------------------------

#[then(expr = "the response status is {int}")]
async fn status_is(w: &mut World, status: u16) {
    let r = w.reply();
    assert_eq!(r.status, status, "unexpected status; body: {}", r.text);
}

#[then(expr = "the response is a JSON error of kind {string}")]
async fn error_kind(w: &mut World, kind: String) {
    let r = w.reply();
    assert_eq!(
        r.json.get("error").and_then(|v| v.as_str()),
        Some(kind.as_str()),
        "expected the `{{error, message}}` envelope with kind {kind:?}; got status {} body {:?}",
        r.status,
        r.text
    );
    assert!(
        r.json.get("message").and_then(|v| v.as_str()).is_some(),
        "envelope carries a `message`: {}",
        r.text
    );
}

#[then(expr = "the error message contains {string}")]
async fn error_message_contains(w: &mut World, needle: String) {
    let msg = w
        .field("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(msg.contains(&needle), "message {msg:?} lacks {needle:?}");
}

#[then(expr = "the response body is a JSON array of length {int}")]
async fn array_len(w: &mut World, n: usize) {
    let r = w.reply();
    let arr = r
        .json
        .as_array()
        .unwrap_or_else(|| panic!("not a JSON array: {}", r.text));
    assert_eq!(arr.len(), n, "array: {}", r.text);
}

#[then(expr = "the response field {string} is {string}")]
async fn field_is(w: &mut World, path: String, expected: String) {
    let expected = w.expand(&expected);
    let actual = w
        .field(&path)
        .cloned()
        .unwrap_or_else(|| panic!("field {path:?} absent in {}", w.reply().text));
    // Expected values are written as JSON when they are not plain strings.
    let expected_json: serde_json::Value = serde_json::from_str(&expected)
        .unwrap_or_else(|_| serde_json::Value::String(expected.clone()));
    let rendered = actual.to_string();
    let matches = actual == expected_json
        || actual.as_str() == Some(expected.as_str())
        || rendered == expected;
    assert!(matches, "field {path:?}: expected {expected}, got {actual}");
}

#[then(expr = "the response field {string} contains {string}")]
async fn field_contains(w: &mut World, path: String, needle: String) {
    let actual = w
        .field(&path)
        .cloned()
        .unwrap_or_else(|| panic!("field {path:?} absent in {}", w.reply().text));
    let hay = actual
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| actual.to_string());
    assert!(
        hay.contains(&needle),
        "field {path:?}: {hay:?} lacks {needle:?}"
    );
}

#[then(expr = "the response field {string} is present")]
async fn field_present(w: &mut World, path: String) {
    assert!(
        w.field(&path).is_some(),
        "field {path:?} absent in {}",
        w.reply().text
    );
}

#[then(expr = "the response field {string} is absent")]
async fn field_absent(w: &mut World, path: String) {
    assert!(
        w.field(&path).is_none(),
        "field {path:?} present in {}",
        w.reply().text
    );
}

#[then(expr = "the response header {string} is {string}")]
async fn header_is(w: &mut World, name: String, value: String) {
    let r = w.reply();
    let got = r
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&name))
        .map(|(_, v)| v.clone());
    assert_eq!(
        got.as_deref(),
        Some(value.as_str()),
        "headers: {:?}",
        r.headers
    );
}

#[then(expr = "the response header {string} is present")]
async fn header_present(w: &mut World, name: String) {
    let r = w.reply();
    assert!(
        r.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(&name)),
        "header {name:?} missing; status {} headers {:?}",
        r.status,
        r.headers
    );
}

#[then(expr = "the response header {string} starts with {string}")]
async fn header_starts(w: &mut World, name: String, prefix: String) {
    let r = w.reply();
    let got = r
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&name))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    assert!(got.starts_with(&prefix), "header {name}: {got:?}");
}

#[then(expr = "the response body contains {string}")]
async fn body_contains(w: &mut World, needle: String) {
    let r = w.reply();
    assert!(
        r.text.contains(&needle),
        "body {:?} lacks {needle:?}",
        r.text
    );
}

#[then("the response body is an HTML document")]
async fn body_is_html(w: &mut World) {
    let t = w.reply().text.to_lowercase();
    assert!(
        t.contains("<!doctype") || t.contains("<html"),
        "not HTML: {t}"
    );
}

#[then("the response body is not an HTML document")]
async fn body_not_html(w: &mut World) {
    let t = w.reply().text.to_lowercase();
    assert!(
        !t.contains("<!doctype") && !t.contains("<html"),
        "HTML: {t}"
    );
}

/// SKADI-T-0525: Sonarr's `propertyName` — the envelope names the offending input
/// so a settings form can mark it, instead of the operator parsing prose.
#[then(expr = "the error field is {string}")]
async fn error_field(w: &mut World, field: String) {
    let body = w.reply().json.clone();
    assert_eq!(
        body.get("field").and_then(|v| v.as_str()),
        Some(field.as_str()),
        "envelope: {body}"
    );
}

/// SKADI-T-0535: a redacted key reports its *presence* but never its value, so
/// the absence of the field is the assertion.
#[then(expr = "the response has no field {string}")]
async fn no_field(w: &mut World, field: String) {
    let body = w.reply().json.clone();
    assert!(
        body.get(&field).is_none(),
        "expected {field:?} to be absent: {body}"
    );
}
