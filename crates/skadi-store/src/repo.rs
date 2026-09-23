//! Repository layer.
//!
//! Repository traits are defined here and implemented on [`Store`] over a single
//! [`DualConnection`](crate::DualConnection) code path (SKADI-I-0015). Diesel row
//! models are mechanical 1:1 mappings using portable column types; domain-facing
//! types like [`DomainState`] are converted at this boundary so callers never
//! see Diesel.
//!
//! `DomainStateRepo` is the first concrete repository and the template every
//! future domain repo (e.g. movies) follows.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::{Json, Timestamp};

use skadi_core::Result;

use crate::schema::domains;
use crate::{Store, db_err};

/// Domain-facing runtime state for one registered domain module (the `domains`
/// table). Backend-agnostic: timestamps are always `DateTime<Utc>` and settings
/// are always a JSON value, regardless of how each backend stores them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainState {
    pub name: String,
    pub enabled: bool,
    pub enabled_at: Option<DateTime<Utc>>,
    pub settings: serde_json::Value,
}

impl DomainState {
    /// A disabled domain with empty settings.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: false,
            enabled_at: None,
            settings: serde_json::json!({}),
        }
    }
}

/// CRUD access to per-domain runtime enable/disable state.
#[async_trait]
pub trait DomainStateRepo {
    /// All known domain states, ordered by name.
    async fn list(&self) -> Result<Vec<DomainState>>;
    /// The state for one domain, if present.
    async fn get(&self, name: &str) -> Result<Option<DomainState>>;
    /// Insert or replace a domain's full state.
    async fn upsert(&self, state: &DomainState) -> Result<()>;
    /// Toggle a domain's enabled flag, stamping `enabled_at` when enabling and
    /// clearing it when disabling. Returns the resulting state (creating a row
    /// with default settings if the domain was not yet present).
    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<DomainState>;
}

/// The stored row. `enabled_at` and `settings_json` use the portable
/// `Timestamp`/`Json` types, unifying the old per-backend row models.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = domains)]
struct Row {
    domain_name: String,
    enabled: bool,
    enabled_at: Option<Timestamp>,
    settings_json: Json<serde_json::Value>,
}

impl From<Row> for DomainState {
    fn from(r: Row) -> Self {
        DomainState {
            name: r.domain_name,
            enabled: r.enabled,
            enabled_at: r.enabled_at.map(|t| t.0),
            settings: r.settings_json.0,
        }
    }
}

fn to_row(s: &DomainState) -> Row {
    Row {
        domain_name: s.name.clone(),
        enabled: s.enabled,
        enabled_at: s.enabled_at.map(Timestamp),
        settings_json: Json(s.settings.clone()),
    }
}

#[async_trait]
impl DomainStateRepo for Store {
    async fn list(&self) -> Result<Vec<DomainState>> {
        self.with_conn(|conn| {
            let rows: Vec<Row> = domains::table
                .select(Row::as_select())
                .order(domains::domain_name.asc())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(DomainState::from).collect())
        })
        .await
    }

    async fn get(&self, name: &str) -> Result<Option<DomainState>> {
        let name = name.to_string();
        self.with_conn(move |conn| {
            let row: Option<Row> = domains::table
                .find(name)
                .select(Row::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            Ok(row.map(DomainState::from))
        })
        .await
    }

    async fn upsert(&self, state: &DomainState) -> Result<()> {
        let row = to_row(state);
        self.with_conn(move |conn| {
            // `on_conflict` isn't supported through MultiBackend — dispatch the
            // (identical) upsert per backend.
            conn.dispatch(
                |pg| {
                    diesel::insert_into(domains::table)
                        .values(&row)
                        .on_conflict(domains::domain_name)
                        .do_update()
                        .set((
                            domains::enabled.eq(row.enabled),
                            domains::enabled_at.eq(&row.enabled_at),
                            domains::settings_json.eq(&row.settings_json),
                        ))
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(domains::table)
                        .values(&row)
                        .on_conflict(domains::domain_name)
                        .do_update()
                        .set((
                            domains::enabled.eq(row.enabled),
                            domains::enabled_at.eq(&row.enabled_at),
                            domains::settings_json.eq(&row.settings_json),
                        ))
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<DomainState> {
        let mut state = self
            .get(name)
            .await?
            .unwrap_or_else(|| DomainState::new(name));
        state.enabled = enabled;
        state.enabled_at = enabled.then(Utc::now);
        self.upsert(&state).await?;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_sqlite_store() -> Store {
        let path = skadi_core::unique_temp_path("repotest").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn upsert_get_list_round_trip_sqlite() {
        let store = temp_sqlite_store().await;

        assert!(store.list().await.unwrap().is_empty());

        let mut movies = DomainState::new("movies");
        movies.settings = serde_json::json!({ "root": "/movies" });
        store.upsert(&movies).await.unwrap();
        assert_eq!(store.get("movies").await.unwrap(), Some(movies.clone()));

        assert_eq!(store.get("music").await.unwrap(), None);

        let enabled = store.set_enabled("movies", true).await.unwrap();
        assert!(enabled.enabled);
        assert!(enabled.enabled_at.is_some());
        assert_eq!(enabled.settings, movies.settings, "settings preserved");

        store.set_enabled("books", true).await.unwrap();

        let all = store.list().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "books", "ordered by name");
        assert_eq!(all[1].name, "movies");
        assert!(all.iter().all(|d| d.enabled));

        let disabled = store.set_enabled("movies", false).await.unwrap();
        assert!(!disabled.enabled);
        assert!(disabled.enabled_at.is_none());
    }
}
