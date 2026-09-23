//! Hunter trace stream (SKADI-T-0323).
//!
//! An append-only, structured per-step event log of what the acquire pipeline
//! *did* — candidates found, the decision and why, snatch → downloader, download
//! stalls/failures, import outcome. The richer companion to [`crate::history`]
//! (coarse `grabbed`/`imported`/`failed` outcomes), built for **diagnosing the
//! hunter** against ephemeral/inconsistent torrent sources: you can read back
//! why an item searched, what it found, what it chose and why, and how a flaky
//! grab/download actually went — without re-running anything.
//!
//! Cross-domain by design: rows are keyed by the opaque `acquirable_ref` and
//! carry a denormalised `kind`, so the read endpoint needs no join back into a
//! domain. The hunter records via [`TraceRepo`] off the acquire hot path,
//! best-effort (a recording failure is logged, never fails the workflow).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::trace_events;
use crate::{Store, db_err};

/// One structured trace event emitted by the acquire pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceEvent {
    pub id: String,
    pub at: DateTime<Utc>,
    /// The acquire run this event belongs to, when known (correlates retries of
    /// the same item). `None` for events emitted outside a tracked run.
    pub run_id: Option<String>,
    /// Domain kind, e.g. `"movie"`.
    pub kind: String,
    /// The acquirable this event is about (opaque ref the domain understands).
    pub acquirable_ref: String,
    /// Pipeline stage: `searching` / `deciding` / `grabbing` / `downloading` /
    /// `importing`.
    pub stage: String,
    /// Short machine event label, e.g. `candidates_found`, `decision`,
    /// `no_release`, `snatched`, `download_failed`, `imported`, `import_failed`.
    pub event: String,
    /// Human one-line summary for the log.
    pub message: String,
    /// Optional structured/long payload (e.g. an error string, infohash).
    pub detail: Option<String>,
}

/// Append + read the hunter trace stream.
#[async_trait]
pub trait TraceRepo: Send + Sync {
    /// Append one trace event (best-effort — callers on the acquire hot path must
    /// not fail the workflow if recording fails; log and continue).
    async fn record_trace(&self, entry: &TraceEvent) -> Result<()>;
    /// Most recent `limit` events (newest first), skipping `offset`.
    async fn list_traces(&self, limit: i64, offset: i64) -> Result<Vec<TraceEvent>>;
    /// All events for one acquirable, newest first.
    async fn traces_for(&self, acquirable_ref: &str) -> Result<Vec<TraceEvent>>;
    /// How many events exist, ignoring paging — the total a paged client needs to
    /// render page controls (SKADI-T-0468).
    async fn count_traces(&self) -> Result<i64>;
    /// Delete rows older than `cutoff` (SKADI-T-0440), returning how many went.
    ///
    /// These tables were unbounded: every acquire run appends to them and nothing
    /// ever removed a row, so a long-lived install grew them forever. Diagnostic
    /// data has a shelf life — a trace from six months ago explains nothing about
    /// today — so age-based retention is the right shape, matching
    /// `acquisition_history`.
    async fn purge_traces_before(&self, cutoff: DateTime<Utc>) -> Result<usize>;
}

/// The stored row. `at` uses the portable `Timestamp` type.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = trace_events)]
struct Row {
    id: String,
    at: Timestamp,
    run_id: Option<String>,
    kind: String,
    acquirable_ref: String,
    stage: String,
    event: String,
    message: String,
    detail: Option<String>,
}

impl From<Row> for TraceEvent {
    fn from(r: Row) -> Self {
        TraceEvent {
            id: r.id,
            at: r.at.0,
            run_id: r.run_id,
            kind: r.kind,
            acquirable_ref: r.acquirable_ref,
            stage: r.stage,
            event: r.event,
            message: r.message,
            detail: r.detail,
        }
    }
}

