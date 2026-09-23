//! Download job queue (SKADI-T-0085).
//!
//! The durable interface between the main daemon and the VPN-isolated download
//! worker (SKADI-I-0013). The daemon's `DbDownloader` `enqueue`s a `queued` row;
//! the worker `claim_next`s it, drives librqbit, and writes progress/completion
//! back. Neither side calls the other — both just reach this table — so the
//! queue is the crash-recoverable record of every in-flight transfer.
//!
//! `v1` claim is deliberately simple (single worker assumed): [`claim_next`]
//! flips the oldest `queued` row to `downloading`, guarded by a status check so
//! a re-claim can't double-grab. A claim **lease** (SKADI-T-0214) — refreshed by
//! the worker's per-tick [`heartbeat`] and swept by [`reclaim_expired_downloads`]
//! — recovers rows a crashed worker left `downloading`.
//!
//! [`heartbeat`]: DownloadJobRepo::heartbeat
//! [`reclaim_expired_downloads`]: DownloadJobRepo::reclaim_expired_downloads
//!
//! [`claim_next`]: DownloadJobRepo::claim_next

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::DualConnection;
use diesel_dualdb::types::{Json, Timestamp};

use skadi_core::{AppError, Result};

use crate::schema::downloads;
use crate::{Store, db_err};

/// Lifecycle state of a download job. Walks `Queued → Downloading →
/// Completed|Error`; teardown is `RemoveRequested → Removed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadJobStatus {
    Queued,
    Downloading,
    /// Operator-paused (SKADI-T-0168). The worker pauses librqbit; the row is not
    /// claimed again until `resume()` flips it back to `queued`.
    Paused,
    /// Downloading but **stuck**: no progress and no peers for the configured
    /// stall timeout (SKADI-T-0213). Still actively tracked — distinct from `Error`
    /// — and flips back to `Downloading` when transfer resumes.
    Stalled,
    Completed,
    /// Finished downloading and **stopped seeding** because a seed-ratio/seed-time
    /// limit was reached (SKADI-T-0210). Terminal: the data is kept but the worker
    /// no longer seeds or re-attaches it.
    Seeded,
    Error,
    RemoveRequested,
    Removed,
}

impl DownloadJobStatus {
    /// The on-disk string (the `status` column value).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadJobStatus::Queued => "queued",
            DownloadJobStatus::Downloading => "downloading",
            DownloadJobStatus::Paused => "paused",
            DownloadJobStatus::Stalled => "stalled",
            DownloadJobStatus::Completed => "completed",
            DownloadJobStatus::Seeded => "seeded",
            DownloadJobStatus::Error => "error",
            DownloadJobStatus::RemoveRequested => "remove_requested",
            DownloadJobStatus::Removed => "removed",
        }
    }

    /// Parse a stored `status` value.
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => DownloadJobStatus::Queued,
            "downloading" => DownloadJobStatus::Downloading,
            "paused" => DownloadJobStatus::Paused,
            "stalled" => DownloadJobStatus::Stalled,
            "completed" => DownloadJobStatus::Completed,
            "seeded" => DownloadJobStatus::Seeded,
            "error" => DownloadJobStatus::Error,
            "remove_requested" => DownloadJobStatus::RemoveRequested,
            "removed" => DownloadJobStatus::Removed,
            other => {
                return Err(AppError::Internal(format!(
                    "unknown download status {other:?}"
                )));
            }
        })
    }
}

/// A request to enqueue a new download. The repo generates the id + timestamps
/// and sets `status = queued`, `progress/total = 0`, `files = []`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDownloadJob {
    /// The acquirable this download is for (opaque ref the domain understands).
    pub acquirable_ref: String,
    /// A magnet URI or an http(s) URL to a `.torrent`.
    pub source: String,
    /// Optional category (the importer uses it to file the result).
    pub category: Option<String>,
    /// Where the worker downloads + seeds (librqbit output folder). `None` ⇒
    /// the worker uses its configured default download dir.
    pub incomplete_dir: Option<String>,
    /// Where finished files are hardlinked on completion (the clean "done"
    /// view). `None` ⇒ the worker reports the incomplete paths directly.
    pub complete_dir: Option<String>,
}

/// Live progress written by the worker while a job is `downloading`. The metric
/// fields are `Some` only while the torrent is live (SKADI-T-0166); `None` clears
/// stale values when it isn't (e.g. initializing / paused).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DownloadProgress {
    pub progress_bytes: i64,
    pub total_bytes: i64,
    /// The resolved torrent info-hash, once known.
    pub info_hash: Option<String>,
    /// Download / upload rate in bytes per second.
    pub down_speed_bps: Option<i64>,
    pub up_speed_bps: Option<i64>,
    /// Total bytes uploaded so far (for the seed ratio = uploaded / downloaded).
    pub uploaded_bytes: Option<i64>,
    /// Connected (live) peers, and total peers seen this session.
    pub peers: Option<i32>,
    pub peers_seen: Option<i32>,
    /// Estimated seconds remaining, when librqbit reports it.
    pub eta_seconds: Option<i64>,
    /// The client's own transfer state this tick (SKADI-T-0394): `initializing`,
    /// `live`, `paused`, `error`. Lets the hunter tell "the client has not started
    /// this yet" from "live but finding no peers" — only the latter can stall.
    pub client_state: Option<String>,
}

