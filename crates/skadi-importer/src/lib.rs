//! `skadi-importer` — finalize a completed download into the library.
//!
//! Purely mechanical and **edition-agnostic**: it parses each media file's
//! name, asks the injected [`AcquirableMatcher`] (supplied by the domain) which
//! acquirable(s) each file satisfies and where each should live, then hardlinks
//! (copy fallback across filesystems) into place. One release may satisfy many
//! acquirables, so [`ImportOutcome`] is plural. All "what does this satisfy" /
//! edition / naming judgment lives in the domain's matcher — never here.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use skadi_core::{AppError, FileRef, Result};
use skadi_downloaders::DownloadHandle;
use skadi_quality::{ParsedRelease, parse};

/// An opaque reference to a domain acquirable (e.g. a `MovieEditionId` encoded
/// by the domain). The importer never interprets it.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct AcquirableRef(pub String);

/// What to do when a match's destination path already holds a file (SKADI-T-0217).
/// The domain chooses per match; the default is the safe `Skip`.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub enum CollisionPolicy {
    /// Leave the existing file untouched and reject this match ("destination exists").
    #[default]
    Skip,
    /// Replace the existing file (an upgrade): remove it, then place the new one.
    Overwrite,
    /// Treat an existing destination as an error for this match.
    Error,
}

/// The domain's decision for one source file: which acquirable it satisfies, the
/// absolute destination path, what to do if that path is already occupied, and any
/// existing library files this import **supersedes** (an upgrade).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct AcquirableMatch {
    pub acquirable: AcquirableRef,
    pub dest: PathBuf,
    /// Collision handling for `dest` (SKADI-T-0217). Defaults to [`CollisionPolicy::Skip`].
    pub on_collision: CollisionPolicy,
    /// Existing library files this import replaces (SKADI-T-0218). After the new file
    /// lands, each is removed (best-effort) and reported in [`ImportOutcome::replaced`].
    /// Lets an upgrade with a *different* filename clean up the old file rather than
    /// orphaning it. The just-placed `dest` is never removed even if listed.
    pub supersedes: Vec<PathBuf>,
    /// Generated sidecar files to write **next to** the placed video (SKADI-T-0298),
    /// as `(absolute path, contents)`. Domain-supplied (e.g. a Kodi/Jellyfin `.nfo`).
    /// Written best-effort after the video lands; a failure never fails the import.
    pub sidecars: Vec<(PathBuf, String)>,
}

impl AcquirableMatch {
    /// A match that skips if its destination already exists (the safe default).
    #[must_use]
    pub fn new(acquirable: AcquirableRef, dest: PathBuf) -> Self {
        Self {
            acquirable,
            dest,
            on_collision: CollisionPolicy::Skip,
            supersedes: Vec::new(),
            sidecars: Vec::new(),
        }
    }

    /// Attach generated sidecar files (`(path, contents)`) to write next to the
    /// placed video, best-effort (SKADI-T-0298).
    #[must_use]
    pub fn with_sidecars(mut self, sidecars: Vec<(PathBuf, String)>) -> Self {
        self.sidecars = sidecars;
        self
    }

    /// Set the collision policy (e.g. `Overwrite` for an upgrade).
    #[must_use]
    pub fn with_collision(mut self, on_collision: CollisionPolicy) -> Self {
        self.on_collision = on_collision;
        self
    }

    /// Mark existing library files this import supersedes (removed after a successful
    /// place). Implies an upgrade, so the destination collision policy is set to
    /// `Overwrite` unless already chosen.
    #[must_use]
    pub fn superseding(mut self, supersedes: Vec<PathBuf>) -> Self {
        self.supersedes = supersedes;
        if self.on_collision == CollisionPolicy::Skip {
            self.on_collision = CollisionPolicy::Overwrite;
        }
        self
    }
}

/// The superseded files actually worth removing for an upgrade (SKADI-T-0218): drop
/// the just-placed `dest` (never delete what we wrote) and de-duplicate. Pure.
#[must_use]
pub fn supersedes_to_remove(supersedes: &[PathBuf], placed_dest: &Path) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    supersedes
        .iter()
        .filter(|p| p.as_path() != placed_dest)
        .filter(|p| seen.insert((*p).clone()))
        .cloned()
        .collect()
}

/// What [`collision_decision`] resolves a `(dest_exists, policy)` pair to.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum CollisionDecision {
    /// Place the file (no existing destination, or policy says replace).
    Place,
    /// An existing file the policy says to leave alone — reject the match.
    Skip,
    /// An existing file the policy treats as a hard error.
    Error,
}

/// Pure collision resolution (SKADI-T-0217): given whether the destination exists
/// and the policy, decide whether to place, skip, or error.
#[must_use]
pub fn collision_decision(dest_exists: bool, policy: CollisionPolicy) -> CollisionDecision {
    if !dest_exists {
        return CollisionDecision::Place;
    }
    match policy {
        CollisionPolicy::Skip => CollisionDecision::Skip,
        CollisionPolicy::Overwrite => CollisionDecision::Place,
        CollisionPolicy::Error => CollisionDecision::Error,
    }
}

/// A finished transfer handed to the importer.
#[derive(Clone, Debug)]
pub struct CompletedDownload {
    pub handle: DownloadHandle,
    pub files: Vec<PathBuf>,
    pub category: String,
}

/// One placed file and the acquirable it satisfied.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct ImportedFile {
    pub acquirable: AcquirableRef,
    pub file: FileRef,
}

/// The result of importing a completed download.
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ImportOutcome {
    pub imported: Vec<ImportedFile>,
    /// Matches whose destination was **already occupied** and left alone under the
    /// `Skip` collision policy (SKADI-T-0385). Keyed like `imported` because the
    /// destination is derived from the acquirable's own identity, so a file already
    /// sitting there *is* that acquirable's library file — typically a duplicate
    /// download of something an earlier run imported. Not a rejection: the caller
    /// may count these as satisfied rather than fail the import.
    pub already_present: Vec<ImportedFile>,
    /// Source files that matched no acquirable, with a reason.
    pub rejected: Vec<(PathBuf, String)>,
    /// Probable wanted media set aside for manual review (SKADI-T-0309/0311) — the matcher
    /// could not confidently assign these to a library item. Not placed, not marked imported.
    pub quarantined: Vec<PathBuf>,
    /// Existing library files removed because an import superseded them (SKADI-T-0218).
    pub replaced: Vec<PathBuf>,
    /// Files that matched but whose placement **errored** (SKADI-T-0220), with the
    /// reason. Distinct from `rejected` (no match / skipped): these were attempted and
    /// failed, so the import is not a silent success — the caller can see what broke.
    pub failed: Vec<(PathBuf, String)>,
}

/// What a [`preview`](Importer::preview) says would happen to one match.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum PlannedAction {
    /// Place into a previously-empty destination.
    Place,
    /// Replace an existing file (overwrite, or an upgrade superseding old files).
    Replace,
}

/// One planned placement in an [`ImportPlan`].
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct PlannedImport {
    pub acquirable: AcquirableRef,
    pub dest: PathBuf,
    pub action: PlannedAction,
}

/// A **dry-run** of an import (SKADI-T-0224): the same matching + collision/space/
/// sample decisions as [`import`](Importer::import) but with **no files touched** — a
/// "what would happen" plan for the manual-import UI.
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ImportPlan {
    pub would_import: Vec<PlannedImport>,
    /// `(source-or-dest, reason)` for files that would be rejected (sample, no match,
    /// collision-skip, insufficient space, …).
    pub would_reject: Vec<(PathBuf, String)>,
    /// Existing library files that would be removed (superseded or overwritten).
    pub would_replace: Vec<PathBuf>,
    /// Files that would be quarantined for manual review (SKADI-T-0309/0311).
    pub would_quarantine: Vec<PathBuf>,
    /// Destinations already holding this acquirable's file, which the import
    /// would leave alone (SKADI-T-0428).
    ///
    /// Separate from `would_reject` because it is not a rejection: `import`
    /// reports the same case as `already_present`, a benign outcome. Reporting it
    /// as a rejection made the manual-import UI show a failure for a library that
    /// is simply already correct — and made the preview disagree with the import
    /// it is supposed to predict.
    pub already_present: Vec<PathBuf>,
}

/// What a domain's matcher decides for a single source file (SKADI-T-0309).
///
/// Most matchers only ever `Place` or `Ignore`, and convert from a match list via
/// [`From<Vec<AcquirableMatch>>`] (non-empty → `Place`, empty → `Ignore`). A matcher that
/// can recognize a file as *probable wanted media it cannot confidently assign* (e.g. a
/// book in a series pack whose title/position is ambiguous) returns `Quarantine` so the
/// importer sets it aside for manual review (SKADI-T-0311) rather than guessing a
/// destination — distinct from `Ignore` (non-media / sample / an item outside the library).
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum FileDisposition {
    /// Place the file at the given acquirable match(es).
    Place(Vec<AcquirableMatch>),
    /// Probable wanted media the matcher could not confidently assign: hardlink it into this
    /// review/quarantine destination (SKADI-T-0311) — chosen by the domain, which knows the
    /// library root — rather than guess a library path. Surfaced for manual import.
    Quarantine(PathBuf),
    /// Not wanted here (non-media, sample/extra, or outside the library) — skip silently.
    Ignore,
}

impl From<Vec<AcquirableMatch>> for FileDisposition {
    /// Convenience for matchers that only ever place-or-ignore: a non-empty match list is
    /// a `Place`, an empty one is an `Ignore`.
    fn from(matches: Vec<AcquirableMatch>) -> Self {
        if matches.is_empty() {
            FileDisposition::Ignore
        } else {
            FileDisposition::Place(matches)
        }
    }
}

