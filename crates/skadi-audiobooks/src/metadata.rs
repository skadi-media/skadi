//! Audiobook metadata sync over a [`MetadataProvider`] (Audnexus) — SKADI-T-0126.
//!
//! Mirrors `skadi_movies::metadata`:
//! - [`refresh_book`] — (re)populate a [`Book`]'s Audnexus-sourced fields from a
//!   `lookup`, resolving its series into a `book_series` row. Preserves
//!   user-controlled fields (`id`/`monitored`/`profile`/`root_folder`/`added_at`/
//!   `author_id`) when `existing` is given.
//! - [`add_book`] — "fetch + create book row + create default Missing
//!   `BookFile`" for a new ASIN.
//! - [`add_author`] — create a monitored [`Author`] from an Audnexus author ASIN.

use chrono::{Datelike, Utc};

use skadi_core::{AppError, AsinId, ProfileId, Result, RootFolder};
use skadi_metadata::{AudnexusProvider, ExternalId, MetadataProvider, MetadataRecord};

use crate::author::{Author, Series, SeriesLink};
use crate::book::Book;
use crate::book_file::BookFile;
use crate::repo::AudiobooksRepo;

/// Defaults used when constructing a fresh [`Book`] (see [`refresh_book`]).
#[derive(Clone, Debug)]
pub struct BookDefaults {
    pub profile: ProfileId,
    pub root_folder: RootFolder,
}

/// Apply a metadata record onto a book's Audnexus-sourced fields. Leaves
/// `series`/`author_id`/`files` for the caller (series needs a repo lookup).
fn apply_record(book: &mut Book, record: &MetadataRecord, asin: &AsinId) {
    book.external_ids.asin = Some(asin.clone());
    book.title = record.title.clone();
    book.subtitle = record.subtitle.clone();
    book.authors = record.authors.clone();
    book.narrators = record.narrators.clone();
    book.overview = record.overview.clone();
    book.runtime_minutes = record.runtime_minutes;
    book.release_date = record.release_date;
    book.year = record
        .release_date
        .map(|d| d.year())
        .and_then(|y| u16::try_from(y).ok());
    if let Some(img) = record.images.first() {
        book.cover_url = Some(img.path.clone());
    }
    book.last_metadata_refresh = Some(Utc::now());
}

/// Fetch the Audnexus record for `asin` and shape it into a [`Book`], resolving
/// the series into a `book_series` row (deduped by name). With `existing`,
/// preserves its user-controlled fields; with `None`, builds a fresh
/// `Book::new(...)` from `defaults`.
pub async fn refresh_book(
    repo: &dyn AudiobooksRepo,
    provider: &dyn MetadataProvider,
    asin: AsinId,
    existing: Option<Book>,
    defaults: Option<BookDefaults>,
) -> Result<Book> {
    let record = provider.lookup(&ExternalId::Asin(asin.clone())).await?;

    let mut book = match existing {
        Some(b) => b,
        None => {
            let d = defaults.ok_or_else(|| {
                AppError::Validation("refresh_book: defaults required when existing is None".into())
            })?;
            Book::new(
                skadi_core::ExternalIds {
                    asin: Some(asin.clone()),
                    ..Default::default()
                },
                String::new(),
                d.profile,
                d.root_folder,
            )
        }
    };

    apply_record(&mut book, &record, &asin);

    // Resolve the series into a `book_series` row (dedupe by unique name).
    if let Some(series_name) = record.series.as_deref() {
        let series = match repo.get_series_by_name(series_name).await? {
            Some(s) => s,
            None => {
                let s = Series::new(series_name);
                repo.upsert_series(&s).await?;
                s
            }
        };
        book.series = Some(SeriesLink {
            series_id: series.id,
            name: series.name,
            position: record.series_position.clone(),
        });
    } else {
        book.series = None;
    }

    Ok(book)
}

/// Add a brand-new audiobook by Audible ASIN: refuses duplicates, runs metadata
/// sync (incl. series resolution), writes the `Book` row, and creates one
/// default `Missing` [`BookFile`]. Returns the persisted `Book` (with `files`).
pub async fn add_book(
    repo: &dyn AudiobooksRepo,
    provider: &dyn MetadataProvider,
    asin: AsinId,
    profile: ProfileId,
    root_folder: RootFolder,
) -> Result<Book> {
    if let Some(existing) = repo.get_book_by_asin(&asin).await? {
        return Err(AppError::Validation(format!(
            "book with ASIN {} already exists ({})",
            asin.0, existing.title
        )));
    }
    let mut book = refresh_book(
        repo,
        provider,
        asin,
        None,
        Some(BookDefaults {
            profile,
            root_folder,
        }),
    )
    .await?;
    repo.upsert_book(&book).await?;

    let file = BookFile::missing(book.id);
    repo.upsert_book_file(&file).await?;
    book.files.push(file);

    Ok(book)
}

