//! `AudiobooksRepo` — CRUD for `authors`, `book_series`, `books`, and
//! `book_files`, mirroring `skadi_movies::MoviesRepo`.
//!
//! Implemented on `skadi_store::Store` over a single
//! [`DualConnection`](skadi_store::DualConnection) via [`Store::with_conn`]
//! (SKADI-I-0015); the domain runs its own queries on the shared dual-backend
//! connection. All ids are stored as `TEXT`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::{Json, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use skadi_core::{
    AcquisitionStatus, AppError, AsinId, AuthorId, BookFileId, BookId, BookSeriesId, ExternalIds,
    FileRef, ProfileId, QualityId, Result, RootFolder, RootFolderId,
};
use skadi_importer::AcquirableRef;
use skadi_store::Store;

use crate::author::{Author, Series, SeriesLink};
use crate::book::Book;
use crate::book_file::BookFile;
use crate::schema::{authors, book_editions, book_series, books, watchers, works};
use crate::work::{WatchScope, Watcher, Work};

// ---------------------------------------------------------------------------
// Public trait + filter types
// ---------------------------------------------------------------------------

/// Filter for [`AudiobooksRepo::list_authors`].
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct AuthorFilter {
    pub monitored: Option<bool>,
}

/// Filter for [`AudiobooksRepo::list_books`].
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct BookFilter {
    pub monitored: Option<bool>,
    /// Page size. `None` is unbounded, for callers that need every row (sweeps,
    /// occupancy scans). The HTTP list path passes a bound (SKADI-T-0494).
    pub limit: Option<i64>,
    /// Rows to skip; ignored when `limit` is `None`.
    pub offset: Option<i64>,
}

/// CRUD over authors, series, books, and book files.
///
/// `Book` values returned from `get_book`/`list_books` come with their `files`
/// and `series` populated; writes to files go through the book-file methods.
#[async_trait]
pub trait AudiobooksRepo: Send + Sync {
    // --- authors ---
    async fn list_authors(&self, filter: AuthorFilter) -> Result<Vec<Author>>;
    async fn get_author(&self, id: AuthorId) -> Result<Option<Author>>;
    async fn get_author_by_asin(&self, asin: &AsinId) -> Result<Option<Author>>;
    async fn upsert_author(&self, author: &Author) -> Result<()>;
    async fn delete_author(&self, id: AuthorId) -> Result<()>;

    // --- series ---
    async fn get_series(&self, id: BookSeriesId) -> Result<Option<Series>>;
    async fn get_series_by_name(&self, name: &str) -> Result<Option<Series>>;
    async fn upsert_series(&self, series: &Series) -> Result<()>;

    // --- books ---
    async fn list_books(&self, filter: BookFilter) -> Result<Vec<Book>>;
    /// How many books match `filter`, ignoring `limit`/`offset` (SKADI-T-0494).
    async fn count_books(&self, filter: BookFilter) -> Result<i64>;
    async fn get_book(&self, id: BookId) -> Result<Option<Book>>;
    async fn get_book_by_asin(&self, asin: &AsinId) -> Result<Option<Book>>;
    async fn list_books_by_author(&self, author_id: AuthorId) -> Result<Vec<Book>>;
    async fn upsert_book(&self, book: &Book) -> Result<()>;
    async fn delete_book(&self, id: BookId) -> Result<()>;

    // --- book files ---
    async fn list_book_files(&self, book_id: BookId) -> Result<Vec<BookFile>>;
    async fn get_book_file(&self, id: BookFileId) -> Result<Option<BookFile>>;
    async fn get_book_file_by_ref(&self, r: &AcquirableRef) -> Result<Option<BookFile>>;
    async fn upsert_book_file(&self, file: &BookFile) -> Result<()>;
    async fn set_book_file_status(&self, id: BookFileId, status: AcquisitionStatus) -> Result<()>;
}

/// CRUD over the **known-works** catalog (SKADI-I-0018) — works that exist per the
/// Audible catalog, owned or not. Keyed by Audible ASIN.
#[async_trait]
pub trait WorksRepo: Send + Sync {
    /// Insert or update a work by ASIN. Preserves `first_seen` on update.
    async fn upsert_work(&self, work: &Work) -> Result<()>;
    /// Upsert many works (idempotent).
    async fn upsert_works(&self, works: &[Work]) -> Result<()>;
    async fn get_work(&self, asin: &AsinId) -> Result<Option<Work>>;
    /// Clear a work's `author_asin` (SKADI-T-0653). A separate method because
    /// [`upsert_work`](Self::upsert_work) never writes NULL — its changeset skips
    /// `None` fields so a sparse re-fetch cannot wipe a known cover or language —
    /// which also means it can never *remove* a wrong attribution.
    async fn clear_work_author(&self, asin: &AsinId) -> Result<()>;
    /// All known works for an author (by author ASIN), newest release first.
    async fn list_works_by_author(&self, author_asin: &AsinId) -> Result<Vec<Work>>;
    /// All known works in a series (by series ASIN), title-ordered.
    async fn list_works_by_series(&self, series_asin: &AsinId) -> Result<Vec<Work>>;
    /// Every known work (for series-completeness rollups).
    async fn list_all_works(&self) -> Result<Vec<Work>>;
}

/// CRUD over **watchers** — the acquisition switches at author/series/book scope
/// (SKADI-T-0156).
#[async_trait]
pub trait WatchersRepo: Send + Sync {
    async fn set_watcher(&self, scope: WatchScope, key: &str) -> Result<()>;
    async fn clear_watcher(&self, scope: WatchScope, key: &str) -> Result<()>;
    async fn list_watchers(&self) -> Result<Vec<Watcher>>;
    async fn is_watched(&self, scope: WatchScope, key: &str) -> Result<bool>;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn db_err(e: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("database error: {e}"))
}