/// Domain-supplied policy: given a parsed file and the completed download,
/// decide which acquirable(s) it satisfies and where each copy should go.
/// Owns *all* edition / naming / "what does this satisfy" logic.
pub trait AcquirableMatcher: Send + Sync {
    fn match_file(
        &self,
        parsed: &ParsedRelease,
        source: &Path,
        completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch>;

    /// Per-file disposition (SKADI-T-0309). Default: place whatever [`match_file`](Self::match_file)
    /// returns (empty → `Ignore`). A domain that can recognize *probable wanted media it cannot
    /// confidently assign* — e.g. an ambiguous book in a series pack — overrides this to return
    /// [`FileDisposition::Quarantine`], so the importer sets the file aside for manual review
    /// (SKADI-T-0311) instead of guessing a destination. Place-or-ignore matchers (movies, TV)
    /// keep the default untouched.
    fn disposition(
        &self,
        parsed: &ParsedRelease,
        source: &Path,
        completed: &CompletedDownload,
    ) -> FileDisposition {
        self.match_file(parsed, source, completed).into()
    }
}

/// Finalizes completed downloads into the library.
#[async_trait]
pub trait Importer: Send + Sync {
    async fn import(&self, completed: CompletedDownload) -> Result<ImportOutcome>;
    /// Dry-run the same decisions as [`import`](Self::import) without touching any
    /// files — the plan the manual-import UI previews (SKADI-T-0224).
    async fn preview(&self, completed: CompletedDownload) -> Result<ImportPlan>;
}

/// The default mechanical importer, parameterized over a domain matcher.
pub struct DefaultImporter<M: AcquirableMatcher> {
    matcher: M,
    /// Free bytes to keep on the destination filesystem (SKADI-T-0219). A placement
    /// that would breach this reserve is rejected. `0` ⇒ only reject a literal
    /// won't-fit.
    min_free_bytes: u64,
    /// Reject source files smaller than this as extras/samples (SKADI-T-0221). `0` ⇒
    /// no size floor (the default; the universal `looks_like_sample` name reject still
    /// applies, and domains keep their own thresholds).
    min_file_bytes: u64,
    /// Hardlink into the library when the filesystem allows it, falling back to a
    /// copy (SKADI-T-0419 — Sonarr's "Use Hardlinks instead of Copy"). `true` by
    /// default: a hardlink costs no space and keeps the torrent seedable, which is
    /// what almost every operator wants. Set `false` when the library and the
    /// download directory are on different filesystems and the copy fallback would
    /// fire anyway, or when a hardlinked file being edited in place would be a
    /// surprise.
    use_hardlinks: bool,
    /// Unix mode applied to each placed file (Sonarr's "Set Permissions"). `None`
    /// leaves whatever the filesystem gave it, which is the default because
    /// changing permissions is not something to do to an operator's library
    /// unasked.
    file_mode: Option<u32>,
    /// The library roots this importer may write into (SKADI-T-0417). Empty ⇒ the
    /// root check is skipped entirely, which is the historical behaviour and what
    /// a caller with no root configured still gets.
    ///
    /// When non-empty this is a boundary as well as a liveness check: a
    /// destination that falls under *none* of these roots is refused rather than
    /// created, so a bad naming template cannot scatter files outside the library.
    library_roots: Vec<PathBuf>,
    /// Require a [`ROOT_MARKER`] in the owning root before writing (SKADI-T-0417).
    require_root_marker: bool,
    /// Retire superseded files here instead of deleting them (SKADI-T-0418).
    /// `None` (the default) keeps the delete-outright behaviour.
    recycle_bin: Option<PathBuf>,
    /// Remove the source after a successful place (SKADI-T-0138). `false` (the
    /// default) hardlinks and keeps it, so the torrent keeps seeding.
    move_on_import: bool,
}

impl<M: AcquirableMatcher> DefaultImporter<M> {
    pub fn new(matcher: M) -> Self {
        Self {
            matcher,
            min_free_bytes: 0,
            min_file_bytes: 0,
            use_hardlinks: true,
            file_mode: None,
            library_roots: Vec::new(),
            require_root_marker: false,
            recycle_bin: None,
            move_on_import: false,
        }
    }

    /// Hardlink placed files (the default) or copy them (SKADI-T-0419).
    #[must_use]
    pub fn with_hardlinks(mut self, use_hardlinks: bool) -> Self {
        self.use_hardlinks = use_hardlinks;
        self
    }

    /// Apply `mode` to each placed file after placement (SKADI-T-0419). A
    /// hardlink shares its inode with the source, so setting a mode changes the
    /// source's permissions too — which is why this is off unless asked for.
    #[must_use]
    pub fn with_file_mode(mut self, mode: Option<u32>) -> Self {
        self.file_mode = mode;
        self
    }

    /// The library roots this importer may write into (SKADI-T-0417). Each is
    /// checked live before placement, and a destination outside all of them is
    /// refused. Empty (the default) skips the check.
    #[must_use]
    pub fn with_library_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.library_roots = roots;
        self
    }

    /// Require a [`ROOT_MARKER`] file in the owning library root before writing
    /// (SKADI-T-0417). Off by default; see [`root_problem`] for why the marker is
    /// never created for you.
    #[must_use]
    pub fn with_require_root_marker(mut self, require: bool) -> Self {
        self.require_root_marker = require;
        self
    }

    /// Remove the source after a successful place, making the import a **move**
    /// rather than a hardlink-and-seed (SKADI-T-0138).
    ///
    /// Implemented as link-then-unlink rather than `rename`: the file is placed
    /// exactly as it always is — collision handling, atomic overwrite and all —
    /// and only once it is provably at the destination is the source removed. A
    /// failure at any point therefore leaves the source intact, which `rename`
    /// could not promise across a filesystem boundary.
    #[must_use]
    pub fn with_move_on_import(mut self, move_on_import: bool) -> Self {
        self.move_on_import = move_on_import;
        self
    }

    /// Retire superseded files to `bin` instead of deleting them (SKADI-T-0418).
    /// `None` keeps the delete-outright behaviour.
    #[must_use]
    pub fn with_recycle_bin(mut self, bin: Option<PathBuf>) -> Self {
        self.recycle_bin = bin;
        self
    }

    /// Keep at least `bytes` free on the destination filesystem (the free-space
    /// reserve). Imports that would breach it are rejected.
    #[must_use]
    pub fn with_min_free_bytes(mut self, bytes: u64) -> Self {
        self.min_free_bytes = bytes;
        self
    }

    /// Reject source files smaller than `bytes` as extras/samples (the shared size
    /// floor, off by default).
    #[must_use]
    pub fn with_min_file_bytes(mut self, bytes: u64) -> Self {
        self.min_file_bytes = bytes;
        self
    }
}

