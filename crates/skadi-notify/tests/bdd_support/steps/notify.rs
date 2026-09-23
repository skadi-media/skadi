//! Webhook notifier delivery, signing, channel filtering, settings rows and the
//! event vocabulary.
use std::time::Duration;

use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use skadi_core::NotifierId;
use skadi_http::HttpClient;
use skadi_notify::{
    EventPayload, NotificationEvent, NotificationKind, NotifierConfig, WebhookNotifier,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::bdd_support::World;

fn http() -> HttpClient {
    HttpClient::new(Duration::from_secs(5)).expect("http client")
}

fn kinds(s: &str) -> Vec<NotificationKind> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|k| {
            serde_json::from_value(serde_json::Value::String(k.to_string()))
                .unwrap_or_else(|_| panic!("unknown notification kind {k:?}"))
        })
        .collect()
}

fn event(kind: &str, title: &str, year: Option<u16>, quality: Option<&str>) -> NotificationEvent {
    let p = EventPayload {
        title: title.into(),
        year,
        quality: quality.map(str::to_string),
        message: None,
    };
    match kind {
        "grabbed" => NotificationEvent::Grabbed(p),
        "imported" => NotificationEvent::Imported(p),
        "upgraded" => NotificationEvent::Upgraded(p),
        "failed" => NotificationEvent::Failed(p),
        "health" => NotificationEvent::Health(p),
        other => panic!("unknown event kind {other:?}"),
    }
}

#[given(regex = r#"^a webhook notifier subscribed to "([^"]*)"$"#)]
async fn notifier(w: &mut World, channels: String) {
    let server = MockServer::start().await;
    w.notifier = Some(Box::new(WebhookNotifier::new(
        NotifierId::new(),
        format!("{}/hook", server.uri()),
        kinds(&channels),
        http(),
    )));
    w.secret = None;
    w.server = Some(server);
}

#[given(regex = r#"^a webhook notifier subscribed to "([^"]*)" signed with "([^"]*)"$"#)]
async fn signed_notifier(w: &mut World, channels: String, secret: String) {
    let server = MockServer::start().await;
    w.notifier = Some(Box::new(
        WebhookNotifier::new(
            NotifierId::new(),
            format!("{}/hook", server.uri()),
            kinds(&channels),
            http(),
        )
        .with_secret(secret.clone().into_bytes()),
    ));
    w.secret = Some(secret.into_bytes());
    w.server = Some(server);
}

#[given(regex = r"^the webhook endpoint answers HTTP (\d+)$")]
async fn endpoint(w: &mut World, status: u16) {
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(status))
        .mount(w.server())
        .await;
}

#[given(regex = r"^the webhook endpoint answers HTTP (\d+) once and then HTTP (\d+)$")]
async fn flaky_endpoint(w: &mut World, first: u16, then: u16) {
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(first))
        .up_to_n_times(1)
        .mount(w.server())
        .await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(then))
        .mount(w.server())
        .await;
}

/// Dispatch the way the hunter does: only notifiers that `want` the event get it.
async fn dispatch(w: &mut World, ev: NotificationEvent) {
    w.delivery = if w.notifier().wants(&ev) {
        Some(w.notifier().notify(&ev).await.map_err(|e| e.to_string()))
    } else {
        None
    };
    w.last_event = Some(ev);
}

#[when(regex = r#"^a "([a-z]+)" event for "([^"]*)" \((\d{4})\) at "([^"]*)" is dispatched$"#)]
async fn dispatch_full(w: &mut World, kind: String, title: String, year: u16, quality: String) {
    dispatch(w, event(&kind, &title, Some(year), Some(&quality))).await;
}

#[when(regex = r#"^a "([a-z]+)" event for "([^"]*)" is dispatched$"#)]
async fn dispatch_bare(w: &mut World, kind: String, title: String) {
    dispatch(w, event(&kind, &title, None, None)).await;
}

