//! Media kinds and the acquisition state machine.
//!
//! [`AcquisitionStatus`] tracks where an [`Acquirable`](crate) is in its journey
//! from wanted to imported. Per the foundation design decision (2026-05-26), the
//! `Imported` variant stores a [`QualityId`] plus a numeric score rather than a
//! rich `Quality` value — the `Quality` type lives in `skadi-quality`, which
//! depends on `skadi-core`, so referencing it here would create a dependency
//! cycle. Higher layers resolve the `QualityId` into a full `Quality`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{DownloaderId, QualityId, ReleaseId};

/// The kind of media a domain manages.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum MediaKind {
    Movie,
    Series,
    Music,
    /// E-books (reserved for a future ebook domain — not yet implemented).
    Book,
    /// Audiobooks (SKADI-I-0017): narrated books, distinct from `Book` (e-books)
    /// in file shape, indexer category, and quality model.
    Audiobook,
    Subtitle,
}

/// The broad media **type** a [`MediaKind`] belongs to — the axis that owns its
/// quality profiles + library subfolder (SKADI-I-0045). Movies + TV are `Video` and
/// share the video quality profiles; audiobooks (+ music) are `Audio`; e-books/comics
/// are `Print`. A domain only ever sees profiles of its own type.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum MediaType {
    Video,
    Audio,
    Print,
}

impl MediaType {
    /// Lowercase wire/key string (`"video"`, `"audio"`, `"print"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::Video => "video",
            MediaType::Audio => "audio",
            MediaType::Print => "print",
        }
    }
}

impl MediaKind {
    /// The media type this kind belongs to (SKADI-I-0045): the grouping that owns
    /// the quality-profile axis and the library subfolder.
    #[must_use]
    pub fn media_type(self) -> MediaType {
        match self {
            MediaKind::Movie | MediaKind::Series | MediaKind::Subtitle => MediaType::Video,
            MediaKind::Music | MediaKind::Audiobook => MediaType::Audio,
            MediaKind::Book => MediaType::Print,
        }
    }

    /// This kind's subfolder under the single `library.root` (SKADI-T-0302). skadi
    /// owns the layout: each domain writes to `<library.root>/<subfolder>/...`. The
    /// names are the opinionated targets the operator approved (movie / television /
    /// audiobook); music/book reserve their own slot for future domains.
    #[must_use]
    pub fn library_subfolder(self) -> &'static str {
        match self {
            MediaKind::Movie => "movie",
            MediaKind::Series => "television",
            MediaKind::Audiobook => "audiobook",
            MediaKind::Music => "music",
            MediaKind::Book => "book",
            MediaKind::Subtitle => "subtitle",
        }
    }
}

/// A reference to a file that has been imported into a root folder.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileRef {
    /// Path to the imported file, relative to its root folder.
    pub path: PathBuf,
}

/// Why an acquisition failed.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum FailureReason {
    /// No release satisfied the request within the configured policy.
    NoSuitableRelease,
    /// The downloader rejected or failed the transfer.
    DownloadFailed(String),
    /// Import (parse/match/rename/move) failed.
    ImportFailed(String),
    /// A catch-all for reasons not yet modeled explicitly.
    Other(String),
}

impl FailureReason {
    /// A stable, machine-filterable reason code (SKADI-T-0200) — the structured
    /// companion to the free-text message, so History can group/filter failures
    /// without parsing prose. Stable across releases (don't rename existing codes).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            FailureReason::NoSuitableRelease => "no_suitable_release",
            FailureReason::DownloadFailed(_) => "download_failed",
            FailureReason::ImportFailed(_) => "import_failed",
            FailureReason::Other(_) => "other",
        }
    }
}

/// Where an acquirable is in the wanted → imported lifecycle.
///
/// Note: this derives `PartialEq` but not `Eq`/`Hash` because the
/// `Downloading` variant carries an `f32` progress value.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum AcquisitionStatus {
    /// Wanted but nothing has been done yet.
    Missing,
    /// Actively searching indexers for a satisfying release.
    Searching { since: DateTime<Utc>, attempts: u32 },
    /// A release was sent to a downloader.
    Snatched {
        release: ReleaseId,
        downloader: DownloaderId,
        at: DateTime<Utc>,
    },
    /// The downloader is transferring the release.
    Downloading { release: ReleaseId, progress: f32 },
    /// Imported into the library.
    ///
    /// Stores `quality` as a [`QualityId`] (see module docs) plus the custom
    /// format `score` at import time.
    Imported {
        file: FileRef,
        quality: QualityId,
        score: i32,
        at: DateTime<Utc>,
    },
    /// Imported and at/above the profile cutoff — will not upgrade.
    Cutoff,
    /// The acquisition failed; may be retried after `retry_at`.
    Failed {
        reason: FailureReason,
        retry_at: Option<DateTime<Utc>>,
        /// Consecutive **not-found** attempts (search-returned-nothing /
        /// no-suitable-release), driving the escalating re-check backoff
        /// (SKADI-T-0177). `0` for transfer failures, which use a fixed backoff.
        /// `#[serde(default)]` so pre-existing persisted statuses load as `0`.
        #[serde(default)]
        attempts: u32,
    },
}