#[async_trait]
impl<M: AcquirableMatcher> Importer for DefaultImporter<M> {
    async fn import(&self, completed: CompletedDownload) -> Result<ImportOutcome> {
        let mut outcome = ImportOutcome::default();
        for source in &completed.files {
            let name = source
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();

            // Shared sample/extras floor (SKADI-T-0221): reject obvious samples by name
            // and (if a size floor is configured) tiny files, before the domain matcher
            // even runs. Domains layer their own stricter rules on top.
            if looks_like_sample(source) {
                outcome
                    .rejected
                    .push((source.clone(), "sample (by name)".to_string()));
                continue;
            }
            if self.min_file_bytes > 0 {
                let src = source.clone();
                let size = tokio::task::spawn_blocking(move || {
                    std::fs::metadata(&src).map(|md| md.len()).ok()
                })
                .await
                .map_err(|e| AppError::Internal(format!("size-probe task panicked: {e}")))?;
                if let Some(size) = size
                    && size < self.min_file_bytes
                {
                    tracing::debug!(
                        src = ?source,
                        size,
                        floor = self.min_file_bytes,
                        "import rejected: below the size floor"
                    );
                    outcome.rejected.push((
                        source.clone(),
                        format!("below size floor: {size} < {} bytes", self.min_file_bytes),
                    ));
                    continue;
                }
            }

            let parsed = parse(name);
            let matches = match self.matcher.disposition(&parsed, source, &completed) {
                FileDisposition::Place(m) => m,
                FileDisposition::Quarantine(dest) => {
                    // SKADI-T-0311: probable wanted media we can't confidently place. Hardlink
                    // (copy fallback) it into the domain-chosen review area — preserved +
                    // discoverable by manual import, never guessed into a library folder. The
                    // source stays in place (seedable). Failure is isolated, not silent.
                    let src = source.clone();
                    let d = dest.clone();
                    let hardlinks = self.use_hardlinks;
                    // Pick a free name rather than clobbering an earlier
                    // quarantine of the same title (SKADI-T-0413). Both copies
                    // stay reviewable, which is the point of a review area.
                    let placed = tokio::task::spawn_blocking(move || {
                        let target = non_colliding_path(&d);
                        place_file(&src, &target, hardlinks)
                    })
                    .await
                    .map_err(|e| AppError::Internal(format!("quarantine task panicked: {e}")))?;
                    match placed {
                        Ok(()) => outcome.quarantined.push(dest),
                        Err(e) => outcome
                            .failed
                            .push((source.clone(), format!("quarantine placement failed: {e}"))),
                    }
                    continue;
                }
                FileDisposition::Ignore => {
                    tracing::debug!(src = ?source, "import rejected: no matching acquirable");
                    outcome
                        .rejected
                        .push((source.clone(), "no matching acquirable".to_string()));
                    continue;
                }
            };
            for m in matches {
                // Root-liveness gate (SKADI-T-0417). Must run before anything
                // creates a directory: placement `create_dir_all`s the whole
                // destination path, so a dropped mount silently redirected the
                // library onto the local disk under the mount point.
                if !self.library_roots.is_empty() {
                    let dest = m.dest.clone();
                    let roots = self.library_roots.clone();
                    let require_marker = self.require_root_marker;
                    let problem = tokio::task::spawn_blocking(move || {
                        // `probe_root_status` writes a probe file, so this is
                        // blocking I/O against a mount that may be stuck.
                        match owning_root(&dest, &roots) {
                            None => Some(format!(
                                "destination {dest:?} is outside every configured library root"
                            )),
                            Some(root) => root_problem(root, require_marker),
                        }
                    })
                    .await
                    .map_err(|e| AppError::Internal(format!("root-check task panicked: {e}")))?;
                    if let Some(problem) = problem {
                        // WARN, not debug: a failed root check means the library
                        // is not where we think it is, which is an operator
                        // problem, not a property of this file (SKADI-T-0426).
                        tracing::warn!(src = ?source, dest = ?m.dest, %problem, "import rejected: library root unusable");
                        // Keyed by the SOURCE like every other rejection
                        // (SKADI-T-0414), so the operator can match it back to a
                        // real file.
                        outcome.rejected.push((source.clone(), problem));
                        continue;
                    }
                }

                // Free-space pre-check (SKADI-T-0219): refuse to place a file the
                // destination filesystem can't hold (keeping `min_free_bytes` in
                // reserve) rather than half-filling the library. Best-effort — if the
                // size or free space can't be measured, the import proceeds.
                {
                    let src = source.clone();
                    let dest = m.dest.clone();
                    let reserve = self.min_free_bytes;
                    let verdict = tokio::task::spawn_blocking(move || {
                        let need = std::fs::metadata(&src).map(|md| md.len()).unwrap_or(0);
                        space_check(need, available_space(&dest), reserve)
                    })
                    .await
                    .map_err(|e| AppError::Internal(format!("space-check task panicked: {e}")))?;
                    if let SpaceVerdict::Insufficient { need, available } = verdict {
                        // Keyed by the SOURCE, like every other rejection
                        // (SKADI-T-0414). This one named the destination, so an
                        // operator reading the rejection list saw a library path
                        // that does not exist next to a set of real source files,
                        // with nothing to match it back to.
                        tracing::warn!(
                            src = ?source,
                            dest = ?m.dest,
                            need,
                            available,
                            "import rejected: insufficient free space"
                        );
                        outcome.rejected.push((
                            source.clone(),
                            format!(
                                "insufficient free space for {:?}: need {need} bytes, {available} available",
                                m.dest
                            ),
                        ));
                        continue;
                    }
                }

                // Place the file on the blocking pool: existence check + hard_link/
                // copy/rename are blocking syscalls and a real movie copy (or a stuck
                // mount) must never tie up an async worker — that would starve the HTTP
                // control plane (SKADI-T-0081). Collision handling (SKADI-T-0217)
                // resolves an already-occupied destination per the match's policy.
                let src = source.clone();
                let dest = m.dest.clone();
                let policy = m.on_collision;
                let hardlinks = self.use_hardlinks;
                let dest_for_cleanup = m.dest.clone();
                let placed = match tokio::task::spawn_blocking(move || {
                    place_with_collision(&src, &dest, policy, hardlinks)
                })
                .await
                .map_err(|e| AppError::Internal(format!("file-placement task panicked: {e}")))?
                {
                    Ok(p) => p,
                    // Partial-failure safety (SKADI-T-0220): a placement error is
                    // isolated to this file — record it (never a silent success), clean
                    // up any staging artifacts, and keep importing the rest. The atomic
                    // overwrite leaves an existing file intact on failure.
                    Err(e) => {
                        let d = dest_for_cleanup.clone();
                        let _ = tokio::task::spawn_blocking(move || cleanup_staging(&d)).await;
                        // ERROR: this file was wanted, matched and cleared every
                        // gate, and we still could not place it (SKADI-T-0426).
                        tracing::error!(src = ?source, dest = ?m.dest, error = %e, "import failed: placement error");
                        outcome.failed.push((m.dest, e.to_string()));
                        continue;
                    }
                };
                // Apply the operator's configured mode to the placed file
                // (SKADI-T-0419). Best-effort: a permissions failure must not undo
                // an otherwise-good import, and the file is already where it
                // belongs. Note a hardlink shares its inode with the source, so
                // this changes the source's permissions too — which is why the
                // setting is off unless asked for.
                if matches!(placed, Placement::Placed | Placement::Replaced)
                    && let Some(mode) = self.file_mode
                {
                    let d = m.dest.clone();
                    let _ = tokio::task::spawn_blocking(move || set_file_mode(&d, mode)).await;
                }
                match placed {
                    Placement::Placed | Placement::Replaced => {
                        // Write any generated sidecars (e.g. the Kodi/Jellyfin `.nfo`)
                        // next to the placed video — best-effort, blocking pool; a write
                        // failure never fails the import (SKADI-T-0298).
                        for (path, contents) in m.sidecars {
                            let _ = tokio::task::spawn_blocking(move || {
                                std::fs::write(&path, contents)
                            })
                            .await;
                        }
                        // Upgrade cleanup (SKADI-T-0218): remove any superseded library
                        // files (a prior import at a different path), best-effort, and
                        // report them. The just-placed dest is never in this list.
                        for old in supersedes_to_remove(&m.supersedes, &m.dest) {
                            let p = old.clone();
                            let bin = self.recycle_bin.clone();
                            let new_dest = m.dest.clone();
                            let removed = tokio::task::spawn_blocking(move || {
                                if !p.exists() {
                                    return false;
                                }
                                let gone = match &bin {
                                    // Retire rather than delete (SKADI-T-0418). If
                                    // retiring fails we deliberately leave the file
                                    // alone: deleting it anyway would be exactly the
                                    // data loss the bin exists to prevent.
                                    Some(bin) => match recycle(&p, bin) {
                                        Some(at) => {
                                            tracing::info!(from = ?p, to = ?at, "superseded file recycled");
                                            true
                                        }
                                        None => {
                                            tracing::warn!(file = ?p, ?bin, "could not recycle superseded file; leaving it in place");
                                            false
                                        }
                                    },
                                    None => std::fs::remove_file(&p).is_ok(),
                                };
                                if gone && let Some(parent) = p.parent() {
                                    // Prune only what this upgrade emptied: the
                                    // boundary is the deepest directory the old and
                                    // new paths share — the item's own folder — so
                                    // this can never climb out of the library.
                                    let boundary = common_ancestor(&p, &new_dest);
                                    for dir in prune_empty_dirs(parent, &boundary) {
                                        tracing::debug!(?dir, "pruned folder emptied by the upgrade");
                                    }
                                }
                                gone
                            })
                            .await
                            .map_err(|e| {
                                AppError::Internal(format!("supersede-cleanup task panicked: {e}"))
                            })?;
                            if removed {
                                outcome.replaced.push(old);
                            }
                        }
                        // A same-path overwrite is itself a replacement worth reporting.
                        if placed == Placement::Replaced {
                            outcome.replaced.push(m.dest.clone());
                        }
                        // Move mode (SKADI-T-0138): the file is provably at the
                        // destination, so drop the source and let the download
                        // directory stay clean. Best-effort — a source we cannot
                        // remove is clutter, never a failed import, and the
                        // library copy is already good.
                        if self.move_on_import {
                            let src = source.clone();
                            let removed =
                                tokio::task::spawn_blocking(move || std::fs::remove_file(&src))
                                    .await
                                    .map(|r| r.is_ok())
                                    .unwrap_or(false);
                            if !removed {
                                tracing::warn!(
                                    src = ?source,
                                    "move-on-import: could not remove the source (the library copy is placed)"
                                );
                            }
                        }
                        tracing::info!(
                            acquirable = %m.acquirable.0,
                            src = ?source,
                            dest = ?m.dest,
                            replaced = (placed == Placement::Replaced),
                            moved = self.move_on_import,
                            "imported"
                        );
                        outcome.imported.push(ImportedFile {
                            acquirable: m.acquirable,
                            file: FileRef { path: m.dest },
                        });
                    }
                    // The destination is already this acquirable's file (SKADI-T-0385):
                    // report it as present, not rejected, so a re-download of an
                    // already-imported item resolves instead of failing forever.
                    Placement::Skipped => {
                        tracing::info!(
                            acquirable = %m.acquirable.0,
                            src = ?source,
                            dest = ?m.dest,
                            "already present: destination holds this acquirable's file"
                        );
                        outcome.already_present.push(ImportedFile {
                            acquirable: m.acquirable,
                            file: FileRef { path: m.dest },
                        });
                    }
                }
            }
        }
        Ok(outcome)
    }

    async fn preview(&self, completed: CompletedDownload) -> Result<ImportPlan> {
        let mut plan = ImportPlan::default();
        for source in &completed.files {
            let name = source
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();

            // Same floors as `import`, read-only (SKADI-T-0224).
            if looks_like_sample(source) {
                plan.would_reject
                    .push((source.clone(), "sample (by name)".to_string()));
                continue;
            }
            // Read-only probe of size + (per match below) free space, on the blocking
            // pool — `preview` never mutates the filesystem.
            let src_size = {
                let s = source.clone();
                tokio::task::spawn_blocking(move || std::fs::metadata(&s).map(|md| md.len()).ok())
                    .await
                    .map_err(|e| AppError::Internal(format!("size-probe task panicked: {e}")))?
            };
            if self.min_file_bytes > 0
                && let Some(size) = src_size
                && size < self.min_file_bytes
            {
                plan.would_reject.push((
                    source.clone(),
                    format!("below size floor: {size} < {} bytes", self.min_file_bytes),
                ));
                continue;
            }

            let parsed = parse(name);
            let matches = match self.matcher.disposition(&parsed, source, &completed) {
                FileDisposition::Place(m) => m,
                FileDisposition::Quarantine(dest) => {
                    plan.would_quarantine.push(dest);
                    continue;
                }
                FileDisposition::Ignore => {
                    plan.would_reject
                        .push((source.clone(), "no matching acquirable".to_string()));
                    continue;
                }
            };
            for m in matches {
                // Read-only facts for this destination (existence + free space).
                let dest = m.dest.clone();
                let reserve = self.min_free_bytes;
                let need = src_size.unwrap_or(0);
                let (exists, available) =
                    tokio::task::spawn_blocking(move || (dest.exists(), available_space(&dest)))
                        .await
                        .map_err(|e| {
                            AppError::Internal(format!("preview-probe task panicked: {e}"))
                        })?;

                if let SpaceVerdict::Insufficient { need, available } =
                    space_check(need, available, reserve)
                {
                    plan.would_reject.push((
                        m.dest,
                        format!(
                            "insufficient free space: need {need} bytes, {available} available"
                        ),
                    ));
                    continue;
                }
                match collision_decision(exists, m.on_collision) {
                    // Not a rejection: `import` reports this as `already_present`
                    // and touches nothing (SKADI-T-0428).
                    CollisionDecision::Skip => plan.already_present.push(m.dest),
                    CollisionDecision::Error => plan
                        .would_reject
                        .push((m.dest, "destination already exists (error)".to_string())),
                    CollisionDecision::Place => {
                        for old in supersedes_to_remove(&m.supersedes, &m.dest) {
                            // Only existing files would actually be removed.
                            let o = old.clone();
                            let would = tokio::task::spawn_blocking(move || o.exists())
                                .await
                                .map_err(|e| {
                                    AppError::Internal(format!("preview-probe task panicked: {e}"))
                                })?;
                            if would {
                                plan.would_replace.push(old);
                            }
                        }
                        if exists {
                            plan.would_replace.push(m.dest.clone());
                        }
                        plan.would_import.push(PlannedImport {
                            acquirable: m.acquirable,
                            dest: m.dest,
                            action: if exists {
                                PlannedAction::Replace
                            } else {
                                PlannedAction::Place
                            },
                        });
                    }
                }
            }
        }
        Ok(plan)
    }
}

