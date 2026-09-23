//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod fixtures;
pub mod steps;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use skadi_core::MediaKind;
use skadi_hunter::{AcquireState, HunterServices, InMemoryStatusSink};
use skadi_quality::{CustomFormat, QualityDefinition, QualityProfile};

use fixtures::{RecordingNotifier, ScriptedDownloader};

/// One in-process scenario's state. Scenarios that touch the process-global
/// service registry or in-flight tracker are tagged `@serial` in the feature
/// files so cucumber never runs two of them at once.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    // --- decision engine (pure `pipeline::decide` / `explain`) ---
    pub kind: Option<MediaKind>,
    pub state: Option<AcquireState>,
    /// Named states for multi-acquirable scenarios (`"S03E01"` → state).
    pub states: HashMap<String, AcquireState>,
    pub profile: Option<QualityProfile>,
    pub definitions: Vec<QualityDefinition>,
    pub formats: Vec<CustomFormat>,
    pub blocklisted: HashSet<String>,
    pub min_seeders: u32,
    /// Reachability policy for `decide` (SKADI-T-0598): `None` ⇒ the
    /// quality-first order every quality-ladder scenario was written against;
    /// `Some((prefer_seeders, floor_height))` opts in.
    pub reachability: Option<(bool, u16)>,
    pub current_quality: Option<skadi_core::QualityId>,
    pub current_format_score: Option<i32>,
    /// `Ok(())` or the error text of the last `decide`/stage call.
    pub outcome: Option<Result<(), String>>,
    // --- pipeline stages with fakes ---
    pub indexers: Vec<Arc<dyn skadi_indexers::Indexer>>,
    pub downloader: Option<Arc<ScriptedDownloader>>,
    pub notifiers: Vec<Arc<RecordingNotifier>>,
    pub tmp: Option<Arc<tempfile::TempDir>>,
    // --- step bodies against the service registry ---
    pub services: Option<Arc<HunterServices>>,
    pub status: Option<Arc<InMemoryStatusSink>>,
    pub store: Option<skadi_store::Store>,
    /// Cloacina contexts for named runs (`"run-a"` → context).
    pub contexts: HashMap<String, cloacina::Context<serde_json::Value>>,
    /// Acquirable refs this scenario registered in the global tracker (cleared
    /// at the end of each `@serial` scenario).
    pub tracked_refs: Vec<String>,
    /// The tracker run ids this scenario started, by acquirable ref.
    pub run_ids: HashMap<String, String>,
    /// Last `try_start` answer.
    pub last_claim: Option<bool>,
    /// Last `adopt` answer.
    pub last_ownership: Option<skadi_hunter::tracker::Ownership>,
    /// Last `expire_adopted` answer.
    pub expired: Vec<String>,
    /// Last `observe_progress` flush answer.
    pub last_flush: Option<bool>,
    /// Register the domain with an importer whose matcher places nothing.
    pub reject_imports: bool,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("kind", &self.kind)
            .field("notes", &self.notes)
            .field("outcome", &self.outcome)
            .field("tracked_refs", &self.tracked_refs)
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn kind(&self) -> MediaKind {
        self.kind.unwrap_or(MediaKind::Movie)
    }

    pub fn state_mut(&mut self) -> &mut AcquireState {
        self.state
            .as_mut()
            .expect("a wanted item must be set up first")
    }

    pub fn profile(&self) -> &QualityProfile {
        self.profile
            .as_ref()
            .expect("a profile must be set up first")
    }
}