fn parse_uuid(s: &str, ctx: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| AppError::Internal(format!("invalid {ctx} uuid: {e}")))
}

fn decode_acquirable_ref(r: &AcquirableRef) -> Result<BookFileId> {
    Uuid::parse_str(&r.0)
        .map(BookFileId::from)
        .map_err(|e| AppError::Validation(format!("invalid AcquirableRef for book file: {e}")))
}

/// Discriminant tag for `AcquisitionStatus`, stored in `status_kind` for
/// indexable lookups. Must stay in sync with the variant names.
fn status_kind_of(s: &AcquisitionStatus) -> &'static str {
    match s {
        AcquisitionStatus::Missing => "Missing",
        AcquisitionStatus::Searching { .. } => "Searching",
        AcquisitionStatus::Snatched { .. } => "Snatched",
        AcquisitionStatus::Downloading { .. } => "Downloading",
        AcquisitionStatus::Imported { .. } => "Imported",
        AcquisitionStatus::Cutoff => "Cutoff",
        AcquisitionStatus::Failed { .. } => "Failed",
    }
}

// ---------------------------------------------------------------------------
// Row models
// ---------------------------------------------------------------------------

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = authors)]
struct AuthorRow {
    id: String,
    asin: Option<String>,
    name: String,
    description: Option<String>,
    image_url: Option<String>,
    monitored: bool,
    added_at: Timestamp,
    last_metadata_refresh: Option<Timestamp>,
}

impl TryFrom<AuthorRow> for Author {
    type Error = AppError;
    fn try_from(r: AuthorRow) -> Result<Self> {
        Ok(Author {
            id: AuthorId::from(parse_uuid(&r.id, "author")?),
            asin: r.asin.map(AsinId),
            name: r.name,
            description: r.description,
            image_url: r.image_url,
            monitored: r.monitored,
            added_at: r.added_at.0,
            last_metadata_refresh: r.last_metadata_refresh.map(|t| t.0),
        })
    }
}

fn author_to_row(a: &Author) -> AuthorRow {
    AuthorRow {
        id: a.id.to_string(),
        asin: a.asin.as_ref().map(|x| x.0.clone()),
        name: a.name.clone(),
        description: a.description.clone(),
        image_url: a.image_url.clone(),
        monitored: a.monitored,
        added_at: Timestamp(a.added_at),
        last_metadata_refresh: a.last_metadata_refresh.map(Timestamp),
    }
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = book_series)]
struct SeriesRow {
    id: String,
    asin: Option<String>,
    name: String,
}

impl TryFrom<SeriesRow> for Series {
    type Error = AppError;
    fn try_from(r: SeriesRow) -> Result<Self> {
        Ok(Series {
            id: BookSeriesId::from(parse_uuid(&r.id, "series")?),
            asin: r.asin.map(AsinId),
            name: r.name,
        })
    }
}

fn series_to_row(s: &Series) -> SeriesRow {
    SeriesRow {
        id: s.id.to_string(),
        asin: s.asin.as_ref().map(|x| x.0.clone()),
        name: s.name.clone(),
    }
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = books)]
struct BookRow {
    id: String,
    asin: Option<String>,
    title: String,
    subtitle: Option<String>,
    author_id: Option<String>,
    authors_json: Json<Vec<String>>,
    narrators_json: Json<Vec<String>>,
    series_id: Option<String>,
    series_position: Option<String>,
    year: Option<i32>,
    overview: Option<String>,
    runtime_minutes: Option<i32>,
    cover_url: Option<String>,
    release_date: Option<String>,
    monitored: bool,
    profile_id: String,
    root_folder_id: String,
    root_folder_path: String,
    added_at: Timestamp,
    last_metadata_refresh: Option<Timestamp>,
}

/// Convert a row to a `Book` with `series`/`files` left empty (the repo
/// populates them on load).
fn book_from_row(r: BookRow) -> Result<Book> {
    Ok(Book {
        id: BookId::from(parse_uuid(&r.id, "book")?),
        external_ids: ExternalIds {
            asin: r.asin.map(AsinId),
            ..Default::default()
        },
        title: r.title,
        subtitle: r.subtitle,
        author_id: r
            .author_id
            .map(|a| parse_uuid(&a, "author").map(AuthorId::from))
            .transpose()?,
        authors: r.authors_json.0,
        narrators: r.narrators_json.0,
        series: None,
        year: r.year.and_then(|y| u16::try_from(y).ok()),
        overview: r.overview,
        runtime_minutes: r.runtime_minutes.and_then(|m| u32::try_from(m).ok()),
        cover_url: r.cover_url,
        release_date: r
            .release_date
            .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()),
        monitored: r.monitored,
        profile: ProfileId::from(parse_uuid(&r.profile_id, "profile")?),
        root_folder: RootFolder {
            id: RootFolderId::from(parse_uuid(&r.root_folder_id, "root_folder")?),
            path: PathBuf::from(r.root_folder_path),
        },
        added_at: r.added_at.0,
        last_metadata_refresh: r.last_metadata_refresh.map(|t| t.0),
        files: Vec::new(),
    })
}

/// Like [`book_from_row`] but also carries forward the row's raw
/// `series_id`/`series_position` (which `book_from_row` discards), so batched
/// callers can resolve the series link without re-reading the book row.
fn book_from_row_with_series(r: BookRow) -> Result<(Book, Option<String>, Option<String>)> {
    let series_id = r.series_id.clone();
    let series_position = r.series_position.clone();
    let book = book_from_row(r)?;
    Ok((book, series_id, series_position))
}

