//! Generic configuration store (SKADI-T-0053).
//!
//! The daemon's settings entities — indexers, downloaders, quality profiles,
//! root folders, notifiers — are heterogeneous JSON documents (different indexer
//! types carry different fields, etc.). Rather than a rigid typed schema per
//! kind, they share one `settings` table keyed by `(kind, id)` with a JSON
//! `body`. The HTTP layer ([`skadi-api`]) validates the typed shape per kind;
//! this repo is the backend-agnostic persistence.
//!
//! `edition_kinds` is deliberately **not** here — it lives in `skadi-movies`
//! with its own typed repo and built-in-row protection.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::{Json, Timestamp};

use skadi_core::Result;

use crate::schema::settings;
use crate::{Store, db_err};

/// One stored settings document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingRecord {
    pub kind: String,
    pub id: String,
    pub body: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// CRUD over the generic `(kind, id) -> JSON` settings store.
#[async_trait]
pub trait SettingsRepo {
    /// All documents of a kind, ordered by id.
    async fn list_settings(&self, kind: &str) -> Result<Vec<SettingRecord>>;
    /// One document, if present.
    async fn get_setting(&self, kind: &str, id: &str) -> Result<Option<SettingRecord>>;
    /// Insert or replace a document's body, returning the stored record.
    /// `created_at` is preserved across updates; `updated_at` is stamped now.
    async fn put_setting(
        &self,
        kind: &str,
        id: &str,
        body: &serde_json::Value,
    ) -> Result<SettingRecord>;
    /// Delete a document. Returns whether a row existed.
    async fn delete_setting(&self, kind: &str, id: &str) -> Result<bool>;
}

/// The stored row. `body` is the portable `Json` type (`TEXT`/`jsonb`);
/// timestamps are portable `Timestamp`.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = settings)]
struct Row {
    kind: String,
    id: String,
    body: Json<serde_json::Value>,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl From<Row> for SettingRecord {
    fn from(r: Row) -> Self {
        SettingRecord {
            kind: r.kind,
            id: r.id,
            body: r.body.0,
            created_at: r.created_at.0,
            updated_at: r.updated_at.0,
        }
    }
}

#[async_trait]
impl SettingsRepo for Store {
    async fn list_settings(&self, kind: &str) -> Result<Vec<SettingRecord>> {
        let kind = kind.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<Row> = settings::table
                .filter(settings::kind.eq(kind))
                .select(Row::as_select())
                .order(settings::id.asc())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(SettingRecord::from).collect())
        })
        .await
    }

    async fn get_setting(&self, kind: &str, id: &str) -> Result<Option<SettingRecord>> {
        let (kind, id) = (kind.to_string(), id.to_string());
        self.with_conn(move |conn| {
            let row: Option<Row> = settings::table
                .find((kind, id))
                .select(Row::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(SettingRecord::from))
        })
        .await
    }

    async fn put_setting(
        &self,
        kind: &str,
        id: &str,
        body: &serde_json::Value,
    ) -> Result<SettingRecord> {
        let now = Utc::now();
        let row = Row {
            kind: kind.to_string(),
            id: id.to_string(),
            body: Json(body.clone()),
            created_at: Timestamp(now),
            updated_at: Timestamp(now),
        };
        let (k, i) = (kind.to_string(), id.to_string());
        self.with_conn(move |conn| {
            // INSERT sets created_at; on conflict only body + updated_at change,
            // so the original created_at is preserved across updates.
            conn.dispatch(
                |pg| {
                    diesel::insert_into(settings::table)
                        .values(&row)
                        .on_conflict((settings::kind, settings::id))
                        .do_update()
                        .set((
                            settings::body.eq(&row.body),
                            settings::updated_at.eq(&row.updated_at),
                        ))
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(settings::table)
                        .values(&row)
                        .on_conflict((settings::kind, settings::id))
                        .do_update()
                        .set((
                            settings::body.eq(&row.body),
                            settings::updated_at.eq(&row.updated_at),
                        ))
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            let stored: Row = settings::table
                .find((k, i))
                .select(Row::as_select())
                .first(conn)
                .map_err(db_err)?;
            Ok(SettingRecord::from(stored))
        })
        .await
    }

    async fn delete_setting(&self, kind: &str, id: &str) -> Result<bool> {
        let (kind, id) = (kind.to_string(), id.to_string());
        self.with_conn(move |conn| {
            let n = diesel::delete(settings::table.find((kind, id)))
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
        let path = skadi_core::unique_temp_path("settings").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn settings_crud_round_trip_sqlite() {
        let store = temp_store().await;

        assert!(store.list_settings("indexers").await.unwrap().is_empty());
        assert!(store.get_setting("indexers", "x").await.unwrap().is_none());

        let body = serde_json::json!({ "name": "nzbgeek", "api_key": "abc" });
        let rec = store.put_setting("indexers", "i1", &body).await.unwrap();
        assert_eq!(rec.kind, "indexers");
        assert_eq!(rec.id, "i1");
        assert_eq!(rec.body, body);

        assert_eq!(
            store.get_setting("indexers", "i1").await.unwrap(),
            Some(rec.clone())
        );

        assert!(store.list_settings("downloaders").await.unwrap().is_empty());

        let body2 = serde_json::json!({ "name": "nzbgeek", "api_key": "xyz" });
        let updated = store.put_setting("indexers", "i1", &body2).await.unwrap();
        assert_eq!(updated.body, body2);
        assert_eq!(updated.created_at, rec.created_at, "created_at preserved");

        let all = store.list_settings("indexers").await.unwrap();
        assert_eq!(all.len(), 1);

        assert!(store.delete_setting("indexers", "i1").await.unwrap());
        assert!(!store.delete_setting("indexers", "i1").await.unwrap());
        assert!(store.get_setting("indexers", "i1").await.unwrap().is_none());
    }
}