/// Collect the regular files to import from an operator-supplied path (SKADI-T-0222,
/// manual import): a single file yields itself; a directory is walked recursively.
/// Results are sorted for deterministic ordering. Errors only on an unreadable root.
pub fn scan_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let meta = std::fs::metadata(root)?;
    if meta.is_file() {
        out.push(root.to_path_buf());
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue; // unreadable subdir — skip, don't fail the whole scan
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(path),
                Ok(ft) if ft.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Container extensions that can hold a film or an episode. The video domains'
/// matchers refuse anything else (SKADI-T-0591): a season pack shipped per-episode
/// PNG screencaps named `s13e01 …png`, the TV matcher matched them by episode
/// code, placed them as the episodes and deleted the real files they superseded.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "mts", "webm", "mpg", "mpeg", "flv",
    "ogm", "ogv", "divx", "vob", "3gp",
];

/// Whether `path` has a video container extension (case-insensitive).
#[must_use]
pub fn is_video_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        let e = e.to_ascii_lowercase();
        VIDEO_EXTENSIONS.contains(&e.as_str())
    })
}

/// The name of the download's own top-level folder — the deepest directory all
/// of its files share — or the single file's parent for a one-file download.
/// `None` when the download reports no files. Release folders usually carry the
/// full title a file inside abbreviates (`Mystery Science Theater 3000 MST3K s13
/// …/MST3K s13e01 ….mkv`), so a matcher can fall back to it for identity
/// (SKADI-T-0591).
#[must_use]
pub fn download_root_name(completed: &CompletedDownload) -> Option<String> {
    let mut iter = completed.files.iter();
    let first = iter.next()?;
    let mut common: PathBuf = first.parent()?.to_path_buf();
    for f in iter {
        while !f.starts_with(&common) {
            common = common.parent()?.to_path_buf();
        }
    }
    common
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

/// A universal sample/extra reject by **name** (SKADI-T-0221): a file or any path
/// component literally called `sample`, or a filename with a `sample` token. Pure and
/// domain-agnostic — the shared floor every domain gets before its own (size/extension)
/// rules run. Size is deliberately *not* part of this floor: the right size threshold
/// is domain-specific (a movie sample is ~tens of MB; an audiobook chapter is tiny).
#[must_use]
pub fn looks_like_sample(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // A `sample` token in the filename (`sample.mkv`, `movie-sample.mkv`, `sample_x`).
    if name == "sample"
        || name.starts_with("sample.")
        || name.starts_with("sample-")
        || name.starts_with("sample_")
        || name.contains("-sample.")
        || name.contains(".sample.")
        || name.contains("_sample.")
    {
        return true;
    }
    // A `Sample/` directory anywhere in the path.
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("sample"))
    })
}

/// Free-space verdict for a planned placement (SKADI-T-0219).
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum SpaceVerdict {
    /// Enough room (or the check couldn't run / is disabled).
    Fits,
    /// Not enough room: `need` bytes required, `available` free (after reserve).
    Insufficient { need: u64, available: u64 },
}

/// Pure free-space decision (SKADI-T-0219): does `need` fit in `available` while
/// keeping `reserve` bytes free? `available == None` (couldn't query) ⇒ `Fits` — the
/// check is best-effort and never blocks an import it can't measure.
#[must_use]
pub fn space_check(need: u64, available: Option<u64>, reserve: u64) -> SpaceVerdict {
    match available {
        None => SpaceVerdict::Fits,
        Some(avail) => {
            let usable = avail.saturating_sub(reserve);
            if need <= usable {
                SpaceVerdict::Fits
            } else {
                SpaceVerdict::Insufficient {
                    need,
                    available: usable,
                }
            }
        }
    }
}

/// Bytes available to an unprivileged user on the filesystem holding `path` (or its
/// nearest existing ancestor, since the destination dir may not exist yet). `None`
/// when it can't be determined (non-unix, or the syscall fails) so the caller treats
/// the space check as a no-op. Blocking (a `statvfs` syscall).
#[must_use]
pub fn available_space(path: &Path) -> Option<u64> {
    // Walk up to the nearest path that exists — `statvfs` needs a real entry.
    let mut probe = path;
    loop {
        if probe.exists() {
            break;
        }
        probe = probe.parent()?;
    }
    statvfs_available(probe)
}

#[cfg(unix)]
fn statvfs_available(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid NUL-terminated path; `stat` is fully written by a
    // successful `statvfs` before we read it.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    // Available blocks to an unprivileged process × fragment size.
    //
    // `libc::statvfs` field widths differ by platform: on Linux — the only
    // place skadi actually runs, since it ships as a container — these are
    // already `u64` and the cast is a no-op, so clippy calls it unnecessary.
    // On macOS, where this is developed, `f_bavail` is narrower and the
    // widening is required to compile at all. No single spelling is clean on
    // both: `u64::from` trades this lint for `useless_conversion` on macOS.
    // The cast is correct everywhere; only the lint is platform-specific.
    #[allow(clippy::unnecessary_cast)]
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(unix))]
fn statvfs_available(_path: &Path) -> Option<u64> {
    None
}

/// The filename an operator drops into a library root to assert "this is the real
/// filesystem, not an empty mount point" (SKADI-T-0417).
pub const ROOT_MARKER: &str = ".skadi-root";

/// Why a library root cannot be written to right now, or `None` if it is fine
/// (SKADI-T-0417).
///
/// Sonarr/Radarr refuse an import into a missing root ("root folder is missing")
/// rather than creating it. We did the opposite: placement `create_dir_all`s the
/// destination, so a dropped NAS mount left an empty mount point on the local
/// disk and the import quietly filled *that* — the library looked empty while the
/// system disk filled up.
///
/// Two levels, because they are not equally achievable:
///
/// 1. **Always**: the root must exist, be a directory, and be writable. This is
///    Sonarr's check. It catches an unmount that leaves nothing behind, and it is
///    what the `create_dir_all` was papering over.
/// 2. **`require_marker`**: a [`ROOT_MARKER`] file must be present in the root.
///    The marker lives *on the mounted filesystem*, so it disappears when the
///    mount does — the only reliable way to tell a live mount from an empty mount
///    point that happens to be writable local disk.
///
/// Level 2 is opt-in and the marker is **never created automatically**. Creating
/// it would write onto whatever filesystem is present at that moment; if the
/// mount were already down that is the local disk, which defeats the check and
/// hands the operator false confidence. The operator runs `touch
/// /library/.skadi-root` once, while the mount is up.
#[must_use]
pub fn root_problem(root: &Path, require_marker: bool) -> Option<String> {
    let status = skadi_core::probe_root_status(root);
    if let Some(p) = status.problem() {
        return Some(format!("library root {root:?} is unusable: {p}"));
    }
    if require_marker && !root.join(ROOT_MARKER).exists() {
        return Some(format!(
            "library root {root:?} has no {ROOT_MARKER} marker — refusing to write \
             (this is what a dropped mount looks like)"
        ));
    }
    None
}

/// Whether `dest` is inside `root`, comparing whole path components so that
/// `/library/movies-old` is not treated as living inside `/library/movies`.
fn is_under(dest: &Path, root: &Path) -> bool {
    dest.starts_with(root)
}

/// Resolve which configured root `dest` belongs to (the longest match, so a
/// nested root wins over its parent), or `None` when it belongs to none.
fn owning_root<'a>(dest: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|r| is_under(dest, r))
        .max_by_key(|r| r.components().count())
}

/// Retire `file` to the recycle bin at `bin` instead of deleting it
/// (SKADI-T-0418, Sonarr/Radarr "Recycling Bin").
///
/// An upgrade used to `remove_file` the superseded copy outright, so a bad grab
/// — a mislabelled 2160p that is really an upscale, a broken remux — destroyed a
/// good file with no way back. The bin makes that recoverable.
///
/// Moved by rename where possible, falling back to copy-then-delete across
/// filesystems. Returns the path it now lives at, or `None` if it could not be
/// retired — in which case the caller must **not** delete the original, because
/// failing to retire it and then removing it anyway would be exactly the data
/// loss this exists to prevent.
#[must_use]
pub fn recycle(file: &Path, bin: &Path) -> Option<PathBuf> {
    if std::fs::create_dir_all(bin).is_err() {
        return None;
    }
    let dest = non_colliding_path(&bin.join(file.file_name()?));
    if std::fs::rename(file, &dest).is_ok() {
        return Some(dest);
    }
    // Cross-filesystem: copy then remove. If the copy fails there is nothing to
    // clean up; if the remove fails the file is in the bin *and* in place, which
    // is a duplicate rather than a loss, so the caller still treats it as retired.
    std::fs::copy(file, &dest).ok()?;
    let _ = std::fs::remove_file(file);
    Some(dest)
}

/// Remove directories left empty by a removed file, walking up from `from` and
/// stopping below `boundary` (SKADI-T-0418, Radarr "Delete empty folders").
///
/// `boundary` is never removed and never climbed past. Callers pass the deepest
/// directory that must survive — for an upgrade, the common ancestor of the old
/// and new paths, which is the item's own folder. That bound means this can only
/// ever remove folders the upgrade itself emptied, with no configured root
/// required and no way to climb out of the library.
///
/// Best-effort throughout: a non-empty directory (or one we cannot remove) stops
/// the walk rather than erroring. Returns the directories actually removed.
#[must_use]
pub fn prune_empty_dirs(from: &Path, boundary: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut dir = from.to_path_buf();
    while dir != boundary && dir.starts_with(boundary) {
        // `remove_dir` only succeeds on an empty directory, so this can never
        // take anything that still holds media.
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
        removed.push(dir.clone());
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    removed
}

/// The deepest directory that contains both paths — the boundary an upgrade's
/// folder pruning must not climb past.
#[must_use]
pub fn common_ancestor(a: &Path, b: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for (x, y) in a.components().zip(b.components()) {
        if x != y {
            break;
        }
        out.push(x);
    }
    out
}

/// Files in the recycle bin older than `retention` (SKADI-T-0418), so the bin
/// does not grow without bound.
///
/// Pure over the (path, age) pairs the caller measured, so the retention rule is
/// unit-testable without touching a clock or a filesystem. A file whose age
/// cannot be read is **kept**: the bin exists so a superseded file can be
/// recovered, and deleting one because we could not stat it would defeat that.
#[must_use]
pub fn expired_in_bin(
    entries: &[(PathBuf, Option<std::time::Duration>)],
    retention: std::time::Duration,
) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|(_, age)| age.is_some_and(|a| a > retention))
        .map(|(p, _)| p.clone())
        .collect()
}

