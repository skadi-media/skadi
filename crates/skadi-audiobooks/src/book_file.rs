//! [`BookFile`] — the [`Acquirable`] unit of the audiobooks domain
//! (SKADI-I-0017). One audiobook per book; the hunter acquires a `BookFile`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{Acquirable, AcquisitionStatus, BookFileId, BookId, FileRef, QualityId};
use skadi_importer::AcquirableRef;

/// The audiobook file for a [`Book`](crate::book::Book) — the unit the hunter
/// acquires. One per book (abridged/unabridged is a quality-axis concern, not a
/// separate acquirable).
/// The edition every pre-existing file is, and what a plain "add this book"
/// creates (SKADI-T-0448). Matches the `is_default` row the migration seeds.
pub const DEFAULT_EDITION_KIND: &str = "unabridged";

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BookFile {
    pub id: BookFileId,
    pub book_id: BookId,
    /// Which edition of the work this is (SKADI-T-0448): `unabridged`,
    /// `full_cast`, `dramatized`, `booktrack`, `abridged`. One book holds one
    /// row per kind, the way a movie holds one `movie_edition` per kind.
    ///
    /// A slug rather than an id newtype: the registry is keyed by slug, which is
    /// stable, readable in a database, and what every caller already has.
    #[serde(default = "default_edition_kind")]
    pub kind: String,
    /// Whether this edition is wanted (SKADI-T-0562).
    ///
    /// Distinct from `Book.monitored`, which says whether the *work* is wanted
    /// at all. A book can be monitored while only one of its editions is —
    /// that is the point of modelling editions.
    ///
    /// A newly **discovered** edition arrives `false`: visible and selectable,
    /// but not acquired until an operator opts in. Arriving `true` would
    /// reintroduce exactly what SKADI-T-0400 stopped — fetching another edition
    /// of a book already on disk.
    #[serde(default = "yes")]
    pub monitored: bool,
    pub status: AcquisitionStatus,
    /// Set once the audiobook has been imported.
    pub file: Option<FileRef>,
    /// Quality classification of the imported file (an audiobook
    /// `QualityId`; set with `Imported`).
    pub quality: Option<QualityId>,
    /// Aggregate custom-format score recorded at import time.
    pub format_score: i32,
    pub updated_at: DateTime<Utc>,
    /// Probed media-info of the imported file (SKADI-T-0236); `None` until the
    /// post-import probe step runs.
    pub media_info: Option<skadi_core::MediaInfo>,
}

/// Infer an edition kind from an Audible-style product title (SKADI-T-0562).
///
/// Audible marks non-standard editions in the title — "(Dramatized Adaptation)",
/// "Full Cast", "Booktrack Edition" — because they are separate products with
/// their own ASINs. That is the only signal available: the catalog record itself
/// carries no edition field.
///
/// Anything unrecognised is [`DEFAULT_EDITION_KIND`], and that fallback is
/// load-bearing rather than lazy. A discovered ASIN whose title says nothing
/// special is almost always the *same* edition re-listed, and returning
/// `unabridged` makes it collide with the edition already on the book — which is
/// the correct outcome, since `UNIQUE(book_id, kind_slug)` then rejects it as the
/// duplicate it is. Inventing a distinct kind for it would manufacture a second
/// edition out of a re-listing.
#[must_use]
pub fn edition_kind_from_title(title: &str) -> &'static str {
    let t = title.to_ascii_lowercase();
    if t.contains("dramatiz") || t.contains("dramatis") {
        "dramatized"
    } else if t.contains("full cast") || t.contains("full-cast") {
        "full_cast"
    } else if t.contains("booktrack") {
        "booktrack"
    } else if t.contains("abridged") && !t.contains("unabridged") {
        "abridged"
    } else {
        DEFAULT_EDITION_KIND
    }
}

fn yes() -> bool {
    true
}

fn default_edition_kind() -> String {
    DEFAULT_EDITION_KIND.to_string()
}

impl BookFile {
    /// A fresh `Missing` audiobook file for `book_id`.
    #[must_use]
    pub fn missing(book_id: BookId) -> Self {
        Self::missing_of_kind(book_id, DEFAULT_EDITION_KIND)
    }

    /// A fresh `Missing` edition of a specific kind (SKADI-T-0448).
    #[must_use]
    pub fn missing_of_kind(book_id: BookId, kind: &str) -> Self {
        Self {
            id: BookFileId::new(),
            book_id,
            kind: kind.to_string(),
            // The default constructor is the *add a book* path, where the
            // operator asked for this edition. Discovery uses
            // `discovered_of_kind`, which does not.
            monitored: true,
            status: AcquisitionStatus::Missing,
            file: None,
            quality: None,
            format_score: 0,
            updated_at: Utc::now(),
            media_info: None,
        }
    }

