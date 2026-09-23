//! The `config` key/value store (SKADI-T-0099) — storage for the unified config
//! plane (SKADI-I-0014).
//!
//! A scalar key→value table that is the runtime source of truth for process
//! configuration. On boot the daemon upserts every `SKADI_*` env var here
//! (`source = env`, env overwrites); services read from it and react per their
//! own policy. Distinct from [`settings`](crate::SettingsRepo), which holds
//! multi-row provider *entities*. The *schema* (key names, defaults, types) is
//! owned by the `skadi-config` crate; this module is pure storage, value-as-text.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::{AppError, Result};

use crate::schema::config;
use crate::{Store, db_err};

/// Where a config value came from. Env values are written at boot (and
/// overwrite); runtime values come from the API/UI between boots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    Env,
    Runtime,
}

impl ConfigSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigSource::Env => "env",
            ConfigSource::Runtime => "runtime",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "env" => Ok(ConfigSource::Env),
            "runtime" => Ok(ConfigSource::Runtime),
            other => Err(AppError::Internal(format!(
                "invalid config.source {other:?}"
            ))),
        }
    }
}

/// One stored config entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
    pub source: ConfigSource,
    pub updated_at: DateTime<Utc>,
}

/// CRUD + change-detection over the `config` table.
#[async_trait]
pub trait ConfigRepo: Send + Sync {
    /// Fetch one entry by key.
    async fn get_config(&self, key: &str) -> Result<Option<ConfigEntry>>;
    /// Upsert a key to `value` with the given `source` (env overwrites runtime).
    async fn set_config(&self, key: &str, value: &str, source: ConfigSource)
    -> Result<ConfigEntry>;
    /// All entries, ordered by key.
    async fn list_config(&self) -> Result<Vec<ConfigEntry>>;
    /// Remove a key; returns whether a row was deleted.
    async fn delete_config(&self, key: &str) -> Result<bool>;
    /// A cheap fingerprint of the whole config (every `(key, value, updated_at)`),
    /// for change-detection on the supervisor tick — mirrors `provider_fingerprint`.
    async fn config_fingerprint(&self) -> Result<u64>;
}

/// The stored row. Portable `Timestamp` unifies the old NaiveDateTime/DateTime
/// split into a single `DateTime<Utc>`-backed type across both backends.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = config)]
struct Row {
    key: String,
    value: String,
    source: String,
    updated_at: Timestamp,
}

impl TryFrom<Row> for ConfigEntry {
    type Error = AppError;
    fn try_from(r: Row) -> Result<Self> {
        Ok(ConfigEntry {
            key: r.key,
            value: r.value,
            source: ConfigSource::parse(&r.source)?,
            updated_at: r.updated_at.0,
        })
    }
}

/// Hash every entry's `(key, value, updated_at)` into a stable fingerprint.
/// Entries must be pre-sorted by key for determinism.
fn fingerprint_entries(entries: &[ConfigEntry]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for e in entries {
        e.key.hash(&mut h);
        e.value.hash(&mut h);
        e.updated_at.to_rfc3339().hash(&mut h);
    }
    h.finish()
}

#[async_trait]
impl ConfigRepo for Store {
    async fn get_config(&self, key: &str) -> Result<Option<ConfigEntry>> {
        let key = key.to_string();
        self.with_conn(move |conn| {
            let row: Option<Row> = config::table
                .find(&key)
                .select(Row::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(ConfigEntry::try_from).transpose()
        })
        .await
    }

    async fn set_config(
        &self,
        key: &str,
        value: &str,
        source: ConfigSource,
    ) -> Result<ConfigEntry> {
        let row = Row {
            key: key.to_string(),
            value: value.to_string(),
            source: source.as_str().to_string(),
            updated_at: Timestamp(Utc::now()),
        };
        self.with_conn(move |conn| {
            // `on_conflict` can't go through MultiBackend, so run the (identical)
            // upsert per backend via the dispatch escape hatch.
            conn.dispatch(
                |pg| {
                    diesel::insert_into(config::table)
                        .values(&row)
                        .on_conflict(config::key)
                        .do_update()
                        .set((
                            config::value.eq(&row.value),
                            config::source.eq(&row.source),
                            config::updated_at.eq(&row.updated_at),
                        ))
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(config::table)
                        .values(&row)
                        .on_conflict(config::key)
                        .do_update()
                        .set((
                            config::value.eq(&row.value),
                            config::source.eq(&row.source),
                            config::updated_at.eq(&row.updated_at),
                        ))
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            let stored: Row = config::table
                .find(&row.key)
                .select(Row::as_select())
                .first(conn)
                .map_err(db_err)?;
            ConfigEntry::try_from(stored)
        })
        .await
    }

    async fn list_config(&self) -> Result<Vec<ConfigEntry>> {
        self.with_conn(|conn| {
            let rows: Vec<Row> = config::table
                .order(config::key.asc())
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(ConfigEntry::try_from).collect()
        })
        .await
    }

    async fn delete_config(&self, key: &str) -> Result<bool> {
        let key = key.to_string();
        self.with_conn(move |conn| {
            let n = diesel::delete(config::table.find(&key))
                .execute(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }

    async fn config_fingerprint(&self) -> Result<u64> {
        Ok(fingerprint_entries(&self.list_config().await?))
    }
}
