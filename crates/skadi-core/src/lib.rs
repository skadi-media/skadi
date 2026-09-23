//! `skadi-core` — the shared domain vocabulary every other Skadi crate speaks.
//!
//! This crate is intentionally free of storage, HTTP, and other infrastructure
//! dependencies: it defines identifiers, domain traits, and core value types
//! only. Dependency direction across the workspace is strictly downward onto
//! this crate.

pub mod rating;
pub mod errors;
pub mod external;
pub mod folder;
pub mod id;
pub mod item;
pub mod media;
pub mod module;
pub mod nfo;
pub mod protocol;
pub mod status;
pub mod streaming;

pub use errors::{AppError, Result};
pub use external::{AsinId, ExternalIds, GoodreadsId, ImdbId, MusicBrainzId, TmdbId, TvdbId};
pub use folder::{RootFolder, RootFolderStatus, probe_root_status};
pub use id::{
    AuthorId, BookFileId, BookId, BookSeriesId, CustomFormatId, DownloaderId, EditionKindId,
    EpisodeId, IndexerId, ItemId, MovieEditionId, MovieId, NotifierId, ProfileId, QualityId,
    ReleaseId, RootFolderId, SeasonId, SeriesId,
};
pub use item::{Acquirable, LibraryItem};
pub use media::{AudioInfo, MediaInfo, VideoInfo};
pub use module::{BoxFuture, BoxedWorker, DomainModule, Worker};
pub use protocol::Protocol;
pub use status::{AcquisitionStatus, FailureReason, FileRef, MediaKind, MediaType};
pub use streaming::{Remedy, Severity, StreamingIssue, assess, is_broken};

#[cfg(test)]
mod tests;

/// A collision-free temporary path under the system temp dir (SKADI-T-0530).
///
/// Names were built from `pid + timestamp`, which is not unique: two tests
/// starting in the same process within the same nanosecond tick mint the same
/// name, and then one migrates the other's SQLite file — surfacing as a
/// mystifying "database is locked" rather than as the name clash it is.
///
/// `pid` still separates concurrent *processes* (`cargo test` runs one binary per
/// crate); the counter separates callers within one. A timestamp adds nothing
/// once the counter is there, so it is gone — a name that is unique by
/// construction beats one that is unique in practice.
#[must_use]
pub fn unique_temp_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "skadi-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}
