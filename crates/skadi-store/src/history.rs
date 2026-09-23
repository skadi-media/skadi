//! Persistent acquisition history (SKADI-T-0082).
//!
//! An append-only log of the meaningful acquire transitions — grabbed,
//! imported, failed — so the operator can see what the daemon has done over
//! time (the live in-flight view is the in-memory tracker in `skadi-hunter`).
//!
//! Cross-domain by design: rows are keyed by the opaque `acquirable_ref` and
//! carry a denormalised `label` (the item title at record time) so the read
//! endpoint needs no join back into a domain. Domains record via the
//! [`HistoryRepo`] (the movies status sink does, in `skadi-movies`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::acquisition_history;
use crate::{Store, db_err};

/// One recorded acquisition event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: String,
    pub at: DateTime<Utc>,
    /// Domain kind, e.g. `"movie"`.
    pub kind: String,
    /// The acquirable this event is about (opaque ref the domain understands).
    pub acquirable_ref: String,
    /// Human label at record time (e.g. the movie title).
    pub label: String,
    /// `grabbed` / `imported` / `failed`.
    pub event: String,
    /// Extra context: quality, indexer, or a failure reason (free text).
    pub detail: Option<String>,
    /// Structured failure reason code (SKADI-T-0200) for failed events — the
    /// machine-filterable companion to `detail`. `None` for non-failures.
    pub reason_code: Option<String>,
}

/// Server-side filter for the history read (SKADI-T-0194). Every field is
/// optional and AND-combined; `None` means "don't constrain on this axis". All
/// filters are exact-match except the `[since, until]` half-open time window, so
/// they're portable across SQLite/Postgres with no `ILIKE`/collation concerns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    /// `grabbed` / `imported` / `failed`.
    pub event: Option<String>,
    /// Domain kind, e.g. `"movie"`.
    pub kind: Option<String>,
    /// Scope to one acquirable.
    pub acquirable_ref: Option<String>,
    /// Inclusive lower time bound.
    pub since: Option<DateTime<Utc>>,
    /// Exclusive upper time bound.
    pub until: Option<DateTime<Utc>>,
    /// Structured failure reason code (e.g. `import_failed`).
    pub reason_code: Option<String>,
}

/// Per-event totals for the dashboard badges (SKADI-T-0194).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryCounts {
    pub total: i64,
    pub grabbed: i64,
    pub imported: i64,
    pub failed: i64,
}