#[when("the notifier's test button is pressed")]
async fn test_button(w: &mut World) {
    w.test_result = Some(w.notifier().test().await.map_err(|e| e.to_string()));
}

#[then("the event is delivered")]
fn delivered(w: &mut World) {
    assert!(
        matches!(w.delivery, Some(Ok(()))),
        "delivery outcome {:?}",
        w.delivery
    );
}

#[then("the event is filtered out before any request is made")]
async fn filtered(w: &mut World) {
    assert!(w.delivery.is_none(), "outcome {:?}", w.delivery);
    assert!(w.posts().await.is_empty());
}

#[then(regex = r#"^the delivery fails with an error containing "([^"]*)"$"#)]
fn delivery_fails(w: &mut World, needle: String) {
    match &w.delivery {
        Some(Err(e)) => assert!(e.contains(&needle), "error {e:?} lacks {needle:?}"),
        other => panic!("expected a failed delivery, got {other:?}"),
    }
}

#[then("the test passes")]
fn test_ok(w: &mut World) {
    assert!(matches!(w.test_result, Some(Ok(()))), "{:?}", w.test_result);
}

#[then(regex = r"^the endpoint received (\d+) POSTs?$")]
async fn n_posts(w: &mut World, n: usize) {
    assert_eq!(w.posts().await.len(), n);
}

