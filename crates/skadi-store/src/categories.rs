//! First-class download categories (SKADI-T-0215) — categories are real rows,
//! not free-text strings on a download.
//!
//! A download's free-text `category` resolves to one of these rows, which carry a
//! **save-path** (where the worker hardlinks finished files for that category) and
//! optional **per-category seed-policy overrides** (ratio / time / action). Any NULL
//! override falls back to the worker's global setting; the worker resolves the row
//! once at job start. Pure storage — the override *semantics* live in the worker.

use async_trait::async_trait;
use chrono::Utc;
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::download_categories;
use crate::{Store, db_err};

/// A download category: a name plus a save-path and optional seed overrides. A
/// `None` override means "use the worker's global setting".
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadCategory {
    pub name: String,
    /// Where finished files for this category are hardlinked (overrides the
    /// downloader's global complete dir). `None` ⇒ use the global dir.
    pub save_path: Option<String>,
    /// Per-category seed-ratio limit (`> 0`); `None` ⇒ global.
    pub seed_ratio: Option<f64>,
    /// Per-category seed-time limit in minutes (`> 0`); `None` ⇒ global.
    pub seed_time_mins: Option<i64>,
    /// Per-category seed action (`stop` | `remove`); `None` ⇒ global.
    pub seed_action: Option<String>,
}

impl DownloadCategory {
    /// A name-only category (no overrides) — the simplest useful object.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            save_path: None,
            seed_ratio: None,
            seed_time_mins: None,
            seed_action: None,
        }
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = download_categories)]
struct Row {
    name: String,
    save_path: Option<String>,
    // Stored as text to mirror the `worker.seed_ratio` config key (a string float).
    seed_ratio: Option<String>,
    seed_time_mins: Option<i64>,
    seed_action: Option<String>,
    updated_at: Timestamp,
}

impl From<Row> for DownloadCategory {
    fn from(r: Row) -> Self {
        DownloadCategory {
            name: r.name,
            save_path: r.save_path,
            // A malformed stored ratio degrades to "no override" rather than erroring.
            seed_ratio: r.seed_ratio.and_then(|s| s.parse().ok()),
            seed_time_mins: r.seed_time_mins,
            seed_action: r.seed_action,
        }
    }
}

/// CRUD over the `download_categories` table.
#[async_trait]
pub trait DownloadCategoryRepo: Send + Sync {
    /// Insert or replace a category by name; returns the stored object.
    async fn upsert_category(&self, cat: &DownloadCategory) -> Result<DownloadCategory>;
    /// Fetch one category by name.
    async fn get_category(&self, name: &str) -> Result<Option<DownloadCategory>>;
    /// All categories, name-ascending.
    async fn list_categories(&self) -> Result<Vec<DownloadCategory>>;
    /// Delete a category; returns whether a row was removed.
    async fn delete_category(&self, name: &str) -> Result<bool>;
}

#[async_trait]
impl DownloadCategoryRepo for Store {
    async fn upsert_category(&self, cat: &DownloadCategory) -> Result<DownloadCategory> {
        let row = Row {
            name: cat.name.clone(),
            save_path: cat.save_path.clone(),
            seed_ratio: cat.seed_ratio.map(|r| r.to_string()),
            seed_time_mins: cat.seed_time_mins,
            seed_action: cat.seed_action.clone(),
            updated_at: Timestamp(Utc::now()),
        };
        self.with_conn(move |conn| {
            // `on_conflict` can't go through MultiBackend, so run the (identical)
            // upsert per backend via the dispatch escape hatch (mirrors `config`).
            let set = (
                download_categories::save_path.eq(&row.save_path),
                download_categories::seed_ratio.eq(&row.seed_ratio),
                download_categories::seed_time_mins.eq(&row.seed_time_mins),
                download_categories::seed_action.eq(&row.seed_action),
                download_categories::updated_at.eq(&row.updated_at),
            );
            conn.dispatch(
                |pg| {
                    diesel::insert_into(download_categories::table)
                        .values(&row)
                        .on_conflict(download_categories::name)
                        .do_update()
                        .set(set)
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(download_categories::table)
                        .values(&row)
                        .on_conflict(download_categories::name)
                        .do_update()
                        .set(set)
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            let stored: Row = download_categories::table
                .find(&row.name)
                .select(Row::as_select())
                .first(conn)
                .map_err(db_err)?;
            Ok(DownloadCategory::from(stored))
        })
        .await
    }

    async fn get_category(&self, name: &str) -> Result<Option<DownloadCategory>> {
        let name = name.to_string();
        self.with_conn(move |conn| {
            let row: Option<Row> = download_categories::table
                .find(&name)
                .select(Row::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(DownloadCategory::from))
        })
        .await
    }

    async fn list_categories(&self) -> Result<Vec<DownloadCategory>> {
        self.with_conn(|conn| {
            let rows: Vec<Row> = download_categories::table
                .order(download_categories::name.asc())
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(DownloadCategory::from).collect())
        })
        .await
    }

    async fn delete_category(&self, name: &str) -> Result<bool> {
        let name = name.to_string();
        self.with_conn(move |conn| {
            let n = diesel::delete(download_categories::table.find(&name))
                .execute(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("cats").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn upsert_get_list_delete_round_trip() {
        let store = temp_store().await;
        assert!(store.get_category("movies").await.unwrap().is_none());

        let cat = DownloadCategory {
            name: "movies".into(),
            save_path: Some("/media/movies".into()),
            seed_ratio: Some(2.0),
            seed_time_mins: Some(1440),
            seed_action: Some("remove".into()),
        };
        let stored = store.upsert_category(&cat).await.unwrap();
        assert_eq!(stored, cat);

        // Round-trips through the text-encoded ratio.
        let got = store.get_category("movies").await.unwrap().unwrap();
        assert_eq!(got.seed_ratio, Some(2.0));
        assert_eq!(got.save_path.as_deref(), Some("/media/movies"));

        // Upsert replaces.
        let updated = DownloadCategory {
            save_path: Some("/media/films".into()),
            seed_ratio: None,
            ..DownloadCategory::named("movies")
        };
        store.upsert_category(&updated).await.unwrap();
        let got = store.get_category("movies").await.unwrap().unwrap();
        assert_eq!(got.save_path.as_deref(), Some("/media/films"));
        assert_eq!(got.seed_ratio, None, "cleared override");

        // A name-only second category, then list is name-ascending.
        store
            .upsert_category(&DownloadCategory::named("audiobooks"))
            .await
            .unwrap();
        let names: Vec<String> = store
            .list_categories()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["audiobooks".to_string(), "movies".to_string()]);

        assert!(store.delete_category("movies").await.unwrap());
        assert!(!store.delete_category("movies").await.unwrap());
        assert!(store.get_category("movies").await.unwrap().is_none());
    }
}
