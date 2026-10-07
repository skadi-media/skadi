//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use std::collections::HashMap;

use skadi_downloader_worker::seed_policy::{SeedPolicy, SeedVerdict};
use skadi_store::{DownloadJob, DownloadJobRepo, Store};

/// One scenario: a throw-away SQLite `downloads` queue (the worker's side of the
/// daemon↔worker contract), named jobs, and the last outcome. The librqbit
/// session itself is out of scope here (network); the pure decision helpers and
/// the queue protocol are what the daemon relies on.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub store: Option<Store>,
    /// Scenario-local job names → row ids.
    pub jobs: HashMap<String, String>,
    /// Row ids → names (for reporting).
    pub names: HashMap<String, String>,
    /// The last `claim_next` answer.
    pub claimed: Option<Option<DownloadJob>>,
    /// The names the last budgeted claim tick (`claim_within_cap`) claimed.
    pub tick_claims: Option<Vec<String>>,
    pub reclaimed: Option<usize>,
    pub policy: Option<SeedPolicy>,
    pub verdict: Option<SeedVerdict>,
    pub config: Option<skadi_downloader_worker::Config>,
    pub config_error: Option<String>,
    pub path_out: Option<Option<String>>,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("notes", &self.notes)
            .field("jobs", &self.jobs)
            .field(
                "claimed",
                &self.claimed.as_ref().map(|c| c.as_ref().map(|j| &j.id)),
            )
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn store(&self) -> &Store {
        self.store.as_ref().expect("a queue store must exist")
    }
    pub fn id(&self, name: &str) -> &str {
        self.jobs
            .get(name)
            .unwrap_or_else(|| panic!("no job named {name:?}"))
    }
    pub async fn job(&self, name: &str) -> DownloadJob {
        self.store()
            .get_download(self.id(name))
            .await
            .expect("get_download")
            .unwrap_or_else(|| panic!("job {name:?} vanished"))
    }
}

/// A fresh SQLite-backed store with the schema migrated.
pub async fn temp_store() -> Store {
    let path = skadi_core::unique_temp_path("worker-bdd").with_extension("db");
    let store = Store::connect(&format!("sqlite://{}", path.display())).expect("sqlite store");
    store.run_migrations().await.expect("migrations");
    store
}
