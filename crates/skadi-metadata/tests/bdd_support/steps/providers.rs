//! Metadata providers (TMDB, Servarr, Skyhook, Audnexus, Audible catalog)
//! against an in-process mock of each upstream API.
use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use skadi_core::{AsinId, ImdbId, MediaKind, TmdbId, TvdbId};
use skadi_metadata::{
    AudibleCatalogProvider, AudnexusProvider, ExternalId, ImageKind, MetadataQuery,
    ServarrProvider, SkyhookProvider, TmdbProvider,
};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::bdd_support::{Provider, World, fast_http};

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

async fn start(w: &mut World) -> String {
    let s = MockServer::start().await;
    let uri = s.uri();
    w.server = Some(s);
    uri
}

#[given(regex = r#"^a TMDB provider with API key "([^"]*)"$"#)]
async fn tmdb(w: &mut World, key: String) {
    let uri = start(w).await;
    w.provider = Some(Provider::Tmdb(
        TmdbProvider::new(key, fast_http()).with_base_url(uri),
    ));
}

#[given("a Servarr metadata provider")]
async fn servarr(w: &mut World) {
    let uri = start(w).await;
    w.provider = Some(Provider::Servarr(
        ServarrProvider::new(fast_http()).with_base_url(uri),
    ));
}

#[given("a Skyhook provider")]
async fn skyhook(w: &mut World) {
    let uri = start(w).await;
    w.provider = Some(Provider::Skyhook(
        SkyhookProvider::new(fast_http()).with_base_url(uri),
    ));
}

#[given("an Audnexus provider")]
async fn audnexus(w: &mut World) {
    let uri = start(w).await;
    w.provider = Some(Provider::Audnexus(
        AudnexusProvider::new(fast_http()).with_base_url(uri),
    ));
}

#[given("an Audible catalog provider")]
async fn audible(w: &mut World) {
    let uri = start(w).await;
    w.provider = Some(Provider::Audible(
        AudibleCatalogProvider::new(fast_http()).with_base_url(uri),
    ));
}

// --- canned upstream answers ------------------------------------------------------------

fn split_spec(spec: &str) -> (String, Vec<(String, String)>) {
    let (p, q) = spec.split_once('?').unwrap_or((spec, ""));
    let pairs = q
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    (p.to_string(), pairs)
}

fn mock_for(spec: &str) -> wiremock::MockBuilder {
    let (p, pairs) = split_spec(spec);
    let mut m = Mock::given(method("GET")).and(path(p));
    for (k, v) in pairs {
        m = m.and(query_param(k, v));
    }
    m
}

#[given(regex = r#"^the upstream answers "([^"]+)" with JSON:$"#)]
async fn answers_json(w: &mut World, spec: String, step: &Step) {
    let body: serde_json::Value =
        serde_json::from_str(step.docstring().expect("JSON")).expect("valid JSON");
    mock_for(&spec)
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(w.server())
        .await;
}

#[given(regex = r#"^the upstream answers "([^"]+)" with HTTP (\d+)$"#)]
async fn answers_status(w: &mut World, spec: String, status: u16) {
    mock_for(&spec)
        .respond_with(ResponseTemplate::new(status))
        .mount(w.server())
        .await;
}

#[given(regex = r#"^the upstream answers "([^"]+)" with HTTP (\d+) and body "([^"]*)"$"#)]
async fn answers_body(w: &mut World, spec: String, status: u16, body: String) {
    mock_for(&spec)
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(w.server())
        .await;
}

#[given(
    regex = r#"^the upstream answers "([^"]+)" with HTTP (\d+) and Retry-After (\d+) once, then with JSON:$"#
)]
async fn answers_retry_after(w: &mut World, spec: String, status: u16, secs: u64, step: &Step) {
    mock_for(&spec)
        .respond_with(ResponseTemplate::new(status).insert_header("retry-after", secs.to_string()))
        .up_to_n_times(1)
        .mount(w.server())
        .await;
    let body: serde_json::Value =
        serde_json::from_str(step.docstring().expect("JSON")).expect("valid JSON");
    mock_for(&spec)
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(w.server())
        .await;
}

// --- driving the providers ---------------------------------------------------------------

fn clear(w: &mut World) {
    w.error = None;
    w.matches.clear();
    w.record = None;
    w.series = None;
    w.started = Some(std::time::Instant::now());
}

