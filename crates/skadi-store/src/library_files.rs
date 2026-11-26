//! Storage facts of the library's files (SKADI-T-0772, design SKADI-D-0040).
//!
//! One row per file the library holds, with what the mount gives (size, space
//! on disk, links, identity), what a probe of the storage host gives (pieces)
//! and a checksum. Beside it: a history of the values that change, the files on
//! disk that no record points at, and the state of each probe.
//!
//! In the shared store, following [`crate::history`] and [`crate::item_tags`]:
//! the facts are the same for a film, an episode and an audiobook file, and a
//! row carries an opaque `(kind, acquirable_ref)` rather than a foreign key.
//!
//! This module only stores. The collectors that read the disk and the storage
//! host are other tasks of COLLIERY-I-0686.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Double};
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::{library_file_samples, library_files, library_orphans, storage_probe_state};
use crate::{Store, db_err};

/// The storage facts of one library file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFile {
    pub id: String,
    /// `movie` | `series` | `audiobook`.
    pub kind: String,
    /// The edition or episode the file belongs to.
    pub acquirable_ref: String,
    /// As Skadi sees it.
    pub path: String,
    pub dev: i64,
    pub ino: i64,
    pub size_bytes: i64,
    /// Space used on disk.
    pub disk_bytes: i64,
    /// Hard link count.
    pub links: i64,
    pub mtime: DateTime<Utc>,
    /// Extents. The last known count; current only while `pieces_at` is set.
    pub pieces: Option<i64>,
    /// `None` = to be measured: never measured, or the file changed since.
    pub pieces_at: Option<DateTime<Utc>>,
    /// The last known checksum; current only while `checksum_at` is set.
    pub checksum: Option<String>,
    /// `None` = to be computed: never computed, or the file changed since.
    pub checksum_at: Option<DateTime<Utc>>,
    /// When the mount facts were last read.
    pub stat_at: DateTime<Utc>,
    /// Set when the file was not found on disk.
    pub gone_at: Option<DateTime<Utc>>,
}

/// What one `stat` of a library file gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFileStat {
    pub kind: String,
    pub acquirable_ref: String,
    pub path: String,
    pub dev: i64,
    pub ino: i64,
    pub size_bytes: i64,
    pub disk_bytes: i64,
    pub links: i64,
    pub mtime: DateTime<Utc>,
}

/// One point of a file's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFileSample {
    pub file_id: String,
    pub at: DateTime<Utc>,
    pub size_bytes: i64,
    pub links: i64,
    pub pieces: Option<i64>,
}

/// A file under the library root that no record points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryOrphan {
    pub path: String,
    pub size_bytes: i64,
    pub mtime: DateTime<Utc>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// The totals of one kind. Sizes and counts leave out the files that are gone;
/// `gone` counts those.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryKindTotals {
    pub kind: String,
    /// Files on disk.
    pub files: i64,
    pub size_bytes: i64,
    pub disk_bytes: i64,
    /// Files with more than one hard link.
    pub linked: i64,
    /// Files with a current piece count, their bytes and their pieces: the
    /// three numbers that give pieces per GB for the measured share.
    pub measured: i64,
    pub measured_bytes: i64,
    pub pieces: i64,
    /// Files with a current checksum.
    pub checksummed: i64,
    /// Records whose file was not found.
    pub gone: i64,
}

/// Whether a probe may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeState {
    On,
    /// The probe failed and stays off until an operator clears it.
    Failed,
}

impl ProbeState {
    fn as_str(self) -> &'static str {
        match self {
            ProbeState::On => "on",
            ProbeState::Failed => "failed",
        }
    }
}

/// The recorded state of one probe kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageProbeStatus {
    pub probe: String,
    pub state: ProbeState,
    pub failed_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub cleared_at: Option<DateTime<Utc>>,
    pub cleared_by: Option<String>,
}

