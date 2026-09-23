//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub item: Option<steps::framework::Item>,
    pub transition_ok: Option<bool>,
    pub module: Option<steps::framework::DummyModule>,
    // ---- C01 kernel (pass P5) ----
    /// The last error under test, rendered with `Display`.
    pub error_text: Option<String>,
    /// The last error's variant name.
    pub error_variant: Option<String>,
    /// The last JSON text produced.
    pub json: Option<String>,
    /// A scratch directory for filesystem probes (removed on drop).
    pub tmp: Option<steps::kernel::ScratchDir>,
    /// The last root-folder probe.
    pub root_status: Option<skadi_core::RootFolderStatus>,
}