/// One row in the `downloads` queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadJob {
    pub id: String,
    pub acquirable_ref: String,
    pub source: String,
    pub category: Option<String>,
    pub status: DownloadJobStatus,
    pub info_hash: Option<String>,
    pub progress_bytes: i64,
    pub total_bytes: i64,
    /// Live torrent metrics (SKADI-T-0166); `None` until the worker reports them.
    pub down_speed_bps: Option<i64>,
    pub up_speed_bps: Option<i64>,
    pub uploaded_bytes: Option<i64>,
    pub peers: Option<i32>,
    pub peers_seen: Option<i32>,
    pub eta_seconds: Option<i64>,
    /// Absolute output file paths (populated on completion; the importer
    /// hardlinks these).
    pub files: Vec<String>,
    pub error: Option<String>,
    pub worker_id: Option<String>,
    /// Whether a removal should delete the downloaded data (vs keep it for
    /// seeding/import). Only meaningful once `status = remove_requested`.
    pub delete_data: bool,
    /// Where the worker downloads + seeds (librqbit output folder).
    pub incomplete_dir: Option<String>,
    /// Where finished files are hardlinked on completion.
    pub complete_dir: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the job finished downloading and began seeding (SKADI-T-0210); `None`
    /// until completion. Drives the seed-time limit (`seeded_secs = now - completed_at`).
    pub completed_at: Option<DateTime<Utc>>,
    /// When the worker's claim lease lapses (SKADI-T-0214); refreshed every tick
    /// while tracked. `None` until the first heartbeat after a claim. A lapsed lease
    /// means the worker died and the row can be reclaimed.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// What the download **client** says the transfer is doing right now —
    /// `initializing` (queued behind the client's hash/init work, or fetching
    /// magnet metadata), `live`, `paused`, `error` (SKADI-T-0394). Distinct from
    /// `status`, which is skadi's own view: a row can be `downloading` with the
    /// client still `initializing`, which is exactly the case the stall watch used
    /// to mistake for a dead transfer. `None` when the client reports nothing.
    pub client_state: Option<String>,
}

/// The daemon enqueues + reads; the worker claims, writes progress, and tears
/// down. Both sides go through this one repo.
#[async_trait]
pub trait DownloadJobRepo: Send + Sync {
    /// Insert a new `queued` job; returns the stored row (with generated id).
    async fn enqueue(&self, req: &NewDownloadJob) -> Result<DownloadJob>;
    /// Fetch one job by id.
    async fn get_download(&self, id: &str) -> Result<Option<DownloadJob>>;
    /// Atomically claim the oldest `queued` job for `worker_id`, flipping it to
    /// `downloading`. Returns `None` when nothing is queued. (Guarded by a
    /// status check so a concurrent claim can't double-grab.)
    async fn claim_next(&self, worker_id: &str) -> Result<Option<DownloadJob>>;
    /// Count rows currently `downloading` — the active-transfer count the worker
    /// bounds against `worker.max_active` (SKADI-T-0212).
    async fn count_active_downloads(&self) -> Result<i64>;
    /// Flip a download between `downloading` and `stalled` (SKADI-T-0213). Guarded
    /// (only the matching source status flips), so it never disturbs a row that has
    /// since completed/paused/errored.
    async fn set_download_stalled(&self, id: &str, stalled: bool) -> Result<()>;
    /// Extend a tracked (`downloading`/`stalled`) row's claim lease to `now +
    /// lease_secs` (SKADI-T-0214). The worker calls this on claim and every tick;
    /// `lease_secs` is normally `worker.lease_secs` (negative is allowed only to
    /// force an already-expired lease in tests).
    async fn heartbeat(&self, id: &str, lease_secs: i64) -> Result<()>;
    /// Return every tracked row whose lease has **lapsed** to `queued` so it gets
    /// re-claimed (SKADI-T-0214) — precise crash recovery that, unlike
    /// [`reclaim_orphaned_downloads`](Self::reclaim_orphaned_downloads), never
    /// touches a still-heartbeating row. Returns how many were reclaimed.
    async fn reclaim_expired_downloads(&self) -> Result<usize>;
    /// Update live progress (also records info_hash/total) while downloading.
    async fn update_progress(&self, id: &str, progress: &DownloadProgress) -> Result<()>;
    /// Mark a job `completed` with its absolute output file paths (also stamps
    /// `completed_at`, the seed-start time).
    async fn mark_complete(&self, id: &str, files: &[String]) -> Result<()>;
    /// Mark a seeding (`completed`) job `seeded` — a seed-ratio/seed-time limit was
    /// reached, so the worker stopped seeding it (SKADI-T-0210). Terminal.
    async fn mark_seeded(&self, id: &str) -> Result<()>;
    /// Mark a job `error` with a reason.
    async fn mark_error(&self, id: &str, reason: &str) -> Result<()>;
    /// Request removal (`remove_requested`); the worker actions it then calls
    /// [`mark_removed`](Self::mark_removed). `delete_data` removes the files too.
    async fn request_remove(&self, id: &str, delete_data: bool) -> Result<()>;
    /// Pause an active (`queued`/`downloading`) job → `paused` (SKADI-T-0168). The
    /// worker pauses librqbit and stops claiming it until resumed.
    async fn request_pause(&self, id: &str) -> Result<()>;
    /// Resume a `paused` job → `queued` so the worker re-claims and re-tracks it.
    async fn resume(&self, id: &str) -> Result<()>;
    /// Reset every `downloading` row back to `queued` — called once at worker
    /// startup so jobs orphaned by a previous worker instance get re-claimed and
    /// re-tracked (SKADI-T-0168). Returns how many were reset.
    async fn reclaim_orphaned_downloads(&self) -> Result<usize>;
    /// Un-claim the active rows beyond `keep`, oldest-claimed first, returning the
    /// jobs released (SKADI-T-0489). Used to bring a session restore back under
    /// `worker.max_active`: librqbit restarts every persisted torrent regardless of
    /// the cap, so the worker can come up running far more transfers than it is
    /// configured for.
    ///
    /// Keeps the **oldest** active rows — the same FIFO order `claim_next` uses —
    /// so the queue's discipline is unchanged and the jobs released are the ones
    /// that would have been claimed last anyway.
    async fn release_active_beyond(&self, keep: usize) -> Result<Vec<DownloadJob>>;
    /// Jobs awaiting worker teardown (`status = remove_requested`).
    async fn list_remove_requested(&self) -> Result<Vec<DownloadJob>>;
    /// All jobs, newest first (for a queue/activity view and tests).
    async fn list_downloads(&self) -> Result<Vec<DownloadJob>>;

