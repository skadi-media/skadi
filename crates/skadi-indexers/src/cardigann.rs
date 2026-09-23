//! Native Cardigann indexer adapter (SKADI-T-0257 / T-0258): wraps the
//! `skadi-cardigann` engine as an [`Indexer`], supplying a
//! [`Fetcher`](skadi_cardigann::Fetcher) backed by a **per-indexer reqwest client
//! with its own cookie jar** (so a private-tracker login session is carried into
//! search), and mapping the engine's raw `CardigannRelease`s into skadi
//! [`Release`]s. The decoupled engine runs inside skadi's existing search fan-out
//! (rate-limit + health decorators wrap this like any other indexer); login runs
//! lazily once before the first search.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use skadi_cardigann::engine::{
    FetchReq, FetchResp, Fetcher as EngineFetcher, Method as EngineMethod,
};
use skadi_cardigann::template::Value as TVal;
use skadi_cardigann::{CardigannRelease, Definition, LoginOutcome, SearchInput};
use skadi_core::{AppError, IndexerId, MediaKind, Protocol, Result};
use tokio::sync::Mutex;

use crate::flaresolverr::{self, FlareSolverr};
use crate::{
    Category, IdParam, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};

/// A `skadi-cardigann` fetcher with its **own per-indexer cookie jar** (so a login
/// session is carried across requests) and optional **FlareSolverr** fallback for
/// CloudFlare/DDoS-Guard challenges (SKADI-T-0259). GET + POST both supported.
pub struct HttpFetcher {
    client: reqwest::Client,
    jar: Arc<reqwest::cookie::Jar>,
    flaresolverr: Option<FlareSolverr>,
    /// Per-host user-agent to replay after a solve (the `cf_clearance` cookie is
    /// bound to the browser UA FlareSolverr used).
    solved_ua: Mutex<HashMap<String, String>>,
    /// Which indexer's fetches these are, so the solve path can be recorded
    /// against it (SKADI-T-0552). `None` for a fetcher not tied to one — the
    /// definition-refresh path, and tests.
    /// `OnceLock` rather than a mutex: it is written once at construction and
    /// read on every fetch, so there is no guard to hold across an await.
    indexer: std::sync::OnceLock<skadi_core::IndexerId>,
}

impl HttpFetcher {
    /// Tie this fetcher's counters to an indexer (SKADI-T-0552).
    pub fn track_as(&self, id: skadi_core::IndexerId) {
        // Setting it twice would mean one fetcher serving two indexers, which
        // would make the counters meaningless — ignore rather than panic.
        let _ = self.indexer.set(id);
    }

    /// Record how a fetch was served, when this fetcher belongs to an indexer.
    fn record_path(&self, path: crate::health::SolvePath) {
        if let Some(id) = self.indexer.get() {
            crate::health::indexer_health().record_path(*id, path);
        }
    }

    /// Fetch a URL through this indexer's session and return the **raw bytes**
    /// (SKADI-T-0497).
    ///
    /// The engine's `fetch` decodes into a `String`, which is right for HTML and
    /// JSON and destroys a `.torrent`: invalid UTF-8 becomes replacement
    /// characters, so an info-hash computed from it would be wrong. This shares
    /// the same client, and therefore the same cookie jar and login state, so a
    /// download link that only works while signed in works here too.
    pub async fn fetch_bytes(&self, url: &str) -> std::result::Result<(u16, Vec<u8>), String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("reading {url}: {e}"))?
            .to_vec();
        Ok((status, bytes))
    }

    #[must_use]
    pub fn new(flaresolverr_url: Option<&str>, proxy_url: Option<&str>) -> Self {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(45))
            .cookie_provider(jar.clone())
            .user_agent("Mozilla/5.0 (compatible; skadi/cardigann)");
        // Route tracker requests via the (VPN) proxy so they share FlareSolverr's
        // exit IP. The FlareSolverr client itself is direct (it talks to the
        // solver service, which already egresses via the VPN).
        if let Some(p) = proxy_url.filter(|s| !s.trim().is_empty())
            && let Ok(proxy) = reqwest::Proxy::all(p)
        {
            builder = builder.proxy(proxy);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        let flaresolverr = flaresolverr_url
            .filter(|s| !s.trim().is_empty())
            .map(FlareSolverr::new);
        Self {
            client,
            jar,
            flaresolverr,
            solved_ua: Mutex::new(HashMap::new()),
            indexer: std::sync::OnceLock::new(),
        }
    }

    /// Inject a solved challenge's cookies into the jar + remember its UA, so
    /// later direct requests to the host carry a valid `cf_clearance`.
    async fn apply_solution(&self, url: &str, solved: &flaresolverr::Solved) {
        if let Ok(parsed) = url.parse::<reqwest::Url>() {
            for (name, value) in &solved.cookies {
                self.jar.add_cookie_str(&format!("{name}={value}"), &parsed);
            }
            if !solved.user_agent.is_empty()
                && let Some(host) = parsed.host_str()
            {
                self.solved_ua
                    .lock()
                    .await
                    .insert(host.to_string(), solved.user_agent.clone());
            }
        }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new(None, None)
    }
}

