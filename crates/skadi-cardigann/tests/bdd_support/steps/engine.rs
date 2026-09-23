//! Definition parsing, the search executor, login, download resolution, the
//! filter pipeline and the template engine — all against a scripted fetcher.
use std::collections::BTreeMap;

use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use skadi_cardigann::filters::{self, FilterCtx};
use skadi_cardigann::template::{self, TemplateContext, Value as TVal};
use skadi_cardigann::{LoginOutcome, load_all, parse_definition, resolve_download};

use crate::bdd_support::{World, now};

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("fixture {}: {e}", p.display()))
}

// --- definitions -----------------------------------------------------------------

#[given("the definition:")]
fn definition(w: &mut World, step: &Step) {
    let yaml = step.docstring().expect("a YAML doc string");
    match parse_definition(yaml) {
        Ok(d) => {
            w.def = Some(d);
            w.load_error = None;
        }
        Err(e) => {
            w.def = None;
            w.load_error = Some(e);
        }
    }
}

#[given(regex = r#"^the real "([^"]+)" definition from the fixture library$"#)]
fn real_definition(w: &mut World, name: String) {
    w.def = Some(parse_definition(&fixture(&format!("{name}.yml"))).expect("real def parses"));
}

#[then("the definition parses")]
fn parses(w: &mut World) {
    assert!(w.def.is_some(), "parse failed: {:?}", w.load_error);
}

#[then(
    regex = r#"^the definition is rejected for id "([^"]*)" with a reason mentioning "([^"]*)"$"#
)]
fn rejected(w: &mut World, id: String, needle: String) {
    let e = w.load_error.as_ref().expect("expected a load error");
    assert_eq!(e.id.as_deref(), Some(id.as_str()));
    assert!(e.message.contains(&needle), "{e}");
}

#[then(regex = r"^the definition (needs|does not need) a login$")]
fn needs_login(w: &mut World, yes: String) {
    assert_eq!(w.def().needs_login(), yes == "needs");
}

#[then(regex = r#"^the definition maps tracker category "([^"]*)" to "([^"]*)"$"#)]
fn category_pair(w: &mut World, id: String, cat: String) {
    let pairs = w.def().caps.category_pairs();
    assert!(pairs.contains(&(id.clone(), cat.clone())), "{pairs:?}");
}

#[then(
    "a batch of one good and one malformed definition loads the good one and reports the bad one"
)]
fn batch(_w: &mut World) {
    let good = "id: t\nname: T\ncaps: {}\nsearch:\n  rows:\n    selector: tr\n";
    let bad = "id: b\nname: B\nsettings: not-a-list\n";
    let (ok, err) = load_all([good, bad]);
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].id, "t");
    assert_eq!(err.len(), 1);
    assert_eq!(err[0].id.as_deref(), Some("b"));
}

// --- the scripted tracker ------------------------------------------------------------

#[given(regex = r#"^the tracker answers URLs containing "([^"]+)" with HTTP (\d+) and body:$"#)]
fn route_body(w: &mut World, frag: String, status: u16, step: &Step) {
    let body = step.docstring().expect("a body").to_string();
    w.tracker.routes.push((frag, status, body));
}

#[given(regex = r#"^the tracker answers URLs containing "([^"]+)" with HTTP (\d+)$"#)]
fn route_status(w: &mut World, frag: String, status: u16) {
    w.tracker.routes.push((frag, status, String::new()));
}

#[given(
    regex = r#"^the tracker answers URLs containing "([^"]+)" with the recorded response "([^"]+)"$"#
)]
fn route_fixture(w: &mut World, frag: String, name: String) {
    w.tracker
        .routes
        .push((frag, 200, fixture(&format!("responses/{name}"))));
}

// --- search input -------------------------------------------------------------------

#[given(regex = r#"^the search keywords "([^"]*)"$"#)]
fn keywords(w: &mut World, kw: String) {
    w.keywords = kw;
}

#[given(regex = r#"^the search categories "([^"]*)"$"#)]
fn categories(w: &mut World, cats: String) {
    w.categories = cats
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
}

