//! Persisted decision history (SKADI-T-0187).
//!
//! An append-only record of *why* the daemon grabbed each release: the chosen
//! release's full decision explanation (classified quality, the
//! Accept/Upgrade/Reject/MeetsCutoff decision, matched custom formats + scores,
//! the rank, and the reason) serialized at grab time. This is the persisted
//! companion to the *live* "why" that `…/quality/test` and `…/releases` already
//! compute — so the Activity/History UI can explain a past grab without
//! re-searching.
//!
//! Cross-domain by design: rows are keyed by the opaque `acquirable_ref` and
//! carry a denormalised `kind`/`title` so the read endpoint needs no join back
//! into a domain. The hunter records via [`DecisionHistoryRepo`] at the snatch
//! boundary (it has the chosen release + scoring there).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::decision_history;
use crate::{Store, db_err};

/// One recorded decision: why a chosen release was grabbed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionEntry {
    pub id: String,
    pub at: DateTime<Utc>,
    /// Domain kind, e.g. `"movie"`.
    pub kind: String,
    /// The acquirable this decision is about (opaque ref the domain understands).
    pub acquirable_ref: String,
    /// The chosen release title.
    pub title: String,
    /// Classified quality name, when known (denormalised from `explanation`).
    pub quality: Option<String>,
    /// The profile decision (`Accept`/`Upgrade`/…), when classified.
    pub decision: Option<String>,
    /// Aggregate custom-format score.
    pub format_score: i32,
    /// The full serialized `ReleaseExplanation` JSON (matched formats, rank, reason).
    pub explanation: String,
    /// Stable blocklist identity of the chosen release (SKADI-T-0196) — correlates
    /// this grab to its History/Activity row and powers "blocklist-and-search".
    /// `None` for decisions recorded before this column existed.
    pub release_key: Option<String>,
}

/// Append + read the decision history.
#[async_trait]
pub trait DecisionHistoryRepo: Send + Sync {
    /// Append one decision (best-effort — callers on the acquire hot path must
    /// not fail the workflow if recording fails; log and continue).
    async fn record_decision(&self, entry: &DecisionEntry) -> Result<()>;
    /// Most recent `limit` decisions (newest first), skipping `offset`.
    async fn list_decisions(&self, limit: i64, offset: i64) -> Result<Vec<DecisionEntry>>;
    /// All decisions for one acquirable, newest first.
    async fn decisions_for(&self, acquirable_ref: &str) -> Result<Vec<DecisionEntry>>;
    /// How many decisions exist, ignoring paging (SKADI-T-0468).
    async fn count_decisions(&self) -> Result<i64>;
    /// Delete rows older than `cutoff` (SKADI-T-0440), returning how many went.
    ///
    /// These tables were unbounded: every acquire run appends to them and nothing
    /// ever removed a row, so a long-lived install grew them forever. Diagnostic
    /// data has a shelf life — a trace from six months ago explains nothing about
    /// today — so age-based retention is the right shape, matching
    /// `acquisition_history`.
    async fn purge_decisions_before(&self, cutoff: DateTime<Utc>) -> Result<usize>;
}

/// The stored row. `at` uses the portable `Timestamp` type.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = decision_history)]
struct Row {
    id: String,
    at: Timestamp,
    kind: String,
    acquirable_ref: String,
    title: String,
    quality: Option<String>,
    decision: Option<String>,
    format_score: i32,
    explanation: String,
    release_key: Option<String>,
}

impl From<Row> for DecisionEntry {
    fn from(r: Row) -> Self {
        DecisionEntry {
            id: r.id,
            at: r.at.0,
            kind: r.kind,
            acquirable_ref: r.acquirable_ref,
            title: r.title,
            quality: r.quality,
            decision: r.decision,
            format_score: r.format_score,
            explanation: r.explanation,
            release_key: r.release_key,
        }
    }
}