#[async_trait]
impl EngineFetcher for HttpFetcher {
    async fn fetch(
        &self,
        req: FetchReq,
    ) -> std::result::Result<FetchResp, skadi_cardigann::FetchError> {
        use skadi_cardigann::FetchError;
        // Replay the solved UA for this host (keeps cf_clearance valid).
        let host = req
            .url
            .parse::<reqwest::Url>()
            .ok()
            .and_then(|u| u.host_str().map(String::from));
        let replay_ua = match &host {
            Some(h) => self.solved_ua.lock().await.get(h).cloned(),
            None => None,
        };

        let mut rb = match req.method {
            EngineMethod::Get => self.client.get(&req.url),
            EngineMethod::Post => {
                let b = self
                    .client
                    .post(&req.url)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
                match &req.body {
                    Some(body) => b.body(body.clone()),
                    None => b,
                }
            }
        };
        if let Some(ua) = &replay_ua {
            rb = rb.header(USER_AGENT, ua);
        }
        for (k, v) in &req.headers {
            rb = rb.header(k, v);
        }
        let resp = rb.send().await.map_err(|e| FetchError(e.to_string()))?;
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let body = resp.text().await.map_err(|e| FetchError(e.to_string()))?;

        // On a CloudFlare/DDoS-Guard challenge, solve via FlareSolverr (if any)
        // and return the rendered page; later requests reuse the injected cookie.
        let challenged = flaresolverr::is_challenge(status, &body);
        if challenged && let Some(fs) = &self.flaresolverr {
            let post_data = match req.method {
                EngineMethod::Post => req.body.as_deref(),
                EngineMethod::Get => None,
            };
            match fs.solve(&req.url, post_data).await {
                Ok(solved) => {
                    self.apply_solution(&req.url, &solved).await;
                    self.record_path(crate::health::SolvePath::Solved);
                    return Ok(FetchResp {
                        status: solved.status,
                        final_url: req.url,
                        body: solved.body,
                    });
                }
                Err(e) => {
                    tracing::warn!(url = %req.url, error = %e, "flaresolverr solve failed");
                }
            }
        }
        // Reached here either without a challenge, or with one nothing solved.
        // The distinction is the whole point of the counters (SKADI-T-0552): a
        // tracker that never challenges does not need the solver at all, and one
        // whose challenges go unsolved is failing in a way the current setup
        // cannot fix.
        self.record_path(if challenged {
            crate::health::SolvePath::Unsolved
        } else {
            crate::health::SolvePath::Direct
        });
        Ok(FetchResp {
            status,
            final_url,
            body,
        })
    }
}

/// A native Cardigann tracker presented as a skadi [`Indexer`].
pub struct CardigannIndexer {
    id: IndexerId,
    def: Arc<Definition>,
    config: BTreeMap<String, TVal>,
    base_url: String,
    fetcher: Arc<HttpFetcher>,
    needs_login: bool,
    /// Whether the per-indexer session has authenticated (login runs lazily once).
    logged_in: Mutex<bool>,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    flags: crate::config::IndexerFlags,
}

impl CardigannIndexer {
    /// Build from a parsed definition + non-secret setting overrides + an optional
    /// sealed secret (a JSON object of secret settings, e.g. password/cookie).
    #[must_use]
    pub fn new(
        id: IndexerId,
        def: Arc<Definition>,
        overrides: &BTreeMap<String, String>,
        secret: Option<&str>,
        flaresolverr_url: Option<&str>,
        proxy_url: Option<&str>,
    ) -> Self {
        let base_url = def.links.first().cloned().unwrap_or_default();
        let mut config = skadi_cardigann::engine::resolve_config(&def, overrides);
        // Cardigann exposes the working base URL as `.Config.sitelink`.
        config.insert("sitelink".into(), TVal::Str(with_trailing_slash(&base_url)));
        // Merge sealed secret settings (a JSON object) into the login config.
        if let Some(secret) = secret
            && let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(secret)
        {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    config.insert(k, TVal::Str(s.to_string()));
                }
            }
        }
        let needs_login = def.needs_login();
        let fetcher = Arc::new(HttpFetcher::new(flaresolverr_url, proxy_url));
        // Attribute this fetcher's solve paths to this indexer (SKADI-T-0552),
        // so /indexers/health can say which trackers actually needed the solver.
        fetcher.track_as(id);
        Self {
            id,
            def,
            config,
            base_url,
            fetcher,
            needs_login,
            logged_in: Mutex::new(false),
            flags: crate::config::IndexerFlags::default(),
        }
    }

    /// Apply the stored per-indexer flags (SKADI-T-0505).
    #[must_use]
    pub fn with_flags(mut self, flags: crate::config::IndexerFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Authenticate (once) before searching a private tracker.
    async fn ensure_login(&self) -> Result<()> {
        if !self.needs_login {
            return Ok(());
        }
        let mut guard = self.logged_in.lock().await;
        if *guard {
            return Ok(());
        }
        let outcome = skadi_cardigann::login(
            &self.def,
            &self.config,
            &BTreeMap::new(),
            &self.base_url,
            self.fetcher.as_ref(),
            Utc::now(),
        )
        .await
        .map_err(|e| AppError::Network(format!("cardigann '{}' login: {e}", self.def.id)))?;
        match outcome {
            LoginOutcome::Ok | LoginOutcome::NotRequired => {
                *guard = true;
                Ok(())
            }
            LoginOutcome::Failed(reason) => Err(AppError::Network(format!(
                "cardigann '{}' login failed: {reason}",
                self.def.id
            ))),
        }
    }

    /// The tracker category ids that map to the requested media kind.
    fn tracker_categories(&self, kind: MediaKind) -> Vec<String> {
        let prefixes: &[&str] = match kind {
            MediaKind::Movie => &["Movies"],
            MediaKind::Series => &["TV"],
            MediaKind::Music => &["Audio"],
            // Audiobooks live under Audio/Audiobook; fall back to all Audio.
            MediaKind::Audiobook => &["Audio/Audiobook", "Audio"],
            MediaKind::Book => &["Books"],
            // Kinds without a clean tracker category (e.g. subtitles) aren't scoped.
            _ => &[],
        };
        self.def
            .caps
            .category_pairs()
            .into_iter()
            .filter(|(_, cat)| prefixes.iter().any(|p| cat.starts_with(p)))
            .map(|(id, _)| id)
            .collect()
    }

    /// ID query params (`imdbid`/`tmdbid`/`tvdbid`) + scope extras the def can use.
    fn query_params(&self, query: &dyn SearchQuery) -> BTreeMap<String, String> {
        let mut q = BTreeMap::new();
        let e = query.external_ids();
        if let Some(id) = &e.imdb {
            q.insert("imdbid".into(), id.0.clone());
        }
        if let Some(id) = &e.tmdb {
            q.insert("tmdbid".into(), id.0.to_string());
        }
        if let Some(id) = &e.tvdb {
            q.insert("tvdbid".into(), id.0.to_string());
        }
        for (k, v) in query.extra_params() {
            q.insert(k.to_string(), v);
        }
        q
    }

    async fn run(
        &self,
        keywords: String,
        kind: MediaKind,
        query: Option<&dyn SearchQuery>,
    ) -> Result<Vec<Release>> {
        self.ensure_login().await?;
        let input = SearchInput {
            keywords,
            categories: self.tracker_categories(kind),
            query: query.map(|q| self.query_params(q)).unwrap_or_default(),
            config: self.config.clone(),
            base_url: self.base_url.clone(),
        };
        let releases =
            skadi_cardigann::search(&self.def, &input, self.fetcher.as_ref(), Utc::now())
                .await
                .map_err(|e| AppError::Network(format!("cardigann '{}': {e}", self.def.id)))?;
        Ok(releases
            .into_iter()
            .filter_map(|r| to_release(self.id, &self.base_url, r))
            .collect())
    }
}