impl From<&TraceEvent> for Row {
    fn from(e: &TraceEvent) -> Self {
        Row {
            id: e.id.clone(),
            at: Timestamp(e.at),
            run_id: e.run_id.clone(),
            kind: e.kind.clone(),
            acquirable_ref: e.acquirable_ref.clone(),
            stage: e.stage.clone(),
            event: e.event.clone(),
            message: e.message.clone(),
            detail: e.detail.clone(),
        }
    }
}

#[async_trait]
impl TraceRepo for Store {
    async fn record_trace(&self, entry: &TraceEvent) -> Result<()> {
        let row = Row::from(entry);
        self.with_conn(move |conn| {
            diesel::insert_into(trace_events::table)
                .values(&row)
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn purge_traces_before(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        self.with_conn(move |conn| {
            let n =
                diesel::delete(trace_events::table.filter(trace_events::at.lt(Timestamp(cutoff))))
                    .execute(conn)
                    .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn count_traces(&self) -> Result<i64> {
        self.with_conn(move |conn| {
            trace_events::table
                .count()
                .get_result::<i64>(conn)
                .map_err(db_err)
        })
        .await
    }

    async fn list_traces(&self, limit: i64, offset: i64) -> Result<Vec<TraceEvent>> {
        self.with_conn(move |conn| {
            let rows: Vec<Row> = trace_events::table
                .select(Row::as_select())
                .order(trace_events::at.desc())
                .limit(limit)
                .offset(offset)
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(TraceEvent::from).collect())
        })
        .await
    }

    async fn traces_for(&self, acquirable_ref: &str) -> Result<Vec<TraceEvent>> {
        let aref = acquirable_ref.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<Row> = trace_events::table
                .filter(trace_events::acquirable_ref.eq(aref))
                .select(Row::as_select())
                .order(trace_events::at.desc())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(TraceEvent::from).collect())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, secs: i64, event: &str, message: &str) -> TraceEvent {
        TraceEvent {
            id: id.into(),
            at: DateTime::<Utc>::from_timestamp(secs, 0).unwrap(),
            run_id: Some(format!("run-{id}")),
            kind: "movie".into(),
            acquirable_ref: "ed-1".into(),
            stage: "deciding".into(),
            event: event.into(),
            message: message.into(),
            detail: None,
        }
    }

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("trace").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn records_and_lists_newest_first_and_by_acquirable() {
        let store = temp_store().await;

        store
            .record_trace(&event("a", 100, "candidates_found", "found 3 candidates"))
            .await
            .unwrap();
        store
            .record_trace(&event("b", 200, "decision", "chose The Movie of 3"))
            .await
            .unwrap();

        let recent = store.list_traces(10, 0).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].event, "decision", "newest first");
        assert_eq!(recent[0].message, "chose The Movie of 3");
        assert_eq!(recent[0].run_id.as_deref(), Some("run-b"));

        let for_ed = store.traces_for("ed-1").await.unwrap();
        assert_eq!(for_ed.len(), 2);
        assert!(store.traces_for("nope").await.unwrap().is_empty());
    }
    /// SKADI-T-0440: `trace_events` grew forever — every acquire run appends and
    /// nothing removed a row. Diagnostic data has a shelf life, so retention is
    /// age-based like `acquisition_history`.
    #[tokio::test]
    async fn purge_traces_before_removes_only_older_rows() {
        let store = temp_store().await;
        store
            .record_trace(&event("old", 100, "searching", "old"))
            .await
            .unwrap();
        store
            .record_trace(&event("new", 5_000, "searching", "new"))
            .await
            .unwrap();

        let cutoff = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        assert_eq!(store.purge_traces_before(cutoff).await.unwrap(), 1);

        let left = store.list_traces(10, 0).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "new", "the newer row survives");

        // Idempotent: a second pass with the same cutoff removes nothing.
        assert_eq!(store.purge_traces_before(cutoff).await.unwrap(), 0);
    }
}