/// The storage facts of the library's files, their history and the orphans.
#[async_trait]
pub trait LibraryFileRepo: Send + Sync {
    /// Write what one `stat` gave, keyed by path, at `at`.
    ///
    /// A new path gets a row and its first sample. A known path is updated; a
    /// sample is written only when the size or the link count changed. The file
    /// is on disk, so `gone_at` is cleared, and an orphan row of the same path
    /// is removed: a record claims the file now.
    ///
    /// When the size, the modification time or the identity changed, the piece
    /// count and the checksum are no longer current: `pieces_at` and
    /// `checksum_at` are cleared and the values kept as the last known ones.
    async fn record_library_file_stat(
        &self,
        stat: &LibraryFileStat,
        at: DateTime<Utc>,
    ) -> Result<LibraryFile>;

    /// Write a measured piece count. A sample is written when the count differs
    /// from the last known one. `false` = no such file.
    async fn record_library_file_pieces(
        &self,
        id: &str,
        pieces: i64,
        at: DateTime<Utc>,
    ) -> Result<bool>;

    /// Write a computed checksum. `false` = no such file.
    async fn record_library_file_checksum(
        &self,
        id: &str,
        checksum: &str,
        at: DateTime<Utc>,
    ) -> Result<bool>;

    /// The file was not found on disk. Keeps the first time: a file already
    /// marked is not marked again. `false` = no such file.
    async fn mark_library_file_gone(&self, id: &str, at: DateTime<Utc>) -> Result<bool>;

    /// One file by its path.
    async fn library_file_by_path(&self, path: &str) -> Result<Option<LibraryFile>>;

    /// A file's history, oldest first.
    async fn library_file_samples(&self, file_id: &str) -> Result<Vec<LibraryFileSample>>;

    /// The files with the most pieces per GB, worst first; equal values in
    /// path order. Only files on disk with a current piece count and a size.
    async fn worst_library_files(&self, kind: Option<&str>, limit: i64)
    -> Result<Vec<LibraryFile>>;

    /// Totals per kind, in kind order.
    async fn library_totals_by_kind(&self) -> Result<Vec<LibraryKindTotals>>;

    /// The files on disk whose piece count is not current, in the order they
    /// were last read from the mount (longest ago first).
    async fn library_files_awaiting_pieces(&self, limit: i64) -> Result<Vec<LibraryFile>>;

    /// The files on disk whose checksum is not current, longest ago first.
    async fn library_files_awaiting_checksum(&self, limit: i64) -> Result<Vec<LibraryFile>>;

    /// The files on disk with a current piece count, oldest measurement first:
    /// what a probe measures again when nothing waits.
    async fn library_files_oldest_pieces(&self, limit: i64) -> Result<Vec<LibraryFile>>;

    /// A file under the library root that no record points at was seen at `at`.
    /// Keeps `first_seen` of a path already known. A path that a file row
    /// holds is not an orphan and is not recorded: `false`.
    async fn record_library_orphan(
        &self,
        path: &str,
        size_bytes: i64,
        mtime: DateTime<Utc>,
        at: DateTime<Utc>,
    ) -> Result<bool>;

    /// The orphans, largest first, then by path.
    async fn list_library_orphans(&self, limit: i64, offset: i64) -> Result<Vec<LibraryOrphan>>;

    /// Remove the orphans under the folder `under` not seen since `before`: a
    /// full pass of that folder that did not meet them means they are gone
    /// from disk. Orphans of other folders stay. Returns how many.
    async fn prune_library_orphans(&self, under: &str, before: DateTime<Utc>) -> Result<usize>;

    /// Every file row of one kind, gone ones too, in path order.
    async fn library_files_of_kind(&self, kind: &str) -> Result<Vec<LibraryFile>>;

    /// Remove a file row and its history: no record holds the file now.
    /// `false` = no such file.
    async fn delete_library_file(&self, id: &str) -> Result<bool>;
}

/// The state of each probe of the storage host.
#[async_trait]
pub trait StorageProbeRepo: Send + Sync {
    /// The recorded state of a probe. `None` = it never failed: it may run.
    async fn storage_probe_status(&self, probe: &str) -> Result<Option<StorageProbeStatus>>;

    /// The probe failed for a reason that will not fix itself. It stays failed
    /// until [`clear_storage_probe`](Self::clear_storage_probe).
    async fn fail_storage_probe(&self, probe: &str, reason: &str, at: DateTime<Utc>) -> Result<()>;

