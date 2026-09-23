//! Shared steps: assembling a domain query, mounting canned provider responses on
//! the in-process mock servers, driving the `Indexer` trait, and asserting on the
//! returned `Release`s.
use cucumber::{given, then, when};
use skadi_core::{ImdbId, MediaKind, TmdbId, TvdbId};
use skadi_indexers::{Category, ReleaseFetch, SearchMode};
use wiremock::matchers::{path, query_param};
use wiremock::{Mock, ResponseTemplate};

use crate::bdd_support::{World, fixtures};

fn kind_of(s: &str) -> MediaKind {
    match s {
        "movie" => MediaKind::Movie,
        "series" => MediaKind::Series,
        "audiobook" => MediaKind::Audiobook,
        "music" => MediaKind::Music,
        "book" => MediaKind::Book,
        other => panic!("unknown media kind {other:?}"),
    }
}

// --- query assembly -----------------------------------------------------------

#[given(regex = r#"^a "(movie|series|audiobook|music|book)" query for "([^"]*)"$"#)]
fn query_for(w: &mut World, kind: String, title: String) {
    w.query.kind = kind_of(&kind);
    w.query.titles = vec![title];
}

#[given(regex = r#"^a "(movie|series|audiobook)" query for "([^"]*)" \((\d{4})\)$"#)]
fn query_for_year(w: &mut World, kind: String, title: String, year: u16) {
    w.query.kind = kind_of(&kind);
    w.query.titles = vec![title];
    w.query.year = Some(year);
}

#[given(regex = r#"^the query also has the alias "([^"]*)"$"#)]
fn query_alias(w: &mut World, alias: String) {
    w.query.titles.push(alias);
}

#[given(regex = r#"^the query carries imdb id "([^"]*)"$"#)]
fn query_imdb(w: &mut World, id: String) {
    w.query.ids.imdb = Some(ImdbId(id));
}

#[given(regex = r"^the query carries tmdb id (\d+)$")]
fn query_tmdb(w: &mut World, id: u64) {
    w.query.ids.tmdb = Some(TmdbId(id));
}

#[given(regex = r"^the query carries tvdb id (\d+)$")]
fn query_tvdb(w: &mut World, id: u64) {
    w.query.ids.tvdb = Some(TvdbId(id));
}

#[given(regex = r"^the query asks for season (\d+) episode (\d+)$")]
fn query_season_ep(w: &mut World, season: u32, ep: u32) {
    w.query.extra = vec![("season", season.to_string()), ("ep", ep.to_string())];
}

#[given(regex = r#"^the query is scoped to categories "([^"]*)"$"#)]
fn query_cats(w: &mut World, cats: String) {
    w.query.cats = parse_cats(&cats);
}

#[given(regex = r#"^the query mode is "(auto|id-only|title-only)"$"#)]
fn query_mode(w: &mut World, mode: String) {
    w.query.mode = match mode.as_str() {
        "id-only" => SearchMode::IdOnly,
        "title-only" => SearchMode::TitleOnly,
        _ => SearchMode::Auto,
    };
}

pub fn parse_cats(s: &str) -> Vec<Category> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Category(s.parse().expect("category id")))
        .collect()
}

// --- canned provider responses --------------------------------------------------

/// `"t=caps"` → query params on `/api`; `"/api/v1/search"` → that path;
/// `"/api/v1/search?query="` → path + query params.
fn mount_spec(spec: &str) -> (String, Vec<(String, String)>) {
    let (p, q) = if let Some(rest) = spec.strip_prefix('/') {
        match rest.split_once('?') {
            Some((p, q)) => (format!("/{p}"), q.to_string()),
            None => (format!("/{rest}"), String::new()),
        }
    } else {
        ("/api".to_string(), spec.to_string())
    };
    let pairs = q
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    (p, pairs)
}

async fn mount(w: &mut World, server: &str, spec: &str, status: u16, body: Option<String>) {
    let (p, pairs) = mount_spec(spec);
    let mut m = Mock::given(path(p));
    for (k, v) in pairs {
        m = m.and(query_param(k, v));
    }
    let mut resp = ResponseTemplate::new(status);
    if let Some(b) = body {
        resp = resp.set_body_string(b);
    }
    m.respond_with(resp).mount(w.server(server)).await;
}

#[given(regex = r#"^the (indexer|tracker|solver) answers "([^"]+)" with the fixture "([^"]+)"$"#)]
async fn answers_fixture(w: &mut World, server: String, spec: String, fixture: String) {
    mount(w, &server, &spec, 200, Some(fixtures::body(&fixture))).await;
}

#[given(regex = r#"^the (indexer|tracker|solver) answers "([^"]+)" with HTTP (\d+)$"#)]
async fn answers_status(w: &mut World, server: String, spec: String, status: u16) {
    mount(w, &server, &spec, status, None).await;
}

#[given(
    regex = r#"^the (indexer|tracker|solver) answers "([^"]+)" with HTTP (\d+) and the fixture "([^"]+)"$"#
)]
async fn answers_status_fixture(
    w: &mut World,
    server: String,
    spec: String,
    status: u16,
    fixture: String,
) {
    mount(w, &server, &spec, status, Some(fixtures::body(&fixture))).await;
}