/// The `SxxEyy` (or `Sxx` for a season pack) marker a TV search appends to its
/// keywords (SKADI-T-0587), from the hunter's `season`/`ep` extra params.
///
/// Public-tracker definitions template `{{ .Keywords }}` and nothing else: the
/// `season`/`ep` params only reach `.Query.*`, which almost no public def reads.
/// A bare-title search returns the tracker's first page — its newest ~100
/// uploads for that show — so any episode older than that page was never in
/// the candidate set at all (Critical Role S04E35: 424 candidates, none of them
/// the episode). Jackett appends the episode string for exactly this reason.
/// `None` for non-TV searches (no `season` param).
pub(crate) fn tv_keyword_suffix(query: &dyn SearchQuery) -> Option<String> {
    let params = query.extra_params();
    let get = |key: &str| {
        params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    };
    let season: u16 = get("season")?.parse().ok()?;
    Some(match get("ep").and_then(|v| v.parse::<u16>().ok()) {
        Some(episode) => format!("S{season:02}E{episode:02}"),
        None => format!("S{season:02}"),
    })
}

/// `"<title> <suffix>"`, or the title alone when there is no suffix or no title
/// (an empty keyword is the definition's "recent feed" path and must stay empty).
pub(crate) fn with_tv_suffix(title: &str, suffix: Option<&str>) -> String {
    match suffix {
        Some(s) if !title.is_empty() => format!("{title} {s}"),
        _ => title.to_string(),
    }
}

#[async_trait]
impl Indexer for CardigannIndexer {
    fn enable_rss(&self) -> bool {
        self.flags.enable_rss
    }

    fn enable_automatic_search(&self) -> bool {
        self.flags.enable_automatic_search
    }

    fn applies_to_tags(&self, item_tags: &[String]) -> bool {
        self.flags.applies_to(item_tags)
    }

    fn priority(&self) -> u32 {
        self.flags.priority
    }

    fn minimum_seeders(&self) -> u32 {
        self.flags.minimum_seeders
    }

    fn id(&self) -> IndexerId {
        self.id
    }

    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    fn supports(&self, kind: MediaKind) -> bool {
        !self.tracker_categories(kind).is_empty() || self.def.caps.modes.contains_key("search")
    }

