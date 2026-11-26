//! `AudiobookStatusSink` — `StatusSink` for the audiobooks domain (SKADI-T-0129).
//!
//! Decodes the `AcquirableRef` carried by the hunter back to a [`BookFileId`]
//! and persists the [`AcquisitionStatus`] via [`AudiobooksRepo`], plus appends
//! to the acquisition history. Mirrors `skadi_movies::MovieStatusSink`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use skadi_core::{AcquisitionStatus, AppError, BookFileId, Result};
use skadi_hunter::StatusSink;
use skadi_importer::AcquirableRef;
use skadi_quality::audiobook::default_audiobook_definitions;
use skadi_store::{HistoryEntry, HistoryRepo, LibraryFileRepo};

use crate::repo::AudiobooksRepo;

pub struct AudiobookStatusSink {
    repo: Arc<dyn AudiobooksRepo>,
    history: Arc<dyn HistoryRepo>,
    /// Where an imported file's storage facts go (SKADI-T-0773). `None` = not
    /// recorded here; the storage sweep finds the file later.
    storage: Option<Arc<dyn LibraryFileRepo>>,
}

impl AudiobookStatusSink {
    #[must_use]
    pub fn new(repo: Arc<dyn AudiobooksRepo>, history: Arc<dyn HistoryRepo>) -> Self {
        Self {
            repo,
            history,
            storage: None,
        }
    }

