//! Movie editions (the [`Acquirable`] unit) and the runtime-configurable
//! [`EditionKind`] registry.
//!
//! Per SKADI-I-0007, `EditionKind` is a **DB-backed registry** (an
//! `edition_kinds` table seeded with built-ins and extendable by the user)
//! rather than a hard enum. This module defines the value types only; the
//! schema + repo land in SKADI-T-0044, and the registry is consulted by the
//! matcher in SKADI-T-0046.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{
    Acquirable, AcquisitionStatus, EditionKindId, FileRef, MovieEditionId, MovieId, QualityId,
};
use skadi_importer::AcquirableRef;

/// One row in the `edition_kinds` registry.
///
/// Built-in kinds (Theatrical, Extended, Director's Cut, Ultimate Cut, IMAX,
/// Remastered) are seeded by `skadi-movies`'s migrations; users add their own
/// through the API. `match_patterns` is consulted by [`crate::movie::Movie`]'s
/// matcher to map a parsed release title to an [`EditionKindId`].
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct EditionKind {
    pub id: EditionKindId,
    /// Display name (e.g. `"Director's Cut"`).
    pub name: String,
    /// Filesystem-safe tag used in import paths (e.g. `"Directors Cut"`).
    pub normalized_tag: String,
    /// Case-insensitive substrings (or regex strings) matched against the
    /// source's parsed `edition` field. First match wins.
    pub match_patterns: Vec<String>,
    /// `true` for the built-in seed rows; the repo refuses to delete these so
    /// the matcher always has a Theatrical fallback.
    pub builtin: bool,
}

/// A specific cut of a [`crate::movie::Movie`] — the unit the hunter actually
/// acquires. `(movie_id, kind)` is unique per movie.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MovieEdition {
    pub id: MovieEditionId,
    pub movie_id: MovieId,
    pub kind: EditionKindId,
    pub status: AcquisitionStatus,
    /// Set once the edition has been imported.
    pub file: Option<FileRef>,
    /// Quality classification of the imported file (set with `Imported`).
    pub quality: Option<QualityId>,
    /// Aggregate custom-format score recorded at import time.
    pub format_score: i32,
    pub updated_at: DateTime<Utc>,
    /// Probed media-info of the imported file (SKADI-T-0236); `None` until the
    /// post-import probe step runs.
    pub media_info: Option<skadi_core::MediaInfo>,
}

impl MovieEdition {
    /// A fresh `Missing` edition for `movie_id` of `kind`.
    #[must_use]
    pub fn missing(movie_id: MovieId, kind: EditionKindId) -> Self {
        Self {
            id: MovieEditionId::new(),
            movie_id,
            kind,
            status: AcquisitionStatus::Missing,
            file: None,
            quality: None,
            format_score: 0,
            updated_at: Utc::now(),
            media_info: None,
        }
    }

    /// The opaque [`AcquirableRef`] the hunter carries through a workflow run.
    /// `MovieStatusSink` (SKADI-T-0048) decodes this back to a
    /// [`MovieEditionId`] to persist status writes; the encoding is the
    /// edition id's canonical UUID string.
    #[must_use]
    pub fn acquirable_ref(&self) -> AcquirableRef {
        AcquirableRef(self.id.to_string())
    }
}

impl Acquirable for MovieEdition {
    type Item = crate::movie::Movie;
    type Id = MovieEditionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }

    fn parent(&self) -> &MovieId {
        &self.movie_id
    }

    fn status(&self) -> &AcquisitionStatus {
        &self.status
    }

    /// Per-edition wantedness: `true` while we still need a (first) release.
    /// The cross-axis combination with `Movie.monitored` and the
    /// upgrade-below-cutoff check live in `MovieWantedQuery` (SKADI-T-0047).
    fn wanted(&self) -> bool {
        matches!(
            self.status,
            AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_kind() -> EditionKind {
        EditionKind {
            id: EditionKindId::new(),
            name: "Director's Cut".into(),
            normalized_tag: "Directors Cut".into(),
            match_patterns: vec!["director's cut".into(), "directors.cut".into()],
            builtin: true,
        }
    }

    fn sample_edition() -> MovieEdition {
        MovieEdition::missing(MovieId::new(), EditionKindId::new())
    }

    #[test]
    fn edition_kind_serde_round_trips() {
        let k = sample_kind();
        let json = serde_json::to_string(&k).unwrap();
        let back: EditionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(k, back);
    }

    #[test]
    fn movie_edition_serde_round_trips() {
        let e = sample_edition();
        let json = serde_json::to_string(&e).unwrap();
        let back: MovieEdition = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn missing_edition_is_wanted_imported_is_not() {
        let mut e = sample_edition();
        assert!(e.wanted(), "Missing => wanted");
        e.status = AcquisitionStatus::Cutoff;
        assert!(!e.wanted(), "Cutoff => not wanted");
        e.status = AcquisitionStatus::Failed {
            reason: skadi_core::FailureReason::NoSuitableRelease,
            retry_at: None,
            attempts: 0,
        };
        assert!(e.wanted(), "Failed => wanted (retry candidate)");
    }

    #[test]
    fn acquirable_ref_is_the_edition_id_string() {
        let e = sample_edition();
        // The encoding is exactly the edition id's canonical string form —
        // the inverse decode lives in MovieStatusSink (T-0048).
        assert_eq!(e.acquirable_ref().0, e.id.to_string());
    }
}