    async fn capabilities(&self) -> Result<IndexerCaps> {
        let mut id_params = BTreeSet::new();
        for params in self.def.caps.modes.values() {
            for p in params {
                match p.as_str() {
                    "imdbid" => {
                        id_params.insert(IdParam::Imdb);
                    }
                    "tmdbid" => {
                        id_params.insert(IdParam::Tmdb);
                    }
                    "tvdbid" => {
                        id_params.insert(IdParam::Tvdb);
                    }
                    _ => {}
                }
            }
        }
        Ok(IndexerCaps {
            supports_rss: true,
            supports_search: true,
            id_params,
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            // Numeric Newznab category exposure is deferred (the def carries
            // tracker→Newznab *names*); scoping is done via tracker_categories.
            categories: Vec::<Category>::new(),
        })
    }

    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        // Search **every** title alias, not just the first (SKADI-T-0506). A
        // series known by an English and a romanised title, or a film by its
        // local release name, is listed on a tracker under whichever the uploader
        // used — searching only `titles()[0]` silently missed the rest.
        let titles = query.titles();
        let suffix = tv_keyword_suffix(query);
        if titles.len() <= 1 {
            let keywords = titles.first().cloned().unwrap_or_default();
            return self
                .run(
                    with_tv_suffix(&keywords, suffix.as_deref()),
                    query.kind(),
                    Some(query),
                )
                .await;
        }
        let mut out: Vec<Release> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for title in titles {
            // One alias failing (a tracker hiccup, a 403 on a odd query) must not
            // lose the results the others found — the whole point is more
            // coverage, so a partial answer beats an error.
            match self
                .run(
                    with_tv_suffix(title, suffix.as_deref()),
                    query.kind(),
                    Some(query),
                )
                .await
            {
                Ok(rels) => {
                    for r in rels {
                        if seen.insert(crate::release_key(&r)) {
                            out.push(r);
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    indexer = %self.def.id,
                    alias = %title,
                    "cardigann alias search failed (continuing): {e}"
                ),
            }
        }
        Ok(out)
    }

    async fn rss(&self) -> Result<Vec<Release>> {
        // An empty keyword renders the definition's "no query" path (recent feed).
        self.run(String::new(), MediaKind::Movie, None).await
    }

    async fn test(&self) -> Result<()> {
        // Private trackers: a real login (with the test selector) is the check.
        if self.needs_login {
            *self.logged_in.lock().await = false;
            return self.ensure_login().await;
        }
        // Public: cheap reachability of the site.
        let resp = self
            .fetcher
            .fetch(FetchReq {
                method: EngineMethod::Get,
                url: self.base_url.clone(),
                headers: Vec::new(),
                body: None,
            })
            .await
            .map_err(|e| AppError::Network(e.0))?;
        if resp.status >= 400 {
            return Err(AppError::Network(format!(
                "cardigann '{}': site returned HTTP {}",
                self.def.id, resp.status
            )));
        }
        Ok(())
    }

    async fn resolve_fetch(&self, fetch: &ReleaseFetch) -> Result<ReleaseFetch> {
        // Detail-page resolution (SKADI-T-0306): when our definition has a
        // `download.infohash` block and the chosen fetch is the detail-page URL
        // (a `TorrentUrl`, not already a magnet), fetch that page through the same
        // authenticated + FlareSolverr client and build a magnet from the scraped
        // info-hash. Magnets / direct `.torrent`s pass through untouched.
        let ReleaseFetch::TorrentUrl(url) = fetch else {
            return Ok(fetch.clone());
        };
        // Belt as well as braces (SKADI-T-0565). `to_release` now classifies by
        // scheme, but this is the last point before the link is HTTP-fetched, and
        // the cost of being wrong here is a snatch that fails with `GET magnet:`
        // — an error that names the tracker and looks like the tracker's fault.
        // Anything not addressable over HTTP is handed on as-is rather than
        // fetched, so a future producer that mis-types a link degrades to
        // "passed through" instead of "failed".
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ReleaseFetch::from_link(url).unwrap_or_else(|| fetch.clone()));
        }
        let has_download_block = self
            .def
            .download
            .as_ref()
            .is_some_and(|d| d.selectors.is_some() || d.infohash.is_some());
        if !has_download_block {
            // No `download` block, so there is no detail page to scrape — but
            // the link still must not reach the worker bare (SKADI-T-0497).
            // The worker holds no cookies and no solver, so a challenge-fronted or
            // login-gated `.torrent` arrives there as an HTML page and fails with
            // "add failed: error decoding torrent". Fetch it here instead, through
            // the session that can actually get it, and hand on a magnet.
            return self.magnet_from_torrent_link(url).await;
        }
        // The page may sit behind the tracker's session (a no-op for public defs).
        self.ensure_login().await?;
        match skadi_cardigann::resolve_download(
            &self.def,
            url,
            self.fetcher.as_ref(),
            Utc::now(),
            &self.config,
        )
        .await
        .map_err(|e| {
            AppError::Network(format!("cardigann '{}' download resolve: {e}", self.def.id))
        })? {
            // A `download.selectors` block may yield either link shape
            // (SKADI-T-0589): a magnet is final; a `.torrent` URL is fetched
            // through this session and handed on as a magnet like any other.
            Some(link) if link.starts_with("magnet:") => {
                ReleaseFetch::from_link(&link).ok_or_else(|| {
                    AppError::Network(format!(
                        "cardigann '{}': download block yielded a magnet without an info-hash ({link})",
                        self.def.id
                    ))
                })
            }
            Some(link) if link.starts_with("http://") || link.starts_with("https://") => {
                self.magnet_from_torrent_link(&link).await
            }
            Some(link) => Err(AppError::Network(format!(
                "cardigann '{}': download block yielded an unusable link ({link})",
                self.def.id
            ))),
            None => self.magnet_from_torrent_link(url).await,
        }
    }
}