#[then(regex = r#"^the posted JSON has "([^"]+)" = "([^"]*)"$"#)]
async fn json_field(w: &mut World, ptr: String, want: String) {
    let posts = w.posts().await;
    let last = posts.last().expect("a POST");
    let v: serde_json::Value = serde_json::from_slice(&last.body).expect("JSON body");
    let got = v
        .pointer(&format!("/{}", ptr.replace('.', "/")))
        .unwrap_or_else(|| panic!("no {ptr} in {v}"));
    let got_s = match got {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    assert_eq!(got_s, want, "field {ptr} in {v}");
}

#[then(regex = r#"^the posted JSON has no "([^"]+)"$"#)]
async fn json_absent(w: &mut World, ptr: String) {
    let posts = w.posts().await;
    let last = posts.last().expect("a POST");
    let v: serde_json::Value = serde_json::from_slice(&last.body).expect("JSON body");
    assert!(
        v.pointer(&format!("/{}", ptr.replace('.', "/"))).is_none(),
        "{ptr} present in {v}"
    );
}

#[then("the posted JSON carries an RFC3339 timestamp")]
async fn json_ts(w: &mut World) {
    let posts = w.posts().await;
    let v: serde_json::Value = serde_json::from_slice(&posts.last().expect("a POST").body).unwrap();
    let ts = v["timestamp"].as_str().expect("timestamp string");
    assert!(chrono::DateTime::parse_from_rfc3339(ts).is_ok(), "{ts}");
}

#[then("the POST is JSON and carries a valid HMAC-SHA256 signature of its body")]
async fn signed(w: &mut World) {
    let posts = w.posts().await;
    let last = posts.last().expect("a POST");
    assert_eq!(
        last.headers
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let sig = last
        .headers
        .get("x-skadi-signature")
        .and_then(|v| v.to_str().ok())
        .expect("x-skadi-signature header");
    let secret = w.secret.as_ref().expect("a secret");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(&last.body);
    let want: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(sig, want);
}

#[then("the POST carries no signature header")]
async fn unsigned(w: &mut World) {
    let posts = w.posts().await;
    let last = posts.last().expect("a POST");
    assert!(last.headers.get("x-skadi-signature").is_none());
}

#[then(regex = r#"^the notifier (wants|does not want) "([a-z]+)" events$"#)]
fn wants(w: &mut World, yes: String, kind: String) {
    let ev = event(&kind, "x", None, None);
    assert_eq!(w.notifier().wants(&ev), yes == "wants");
}

// --- settings rows ----------------------------------------------------------------

#[given("the notifier settings row:")]
fn settings_row(w: &mut World, step: &Step) {
    let raw = step.docstring().expect("a JSON doc string");
    w.config_json = Some(serde_json::from_str(raw).expect("valid JSON"));
}

/// SKADI-T-0538: the secret is what distinguishes a Telegram/Pushover notifier
/// that can actually send from one that only looks configured, so the tests
/// control it explicitly rather than always supplying one.
#[when(expr = "the provider factory builds the notifier with secret {string}")]
fn build_with_secret(w: &mut World, secret: String) {
    build_inner(w, Some(secret));
}

#[when("the provider factory builds the notifier without a secret")]
fn build_without_secret(w: &mut World) {
    build_inner(w, None);
}

#[when("the provider factory builds the notifier")]
fn build(w: &mut World) {
    // Historic default: most kinds either ignore the secret or treat it as
    // optional (the webhook's HMAC key).
    build_inner(w, Some("s3cret".to_string()));
}

fn build_inner(w: &mut World, secret: Option<String>) {
    let cfg: NotifierConfig = match serde_json::from_value(w.config_json.clone().unwrap()) {
        Ok(c) => c,
        Err(e) => {
            w.build_error = Some(format!("deserialize: {e}"));
            return;
        }
    };
    match cfg.build(NotifierId::new(), secret, http()) {
        Ok(n) => {
            w.notifier = Some(n);
            w.build_error = None;
        }
        Err(e) => w.build_error = Some(e.to_string()),
    }
}

/// SKADI-T-0538: a refusal must name the field, so a settings form can mark it.
#[then(expr = "the notifier is rejected naming the field {string}")]
fn rejected_field(w: &mut World, field: String) {
    let err = w.build_error.as_deref().expect("expected a build failure");
    assert!(
        err.contains(&field),
        "error {err:?} does not name the field {field:?}"
    );
}

#[then("the notifier builds")]
fn builds(w: &mut World) {
    assert!(w.build_error.is_none(), "build failed: {:?}", w.build_error);
}

#[then(regex = r#"^the build is rejected with a validation error mentioning "([^"]*)"$"#)]
fn rejected(w: &mut World, needle: String) {
    let err = w.build_error.as_deref().expect("expected a build failure");
    assert!(
        !err.starts_with("deserialize:"),
        "not a validation error: {err}"
    );
    assert!(err.contains(&needle), "error {err:?} lacks {needle:?}");
}

#[then("the settings row round-trips")]
fn round_trips(w: &mut World) {
    let cfg: NotifierConfig = serde_json::from_value(w.config_json.clone().unwrap()).unwrap();
    let back: NotifierConfig = serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
    assert_eq!(cfg, back);
}

// --- the event vocabulary -----------------------------------------------------------

#[given("the event JSON:")]
fn event_json(w: &mut World, step: &Step) {
    w.event_json = Some(step.docstring().expect("JSON").to_string());
}

#[then(regex = r#"^it deserialises as a "([a-z_]+)" notification event$"#)]
fn event_parses(w: &mut World, kind: String) {
    let raw = w.event_json.as_deref().expect("event JSON");
    let ev: NotificationEvent = serde_json::from_str(raw)
        .unwrap_or_else(|e| panic!("event JSON {raw} does not deserialise: {e}"));
    let got = serde_json::to_value(ev.kind()).unwrap();
    assert_eq!(got, serde_json::Value::String(kind));
}

#[then(regex = r#"^a "([a-z]+)" event serialises with tag "([^"]*)" and no null fields$"#)]
fn event_serialises(_w: &mut World, kind: String, tag: String) {
    let ev = event(&kind, "The Matrix", Some(1999), None);
    let json = serde_json::to_value(&ev).unwrap();
    assert_eq!(json["event"], tag);
    assert_eq!(json["title"], "The Matrix");
    assert_eq!(json["year"], 1999);
    assert!(json.get("quality").is_none(), "{json}");
    assert!(json.get("message").is_none(), "{json}");
}
