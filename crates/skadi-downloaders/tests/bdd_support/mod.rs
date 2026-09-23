//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_indexers::Release;
use skadi_store::Store;

/// One scenario: a throw-away SQLite `downloads` queue (the daemon↔worker
/// contract), the downloader under test, and the last outcome.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub store: Option<Store>,
    pub downloader: Option<Box<dyn Downloader>>,
    pub release: Option<Release>,
    pub handle: Option<DownloadHandle>,
    pub add_error: Option<String>,
    pub status: Option<Result<DownloadStatus, String>>,
    pub remove_error: Option<String>,
    pub test_result: Option<Result<(), String>>,
    pub config_json: Option<serde_json::Value>,
    pub build_error: Option<String>,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("notes", &self.notes)
            .field("handle", &self.handle)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn store(&self) -> &Store {
        self.store.as_ref().expect("a queue store must exist")
    }
    pub fn downloader(&self) -> &dyn Downloader {
        self.downloader
            .as_deref()
            .expect("a downloader must be configured first")
    }
    pub fn handle(&self) -> &DownloadHandle {
        self.handle
            .as_ref()
            .expect("a download must be added first")
    }
}

/// A fresh SQLite-backed store with the schema migrated (same shape as the
/// daemon's Postgres, via diesel-dualdb).
pub async fn temp_store() -> Store {
    let path = skadi_core::unique_temp_path("dl-bdd").with_extension("db");
    let store = Store::connect(&format!("sqlite://{}", path.display())).expect("sqlite store");
    store.run_migrations().await.expect("migrations");
    store
}