impl CardigannIndexer {
    /// Resolve a direct `.torrent` link to a magnet using this indexer's own
    /// session (SKADI-T-0497).
    ///
    /// This errors rather than falling back to the bare URL. Passing it on is
    /// exactly what produced the unexplained "error decoding torrent" in
    /// production on 2026-09-06, and a failure that names the tracker, the link
    /// and the likely cause is worth more than a retry that cannot succeed.
    async fn magnet_from_torrent_link(&self, url: &str) -> Result<ReleaseFetch> {
        self.ensure_login().await?;
        let (status, bytes) = self
            .fetcher
            .fetch_bytes(url)
            .await
            .map_err(|e| AppError::Network(format!("cardigann '{}': {e}", self.def.id)))?;
        if status >= 400 {
            return Err(AppError::Network(format!(
                "cardigann '{}': the tracker refused the download link (HTTP {status}) — {url}",
                self.def.id
            )));
        }
        match crate::torrent::infohash_from_torrent(&bytes) {
            Some(hash) => Ok(ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{hash}"))),
            None => Err(AppError::Network(format!(
                "cardigann '{}': {url} returned {} bytes that are not a torrent file \
                 — a challenge or login page is the usual cause",
                self.def.id,
                bytes.len()
            ))),
        }
    }
}

/// Map one engine release onto a skadi [`Release`] (pure — unit-testable).
/// Map a cardigann definition's Newznab **category name** to its numeric id
/// (SKADI-T-0506).
///
/// Cardigann definitions map a tracker's own category ids to Newznab *names*
/// (`Movies/HD`, `Audio/Audiobook`), and we dropped those on the floor —
/// `Release.categories` came back empty for every cardigann result, so the
/// off-category gate (SKADI-T-0360) never applied to them. That gate is what
/// rejects the games and music a public tracker returns for a text search
/// regardless of the `cat=` filter, so cardigann indexers were the ones that
/// needed it most and were the only ones not getting it.
///
/// Matched most-specific-first, then falling back to the top-level group, so an
/// unrecognised leaf (`Movies/Bluray-Remux`) still lands in `Movies` rather than
/// being dropped — a slightly coarse category still gates correctly, an absent
/// one does not gate at all.
pub(crate) fn newznab_id(name: &str) -> Option<u32> {
    const EXACT: &[(&str, u32)] = &[
        ("Movies/Foreign", 2010),
        ("Movies/Other", 2020),
        ("Movies/SD", 2030),
        ("Movies/HD", 2040),
        ("Movies/UHD", 2045),
        ("Movies/3D", 2050),
        ("Movies/BluRay", 2060),
        ("Movies/DVD", 2070),
        ("Movies/WEB-DL", 2080),
        ("Audio/MP3", 3010),
        ("Audio/Video", 3020),
        ("Audio/Audiobook", 3030),
        ("Audio/Lossless", 3040),
        ("Audio/Foreign", 3060),
        ("PC/0day", 4010),
        ("PC/ISO", 4020),
        ("PC/Mac", 4030),
        ("PC/Mobile-Other", 4040),
        ("PC/Games", 4050),
        ("TV/WEB-DL", 5010),
        ("TV/Foreign", 5020),
        ("TV/SD", 5030),
        ("TV/HD", 5040),
        ("TV/UHD", 5045),
        ("TV/Other", 5050),
        ("TV/Sport", 5060),
        ("TV/Anime", 5070),
        ("TV/Documentary", 5080),
        ("XXX/Other", 6070),
        ("Books/Mags", 7010),
        ("Books/EBook", 7020),
        ("Books/Comics", 7030),
        ("Books/Technical", 7040),
        ("Books/Foreign", 7060),
    ];
    const GROUPS: &[(&str, u32)] = &[
        ("Console", 1000),
        ("Movies", 2000),
        ("Audio", 3000),
        ("PC", 4000),
        ("TV", 5000),
        ("XXX", 6000),
        ("Books", 7000),
        ("Other", 8000),
    ];
    if let Some((_, id)) = EXACT.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Some(*id);
    }
    let group = name.split('/').next().unwrap_or(name);
    GROUPS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(group))
        .map(|(_, id)| *id)
}

fn to_release(id: IndexerId, base_url: &str, r: CardigannRelease) -> Option<Release> {
    if r.title.is_empty() {
        return None;
    }
    let fetch = if let Some(magnet) = r.magnet {
        ReleaseFetch::Magnet(magnet)
    } else if let Some(hash) = &r.infohash {
        ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{hash}"))
    } else if let Some(dl) = r.download {
        // Classify by scheme, not by which field the definition populated
        // (SKADI-T-0565). Many definitions' `download` selector yields a
        // `magnet:` URI; `absolutize` already knows not to treat one as a
        // relative path, and this used to wrap the result in `TorrentUrl`
        // regardless — so the snatch path then tried to HTTP-GET a magnet.
        ReleaseFetch::from_link(&absolutize(base_url, &dl))?
    } else {
        return None;
    };
    let published = r
        .date
        .as_deref()
        .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    Some(Release {
        indexer: id,
        title: r.title.clone(),
        fetch,
        size: r.size.unwrap_or(0),
        published,
        seeders: r.seeders.and_then(|s| u32::try_from(s).ok()),
        // The definition already mapped the tracker's own category to a Newznab
        // name; turn that into the numeric id the off-category gate reads
        // (SKADI-T-0506). De-duplicated, since several names can share a group.
        categories: {
            let mut ids: Vec<Category> = r
                .categories
                .iter()
                .filter_map(|n| newznab_id(n))
                .map(Category)
                .collect();
            ids.sort_unstable_by_key(|c| c.0);
            ids.dedup();
            ids
        },
        parsed: skadi_quality::parse(&r.title),
    })
}