fn book_to_row(b: &Book) -> BookRow {
    BookRow {
        id: b.id.to_string(),
        asin: b.external_ids.asin.as_ref().map(|x| x.0.clone()),
        title: b.title.clone(),
        subtitle: b.subtitle.clone(),
        author_id: b.author_id.map(|a| a.to_string()),
        authors_json: Json(b.authors.clone()),
        narrators_json: Json(b.narrators.clone()),
        series_id: b.series.as_ref().map(|s| s.series_id.to_string()),
        series_position: b.series.as_ref().and_then(|s| s.position.clone()),
        year: b.year.map(i32::from),
        overview: b.overview.clone(),
        runtime_minutes: b.runtime_minutes.and_then(|v| i32::try_from(v).ok()),
        cover_url: b.cover_url.clone(),
        release_date: b.release_date.map(|d| d.format("%Y-%m-%d").to_string()),
        monitored: b.monitored,
        profile_id: b.profile.to_string(),
        root_folder_id: b.root_folder.id.to_string(),
        root_folder_path: b.root_folder.path.to_string_lossy().into_owned(),
        added_at: Timestamp(b.added_at),
        last_metadata_refresh: b.last_metadata_refresh.map(Timestamp),
    }
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = book_editions)]
struct BookFileRow {
    id: String,
    book_id: String,
    /// Which edition this row is (SKADI-T-0448). Keyed by slug — stable,
    /// readable in a database, and what every caller already has.
    kind_slug: String,
    /// Whether this edition is wanted (SKADI-T-0562).
    monitored: bool,
    status_json: Json<AcquisitionStatus>,
    status_kind: String,
    file_path: Option<String>,
    quality_id: Option<String>,
    format_score: i32,
    updated_at: Timestamp,
    /// Probed media-info as JSON (SKADI-T-0236); `None` until the post-import probe.
    media_info: Option<String>,
}

impl TryFrom<BookFileRow> for BookFile {
    type Error = AppError;
    fn try_from(r: BookFileRow) -> Result<Self> {
        Ok(BookFile {
            kind: r.kind_slug,
            monitored: r.monitored,
            id: BookFileId::from(parse_uuid(&r.id, "book_file")?),
            book_id: BookId::from(parse_uuid(&r.book_id, "book")?),
            status: r.status_json.0,
            file: r.file_path.map(|p| FileRef {
                path: PathBuf::from(p),
            }),
            quality: r
                .quality_id
                .map(|q| parse_uuid(&q, "quality").map(QualityId::from))
                .transpose()?,
            format_score: r.format_score,
            updated_at: r.updated_at.0,
            media_info: r
                .media_info
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
        })
    }
}

fn book_file_to_row(f: &BookFile) -> BookFileRow {
    BookFileRow {
        id: f.id.to_string(),
        book_id: f.book_id.to_string(),
        kind_slug: f.kind.clone(),
        monitored: f.monitored,
        status_json: Json(f.status.clone()),
        status_kind: status_kind_of(&f.status).to_string(),
        file_path: f
            .file
            .as_ref()
            .map(|x| x.path.to_string_lossy().into_owned()),
        quality_id: f.quality.map(|q| q.to_string()),
        format_score: f.format_score,
        updated_at: Timestamp(f.updated_at),
        media_info: f
            .media_info
            .as_ref()
            .and_then(|m| serde_json::to_string(m).ok()),
    }
}

// ---------------------------------------------------------------------------
// impl AudiobooksRepo for Store
// ---------------------------------------------------------------------------

#[async_trait]
impl AudiobooksRepo for Store {
    async fn list_authors(&self, filter: AuthorFilter) -> Result<Vec<Author>> {
        let mut out = self
            .with_conn(|conn| {
                let rows: Vec<AuthorRow> = authors::table
                    .select(AuthorRow::as_select())
                    .order(authors::name.asc())
                    .load(conn)
                    .map_err(db_err)?;
                rows.into_iter()
                    .map(Author::try_from)
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        if let Some(m) = filter.monitored {
            out.retain(|a| a.monitored == m);
        }
        Ok(out)
    }

    async fn get_author(&self, id: AuthorId) -> Result<Option<Author>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<AuthorRow> = authors::table
                .find(key)
                .select(AuthorRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(Author::try_from).transpose()
        })
        .await
    }