#[given(regex = r#"^the search query param "([^"]+)" is "([^"]*)"$"#)]
fn query_param(w: &mut World, k: String, v: String) {
    w.query.insert(k, v);
}

#[given(regex = r#"^the config override "([^"]+)" is "([^"]*)"$"#)]
fn override_(w: &mut World, k: String, v: String) {
    w.overrides.insert(k, v);
}

#[given(regex = r#"^the site base URL "([^"]*)"$"#)]
fn base_url(w: &mut World, url: String) {
    w.base_url = url;
}

// --- running the engine --------------------------------------------------------------

#[when("the engine runs the search")]
async fn run_search(w: &mut World) {
    let input = w.input();
    match skadi_cardigann::search(w.def(), &input, &w.tracker, now()).await {
        Ok(r) => {
            w.releases = r;
            w.error = None;
        }
        Err(e) => {
            w.releases.clear();
            w.error = Some(e.to_string());
        }
    }
}

#[when("the engine logs in")]
async fn run_login(w: &mut World) {
    let config = skadi_cardigann::engine::resolve_config(w.def(), &w.overrides);
    let query = w.query.clone();
    let base = w.base_url.clone();
    match skadi_cardigann::login(w.def(), &config, &query, &base, &w.tracker, now()).await {
        Ok(o) => {
            w.login = Some(o);
            w.error = None;
        }
        Err(e) => {
            w.login = None;
            w.error = Some(e.to_string());
        }
    }
}

#[when(regex = r#"^the engine resolves the download URL "([^"]*)"$"#)]
async fn run_resolve(w: &mut World, url: String) {
    match resolve_download(
        w.def(),
        &url,
        &w.tracker,
        now(),
        &std::collections::BTreeMap::new(),
    )
    .await
    {
        Ok(r) => {
            w.resolved = Some(r);
            w.error = None;
        }
        Err(e) => {
            w.resolved = None;
            w.error = Some(e.to_string());
        }
    }
}

// --- assertions ------------------------------------------------------------------------

#[then(regex = r"^(\d+) releases? (?:is|are) extracted$")]
fn n_releases(w: &mut World, n: usize) {
    assert!(w.error.is_none(), "search failed: {:?}", w.error);
    assert_eq!(
        w.releases.len(),
        n,
        "titles: {:?}",
        w.releases.iter().map(|r| &r.title).collect::<Vec<_>>()
    );
}

#[then(regex = r"^the (?:search|login|resolution) fails$")]
fn fails(w: &mut World) {
    assert!(w.error.is_some(), "expected a failure");
}

#[then(regex = r#"^the (?:search|login|resolution) fails with an error containing "([^"]*)"$"#)]
fn fails_with(w: &mut World, needle: String) {
    let e = w.error.as_deref().expect("expected a failure");
    assert!(e.contains(&needle), "error {e:?} lacks {needle:?}");
}

#[then(regex = r#"^release (\d+) has title "([^"]*)"$"#)]
fn title(w: &mut World, n: usize, t: String) {
    assert_eq!(w.release(n).title, t);
}

#[then(regex = r#"^release (\d+) has download "([^"]*)"$"#)]
fn download(w: &mut World, n: usize, d: String) {
    assert_eq!(w.release(n).download.as_deref(), Some(d.as_str()));
}

#[then(regex = r#"^release (\d+) has info hash "([^"]*)"$"#)]
fn infohash(w: &mut World, n: usize, h: String) {
    assert_eq!(w.release(n).infohash.as_deref(), Some(h.as_str()));
}

#[then(regex = r#"^release (\d+) has magnet containing "([^"]*)"$"#)]
fn magnet(w: &mut World, n: usize, needle: String) {
    let m = w.release(n).magnet.as_deref().expect("magnet");
    assert!(m.contains(&needle), "{m}");
}

#[then(regex = r"^release (\d+) has size (\d+)$")]
fn size(w: &mut World, n: usize, s: u64) {
    assert_eq!(w.release(n).size, Some(s));
}