fn absolutize(base_url: &str, url: &str) -> String {
    if url.starts_with("http") || url.starts_with("magnet:") {
        url.to_string()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            url.trim_start_matches('/')
        )
    }
}

fn with_trailing_slash(url: &str) -> String {
    if url.is_empty() || url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(title: &str) -> CardigannRelease {
        CardigannRelease {
            title: title.into(),
            infohash: Some("ABCDEF".into()),
            size: Some(1024),
            seeders: Some(7),
            date: Some("2025-03-01T08:30:00+00:00".into()),
            ..Default::default()
        }
    }

    #[test]
    fn maps_release_with_infohash_to_magnet() {
        let id = IndexerId::new();
        let out = to_release(id, "https://x.test/", rel("The Matrix 1999")).unwrap();
        assert_eq!(out.title, "The Matrix 1999");
        assert_eq!(out.size, 1024);
        assert_eq!(out.seeders, Some(7));
        assert!(matches!(out.fetch, ReleaseFetch::Magnet(m) if m.contains("btih:ABCDEF")));
        assert_eq!(out.published.to_rfc3339(), "2025-03-01T08:30:00+00:00");
    }

    #[test]
    fn relative_download_is_absolutized() {
        let mut r = rel("Dune 2021");
        r.infohash = None;
        r.download = Some("/dl/2.torrent".into());
        let out = to_release(IndexerId::new(), "https://x.test/", r).unwrap();
        assert!(
            matches!(out.fetch, ReleaseFetch::TorrentUrl(u) if u == "https://x.test/dl/2.torrent")
        );
    }

    #[test]
    fn untitled_or_unfetchable_dropped() {
        assert!(to_release(IndexerId::new(), "https://x/", rel("")).is_none());
        let mut r = rel("No Fetch");
        r.infohash = None;
        r.magnet = None;
        r.download = None;
        assert!(to_release(IndexerId::new(), "https://x/", r).is_none());
    }

    /// Private-tracker flow: form login sets a session cookie, which the
    /// per-indexer jar carries into the authenticated search (SKADI-T-0258).
    #[tokio::test]
    async fn private_tracker_logs_in_then_searches_with_session_cookie() {
        use skadi_core::ExternalIds;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Login page → form; POST sets the session cookie.
        Mock::given(method("GET"))
            .and(path("/login.php"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<form id="login" action="/takelogin.php"></form>"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/takelogin.php"))
            .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "sess=ok; Path=/"))
            .mount(&server)
            .await;
        // login.test path confirms we're authenticated.
        Mock::given(method("GET"))
            .and(path("/account.php"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"<a class="logout">out</a>"#),
            )
            .mount(&server)
            .await;
        // Search ONLY answers when the session cookie is presented.
        Mock::given(method("GET"))
            .and(path("/search.php"))
            .and(header("cookie", "sess=ok"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{"name":"Some Movie 2020 1080p","hash":"DEAD","size":"1000"}]"#,
            ))
            .mount(&server)
            .await;

        let def_yaml = r#"
id: priv
name: Priv
type: private
caps:
  categorymappings: [{id: "1", cat: Movies}]
  modes: {search: [q]}
login:
  path: login.php
  method: form
  form: form#login
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  test:
    path: account.php
    selector: a.logout
search:
  paths:
    - path: "search.php?q={{ .Keywords }}"
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    infohash: {selector: hash}
    size: {selector: size}