impl From<&DecisionEntry> for Row {
    fn from(e: &DecisionEntry) -> Self {
        Row {
            id: e.id.clone(),
            at: Timestamp(e.at),
            kind: e.kind.clone(),
            acquirable_ref: e.acquirable_ref.clone(),
            title: e.title.clone(),
            quality: e.quality.clone(),
            decision: e.decision.clone(),
            format_score: e.format_score,
            explanation: e.explanation.clone(),
            release_key: e.release_key.clone(),
        }
    }
}

#[async_trait]
impl DecisionHistoryRepo for Store {
    async fn record_decision(&self, entry: &DecisionEntry) -> Result<()> {
        let row = Row::from(entry);
        self.with_conn(move |conn| {
            diesel::insert_into(decision_history::table)
                .values(&row)
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn purge_decisions_before(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        self.with_conn(move |conn| {
            let n = diesel::delete(
                decision_history::table.filter(decision_history::at.lt(Timestamp(cutoff))),
            )
            .execute(conn)
            .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn count_decisions(&self) -> Result<i64> {
        self.with_conn(move |conn| {
            decision_history::table
                .count()
                .get_result::<i64>(conn)
                .map_err(db_err)
        })
        .await
    }

    async fn list_decisions(&self, limit: i64, offset: i64) -> Result<Vec<DecisionEntry>> {
        self.with_conn(move |conn| {
            let rows: Vec<Row> = decision_history::table
                .select(Row::as_select())
                .order(decision_history::at.desc())
                .limit(limit)
                .offset(offset)
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(DecisionEntry::from).collect())
        })
        .await
    }

    async fn decisions_for(&self, acquirable_ref: &str) -> Result<Vec<DecisionEntry>> {
        let aref = acquirable_ref.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<Row> = decision_history::table
                .filter(decision_history::acquirable_ref.eq(aref))
                .select(Row::as_select())
                .order(decision_history::at.desc())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(DecisionEntry::from).collect())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, secs: i64, title: &str) -> DecisionEntry {
        DecisionEntry {
            id: id.into(),
            at: DateTime::<Utc>::from_timestamp(secs, 0).unwrap(),
            kind: "movie".into(),
            acquirable_ref: "ed-1".into(),
            title: title.into(),
            quality: Some("Bluray-1080p".into()),
            decision: Some("Accept".into()),
            format_score: 200,
            explanation: r#"{"accepted":true}"#.into(),
            release_key: Some(format!("btih:{id}")),
        }
    }

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("decision").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn records_and_lists_newest_first_and_by_acquirable() {
        let store = temp_store().await;

        store
            .record_decision(&entry("a", 100, "old"))
            .await
            .unwrap();
        store
            .record_decision(&entry("b", 200, "new"))
            .await
            .unwrap();

        let recent = store.list_decisions(10, 0).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].title, "new", "newest first");
        assert_eq!(recent[0].format_score, 200);
        // The stable release identity round-trips (grab→import correlation).
        assert_eq!(recent[0].release_key.as_deref(), Some("btih:b"));

        let for_ed = store.decisions_for("ed-1").await.unwrap();
        assert_eq!(for_ed.len(), 2);
        assert!(store.decisions_for("nope").await.unwrap().is_empty());
    }
    /// SKADI-T-0440: `decision_history` grew forever, same as `trace_events`.
    #[tokio::test]
    async fn purge_decisions_before_removes_only_older_rows() {
        let store = temp_store().await;
        store
            .record_decision(&entry("old", 100, "Old"))
            .await
            .unwrap();
        store
            .record_decision(&entry("new", 5_000, "New"))
            .await
            .unwrap();

        let cutoff = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        assert_eq!(store.purge_decisions_before(cutoff).await.unwrap(), 1);

        let left = store.list_decisions(10, 0).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "new", "the newer row survives");
        assert_eq!(store.purge_decisions_before(cutoff).await.unwrap(), 0);
    }
}