    async fn get_author_by_asin(&self, asin: &AsinId) -> Result<Option<Author>> {
        let a = asin.0.clone();
        self.with_conn(move |conn| {
            let row: Option<AuthorRow> = authors::table
                .filter(authors::asin.eq(a))
                .select(AuthorRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(Author::try_from).transpose()
        })
        .await
    }

    async fn upsert_author(&self, author: &Author) -> Result<()> {
        let row = author_to_row(author);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(authors::table)
                        .values(&row)
                        .on_conflict(authors::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(authors::table)
                        .values(&row)
                        .on_conflict(authors::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_author(&self, id: AuthorId) -> Result<()> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::delete(authors::table.find(key))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn get_series(&self, id: BookSeriesId) -> Result<Option<Series>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<SeriesRow> = book_series::table
                .find(key)
                .select(SeriesRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(Series::try_from).transpose()
        })
        .await
    }

    async fn get_series_by_name(&self, name: &str) -> Result<Option<Series>> {
        let n = name.to_string();
        self.with_conn(move |conn| {
            let row: Option<SeriesRow> = book_series::table
                .filter(book_series::name.eq(n))
                .select(SeriesRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(Series::try_from).transpose()
        })
        .await
    }

    async fn upsert_series(&self, series: &Series) -> Result<()> {
        let row = series_to_row(series);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(book_series::table)
                        .values(&row)
                        .on_conflict(book_series::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(book_series::table)
                        .values(&row)
                        .on_conflict(book_series::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn count_books(&self, filter: BookFilter) -> Result<i64> {
        self.with_conn(move |conn| {
            let mut q = books::table.into_boxed();
            if let Some(m) = filter.monitored {
                q = q.filter(books::monitored.eq(m));
            }
            q.count().get_result::<i64>(conn).map_err(db_err)
        })
        .await
    }

    async fn list_books(&self, filter: BookFilter) -> Result<Vec<Book>> {
        // Filter and bound in SQL (SKADI-T-0494) — `monitored` applied after the
        // load would page over the wrong set, and `hydrate_books` below is per
        // row, so an unbounded call hydrates the whole catalog.
        let mut out = self
            .with_conn(move |conn| {
                let mut q = books::table
                    .select(BookRow::as_select())
                    .order(books::added_at.asc())
                    .into_boxed();
                if let Some(m) = filter.monitored {
                    q = q.filter(books::monitored.eq(m));
                }
                if let Some(limit) = filter.limit {
                    q = q.limit(limit).offset(filter.offset.unwrap_or(0));
                }
                let rows: Vec<BookRow> = q.load(conn).map_err(db_err)?;
                rows.into_iter()
                    .map(book_from_row_with_series)
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        hydrate_books(self, &mut out).await?;
        Ok(out.into_iter().map(|(b, _, _)| b).collect())
    }

    async fn get_book(&self, id: BookId) -> Result<Option<Book>> {
        let key = id.to_string();
        let book = self
            .with_conn(move |conn| {
                let row: Option<BookRow> = books::table
                    .find(key)
                    .select(BookRow::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                row.map(book_from_row).transpose()
            })
            .await?;
        match book {
            None => Ok(None),
            Some(mut b) => {
                hydrate(self, &mut b).await?;
                Ok(Some(b))
            }
        }
    }

    async fn get_book_by_asin(&self, asin: &AsinId) -> Result<Option<Book>> {
        let a = asin.0.clone();
        let book = self
            .with_conn(move |conn| {
                let row: Option<BookRow> = books::table
                    .filter(books::asin.eq(a))
                    .select(BookRow::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                row.map(book_from_row).transpose()
            })
            .await?;
        match book {
            None => Ok(None),
            Some(mut b) => {
                hydrate(self, &mut b).await?;
                Ok(Some(b))
            }
        }
    }

    async fn list_books_by_author(&self, author_id: AuthorId) -> Result<Vec<Book>> {
        let key = author_id.to_string();
        let mut out = self
            .with_conn(move |conn| {
                let rows: Vec<BookRow> = books::table
                    .filter(books::author_id.eq(key))
                    .select(BookRow::as_select())
                    .order(books::title.asc())
                    .load(conn)
                    .map_err(db_err)?;
                rows.into_iter()
                    .map(book_from_row_with_series)
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        hydrate_books(self, &mut out).await?;
        Ok(out.into_iter().map(|(b, _, _)| b).collect())
    }

    async fn upsert_book(&self, book: &Book) -> Result<()> {
        let row = book_to_row(book);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(books::table)
                        .values(&row)
                        .on_conflict(books::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(books::table)
                        .values(&row)
                        .on_conflict(books::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_book(&self, id: BookId) -> Result<()> {
        let key = id.to_string();
        let n = self
            .with_conn(move |conn| {
                diesel::delete(books::table.find(key))
                    .execute(conn)
                    .map_err(db_err)
            })
            .await?;
        // Destructive and previously invisible (SKADI-T-0456).
        tracing::info!(book = %id, rows = n, "book deleted");
        Ok(())
    }

    async fn list_book_files(&self, book_id: BookId) -> Result<Vec<BookFile>> {
        let key = book_id.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<BookFileRow> = book_editions::table
                .filter(book_editions::book_id.eq(key))
                .select(BookFileRow::as_select())
                .order(book_editions::updated_at.asc())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(BookFile::try_from).collect()
        })
        .await
    }

    async fn get_book_file(&self, id: BookFileId) -> Result<Option<BookFile>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<BookFileRow> = book_editions::table
                .find(key)
                .select(BookFileRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(BookFile::try_from).transpose()
        })
        .await
    }

    async fn get_book_file_by_ref(&self, r: &AcquirableRef) -> Result<Option<BookFile>> {
        let id = decode_acquirable_ref(r)?;
        self.get_book_file(id).await
    }

    async fn upsert_book_file(&self, file: &BookFile) -> Result<()> {
        let row = book_file_to_row(file);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(book_editions::table)
                        .values(&row)
                        .on_conflict(book_editions::id)
                        .do_update()
                        .set(&row)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(book_editions::table)
                        .values(&row)
                        .on_conflict(book_editions::id)
                        .do_update()
                        .set(&row)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn set_book_file_status(&self, id: BookFileId, status: AcquisitionStatus) -> Result<()> {
        let key = id.to_string();
        let status_kind = status_kind_of(&status).to_string();
        let imported = match &status {
            AcquisitionStatus::Imported {
                file,
                quality,
                score,
                ..
            } => Some((
                file.path.to_string_lossy().into_owned(),
                quality.to_string(),
                *score,
            )),
            _ => None,
        };
        self.with_conn(move |conn| {
            let affected = diesel::update(book_editions::table.find(&key))
                .set((
                    book_editions::status_json.eq(Json(status)),
                    book_editions::status_kind.eq(status_kind),
                    book_editions::updated_at.eq(Timestamp(Utc::now())),
                ))
                .execute(conn)
                .map_err(db_err)?;
            // A status write that matched no row is a silent no-op, and the
            // hunter cannot tell it from a successful one (SKADI-T-0455).
            // Report it; every caller already treats a status write as
            // best-effort, so this surfaces the case without failing a run.
            if affected == 0 {
                return Err(AppError::NotFound(format!("book file {key}")));
            }
            if let Some((fp, qid, fs)) = imported {
                diesel::update(book_editions::table.find(&key))
                    .set((
                        book_editions::file_path.eq(Some(fp)),
                        book_editions::quality_id.eq(Some(qid)),
                        book_editions::format_score.eq(fs),
                    ))
                    .execute(conn)
                    .map_err(db_err)?;
            }
            Ok(())
        })
        .await
    }
}

/// Populate a freshly-loaded `Book`'s `files` and `series` (the series name
/// comes from the `book_series` entity table). A free fn (not a trait method) so
/// the future stays `Send` for the `#[async_trait]` callers.
async fn hydrate(store: &Store, book: &mut Book) -> Result<()> {
    book.files = store.list_book_files(book.id).await?;
    // `book_from_row` left `series` None but the row carried series_id; re-read
    // it via a dedicated query keyed by the book to resolve the series name.
    let key = book.id.to_string();
    let link: Option<(Option<String>, Option<String>)> = store
        .with_conn(move |conn| {
            books::table
                .find(key)
                .select((books::series_id, books::series_position))
                .first(conn)
                .optional()
                .map_err(db_err)
        })
        .await?;
    if let Some((Some(sid), position)) = link {
        let series_id = BookSeriesId::from(parse_uuid(&sid, "series")?);
        if let Some(series) = store.get_series(series_id).await? {
            book.series = Some(SeriesLink {
                series_id,
                name: series.name,
                position,
            });
        }
    }
    Ok(())
}

/// Batched counterpart of [`hydrate`] for list endpoints: populates `files` and
/// `series` for a whole slice of books using a CONSTANT number of queries
/// (one for all `book_files`, one for all referenced `book_series`) regardless
/// of book count. Each element carries the row's raw `series_id`/`series_position`
/// alongside the `Book` (see [`book_from_row_with_series`]).
///
/// Semantics match [`hydrate`]: a book's `series` is set only when the row's
/// `series_id` is present AND the referenced series row exists.
async fn hydrate_books(
    store: &Store,
    books: &mut [(Book, Option<String>, Option<String>)],
) -> Result<()> {
    if books.is_empty() {
        return Ok(());
    }

    // 1. Batch-load every `book_file` for these books in ONE query, converting
    //    rows exactly as `list_book_files` does, grouped by book id. The
    //    `updated_at.asc()` ordering matches the per-book query.
    let ids: Vec<String> = books.iter().map(|(b, _, _)| b.id.to_string()).collect();
    let mut files_by_book: HashMap<BookId, Vec<BookFile>> = store
        .with_conn(move |conn| {
            let rows: Vec<BookFileRow> = book_editions::table
                .filter(book_editions::book_id.eq_any(&ids))
                .select(BookFileRow::as_select())
                .order(book_editions::updated_at.asc())
                .load(conn)
                .map_err(db_err)?;
            let mut map: HashMap<BookId, Vec<BookFile>> = HashMap::new();
            for row in rows {
                let bf = BookFile::try_from(row)?;
                map.entry(bf.book_id).or_default().push(bf);
            }
            Ok(map)
        })
        .await?;

    // 2. Batch-resolve series: collect the unique referenced series ids and load
    //    them in ONE query, keyed by their (string) id.
    let mut seen = HashSet::new();
    let series_ids: Vec<String> = books
        .iter()
        .filter_map(|(_, sid, _)| sid.clone())
        .filter(|sid| seen.insert(sid.clone()))
        .collect();
    let series_by_id: HashMap<String, Series> = if series_ids.is_empty() {
        HashMap::new()
    } else {
        store
            .with_conn(move |conn| {
                let rows: Vec<SeriesRow> = book_series::table
                    .filter(book_series::id.eq_any(&series_ids))
                    .select(SeriesRow::as_select())
                    .load(conn)
                    .map_err(db_err)?;
                let mut map: HashMap<String, Series> = HashMap::new();
                for row in rows {
                    let id = row.id.clone();
                    map.insert(id, Series::try_from(row)?);
                }
                Ok(map)
            })
            .await?
    };

    // 3. Assemble in memory (preserves the incoming order).
    for (book, sid, position) in books.iter_mut() {
        book.files = files_by_book.remove(&book.id).unwrap_or_default();
        if let Some(sid) = sid
            && let Some(series) = series_by_id.get(sid)
        {
            book.series = Some(SeriesLink {
                series_id: series.id,
                name: series.name.clone(),
                position: position.clone(),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Known-works catalog (SKADI-I-0018 / SKADI-T-0154)
// ---------------------------------------------------------------------------

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = works)]
struct WorkRow {
    asin: String,
    title: String,
    authors_json: Json<Vec<String>>,
    author_asin: Option<String>,
    series_name: Option<String>,
    series_asin: Option<String>,
    series_position: Option<String>,
    cover_url: Option<String>,
    release_date: Option<String>,
    language: Option<String>,
    first_seen: Timestamp,
    updated_at: Timestamp,
}

/// Update changeset for `works` — every mutable column **except** `first_seen`,
/// so an upsert preserves when the work was first catalogued.
#[derive(AsChangeset)]
#[diesel(table_name = works)]
struct WorkUpdate {
    title: String,
    authors_json: Json<Vec<String>>,
    author_asin: Option<String>,
    series_name: Option<String>,
    series_asin: Option<String>,
    series_position: Option<String>,
    cover_url: Option<String>,
    release_date: Option<String>,
    language: Option<String>,
    updated_at: Timestamp,
}

fn work_to_row(w: &Work) -> WorkRow {
    WorkRow {
        asin: w.asin.0.clone(),
        title: w.title.clone(),
        authors_json: Json(w.authors.clone()),
        author_asin: w.author_asin.as_ref().map(|a| a.0.clone()),
        series_name: w.series_name.clone(),
        series_asin: w.series_asin.as_ref().map(|a| a.0.clone()),
        series_position: w.series_position.clone(),
        cover_url: w.cover_url.clone(),
        release_date: w.release_date.clone(),
        language: w.language.clone(),
        first_seen: Timestamp(w.first_seen),
        updated_at: Timestamp(w.updated_at),
    }
}

fn work_to_update(w: &Work) -> WorkUpdate {
    WorkUpdate {
        title: w.title.clone(),
        authors_json: Json(w.authors.clone()),
        author_asin: w.author_asin.as_ref().map(|a| a.0.clone()),
        series_name: w.series_name.clone(),
        series_asin: w.series_asin.as_ref().map(|a| a.0.clone()),
        series_position: w.series_position.clone(),
        cover_url: w.cover_url.clone(),
        release_date: w.release_date.clone(),
        language: w.language.clone(),
        updated_at: Timestamp(w.updated_at),
    }
}

impl From<WorkRow> for Work {
    fn from(r: WorkRow) -> Self {
        Work {
            asin: AsinId(r.asin),
            title: r.title,
            authors: r.authors_json.0,
            author_asin: r.author_asin.map(AsinId),
            series_name: r.series_name,
            series_asin: r.series_asin.map(AsinId),
            series_position: r.series_position,
            cover_url: r.cover_url,
            release_date: r.release_date,
            language: r.language,
            first_seen: r.first_seen.0,
            updated_at: r.updated_at.0,
        }
    }
}

#[async_trait]
impl WorksRepo for Store {
    async fn upsert_work(&self, work: &Work) -> Result<()> {
        let row = work_to_row(work);
        let upd = work_to_update(work);
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(works::table)
                        .values(&row)
                        .on_conflict(works::asin)
                        .do_update()
                        .set(&upd)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(works::table)
                        .values(&row)
                        .on_conflict(works::asin)
                        .do_update()
                        .set(&upd)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn upsert_works(&self, works: &[Work]) -> Result<()> {
        for w in works {
            self.upsert_work(w).await?;
        }
        Ok(())
    }

    async fn get_work(&self, asin: &AsinId) -> Result<Option<Work>> {
        let a = asin.0.clone();
        self.with_conn(move |conn| {
            let row: Option<WorkRow> = works::table
                .filter(works::asin.eq(a))
                .select(WorkRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(Work::from))
        })
        .await
    }

    async fn clear_work_author(&self, asin: &AsinId) -> Result<()> {
        let a = asin.0.clone();
        self.with_conn(move |conn| {
            diesel::update(works::table.filter(works::asin.eq(a)))
                .set(works::author_asin.eq(None::<String>))
                .execute(conn)
                .map(|_| ())
                .map_err(db_err)
        })
        .await
    }

    async fn list_works_by_author(&self, author_asin: &AsinId) -> Result<Vec<Work>> {
        let a = author_asin.0.clone();
        self.with_conn(move |conn| {
            let rows: Vec<WorkRow> = works::table
                .filter(works::author_asin.eq(a))
                .order(works::release_date.desc())
                .select(WorkRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(Work::from).collect())
        })
        .await
    }

    async fn list_works_by_series(&self, series_asin: &AsinId) -> Result<Vec<Work>> {
        let a = series_asin.0.clone();
        self.with_conn(move |conn| {
            let rows: Vec<WorkRow> = works::table
                .filter(works::series_asin.eq(a))
                .order(works::title.asc())
                .select(WorkRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(Work::from).collect())
        })
        .await
    }

    async fn list_all_works(&self) -> Result<Vec<Work>> {
        self.with_conn(move |conn| {
            let rows: Vec<WorkRow> = works::table
                .select(WorkRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(Work::from).collect())
        })
        .await
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = watchers)]
struct WatcherRow {
    scope_kind: String,
    scope_key: String,
    created_at: Timestamp,
}

#[async_trait]
impl WatchersRepo for Store {
    async fn set_watcher(&self, scope: WatchScope, key: &str) -> Result<()> {
        let row = WatcherRow {
            scope_kind: scope.as_str().to_string(),
            scope_key: key.to_string(),
            created_at: Timestamp(Utc::now()),
        };
        self.with_conn(move |conn| {
            conn.dispatch(
                |pg| {
                    diesel::insert_into(watchers::table)
                        .values(&row)
                        .on_conflict((watchers::scope_kind, watchers::scope_key))
                        .do_nothing()
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(watchers::table)
                        .values(&row)
                        .on_conflict((watchers::scope_kind, watchers::scope_key))
                        .do_nothing()
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn clear_watcher(&self, scope: WatchScope, key: &str) -> Result<()> {
        let kind = scope.as_str().to_string();
        let key = key.to_string();
        self.with_conn(move |conn| {
            diesel::delete(
                watchers::table
                    .filter(watchers::scope_kind.eq(kind))
                    .filter(watchers::scope_key.eq(key)),
            )
            .execute(conn)
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn list_watchers(&self) -> Result<Vec<Watcher>> {
        self.with_conn(move |conn| {
            let rows: Vec<WatcherRow> = watchers::table
                .select(WatcherRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows
                .into_iter()
                .filter_map(|r| {
                    WatchScope::parse(&r.scope_kind).map(|scope| Watcher {
                        scope,
                        key: r.scope_key,
                    })
                })
                .collect())
        })
        .await
    }

    async fn is_watched(&self, scope: WatchScope, key: &str) -> Result<bool> {
        let kind = scope.as_str().to_string();
        let key = key.to_string();
        self.with_conn(move |conn| {
            let n: i64 = watchers::table
                .filter(watchers::scope_kind.eq(kind))
                .filter(watchers::scope_key.eq(key))
                .count()
                .get_result(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;
    use skadi_core::Acquirable;

    use skadi_core::RootFolder;

    pub(super) async fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skadi.db");
        let url = format!("sqlite://{}", path.display());
        let store = Store::connect(&url).unwrap();
        store.run_migrations().await.unwrap();
        drop(store);
        {
            let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
            conn.run_pending_migrations(crate::SQLITE_MIGRATIONS)
                .unwrap();
        }
        (dir, Store::connect(&url).unwrap())
    }

    pub(super) fn sample_book(asin: &str) -> Book {
        Book::new(
            ExternalIds {
                asin: Some(AsinId(asin.into())),
                ..Default::default()
            },
            "Project Hail Mary",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        )
    }

    #[tokio::test]
    async fn works_upsert_get_list_and_preserve_first_seen() {
        let (_d, store) = fresh_store().await;
        let mut w = Work::new(AsinId("W1".into()), "The Way of Kings");
        w.authors = vec!["Brandon Sanderson".into()];
        w.author_asin = Some(AsinId("AU1".into()));
        w.series_name = Some("The Stormlight Archive".into());
        w.series_asin = Some(AsinId("S1".into()));
        w.series_position = Some("1".into());
        store.upsert_work(&w).await.unwrap();

        let got = store.get_work(&AsinId("W1".into())).await.unwrap().unwrap();
        assert_eq!(got.title, "The Way of Kings");
        assert_eq!(got.authors, vec!["Brandon Sanderson".to_string()]);
        assert_eq!(got.series_position.as_deref(), Some("1"));
        let first_seen = got.first_seen; // DB-rounded value to compare against

        // A second work shares the author + series.
        let mut w2 = Work::new(AsinId("W2".into()), "Words of Radiance");
        w2.author_asin = Some(AsinId("AU1".into()));
        w2.series_asin = Some(AsinId("S1".into()));
        w2.series_position = Some("2".into());
        store.upsert_work(&w2).await.unwrap();

        assert_eq!(
            store
                .list_works_by_author(&AsinId("AU1".into()))
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .list_works_by_series(&AsinId("S1".into()))
                .await
                .unwrap()
                .len(),
            2
        );

        // Re-upsert W1 with a new title: the update lands, first_seen is preserved.
        let w1b = Work::new(AsinId("W1".into()), "The Way of Kings (Updated)");
        store.upsert_work(&w1b).await.unwrap();
        let got2 = store.get_work(&AsinId("W1".into())).await.unwrap().unwrap();
        assert_eq!(got2.title, "The Way of Kings (Updated)");
        assert_eq!(
            got2.first_seen, first_seen,
            "first_seen preserved on update"
        );
    }

    #[tokio::test]
    async fn watchers_set_clear_list_and_query() {
        use crate::work::WatchScope;
        let (_d, store) = fresh_store().await;
        assert!(!store.is_watched(WatchScope::Author, "AU1").await.unwrap());

        store.set_watcher(WatchScope::Author, "AU1").await.unwrap();
        store.set_watcher(WatchScope::Author, "AU1").await.unwrap(); // idempotent
        store.set_watcher(WatchScope::Series, "S1").await.unwrap();

        assert!(store.is_watched(WatchScope::Author, "AU1").await.unwrap());
        assert!(!store.is_watched(WatchScope::Book, "AU1").await.unwrap());
        assert_eq!(store.list_watchers().await.unwrap().len(), 2);

        store
            .clear_watcher(WatchScope::Author, "AU1")
            .await
            .unwrap();
        assert!(!store.is_watched(WatchScope::Author, "AU1").await.unwrap());
        assert_eq!(store.list_watchers().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn author_crud_and_get_by_asin() {
        let (_d, store) = fresh_store().await;
        let mut a = Author::new("Andy Weir");
        a.asin = Some(AsinId("A1".into()));
        // The store holds timestamps to **microsecond** precision, so a value
        // read back is not bit-identical to one built from `Utc::now()` when
        // the clock offers more. Truncate before storing, and the round trip
        // is an equality the store can actually honour.
        //
        // This went unnoticed because macOS clocks stop at microseconds — the
        // assertion passed on the machine it was written on and failed on the
        // Linux runner, where nanoseconds survive (2026-09-23).
        a.added_at = chrono::DateTime::from_timestamp_micros(a.added_at.timestamp_micros())
            .expect("a timestamp from Utc::now() is in range");
        store.upsert_author(&a).await.unwrap();

        assert_eq!(store.get_author(a.id).await.unwrap().unwrap(), a);
        assert_eq!(
            store
                .get_author_by_asin(&AsinId("A1".into()))
                .await
                .unwrap()
                .unwrap()
                .id,
            a.id
        );
        assert_eq!(
            store
                .list_authors(AuthorFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
        store.delete_author(a.id).await.unwrap();
        assert!(store.get_author(a.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn book_round_trips_with_series_and_file() {
        let (_d, store) = fresh_store().await;

        // An author + a series the book belongs to.
        let mut author = Author::new("Brandon Sanderson");
        author.asin = Some(AsinId("AU".into()));
        store.upsert_author(&author).await.unwrap();
        let series = Series::new("Stormlight Archive");
        store.upsert_series(&series).await.unwrap();

        let mut book = sample_book("B1");
        book.author_id = Some(author.id);
        book.authors = vec!["Brandon Sanderson".into()];
        book.narrators = vec!["Michael Kramer".into()];
        book.year = Some(2010);
        book.series = Some(SeriesLink {
            series_id: series.id,
            name: series.name.clone(),
            position: Some("1".into()),
        });
        book.release_date = NaiveDate::from_ymd_opt(2010, 8, 31);
        store.upsert_book(&book).await.unwrap();
        store
            .upsert_book_file(&BookFile::missing(book.id))
            .await
            .unwrap();

        let got = store.get_book(book.id).await.unwrap().unwrap();
        assert_eq!(got.title, "Project Hail Mary");
        assert_eq!(got.narrators, vec!["Michael Kramer".to_string()]);
        assert_eq!(got.release_date, NaiveDate::from_ymd_opt(2010, 8, 31));
        let link = got.series.expect("series populated");
        assert_eq!(link.name, "Stormlight Archive");
        assert_eq!(link.position.as_deref(), Some("1"));
        assert_eq!(got.files.len(), 1, "book file hydrated");

        // get_book_by_asin + list_books_by_author
        assert_eq!(
            store
                .get_book_by_asin(&AsinId("B1".into()))
                .await
                .unwrap()
                .unwrap()
                .id,
            book.id
        );
        assert_eq!(
            store.list_books_by_author(author.id).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn set_book_file_status_imported_records_file() {
        let (_d, store) = fresh_store().await;
        let book = sample_book("B2");
        store.upsert_book(&book).await.unwrap();
        let file = BookFile::missing(book.id);
        store.upsert_book_file(&file).await.unwrap();

        let q = QualityId::new();
        store
            .set_book_file_status(
                file.id,
                AcquisitionStatus::Imported {
                    file: FileRef {
                        path: PathBuf::from("/audiobooks/x.m4b"),
                    },
                    quality: q,
                    score: 7,
                    at: Utc::now(),
                },
            )
            .await
            .unwrap();

        let got = store.get_book_file(file.id).await.unwrap().unwrap();
        assert!(matches!(got.status, AcquisitionStatus::Imported { .. }));
        assert_eq!(got.quality, Some(q));
        assert_eq!(got.format_score, 7);
        assert!(!got.wanted());

        // get_by_ref decodes the acquirable ref
        let by_ref = store
            .get_book_file_by_ref(&file.acquirable_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_ref.id, file.id);
    }
}

#[cfg(test)]
mod edition_tests {
    use super::tests::{fresh_store, sample_book};
    use super::*;

    /// The whole point of SKADI-T-0448: one book, several editions.
    ///
    /// Before this, `book_files` declared `UNIQUE(book_id)`, so a Full Cast
    /// release of a book already owned became a second `books` row with its own
    /// monitored flag — the root of SKADI-T-0400, where the sweep then went and
    /// fetched an edition of a book already on disk.
    #[tokio::test]
    async fn one_book_holds_several_editions() {
        let (_d, store) = fresh_store().await;
        let book = sample_book("B00TEST001");
        store.upsert_book(&book).await.unwrap();

        for kind in ["unabridged", "full_cast"] {
            let f = BookFile::missing_of_kind(book.id, kind);
            store.upsert_book_file(&f).await.unwrap();
        }

        let mut kinds: Vec<String> = store
            .list_book_files(book.id)
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.kind)
            .collect();
        kinds.sort();
        assert_eq!(
            kinds,
            vec!["full_cast".to_string(), "unabridged".to_string()]
        );
    }

    #[tokio::test]
    async fn a_second_row_of_the_same_kind_is_still_refused() {
        // `UNIQUE(book_id, kind_slug)`: several editions, but only one row per
        // kind. Without this a re-ingest would accumulate duplicate Unabridged
        // rows and the sweep would acquire the same edition repeatedly.
        let (_d, store) = fresh_store().await;
        let book = sample_book("B00TEST002");
        store.upsert_book(&book).await.unwrap();

        store
            .upsert_book_file(&BookFile::missing_of_kind(book.id, "unabridged"))
            .await
            .unwrap();
        // A *different* row id, same (book, kind).
        let dup = BookFile::missing_of_kind(book.id, "unabridged");
        assert!(
            store.upsert_book_file(&dup).await.is_err(),
            "a second row of the same kind must be refused"
        );
    }

    #[tokio::test]
    async fn an_editions_monitored_flag_round_trips() {
        let (_d, store) = fresh_store().await;
        let book = sample_book("B00TEST004");
        store.upsert_book(&book).await.unwrap();

        store
            .upsert_book_file(&BookFile::missing_of_kind(book.id, "unabridged"))
            .await
            .unwrap();
        store
            .upsert_book_file(&BookFile::discovered_of_kind(book.id, "full_cast"))
            .await
            .unwrap();

        let files = store.list_book_files(book.id).await.unwrap();
        let wanted = files.iter().find(|f| f.kind == "unabridged").unwrap();
        let discovered = files.iter().find(|f| f.kind == "full_cast").unwrap();
        assert!(wanted.monitored, "the edition the operator asked for");
        assert!(
            !discovered.monitored,
            "a discovered edition is visible but not wanted — if this flips, the \
             sweep acquires an edition of a book already on disk, which is what \
             SKADI-T-0400 exists to stop"
        );
    }

    #[tokio::test]
    async fn a_file_defaults_to_the_unabridged_edition() {
        // Everything that predates kinds is Unabridged — what the ingest path
        // produced before editions existed, and what the migration backfilled.
        let (_d, store) = fresh_store().await;
        let book = sample_book("B00TEST003");
        store.upsert_book(&book).await.unwrap();
        store
            .upsert_book_file(&BookFile::missing(book.id))
            .await
            .unwrap();
        let files = store.list_book_files(book.id).await.unwrap();
        assert_eq!(files[0].kind, crate::book_file::DEFAULT_EDITION_KIND);
    }
}