"#;
        let mut def = skadi_cardigann::parse_definition(def_yaml).unwrap();
        def.links = vec![server.uri()];
        let overrides = BTreeMap::from([("username".to_string(), "alice".to_string())]);
        // Password arrives as a sealed secret (JSON object).
        let idx = CardigannIndexer::new(
            IndexerId::new(),
            Arc::new(def),
            &overrides,
            Some(r#"{"password":"hunter2"}"#),
            None,
            None,
        );

        struct Q;
        impl SearchQuery for Q {
            fn kind(&self) -> MediaKind {
                MediaKind::Movie
            }
            fn titles(&self) -> &[String] {
                static T: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
                T.get_or_init(|| vec!["Some Movie".into()])
            }
            fn year(&self) -> Option<u16> {
                None
            }
            fn external_ids(&self) -> &ExternalIds {
                static E: std::sync::OnceLock<ExternalIds> = std::sync::OnceLock::new();
                E.get_or_init(ExternalIds::default)
            }
            fn categories(&self) -> &[Category] {
                &[]
            }
        }

        let releases = idx.search(&Q).await.unwrap();
        assert_eq!(
            releases.len(),
            1,
            "authenticated search returned the result"
        );
        assert!(releases[0].title.contains("Some Movie"));
    }

    /// A CloudFlare challenge from the tracker is solved via FlareSolverr, and the
    /// fetcher returns the rendered page (SKADI-T-0259).
    #[tokio::test]
    async fn flaresolverr_solves_a_cloudflare_challenge() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The tracker throws a CloudFlare interstitial.
        let tracker = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/q"))
            .respond_with(ResponseTemplate::new(503).set_body_string(
                "<title>Just a moment...</title><div id=\"challenge-platform\"></div>",
            ))
            .mount(&tracker)
            .await;

        // FlareSolverr returns the solved page + a clearance cookie.
        let solver = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "ok",
                "solution": {
                    "status": 200,
                    "response": "<html><body>solved results here</body></html>",
                    "userAgent": "Mozilla/5.0 Chrome/120",
                    "cookies": [{"name": "cf_clearance", "value": "abc123"}]
                }
            })))
            .mount(&solver)
            .await;

        let fetcher = HttpFetcher::new(Some(&solver.uri()), None);
        let resp = fetcher
            .fetch(FetchReq {
                method: EngineMethod::Get,
                url: format!("{}/q", tracker.uri()),
                headers: Vec::new(),
                body: None,
            })
            .await
            .unwrap();

        assert_eq!(resp.status, 200, "returned the solved status, not the 503");
        assert!(
            resp.body.contains("solved results"),
            "returned the solved page"
        );
    }

    /// Full chain through the `Indexer` trait: real reqwest GET against a mock
    /// server, JSON parsed by the engine, mapped to a skadi `Release`. Also proves
    /// keyword URL-encoding ("The Matrix" → a valid request URL).
    #[tokio::test]
    async fn end_to_end_search_through_indexer_trait() {
        use skadi_core::ExternalIds;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"[{"name":"The Matrix 1999 1080p BluRay x264","info_hash":"AAAABBBB","size":"1500000000","seeders":"42","category":"1","added":"1700000000"}]"#;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "The Matrix"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let def_yaml = r#"
id: synthjson
name: SynthJSON
type: public
caps:
  categorymappings: [{id: "1", cat: Movies}]
  modes: {search: [q], movie-search: [q, imdbid]}
search:
  paths:
    - path: "search?q={{ .Keywords }}"
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    infohash: {selector: info_hash}
    size: {selector: size}
    seeders: {selector: seeders}
    category: {selector: category}
    date: {selector: added}
"#;
        let mut def = skadi_cardigann::parse_definition(def_yaml).unwrap();
        def.links = vec![server.uri()];
        let idx = CardigannIndexer::new(
            IndexerId::new(),
            Arc::new(def),
            &BTreeMap::new(),
            None,
            None,
            None,
        );

        struct Q {
            titles: Vec<String>,
            ids: ExternalIds,
        }
        impl SearchQuery for Q {
            fn kind(&self) -> MediaKind {
                MediaKind::Movie
            }
            fn titles(&self) -> &[String] {
                &self.titles
            }
            fn year(&self) -> Option<u16> {
                None
            }
            fn external_ids(&self) -> &ExternalIds {
                &self.ids
            }
            fn categories(&self) -> &[Category] {
                &[]
            }
        }

        let q = Q {
            titles: vec!["The Matrix".into()],
            ids: ExternalIds::default(),
        };
        let releases = idx.search(&q).await.unwrap();

        assert_eq!(releases.len(), 1, "one release from the mock");
        let r = &releases[0];
        assert!(r.title.contains("The Matrix"));
        assert_eq!(r.size, 1_500_000_000);
        assert_eq!(r.seeders, Some(42));
        assert!(matches!(&r.fetch, ReleaseFetch::Magnet(m) if m.contains("AAAABBBB")));
        // The title was parsed for quality scoring.
        assert_eq!(r.published.to_rfc3339(), "2023-11-14T22:13:20+00:00");
    }
}

#[cfg(test)]
mod tv_keyword_suffix_tests {
    use super::*;
    use skadi_core::ExternalIds;

    struct Q {
        titles: Vec<String>,
        ids: ExternalIds,
        extra: Vec<(&'static str, String)>,
    }
    impl SearchQuery for Q {
        fn kind(&self) -> MediaKind {
            MediaKind::Series
        }
        fn titles(&self) -> &[String] {
            &self.titles
        }
        fn year(&self) -> Option<u16> {
            None
        }
        fn external_ids(&self) -> &ExternalIds {
            &self.ids
        }
        fn categories(&self) -> &[Category] {
            &[]
        }
        fn extra_params(&self) -> Vec<(&'static str, String)> {
            self.extra.clone()
        }
    }

