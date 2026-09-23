//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use std::collections::HashMap;

use skadi_audiobooks::{AudiobookMatcher, AudiobooksRepo, Book, BookFile};
use skadi_core::{AsinId, BookId, ExternalIds, ProfileId, QualityId, RootFolder};
use skadi_hunter::AcquireSeed;
use skadi_importer::{AcquirableMatch, FileDisposition};
use skadi_quality::QualityProfile;
use skadi_quality::audiobook::default_audiobook_definitions;
use skadi_store::Store;
use skadi_testsupport::TestDb;

pub struct Db(pub TestDb);
impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Db({})", self.0.backend_name())
    }
}

pub struct Matcher(pub AudiobookMatcher);
impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AudiobookMatcher")
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub db: Option<Db>,
    pub profile: Option<QualityProfile>,
    /// Books by ASIN.
    pub books: HashMap<String, Book>,
    /// The (single) file per book, by ASIN.
    pub files: HashMap<String, BookFile>,
    pub seeds: Vec<AcquireSeed>,
    pub recovered: usize,
    pub tmp: Option<tempfile::TempDir>,
    pub paths: HashMap<String, std::path::PathBuf>,
    pub matcher: Option<Matcher>,
    pub matches: Vec<AcquirableMatch>,
    pub disposition: Option<FileDisposition>,
    pub download_files: Vec<std::path::PathBuf>,
    pub error: Option<String>,
    pub last_book: Option<Book>,
    pub listed: Vec<Book>,
    pub authors: HashMap<String, skadi_audiobooks::Author>,
    pub history: Vec<skadi_store::HistoryEntry>,
    pub read_status: Option<Option<skadi_core::AcquisitionStatus>>,
    pub provider: Option<steps::metadata::FakeProvider>,
    pub watchers: Vec<skadi_audiobooks::Watcher>,
    pub works: Vec<skadi_audiobooks::Work>,
}

impl World {
    pub fn store(&self) -> Store {
        self.db
            .as_ref()
            .expect("library not set up")
            .0
            .store
            .clone()
    }

    pub fn repo(&self) -> std::sync::Arc<dyn AudiobooksRepo> {
        std::sync::Arc::new(self.store())
    }

    pub fn profile(&self) -> QualityProfile {
        self.profile.clone().expect("profile not set up")
    }

    pub fn tmp(&mut self) -> std::path::PathBuf {
        if self.tmp.is_none() {
            self.tmp = Some(tempfile::tempdir().expect("tempdir"));
        }
        self.tmp.as_ref().unwrap().path().to_path_buf()
    }
    #[allow(dead_code)] // kept for future scenarios (P2 review helper)
    pub fn quality_named(name: &str) -> QualityId {
        if name == "Unknown" {
            return skadi_quality::audiobook::unknown_audiobook_id();
        }
        default_audiobook_definitions()
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("no audiobook quality named {name:?}"))
            .id
    }

    #[allow(dead_code)] // review helper kept for future scenarios (SKADI-I-0057 P2)
    pub fn quality_name(id: QualityId) -> String {
        default_audiobook_definitions()
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn new_book(title: &str, author: &str, asin: &str, monitored: bool) -> Book {
        let mut b = Book::new(
            ExternalIds {
                asin: Some(AsinId(asin.into())),
                ..Default::default()
            },
            title,
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        b.authors = vec![author.into()];
        b.year = Some(2010);
        b.monitored = monitored;
        b
    }

    /// Save a book plus one file (status as given) and remember both by ASIN.
    pub async fn save_book_with_file(
        &mut self,
        title: &str,
        author: &str,
        asin: &str,
        monitored: bool,
        status: skadi_core::AcquisitionStatus,
    ) -> (Book, BookFile) {
        let book = Self::new_book(title, author, asin, monitored);
        let store = self.store();
        store.upsert_book(&book).await.expect("upsert book");
        let mut f = BookFile::missing(book.id);
        if let skadi_core::AcquisitionStatus::Imported {
            file,
            quality,
            score,
            ..
        } = &status
        {
            f.file = Some(file.clone());
            f.quality = Some(*quality);
            f.format_score = *score;
        }
        f.status = status;
        store.upsert_book_file(&f).await.expect("upsert file");
        self.books.insert(asin.to_string(), book.clone());
        self.files.insert(asin.to_string(), f.clone());
        (book, f)
    }

    pub fn book_id(&self, asin: &str) -> BookId {
        self.books
            .get(asin)
            .unwrap_or_else(|| panic!("unknown book {asin:?}"))
            .id
    }

    pub async fn reload_file(&self, asin: &str) -> BookFile {
        let f = self
            .files
            .get(asin)
            .unwrap_or_else(|| panic!("no file for {asin:?}"));
        self.store()
            .get_book_file(f.id)
            .await
            .expect("get file")
            .expect("file row present")
    }

    pub fn seed_for(&self, asin: &str) -> &AcquireSeed {
        let f = self
            .files
            .get(asin)
            .unwrap_or_else(|| panic!("no file {asin:?}"));
        self.seeds
            .iter()
            .find(|s| s.acquirable == f.acquirable_ref())
            .unwrap_or_else(|| panic!("no seed for {asin:?}; seeds: {:?}", self.seeds))
    }
}
