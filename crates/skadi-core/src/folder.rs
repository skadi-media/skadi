//! Root folders — filesystem locations that hold a media library.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::id::RootFolderId;
use crate::status::MediaKind;

/// A filesystem path that holds a media library, plus its identifier.
///
/// A [`LibraryItem`](crate::item::LibraryItem) is stored under exactly one root
/// folder; the importer hardlinks/moves completed downloads into it.
///
/// Since SKADI-T-0302 a root is **derived, not operator-chosen**: skadi owns one
/// `library.root` and each domain's root is `<library.root>/<kind subfolder>`
/// (see [`for_domain`](Self::for_domain)). The struct is kept so the wide body of
/// matcher/importer/naming code that reads `item.root_folder.path` is unchanged.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RootFolder {
    pub id: RootFolderId,
    pub path: PathBuf,
}

impl RootFolder {
    /// Create a root folder with a freshly generated id.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            id: RootFolderId::new(),
            path: path.into(),
        }
    }

    /// The derived root for a domain under the single `library.root` (SKADI-T-0302):
    /// `<library_root>/<kind.library_subfolder()>` (e.g. `/data/movie`). This is the
    /// only way a root is chosen now — there is no operator root picker.
    #[must_use]
    pub fn for_domain(library_root: impl AsRef<Path>, kind: MediaKind) -> Self {
        Self::new(library_root.as_ref().join(kind.library_subfolder()))
    }
}

/// Filesystem status of a root-folder path (SKADI-T-0230): does it exist, is it a
/// directory, and is it writable? The judgement ([`is_usable`](Self::is_usable) /
/// [`problem`](Self::problem)) is pure and unit-testable; [`probe_root_status`] does the
/// I/O to populate it.
#[derive(
    Clone, Copy, Eq, PartialEq, Debug, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct RootFolderStatus {
    /// The path exists (as anything).
    pub exists: bool,
    /// The path is a directory.
    pub is_dir: bool,
    /// A probe file could be created in it (rw mount + permissions).
    pub writable: bool,
}

impl RootFolderStatus {
    /// A root is usable for a library only if it's an existing, writable directory.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.exists && self.is_dir && self.writable
    }

    /// The first reason this root is unusable, or `None` if it's fine. Pure — the same
    /// message the API returns when rejecting a bad root.
    #[must_use]
    pub fn problem(&self) -> Option<&'static str> {
        match (self.exists, self.is_dir, self.writable) {
            (false, _, _) => Some("path does not exist"),
            (true, false, _) => Some("path is not a directory"),
            (true, true, false) => Some("path is not writable"),
            (true, true, true) => None,
        }
    }
}

/// Probe a path's [`RootFolderStatus`] (filesystem I/O). Writability is verified by
/// creating then removing a uniquely-named probe file — catching a read-only mount or a
/// permissions problem a metadata check alone would miss. Blocking; call off the async
/// runtime for a slow/stuck mount.
#[must_use]
pub fn probe_root_status(path: &Path) -> RootFolderStatus {
    let exists = path.exists();
    let is_dir = path.is_dir();
    let writable = is_dir && probe_writable(path);
    RootFolderStatus {
        exists,
        is_dir,
        writable,
    }
}

fn probe_writable(dir: &Path) -> bool {
    // A per-process counter keeps concurrent probes from colliding on one name.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let probe = dir.join(format!(
        ".skadi-write-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_problem_reports_the_first_failing_check() {
        let usable = RootFolderStatus {
            exists: true,
            is_dir: true,
            writable: true,
        };
        assert!(usable.is_usable());
        assert_eq!(usable.problem(), None);

        let missing = RootFolderStatus::default();
        assert!(!missing.is_usable());
        assert_eq!(missing.problem(), Some("path does not exist"));

        let a_file = RootFolderStatus {
            exists: true,
            is_dir: false,
            writable: false,
        };
        assert_eq!(a_file.problem(), Some("path is not a directory"));

        let read_only = RootFolderStatus {
            exists: true,
            is_dir: true,
            writable: false,
        };
        assert_eq!(read_only.problem(), Some("path is not writable"));
    }

    #[test]
    fn probe_root_status_distinguishes_dir_file_and_missing() {
        let dir = crate::unique_temp_path("root");
        std::fs::create_dir_all(&dir).unwrap();

        // A writable directory is usable; no probe file is left behind.
        let s = probe_root_status(&dir);
        assert!(s.is_usable(), "writable dir usable: {s:?}");
        let leftover = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftover, 0, "probe file cleaned up");

        // A regular file: exists but not a directory.
        let file = dir.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let s = probe_root_status(&file);
        assert!(s.exists && !s.is_dir);
        assert_eq!(s.problem(), Some("path is not a directory"));

        // A missing path.
        let s = probe_root_status(&dir.join("nope"));
        assert_eq!(s.problem(), Some("path does not exist"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
