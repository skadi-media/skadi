//! Worker liveness heartbeat (SKADI-T-0288).
//!
//! The download worker (`skadi-downloader-worker`, a separate process sharing only
//! the DB) [`heartbeat_worker`](WorkerStatusRepo::heartbeat_worker)s every tick,
//! independent of any claimed jobs. The daemon reads the latest row: a `last_seen_at`
//! within a freshness window means the worker is alive; stale or absent means it's
//! down. This fills the gap the per-job lease couldn't — an idle-but-healthy worker
//! is now distinguishable from a dead one.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::worker_status;
use crate::{Store, db_err};

/// One worker's last-seen heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerStatus {
    pub worker_id: String,
    pub last_seen_at: DateTime<Utc>,
    pub version: String,
    /// The public IP the worker observed for its own egress at its last
    /// heartbeat (SKADI-T-0683): what gluetun's control server answered on the
    /// worker's loopback. `None` = it could not ask (no gluetun in its network
    /// namespace), or an older worker.
    pub egress_ip: Option<String>,
}

impl WorkerStatus {
    /// Whether this heartbeat is fresh enough (last seen within `window`) to count
    /// the worker as running.
    #[must_use]
    pub fn is_fresh(&self, window: chrono::Duration) -> bool {
        Utc::now().signed_duration_since(self.last_seen_at) <= window
    }
}

/// Upsert + read worker liveness.
#[async_trait]
pub trait WorkerStatusRepo: Send + Sync {
    /// Upsert this worker's heartbeat (by `worker_id`) — called every tick —
    /// with the egress IP it last observed (`None` overwrites a previous one:
    /// the heartbeat says what the worker sees now).
    async fn heartbeat_worker(
        &self,
        worker_id: &str,
        version: &str,
        egress_ip: Option<&str>,
    ) -> Result<()>;
    /// The most-recently-seen worker, if any (drives "is the worker up").
    async fn latest_worker_status(&self) -> Result<Option<WorkerStatus>>;
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = worker_status)]
// A beat without an egress IP must clear the last one, not keep it: the
// default changeset skips `None` fields, which would leave a stale "match".
#[diesel(treat_none_as_null = true)]
struct Row {
    worker_id: String,
    last_seen_at: Timestamp,
    version: String,
    egress_ip: Option<String>,
}

impl From<Row> for WorkerStatus {
    fn from(r: Row) -> Self {
        WorkerStatus {
            worker_id: r.worker_id,
            last_seen_at: r.last_seen_at.0,
            version: r.version,
            egress_ip: r.egress_ip,
        }
    }
}

#[async_trait]
impl WorkerStatusRepo for Store {
    async fn heartbeat_worker(
        &self,
        worker_id: &str,
        version: &str,
        egress_ip: Option<&str>,
    ) -> Result<()> {
        let row = Row {
            worker_id: worker_id.to_string(),
            last_seen_at: Timestamp(Utc::now()),
            version: version.to_string(),
            egress_ip: egress_ip.map(str::to_string),
        };
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(worker_status::table)
                        .values(&row)
                        .on_conflict(worker_status::worker_id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sq| {
                    diesel::insert_into(worker_status::table)
                        .values(&row)
                        .on_conflict(worker_status::worker_id)
                        .do_update()
                        .set(&row)
                        .execute(sq)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn latest_worker_status(&self) -> Result<Option<WorkerStatus>> {
        self.with_conn(move |conn| {
            let row: Option<Row> = worker_status::table
                .order(worker_status::last_seen_at.desc())
                .limit(1)
                .select(Row::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(WorkerStatus::from))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("workerstatus").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn heartbeat_upserts_and_latest_reads_back() {
        let store = temp_store().await;
        assert!(
            store.latest_worker_status().await.unwrap().is_none(),
            "none before any beat"
        );

        store.heartbeat_worker("w1", "0.0.1", None).await.unwrap();
        let s = store.latest_worker_status().await.unwrap().unwrap();
        assert_eq!(s.worker_id, "w1");
        assert_eq!(s.version, "0.0.1");
        assert_eq!(s.egress_ip, None);
        assert!(
            s.is_fresh(chrono::Duration::seconds(30)),
            "just-written beat is fresh"
        );

        // A second beat upserts the same row (no duplicate).
        store
            .heartbeat_worker("w1", "0.0.2", Some("203.0.113.7"))
            .await
            .unwrap();
        let s = store.latest_worker_status().await.unwrap().unwrap();
        assert_eq!(s.version, "0.0.2", "upsert updates in place");
        assert_eq!(s.egress_ip.as_deref(), Some("203.0.113.7"));

        // A beat that observed nothing clears the old IP: no stale "match".
        store.heartbeat_worker("w1", "0.0.2", None).await.unwrap();
        let s = store.latest_worker_status().await.unwrap().unwrap();
        assert_eq!(s.egress_ip, None, "the heartbeat says what is seen now");
    }

    #[test]
    fn freshness_window() {
        let stale = WorkerStatus {
            worker_id: "w".into(),
            last_seen_at: Utc::now() - chrono::Duration::seconds(120),
            version: "0".into(),
            egress_ip: None,
        };
        assert!(!stale.is_fresh(chrono::Duration::seconds(30)));
        assert!(stale.is_fresh(chrono::Duration::seconds(300)));
    }
}
