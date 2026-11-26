//! Audiobooks as a source of the storage sweep (SKADI-T-0773).
//!
//! A book file records one path. For a folder of parts that is the first
//! part; [`crate::parts::book_parts`] finds the others. The sweep gets one
//! item per part, so a book of many files has one storage row per file.

use async_trait::async_trait;
use diesel::prelude::*;
use skadi_core::{MediaKind, Result};
use skadi_library_scan::storage::{StorageItem, StorageSource};
use skadi_store::Store;

use crate::schema::book_editions;

/// The files the book files hold.
pub struct BookStorageSource {
    store: Store,
}

impl BookStorageSource {
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl StorageSource for BookStorageSource {
    fn name(&self) -> &'static str {
        "audiobooks"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Audiobook
    }

    async fn files(&self) -> Result<Vec<StorageItem>> {
        let rows: Vec<(String, Option<String>)> = self
            .store
            .with_conn(move |conn| {
                book_editions::table
                    .filter(book_editions::file_path.is_not_null())
                    .select((book_editions::id, book_editions::file_path))
                    .order(book_editions::id.asc())
                    .load(conn)
                    .map_err(crate::repo::db_err)
            })
            .await?;
        // One folder read per book, on the share: off the async runtime.
        tokio::task::spawn_blocking(move || {
            let mut items = Vec::new();
            for (id, path) in rows {
                let Some(path) = path else { continue };
                // `book_parts` gives the recorded file alone when the folder
                // cannot be read, so a book that is gone still has its item.
                for part in crate::parts::book_parts(std::path::Path::new(&path)) {
                    items.push(StorageItem {
                        acquirable_ref: id.clone(),
                        path: part,
                    });
                }
            }
            items
        })
        .await
        .map_err(|e| skadi_core::AppError::Internal(format!("listing book parts: {e}")))
    }
}
