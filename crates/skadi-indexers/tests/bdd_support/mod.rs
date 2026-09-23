//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod fixtures;
pub mod steps;

use std::collections::HashMap;
use std::time::Duration;

use skadi_core::{ExternalIds, IndexerId, MediaKind};
use skadi_indexers::{
    Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchMode, SearchQuery, TokenBucket,
};
use wiremock::MockServer;

/// A domain-style search query the steps assemble ("a movie query for X (1999)
/// carrying imdb tt…"). Implements the object-safe [`SearchQuery`] like the
/// movies/tv/audiobooks domains do.
#[derive(Debug, Clone)]
pub struct Query {
    pub kind: MediaKind,
    pub titles: Vec<String>,
    pub year: Option<u16>,
    pub ids: ExternalIds,
    pub cats: Vec<Category>,
    pub extra: Vec<(&'static str, String)>,
    pub mode: SearchMode,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            kind: MediaKind::Movie,
            titles: Vec::new(),
            year: None,
            ids: ExternalIds::default(),
            cats: Vec::new(),
            extra: Vec::new(),
            mode: SearchMode::Auto,
        }
    }
}

impl SearchQuery for Query {
    fn kind(&self) -> MediaKind {
        self.kind
    }
    fn titles(&self) -> &[String] {
        &self.titles
    }
    fn year(&self) -> Option<u16> {
        self.year
    }
    fn external_ids(&self) -> &ExternalIds {
        &self.ids
    }
    fn categories(&self) -> &[Category] {
        &self.cats
    }
    fn extra_params(&self) -> Vec<(&'static str, String)> {
        self.extra.clone()
    }
    fn mode(&self) -> SearchMode {
        self.mode
    }
}

/// One in-process scenario's state: in-process mock HTTP servers (never the
/// network), the indexer under test, and the last outcome.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// Named mock servers: `indexer` (torznab/prowlarr), `tracker` (cardigann site),
    /// `solver` (FlareSolverr).
    pub servers: HashMap<String, MockServer>,
    pub indexer_id: Option<IndexerId>,
    pub indexer: Option<Box<dyn Indexer>>,
    pub query: Query,
    pub releases: Vec<Release>,
    pub caps: Option<IndexerCaps>,
    /// Text of the last error from search/rss/caps/test/resolve, if any.
    pub error: Option<String>,
    /// Outcome of the last `resolve_fetch`.
    pub resolved: Option<ReleaseFetch>,
    /// Outcome of the last `test()` health check.
    pub test_ok: Option<bool>,
    /// A synthetic release built by the release steps.
    pub release: Option<Release>,
    pub key: Option<String>,
    pub config_json: Option<serde_json::Value>,
    pub build_error: Option<String>,
    pub bucket: Option<TokenBucket>,
    pub waits: Vec<Option<Duration>>,
    /// Last raw fetcher response (status, body) from the cardigann fetcher steps.
    pub fetch_resp: Option<(u16, String)>,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("notes", &self.notes)
            .field("query", &self.query)
            .field("releases", &self.releases.len())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn server(&self, name: &str) -> &MockServer {
        self.servers
            .get(name)
            .unwrap_or_else(|| panic!("no mock server named {name:?} in this scenario"))
    }

    pub async fn start_server(&mut self, name: &str) -> String {
        let s = MockServer::start().await;
        let uri = s.uri();
        self.servers.insert(name.to_string(), s);
        uri
    }

    pub fn indexer(&self) -> &dyn Indexer {
        self.indexer
            .as_deref()
            .expect("an indexer must be configured first")
    }

    pub fn release(&self, n: usize) -> &Release {
        self.releases
            .get(n.saturating_sub(1))
            .unwrap_or_else(|| panic!("no release #{n} (got {})", self.releases.len()))
    }

    /// Count requests the named mock server received whose query string carries
    /// `key=value`.
    pub async fn requests_with(&self, server: &str, key: &str, value: &str) -> usize {
        self.server(server)
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.query_pairs().any(|(k, v)| k == key && v == value))
            .count()
    }

    /// Whether the named mock server saw a request whose `key` query parameter
    /// contains `needle` (e.g. `cat=1,2` contains `2`).
    pub async fn saw_param_containing(&self, server: &str, key: &str, needle: &str) -> bool {
        self.server(server)
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| {
                r.url
                    .query_pairs()
                    .any(|(k, v)| k == key && v.contains(needle))
            })
    }

    /// Requests the named mock server received for `path`.
    pub async fn requests_to(&self, server: &str, path: &str) -> usize {
        self.server(server)
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == path)
            .count()
    }
}

/// A fast HTTP client for the mocks: same retry semantics as the daemon's
/// (3 retries), but millisecond backoff so a 5xx scenario stays sub-second.
pub fn fast_http() -> skadi_http::HttpClient {
    skadi_http::HttpClient::with_config(
        Duration::from_secs(5),
        skadi_http::RetryConfig {
            max_retries: 3,
            base_backoff: Duration::from_millis(1),
        },
    )
    .expect("http client")
}

/// `MockServer::drop` verifies expectations with `futures::executor::block_on`;
/// doing that on a tokio worker thread while dozens of scenarios run
/// concurrently can park every worker (deadlock). Hand the servers to a plain
/// OS thread so the runtime keeps driving the servers' shutdown.
impl Drop for World {
    fn drop(&mut self) {
        let servers = std::mem::take(&mut self.servers);
        if !servers.is_empty() {
            let _ = std::thread::spawn(move || drop(servers)).join();
        }
    }
}