    /// Downloads in any of `statuses`, filtered **in the query**
    /// (SKADI-T-0494).
    ///
    /// The downloads page wants the handful of live transfers, but the table is
    /// dominated by finished and removed rows that accumulate forever. Loading
    /// all of them to discard most in the handler is the scan this avoids.
    ///
    /// Deliberately a status filter and not a `LIMIT`: the handler collapses
    /// duplicate rows by `info_hash` across the *whole* set, so a row's duplicate
    /// may sit anywhere in the ordering. A `LIMIT` pushed down would hand the
    /// handler a page it cannot correctly deduplicate.
    async fn list_downloads_with_status(
        &self,
        statuses: &[DownloadJobStatus],
    ) -> Result<Vec<DownloadJob>>;
    /// Mark a job fully `removed` (the worker has torn it down).
    async fn mark_removed(&self, id: &str) -> Result<()>;
}

/// Parse a stored `files` JSON array back into abs paths (tolerant: a malformed
/// value yields an empty list rather than failing a read).
fn files_from_value(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Serialize the abs-path file list for storage as a JSON array.
fn files_to_json(files: &[String]) -> serde_json::Value {
    serde_json::Value::Array(
        files
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    )
}

/// The stored row. `files` is the portable `Json` type; timestamps are portable
/// `Timestamp`.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = downloads)]
struct Row {
    id: String,
    acquirable_ref: String,
    source: String,
    category: Option<String>,
    status: String,
    info_hash: Option<String>,
    progress_bytes: i64,
    total_bytes: i64,
    down_speed_bps: Option<i64>,
    up_speed_bps: Option<i64>,
    uploaded_bytes: Option<i64>,
    peers: Option<i32>,
    peers_seen: Option<i32>,
    eta_seconds: Option<i64>,
    files: Json<serde_json::Value>,
    error: Option<String>,
    worker_id: Option<String>,
    delete_data: bool,
    incomplete_dir: Option<String>,
    complete_dir: Option<String>,
    created_at: Timestamp,
    updated_at: Timestamp,
    completed_at: Option<Timestamp>,
    lease_expires_at: Option<Timestamp>,
    client_state: Option<String>,
}

impl TryFrom<Row> for DownloadJob {
    type Error = AppError;
    fn try_from(r: Row) -> Result<Self> {
        Ok(DownloadJob {
            id: r.id,
            acquirable_ref: r.acquirable_ref,
            source: r.source,
            category: r.category,
            status: DownloadJobStatus::parse(&r.status)?,
            info_hash: r.info_hash,
            progress_bytes: r.progress_bytes,
            total_bytes: r.total_bytes,
            down_speed_bps: r.down_speed_bps,
            up_speed_bps: r.up_speed_bps,
            uploaded_bytes: r.uploaded_bytes,
            peers: r.peers,
            peers_seen: r.peers_seen,
            eta_seconds: r.eta_seconds,
            files: files_from_value(&r.files.0),
            error: r.error,
            worker_id: r.worker_id,
            delete_data: r.delete_data,
            incomplete_dir: r.incomplete_dir,
            complete_dir: r.complete_dir,
            created_at: r.created_at.0,
            updated_at: r.updated_at.0,
            completed_at: r.completed_at.map(|t| t.0),
            lease_expires_at: r.lease_expires_at.map(|t| t.0),
            client_state: r.client_state,
        })
    }
}

/// Synchronous single-row load, shared by the methods that re-read after a write.
fn load_one(conn: &mut DualConnection, id: &str) -> Result<Option<DownloadJob>> {
    let row: Option<Row> = downloads::table
        .find(id)
        .select(Row::as_select())
        .first(conn)
        .optional()
        .map_err(db_err)?;
    row.map(DownloadJob::try_from).transpose()
}

#[async_trait]
impl DownloadJobRepo for Store {
    async fn enqueue(&self, req: &NewDownloadJob) -> Result<DownloadJob> {
        let now = Utc::now();
        let row = Row {
            id: uuid::Uuid::new_v4().to_string(),
            acquirable_ref: req.acquirable_ref.clone(),
            source: req.source.clone(),
            category: req.category.clone(),
            status: DownloadJobStatus::Queued.as_str().to_string(),
            info_hash: None,
            progress_bytes: 0,
            total_bytes: 0,
            down_speed_bps: None,
            up_speed_bps: None,
            uploaded_bytes: None,
            peers: None,
            peers_seen: None,
            eta_seconds: None,
            files: Json(serde_json::Value::Array(Vec::new())),
            error: None,
            worker_id: None,
            delete_data: false,
            incomplete_dir: req.incomplete_dir.clone(),
            complete_dir: req.complete_dir.clone(),
            created_at: Timestamp(now),
            updated_at: Timestamp(now),
            completed_at: None,
            lease_expires_at: None,
            client_state: None,
        };
        let id = row.id.clone();
        let acquirable_ref = req.acquirable_ref.clone();
        self.with_conn(move |conn| {
            // Dedup (SKADI-T-0167): if a non-terminal job already exists for this
            // acquirable, return it instead of enqueuing a second concurrent
            // download. A sweep that re-acquires a still-in-flight item (its status
            // hasn't caught up yet) thus becomes an idempotent no-op rather than a
            // duplicate of the same torrent. The single-worker queue makes the
            // check-then-insert race-free enough; a UNIQUE-ish guard can come later.
            let active: Option<String> = downloads::table
                .filter(downloads::acquirable_ref.eq(&acquirable_ref))
                .filter(downloads::status.eq_any([
                    DownloadJobStatus::Queued.as_str(),
                    DownloadJobStatus::Downloading.as_str(),
                    DownloadJobStatus::Paused.as_str(),
                ]))
                .order(downloads::created_at.asc())
                .select(downloads::id)
                .first(conn)
                .optional()
                .map_err(db_err)?;
            if let Some(existing) = active {
                return load_one(conn, &existing)?
                    .ok_or_else(|| AppError::Internal("dedup: active download vanished".into()));
            }
            diesel::insert_into(downloads::table)
                .values(&row)
                .execute(conn)
                .map_err(db_err)?;
            load_one(conn, &id)?
                .ok_or_else(|| AppError::Internal("download row vanished after insert".into()))
        })
        .await
    }

    async fn get_download(&self, id: &str) -> Result<Option<DownloadJob>> {
        let id = id.to_string();
        self.with_conn(move |conn| load_one(conn, &id)).await
    }

    async fn claim_next(&self, worker_id: &str) -> Result<Option<DownloadJob>> {
        let worker_id = worker_id.to_string();
        self.with_conn(move |conn| {
            // Oldest queued id, then a status-guarded flip — portable + safe
            // enough for the single-worker v1 (the guard prevents a double-grab
            // if raced). All on one connection.
            let next: Option<String> = downloads::table
                .filter(downloads::status.eq(DownloadJobStatus::Queued.as_str()))
                .order(downloads::created_at.asc())
                .select(downloads::id)
                .first(conn)
                .optional()
                .map_err(db_err)?;
            let Some(id) = next else { return Ok(None) };
            let n = diesel::update(
                downloads::table
                    .find(&id)
                    .filter(downloads::status.eq(DownloadJobStatus::Queued.as_str())),
            )
            .set((
                downloads::status.eq(DownloadJobStatus::Downloading.as_str()),
                downloads::worker_id.eq(&worker_id),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)?;
            if n == 0 {
                return Ok(None);
            }
            load_one(conn, &id)
        })
        .await
    }

    async fn count_active_downloads(&self) -> Result<i64> {
        self.with_conn(|conn| {
            downloads::table
                // `Stalled` counts too (SKADI-T-0513). A stalled transfer is still
                // loaded in the client — the status only means "no progress and no
                // peers right now", and it flips back to `Downloading` on its own
                // when transfer resumes. Counting only `Downloading` meant every
                // stalled torrent freed a slot it was still occupying, so the
                // worker ran `max_active + <stalled>` transfers: on a box sized for
                // its configured limit, the stalled ones are exactly the torrents
                // most likely to be sitting there for hours.
                //
                // `Paused` deliberately does not count: the operator asked for it
                // to stop, and it is not claimed again until `resume()`.
                .filter(
                    downloads::status
                        .eq(DownloadJobStatus::Downloading.as_str())
                        .or(downloads::status.eq(DownloadJobStatus::Stalled.as_str())),
                )
                .count()
                .get_result(conn)
                .map_err(db_err)
        })
        .await
    }

    async fn set_download_stalled(&self, id: &str, stalled: bool) -> Result<()> {
        let id = id.to_string();
        let (from, to) = if stalled {
            (DownloadJobStatus::Downloading, DownloadJobStatus::Stalled)
        } else {
            (DownloadJobStatus::Stalled, DownloadJobStatus::Downloading)
        };
        self.with_conn(move |conn| {
            diesel::update(
                downloads::table
                    .find(&id)
                    .filter(downloads::status.eq(from.as_str())),
            )
            .set((
                downloads::status.eq(to.as_str()),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn heartbeat(&self, id: &str, lease_secs: i64) -> Result<()> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            let until = Timestamp(Utc::now() + chrono::Duration::seconds(lease_secs));
            diesel::update(
                downloads::table.find(&id).filter(
                    downloads::status
                        .eq(DownloadJobStatus::Downloading.as_str())
                        .or(downloads::status.eq(DownloadJobStatus::Stalled.as_str())),
                ),
            )
            .set(downloads::lease_expires_at.eq(until))
            .execute(conn)
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn reclaim_expired_downloads(&self) -> Result<usize> {
        self.with_conn(|conn| {
            // Only rows with a *set* lease that's now in the past — a freshly-claimed
            // row whose first heartbeat hasn't landed (lease still NULL) is left alone.
            let now = Timestamp(Utc::now());
            diesel::update(
                downloads::table
                    .filter(
                        downloads::status
                            .eq(DownloadJobStatus::Downloading.as_str())
                            .or(downloads::status.eq(DownloadJobStatus::Stalled.as_str())),
                    )
                    .filter(downloads::lease_expires_at.is_not_null())
                    .filter(downloads::lease_expires_at.lt(now)),
            )
            .set((
                downloads::status.eq(DownloadJobStatus::Queued.as_str()),
                downloads::worker_id.eq(None::<String>),
                downloads::lease_expires_at.eq(None::<Timestamp>),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)
        })
        .await
    }

    async fn update_progress(&self, id: &str, progress: &DownloadProgress) -> Result<()> {
        let id = id.to_string();
        let progress = progress.clone();
        self.with_conn(move |conn| {
            diesel::update(downloads::table.find(&id))
                .set((
                    downloads::progress_bytes.eq(progress.progress_bytes),
                    downloads::total_bytes.eq(progress.total_bytes),
                    downloads::info_hash.eq(progress.info_hash),
                    downloads::down_speed_bps.eq(progress.down_speed_bps),
                    downloads::up_speed_bps.eq(progress.up_speed_bps),
                    downloads::uploaded_bytes.eq(progress.uploaded_bytes),
                    downloads::peers.eq(progress.peers),
                    downloads::peers_seen.eq(progress.peers_seen),
                    downloads::client_state.eq(progress.client_state),
                    downloads::eta_seconds.eq(progress.eta_seconds),
                    downloads::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn mark_complete(&self, id: &str, files: &[String]) -> Result<()> {
        let id = id.to_string();
        let files = Json(files_to_json(files));
        self.with_conn(move |conn| {
            let now = Timestamp(Utc::now());
            diesel::update(downloads::table.find(&id))
                .set((
                    downloads::status.eq(DownloadJobStatus::Completed.as_str()),
                    downloads::files.eq(files),
                    downloads::updated_at.eq(now),
                    // Stamp the seed start so the worker can enforce seed-time limits
                    // (SKADI-T-0210).
                    downloads::completed_at.eq(now),
                ))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn mark_seeded(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            // Only a Completed (seeding) row becomes Seeded — guards against racing a
            // concurrent remove/pause.
            diesel::update(
                downloads::table
                    .find(&id)
                    .filter(downloads::status.eq(DownloadJobStatus::Completed.as_str())),
            )
            .set((
                downloads::status.eq(DownloadJobStatus::Seeded.as_str()),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn mark_error(&self, id: &str, reason: &str) -> Result<()> {
        let id = id.to_string();
        let reason = reason.to_string();
        self.with_conn(move |conn| {
            diesel::update(downloads::table.find(&id))
                .set((
                    downloads::status.eq(DownloadJobStatus::Error.as_str()),
                    downloads::error.eq(reason),
                    downloads::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn request_remove(&self, id: &str, delete_data: bool) -> Result<()> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            // The list view collapses duplicate rows sharing an info_hash (legacy
            // re-acquires before enqueue-dedup), so the single id the UI sends can
            // have managed siblings. Sweep them all to RemoveRequested — otherwise
            // a surviving sibling re-collapses into the same visible row and Remove
            // looks like a no-op (SKADI-T-0171).
            let hash: Option<String> = downloads::table
                .find(&id)
                .select(downloads::info_hash)
                .first::<Option<String>>(conn)
                .optional()
                .map_err(db_err)?
                .flatten();
            let managed = [
                DownloadJobStatus::Queued.as_str(),
                DownloadJobStatus::Downloading.as_str(),
                DownloadJobStatus::Paused.as_str(),
                DownloadJobStatus::Completed.as_str(),
            ];
            match hash {
                Some(h) => diesel::update(
                    downloads::table
                        .filter(downloads::info_hash.eq(h))
                        .filter(downloads::status.eq_any(managed)),
                )
                .set((
                    downloads::status.eq(DownloadJobStatus::RemoveRequested.as_str()),
                    downloads::delete_data.eq(delete_data),
                    downloads::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?,
                None => diesel::update(downloads::table.find(&id))
                    .set((
                        downloads::status.eq(DownloadJobStatus::RemoveRequested.as_str()),
                        downloads::delete_data.eq(delete_data),
                        downloads::updated_at.eq(Timestamp(Utc::now())),
                    ))
                    .execute(conn)
                    .map_err(db_err)?,
            };
            Ok(())
        })
        .await
    }

    async fn request_pause(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            // Only an active job can be paused (guarded so we never resurrect a
            // terminal/removed row).
            diesel::update(downloads::table.find(&id).filter(downloads::status.eq_any([
                DownloadJobStatus::Queued.as_str(),
                DownloadJobStatus::Downloading.as_str(),
            ])))
            .set((
                downloads::status.eq(DownloadJobStatus::Paused.as_str()),
                downloads::down_speed_bps.eq(None::<i64>),
                downloads::up_speed_bps.eq(None::<i64>),
                downloads::eta_seconds.eq(None::<i64>),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn resume(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            diesel::update(
                downloads::table
                    .find(&id)
                    .filter(downloads::status.eq(DownloadJobStatus::Paused.as_str())),
            )
            .set((
                downloads::status.eq(DownloadJobStatus::Queued.as_str()),
                downloads::worker_id.eq(None::<String>),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn reclaim_orphaned_downloads(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let n = diesel::update(
                downloads::table
                    .filter(downloads::status.eq(DownloadJobStatus::Downloading.as_str())),
            )
            .set((
                downloads::status.eq(DownloadJobStatus::Queued.as_str()),
                downloads::worker_id.eq(None::<String>),
                downloads::updated_at.eq(Timestamp(Utc::now())),
            ))
            .execute(conn)
            .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn release_active_beyond(&self, keep: usize) -> Result<Vec<DownloadJob>> {
        self.with_conn(move |conn| {
            let rows: Vec<Row> = downloads::table
                .filter(
                    downloads::status
                        .eq(DownloadJobStatus::Downloading.as_str())
                        .or(downloads::status.eq(DownloadJobStatus::Stalled.as_str())),
                )
                // Same ordering as `claim_next`, so "beyond the cap" means the same
                // thing here as it does there.
                .order((downloads::created_at.asc(), downloads::id.asc()))
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            let jobs: Vec<DownloadJob> = rows
                .into_iter()
                .skip(keep)
                .map(DownloadJob::try_from)
                .collect::<Result<_>>()?;
            for job in &jobs {
                diesel::update(downloads::table.find(&job.id))
                    .set((
                        downloads::status.eq(DownloadJobStatus::Queued.as_str()),
                        downloads::worker_id.eq(None::<String>),
                        downloads::lease_expires_at.eq(None::<Timestamp>),
                        downloads::updated_at.eq(Timestamp(Utc::now())),
                    ))
                    .execute(conn)
                    .map_err(db_err)?;
            }
            Ok(jobs)
        })
        .await
    }

    async fn list_remove_requested(&self) -> Result<Vec<DownloadJob>> {
        self.with_conn(|conn| {
            let rows: Vec<Row> = downloads::table
                .filter(downloads::status.eq(DownloadJobStatus::RemoveRequested.as_str()))
                .order(downloads::created_at.asc())
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(DownloadJob::try_from).collect()
        })
        .await
    }

    async fn list_downloads(&self) -> Result<Vec<DownloadJob>> {
        self.with_conn(|conn| {
            // Tiebreak on the unique id (SKADI-T-0290): rows enqueued in the same
            // instant share `created_at`, and `ORDER BY created_at` alone returns
            // ties in an arbitrary, query-to-query-unstable order — which makes the
            // fast-polling downloads UI visibly reorder ("bounce") every refresh.
            let rows: Vec<Row> = downloads::table
                .order((downloads::created_at.desc(), downloads::id.asc()))
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(DownloadJob::try_from).collect()
        })
        .await
    }

    async fn list_downloads_with_status(
        &self,
        statuses: &[DownloadJobStatus],
    ) -> Result<Vec<DownloadJob>> {
        let wanted: Vec<String> = statuses.iter().map(|s| s.as_str().to_string()).collect();
        self.with_conn(move |conn| {
            // Same order and tiebreak as `list_downloads` (SKADI-T-0290): a
            // filtered list that ordered differently would make the fast-polling
            // downloads UI bounce.
            let rows: Vec<Row> = downloads::table
                .filter(downloads::status.eq_any(&wanted))
                .order((downloads::created_at.desc(), downloads::id.asc()))
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(DownloadJob::try_from).collect()
        })
        .await
    }

    async fn mark_removed(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            diesel::update(downloads::table.find(&id))
                .set((
                    downloads::status.eq(DownloadJobStatus::Removed.as_str()),
                    downloads::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Store {
        // A UUID (not a timestamp) guarantees a distinct DB per test even when
        // parallel tests start within the same clock tick — otherwise two tests
        // share a DB and `claim_next`'s "oldest queued" grabs a foreign row.
        let path = skadi_core::unique_temp_path("downloads").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    fn req(refr: &str, source: &str) -> NewDownloadJob {
        NewDownloadJob {
            acquirable_ref: refr.into(),
            source: source.into(),
            category: Some("movies".into()),
            incomplete_dir: Some("/mnt/storage/skadi-torrent/incomplete".into()),
            complete_dir: Some("/mnt/storage/skadi-torrent/complete".into()),
        }
    }

    #[tokio::test]
    async fn enqueue_then_get_round_trips() {
        let store = temp_store().await;
        let job = store.enqueue(&req("ref-1", "magnet:?x")).await.unwrap();
        assert_eq!(job.status, DownloadJobStatus::Queued);
        assert_eq!(job.acquirable_ref, "ref-1");
        assert_eq!(job.category.as_deref(), Some("movies"));
        assert_eq!(job.progress_bytes, 0);
        assert!(job.files.is_empty());
        assert!(job.worker_id.is_none());
        assert_eq!(
            job.incomplete_dir.as_deref(),
            Some("/mnt/storage/skadi-torrent/incomplete")
        );
        assert_eq!(
            job.complete_dir.as_deref(),
            Some("/mnt/storage/skadi-torrent/complete")
        );

        let fetched = store.get_download(&job.id).await.unwrap().unwrap();
        assert_eq!(fetched, job);
        assert!(store.get_download("nope").await.unwrap().is_none());
    }

    /// The downloads page's status filter runs in SQL now (SKADI-T-0494). It has
    /// to select exactly what the handler's `matches!` used to, or the page
    /// silently gains or loses transfers.
    #[tokio::test]
    async fn the_status_filter_selects_the_same_rows_the_handler_used_to() {
        let store = temp_store().await;
        let queued = store.enqueue(&req("q", "magnet:?a")).await.unwrap();
        let downloading = store.enqueue(&req("d", "magnet:?b")).await.unwrap();
        let done = store.enqueue(&req("c", "magnet:?c")).await.unwrap();
        let gone = store.enqueue(&req("r", "magnet:?d")).await.unwrap();

        // downloading, completed, removed; `queued` is left as enqueued.
        store.claim_next("w").await.unwrap().unwrap(); // queued -> downloading
        store.claim_next("w").await.unwrap().unwrap();
        store.claim_next("w").await.unwrap().unwrap();
        store.mark_complete(&done.id, &["/x".into()]).await.unwrap();
        store.mark_removed(&gone.id).await.unwrap();

        let wanted = [
            DownloadJobStatus::Queued,
            DownloadJobStatus::Downloading,
            DownloadJobStatus::Paused,
            DownloadJobStatus::Stalled,
            DownloadJobStatus::Completed,
        ];
        let page = store.list_downloads_with_status(&wanted).await.unwrap();

        // Exactly the rows the old in-handler filter kept.
        let expected: Vec<String> = store
            .list_downloads()
            .await
            .unwrap()
            .into_iter()
            .filter(|j| wanted.contains(&j.status))
            .map(|j| j.id)
            .collect();
        assert_eq!(
            page.iter().map(|j| j.id.clone()).collect::<Vec<_>>(),
            expected
        );

        // And the removed row is genuinely excluded — the point of the filter.
        assert!(!page.iter().any(|j| j.id == gone.id));
        assert!(page.iter().any(|j| j.id == queued.id));
        assert!(page.iter().any(|j| j.id == downloading.id));
        assert!(page.iter().any(|j| j.id == done.id));
    }

    #[tokio::test]
    async fn set_download_stalled_flips_downloading_and_back() {
        let store = temp_store().await;
        let job = store.enqueue(&req("st", "magnet:?x")).await.unwrap();
        store.claim_next("w").await.unwrap().unwrap(); // -> downloading

        store.set_download_stalled(&job.id, true).await.unwrap();
        assert_eq!(
            store.get_download(&job.id).await.unwrap().unwrap().status,
            DownloadJobStatus::Stalled
        );
        // A stalled row IS counted as active (SKADI-T-0513): the torrent is still
        // loaded in the client, so it occupies the slot `max_active` bounds. This
        // asserted 0 before, which is what let the worker exceed its cap by one per
        // stalled transfer.
        assert_eq!(store.count_active_downloads().await.unwrap(), 1);

        store.set_download_stalled(&job.id, false).await.unwrap();
        assert_eq!(
            store.get_download(&job.id).await.unwrap().unwrap().status,
            DownloadJobStatus::Downloading
        );

        // Guarded: stalling a non-downloading row is a no-op (e.g. after complete).
        store.mark_complete(&job.id, &["/x".into()]).await.unwrap();
        store.set_download_stalled(&job.id, true).await.unwrap();
        assert_eq!(
            store.get_download(&job.id).await.unwrap().unwrap().status,
            DownloadJobStatus::Completed
        );
    }

    #[tokio::test]
    async fn heartbeat_and_reclaim_expired_only_touch_lapsed_leases() {
        let store = temp_store().await;
        let fresh = store.enqueue(&req("fresh", "magnet:?a")).await.unwrap();
        let dead = store.enqueue(&req("dead", "magnet:?b")).await.unwrap();
        let pending = store.enqueue(&req("pending", "magnet:?c")).await.unwrap();
        // Claim all three → downloading (claim order = oldest first).
        for _ in 0..3 {
            store.claim_next("w").await.unwrap().unwrap();
        }
        // fresh: live lease; dead: already-expired lease (negative); pending: never
        // heartbeated (NULL lease).
        store.heartbeat(&fresh.id, 120).await.unwrap();
        store.heartbeat(&dead.id, -10).await.unwrap();

        let n = store.reclaim_expired_downloads().await.unwrap();
        assert_eq!(n, 1, "only the lapsed-lease row is reclaimed");

        let d = store.get_download(&dead.id).await.unwrap().unwrap();
        assert_eq!(d.status, DownloadJobStatus::Queued, "lapsed → requeued");
        assert_eq!(d.worker_id, None, "claim released");
        assert!(d.lease_expires_at.is_none(), "lease cleared");
        // A live lease and a not-yet-heartbeated (NULL) claim are both left alone.
        assert_eq!(
            store.get_download(&fresh.id).await.unwrap().unwrap().status,
            DownloadJobStatus::Downloading
        );
        assert_eq!(
            store
                .get_download(&pending.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DownloadJobStatus::Downloading
        );
    }

    #[tokio::test]
    async fn count_active_downloads_counts_only_downloading() {
        let store = temp_store().await;
        assert_eq!(store.count_active_downloads().await.unwrap(), 0);

        // Two queued; nothing active yet.
        let a = store.enqueue(&req("a", "magnet:?a")).await.unwrap();
        let _b = store.enqueue(&req("b", "magnet:?b")).await.unwrap();
        assert_eq!(store.count_active_downloads().await.unwrap(), 0);

        // Claim both → both downloading.
        store.claim_next("w").await.unwrap().unwrap();
        store.claim_next("w").await.unwrap().unwrap();
        assert_eq!(store.count_active_downloads().await.unwrap(), 2);

        // Completing one drops the active count (Completed/seeding isn't "active").
        store.mark_complete(&a.id, &["/x".into()]).await.unwrap();
        assert_eq!(store.count_active_downloads().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn complete_stamps_completed_at_then_mark_seeded_is_terminal() {
        let store = temp_store().await;
        let job = store.enqueue(&req("s", "magnet:?x")).await.unwrap();
        store.claim_next("w").await.unwrap().unwrap(); // -> downloading
        assert!(job.completed_at.is_none(), "not completed yet");

        // Completion stamps completed_at (the seed-start time, SKADI-T-0210).
        store
            .mark_complete(&job.id, &["/x/a.mkv".into()])
            .await
            .unwrap();
        let c = store.get_download(&job.id).await.unwrap().unwrap();
        assert_eq!(c.status, DownloadJobStatus::Completed);
        assert!(
            c.completed_at.is_some(),
            "completed_at stamped on completion"
        );

        // Reaching a seed limit → Seeded (terminal), round-trips through the status
        // column.
        store.mark_seeded(&job.id).await.unwrap();
        let s = store.get_download(&job.id).await.unwrap().unwrap();
        assert_eq!(s.status, DownloadJobStatus::Seeded);
        assert!(s.completed_at.is_some(), "completed_at preserved");

        // mark_seeded only acts on a Completed row — a second call (now Seeded) is a
        // no-op, not an error.
        store.mark_seeded(&job.id).await.unwrap();
        assert_eq!(
            store.get_download(&job.id).await.unwrap().unwrap().status,
            DownloadJobStatus::Seeded
        );
    }

    #[tokio::test]
    async fn pause_resume_and_reclaim_orphans() {
        let store = temp_store().await;
        let job = store.enqueue(&req("p", "magnet:?x")).await.unwrap();
        store.claim_next("w").await.unwrap().unwrap(); // queued -> downloading

        // Pause: downloading -> paused, and it's no longer claimable.
        store.request_pause(&job.id).await.unwrap();
        let st = |s: &Store, id: &str| {
            let id = id.to_string();
            let s = s.clone();
            async move { s.get_download(&id).await.unwrap().unwrap().status }
        };
        assert_eq!(st(&store, &job.id).await, DownloadJobStatus::Paused);
        assert!(store.claim_next("w").await.unwrap().is_none());

        // Resume: paused -> queued, claimable again.
        store.resume(&job.id).await.unwrap();
        assert_eq!(st(&store, &job.id).await, DownloadJobStatus::Queued);
        assert_eq!(store.claim_next("w").await.unwrap().unwrap().id, job.id);

        // Reclaim resets the now-downloading row back to queued (worker restart).
        assert_eq!(store.reclaim_orphaned_downloads().await.unwrap(), 1);
        assert_eq!(st(&store, &job.id).await, DownloadJobStatus::Queued);
    }

    #[tokio::test]
    async fn enqueue_dedups_active_jobs_for_the_same_acquirable() {
        let store = temp_store().await;
        // Two enqueues for the same acquirable while the first is still queued →
        // the second returns the SAME job, not a duplicate (SKADI-T-0167).
        let a = store.enqueue(&req("dup", "magnet:?1")).await.unwrap();
        let b = store.enqueue(&req("dup", "magnet:?2")).await.unwrap();
        assert_eq!(a.id, b.id, "second enqueue coalesced onto the active job");
        assert_eq!(store.list_downloads().await.unwrap().len(), 1);

        // A different acquirable is independent.
        store.enqueue(&req("other", "magnet:?3")).await.unwrap();
        assert_eq!(store.list_downloads().await.unwrap().len(), 2);

        // Once the first reaches a terminal state, the acquirable can be re-queued
        // (e.g. a genuine re-grab / upgrade) — dedup only guards *active* jobs.
        store.mark_complete(&a.id, &[]).await.unwrap();
        let c = store.enqueue(&req("dup", "magnet:?4")).await.unwrap();
        assert_ne!(c.id, a.id, "completed job no longer blocks a fresh enqueue");
        assert_eq!(store.list_downloads().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn claim_is_fifo_and_single_grab() {
        let store = temp_store().await;
        let a = store.enqueue(&req("a", "magnet:?a")).await.unwrap();
        let _b = store.enqueue(&req("b", "magnet:?b")).await.unwrap();

        let claimed = store.claim_next("w1").await.unwrap().unwrap();
        assert_eq!(claimed.id, a.id);
        assert_eq!(claimed.status, DownloadJobStatus::Downloading);
        assert_eq!(claimed.worker_id.as_deref(), Some("w1"));

        let second = store.claim_next("w1").await.unwrap().unwrap();
        assert_eq!(second.acquirable_ref, "b");

        assert!(store.claim_next("w1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn progress_complete_error_and_remove_lifecycle() {
        let store = temp_store().await;
        let job = store.enqueue(&req("ref-1", "magnet:?x")).await.unwrap();
        store.claim_next("w1").await.unwrap().unwrap();

        store
            .update_progress(
                &job.id,
                &DownloadProgress {
                    progress_bytes: 500,
                    total_bytes: 1000,
                    info_hash: Some("abc123".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let p = store.get_download(&job.id).await.unwrap().unwrap();
        assert_eq!(p.progress_bytes, 500);
        assert_eq!(p.total_bytes, 1000);
        assert_eq!(p.info_hash.as_deref(), Some("abc123"));
        assert_eq!(p.status, DownloadJobStatus::Downloading);

        store
            .mark_complete(&job.id, &["/data/downloads/x/movie.mkv".into()])
            .await
            .unwrap();
        let c = store.get_download(&job.id).await.unwrap().unwrap();
        assert_eq!(c.status, DownloadJobStatus::Completed);
        assert_eq!(c.files, vec!["/data/downloads/x/movie.mkv".to_string()]);

        store.request_remove(&job.id, true).await.unwrap();
        let pending = store.list_remove_requested().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, job.id);
        assert!(pending[0].delete_data);

        store.mark_removed(&job.id).await.unwrap();
        assert!(store.list_remove_requested().await.unwrap().is_empty());
        let r = store.get_download(&job.id).await.unwrap().unwrap();
        assert_eq!(r.status, DownloadJobStatus::Removed);

        let job2 = store.enqueue(&req("ref-2", "magnet:?y")).await.unwrap();
        store.mark_error(&job2.id, "no peers").await.unwrap();
        let e = store.get_download(&job2.id).await.unwrap().unwrap();
        assert_eq!(e.status, DownloadJobStatus::Error);
        assert_eq!(e.error.as_deref(), Some("no peers"));
    }

    #[tokio::test]
    async fn request_remove_sweeps_duplicate_rows_sharing_info_hash() {
        // Legacy re-acquires can leave several managed rows for one real torrent
        // (same info_hash). The list view collapses them into one visible row, so
        // removing the single displayed id must take its siblings with it — else
        // the row re-collapses into view and Remove looks like a no-op (T-0171).
        let store = temp_store().await;
        let hash = "deadbeefcafef00d";
        let a = store.enqueue(&req("dup-a", "magnet:?a")).await.unwrap();
        let b = store.enqueue(&req("dup-b", "magnet:?b")).await.unwrap();
        for j in [&a, &b] {
            store
                .update_progress(
                    &j.id,
                    &DownloadProgress {
                        progress_bytes: 1000,
                        total_bytes: 1000,
                        info_hash: Some(hash.into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            store.mark_complete(&j.id, &[]).await.unwrap();
        }

        // Removing just one id sweeps every managed row with the same info_hash.
        store.request_remove(&a.id, false).await.unwrap();
        let pending = store.list_remove_requested().await.unwrap();
        let ids: std::collections::HashSet<_> = pending.iter().map(|j| j.id.clone()).collect();
        assert_eq!(
            ids.len(),
            2,
            "both duplicate rows are removed, not just one"
        );
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }
}
