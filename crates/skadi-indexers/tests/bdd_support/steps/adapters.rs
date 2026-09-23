//! Building the indexer under test: Torznab, the Prowlarr aggregate, and native
//! Cardigann trackers — always pointed at an in-process mock server.
use std::collections::BTreeMap;
use std::sync::Arc;

use cucumber::{given, then, when};
use skadi_cardigann::engine::{FetchReq, Fetcher as _, Method};
use skadi_core::IndexerId;
use skadi_indexers::cardigann::{CardigannIndexer, HttpFetcher};
use skadi_indexers::flaresolverr::is_challenge;
use skadi_indexers::{Prowlarr, StubIndexer, Torznab};

use crate::bdd_support::steps::common::parse_cats;
use crate::bdd_support::{World, fast_http, fixtures};

#[given(regex = r#"^a Torznab indexer configured with categories "([^"]*)"$"#)]
async fn torznab(w: &mut World, cats: String) {
    let uri = w.start_server("indexer").await;
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(Box::new(Torznab::new(
        id,
        uri,
        "secret",
        parse_cats(&cats),
        fast_http(),
    )));
}

#[given(regex = r#"^a Prowlarr aggregate indexer configured with categories "([^"]*)"$"#)]
async fn prowlarr(w: &mut World, cats: String) {
    let uri = w.start_server("indexer").await;
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(Box::new(Prowlarr::new(
        id,
        uri,
        "secret",
        parse_cats(&cats),
        fast_http(),
    )));
}

#[given("the built-in stub indexer")]
fn stub(w: &mut World) {
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(Box::new(StubIndexer::new(id)));
}

#[given(regex = r#"^a FlareSolverr solver that answers with the fixture "([^"]+)"$"#)]
async fn solver(w: &mut World, fixture: String) {
    w.start_server("solver").await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string(fixtures::body(&fixture)),
        )
        .mount(w.server("solver"))
        .await;
}

async fn build_cardigann(
    w: &mut World,
    def_name: &str,
    overrides: BTreeMap<String, String>,
    secret: Option<&str>,
    with_solver: bool,
) {
    let tracker = w.start_server("tracker").await;
    let mut def = skadi_cardigann::parse_definition(&fixtures::definition(def_name))
        .expect("fixture definition parses");
    def.links = vec![tracker];
    let solver_url = if with_solver {
        Some(w.server("solver").uri())
    } else {
        None
    };
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(Box::new(CardigannIndexer::new(
        id,
        Arc::new(def),
        &overrides,
        secret,
        solver_url.as_deref(),
        None,
    )));
}

#[given(regex = r#"^a cardigann indexer from the "([^"]+)" definition$"#)]
async fn cardigann(w: &mut World, def: String) {
    build_cardigann(w, &def, BTreeMap::new(), None, false).await;
}

#[given(regex = r#"^a cardigann indexer from the "([^"]+)" definition using the solver$"#)]
async fn cardigann_with_solver(w: &mut World, def: String) {
    build_cardigann(w, &def, BTreeMap::new(), None, true).await;
}

#[given(
    regex = r#"^a cardigann indexer from the "([^"]+)" definition with username "([^"]*)" and password "([^"]*)"$"#
)]
async fn cardigann_login(w: &mut World, def: String, user: String, pass: String) {
    let overrides = BTreeMap::from([("username".to_string(), user)]);
    let secret = serde_json::json!({ "password": pass }).to_string();
    build_cardigann(w, &def, overrides, Some(&secret), false).await;
}

/// Private-tracker mocks: the login page, the form POST that sets the session
/// cookie, the `login.test` page, and a search that only answers with the cookie.
#[given(
    regex = r#"^the tracker requires a login session and its account page shows "(logged in|logged out)"$"#
)]
async fn tracker_login(w: &mut World, state: String) {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, ResponseTemplate};
    let s = w.server("tracker");
    Mock::given(method("GET"))
        .and(path("/login.php"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixtures::body("login_form")))
        .mount(s)
        .await;
    Mock::given(method("POST"))
        .and(path("/takelogin.php"))
        .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "sess=ok; Path=/"))
        .mount(s)
        .await;
    let page = if state == "logged in" {
        "logged_in"
    } else {
        "not_logged_in"
    };
    Mock::given(method("GET"))
        .and(path("/account.php"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixtures::body(page)))
        .mount(s)
        .await;
    Mock::given(method("GET"))
        .and(path("/search.php"))
        .and(header("cookie", "sess=ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"name":"Some Movie 2020 1080p","hash":"DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF","size":"1000"}]"#,
        ))
        .mount(s)
        .await;
    Mock::given(method("GET"))
        .and(path("/search.php"))
        .respond_with(ResponseTemplate::new(403).set_body_string("no session"))
        .mount(s)
        .await;
}

// --- the raw cardigann fetcher (challenge handling) ------------------------------

#[when(
    regex = r#"^the cardigann fetcher (with|without) the solver fetches "([^"]+)" from the tracker$"#
)]
async fn fetcher_fetch(w: &mut World, with: String, p: String) {
    let solver = if with == "with" {
        Some(w.server("solver").uri())
    } else {
        None
    };
    let fetcher = HttpFetcher::new(solver.as_deref(), None);
    let url = format!("{}{}", w.server("tracker").uri(), p);
    let resp = fetcher
        .fetch(FetchReq {
            method: Method::Get,
            url,
            headers: Vec::new(),
            body: None,
        })
        .await
        .expect("fetch");
    w.fetch_resp = Some((resp.status, resp.body));
}

#[then(regex = r#"^the fetched page has status (\d+) and contains "([^"]*)"$"#)]
fn fetched(w: &mut World, status: u16, needle: String) {
    let (s, body) = w.fetch_resp.as_ref().expect("fetch response");
    assert_eq!(*s, status);
    assert!(body.contains(&needle), "body lacks {needle:?}: {body}");
}

#[then("the fetched page is still an unsolved challenge")]
fn fetched_challenge(w: &mut World) {
    let (s, body) = w.fetch_resp.as_ref().expect("fetch response");
    assert!(is_challenge(*s, body), "status {s}, body {body}");
}

#[then(regex = r"^the fetched page (is|is not) recognised as a challenge$")]
fn recognised(w: &mut World, yes: String) {
    let (s, body) = w.fetch_resp.as_ref().expect("fetch response");
    assert_eq!(is_challenge(*s, body), yes == "is");
}