/// Delete recycle-bin entries older than `retention_days`, returning what was
/// removed. Blocking. `retention_days == 0` disables the sweep — an unbounded
/// bin is a disk-space problem, but deleting the operator's only copy of a
/// superseded file on a default they never set would be worse.
#[must_use]
pub fn sweep_recycle_bin(bin: &Path, retention_days: u64) -> Vec<PathBuf> {
    if retention_days == 0 {
        return Vec::new();
    }
    let retention = std::time::Duration::from_secs(retention_days * 24 * 60 * 60);
    let Ok(rd) = std::fs::read_dir(bin) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let entries: Vec<(PathBuf, Option<std::time::Duration>)> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| {
            let age = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok());
            (e.path(), age)
        })
        .collect();
    let mut removed = Vec::new();
    for path in expired_in_bin(&entries, retention) {
        if std::fs::remove_file(&path).is_ok() {
            tracing::info!(
                ?path,
                retention_days,
                "recycle-bin entry expired and removed"
            );
            removed.push(path);
        }
    }
    removed
}

/// The `(device, inode)` identities of `files` that exist (SKADI-T-0320).
///
/// Adoption and import both place library files as **hardlinks** to their source
/// (SKADI-T-0424), so a file already in the library and the copy still sitting in
/// a scan directory are the *same inode* under two names. Comparing paths would
/// miss that entirely — the library path is the canonical name, the scan path is
/// whatever the operator called it.
///
/// Missing files are skipped rather than erroring: a library row whose file has
/// gone is a separate problem, and it must not stop a scan.
#[must_use]
pub fn file_identities(files: &[PathBuf]) -> std::collections::HashSet<(u64, u64)> {
    identities_with_concurrency(files, stat_concurrency())
}

/// How many `stat` calls to have in flight at once.
///
/// This is latency-bound, not CPU-bound: each call is a network round trip to
/// the NFS server and the thread spends all of it asleep. So the useful number
/// is far above the core count, and is capped only to avoid burying the server.
fn stat_concurrency() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() * 8).clamp(8, 64))
        .unwrap_or(16)
}

/// The parallel sweep, with the width exposed so a test can pin the behaviour.
///
/// **Serial `metadata()` over a network mount is the whole problem.** Each call
/// costs a round trip — 5–50 ms on the operator's NFS share — and the library
/// import filter stats *every file in the library* to find hardlinks by inode.
/// At 18,600 episodes that is over three minutes, and the TV scan endpoint
/// simply never returned; movies took 91 s and audiobooks 10 s, tracking
/// library size exactly (measured 2026-09-24, SKADI-T-0634).
///
/// A naive benchmark says this work is cheap, and it lies: walking the tree
/// with `find` warms the kernel's attribute cache and batches the syscalls, so
/// it finishes in seconds. Cold, serial, one at a time is a different animal.
///
/// The stats are independent, so they overlap.
fn identities_with_concurrency(
    files: &[PathBuf],
    width: usize,
) -> std::collections::HashSet<(u64, u64)> {
    if files.len() < 2 || width < 2 {
        return files.iter().filter_map(|p| file_identity(p)).collect();
    }
    let width = width.min(files.len());
    let chunk = files.len().div_ceil(width);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|slice| {
                scope.spawn(move || -> Vec<(u64, u64)> {
                    slice.iter().filter_map(|p| file_identity(p)).collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .flatten()
            .collect()
    })
}

/// One file's `(device, inode)`, or `None` when it cannot be stat'd.
///
/// Always `None` on non-unix, where there is no inode identity — callers then see
/// nothing as already-imported, which is the safe direction: showing a file that
/// is already in the library wastes the operator's attention, while *hiding* one
/// that is not would lose it.
#[must_use]
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some((m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// The outcome of adopting an existing library file into its canonical path
/// (SKADI-T-0424).
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Adoption {
    /// `src` already *is* `dest` — the same path, or a hardlink to the same
    /// inode. Nothing to do.
    InPlace,
    /// `dest` exists as a **different** file. Never overwritten: adoption brings
    /// a file the operator already has into the library's naming, so silently
    /// replacing something else that is already there would destroy data the
    /// operator never offered up.
    DestOccupied,
    /// A new hardlink was created at `dest`.
    Linked,
}

/// Adopt `src` into its canonical `dest` by hardlink (SKADI-T-0424).
///
/// This existed three times — movies' `restructure_into`, TV's `place_episode`,
/// audiobooks' `place_one` — each with its own `same_inode`, all byte-identical
/// apart from the enum they returned. Three copies means three places for a
/// collision, atomicity or quality fix to be applied twice and forgotten once;
/// this is the one primitive they now share.
///
/// **Hardlink or nothing.** A cross-device link is an error rather than a copy:
/// copying would silently double disk usage for a library the operator believes
/// is being *organised*, and the fix (scan through the same mount as the root
/// folder) is something only they can do.
///
/// Blocking — call from `spawn_blocking` in async contexts.
pub fn adopt_into(src: &Path, dest: &Path) -> Result<Adoption> {
    if src == dest {
        return Ok(Adoption::InPlace);
    }
    if dest.exists() {
        // Same inode ⇒ already adopted (a re-scan, or a previous run), which is
        // success, not a collision.
        if same_inode(src, dest)? {
            return Ok(Adoption::InPlace);
        }
        return Ok(Adoption::DestOccupied);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::hard_link(src, dest) {
        Ok(()) => Ok(Adoption::Linked),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            Err(AppError::Validation(format!(
                "cross-device: {} cannot hardlink into {} — import refuses to copy; \
                 scan the library through the same mount as the root folder",
                src.display(),
                dest.display()
            )))
        }
        Err(e) => Err(AppError::Io(std::io::Error::new(
            e.kind(),
            format!("hardlinking {} -> {}: {e}", src.display(), dest.display()),
        ))),
    }
}

/// Do two paths refer to the same inode on the same device?
///
/// Non-unix has no inode identity to compare, so it answers `false` — the
/// conservative direction: a caller then treats an occupied destination as a
/// collision rather than assuming it is already the same file.
fn same_inode(a: &Path, b: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ma = std::fs::metadata(a)?;
        let mb = std::fs::metadata(b)?;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        Ok(false)
    }
}

/// The outcome of a single collision-aware placement.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Placement {
    /// The file was placed at a previously-empty destination.
    Placed,
    /// The file replaced an existing destination (overwrite policy, SKADI-T-0218).
    Replaced,
    /// An existing destination was left untouched (skip policy).
    Skipped,
}

/// Place `src` at `dest` honouring `policy` when `dest` already exists
/// (SKADI-T-0217): `Skip` → `Skipped` (no I/O); `Overwrite` → remove then place;
/// `Error` → an error. Runs the existence check + placement together so the decision
/// and the act are one blocking step (no TOCTOU gap across an await).
pub fn place_with_collision(
    src: &Path,
    dest: &Path,
    policy: CollisionPolicy,
    allow_hardlink: bool,
) -> Result<Placement> {
    let exists = dest.exists();
    match collision_decision(exists, policy) {
        CollisionDecision::Skip => Ok(Placement::Skipped),
        CollisionDecision::Error => Err(AppError::Validation(format!(
            "destination already exists: {dest:?}"
        ))),
        CollisionDecision::Place if exists => {
            // Atomic replace (SKADI-T-0220): stage the new file beside `dest` then
            // rename over it. The existing file is only gone once the new one is
            // fully in place — a failure mid-replace can never destroy both.
            let staging = staging_path(dest);
            let _ = std::fs::remove_file(&staging); // clear any stale stage
            place_file(src, &staging, allow_hardlink)?;
            std::fs::rename(&staging, dest)?;
            Ok(Placement::Replaced)
        }
        CollisionDecision::Place => {
            place_file(src, dest, allow_hardlink)?;
            Ok(Placement::Placed)
        }
    }
}

/// The temp path an atomic overwrite stages into, beside `dest`.
fn staging_path(dest: &Path) -> PathBuf {
    dest.with_extension("skadi-incoming")
}

/// Best-effort removal of the staging artifacts a failed placement may leave beside
/// `dest` (SKADI-T-0220). Never touches `dest` itself — an atomic overwrite leaves the
/// original intact on failure, and a fresh place that failed never created `dest`.
fn cleanup_staging(dest: &Path) {
    let _ = std::fs::remove_file(staging_path(dest));
    let _ = std::fs::remove_file(dest.with_extension("skadi-partial"));
}

/// Place `src` at `dest`: create parent dirs, then hardlink; if hardlinking
/// fails (e.g. cross-filesystem `EXDEV`), fall back to a copy. The source is
/// never removed — torrents stay seedable. `allow_hardlink=false` forces the
/// copy path (used by tests to exercise the fallback deterministically).
/// A path that does not exist yet: `dest` if free, else `stem.1.ext`, `stem.2.ext`…
///
/// Used by quarantine (SKADI-T-0413) so a second file with the same name joins the
/// first in the review area instead of replacing it. Gives up after a bounded
/// number of attempts and returns the last candidate, letting `place_file` refuse
/// rather than looping forever on a pathological directory.
fn non_colliding_path(dest: &Path) -> PathBuf {
    if !dest.exists() {
        return dest.to_path_buf();
    }
    let stem = dest
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = dest.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..=999 {
        let name = match &ext {
            Some(e) => format!("{stem}.{n}.{e}"),
            None => format!("{stem}.{n}"),
        };
        let candidate = dest.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dest.to_path_buf()
}

/// Apply a Unix mode to a placed file (SKADI-T-0419).
///
/// A no-op on non-Unix, where there is no equivalent concept and the operator's
/// setting simply does not apply.
fn set_file_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

pub fn place_file(src: &Path, dest: &Path, allow_hardlink: bool) -> Result<()> {
    // Never overwrite (SKADI-T-0413). `hard_link` fails with EEXIST when `dest`
    // is taken, and the copy fallback below ends in a `rename` that silently
    // replaces it — so a hardlink-unsupported filesystem turned "the name is
    // taken" into "the other file is gone". Library placement never relied on
    // that: it resolves collisions first and renames over `dest` itself
    // (`CollisionDecision::Place if exists`), passing a freshly-cleared staging
    // path here. Quarantine had no such policy, which is where this bit.
    if dest.exists() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to overwrite {dest:?}"),
        )));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if allow_hardlink && std::fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    // Copy fallback (cross-device, or hardlink unsupported). Copy to a temp file
    // then rename so a partial copy never appears at `dest`.
    let tmp = dest.with_extension("skadi-partial");
    std::fs::copy(src, &tmp).map_err(|e| {
        AppError::Io(std::io::Error::new(
            e.kind(),
            format!("copying {src:?}: {e}"),
        ))
    })?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Delete `files` from disk and prune now-empty ancestor directories up to (but not including)