    /// Write the storage facts of each imported file (size, links, identity)
    /// when the import is recorded (SKADI-T-0773).
    #[must_use]
    pub fn with_storage(mut self, storage: Arc<dyn LibraryFileRepo>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Resolve a book title for the history label, best-effort.
    async fn title_for(&self, id: BookFileId) -> Option<String> {
        let file = self.repo.get_book_file(id).await.ok().flatten()?;
        let book = self.repo.get_book(file.book_id).await.ok().flatten()?;
        Some(book.title)
    }
}

/// `(event, detail, reason_code, at)` of one history row.
type HistoryEvent = (&'static str, Option<String>, Option<String>, DateTime<Utc>);

/// Map a status to a history event, or `None` for the transient states we don't
/// log. Returns `(event, detail, reason_code, at)` — `reason_code` is the
/// structured failure code for `failed` events (SKADI-T-0200).
fn history_event(status: &AcquisitionStatus) -> Option<HistoryEvent> {
    match status {
        AcquisitionStatus::Snatched { at, .. } => Some(("grabbed", None, None, *at)),
        AcquisitionStatus::Imported { quality, at, .. } => {
            let name = default_audiobook_definitions()
                .into_iter()
                .find(|d| d.id == *quality)
                .map(|d| d.name);
            Some(("imported", name, None, *at))
        }
        AcquisitionStatus::Failed { reason, .. } => Some((
            "failed",
            Some(format!("{reason:?}")),
            Some(reason.code().to_string()),
            Utc::now(),
        )),
        _ => None,
    }
}

impl AudiobookStatusSink {
    /// Persist `status`, then append its history row. `upgrade` says the
    /// import replaced a held file (SKADI-T-0743).
    async fn write(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
        upgrade: bool,
    ) -> Result<()> {
        let id = decode(acquirable)?;
        let event = history_event(&status);
        let imported = matches!(status, AcquisitionStatus::Imported { .. });
        // A resumed run writes the same import again: one file, one row in the
        // new-imports list. An upgrade to the same path and quality is still new.
        let replay = !upgrade
            && imported
            && self
                .repo
                .get_book_file(id)
                .await?
                .is_some_and(|f| f.status.is_same_import(&status));
        let placed = match &status {
            AcquisitionStatus::Imported { file, .. } => Some(file.path.clone()),
            _ => None,
        };
        self.repo.set_book_file_status(id, status).await?;
        if let (Some(storage), Some(file)) = (&self.storage, &placed) {
            // A book of many files has one row per file.
            let primary = file.clone();
            let parts = tokio::task::spawn_blocking(move || crate::parts::book_parts(&primary))
                .await
                .unwrap_or_else(|_| vec![file.clone()]);
            for part in parts {
                skadi_library_scan::storage::record_file(
                    storage.as_ref(),
                    skadi_core::MediaKind::Audiobook,
                    &acquirable.0,
                    &part,
                )
                .await;
            }
        }
        // Wake the M4B conversion (SKADI-T-0139); it does nothing when off.
        if imported {
            crate::convert::nudge();
        }

        if let Some(event) = event
            && !replay
        {
            self.record(id, acquirable, event, upgrade).await;
        }
        Ok(())
    }

    /// Append to the acquisition history, best-effort.
    async fn record(
        &self,
        id: BookFileId,
        acquirable: &AcquirableRef,
        (event, detail, reason_code, at): HistoryEvent,
        upgrade: bool,
    ) {
        let label = self
            .title_for(id)
            .await
            .unwrap_or_else(|| "(unknown)".into());
        let entry = HistoryEntry {
            id: Uuid::new_v4().to_string(),
            at,
            kind: "audiobook".into(),
            acquirable_ref: acquirable.0.clone(),
            label,
            event: event.into(),
            detail,
            reason_code,
            upgrade: upgrade && event == "imported",
        };
        if let Err(e) = self.history.record_history(&entry).await {
            tracing::warn!(error = %e, "failed to record acquisition history");
        }
    }

    /// Append the `imported` row for a book file that library import placed
    /// from an upload (SKADI-T-0743). The commit already persisted the status.
    pub async fn record_upload(&self, file: BookFileId, status: &AcquisitionStatus) {
        if let Some(event) = history_event(status) {
            let acquirable = AcquirableRef(file.to_string());
            self.record(file, &acquirable, event, false).await;
        }
    }
}

#[async_trait]
impl StatusSink for AudiobookStatusSink {
    async fn set_status(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
    ) -> Result<()> {
        self.write(acquirable, status, false).await
    }

    async fn set_imported(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
        upgrade: bool,
    ) -> Result<()> {
        self.write(acquirable, status, upgrade).await
    }

    async fn get_status(&self, acquirable: &AcquirableRef) -> Result<Option<AcquisitionStatus>> {
        let id = decode(acquirable)?;
        Ok(self.repo.get_book_file(id).await?.map(|f| f.status))
    }

    async fn set_media_info(
        &self,
        acquirable: &AcquirableRef,
        media_info: skadi_core::MediaInfo,
    ) -> Result<()> {
        let id = decode(acquirable)?;
        if let Some(mut file) = self.repo.get_book_file(id).await? {
            // The hunter probed the recorded file, which for a multi-file book
            // is the first part. Widen to the whole book (SKADI-T-0738).
            let media_info = match &file.status {
                AcquisitionStatus::Imported { file: fr, .. } => {
                    let path = fr.path.clone();
                    let probed = media_info.clone();
                    tokio::task::spawn_blocking(move || {
                        let parts = crate::parts::book_parts(&path);
                        crate::parts::joined_media_info(probed, &parts)
                    })
                    .await
                    .unwrap_or(media_info)
                }
                _ => media_info,
            };
            file.media_info = Some(media_info);
            self.repo.upsert_book_file(&file).await?;
        }
        Ok(())
    }
}

fn decode(r: &AcquirableRef) -> Result<BookFileId> {
    Uuid::parse_str(&r.0)
        .map(BookFileId::from)
        .map_err(|e| AppError::Validation(format!("invalid audiobook AcquirableRef: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;

    use skadi_core::{ExternalIds, FileRef, ProfileId, QualityId, RootFolder};
    use skadi_store::{HistoryRepo, Store};

    use crate::SQLITE_MIGRATIONS;
    use crate::book::Book;
    use crate::book_file::BookFile;
    use crate::repo::AudiobooksRepo;

    async fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skadi.db");
        let url = format!("sqlite://{}", path.display());
        let s = Store::connect(&url).unwrap();
        s.run_migrations().await.unwrap();
        drop(s);
        let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
        (dir, Store::connect(&url).unwrap())
    }

    async fn seed(store: &Store) -> BookFile {
        let book = Book::new(
            ExternalIds::default(),
            "The Way of Kings",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        store.upsert_book(&book).await.unwrap();
        let file = BookFile::missing(book.id);
        store.upsert_book_file(&file).await.unwrap();
        file
    }

    #[tokio::test]
    async fn set_then_get_round_trips_and_imported_records_history() {
        let (_d, store) = fresh_store().await;
        let file = seed(&store).await;
        let s = Arc::new(store);
        let sink = AudiobookStatusSink::new(s.clone(), s.clone());
        let aref = file.acquirable_ref();

        // Searching is transient — no history.
        sink.set_status(
            &aref,
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 1,
            },
        )
        .await
        .unwrap();
        assert!(s.list_history(10, 0).await.unwrap().is_empty());

        // Imported persists + records a labelled "imported" history row.
        sink.set_status(
            &aref,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/audiobooks/x.m4b".into(),
                },
                quality: QualityId::new(),
                score: 0,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();
        let got = sink.get_status(&aref).await.unwrap().unwrap();
        assert!(matches!(got, AcquisitionStatus::Imported { .. }));
        let hist = s.list_history(10, 0).await.unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].event, "imported");
        assert_eq!(hist[0].kind, "audiobook");
        assert_eq!(hist[0].label, "The Way of Kings");
    }

    /// A book of many files gets one storage row per file, at import and from
    /// the sweep's source (SKADI-T-0773).
    #[tokio::test]
    async fn an_import_of_a_book_of_parts_writes_one_storage_row_per_part() {
        use skadi_library_scan::storage::StorageSource as _;
        use skadi_store::LibraryFileRepo as _;

        let (dir, store) = fresh_store().await;
        let file = seed(&store).await;
        let folder = dir.path().join("The Way of Kings");
        std::fs::create_dir_all(&folder).unwrap();
        for part in ["Part 1.mp3", "Part 2.mp3", "Part 10.mp3"] {
            std::fs::write(folder.join(part), vec![0u8; 2048]).unwrap();
        }
        std::fs::write(folder.join("cover.jpg"), b"img").unwrap();
        let s = Arc::new(store.clone());
        let sink = AudiobookStatusSink::new(s.clone(), s.clone()).with_storage(s.clone());

        sink.set_status(
            &file.acquirable_ref(),
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: folder.join("Part 1.mp3"),
                },
                quality: QualityId::new(),
                score: 0,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();

        let rows = store.library_files_of_kind("audiobook").await.unwrap();
        assert_eq!(rows.len(), 3, "one row per part, none for the cover");
        assert!(
            rows.iter()
                .all(|r| r.acquirable_ref == file.acquirable_ref().0)
        );

        let items = crate::storage::BookStorageSource::new(store)
            .files()
            .await
            .unwrap();
        let mut listed: Vec<String> = items
            .iter()
            .map(|i| i.path.to_string_lossy().into_owned())
            .collect();
        listed.sort();
        let mut stored: Vec<String> = rows.into_iter().map(|r| r.path).collect();
        stored.sort();
        assert_eq!(listed, stored, "the sweep lists the files the import wrote");
    }

    #[tokio::test]
    async fn malformed_ref_is_validation_error() {
        let (_d, store) = fresh_store().await;
        let s = Arc::new(store);
        let sink = AudiobookStatusSink::new(s.clone(), s);
        let bad = AcquirableRef("not-a-uuid".into());
        assert!(matches!(
            sink.set_status(&bad, AcquisitionStatus::Cutoff)
                .await
                .unwrap_err(),
            AppError::Validation(_)
        ));
    }
}