    /// An operator cleared the failure: the probe may run again. The reason and
    /// the time of the failure stay on the row as a record. `false` = the
    /// probe was not failed, and nothing was written.
    async fn clear_storage_probe(&self, probe: &str, by: &str, at: DateTime<Utc>) -> Result<bool>;
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = library_files)]
struct FileRow {
    id: String,
    kind: String,
    acquirable_ref: String,
    path: String,
    dev: i64,
    ino: i64,
    size_bytes: i64,
    disk_bytes: i64,
    links: i64,
    mtime: Timestamp,
    pieces: Option<i64>,
    pieces_at: Option<Timestamp>,
    checksum: Option<String>,
    checksum_at: Option<Timestamp>,
    stat_at: Timestamp,
    gone_at: Option<Timestamp>,
}

impl From<FileRow> for LibraryFile {
    fn from(r: FileRow) -> Self {
        LibraryFile {
            id: r.id,
            kind: r.kind,
            acquirable_ref: r.acquirable_ref,
            path: r.path,
            dev: r.dev,
            ino: r.ino,
            size_bytes: r.size_bytes,
            disk_bytes: r.disk_bytes,
            links: r.links,
            mtime: r.mtime.0,
            pieces: r.pieces,
            pieces_at: r.pieces_at.map(|t| t.0),
            checksum: r.checksum,
            checksum_at: r.checksum_at.map(|t| t.0),
            stat_at: r.stat_at.0,
            gone_at: r.gone_at.map(|t| t.0),
        }
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = library_file_samples)]
struct SampleRow {
    id: String,
    file_id: String,
    at: Timestamp,
    size_bytes: i64,
    links: i64,
    pieces: Option<i64>,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = library_orphans)]
struct OrphanRow {
    path: String,
    size_bytes: i64,
    mtime: Timestamp,
    first_seen: Timestamp,
    last_seen: Timestamp,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = storage_probe_state)]
struct ProbeRow {
    probe: String,
    state: String,
    failed_at: Option<Timestamp>,
    reason: Option<String>,
    cleared_at: Option<Timestamp>,
    cleared_by: Option<String>,
}

impl From<ProbeRow> for StorageProbeStatus {
    fn from(r: ProbeRow) -> Self {
        StorageProbeStatus {
            probe: r.probe,
            // Anything that is not `failed` lets the probe run: an unknown
            // word must not hold a probe off with no failure on record.
            state: if r.state == ProbeState::Failed.as_str() {
                ProbeState::Failed
            } else {
                ProbeState::On
            },
            failed_at: r.failed_at.map(|t| t.0),
            reason: r.reason,
            cleared_at: r.cleared_at.map(|t| t.0),
            cleared_by: r.cleared_by,
        }
    }
}

fn insert_sample(
    conn: &mut crate::DualConnection,
    file_id: &str,
    at: DateTime<Utc>,
    size_bytes: i64,
    links: i64,
    pieces: Option<i64>,
) -> QueryResult<usize> {
    diesel::insert_into(library_file_samples::table)
        .values(SampleRow {
            id: uuid::Uuid::new_v4().to_string(),
            file_id: file_id.to_string(),
            at: Timestamp(at),
            size_bytes,
            links,
            pieces,
        })
        .execute(conn)
}

fn files(rows: QueryResult<Vec<FileRow>>) -> Result<Vec<LibraryFile>> {
    Ok(rows
        .map_err(db_err)?
        .into_iter()
        .map(LibraryFile::from)
        .collect())
}