    fn q(titles: &[&str], extra: Vec<(&'static str, String)>) -> Q {
        Q {
            titles: titles.iter().map(|t| t.to_string()).collect(),
            ids: ExternalIds::default(),
            extra,
        }
    }

    #[test]
    fn suffix_follows_the_tv_scope() {
        let ep = q(&["Show"], vec![("season", "4".into()), ("ep", "35".into())]);
        assert_eq!(tv_keyword_suffix(&ep).as_deref(), Some("S04E35"));
        let pack = q(&["Show"], vec![("season", "4".into())]);
        assert_eq!(tv_keyword_suffix(&pack).as_deref(), Some("S04"));
        let movie = q(&["Film"], vec![]);
        assert_eq!(tv_keyword_suffix(&movie), None);
        assert_eq!(with_tv_suffix("Show", Some("S04E35")), "Show S04E35");
        assert_eq!(with_tv_suffix("Film", None), "Film");
        // The recent-feed path (empty keyword) is never turned into a search.
        assert_eq!(with_tv_suffix("", Some("S04E35")), "");
    }

    /// The production miss (SKADI-T-0587): every alias of a TV search reaches the
    /// tracker with the episode code, so the wanted episode is on the page the
    /// tracker returns instead of buried behind its newest hundred uploads.
    #[tokio::test]
    async fn tv_search_sends_the_episode_code_with_every_alias() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Distinct info hashes: the alias merge dedups by release key.
        let body = |name: &str, hash: &str| {
            format!(
                r#"[{{"name":"{name}","info_hash":"{hash}","size":"1500000000","seeders":"4","category":"5","added":"1700000000"}}]"#
            )
        };
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "Critical Role S04E35"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body("Critical Role S04E35 1080p WEB", "AAAABBBB")),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "Critical Role Campaign 4 S04E35"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body("Critical Role Campaign 4 S04E35 720p", "CCCCDDDD")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let def_yaml = r#"
id: synthtv
name: SynthTV
type: public
caps:
  categorymappings: [{id: "5", cat: TV}]
  modes: {search: [q], tv-search: [q, season, ep]}
search:
  paths:
    - path: "search?q={{ .Keywords }}"
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    infohash: {selector: info_hash}
    size: {selector: size}
    seeders: {selector: seeders}
    category: {selector: category}
    date: {selector: added}
"#;
        let mut def = skadi_cardigann::parse_definition(def_yaml).unwrap();
        def.links = vec![server.uri()];
        let idx = CardigannIndexer::new(
            IndexerId::new(),
            Arc::new(def),
            &BTreeMap::new(),
            None,
            None,
            None,
        );
        let query = q(
            &["Critical Role", "Critical Role Campaign 4"],
            vec![("season", "4".into()), ("ep", "35".into())],
        );
        let releases = idx.search(&query).await.unwrap();
        assert_eq!(
            releases.len(),
            2,
            "both alias searches carried the episode code"
        );
        assert!(releases.iter().all(|r| r.title.contains("S04E35")));
    }
}

#[cfg(test)]
mod magnet_in_download_field_tests {
    use super::*;

    fn release(download: Option<&str>) -> CardigannRelease {
        CardigannRelease {
            title: "Some.Release.1080p".into(),
            download: download.map(str::to_string),
            ..Default::default()
        }
    }

    /// The production failure, reproduced (SKADI-T-0565).
    ///
    /// Six snatches on 2026-09-10 failed with `GET magnet:?xt=urn:btih:...`. Many
    /// cardigann definitions' `download` selector yields a magnet URI, and
    /// `to_release` classified by which *field* was populated rather than by what
    /// the string was — so the magnet arrived as a `TorrentUrl` and the snatch
    /// path tried to fetch it over HTTP.
    #[test]
    fn a_magnet_in_the_download_field_is_a_magnet_not_a_url() {
        let magnet = "magnet:?xt=urn:btih:062f3f58ca0f4c3e1f0000000000000000000000&dn=x";
        let r = to_release(
            IndexerId::new(),
            "https://tracker.example",
            release(Some(magnet)),
        )
        .expect("a magnet download link is a usable release");
        match r.fetch {
            ReleaseFetch::Magnet(m) => assert_eq!(m, magnet),
            other => panic!("classified by field, not scheme: {other:?}"),
        }
    }

    #[test]
    fn an_http_download_field_is_still_a_torrent_url() {
        let r = to_release(
            IndexerId::new(),
            "https://tracker.example",
            release(Some("/download/abc.torrent")),
        )
        .expect("a relative download link is usable");
        match r.fetch {
            ReleaseFetch::TorrentUrl(u) => {
                assert_eq!(u, "https://tracker.example/download/abc.torrent");
            }
            other => panic!("a relative path must absolutize to a URL: {other:?}"),
        }
    }

    #[test]
    fn a_magnet_without_a_btih_hash_is_not_a_release() {
        // Not addressable by any downloader here, so it must not become a release
        // that later fails deep in the snatch path.
        assert!(
            to_release(
                IndexerId::new(),
                "https://tracker.example",
                release(Some("magnet:?dn=no-hash-here")),
            )
            .is_none()
        );
    }

    /// The second half of the fix: even if some future producer mis-types a link,
    /// the last point before an HTTP fetch refuses to make one.
    #[tokio::test]
    async fn fetch_never_http_gets_a_non_http_scheme() {
        const DEF: &str = concat!(
            "id: t\n",
            "name: t\n",
            "type: public\n",
            "links: [\"https://t.test/\"]\n",
            "caps: {}\n",
            "search: {}\n",
        );
        let magnet = "magnet:?xt=urn:btih:084eeb77b8eefe1a0000000000000000000000ab";
        // A definition with no `download.infohash` block — the path that would
        // otherwise reach `magnet_from_torrent_link` and issue the GET.
        let ix = CardigannIndexer::new(
            IndexerId::new(),
            std::sync::Arc::new(
                skadi_cardigann::parse_definition(DEF).expect("minimal definition parses"),
            ),
            &std::collections::BTreeMap::new(),
            None,
            None,
            None,
        );
        let out: ReleaseFetch = ix
            .resolve_fetch(&ReleaseFetch::TorrentUrl(magnet.into()))
            .await
            .expect("a mis-typed magnet must not fail the snatch");
        match out {
            ReleaseFetch::Magnet(m) => assert_eq!(m, magnet),
            other => panic!("expected the magnet to be recovered, got {other:?}"),
        }
    }
}