/// `root` — the file side of a proper library-item delete (SKADI-T-0316). Best-effort and **safe**:
/// a missing file or a still-non-empty directory is skipped, never an error; `remove_file` only
/// touches the exact paths given and `remove_dir` only removes a directory once it is empty, so
/// this can never cascade outside the files it was handed. Pruning stops at `root` (the domain
/// library root), which is never removed. Returns the files actually removed.
#[must_use]
pub fn delete_files_and_prune(files: &[PathBuf], root: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for f in files {
        if std::fs::remove_file(f).is_ok() {
            removed.push(f.clone());
        }
        // Ancestor dirs strictly under `root` — candidates to prune if they become empty.
        let mut cur = f.parent();
        while let Some(d) = cur {
            if d == root || !d.starts_with(root) {
                break;
            }
            dirs.insert(d.to_path_buf());
            cur = d.parent();
        }
    }
    // Deepest-first so a child dir is removed before its parent.
    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in &dirs {
        let _ = std::fs::remove_dir(d); // succeeds only if the dir is now empty
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_downloaders::DownloadHandle;

    fn unique_dir(tag: &str) -> PathBuf {
        // pid + counter, not pid + timestamp (SKADI-T-0530): two callers in the
        // same nanosecond tick mint the same name, and the collision surfaces as
        // whatever the shared directory then does — never as the name clash it is.
        let p = skadi_core::unique_temp_path(&format!("imp-{tag}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A matcher that sends every file to `<root>/<filename>` and tags it with a
    /// fixed acquirable id (good enough to exercise the mechanical importer).
    struct ToRoot {
        root: PathBuf,
        acquirable: String,
    }
    impl AcquirableMatcher for ToRoot {
        fn match_file(
            &self,
            _parsed: &ParsedRelease,
            source: &Path,
            _completed: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            let name = source.file_name().unwrap().to_str().unwrap();
            vec![AcquirableMatch::new(
                AcquirableRef(self.acquirable.clone()),
                self.root.join(name),
            )]
        }
    }

    /// A matcher that rejects everything.
    struct RejectAll;
    impl AcquirableMatcher for RejectAll {
        fn match_file(
            &self,
            _: &ParsedRelease,
            _: &Path,
            _: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            vec![]
        }
    }

    fn completed(files: Vec<PathBuf>) -> CompletedDownload {
        CompletedDownload {
            handle: DownloadHandle {
                native_id: "h".into(),
                category: "2000".into(),
            },
            files,
            category: "2000".into(),
        }
    }

    /// A matcher that quarantines every audio file into a fixed review dir (SKADI-T-0311).
    struct Quarantiner {
        review: PathBuf,
    }
    impl AcquirableMatcher for Quarantiner {
        fn match_file(
            &self,
            _: &ParsedRelease,
            _: &Path,
            _: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            vec![]
        }
        fn disposition(
            &self,
            _: &ParsedRelease,
            source: &Path,
            _: &CompletedDownload,
        ) -> FileDisposition {
            FileDisposition::Quarantine(self.review.join(source.file_name().unwrap()))
        }
    }

    #[tokio::test]
    async fn quarantine_hardlinks_into_review_and_records_it() {
        let dir = unique_dir("quar");
        let src = dir.join("ambiguous.m4b");
        std::fs::write(&src, b"audio").unwrap();
        let review = dir.join("_review");

        let importer = DefaultImporter::new(Quarantiner {
            review: review.clone(),
        });
        let outcome = importer.import(completed(vec![src.clone()])).await.unwrap();

        let dest = review.join("ambiguous.m4b");
        assert!(
            dest.exists(),
            "quarantined file is placed in the review area"
        );
        assert!(src.exists(), "source stays in place (seedable)");
        assert_eq!(outcome.quarantined, vec![dest]);
        assert!(outcome.imported.is_empty());
        assert!(outcome.rejected.is_empty());
    }

    /// SKADI-T-0424: the shared adoption primitive, which replaced three
    /// byte-identical copies (movies, TV, audiobooks) each with its own
    /// `same_inode`.
    #[test]
    fn adopt_into_links_and_never_overwrites() {
        let dir = unique_dir("adopt");
        let src = dir.join("Movie.mkv");
        std::fs::write(&src, b"video").unwrap();

        // A fresh destination is hardlinked, creating parents on the way.
        let dest = dir.join("Movie (2020)/Movie (2020).mkv");
        assert_eq!(adopt_into(&src, &dest).unwrap(), Adoption::Linked);
        assert!(dest.is_file());

        // Re-running is InPlace, not a collision: the same inode means it is
        // already adopted (a re-scan, or a previous run).
        assert_eq!(adopt_into(&src, &dest).unwrap(), Adoption::InPlace);
        // As is src == dest.
        assert_eq!(adopt_into(&src, &src).unwrap(), Adoption::InPlace);

        // A *different* file already at the destination is reported, never
        // replaced — adoption organises files the operator already has, so
        // overwriting something else there would destroy data they never offered.
        let other = dir.join("other.mkv");
        std::fs::write(&other, b"different").unwrap();
        let occupied = dir.join("Movie (2020)/Movie (2020).mkv");
        assert_eq!(
            adopt_into(&other, &occupied).unwrap(),
            Adoption::DestOccupied
        );
        assert_eq!(std::fs::read(&occupied).unwrap(), b"video");
    }

    #[test]
    fn recycle_retention_keeps_what_it_cannot_age() {
        use std::time::Duration;
        let day = Duration::from_secs(24 * 60 * 60);
        let entries = vec![
            (PathBuf::from("/bin/old.mkv"), Some(day * 8)),
            (PathBuf::from("/bin/fresh.mkv"), Some(day * 2)),
            // Age unreadable: kept. The bin holds the operator's only remaining
            // copy of a superseded file, so a failed stat must never cost them it.
            (PathBuf::from("/bin/unknown.mkv"), None),
            // Exactly at the boundary is kept — expiry is strictly older-than.
            (PathBuf::from("/bin/boundary.mkv"), Some(day * 7)),
        ];
        assert_eq!(
            expired_in_bin(&entries, day * 7),
            vec![PathBuf::from("/bin/old.mkv")]
        );
    }

    #[test]
    fn recycle_sweep_is_off_at_zero_retention() {
        // A default the operator never chose must not delete their files.
        assert!(sweep_recycle_bin(Path::new("/nonexistent"), 0).is_empty());
    }

    #[test]
    fn prune_stops_at_the_boundary() {
        let dir = unique_dir("prune");
        let boundary = dir.join("Movie_(2020)");
        let deep = boundary.join("Theatrical").join("nested");
        std::fs::create_dir_all(&deep).unwrap();
        let removed = prune_empty_dirs(&deep, &boundary);
        assert_eq!(removed.len(), 2, "both empty levels go");
        assert!(boundary.exists(), "the boundary itself is never removed");
    }

    #[test]
    fn prune_stops_at_the_first_non_empty_directory() {
        let dir = unique_dir("prune-stop");
        let boundary = dir.join("Movie_(2020)");
        let keep = boundary.join("Theatrical");
        let deep = keep.join("nested");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(keep.join("poster.jpg"), b"x").unwrap();
        let removed = prune_empty_dirs(&deep, &boundary);
        assert_eq!(removed, vec![deep], "only the empty level goes");
        assert!(
            keep.exists(),
            "a directory that still holds a file survives"
        );
    }

    #[test]
    fn common_ancestor_is_the_deepest_shared_directory() {
        assert_eq!(
            common_ancestor(
                Path::new("/lib/Movie_(2020)/Theatrical/a.mkv"),
                Path::new("/lib/Movie_(2020)/Remastered/b.mkv")
            ),
            PathBuf::from("/lib/Movie_(2020)")
        );
    }

    #[test]
    fn delete_files_prunes_empty_dirs_but_keeps_siblings_and_root() {
        let root = unique_dir("del");
        let book = root.join("author/series/book");
        std::fs::create_dir_all(&book).unwrap();
        let f1 = book.join("part1.m4b");
        let f2 = book.join("part2.m4b");
        std::fs::write(&f1, b"a").unwrap();
        std::fs::write(&f2, b"b").unwrap();
        // A sibling book sharing the series dir — must survive untouched.
        let other = root.join("author/series/other");
        std::fs::create_dir_all(&other).unwrap();
        let keep = other.join("keep.m4b");
        std::fs::write(&keep, b"keep").unwrap();

        let removed = delete_files_and_prune(&[f1.clone(), f2.clone()], &root);

        assert_eq!(removed.len(), 2);
        assert!(!f1.exists() && !f2.exists(), "the two files are deleted");
        assert!(!book.exists(), "the now-empty book dir is pruned");
        assert!(
            other.exists() && keep.exists(),
            "the sibling book is untouched"
        );
        assert!(
            root.join("author/series").exists(),
            "a still-non-empty ancestor is kept"
        );
        assert!(root.exists(), "the library root is never removed");
    }

    #[test]
    fn delete_files_is_safe_on_missing_and_outside_root() {
        let root = unique_dir("del2");
        std::fs::create_dir_all(&root).unwrap();
        // Missing file → no panic, nothing reported removed, root intact.
        let removed = delete_files_and_prune(&[root.join("nope.m4b")], &root);
        assert!(removed.is_empty());
        assert!(root.exists());

        // A file outside `root` is removed if listed, but no dir outside root is pruned.
        let outside = unique_dir("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let of = outside.join("x.m4b");
        std::fs::write(&of, b"x").unwrap();
        let removed = delete_files_and_prune(std::slice::from_ref(&of), &root);
        assert_eq!(removed, vec![of]);
        assert!(outside.exists(), "a dir outside root is never pruned");
    }

    #[tokio::test]
    async fn imports_and_hardlinks() {
        let src_dir = unique_dir("src");
        let lib_dir = unique_dir("lib");
        let src = src_dir.join("Movie.2020.1080p.BluRay.x264-G.mkv");
        std::fs::write(&src, b"video").unwrap();

        let importer = DefaultImporter::new(ToRoot {
            root: lib_dir.clone(),
            acquirable: "ed1".into(),
        });
        let outcome = importer.import(completed(vec![src.clone()])).await.unwrap();

        assert_eq!(outcome.imported.len(), 1);
        assert!(outcome.rejected.is_empty());
        let dest = &outcome.imported[0].file.path;
        assert!(dest.exists());
        assert_eq!(std::fs::read(dest).unwrap(), b"video");
        // Source is preserved (seedable).
        assert!(src.exists());
        assert_eq!(outcome.imported[0].acquirable, AcquirableRef("ed1".into()));

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&lib_dir);
    }

    #[tokio::test]
    async fn multi_file_multi_acquirable() {
        let src_dir = unique_dir("msrc");
        let lib_dir = unique_dir("mlib");
        let a = src_dir.join("S01E01.mkv");
        let b = src_dir.join("S01E02.mkv");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let importer = DefaultImporter::new(ToRoot {
            root: lib_dir.clone(),
            acquirable: "ep".into(),
        });
        let outcome = importer.import(completed(vec![a, b])).await.unwrap();
        assert_eq!(outcome.imported.len(), 2);

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&lib_dir);
    }

    #[tokio::test]
    async fn unmatched_files_are_rejected() {
        let src_dir = unique_dir("rsrc");
        let src = src_dir.join("sample.mkv");
        std::fs::write(&src, b"x").unwrap();

        let importer = DefaultImporter::new(RejectAll);
        let outcome = importer.import(completed(vec![src.clone()])).await.unwrap();
        assert!(outcome.imported.is_empty());
        assert_eq!(outcome.rejected.len(), 1);
        assert_eq!(outcome.rejected[0].0, src);

        let _ = std::fs::remove_dir_all(&src_dir);
    }

    #[test]
    fn collision_decision_covers_all_cases() {
        // No existing file → always place, whatever the policy.
        for p in [
            CollisionPolicy::Skip,
            CollisionPolicy::Overwrite,
            CollisionPolicy::Error,
        ] {
            assert_eq!(collision_decision(false, p), CollisionDecision::Place);
        }
        // Existing file → policy decides.
        assert_eq!(
            collision_decision(true, CollisionPolicy::Skip),
            CollisionDecision::Skip
        );
        assert_eq!(
            collision_decision(true, CollisionPolicy::Overwrite),
            CollisionDecision::Place
        );
        assert_eq!(
            collision_decision(true, CollisionPolicy::Error),
            CollisionDecision::Error
        );
        // The safe default is Skip.
        assert_eq!(CollisionPolicy::default(), CollisionPolicy::Skip);
    }

    /// A matcher that sends every file to a fixed dest with a chosen collision policy.
    struct ToDest {
        dest: PathBuf,
        on_collision: CollisionPolicy,
    }
    impl AcquirableMatcher for ToDest {
        fn match_file(
            &self,
            _: &ParsedRelease,
            _: &Path,
            _: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            vec![
                AcquirableMatch::new(AcquirableRef("ed".into()), self.dest.clone())
                    .with_collision(self.on_collision),
            ]
        }
    }

    #[tokio::test]
    async fn existing_destination_skips_by_default_and_overwrites_on_policy() {
        let dir = unique_dir("coll");
        let src = dir.join("New.mkv");
        let dest = dir.join("lib/Movie.mkv");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&src, b"new").unwrap();
        std::fs::write(&dest, b"old").unwrap();

        // Skip (default): the existing file is preserved and the match is reported
        // as already present for its acquirable — not rejected (SKADI-T-0385).
        let skip = DefaultImporter::new(ToDest {
            dest: dest.clone(),
            on_collision: CollisionPolicy::Skip,
        });
        let outcome = skip.import(completed(vec![src.clone()])).await.unwrap();
        assert!(outcome.imported.is_empty());
        assert!(outcome.rejected.is_empty());
        assert_eq!(outcome.already_present.len(), 1);
        assert_eq!(
            outcome.already_present[0].acquirable,
            AcquirableRef("ed".into())
        );
        assert_eq!(outcome.already_present[0].file.path, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), b"old", "untouched");

        // Overwrite: the destination is replaced with the new file.
        let over = DefaultImporter::new(ToDest {
            dest: dest.clone(),
            on_collision: CollisionPolicy::Overwrite,
        });
        let outcome = over.import(completed(vec![src.clone()])).await.unwrap();
        assert_eq!(outcome.imported.len(), 1);
        assert!(outcome.rejected.is_empty());
        assert_eq!(std::fs::read(&dest).unwrap(), b"new", "replaced");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A matcher that routes each source to `<root>/<filename>` but sends any file
    /// whose name contains "bad" to an uncreatable path (its parent is a file), so
    /// that file's placement fails while others succeed.
    struct PartlyBad {
        root: PathBuf,
        blocker: PathBuf,
    }
    impl AcquirableMatcher for PartlyBad {
        fn match_file(
            &self,
            _: &ParsedRelease,
            source: &Path,
            _: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            let name = source.file_name().unwrap().to_str().unwrap();
            let dest = if name.contains("bad") {
                // `blocker` is a regular file, so create_dir_all(blocker) fails.
                self.blocker.join("nope.mkv")
            } else {
                self.root.join(name)
            };
            vec![AcquirableMatch::new(AcquirableRef("ed".into()), dest)]
        }
    }

    #[tokio::test]
    async fn placement_failure_is_isolated_not_silently_successful() {
        let dir = unique_dir("partfail");
        let good = dir.join("good.mkv");
        let bad = dir.join("bad.mkv");
        std::fs::write(&good, b"g").unwrap();
        std::fs::write(&bad, b"b").unwrap();
        let blocker = dir.join("blocker"); // a FILE used as a (bad) parent dir
        std::fs::write(&blocker, b"x").unwrap();
        let lib = dir.join("lib");

        let importer = DefaultImporter::new(PartlyBad {
            root: lib.clone(),
            blocker,
        });
        let outcome = importer
            .import(completed(vec![good.clone(), bad.clone()]))
            .await
            .unwrap();

        // The good file imported; the bad one is reported in `failed`, not silent.
        assert_eq!(outcome.imported.len(), 1);
        assert_eq!(outcome.failed.len(), 1, "the failure is surfaced");
        assert!(lib.join("good.mkv").exists());
        // Both sources preserved.
        assert!(good.exists() && bad.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failed_overwrite_preserves_the_existing_file() {
        let dir = unique_dir("atomic");
        // The "source" is a directory → hardlink/copy both fail, so the staged
        // overwrite errors *before* the rename and the original survives.
        let src_dir = dir.join("a-directory-source");
        std::fs::create_dir_all(&src_dir).unwrap();
        let dest = dir.join("lib/Movie.mkv");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"original").unwrap();

        let importer = DefaultImporter::new(ToDest {
            dest: dest.clone(),
            on_collision: CollisionPolicy::Overwrite,
        });
        let outcome = importer.import(completed(vec![src_dir])).await.unwrap();

        assert!(outcome.imported.is_empty());
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"original",
            "existing file intact after a failed replace"
        );
        // No staging artifacts left behind.
        assert!(!dest.with_extension("skadi-incoming").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn preview_plans_without_touching_any_files() {
        let dir = unique_dir("preview");
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let fresh = dir.join("Fresh.mkv");
        let sample = dir.join("Sample-Movie.mkv");
        std::fs::write(&fresh, b"f").unwrap();
        std::fs::write(&sample, b"s").unwrap();

        let importer = DefaultImporter::new(ToRoot {
            root: lib.clone(),
            acquirable: "ed".into(),
        });
        let plan = importer
            .preview(completed(vec![fresh.clone(), sample.clone()]))
            .await
            .unwrap();

        // Fresh file → planned Place; sample → rejected; nothing else.
        assert_eq!(plan.would_import.len(), 1);
        assert_eq!(plan.would_import[0].action, PlannedAction::Place);
        assert_eq!(plan.would_import[0].dest, lib.join("Fresh.mkv"));
        assert_eq!(plan.would_reject.len(), 1);
        assert!(plan.would_reject[0].1.contains("sample"));
        // Crucially: the library is still empty — preview placed nothing.
        assert!(
            !lib.join("Fresh.mkv").exists(),
            "preview is side-effect free"
        );
        assert!(std::fs::read_dir(&lib).unwrap().next().is_none());

        // With the destination already present, the plan flips to Replace/reject by
        // policy — still without touching disk.
        std::fs::write(lib.join("Fresh.mkv"), b"old").unwrap();
        let plan = importer
            .preview(completed(vec![fresh.clone()]))
            .await
            .unwrap();
        // ToRoot uses the default Skip policy → the existing file is left alone and
        // reported as already-present, the same way `import` reports it
        // (SKADI-T-0428). Not a rejection: the library is already correct.
        assert!(plan.would_import.is_empty());
        assert!(plan.would_reject.is_empty(), "{:?}", plan.would_reject);
        assert_eq!(plan.already_present.len(), 1);
        assert_eq!(std::fs::read(lib.join("Fresh.mkv")).unwrap(), b"old");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_files_collects_recursively_and_sorts() {
        let dir = unique_dir("scan");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.mkv"), b"b").unwrap();
        std::fs::write(dir.join("a.mkv"), b"a").unwrap();
        std::fs::write(dir.join("sub/c.mkv"), b"c").unwrap();

        let files = scan_files(&dir).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.mkv", "b.mkv", "c.mkv"], "recursive + sorted");

        // A single file scans to itself.
        let one = scan_files(&dir.join("a.mkv")).unwrap();
        assert_eq!(one, vec![dir.join("a.mkv")]);
        // A missing path errors.
        assert!(scan_files(&dir.join("nope")).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn looks_like_sample_matches_names_and_dirs_not_real_files() {
        let sample = |p: &str| looks_like_sample(Path::new(p));
        assert!(sample("/dl/Movie/Sample.mkv"));
        assert!(sample("/dl/Movie/sample-movie.mkv"));
        assert!(sample("/dl/Movie/movie-sample.mkv"));
        assert!(sample("/dl/Movie/Sample/main.mkv")); // Sample/ dir
        assert!(!sample("/dl/Movie/The.Matrix.1999.1080p.mkv"));
        // "sample" as a substring of a real word must not trip it.
        assert!(!sample("/dl/Resampled.Audio.flac"));
    }

    #[test]
    fn is_video_file_accepts_containers_only() {
        assert!(is_video_file(Path::new("/dl/Show.S01E01.1080p.mkv")));
        assert!(is_video_file(Path::new("/dl/Show.S01E01.MP4")));
        assert!(!is_video_file(Path::new("/dl/s13e01 screencap.png")));
        assert!(!is_video_file(Path::new("/dl/Show.S01E01.srt")));
        assert!(!is_video_file(Path::new("/dl/Show.S01E01.mka")));
        assert!(!is_video_file(Path::new("/dl/Show.S01E01.nfo")));
        assert!(!is_video_file(Path::new("/dl/noext")));
    }

    #[test]
    fn download_root_name_is_the_deepest_shared_folder() {
        let mut c = CompletedDownload {
            handle: DownloadHandle {
                native_id: "h".into(),
                category: "tv".into(),
            },
            files: vec![
                PathBuf::from("/dl/complete/Show S13 Pack/Show s13e01.mkv"),
                PathBuf::from("/dl/complete/Show S13 Pack/screencaps/s13e01.png"),
            ],
            category: "tv".into(),
        };
        assert_eq!(download_root_name(&c).as_deref(), Some("Show S13 Pack"));
        c.files = vec![PathBuf::from("/dl/complete/Show.S01E01.mkv")];
        assert_eq!(download_root_name(&c).as_deref(), Some("complete"));
        c.files.clear();
        assert_eq!(download_root_name(&c), None);
    }

    #[tokio::test]
    async fn shared_floor_rejects_samples_before_matching() {
        let dir = unique_dir("floor");
        let lib = dir.join("lib");
        let big = dir.join("Movie.1080p.mkv");
        let sample = dir.join("Sample-Movie.mkv");
        let tiny = dir.join("Tiny.mkv");
        std::fs::write(&big, vec![0u8; 2048]).unwrap();
        std::fs::write(&sample, b"x").unwrap();
        std::fs::write(&tiny, vec![0u8; 10]).unwrap();

        // Size floor of 1 KiB: the sample is rejected by name, the tiny file by size,
        // the big file imports.
        let importer = DefaultImporter::new(ToRoot {
            root: lib.clone(),
            acquirable: "ed".into(),
        })
        .with_min_file_bytes(1024);
        let outcome = importer
            .import(completed(vec![big.clone(), sample.clone(), tiny.clone()]))
            .await
            .unwrap();

        assert_eq!(outcome.imported.len(), 1, "only the real file imports");
        assert_eq!(outcome.rejected.len(), 2);
        let reasons: String = outcome.rejected.iter().map(|(_, r)| r.clone()).collect();
        assert!(reasons.contains("sample"));
        assert!(reasons.contains("size floor"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn space_check_covers_fit_reserve_and_unknown() {
        // Plenty of room.
        assert_eq!(space_check(100, Some(1000), 0), SpaceVerdict::Fits);
        // Exactly fits at the boundary.
        assert_eq!(space_check(1000, Some(1000), 0), SpaceVerdict::Fits);
        // Reserve eats into the usable space.
        assert_eq!(
            space_check(600, Some(1000), 500),
            SpaceVerdict::Insufficient {
                need: 600,
                available: 500
            }
        );
        // Unknown free space → never blocks.
        assert_eq!(space_check(u64::MAX, None, 0), SpaceVerdict::Fits);
    }

    #[tokio::test]
    async fn import_rejects_when_reserve_cannot_be_met() {
        let src_dir = unique_dir("space-src");
        let lib_dir = unique_dir("space-lib");
        let src = src_dir.join("Movie.mkv");
        std::fs::write(&src, b"video-bytes").unwrap();

        // A reserve larger than any real disk forces every placement to be rejected.
        let importer = DefaultImporter::new(ToRoot {
            root: lib_dir.clone(),
            acquirable: "ed1".into(),
        })
        .with_min_free_bytes(u64::MAX);
        let outcome = importer.import(completed(vec![src.clone()])).await.unwrap();

        assert!(outcome.imported.is_empty());
        assert_eq!(outcome.rejected.len(), 1);
        assert!(outcome.rejected[0].1.contains("insufficient free space"));
        assert!(src.exists(), "source untouched on rejection");

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&lib_dir);
    }

    #[test]
    fn supersedes_to_remove_drops_placed_dest_and_dedups() {
        let dest = PathBuf::from("/lib/Movie.1080p.mkv");
        let old = PathBuf::from("/lib/Movie.720p.mkv");
        let got = supersedes_to_remove(&[old.clone(), dest.clone(), old.clone()], &dest);
        assert_eq!(got, vec![old], "drop the just-placed dest + de-dup");
        // Nothing to remove when supersedes is empty or only the dest.
        assert!(supersedes_to_remove(std::slice::from_ref(&dest), &dest).is_empty());
        assert!(supersedes_to_remove(&[], &dest).is_empty());
    }

    /// A matcher that places to `dest` and supersedes `old` (an upgrade).
    struct Upgrade {
        dest: PathBuf,
        old: PathBuf,
    }
    impl AcquirableMatcher for Upgrade {
        fn match_file(
            &self,
            _: &ParsedRelease,
            _: &Path,
            _: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            vec![
                AcquirableMatch::new(AcquirableRef("ed".into()), self.dest.clone())
                    .superseding(vec![self.old.clone()]),
            ]
        }
    }

    #[tokio::test]
    async fn upgrade_places_new_and_removes_superseded_old_file() {
        let dir = unique_dir("upg");
        let src = dir.join("Movie.1080p.mkv");
        let dest = dir.join("lib/Movie_(2020)/Movie_1080p.mkv");
        let old = dir.join("lib/Movie_(2020)/Movie_720p.mkv");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&src, b"hd").unwrap();
        std::fs::write(&old, b"sd").unwrap();

        let importer = DefaultImporter::new(Upgrade {
            dest: dest.clone(),
            old: old.clone(),
        });
        let outcome = importer.import(completed(vec![src.clone()])).await.unwrap();

        assert_eq!(outcome.imported.len(), 1);
        assert!(dest.exists(), "new file placed");
        assert!(!old.exists(), "superseded old file removed");
        assert_eq!(outcome.replaced, vec![old], "replacement reported");
        assert!(src.exists(), "source preserved (seedable)");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn place_file_copy_fallback_keeps_source() {
        let dir = unique_dir("copy");
        let src = dir.join("a.bin");
        let dest = dir.join("nested/b.bin");
        std::fs::write(&src, b"data").unwrap();

        // Force the copy path.
        place_file(&src, &dest, false).unwrap();
        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"data");
        assert!(src.exists(), "copy fallback preserves the source");
        // No partial file left behind.
        assert!(!dest.with_extension("skadi-partial").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod identity_sweep_tests {
    use super::*;

    /// Build `n` real files and return their directory and paths.
    ///
    /// Uses the crate's own `unique_temp_path` convention rather than pulling
    /// in `tempfile` as a dev-dependency for five tests.
    fn scratch(n: usize) -> (PathBuf, Vec<PathBuf>) {
        let dir = skadi_core::unique_temp_path("ident-sweep");
        std::fs::create_dir_all(&dir).unwrap();
        let paths = (0..n)
            .map(|i| {
                let p = dir.join(format!("f{i}.mkv"));
                std::fs::write(&p, b"x").unwrap();
                p
            })
            .collect();
        (dir, paths)
    }

    #[test]
    fn the_parallel_sweep_finds_exactly_what_the_serial_one_does() {
        // The only thing that must not change. Concurrency here is an
        // optimisation, and an optimisation that alters the answer is a bug.
        let (_d, paths) = scratch(50);
        let serial: std::collections::HashSet<_> =
            paths.iter().filter_map(|p| file_identity(p)).collect();
        for width in [1, 2, 7, 64, 500] {
            assert_eq!(
                identities_with_concurrency(&paths, width),
                serial,
                "width {width} disagreed with the serial sweep"
            );
        }
    }

    #[test]
    fn a_hardlink_is_the_same_identity_as_its_original() {
        // The property the filter exists for: import hardlinks into the
        // library, so the held copy and the scanned one are one inode under
        // two names and a path comparison would miss every one.
        let dir = skadi_core::unique_temp_path("ident-link");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("original.mkv");
        let b = dir.join("linked.mkv");
        std::fs::write(&a, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert_eq!(file_identity(&a), file_identity(&b));
        assert_eq!(identities_with_concurrency(&[a, b], 8).len(), 1);
    }

    #[test]
    fn paths_that_cannot_be_stat_are_skipped_not_fatal() {
        // A library row can outlive its file. Losing the whole sweep over one
        // missing path would hide every genuinely-imported file.
        let (_d, mut paths) = scratch(4);
        paths.push(PathBuf::from("/definitely/not/here.mkv"));
        assert_eq!(identities_with_concurrency(&paths, 8).len(), 4);
    }

    #[test]
    fn an_empty_or_single_input_is_handled_without_spawning() {
        assert!(identities_with_concurrency(&[], 16).is_empty());
        let (_d, one) = scratch(1);
        assert_eq!(identities_with_concurrency(&one, 16).len(), 1);
    }

    #[test]
    fn the_width_is_bounded_on_both_ends() {
        // Unbounded would bury the NFS server; one would be the bug being fixed.
        let w = stat_concurrency();
        assert!((8..=64).contains(&w), "width {w} out of range");
    }
}