#[then(regex = r"^release (\d+) has (\d+) seeders and (\d+) leechers$")]
fn peers(w: &mut World, n: usize, s: u64, l: u64) {
    assert_eq!(w.release(n).seeders, Some(s));
    assert_eq!(w.release(n).leechers, Some(l));
}

#[then(regex = r#"^release (\d+) has categories "([^"]*)"$"#)]
fn cats(w: &mut World, n: usize, list: String) {
    let want: Vec<String> = list
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    assert_eq!(w.release(n).categories, want);
}

#[then(regex = r#"^release (\d+) has date "([^"]*)"$"#)]
fn date(w: &mut World, n: usize, d: String) {
    assert_eq!(w.release(n).date.as_deref(), Some(d.as_str()));
}

#[then(regex = r#"^release (\d+) has imdb "([^"]*)"$"#)]
fn imdb(w: &mut World, n: usize, id: String) {
    assert_eq!(w.release(n).imdb.as_deref(), Some(id.as_str()));
}

#[then(regex = r#"^the tracker was asked for a URL containing "([^"]*)"$"#)]
fn asked(w: &mut World, needle: String) {
    let urls: Vec<String> = w.tracker.calls().into_iter().map(|c| c.url).collect();
    assert!(
        urls.iter().any(|u| u.contains(&needle)),
        "no request URL contains {needle:?}; saw {urls:?}"
    );
}

#[then(regex = r#"^the tracker was not asked for a URL containing "([^"]*)"$"#)]
fn not_asked(w: &mut World, needle: String) {
    let urls: Vec<String> = w.tracker.calls().into_iter().map(|c| c.url).collect();
    assert!(
        !urls.iter().any(|u| u.contains(&needle)),
        "a request URL contains {needle:?}: {urls:?}"
    );
}

#[then(regex = r"^the tracker received (\d+) requests?$")]
fn n_requests(w: &mut World, n: usize) {
    assert_eq!(w.tracker.calls().len(), n);
}

#[then(regex = r#"^the tracker received a POST whose body contains "([^"]*)"$"#)]
fn post_body(w: &mut World, needle: String) {
    let posts = w.tracker.posts();
    assert!(
        posts
            .iter()
            .any(|p| p.body.as_deref().is_some_and(|b| b.contains(&needle))),
        "no POST body contains {needle:?}; posts: {posts:?}"
    );
}

#[then(regex = r#"^the tracker received a request with header "([^"]+)" = "([^"]*)"$"#)]
fn header(w: &mut World, k: String, v: String) {
    let calls = w.tracker.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.headers.iter().any(|(hk, hv)| *hk == k && *hv == v)),
        "no request carried {k}: {v}; saw {calls:?}"
    );
}

#[then(regex = r#"^the login outcome is "(ok|failed|not required)"$"#)]
fn login_outcome(w: &mut World, want: String) {
    let got = match w.login.as_ref().expect("a login outcome") {
        LoginOutcome::Ok => "ok",
        LoginOutcome::Failed(_) => "failed",
        LoginOutcome::NotRequired => "not required",
    };
    assert_eq!(got, want, "{:?}", w.login);
}

#[then(regex = r#"^the login failed with a reason containing "([^"]*)"$"#)]
fn login_reason(w: &mut World, needle: String) {
    match w.login.as_ref() {
        Some(LoginOutcome::Failed(r)) => assert!(r.contains(&needle), "{r}"),
        other => panic!("expected a failed login, got {other:?}"),
    }
}

#[then(regex = r#"^the resolved magnet is "([^"]*)"$"#)]
fn resolved(w: &mut World, want: String) {
    assert_eq!(
        w.resolved.as_ref().and_then(|r| r.as_deref()),
        Some(want.as_str()),
        "{:?}",
        w.error
    );
}

#[then("no download resolution applies")]
fn no_resolution(w: &mut World) {
    assert_eq!(w.resolved, Some(None), "{:?}", w.error);
}

// --- filters + templates (pure) ---------------------------------------------------------------