// --- driving the Indexer trait ----------------------------------------------------

#[when("the hunter searches the indexer")]
async fn search(w: &mut World) {
    let q = w.query.clone();
    match w.indexer().search(&q).await {
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

#[when(regex = r"^the hunter searches the indexer (\d+) times$")]
async fn search_n(w: &mut World, n: usize) {
    for _ in 0..n {
        search(w).await;
    }
}

#[when("the hunter pulls the RSS feed")]
async fn rss(w: &mut World) {
    match w.indexer().rss().await {
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

#[when("the indexer capabilities are negotiated")]
async fn caps(w: &mut World) {
    match w.indexer().capabilities().await {
        Ok(c) => {
            w.caps = Some(c);
            w.error = None;
        }
        Err(e) => {
            w.caps = None;
            w.error = Some(e.to_string());
        }
    }
}

#[when("the indexer health check runs")]
async fn test(w: &mut World) {
    match w.indexer().test().await {
        Ok(()) => {
            w.test_ok = Some(true);
            w.error = None;
        }
        Err(e) => {
            w.test_ok = Some(false);
            w.error = Some(e.to_string());
        }
    }
}

#[when(regex = r"^the grab resolves release (\d+)'s fetch$")]
async fn resolve(w: &mut World, n: usize) {
    let fetch = w.release(n).fetch.clone();
    match w.indexer().resolve_fetch(&fetch).await {
        Ok(f) => {
            w.resolved = Some(f);
            w.error = None;
        }
        Err(e) => {
            w.resolved = None;
            w.error = Some(e.to_string());
        }
    }
}

// --- assertions -------------------------------------------------------------------

#[then(regex = r"^(\d+) releases? (?:is|are) returned$")]
fn n_releases(w: &mut World, n: usize) {
    assert!(
        w.error.is_none(),
        "expected releases but the call failed: {:?}",
        w.error
    );
    assert_eq!(
        w.releases.len(),
        n,
        "titles: {:?}",
        w.releases.iter().map(|r| &r.title).collect::<Vec<_>>()
    );
}

#[then(regex = r"^the (?:search|RSS pull|health check|capability negotiation|grab) fails$")]
fn fails(w: &mut World) {
    assert!(
        w.error.is_some(),
        "expected a failure but got {} release(s) / ok",
        w.releases.len()
    );
}

#[then(
    regex = r#"^the (?:search|RSS pull|health check|capability negotiation|grab) fails with an error containing "([^"]*)"$"#
)]
fn fails_with(w: &mut World, needle: String) {
    let err = w.error.as_deref().expect("expected a failure");
    assert!(err.contains(&needle), "error {err:?} lacks {needle:?}");
}

#[then("the health check passes")]
fn health_ok(w: &mut World) {
    assert_eq!(w.test_ok, Some(true), "health check failed: {:?}", w.error);
}

#[then(regex = r#"^release (\d+) has title "([^"]*)"$"#)]
fn rel_title(w: &mut World, n: usize, title: String) {
    assert_eq!(w.release(n).title, title);
}

#[then(regex = r"^release (\d+) has (\d+) seeders$")]
fn rel_seeders(w: &mut World, n: usize, seeders: u32) {
    assert_eq!(w.release(n).seeders, Some(seeders));
}

#[then(regex = r"^release (\d+) has unknown seeders$")]
fn rel_no_seeders(w: &mut World, n: usize) {
    assert_eq!(w.release(n).seeders, None);
}

#[then(regex = r"^release (\d+) has size (\d+)$")]
fn rel_size(w: &mut World, n: usize, size: u64) {
    assert_eq!(w.release(n).size, size);
}

#[then(regex = r#"^release (\d+) was published at "([^"]*)"$"#)]
fn rel_published(w: &mut World, n: usize, at: String) {
    assert_eq!(w.release(n).published.to_rfc3339(), at);
}

#[then(regex = r"^release (\d+) was published within the last minute$")]
fn rel_published_now(w: &mut World, n: usize) {
    let age = chrono::Utc::now() - w.release(n).published;
    assert!(age.num_seconds().abs() < 60, "published {age} ago");
}

#[then(regex = r#"^release (\d+)'s fetch is a magnet containing "([^"]*)"$"#)]
fn rel_magnet(w: &mut World, n: usize, needle: String) {
    match &w.release(n).fetch {
        ReleaseFetch::Magnet(m) => assert!(m.contains(&needle), "magnet {m:?} lacks {needle:?}"),
        other => panic!("expected a magnet, got {other:?}"),
    }
}

#[then(regex = r#"^release (\d+)'s fetch is a torrent URL containing "([^"]*)"$"#)]
fn rel_torrent(w: &mut World, n: usize, needle: String) {
    match &w.release(n).fetch {
        ReleaseFetch::TorrentUrl(u) => {
            assert!(u.contains(&needle), "url {u:?} lacks {needle:?}");
        }
        other => panic!("expected a torrent URL, got {other:?}"),
    }
}

#[then(regex = r#"^release (\d+) carries categories "([^"]*)"$"#)]
fn rel_cats(w: &mut World, n: usize, cats: String) {
    assert_eq!(w.release(n).categories, parse_cats(&cats));
}

#[then(regex = r#"^release (\d+) parsed as year (\d{4}) and resolution "([^"]*)"$"#)]
fn rel_parsed(w: &mut World, n: usize, year: u16, res: String) {
    let p = &w.release(n).parsed;
    assert_eq!(p.year, Some(year));
    assert_eq!(p.resolution.as_deref(), Some(res.as_str()));
}

#[then(
    regex = r#"^the (indexer|tracker|solver) received (\d+) requests? with "([^"=]+)=([^"]*)"$"#
)]
async fn received_with(w: &mut World, server: String, n: usize, key: String, value: String) {
    let got = w.requests_with(&server, &key, &value).await;
    assert_eq!(got, n, "requests carrying {key}={value}");
}

