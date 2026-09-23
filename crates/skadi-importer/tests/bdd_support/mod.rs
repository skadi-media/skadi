//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
//!
//! The world owns one private scratch directory per scenario (`<tmp>/skadi-bdd-imp-…`)
//! holding a `downloads/` source area and a `library/` root, and a **scripted
//! matcher** — the domain's `AcquirableMatcher` seam is replaced by per-file rules
//! the scenario declares ("map X to library path Y", "quarantine X", "ignore X").
//! Nothing outside that directory is ever touched; it is removed on drop (after
//! restoring any permissions a scenario tightened).
pub mod steps;

use std::path::{Path, PathBuf};

use skadi_downloaders::DownloadHandle;
use skadi_importer::{
    AcquirableMatch, AcquirableMatcher, AcquirableRef, CollisionPolicy, CompletedDownload,
    DefaultImporter, FileDisposition, ImportOutcome, ImportPlan, Placement,
};
use skadi_quality::ParsedRelease;

/// One scripted matcher decision for a source file name.
#[derive(Clone, Debug)]
pub enum Rule {
    Place {
        file: String,
        acquirable: String,
        dest: PathBuf,
        /// `None` = the domain's default (`Skip`, or `Overwrite` once it supersedes).
        policy: Option<CollisionPolicy>,
        supersedes: Vec<PathBuf>,
        sidecars: Vec<(PathBuf, String)>,
    },
    Quarantine {
        file: String,
        dest: PathBuf,
    },
}

/// The domain seam, scripted by the scenario. A file with no rule is `Ignore`
/// ("no matching acquirable"), exactly like a domain matcher that finds nothing.
#[derive(Clone, Debug, Default)]
pub struct Scripted {
    pub rules: Vec<Rule>,
}

impl AcquirableMatcher for Scripted {
    fn match_file(
        &self,
        _parsed: &ParsedRelease,
        source: &Path,
        _completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        let name = source.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.rules
            .iter()
            .filter_map(|r| match r {
                Rule::Place {
                    file,
                    acquirable,
                    dest,
                    policy,
                    supersedes,
                    sidecars,
                } if file == name => {
                    let mut m =
                        AcquirableMatch::new(AcquirableRef(acquirable.clone()), dest.clone())
                            .with_sidecars(sidecars.clone());
                    if !supersedes.is_empty() {
                        m = m.superseding(supersedes.clone());
                    }
                    if let Some(p) = policy {
                        m = m.with_collision(*p);
                    }
                    Some(m)
                }
                _ => None,
            })
            .collect()
    }

    fn disposition(
        &self,
        parsed: &ParsedRelease,
        source: &Path,
        completed: &CompletedDownload,
    ) -> FileDisposition {
        let name = source.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(Rule::Quarantine { dest, .. }) = self
            .rules
            .iter()
            .find(|r| matches!(r, Rule::Quarantine { file, .. } if file == name))
        {
            return FileDisposition::Quarantine(dest.clone());
        }
        self.match_file(parsed, source, completed).into()
    }
}

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// This scenario's private scratch directory (removed on drop).
    pub tmp: PathBuf,
    /// Source files of the completed download, in declaration order.
    pub sources: Vec<PathBuf>,
    /// The scripted domain decisions.
    pub rules: Vec<Rule>,
    /// `DefaultImporter` knobs.
    pub min_free: u64,
    /// SKADI-T-0419 knobs.
    pub use_hardlinks: bool,
    pub file_mode: Option<u32>,
    pub min_file: u64,
    /// Results.
    pub outcome: Option<ImportOutcome>,
    pub plan: Option<ImportPlan>,
    pub place_result: Option<Result<Placement, String>>,
    pub scanned: Option<Result<Vec<PathBuf>, String>>,
    pub removed: Vec<PathBuf>,
    pub root_status: Option<skadi_core::RootFolderStatus>,
    /// Library roots the importer is configured with (SKADI-T-0417). Empty ⇒ the
    /// root check is off, which is what most scenarios want.
    pub library_roots: Vec<PathBuf>,
    pub require_root_marker: bool,
    /// Recycle bin the importer retires superseded files to (SKADI-T-0418).
    pub recycle_bin: Option<PathBuf>,
    /// SKADI-T-0138: remove the source after a successful place.
    pub move_on_import: bool,
    pub derived_root: Option<skadi_core::RootFolder>,
    pub space: Option<Option<u64>>,
    /// Quality-derivation scratch (adoption review, SKADI-T-0399).
    pub parsed: Option<ParsedRelease>,
    pub quality_name: Option<String>,
    /// Directories whose mode a scenario tightened; restored before cleanup.
    pub restore_perms: Vec<PathBuf>,
}

impl World {
    fn new() -> Self {
        let tmp = skadi_core::unique_temp_path("bdd-imp");
        std::fs::create_dir_all(tmp.join("downloads")).expect("scratch downloads dir");
        std::fs::create_dir_all(tmp.join("library")).expect("scratch library dir");
        Self {
            notes: Vec::new(),
            tmp,
            sources: Vec::new(),
            rules: Vec::new(),
            min_free: 0,
            use_hardlinks: true,
            file_mode: None,
            min_file: 0,
            library_roots: Vec::new(),
            require_root_marker: false,
            recycle_bin: None,
            move_on_import: false,
            outcome: None,
            plan: None,
            place_result: None,
            scanned: None,
            removed: Vec::new(),
            root_status: None,
            derived_root: None,
            space: None,
            parsed: None,
            quality_name: None,
            restore_perms: Vec::new(),
        }
    }

    /// The download (source) area.
    pub fn downloads(&self) -> PathBuf {
        self.tmp.join("downloads")
    }

    /// The library root.
    pub fn library(&self) -> PathBuf {
        self.tmp.join("library")
    }

    /// A source file path by name.
    pub fn src(&self, name: &str) -> PathBuf {
        self.downloads().join(name)
    }

    /// A library path by root-relative path.
    pub fn lib(&self, rel: &str) -> PathBuf {
        self.library().join(rel)
    }

    /// The completed download as the importer sees it.
    pub fn completed(&self) -> CompletedDownload {
        CompletedDownload {
            handle: DownloadHandle {
                native_id: "bdd-handle".into(),
                category: "2000".into(),
            },
            files: self.sources.clone(),
            category: "2000".into(),
        }
    }

    /// The importer under test, built from the scenario's rules + knobs.
    pub fn importer(&self) -> DefaultImporter<Scripted> {
        DefaultImporter::new(Scripted {
            rules: self.rules.clone(),
        })
        .with_min_free_bytes(self.min_free)
        .with_min_file_bytes(self.min_file)
        .with_hardlinks(self.use_hardlinks)
        .with_file_mode(self.file_mode)
        .with_library_roots(self.library_roots.clone())
        .with_require_root_marker(self.require_root_marker)
        .with_recycle_bin(self.recycle_bin.clone())
        .with_move_on_import(self.move_on_import)
    }

    pub fn outcome(&self) -> &ImportOutcome {
        self.outcome.as_ref().expect("the download was imported")
    }

    pub fn plan(&self) -> &ImportPlan {
        self.plan.as_ref().expect("the import was previewed")
    }

    /// Make a directory read-only for the scenario (restored on drop).
    #[cfg(unix)]
    pub fn make_read_only(&mut self, dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod 555");
        self.restore_perms.push(dir.to_path_buf());
    }
}

impl Drop for World {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in &self.restore_perms {
                let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}