#[then(regex = r#"^the filter "([^"]+)" with args (.+) maps "([^"]*)" to "([^"]*)"$"#)]
fn filter(_w: &mut World, name: String, args: String, input: String, want: String) {
    let args: serde_yaml::Value = serde_yaml::from_str(&args).expect("YAML args");
    let got = filters::apply(&name, &args, &input, &FilterCtx { now: now() })
        .unwrap_or_else(|e| panic!("filter {name} failed: {e}"));
    assert_eq!(got, want);
}

#[then(regex = r#"^the template "(.+)" renders "([^"]*)"$"#)]
fn render(w: &mut World, tpl: String, want: String) {
    let mut config: BTreeMap<String, TVal> = BTreeMap::new();
    for (k, v) in &w.overrides {
        let val = match v.as_str() {
            "true" => TVal::Bool(true),
            "false" => TVal::Bool(false),
            s => TVal::Str(s.to_string()),
        };
        config.insert(k.clone(), val);
    }
    let ctx = TemplateContext {
        keywords: w.keywords.clone(),
        categories: w.categories.clone(),
        config,
        query: w.query.clone(),
        result: BTreeMap::new(),
    };
    let got = template::render(&tpl, &ctx).unwrap_or_else(|e| panic!("render failed: {e}"));
    assert_eq!(got, want);
}

// --- catalog -------------------------------------------------------------------------------------

#[given("a definitions directory holding two valid definitions and one malformed file")]
fn defs_dir(w: &mut World) {
    // pid + counter, not pid + timestamp (SKADI-T-0530). A local counter rather
    // than `skadi_core::unique_temp_path`: this crate does not depend on
    // skadi-core, and adding one for a test path would be the wrong trade.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "skadi-cardigann-bdd-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(
        dir.join("alpha.yml"),
        "id: alpha\nname: Alpha\ntype: public\nlanguage: en-US\ndescription: A\nlinks: [https://alpha.test/]\ncaps:\n  categorymappings: [{id: '1', cat: Movies}]\n  modes: {search: [q]}\nsearch:\n  rows: {selector: tr}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("nested/beta.yaml"),
        "id: beta\nname: Beta\ntype: private\nsettings:\n  - {name: username, type: text, label: Username}\n  - {name: password, type: password, label: Password}\nlogin:\n  path: login.php\n  method: form\n  form: form\nsearch:\n  rows: {selector: tr}\n",
    )
    .unwrap();
    std::fs::write(dir.join("broken.yml"), "id: broken\nname: B\ncaps: 42\n").unwrap();
    std::fs::write(dir.join("README.md"), "not a definition").unwrap();
    let (catalog, errors) = skadi_cardigann::Catalog::load_dir(&dir).expect("load_dir");
    w.catalog = Some(catalog);
    w.catalog_errors = errors;
}

#[then(regex = r#"^the catalog lists "([^"]*)" and reports (\d+) load errors?$"#)]
fn catalog_lists(w: &mut World, ids: String, n: usize) {
    let c = w.catalog.as_ref().expect("catalog");
    let got: Vec<&str> = c.list().iter().map(|e| e.id.as_str()).collect();
    let want: Vec<&str> = ids.split(',').map(str::trim).collect();
    assert_eq!(got, want);
    assert_eq!(w.catalog_errors.len(), n, "{:?}", w.catalog_errors);
}

#[then(regex = r#"^the catalog entry "([^"]+)" is "([^"]+)" and offers the setting "([^"]+)"$"#)]
fn catalog_entry(w: &mut World, id: String, privacy: String, setting: String) {
    let c = w.catalog.as_ref().expect("catalog");
    let e = c.entry(&id).expect("entry");
    assert_eq!(e.privacy, privacy);
    assert!(
        e.settings.iter().any(|s| s.name == setting),
        "settings: {:?}",
        e.settings
    );
    assert!(c.get(&id).is_some(), "definition retrievable for building");
}

#[then(regex = r"^release (\d+) has an info hash and a size$")]
fn hash_and_size(w: &mut World, n: usize) {
    let r = w.release(n);
    assert!(
        r.infohash.as_deref().is_some_and(|h| !h.is_empty()),
        "{r:?}"
    );
    assert!(r.size.unwrap_or(0) > 0, "{r:?}");
}