/// Add (or return the existing) monitored [`Author`] for an Audnexus author
/// ASIN. Idempotent: if an author with this ASIN already exists, returns it
/// unchanged. Uses the Audnexus author lookup (`lookup_author`), which is an
/// inherent [`AudnexusProvider`] method (not on the generic provider trait).
pub async fn add_author(
    repo: &dyn AudiobooksRepo,
    provider: &AudnexusProvider,
    asin: AsinId,
) -> Result<Author> {
    if let Some(existing) = repo.get_author_by_asin(&asin).await? {
        return Ok(existing);
    }
    let resolved = provider.lookup_author(&asin).await?;
    let mut author = Author::new(resolved.name);
    author.asin = Some(resolved.asin);
    author.description = resolved.description;
    author.image_url = resolved.image;
    // Know-only by default (SKADI-I-0018): adding an author records them + ingests
    // their catalog, but acquires nothing until the operator applies a watcher.
    author.monitored = false;
    repo.upsert_author(&author).await?;
    Ok(author)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::NaiveDate;
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;

    use skadi_core::MediaKind;
    use skadi_metadata::{MetadataMatch, MetadataQuery};
    use skadi_store::Store;

    async fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skadi.db");
        let url = format!("sqlite://{}", path.display());
        let store = Store::connect(&url).unwrap();
        store.run_migrations().await.unwrap();
        drop(store);
        {
            let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
            conn.run_pending_migrations(crate::SQLITE_MIGRATIONS)
                .unwrap();
        }
        (dir, Store::connect(&url).unwrap())
    }

    /// A scripted provider whose `lookup` returns a canned audiobook record.
    struct FakeProvider {
        record: MetadataRecord,
    }
    #[async_trait]
    impl MetadataProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn supports(&self, kind: MediaKind) -> bool {
            kind == MediaKind::Audiobook
        }
        async fn search(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
            Ok(vec![])
        }
        async fn lookup(&self, _id: &ExternalId) -> Result<MetadataRecord> {
            Ok(self.record.clone())
        }
    }

    fn phm_record() -> MetadataRecord {
        MetadataRecord {
            title: "The Way of Kings".into(),
            subtitle: Some("Book One".into()),
            authors: vec!["Brandon Sanderson".into()],
            narrators: vec!["Michael Kramer".into(), "Kate Reading".into()],
            series: Some("Stormlight Archive".into()),
            series_position: Some("1".into()),
            runtime_minutes: Some(2734),
            release_date: NaiveDate::from_ymd_opt(2010, 8, 31),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn add_book_creates_book_series_and_default_file() {
        let (_d, store) = fresh_store().await;
        let provider = FakeProvider {
            record: phm_record(),
        };

        let book = add_book(
            &store,
            &provider,
            AsinId("B003ITRL7G".into()),
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        )
        .await
        .expect("add_book ok");

        assert_eq!(book.title, "The Way of Kings");
        assert_eq!(book.year, Some(2010));
        assert_eq!(book.narrators.len(), 2);
        let link = book.series.as_ref().expect("series resolved");
        assert_eq!(link.name, "Stormlight Archive");
        assert_eq!(link.position.as_deref(), Some("1"));
        assert_eq!(book.files.len(), 1, "default Missing file");

        // Persisted: fetch via the repo returns the same data with the file +
        // the series row was created.
        let from_db = store
            .get_book_by_asin(&AsinId("B003ITRL7G".into()))
            .await
            .unwrap()
            .expect("book persisted");
        assert_eq!(from_db.files.len(), 1);
        assert!(
            store
                .get_series_by_name("Stormlight Archive")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn add_book_rejects_duplicate_asin() {
        let (_d, store) = fresh_store().await;
        let provider = FakeProvider {
            record: phm_record(),
        };
        let asin = AsinId("B003ITRL7G".into());
        add_book(
            &store,
            &provider,
            asin.clone(),
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        )
        .await
        .unwrap();

        let err = add_book(
            &store,
            &provider,
            asin,
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[tokio::test]
    async fn add_author_is_idempotent_on_existing_asin() {
        let (_d, store) = fresh_store().await;
        // Pre-seed an author with ASIN "A1".
        let mut seeded = Author::new("Brandon Sanderson");
        seeded.asin = Some(AsinId("A1".into()));
        store.upsert_author(&seeded).await.unwrap();

        // add_author short-circuits on the existing ASIN — the provider (pointed
        // at the default endpoint) is never called, so no network happens.
        let provider = AudnexusProvider::new(
            skadi_http::HttpClient::new(std::time::Duration::from_secs(5)).unwrap(),
        );
        let got = add_author(&store, &provider, AsinId("A1".into()))
            .await
            .unwrap();
        assert_eq!(
            got.id, seeded.id,
            "returned the existing author, no re-create"
        );
    }

    #[tokio::test]
    async fn refresh_book_preserves_user_fields_and_reuses_series() {
        let (_d, store) = fresh_store().await;
        let provider = FakeProvider {
            record: phm_record(),
        };
        let book = add_book(
            &store,
            &provider,
            AsinId("B003ITRL7G".into()),
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        )
        .await
        .unwrap();
        let original_profile = book.profile;
        let series_id = book.series.as_ref().unwrap().series_id;

        // Refresh with the existing book → same series row reused (not duplicated),
        // user fields preserved.
        let refreshed = refresh_book(
            &store,
            &provider,
            AsinId("B003ITRL7G".into()),
            Some(book.clone()),
            None,
        )
        .await
        .unwrap();
        assert_eq!(refreshed.profile, original_profile, "profile preserved");
        assert_eq!(refreshed.id, book.id, "id preserved");
        assert_eq!(
            refreshed.series.as_ref().unwrap().series_id,
            series_id,
            "series row reused by name, not duplicated"
        );
        assert!(refreshed.last_metadata_refresh.is_some());
    }
}
