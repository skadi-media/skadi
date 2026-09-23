//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use std::collections::BTreeMap;
use std::sync::Mutex;

use skadi_cardigann::engine::{FetchError, FetchReq, FetchResp, Fetcher, Method};
use skadi_cardigann::{
    CardigannRelease, Catalog, Definition, LoadError, LoginOutcome, SearchInput,
};

/// A scripted tracker: canned `(url fragment, status, body)` routes, first match
/// wins, unmatched → 404. Records every request so steps can assert on the wire.
#[derive(Default)]
pub struct Scripted {
    pub routes: Vec<(String, u16, String)>,
    pub calls: Mutex<Vec<FetchReq>>,
}

#[async_trait::async_trait]
impl Fetcher for Scripted {
    async fn fetch(&self, req: FetchReq) -> Result<FetchResp, FetchError> {
        self.calls.lock().unwrap().push(req.clone());
        for (frag, status, body) in &self.routes {
            if req.url.contains(frag.as_str()) {
                return Ok(FetchResp {
                    status: *status,
                    final_url: req.url,
                    body: body.clone(),
                });
            }
        }
        Ok(FetchResp {
            status: 404,
            final_url: req.url,
            body: String::new(),
        })
    }
}

impl Scripted {
    pub fn calls(&self) -> Vec<FetchReq> {
        self.calls.lock().unwrap().clone()
    }
    pub fn posts(&self) -> Vec<FetchReq> {
        self.calls()
            .into_iter()
            .filter(|r| r.method == Method::Post)
            .collect()
    }
}

/// One scenario: a parsed definition, the scripted tracker, the search input and
/// the last outcome.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub def: Option<Definition>,
    pub load_error: Option<LoadError>,
    pub tracker: Scripted,
    pub keywords: String,
    pub categories: Vec<String>,
    pub query: BTreeMap<String, String>,
    pub overrides: BTreeMap<String, String>,
    pub base_url: String,
    pub releases: Vec<CardigannRelease>,
    pub error: Option<String>,
    pub login: Option<LoginOutcome>,
    /// `Some(Some(magnet))` resolved, `Some(None)` no download block applies.
    pub resolved: Option<Option<String>>,
    pub catalog: Option<Catalog>,
    pub catalog_errors: Vec<LoadError>,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("notes", &self.notes)
            .field("def", &self.def.as_ref().map(|d| &d.id))
            .field("keywords", &self.keywords)
            .field("releases", &self.releases.len())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn def(&self) -> &Definition {
        self.def
            .as_ref()
            .expect("a definition must be parsed first")
    }

    pub fn input(&self) -> SearchInput {
        SearchInput {
            keywords: self.keywords.clone(),
            categories: self.categories.clone(),
            query: self.query.clone(),
            config: skadi_cardigann::engine::resolve_config(self.def(), &self.overrides),
            base_url: self.base_url.clone(),
        }
    }

    pub fn release(&self, n: usize) -> &CardigannRelease {
        self.releases
            .get(n.saturating_sub(1))
            .unwrap_or_else(|| panic!("no release #{n} (got {})", self.releases.len()))
    }
}

/// A fixed "now" so relative-date filters are deterministic.
pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-09-06T12:00:00+00:00")
        .unwrap()
        .with_timezone(&chrono::Utc)
}