fn done(w: &mut World) {
    w.elapsed = w.started.map(|s| s.elapsed());
}

#[when(regex = r#"^the user searches for "([^"]*)" \((\d{4})\) as a "([a-z]+)"$"#)]
async fn search_year(w: &mut World, title: String, year: u16, kind: String) {
    clear(w);
    let q = MetadataQuery {
        title,
        year: Some(year),
        kind: kind_of(&kind),
    };
    let r = match w.provider() {
        Provider::Tmdb(p) if q.kind == MediaKind::Series => p.search_series(&q).await,
        Provider::Skyhook(p) => p.search_series(&q).await,
        other => other.common().search(&q).await,
    };
    match r {
        Ok(m) => w.matches = m,
        Err(e) => w.error = Some(e.to_string()),
    }
    done(w);
}

#[when(regex = r#"^the user searches for "([^"]*)" as a "([a-z]+)"$"#)]
async fn search(w: &mut World, title: String, kind: String) {
    clear(w);
    let q = MetadataQuery {
        title,
        year: None,
        kind: kind_of(&kind),
    };
    let r = match w.provider() {
        Provider::Skyhook(p) => p.search_series(&q).await,
        other => other.common().search(&q).await,
    };
    match r {
        Ok(m) => w.matches = m,
        Err(e) => w.error = Some(e.to_string()),
    }
    done(w);
}

async fn lookup(w: &mut World, id: ExternalId) {
    clear(w);
    match w.provider().common().lookup(&id).await {
        Ok(r) => w.record = Some(r),
        Err(e) => w.error = Some(e.to_string()),
    }
    done(w);
}

#[when(regex = r"^the movie (\d+) is looked up by TMDB id$")]
async fn lookup_tmdb(w: &mut World, id: u64) {
    lookup(w, ExternalId::Tmdb(TmdbId(id))).await;
}

#[when(regex = r#"^the movie "([^"]*)" is looked up by IMDb id$"#)]
async fn lookup_imdb(w: &mut World, id: String) {
    lookup(w, ExternalId::Imdb(ImdbId(id))).await;
}

#[when(regex = r#"^the item "([^"]*)" is looked up by ASIN$"#)]
async fn lookup_asin(w: &mut World, asin: String) {
    lookup(w, ExternalId::Asin(AsinId(asin))).await;
}

#[when(regex = r"^the item (\d+) is looked up by TVDB id$")]
async fn lookup_tvdb(w: &mut World, id: u64) {
    lookup(w, ExternalId::Tvdb(TvdbId(id))).await;
}

#[when(regex = r"^the movie (\d+) is refreshed$")]
async fn refresh(w: &mut World, id: u64) {
    clear(w);
    match w
        .provider()
        .common()
        .refresh(&ExternalId::Tmdb(TmdbId(id)))
        .await
    {
        Ok(r) => w.record = Some(r),
        Err(e) => w.error = Some(e.to_string()),
    }
    done(w);
}

#[when(regex = r"^the series (\d+) is looked up with its seasons and episodes$")]
async fn lookup_series(w: &mut World, id: u64) {
    clear(w);
    let r = match w.provider() {
        Provider::Tmdb(p) => p.lookup_series(TmdbId(id)).await,
        Provider::Skyhook(p) => p.lookup_series(TvdbId(id)).await,
        _ => panic!("not a series provider"),
    };
    match r {
        Ok(s) => w.series = Some(s),
        Err(e) => w.error = Some(e.to_string()),
    }
    done(w);
}

