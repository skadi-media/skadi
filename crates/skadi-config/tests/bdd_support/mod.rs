//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use skadi_config::{ConfigError, ConfigView};

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// The typed view under test (C03), built from a table snapshot.
    pub view: Option<ConfigView>,
    /// The last typed-read outcome, stringified (`Ok(value)` / the error).
    pub last: Option<Result<String, ConfigError>>,
    /// What `read_env` collected (C03 env seeding).
    pub seeded: Vec<(&'static str, String)>,
    /// Env vars this scenario set, restored afterwards (`@serial` only).
    pub env_touched: Vec<(String, Option<String>)>,
    /// The error `read_env` returned, rendered (C37 secret files).
    pub seed_error: Option<String>,
    /// Scratch directory holding this scenario's secret files (C37).
    pub secret_dir: Option<std::path::PathBuf>,
}