    /// A newly **discovered** edition: present and selectable, but not acquired
    /// until an operator monitors it (SKADI-T-0562).
    #[must_use]
    pub fn discovered_of_kind(book_id: BookId, kind: &str) -> Self {
        Self {
            monitored: false,
            ..Self::missing_of_kind(book_id, kind)
        }
    }

    /// The opaque [`AcquirableRef`] the hunter carries through a workflow run.
    /// `AudiobookStatusSink` (SKADI-T-0129) decodes this back to a
    /// [`BookFileId`]; the encoding is the id's canonical UUID string (mirrors
    /// `MovieEdition::acquirable_ref`).
    #[must_use]
    pub fn acquirable_ref(&self) -> AcquirableRef {
        AcquirableRef(self.id.to_string())
    }
}

impl Acquirable for BookFile {
    type Item = crate::book::Book;
    type Id = BookFileId;

    fn id(&self) -> &Self::Id {
        &self.id
    }

    fn parent(&self) -> &BookId {
        &self.book_id
    }

    fn status(&self) -> &AcquisitionStatus {
        &self.status
    }

    /// Per-file wantedness: `true` while we still need a (first) release. The
    /// cross-axis combination with `Book.monitored` and the upgrade-below-cutoff
    /// check live in the wanted-query (SKADI-T-0128).
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

    fn sample() -> BookFile {
        BookFile::missing(BookId::new())
    }

    #[test]
    fn book_file_serde_round_trips() {
        let f = sample();
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(f, serde_json::from_str::<BookFile>(&json).unwrap());
    }

    #[test]
    fn missing_is_wanted_imported_is_not() {
        let mut f = sample();
        assert!(f.wanted(), "Missing => wanted");
        f.status = AcquisitionStatus::Cutoff;
        assert!(!f.wanted(), "Cutoff => not wanted");
        f.status = AcquisitionStatus::Failed {
            reason: skadi_core::FailureReason::NoSuitableRelease,
            retry_at: None,
            attempts: 0,
        };
        assert!(f.wanted(), "Failed => wanted (retry candidate)");
    }

    #[test]
    fn acquirable_ref_is_the_id_string() {
        let f = sample();
        assert_eq!(f.acquirable_ref().0, f.id.to_string());
    }
}

#[cfg(test)]
mod edition_kind_tests {
    use super::*;

    #[test]
    fn known_markers_map_to_their_kind() {
        for (title, want) in [
            ("The Sandman: Act I (Dramatized Adaptation)", "dramatized"),
            ("Project Hail Mary - Full Cast Production", "full_cast"),
            ("Some Book (Booktrack Edition)", "booktrack"),
            ("Some Book (Abridged)", "abridged"),
        ] {
            assert_eq!(edition_kind_from_title(title), want, "{title}");
        }
    }

    #[test]
    fn an_ordinary_title_infers_the_default_kind() {
        // Load-bearing: a discovered ASIN whose title says nothing special is
        // almost always the same edition re-listed. Inferring `unabridged` makes
        // it collide with the edition the book already has, so the unique key
        // rejects it as the duplicate it is. Inventing a distinct kind would
        // manufacture a second edition out of a re-listing.
        assert_eq!(
            edition_kind_from_title("Project Hail Mary"),
            DEFAULT_EDITION_KIND
        );
        assert_eq!(
            edition_kind_from_title("The Way of Kings: Book One"),
            DEFAULT_EDITION_KIND
        );
    }

    #[test]
    fn unabridged_is_not_read_as_abridged() {
        // "Unabridged" contains "abridged" — the obvious substring bug, and it
        // would mislabel the most common edition of all.
        assert_eq!(
            edition_kind_from_title("Project Hail Mary (Unabridged)"),
            DEFAULT_EDITION_KIND
        );
    }

    #[test]
    fn a_discovered_edition_is_not_monitored() {
        // The operator's decision (2026-09-09): discovered editions are visible
        // but not acquired. The plain constructor is the *add a book* path, where
        // the operator asked for it.
        let book = BookId::new();
        assert!(!BookFile::discovered_of_kind(book, "full_cast").monitored);
        assert!(BookFile::missing_of_kind(book, "full_cast").monitored);
        assert!(BookFile::missing(book).monitored);
    }
}