#[async_trait]
impl LibraryFileRepo for Store {
    async fn record_library_file_stat(
        &self,
        stat: &LibraryFileStat,
        at: DateTime<Utc>,
    ) -> Result<LibraryFile> {
        let mut stat = stat.clone();
        // Postgres keeps microseconds. A finer mtime would read back changed
        // on every pass and queue the file's measurements again for nothing.
        stat.mtime =
            chrono::DurationRound::duration_trunc(stat.mtime, chrono::Duration::microseconds(1))
                .unwrap_or(stat.mtime);
        self.with_conn(move |conn| {
            // Read, then insert or update, in one transaction: the path is the
            // key, and the sample depends on what the row held before.
            conn.transaction(|conn| {
                let old: Option<FileRow> = library_files::table
                    .filter(library_files::path.eq(&stat.path))
                    .select(FileRow::as_select())
                    .first(conn)
                    .optional()?;
                let id = match old {
                    None => {
                        let id = uuid::Uuid::new_v4().to_string();
                        diesel::insert_into(library_files::table)
                            .values(FileRow {
                                id: id.clone(),
                                kind: stat.kind.clone(),
                                acquirable_ref: stat.acquirable_ref.clone(),
                                path: stat.path.clone(),
                                dev: stat.dev,
                                ino: stat.ino,
                                size_bytes: stat.size_bytes,
                                disk_bytes: stat.disk_bytes,
                                links: stat.links,
                                mtime: Timestamp(stat.mtime),
                                pieces: None,
                                pieces_at: None,
                                checksum: None,
                                checksum_at: None,
                                stat_at: Timestamp(at),
                                gone_at: None,
                            })
                            .execute(conn)?;
                        insert_sample(conn, &id, at, stat.size_bytes, stat.links, None)?;
                        id
                    }
                    Some(old) => {
                        let content_changed = old.size_bytes != stat.size_bytes
                            || old.mtime.0 != stat.mtime
                            || old.dev != stat.dev
                            || old.ino != stat.ino;
                        let target = library_files::table.filter(library_files::id.eq(&old.id));
                        diesel::update(target)
                            .set((
                                library_files::kind.eq(&stat.kind),
                                library_files::acquirable_ref.eq(&stat.acquirable_ref),
                                library_files::dev.eq(stat.dev),
                                library_files::ino.eq(stat.ino),
                                library_files::size_bytes.eq(stat.size_bytes),
                                library_files::disk_bytes.eq(stat.disk_bytes),
                                library_files::links.eq(stat.links),
                                library_files::mtime.eq(Timestamp(stat.mtime)),
                                library_files::stat_at.eq(Timestamp(at)),
                                library_files::gone_at.eq(None::<Timestamp>),
                            ))
                            .execute(conn)?;
                        if content_changed {
                            diesel::update(target)
                                .set((
                                    library_files::pieces_at.eq(None::<Timestamp>),
                                    library_files::checksum_at.eq(None::<Timestamp>),
                                ))
                                .execute(conn)?;
                        }
                        if old.size_bytes != stat.size_bytes || old.links != stat.links {
                            insert_sample(
                                conn,
                                &old.id,
                                at,
                                stat.size_bytes,
                                stat.links,
                                old.pieces,
                            )?;
                        }
                        old.id
                    }
                };
                diesel::delete(library_orphans::table.filter(library_orphans::path.eq(&stat.path)))
                    .execute(conn)?;
                library_files::table
                    .filter(library_files::id.eq(&id))
                    .select(FileRow::as_select())
                    .first(conn)
            })
            .map(LibraryFile::from)
            .map_err(db_err)
        })
        .await
    }