#[when(regex = r#"^the author "([^"]*)" is looked up$"#)]
async fn lookup_author(w: &mut World, asin: String) {
    clear(w);
    let Provider::Audnexus(p) = w.provider() else {
        panic!("not audnexus")
    };
    match p.lookup_author(&AsinId(asin)).await {
        Ok(a) => w.author = Some(a),
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[when(regex = r#"^authors named "([^"]*)" are searched$"#)]
async fn search_authors(w: &mut World, name: String) {
    clear(w);
    let Provider::Audnexus(p) = w.provider() else {
        panic!("not audnexus")
    };
    match p.search_authors(&name).await {
        Ok(a) => w.authors = a,
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[when(regex = r#"^the catalog lists products by "([^"]*)"$"#)]
async fn list_by_author(w: &mut World, author: String) {
    clear(w);
    let Provider::Audible(p) = w.provider() else {
        panic!("not audible")
    };
    match p.list_by_author(&author).await {
        Ok(i) => w.items = i,
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[when(regex = r#"^the catalog is searched for "([^"]*)"$"#)]
async fn catalog_search(w: &mut World, kw: String) {
    clear(w);
    let Provider::Audible(p) = w.provider() else {
        panic!("not audible")
    };
    match p.search(&kw).await {
        Ok(i) => w.items = i,
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[when(regex = r#"^the language of product "([^"]*)" is fetched$"#)]
async fn product_language(w: &mut World, asin: String) {
    clear(w);
    let Provider::Audible(p) = w.provider() else {
        panic!("not audible")
    };
    match p.product_language(&AsinId(asin)).await {
        Ok(l) => w.language = Some(l),
        Err(e) => w.error = Some(e.to_string()),
    }
}

// --- assertions ---------------------------------------------------------------------------

#[then(regex = r"^(\d+) match(?:es)? (?:is|are) returned$")]
fn n_matches(w: &mut World, n: usize) {
    assert!(w.error.is_none(), "search failed: {:?}", w.error);
    assert_eq!(w.matches.len(), n, "{:?}", w.matches);
}

#[then(regex = r#"^match (\d+) is "([^"]*)" \((\d{4})\) with tmdb id (\d+)$"#)]
fn match_tmdb(w: &mut World, n: usize, title: String, year: u16, id: u64) {
    let m = &w.matches[n - 1];
    assert_eq!(m.title, title);
    assert_eq!(m.year, Some(year));
    assert_eq!(m.external_ids.tmdb, Some(TmdbId(id)));
}

#[then(regex = r#"^match (\d+) is "([^"]*)" \((\d{4})\) with tvdb id (\d+)$"#)]
fn match_tvdb(w: &mut World, n: usize, title: String, year: u16, id: u64) {
    let m = &w.matches[n - 1];
    assert_eq!(m.title, title);
    assert_eq!(m.year, Some(year));
    assert_eq!(m.external_ids.tvdb, Some(TvdbId(id)));
}

#[then(regex = r#"^match (\d+) has poster "([^"]*)" and an overview$"#)]
fn match_poster(w: &mut World, n: usize, poster: String) {
    let m = &w.matches[n - 1];
    assert_eq!(m.poster_url.as_deref(), Some(poster.as_str()));
    assert!(m.overview.as_deref().is_some_and(|o| !o.is_empty()));
}

#[then(regex = r"^match (\d+) has no poster$")]
fn match_no_poster(w: &mut World, n: usize) {
    assert_eq!(w.matches[n - 1].poster_url, None);
}

#[then(regex = r"^match (\d+) scores higher than match (\d+)$")]
fn match_score(w: &mut World, a: usize, b: usize) {
    assert!(w.matches[a - 1].score > w.matches[b - 1].score);
}

#[then(regex = r#"^the upstream received a request with "([^"]+)=([^"]*)"$"#)]
async fn received_param(w: &mut World, k: String, v: String) {
    let reqs = w.server().received_requests().await.unwrap_or_default();
    assert!(
        reqs.iter()
            .any(|r| r.url.query_pairs().any(|(qk, qv)| qk == k && qv == v)),
        "no request carried {k}={v}: {:?}",
        reqs.iter().map(|r| r.url.to_string()).collect::<Vec<_>>()
    );
}

#[then(regex = r#"^the upstream received (\d+) requests? to "([^"]+)"$"#)]
async fn received_n(w: &mut World, n: usize, p: String) {
    assert_eq!(w.requests_to(&p).await, n, "requests to {p}");
}

#[then(regex = r#"^the record is "([^"]*)" released (\d{4}-\d{2}-\d{2}) running (\d+) minutes$"#)]
fn record_core(w: &mut World, title: String, date: String, runtime: u32) {
    let r = w.record();
    assert_eq!(r.title, title);
    assert_eq!(r.release_date.map(|d| d.to_string()), Some(date));
    assert_eq!(r.runtime_minutes, Some(runtime));
}

#[then(regex = r#"^the record has tmdb id (\d+) and imdb id "([^"]*)"$"#)]
fn record_ids(w: &mut World, tmdb: u64, imdb: String) {
    let r = w.record();
    assert_eq!(r.external_ids.tmdb, Some(TmdbId(tmdb)));
    assert_eq!(
        r.external_ids.imdb.as_ref().map(|i| i.0.as_str()),
        Some(imdb.as_str())
    );
}

#[then(expr = "the record has content rating {string}")]
fn record_content_rating(w: &mut World, rating: String) {
    assert_eq!(w.record().content_rating.as_deref(), Some(rating.as_str()));
}

#[then(regex = r#"^the record has a poster "([^"]*)" and a backdrop "([^"]*)"$"#)]
fn record_images(w: &mut World, poster: String, backdrop: String) {
    let r = w.record();
    let find = |k: ImageKind| {
        r.images
            .iter()
            .find(|i| i.kind == k)
            .map(|i| i.path.as_str())
    };
    assert_eq!(
        find(ImageKind::Poster),
        Some(poster.as_str()),
        "{:?}",
        r.images
    );
    assert_eq!(
        find(ImageKind::Backdrop),
        Some(backdrop.as_str()),
        "{:?}",
        r.images
    );
    assert_eq!(
        r.images.len(),
        2,
        "only poster/backdrop kept: {:?}",
        r.images
    );
}

#[then(regex = r#"^the record has authors "([^"]*)" and narrators "([^"]*)"$"#)]
fn record_people(w: &mut World, authors: String, narrators: String) {
    let r = w.record();
    let split = |s: &str| -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    assert_eq!(r.authors, split(&authors));
    assert_eq!(r.narrators, split(&narrators));
}

#[then(
    regex = r#"^the record is in series "([^"]*)" at position "([^"]*)" and is (abridged|unabridged)$"#
)]
fn record_series(w: &mut World, series: String, pos: String, abridged: String) {
    let r = w.record();
    assert_eq!(r.series.as_deref(), Some(series.as_str()));
    assert_eq!(r.series_position.as_deref(), Some(pos.as_str()));
    assert_eq!(r.abridged, Some(abridged == "abridged"));
}

#[then(regex = r#"^the record has subtitle "([^"]*)"$"#)]
fn record_subtitle(w: &mut World, sub: String) {
    assert_eq!(w.record().subtitle.as_deref(), Some(sub.as_str()));
}

#[then(regex = r#"^the lookup fails with an error containing "([^"]*)"$"#)]
fn lookup_fails(w: &mut World, needle: String) {
    let e = w.error.as_deref().expect("expected the lookup to fail");
    assert!(e.contains(&needle), "error {e:?} lacks {needle:?}");
}

#[then("the lookup fails")]
fn lookup_fails_any(w: &mut World) {
    assert!(w.error.is_some(), "expected a failure, got {:?}", w.record);
}

/// A provider 404 must be distinguishable from an outage: Radarr marks the movie
/// "removed from TMDB" rather than retrying forever.
#[then("the lookup reports the item as not found rather than a network failure")]
fn not_found(w: &mut World) {
    let e = w.error.as_deref().expect("expected the lookup to fail");
    assert!(
        !e.starts_with("Network error") && e.to_lowercase().contains("not found"),
        "a 404 surfaced as {e:?}"
    );
}

#[then("the lookup succeeds")]
fn lookup_ok(w: &mut World) {
    assert!(w.error.is_none(), "{:?}", w.error);
    assert!(w.record.is_some() || w.series.is_some() || !w.matches.is_empty());
}

#[then(regex = r"^the lookup waited at least (\d+) seconds$")]
fn waited(w: &mut World, secs: u64) {
    let e = w.elapsed.expect("timed");
    assert!(
        e >= std::time::Duration::from_secs(secs),
        "only waited {e:?}"
    );
}

#[then(regex = r#"^the provider is named "([^"]*)" and supports "([^"]*)" but not "([^"]*)"$"#)]
fn supports(w: &mut World, name: String, yes: String, no: String) {
    let p = w.provider().common();
    assert_eq!(p.name(), name);
    assert!(p.supports(kind_of(&yes)));
    assert!(!p.supports(kind_of(&no)));
}

#[then(regex = r#"^the series is "([^"]*)" on "([^"]*)" with status "([^"]*)"$"#)]
fn series_core(w: &mut World, title: String, network: String, status: String) {
    let s = w.series();
    assert_eq!(s.record.title, title);
    assert_eq!(s.network.as_deref(), Some(network.as_str()));
    assert_eq!(s.status.as_deref(), Some(status.as_str()));
}

#[then(regex = r"^the series has (\d+) seasons and (\d+) episodes$")]
fn series_counts(w: &mut World, seasons: usize, episodes: usize) {
    let s = w.series();
    assert_eq!(s.seasons.len(), seasons, "{:?}", s.seasons);
    assert_eq!(s.episodes.len(), episodes);
}

#[then(
    regex = r#"^season (\d+) is "([^"]*)" with (\d+) episodes first airing (\d{4}-\d{2}-\d{2})$"#
)]
fn season(w: &mut World, n: u16, name: String, eps: u16, air: String) {
    let s = w.series();
    let season = s.seasons.iter().find(|x| x.number == n).expect("season");
    assert_eq!(season.name.as_deref(), Some(name.as_str()));
    assert_eq!(season.episode_count, eps);
    assert_eq!(season.air_date.map(|d| d.to_string()), Some(air));
}

#[then(regex = r#"^episode S(\d+)E(\d+) is "([^"]*)" with absolute number (\d+)$"#)]
fn episode(w: &mut World, s: u16, e: u16, title: String, abs: u32) {
    let ep = w
        .series()
        .episodes
        .iter()
        .find(|x| x.season == s && x.number == e)
        .expect("episode");
    assert_eq!(ep.title.as_deref(), Some(title.as_str()));
    assert_eq!(ep.absolute, Some(abs));
}

#[then(regex = r"^the series (is|is not) flagged as anime$")]
fn anime(w: &mut World, yes: String) {
    assert_eq!(w.series().is_anime, yes == "is");
}

#[then(regex = r#"^the series has tvdb id (\d+), tmdb id (\d+) and imdb id "([^"]*)"$"#)]
fn series_ids(w: &mut World, tvdb: u64, tmdb: u64, imdb: String) {
    let ids = &w.series().record.external_ids;
    assert_eq!(ids.tvdb, Some(TvdbId(tvdb)));
    assert_eq!(ids.tmdb, Some(TmdbId(tmdb)));
    assert_eq!(ids.imdb.as_ref().map(|i| i.0.as_str()), Some(imdb.as_str()));
}

#[then(regex = r#"^the author is "([^"]*)" with a description$"#)]
fn author_is(w: &mut World, name: String) {
    let a = w
        .author
        .as_ref()
        .unwrap_or_else(|| panic!("no author: {:?}", w.error));
    assert_eq!(a.name, name);
    assert!(a.description.is_some());
}

#[then(regex = r#"^the author matches are "([^"]*)"$"#)]
fn author_matches(w: &mut World, asins: String) {
    let got: Vec<&str> = w.authors.iter().map(|a| a.asin.0.as_str()).collect();
    let want: Vec<&str> = asins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(got, want, "{:?}", w.error);
}

#[then(regex = r#"^the catalog items are "([^"]*)"$"#)]
fn items(w: &mut World, asins: String) {
    let got: Vec<&str> = w.items.iter().map(|i| i.asin.0.as_str()).collect();
    let want: Vec<&str> = asins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(got, want, "{:?}", w.error);
}

#[then(regex = r#"^catalog item (\d+) is "([^"]*)" by "([^"]*)" in series "([^"]*)" #([^"]*)$"#)]
fn item_detail(
    w: &mut World,
    n: usize,
    title: String,
    author: String,
    series: String,
    seq: String,
) {
    let i = &w.items[n - 1];
    assert_eq!(i.title, title);
    assert_eq!(i.authors, vec![author]);
    assert_eq!(i.series_name.as_deref(), Some(series.as_str()));
    assert_eq!(i.series_position.as_deref(), Some(seq.as_str()));
}

#[then(regex = r#"^the product language is "([^"]*)"$"#)]
fn language(w: &mut World, lang: String) {
    let got = w.language.clone().flatten();
    assert_eq!(
        got.as_deref(),
        (!lang.is_empty()).then_some(lang.as_str()),
        "{:?}",
        w.error
    );
}
