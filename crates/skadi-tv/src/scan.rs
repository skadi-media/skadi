//! Television as a library-scan source (SKADI-T-0583).
//!
//! Supplies imported episodes that carry a file but no media info, and records
//! what the prober found. See [`skadi_library_scan`] for why this exists.

use async_trait::async_trait;
use diesel::prelude::*;
use skadi_core::{MediaInfo, Result};
use skadi_library_scan::{ScanItem, ScanProgress, ScanSource};
use skadi_store::Store;

use crate::schema::episodes;

/// Reads and writes `episodes.media_info`.
pub struct EpisodeScanSource {
    store: Store,
}

impl EpisodeScanSource {
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ScanSource for EpisodeScanSource {
    fn name(&self) -> &'static str {
        "television"
    }

    async fn unscanned(&self, limit: i64) -> Result<Vec<ScanItem>> {
        self.store
            .with_conn(move |conn| {
                let rows: Vec<(String, Option<String>)> = episodes::table
                    .filter(episodes::file_path.is_not_null())
                    .filter(episodes::media_info.is_null())
                    .select((episodes::id, episodes::file_path))
                    // Stable order so successive batches walk forward rather
                    // than re-reading whatever the planner happened to return.
                    .order(episodes::id.asc())
                    .limit(limit)
                    .load(conn)
                    .map_err(crate::repo::db_err)?;
                Ok(rows
                    .into_iter()
                    .filter_map(|(id, path)| path.map(|p| ScanItem { id, path: p.into() }))
                    .collect())
            })
            .await
    }

    async fn store(&self, id: &str, info: Option<MediaInfo>) -> Result<()> {
        // A failed probe is stored as an EMPTY MediaInfo, not left NULL.
        //
        // This is the livelock guard: `unscanned` selects on `media_info IS
        // NULL`, so leaving a file we cannot read as NULL brings it back in
        // every batch forever and the scan never reaches the rest of the
        // library. An empty struct reads as "asked, learned nothing", which
        // `assess` correctly treats as no issue rather than as a fault.
        let info = info.unwrap_or_default();
        let json = serde_json::to_string(&info)
            .map_err(|e| skadi_core::AppError::Internal(format!("encoding media_info: {e}")))?;
        let id = id.to_string();
        self.store
            .with_conn(move |conn| {
                diesel::update(episodes::table.filter(episodes::id.eq(&id)))
                    .set(episodes::media_info.eq(Some(json)))
                    .execute(conn)
                    .map_err(crate::repo::db_err)?;
                Ok(())
            })
            .await
    }

    async fn progress(&self) -> Result<ScanProgress> {
        self.store
            .with_conn(move |conn| {
                // Only files that exist are scannable: an unaired or un-acquired
                // episode has nothing to probe, and counting it would leave the
                // progress bar permanently short of 100%.
                let total: i64 = episodes::table
                    .filter(episodes::file_path.is_not_null())
                    .count()
                    .get_result(conn)
                    .map_err(crate::repo::db_err)?;
                let scanned: i64 = episodes::table
                    .filter(episodes::file_path.is_not_null())
                    .filter(episodes::media_info.is_not_null())
                    .count()
                    .get_result(conn)
                    .map_err(crate::repo::db_err)?;
                Ok(ScanProgress { scanned, total })
            })
            .await
    }
}