    async fn record_library_file_pieces(
        &self,
        id: &str,
        pieces: i64,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            conn.transaction(|conn| {
                let old: Option<FileRow> = library_files::table
                    .filter(library_files::id.eq(&id))
                    .select(FileRow::as_select())
                    .first(conn)
                    .optional()?;
                let Some(old) = old else {
                    return Ok(false);
                };
                diesel::update(library_files::table.filter(library_files::id.eq(&id)))
                    .set((
                        library_files::pieces.eq(Some(pieces)),
                        library_files::pieces_at.eq(Some(Timestamp(at))),
                    ))
                    .execute(conn)?;
                if old.pieces != Some(pieces) {
                    insert_sample(conn, &id, at, old.size_bytes, old.links, Some(pieces))?;
                }
                QueryResult::Ok(true)
            })
            .map_err(db_err)
        })
        .await
    }

    async fn record_library_file_checksum(
        &self,
        id: &str,
        checksum: &str,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let (id, checksum) = (id.to_string(), checksum.to_string());
        self.with_conn(move |conn| {
            let n = diesel::update(library_files::table.filter(library_files::id.eq(&id)))
                .set((
                    library_files::checksum.eq(Some(checksum)),
                    library_files::checksum_at.eq(Some(Timestamp(at))),
                ))
                .execute(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }

    async fn mark_library_file_gone(&self, id: &str, at: DateTime<Utc>) -> Result<bool> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            conn.transaction(|conn| {
                let known: i64 = library_files::table
                    .filter(library_files::id.eq(&id))
                    .count()
                    .get_result(conn)?;
                diesel::update(
                    library_files::table
                        .filter(library_files::id.eq(&id))
                        .filter(library_files::gone_at.is_null()),
                )
                .set(library_files::gone_at.eq(Some(Timestamp(at))))
                .execute(conn)?;
                QueryResult::Ok(known > 0)
            })
            .map_err(db_err)
        })
        .await
    }

    async fn library_file_by_path(&self, path: &str) -> Result<Option<LibraryFile>> {
        let path = path.to_string();
        self.with_conn(move |conn| {
            let row: Option<FileRow> = library_files::table
                .filter(library_files::path.eq(&path))
                .select(FileRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(LibraryFile::from))
        })
        .await
    }

    async fn library_file_samples(&self, file_id: &str) -> Result<Vec<LibraryFileSample>> {
        let file_id = file_id.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<SampleRow> = library_file_samples::table
                .filter(library_file_samples::file_id.eq(&file_id))
                .order((
                    library_file_samples::at.asc(),
                    library_file_samples::id.asc(),
                ))
                .select(SampleRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows
                .into_iter()
                .map(|r| LibraryFileSample {
                    file_id: r.file_id,
                    at: r.at.0,
                    size_bytes: r.size_bytes,
                    links: r.links,
                    pieces: r.pieces,
                })
                .collect())
        })
        .await
    }

    async fn worst_library_files(
        &self,
        kind: Option<&str>,
        limit: i64,
    ) -> Result<Vec<LibraryFile>> {
        let kind = kind.map(str::to_string);
        self.with_conn(move |conn| {
            let mut q = library_files::table
                .filter(library_files::gone_at.is_null())
                .filter(library_files::pieces_at.is_not_null())
                .filter(library_files::pieces.is_not_null())
                .filter(library_files::size_bytes.gt(0))
                .into_boxed();
            if let Some(kind) = &kind {
                q = q.filter(library_files::kind.eq(kind));
            }
            // Pieces per byte orders the same as pieces per GB. A raw fragment:
            // the division must be done as a float on both backends.
            let rows = q
                .order((
                    sql::<Double>("CAST(pieces AS DOUBLE PRECISION) / size_bytes").desc(),
                    library_files::path.asc(),
                ))
                .limit(limit)
                .select(FileRow::as_select())
                .load(conn);
            files(rows)
        })
        .await
    }

    async fn library_totals_by_kind(&self) -> Result<Vec<LibraryKindTotals>> {
        // One pass in the database. Raw aggregate fragments: `SUM` of a bigint
        // is a numeric on Postgres, so each is cast back; `gone` rows count
        // only in the last column.
        const HERE: &str = "gone_at IS NULL";
        const MEASURED: &str = "gone_at IS NULL AND pieces_at IS NOT NULL AND pieces IS NOT NULL";
        fn count(cond: &str) -> diesel::expression::SqlLiteral<BigInt> {
            sql::<BigInt>(&format!(
                "CAST(COALESCE(SUM(CASE WHEN {cond} THEN 1 ELSE 0 END), 0) AS BIGINT)"
            ))
        }
        fn sum(cond: &str, col: &str) -> diesel::expression::SqlLiteral<BigInt> {
            sql::<BigInt>(&format!(
                "CAST(COALESCE(SUM(CASE WHEN {cond} THEN {col} ELSE 0 END), 0) AS BIGINT)"
            ))
        }
        type Row = (String, i64, i64, i64, i64, i64, i64, i64, i64, i64);
        self.with_conn(move |conn| {
            let rows: Vec<Row> = library_files::table
                .group_by(library_files::kind)
                .order(library_files::kind.asc())
                .select((
                    library_files::kind,
                    count(HERE),
                    sum(HERE, "size_bytes"),
                    sum(HERE, "disk_bytes"),
                    count(&format!("{HERE} AND links > 1")),
                    count(MEASURED),
                    sum(MEASURED, "size_bytes"),
                    sum(MEASURED, "pieces"),
                    count(&format!("{HERE} AND checksum_at IS NOT NULL")),
                    count("gone_at IS NOT NULL"),
                ))
                .load(conn)
                .map_err(db_err)?;
            Ok(rows
                .into_iter()
                .map(|r| LibraryKindTotals {
                    kind: r.0,
                    files: r.1,
                    size_bytes: r.2,
                    disk_bytes: r.3,
                    linked: r.4,
                    measured: r.5,
                    measured_bytes: r.6,
                    pieces: r.7,
                    checksummed: r.8,
                    gone: r.9,
                })
                .collect())
        })
        .await
    }

    async fn library_files_awaiting_pieces(&self, limit: i64) -> Result<Vec<LibraryFile>> {
        self.with_conn(move |conn| {
            let rows = library_files::table
                .filter(library_files::gone_at.is_null())
                .filter(library_files::pieces_at.is_null())
                .order((library_files::stat_at.asc(), library_files::path.asc()))
                .limit(limit)
                .select(FileRow::as_select())
                .load(conn);
            files(rows)
        })
        .await
    }

    async fn library_files_awaiting_checksum(&self, limit: i64) -> Result<Vec<LibraryFile>> {
        self.with_conn(move |conn| {
            let rows = library_files::table
                .filter(library_files::gone_at.is_null())
                .filter(library_files::checksum_at.is_null())
                .order((library_files::stat_at.asc(), library_files::path.asc()))
                .limit(limit)
                .select(FileRow::as_select())
                .load(conn);
            files(rows)
        })
        .await
    }

    async fn library_files_oldest_pieces(&self, limit: i64) -> Result<Vec<LibraryFile>> {
        self.with_conn(move |conn| {
            let rows = library_files::table
                .filter(library_files::gone_at.is_null())
                .filter(library_files::pieces_at.is_not_null())
                .order((library_files::pieces_at.asc(), library_files::path.asc()))
                .limit(limit)
                .select(FileRow::as_select())
                .load(conn);
            files(rows)
        })
        .await
    }

    async fn record_library_orphan(
        &self,
        path: &str,
        size_bytes: i64,
        mtime: DateTime<Utc>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let path = path.to_string();
        self.with_conn(move |conn| {
            conn.transaction(|conn| {
                let claimed: i64 = library_files::table
                    .filter(library_files::path.eq(&path))
                    .count()
                    .get_result(conn)?;
                if claimed > 0 {
                    return Ok(false);
                }
                // Update, then insert when nothing was there: an upsert clause
                // is not one fragment across both backends, and this keeps
                // `first_seen` without one.
                let n =
                    diesel::update(library_orphans::table.filter(library_orphans::path.eq(&path)))
                        .set((
                            library_orphans::size_bytes.eq(size_bytes),
                            library_orphans::mtime.eq(Timestamp(mtime)),
                            library_orphans::last_seen.eq(Timestamp(at)),
                        ))
                        .execute(conn)?;
                if n == 0 {
                    diesel::insert_into(library_orphans::table)
                        .values(OrphanRow {
                            path: path.clone(),
                            size_bytes,
                            mtime: Timestamp(mtime),
                            first_seen: Timestamp(at),
                            last_seen: Timestamp(at),
                        })
                        .execute(conn)?;
                }
                QueryResult::Ok(true)
            })
            .map_err(db_err)
        })
        .await
    }

    async fn list_library_orphans(&self, limit: i64, offset: i64) -> Result<Vec<LibraryOrphan>> {
        self.with_conn(move |conn| {
            let rows: Vec<OrphanRow> = library_orphans::table
                .order((
                    library_orphans::size_bytes.desc(),
                    library_orphans::path.asc(),
                ))
                .limit(limit)
                .offset(offset)
                .select(OrphanRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows
                .into_iter()
                .map(|r| LibraryOrphan {
                    path: r.path,
                    size_bytes: r.size_bytes,
                    mtime: r.mtime.0,
                    first_seen: r.first_seen.0,
                    last_seen: r.last_seen.0,
                })
                .collect())
        })
        .await
    }

    async fn prune_library_orphans(&self, under: &str, before: DateTime<Utc>) -> Result<usize> {
        let under = std::path::PathBuf::from(under);
        self.with_conn(move |conn| {
            conn.transaction(|conn| {
                // The folder test is done here, on path components: a LIKE
                // pattern would need the path escaped, and `/lib/tv` must not
                // match `/lib/tv2`.
                let stale: Vec<String> = library_orphans::table
                    .filter(library_orphans::last_seen.lt(Timestamp(before)))
                    .select(library_orphans::path)
                    .load(conn)?;
                let mut n = 0;
                for path in stale {
                    if std::path::Path::new(&path).starts_with(&under) {
                        n += diesel::delete(
                            library_orphans::table.filter(library_orphans::path.eq(&path)),
                        )
                        .execute(conn)?;
                    }
                }
                QueryResult::Ok(n)
            })
            .map_err(db_err)
        })
        .await
    }

    async fn library_files_of_kind(&self, kind: &str) -> Result<Vec<LibraryFile>> {
        let kind = kind.to_string();
        self.with_conn(move |conn| {
            let rows = library_files::table
                .filter(library_files::kind.eq(&kind))
                .select(FileRow::as_select())
                .load(conn);
            // Sorted here, by bytes: the database's order of text depends on
            // its collation, which differs between the backends.
            let mut rows = files(rows)?;
            rows.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(rows)
        })
        .await
    }

    async fn delete_library_file(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            conn.transaction(|conn| {
                diesel::delete(
                    library_file_samples::table.filter(library_file_samples::file_id.eq(&id)),
                )
                .execute(conn)?;
                let n = diesel::delete(library_files::table.filter(library_files::id.eq(&id)))
                    .execute(conn)?;
                QueryResult::Ok(n > 0)
            })
            .map_err(db_err)
        })
        .await
    }
}