/// Coarse discriminant of an [`AcquisitionStatus`], used for transition rules
/// without having to match on each variant's data.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum StatusKind {
    Missing,
    Searching,
    Snatched,
    Downloading,
    Imported,
    Cutoff,
    Failed,
}

impl AcquisitionStatus {
    fn kind(&self) -> StatusKind {
        match self {
            Self::Missing => StatusKind::Missing,
            Self::Searching { .. } => StatusKind::Searching,
            Self::Snatched { .. } => StatusKind::Snatched,
            Self::Downloading { .. } => StatusKind::Downloading,
            Self::Imported { .. } => StatusKind::Imported,
            Self::Cutoff => StatusKind::Cutoff,
            Self::Failed { .. } => StatusKind::Failed,
        }
    }

    /// Whether a transition from the current status to `next` is legal.
    ///
    /// The hunter pipeline drives statuses forward; upgrades loop an imported
    /// item back through searching. Self-transitions (e.g. retrying a search,
    /// updating download progress) are permitted.
    #[must_use]
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use StatusKind::*;
        let from = self.kind();
        let to = next.kind();
        if from == to {
            // Allow refreshing in place (new attempt, progress update, re-import).
            return true;
        }
        matches!(
            (from, to),
            (Missing, Searching)
                | (Searching, Snatched)
                | (Searching, Failed)
                | (Searching, Missing)
                | (Snatched, Downloading)
                | (Snatched, Failed)
                | (Downloading, Imported)
                | (Downloading, Failed)
                | (Imported, Cutoff)
                | (Imported, Searching) // upgrade pass
                | (Cutoff, Searching)   // profile changed; re-evaluate
                | (Failed, Searching)   // retry
                | (Failed, Missing) // reset
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn searching() -> AcquisitionStatus {
        AcquisitionStatus::Searching {
            since: Utc::now(),
            attempts: 1,
        }
    }

    fn imported() -> AcquisitionStatus {
        AcquisitionStatus::Imported {
            file: FileRef {
                path: PathBuf::from("Movie (1999)/movie.mkv"),
            },
            quality: QualityId::new(),
            score: 100,
            at: Utc::now(),
        }
    }

    #[test]
    fn legal_forward_transitions() {
        assert!(AcquisitionStatus::Missing.can_transition_to(&searching()));
        let snatched = AcquisitionStatus::Snatched {
            release: ReleaseId::new(),
            downloader: DownloaderId::new(),
            at: Utc::now(),
        };
        assert!(searching().can_transition_to(&snatched));
        let downloading = AcquisitionStatus::Downloading {
            release: ReleaseId::new(),
            progress: 0.5,
        };
        assert!(snatched.can_transition_to(&downloading));
        assert!(downloading.can_transition_to(&imported()));
        assert!(imported().can_transition_to(&AcquisitionStatus::Cutoff));
    }

    #[test]
    fn upgrade_loops_imported_back_to_searching() {
        assert!(imported().can_transition_to(&searching()));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(!AcquisitionStatus::Missing.can_transition_to(&imported()));
        assert!(!AcquisitionStatus::Cutoff.can_transition_to(&imported()));
        let downloading = AcquisitionStatus::Downloading {
            release: ReleaseId::new(),
            progress: 0.1,
        };
        assert!(!AcquisitionStatus::Missing.can_transition_to(&downloading));
    }

    #[test]
    fn self_transition_allowed_for_progress() {
        let a = AcquisitionStatus::Downloading {
            release: ReleaseId::new(),
            progress: 0.1,
        };
        let b = AcquisitionStatus::Downloading {
            release: ReleaseId::new(),
            progress: 0.9,
        };
        assert!(a.can_transition_to(&b));
    }

    #[test]
    fn status_round_trips_through_json() {
        let status = imported();
        let json = serde_json::to_string(&status).unwrap();
        let back: AcquisitionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
    }

    #[test]
    fn media_kind_round_trips() {
        let json = serde_json::to_string(&MediaKind::Movie).unwrap();
        assert_eq!(json, r#""Movie""#);
        let back: MediaKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MediaKind::Movie);
    }

    #[test]
    fn media_kind_maps_to_its_type() {
        use MediaKind::*;
        assert_eq!(Movie.media_type(), MediaType::Video);
        assert_eq!(Series.media_type(), MediaType::Video);
        assert_eq!(Audiobook.media_type(), MediaType::Audio);
        assert_eq!(Music.media_type(), MediaType::Audio);
        assert_eq!(Book.media_type(), MediaType::Print);
        assert_eq!(MediaType::Audio.as_str(), "audio");
    }
}