/// Append + read the acquisition history.
#[async_trait]
pub trait HistoryRepo: Send + Sync {
    /// Append one event (best-effort — callers on the acquire hot path should
    /// not fail the workflow if history recording fails; log and continue).
    async fn record_history(&self, entry: &HistoryEntry) -> Result<()>;
    /// Most recent `limit` events (newest first), skipping `offset`.
    async fn list_history(&self, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>>;
    /// One event by id, or `None` if absent (the per-event detail read).
    async fn get_history(&self, id: &str) -> Result<Option<HistoryEntry>>;
    /// Filtered, paginated history (newest first) — the server-side query the
    /// Activity/History view drives (SKADI-T-0194).
    async fn list_history_filtered(
        &self,
        query: &HistoryQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<HistoryEntry>>;
    /// Total rows matching `query` (ignores limit/offset) — for pagination.
    async fn count_history(&self, query: &HistoryQuery) -> Result<i64>;
    /// Whole-table per-event totals for badges (unfiltered).
    async fn history_counts(&self) -> Result<HistoryCounts>;
    /// Delete events older than `cutoff` (age-based retention, SKADI-T-0199);
    /// returns the number purged.
    async fn purge_history_before(&self, cutoff: DateTime<Utc>) -> Result<usize>;
    /// Keep only the newest `max_rows` events, deleting the rest (size-based
    /// retention, SKADI-T-0199); returns the number purged. `max_rows <= 0` clears
    /// the table. Ties on the boundary timestamp are kept (so the result may retain
    /// marginally more than `max_rows`).
    async fn trim_history_to_newest(&self, max_rows: i64) -> Result<usize>;
}

/// The stored row. `at` uses the portable `Timestamp` type.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = acquisition_history)]
struct Row {
    id: String,
    at: Timestamp,
    kind: String,
    acquirable_ref: String,
    label: String,
    event: String,
    detail: Option<String>,
    reason_code: Option<String>,
}

impl From<Row> for HistoryEntry {
    fn from(r: Row) -> Self {
        HistoryEntry {
            id: r.id,
            at: r.at.0,
            kind: r.kind,
            acquirable_ref: r.acquirable_ref,
            label: r.label,
            event: r.event,
            detail: r.detail,
            reason_code: r.reason_code,
        }
    }
}

impl From<&HistoryEntry> for Row {
    fn from(e: &HistoryEntry) -> Self {
        Row {
            id: e.id.clone(),
            at: Timestamp(e.at),
            kind: e.kind.clone(),
            acquirable_ref: e.acquirable_ref.clone(),
            label: e.label.clone(),
            event: e.event.clone(),
            detail: e.detail.clone(),
            reason_code: e.reason_code.clone(),
        }
    }
}

#[async_trait]
impl HistoryRepo for Store {
    async fn record_history(&self, entry: &HistoryEntry) -> Result<()> {
        let row = Row::from(entry);
        self.with_conn(move |conn| {
            diesel::insert_into(acquisition_history::table)
                .values(&row)
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn list_history(&self, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>> {
        self.list_history_filtered(&HistoryQuery::default(), limit, offset)
            .await
    }

    async fn get_history(&self, id: &str) -> Result<Option<HistoryEntry>> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<Row> = acquisition_history::table
                .find(&id)
                .select(Row::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(HistoryEntry::from))
        })
        .await
    }

    async fn list_history_filtered(
        &self,
        query: &HistoryQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<HistoryEntry>> {
        let query = query.clone();
        self.with_conn(move |conn| {
            // `into_boxed()` gives a uniform type so optional filters can be added
            // conditionally; the backend is inferred from `load(conn)` (MultiBackend).
            let mut q = acquisition_history::table.into_boxed();
            if let Some(ev) = &query.event {
                q = q.filter(acquisition_history::event.eq(ev.clone()));
            }
            if let Some(kind) = &query.kind {
                q = q.filter(acquisition_history::kind.eq(kind.clone()));
            }
            if let Some(aref) = &query.acquirable_ref {
                q = q.filter(acquisition_history::acquirable_ref.eq(aref.clone()));
            }
            if let Some(rc) = &query.reason_code {
                q = q.filter(acquisition_history::reason_code.eq(rc.clone()));
            }
            if let Some(since) = query.since {
                q = q.filter(acquisition_history::at.ge(Timestamp(since)));
            }
            if let Some(until) = query.until {
                q = q.filter(acquisition_history::at.lt(Timestamp(until)));
            }
            let rows: Vec<Row> = q
                .select(Row::as_select())
                .order(acquisition_history::at.desc())
                .limit(limit)
                .offset(offset)
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(HistoryEntry::from).collect())
        })
        .await
    }

    async fn count_history(&self, query: &HistoryQuery) -> Result<i64> {
        let query = query.clone();
        self.with_conn(move |conn| {
            let mut q = acquisition_history::table.into_boxed();
            if let Some(ev) = &query.event {
                q = q.filter(acquisition_history::event.eq(ev.clone()));
            }
            if let Some(kind) = &query.kind {
                q = q.filter(acquisition_history::kind.eq(kind.clone()));
            }
            if let Some(aref) = &query.acquirable_ref {
                q = q.filter(acquisition_history::acquirable_ref.eq(aref.clone()));
            }
            if let Some(rc) = &query.reason_code {
                q = q.filter(acquisition_history::reason_code.eq(rc.clone()));
            }
            if let Some(since) = query.since {
                q = q.filter(acquisition_history::at.ge(Timestamp(since)));
            }
            if let Some(until) = query.until {
                q = q.filter(acquisition_history::at.lt(Timestamp(until)));
            }
            let n: i64 = q.count().get_result(conn).map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn history_counts(&self) -> Result<HistoryCounts> {
        self.with_conn(move |conn| {
            let count_event = |conn: &mut crate::DualConnection, ev: &str| -> Result<i64> {
                acquisition_history::table
                    .filter(acquisition_history::event.eq(ev))
                    .count()
                    .get_result(conn)
                    .map_err(db_err)
            };
            let grabbed = count_event(conn, "grabbed")?;
            let imported = count_event(conn, "imported")?;
            let failed = count_event(conn, "failed")?;
            let total: i64 = acquisition_history::table
                .count()
                .get_result(conn)
                .map_err(db_err)?;
            Ok(HistoryCounts {
                total,
                grabbed,
                imported,
                failed,
            })
        })
        .await
    }

    async fn purge_history_before(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        self.with_conn(move |conn| {
            let n = diesel::delete(
                acquisition_history::table.filter(acquisition_history::at.lt(Timestamp(cutoff))),
            )
            .execute(conn)
            .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn trim_history_to_newest(&self, max_rows: i64) -> Result<usize> {
        self.with_conn(move |conn| {
            if max_rows <= 0 {
                return diesel::delete(acquisition_history::table)
                    .execute(conn)
                    .map_err(db_err);
            }
            let total: i64 = acquisition_history::table
                .count()
                .get_result(conn)
                .map_err(db_err)?;
            if total <= max_rows {
                return Ok(0);
            }
            // The timestamp of the `max_rows`-th newest row is the keep-boundary;
            // delete everything strictly older (ties at the boundary are kept).
            let threshold: Option<Timestamp> = acquisition_history::table
                .order(acquisition_history::at.desc())
                .select(acquisition_history::at)
                .offset(max_rows - 1)
                .limit(1)
                .first(conn)
                .optional()
                .map_err(db_err)?;
            match threshold {
                Some(t) => {
                    diesel::delete(acquisition_history::table.filter(acquisition_history::at.lt(t)))
                        .execute(conn)
                        .map_err(db_err)
                }
                None => Ok(0),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("history").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    fn entry(id: &str, secs: i64, event: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            at: DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap(),
            kind: "movie".into(),
            acquirable_ref: "ref-1".into(),
            label: "The Matrix".into(),
            event: event.into(),
            detail: Some("Bluray-1080p".into()),
            reason_code: None,
        }
    }

    #[tokio::test]
    async fn history_records_and_lists_newest_first() {
        let store = temp_store().await;
        assert!(store.list_history(10, 0).await.unwrap().is_empty());

        store
            .record_history(&entry("a", 0, "grabbed"))
            .await
            .unwrap();
        store
            .record_history(&entry("b", 10, "imported"))
            .await
            .unwrap();
        store
            .record_history(&entry("c", 5, "failed"))
            .await
            .unwrap();

        let all = store.list_history(10, 0).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
        assert_eq!(all[0].event, "imported");
        assert_eq!(all[0].label, "The Matrix");

        let page = store.list_history(1, 1).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "c");
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    fn full_entry(id: &str, secs: i64, kind: &str, aref: &str, event: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            at: at(secs),
            kind: kind.into(),
            acquirable_ref: aref.into(),
            label: id.into(),
            event: event.into(),
            detail: None,
            reason_code: None,
        }
    }

    #[tokio::test]
    async fn filtered_history_constrains_by_event_kind_acquirable_and_time() {
        let store = temp_store().await;
        for e in [
            full_entry("g1", 0, "movie", "m-1", "grabbed"),
            full_entry("i1", 10, "movie", "m-1", "imported"),
            full_entry("f1", 20, "movie", "m-2", "failed"),
            full_entry("g2", 30, "audiobook", "a-1", "grabbed"),
        ] {
            store.record_history(&e).await.unwrap();
        }

        // By event.
        let failed = store
            .list_history_filtered(
                &HistoryQuery {
                    event: Some("failed".into()),
                    ..Default::default()
                },
                50,
                0,
            )
            .await
            .unwrap();
        assert_eq!(
            failed.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["f1"]
        );

        // By kind.
        let ab = store
            .list_history_filtered(
                &HistoryQuery {
                    kind: Some("audiobook".into()),
                    ..Default::default()
                },
                50,
                0,
            )
            .await
            .unwrap();
        assert_eq!(ab.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["g2"]);

        // By acquirable.
        let m1 = store
            .list_history_filtered(
                &HistoryQuery {
                    acquirable_ref: Some("m-1".into()),
                    ..Default::default()
                },
                50,
                0,
            )
            .await
            .unwrap();
        // newest-first: i1 (t=10) before g1 (t=0).
        assert_eq!(
            m1.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["i1", "g1"]
        );

        // By half-open time window [10, 30): includes i1 (10) and f1 (20), excludes
        // g1 (0, below) and g2 (30, at the exclusive upper bound).
        let win = store
            .list_history_filtered(
                &HistoryQuery {
                    since: Some(at(10)),
                    until: Some(at(30)),
                    ..Default::default()
                },
                50,
                0,
            )
            .await
            .unwrap();
        assert_eq!(
            win.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["f1", "i1"]
        );

        // Combined filters AND together.
        let combined = store
            .list_history_filtered(
                &HistoryQuery {
                    kind: Some("movie".into()),
                    event: Some("grabbed".into()),
                    ..Default::default()
                },
                50,
                0,
            )
            .await
            .unwrap();
        assert_eq!(
            combined.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["g1"]
        );
    }

    #[tokio::test]
    async fn count_history_respects_filters_and_counts_tally_by_event() {
        let store = temp_store().await;
        for e in [
            full_entry("g1", 0, "movie", "m-1", "grabbed"),
            full_entry("g2", 5, "movie", "m-2", "grabbed"),
            full_entry("i1", 10, "movie", "m-1", "imported"),
            full_entry("f1", 20, "audiobook", "a-1", "failed"),
        ] {
            store.record_history(&e).await.unwrap();
        }

        assert_eq!(
            store.count_history(&HistoryQuery::default()).await.unwrap(),
            4
        );
        assert_eq!(
            store
                .count_history(&HistoryQuery {
                    event: Some("grabbed".into()),
                    ..Default::default()
                })
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .count_history(&HistoryQuery {
                    kind: Some("audiobook".into()),
                    ..Default::default()
                })
                .await
                .unwrap(),
            1
        );

        let counts = store.history_counts().await.unwrap();
        assert_eq!(counts.total, 4);
        assert_eq!(counts.grabbed, 2);
        assert_eq!(counts.imported, 1);
        assert_eq!(counts.failed, 1);
    }

    #[tokio::test]
    async fn retention_purges_by_age_and_trims_to_newest() {
        let store = temp_store().await;
        // Five events at t = 0, 10, 20, 30, 40.
        for i in 0..5 {
            store
                .record_history(&full_entry(
                    &format!("e{i}"),
                    i * 10,
                    "movie",
                    "m-1",
                    "grabbed",
                ))
                .await
                .unwrap();
        }

        // Age purge: drop everything before t=20 → removes e0 (0) and e1 (10).
        let purged = store.purge_history_before(at(20)).await.unwrap();
        assert_eq!(purged, 2);
        assert_eq!(
            store.count_history(&HistoryQuery::default()).await.unwrap(),
            3
        );

        // Trim to the newest 2 → removes e2 (20), keeps e3 (30), e4 (40).
        let trimmed = store.trim_history_to_newest(2).await.unwrap();
        assert_eq!(trimmed, 1);
        let kept = store.list_history(10, 0).await.unwrap();
        assert_eq!(
            kept.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["e4", "e3"]
        );

        // Trim with a larger budget is a no-op; max_rows<=0 clears.
        assert_eq!(store.trim_history_to_newest(10).await.unwrap(), 0);
        assert_eq!(store.trim_history_to_newest(0).await.unwrap(), 2);
        assert!(store.list_history(10, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn filters_by_structured_reason_code() {
        let store = temp_store().await;
        let mk = |id: &str, code: Option<&str>| HistoryEntry {
            id: id.into(),
            at: at(0),
            kind: "movie".into(),
            acquirable_ref: "m-1".into(),
            label: id.into(),
            event: "failed".into(),
            detail: Some("boom".into()),
            reason_code: code.map(Into::into),
        };
        store
            .record_history(&mk("a", Some("import_failed")))
            .await
            .unwrap();
        store
            .record_history(&mk("b", Some("download_failed")))
            .await
            .unwrap();
        store
            .record_history(&mk("c", Some("import_failed")))
            .await
            .unwrap();

        let q = HistoryQuery {
            reason_code: Some("import_failed".into()),
            ..Default::default()
        };
        let rows = store.list_history_filtered(&q, 50, 0).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|e| e.reason_code.as_deref() == Some("import_failed"))
        );
        assert_eq!(store.count_history(&q).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn get_history_by_id() {
        let store = temp_store().await;
        store
            .record_history(&full_entry("h1", 0, "movie", "m-1", "grabbed"))
            .await
            .unwrap();
        let got = store.get_history("h1").await.unwrap().expect("found");
        assert_eq!(got.acquirable_ref, "m-1");
        assert_eq!(got.event, "grabbed");
        assert!(store.get_history("nope").await.unwrap().is_none());
    }
}