#[then(regex = r#"^the (indexer|tracker|solver) received (\d+) requests? to "([^"]+)"$"#)]
async fn received_to(w: &mut World, server: String, n: usize, p: String) {
    let got = w.requests_to(&server, &p).await;
    assert_eq!(got, n, "requests to {p}");
}

#[then(
    regex = r#"^the (indexer|tracker|solver) saw a request whose "([^"]+)" parameter contains "([^"]*)"$"#
)]
async fn saw_param(w: &mut World, server: String, key: String, needle: String) {
    let reqs = w
        .server(&server)
        .received_requests()
        .await
        .unwrap_or_default();
    let urls: Vec<String> = reqs.iter().map(|r| r.url.to_string()).collect();
    assert!(
        w.saw_param_containing(&server, &key, &needle).await,
        "no request with {key} containing {needle:?}; saw {urls:?}"
    );
}

#[then(
    regex = r#"^the (indexer|tracker|solver) saw no request whose "([^"]+)" parameter contains "([^"]*)"$"#
)]
async fn saw_no_param(w: &mut World, server: String, key: String, needle: String) {
    assert!(
        !w.saw_param_containing(&server, &key, &needle).await,
        "a request carried {key} containing {needle:?}"
    );
}

#[then(regex = r#"^the capabilities advertise id params "([^"]*)"$"#)]
fn caps_ids(w: &mut World, ids: String) {
    let caps = w.caps.as_ref().expect("caps");
    let got: Vec<String> = caps
        .id_params
        .iter()
        .map(|p| format!("{p:?}").to_lowercase())
        .collect();
    let want: Vec<String> = ids
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(got, want);
}

#[then(regex = r#"^the capabilities list categories "([^"]*)"$"#)]
fn caps_cats(w: &mut World, cats: String) {
    let caps = w.caps.as_ref().expect("caps");
    assert_eq!(caps.categories, parse_cats(&cats));
}

#[then(regex = r"^the capabilities (do|do not) support search$")]
fn caps_search(w: &mut World, yes: String) {
    let caps = w.caps.as_ref().expect("caps");
    assert_eq!(caps.supports_search, yes == "do");
}

#[then(regex = r"^the capabilities (do|do not) support RSS$")]
fn caps_rss(w: &mut World, yes: String) {
    let caps = w.caps.as_ref().expect("caps");
    assert_eq!(caps.supports_rss, yes == "do");
}

#[then(
    regex = r#"^the indexer (serves|does not serve) the "(movie|series|audiobook|music|book)" domain$"#
)]
fn serves(w: &mut World, yes: String, kind: String) {
    assert_eq!(w.indexer().supports(kind_of(&kind)), yes == "serves");
}

#[then(regex = r#"^the resolved fetch is a magnet containing "([^"]*)"$"#)]
fn resolved_magnet(w: &mut World, needle: String) {
    match &w.resolved {
        Some(ReleaseFetch::Magnet(m)) => {
            assert!(m.contains(&needle), "magnet {m:?} lacks {needle:?}");
        }
        other => panic!("expected a magnet, got {other:?} (error {:?})", w.error),
    }
}

#[then(regex = r#"^the resolved fetch is unchanged$"#)]
fn resolved_same(w: &mut World) {
    assert_eq!(w.resolved.as_ref(), Some(&w.release(1).fetch));
}

/// The grab-time contract behind the 2026-09-06 prod outage ("add failed: error
/// decoding torrent"): whatever the indexer hands the download client must be
/// fetchable by a client that has **no** tracker session and **no** FlareSolverr.
/// A bare `.torrent` URL behind a challenge (or a login cookie) is not — it must
/// be resolved to a magnet / torrent bytes here, or the grab must fail.
#[then("the grab does not hand the download client a bare .torrent URL")]
fn no_bare_torrent_url(w: &mut World) {
    assert!(
        !matches!(w.resolved, Some(ReleaseFetch::TorrentUrl(_))),
        "grab yielded a bare .torrent URL the worker would fetch unauthenticated: {:?}",
        w.resolved
    );
}
