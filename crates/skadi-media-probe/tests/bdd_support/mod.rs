//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
//!
//! Scenarios probe either a committed fixture (`tests/fixtures/*`) or a file the
//! scenario synthesises into a private `tempfile` directory (corrupt/empty/WAV).
pub mod steps;

use std::path::PathBuf;

use skadi_media_probe::MediaInfo;
use skadi_media_probe::chapters::ChapterMark;

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// Scratch directory for synthesised files (dropped with the world).
    pub tmp: Option<tempfile::TempDir>,
    /// The file under test.
    pub path: Option<PathBuf>,
    /// `Some(None)` = probed to nothing; `Some(Some(_))` = probed.
    pub info: Option<Option<MediaInfo>>,
    pub chapters: Option<Result<Vec<ChapterMark>, String>>,
}

impl World {
    pub fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    pub fn scratch(&mut self) -> PathBuf {
        if self.tmp.is_none() {
            self.tmp = Some(tempfile::tempdir().expect("tempdir"));
        }
        self.tmp.as_ref().expect("tempdir").path().to_path_buf()
    }

    pub fn path(&self) -> &std::path::Path {
        self.path.as_deref().expect("a file under test")
    }

    pub fn info(&self) -> &MediaInfo {
        self.info
            .as_ref()
            .expect("the file was probed")
            .as_ref()
            .expect("the file probed to something")
    }
}