#[async_trait]
impl StorageProbeRepo for Store {
    async fn storage_probe_status(&self, probe: &str) -> Result<Option<StorageProbeStatus>> {
        let probe = probe.to_string();
        self.with_conn(move |conn| {
            let row: Option<ProbeRow> = storage_probe_state::table
                .filter(storage_probe_state::probe.eq(&probe))
                .select(ProbeRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(StorageProbeStatus::from))
        })
        .await
    }

    async fn fail_storage_probe(&self, probe: &str, reason: &str, at: DateTime<Utc>) -> Result<()> {
        let (probe, reason) = (probe.to_string(), reason.to_string());
        self.with_conn(move |conn| {
            conn.transaction(|conn| {
                let n = diesel::update(
                    storage_probe_state::table.filter(storage_probe_state::probe.eq(&probe)),
                )
                .set((
                    storage_probe_state::state.eq(ProbeState::Failed.as_str()),
                    storage_probe_state::failed_at.eq(Some(Timestamp(at))),
                    storage_probe_state::reason.eq(Some(reason.clone())),
                    // A new failure is not cleared, whatever an earlier one was.
                    storage_probe_state::cleared_at.eq(None::<Timestamp>),
                    storage_probe_state::cleared_by.eq(None::<String>),
                ))
                .execute(conn)?;
                if n == 0 {
                    diesel::insert_into(storage_probe_state::table)
                        .values(ProbeRow {
                            probe: probe.clone(),
                            state: ProbeState::Failed.as_str().to_string(),
                            failed_at: Some(Timestamp(at)),
                            reason: Some(reason.clone()),
                            cleared_at: None,
                            cleared_by: None,
                        })
                        .execute(conn)?;
                }
                QueryResult::Ok(())
            })
            .map_err(db_err)
        })
        .await
    }

    async fn clear_storage_probe(&self, probe: &str, by: &str, at: DateTime<Utc>) -> Result<bool> {
        let (probe, by) = (probe.to_string(), by.to_string());
        self.with_conn(move |conn| {
            let n = diesel::update(
                storage_probe_state::table
                    .filter(storage_probe_state::probe.eq(&probe))
                    .filter(storage_probe_state::state.eq(ProbeState::Failed.as_str())),
            )
            .set((
                storage_probe_state::state.eq(ProbeState::On.as_str()),
                storage_probe_state::cleared_at.eq(Some(Timestamp(at))),
                storage_probe_state::cleared_by.eq(Some(by)),
            ))
            .execute(conn)
            .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }
}
