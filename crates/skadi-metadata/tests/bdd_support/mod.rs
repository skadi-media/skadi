//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use skadi_metadata::{
    AudibleCatalogProvider, AudnexusProvider, AuthorMatch, CatalogItem, MetadataMatch,
    MetadataProvider, MetadataRecord, SeriesMetadata, ServarrProvider, SkyhookProvider,
    TmdbProvider,
};
use wiremock::MockServer;

/// Which concrete provider a scenario built (the trait object covers the common
/// surface; the TV / audiobook extras need the concrete type).
pub enum Provider {
    Tmdb(TmdbProvider),
    Servarr(ServarrProvider),
    Skyhook(SkyhookProvider),
    Audnexus(AudnexusProvider),
    Audible(AudibleCatalogProvider),
}

impl Provider {
    pub fn common(&self) -> &dyn MetadataProvider {
        match self {
            Provider::Tmdb(p) => p,
            Provider::Servarr(p) => p,
            Provider::Skyhook(p) => p,
            Provider::Audnexus(p) => p,
            Provider::Audible(_) => panic!("the Audible catalog is not a MetadataProvider"),
        }
    }
}

/// One scenario: an in-process mock of the upstream API, the provider under
/// test and the last outcome.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub server: Option<MockServer>,
    pub provider: Option<Provider>,
    pub matches: Vec<MetadataMatch>,
    pub record: Option<MetadataRecord>,
    pub series: Option<SeriesMetadata>,
    pub authors: Vec<AuthorMatch>,
    pub author: Option<skadi_metadata::AudnexusAuthor>,
    pub items: Vec<CatalogItem>,
    pub language: Option<Option<String>>,
    pub error: Option<String>,
    pub started: Option<std::time::Instant>,
    pub elapsed: Option<std::time::Duration>,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("notes", &self.notes)
            .field("matches", &self.matches.len())
            .field("record", &self.record.as_ref().map(|r| &r.title))
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn server(&self) -> &MockServer {
        self.server.as_ref().expect("a provider mock must exist")
    }
    pub fn provider(&self) -> &Provider {
        self.provider
            .as_ref()
            .expect("a provider must be configured first")
    }
    pub fn record(&self) -> &MetadataRecord {
        match (&self.record, &self.error) {
            (Some(r), _) => r,
            (None, e) => panic!("no record (error {e:?})"),
        }
    }
    pub fn series(&self) -> &SeriesMetadata {
        match (&self.series, &self.error) {
            (Some(s), _) => s,
            (None, e) => panic!("no series (error {e:?})"),
        }
    }
    pub async fn requests_to(&self, path: &str) -> usize {
        self.server()
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == path)
            .count()
    }
}

/// The daemon's client shape (3 retries) with millisecond backoff so 5xx paths
/// stay fast.
pub fn fast_http() -> skadi_http::HttpClient {
    skadi_http::HttpClient::with_config(
        std::time::Duration::from_secs(5),
        skadi_http::RetryConfig {
            max_retries: 3,
            base_backoff: std::time::Duration::from_millis(1),
        },
    )
    .expect("http client")
}

/// `MockServer::drop` verifies expectations with `futures::executor::block_on`;
/// doing that on a tokio worker thread while scenarios run concurrently can park
/// every worker (deadlock). Hand the server to a plain OS thread instead.
impl Drop for World {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = std::thread::spawn(move || drop(server)).join();
        }
    }
}
