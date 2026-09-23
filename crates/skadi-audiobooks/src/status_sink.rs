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
use skadi_store::{HistoryEntry, HistoryRepo};

use crate::repo::AudiobooksRepo;

pub struct AudiobookStatusSink {
    repo: Arc<dyn AudiobooksRepo>,
    history: Arc<dyn HistoryRepo>,
}

impl AudiobookStatusSink {
    #[must_use]
    pub fn new(repo: Arc<dyn AudiobooksRepo>, history: Arc<dyn HistoryRepo>) -> Self {
        Self { repo, history }
    }

    /// Resolve a book title for the history label, best-effort.
    async fn title_for(&self, id: BookFileId) -> Option<String> {
        let file = self.repo.get_book_file(id).await.ok().flatten()?;
        let book = self.repo.get_book(file.book_id).await.ok().flatten()?;
        Some(book.title)
    }
}

/// Map a status to a history event, or `None` for the transient states we don't
/// log. Returns `(event, detail, reason_code, at)` — `reason_code` is the
/// structured failure code for `failed` events (SKADI-T-0200).
fn history_event(
    status: &AcquisitionStatus,
) -> Option<(&'static str, Option<String>, Option<String>, DateTime<Utc>)> {
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

#[async_trait]
impl StatusSink for AudiobookStatusSink {
    async fn set_status(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
    ) -> Result<()> {
        let id = decode(acquirable)?;
        let event = history_event(&status);
        self.repo.set_book_file_status(id, status).await?;

        if let Some((event, detail, reason_code, at)) = event {
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
            };
            if let Err(e) = self.history.record_history(&entry).await {
                tracing::warn!(error = %e, "failed to record acquisition history");
            }
        }
        Ok(())
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
