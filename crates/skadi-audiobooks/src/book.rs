//! [`Book`] — the [`LibraryItem`] of the audiobooks domain (SKADI-I-0017).
//!
//! A `Book` carries the user-controlled library status (`monitored`) plus the
//! Audnexus-sourced descriptive fields, and owns an in-memory
//! `files: Vec<BookFile>` so [`LibraryItem::acquirables`] can yield them (one
//! audiobook per book). The DB stores books + book_files in separate tables
//! (SKADI-T-0125); the repo populates `files` on load.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{AuthorId, BookId, ExternalIds, LibraryItem, MediaKind, ProfileId, RootFolder};

use crate::author::SeriesLink;
use crate::book_file::BookFile;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Book {
    pub id: BookId,
    /// External ids; for audiobooks the key is `external_ids.asin`.
    pub external_ids: ExternalIds,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Primary author link (the organizational owner). `None` until resolved.
    #[serde(default)]
    pub author_id: Option<AuthorId>,
    /// Author display names (denormalized from metadata; primary first).
    #[serde(default)]
    pub authors: Vec<String>,
    /// Narrator display names.
    #[serde(default)]
    pub narrators: Vec<String>,
    /// Series membership (name + position), when part of one.
    #[serde(default)]
    pub series: Option<SeriesLink>,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub runtime_minutes: Option<u32>,
    /// Absolute cover image URL from metadata, if any.
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub release_date: Option<NaiveDate>,
    /// Library-axis status: do we want this audiobook in the library?
    pub monitored: bool,
    pub profile: ProfileId,
    pub root_folder: RootFolder,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub last_metadata_refresh: Option<DateTime<Utc>>,
    /// The acquirable audiobook file(s) for this book (typically one). Populated
    /// by the repo on load; empty on a freshly-constructed `Book`.
    #[serde(default)]
    pub files: Vec<BookFile>,
}

impl Book {
    /// A fresh, monitored `Book` with no files, to be enriched by metadata sync
    /// and persisted by the repo.
    #[must_use]
    pub fn new(
        external_ids: ExternalIds,
        title: impl Into<String>,
        profile: ProfileId,
        root_folder: RootFolder,
    ) -> Self {
        Self {
            id: BookId::new(),
            external_ids,
            title: title.into(),
            subtitle: None,
            author_id: None,
            authors: Vec::new(),
            narrators: Vec::new(),
            series: None,
            year: None,
            overview: None,
            runtime_minutes: None,
            cover_url: None,
            release_date: None,
            monitored: true,
            profile,
            root_folder,
            added_at: Utc::now(),
            last_metadata_refresh: None,
            files: Vec::new(),
        }
    }
}

impl LibraryItem for Book {
    type Id = BookId;
    type Acquirable = BookFile;

    fn id(&self) -> &Self::Id {
        &self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Audiobook
    }

    fn monitored(&self) -> bool {
        self.monitored
    }

    fn quality_profile(&self) -> ProfileId {
        self.profile
    }

    fn root_folder(&self) -> &RootFolder {
        &self.root_folder
    }

    fn external_ids(&self) -> &ExternalIds {
        &self.external_ids
    }

    fn acquirables(&self) -> Box<dyn Iterator<Item = Self::Acquirable> + '_> {
        Box::new(self.files.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::{Acquirable, AcquisitionStatus, AsinId};

    fn sample_book() -> Book {
        Book::new(
            ExternalIds {
                asin: Some(AsinId("B08G9PRS1K".into())),
                ..Default::default()
            },
            "Project Hail Mary",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        )
    }

    #[test]
    fn book_serde_round_trips_with_files() {
        let mut b = sample_book();
        b.year = Some(2021);
        b.authors = vec!["Andy Weir".into()];
        b.files.push(BookFile::missing(b.id));
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(b, serde_json::from_str::<Book>(&json).unwrap());
    }

    #[test]
    fn library_item_impl_exposes_files() {
        let mut b = sample_book();
        b.files.push(BookFile::missing(b.id));
        b.files.push(BookFile {
            status: AcquisitionStatus::Cutoff,
            ..BookFile::missing(b.id)
        });

        assert_eq!(b.title(), "Project Hail Mary");
        assert_eq!(b.kind(), MediaKind::Audiobook);
        assert!(b.monitored());
        assert_eq!(
            b.external_ids().asin.as_ref().map(|a| a.0.as_str()),
            Some("B08G9PRS1K")
        );

        let acquirables: Vec<_> = b.acquirables().collect();
        assert_eq!(acquirables.len(), 2);
        assert!(acquirables[0].wanted());
        assert!(!acquirables[1].wanted(), "Cutoff file is not wanted");
        assert_eq!(acquirables[0].parent(), b.id());
    }
}
