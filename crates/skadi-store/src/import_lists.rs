//! Import lists and their exclusions (SKADI-T-0511, component C19).
//!
//! An import list is a external collection — a TMDB collection, a Trakt list —
//! that the daemon periodically syncs into the library. Sonarr and Radarr both
//! have this, and it is how most people actually populate a library rather than
//! adding items one at a time.
//!
//! Cross-domain by design, like [`crate::item_tags`]: a list names the domain it
//! targets rather than living in that domain's schema, so adding a provider or a
//! field is one migration instead of three.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

use skadi_core::{AppError, Result};

use crate::schema::{import_list_exclusions, import_lists};
use crate::{Store, db_err};

/// A configured import list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportList {
    pub id: String,
    pub name: String,
    /// Provider slug — `tmdb_collection`, `trakt_list`, …
    pub kind: String,
    /// `movie` | `series` | `audiobook`.
    pub target_domain: String,
    /// Provider-specific settings, opaque here so a new provider needs no
    /// migration.
    pub settings: serde_json::Value,
    pub profile_id: Option<String>,
    pub root_folder: Option<String>,
    /// Whether items arrive monitored.
    ///
    /// **Defaults false.** A list that silently starts acquiring on its first
    /// sync is how someone wakes up to a full disk; opting in is one toggle,
    /// undoing a hundred grabs is not.
    pub add_monitored: bool,
    pub enabled: bool,
    pub interval_minutes: i32,
    pub last_synced_at: Option<DateTime<Utc>>,
    /// Why the last sync failed, if it did — kept on the row so the list page can
    /// show a broken list without cross-referencing the log.
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ImportList {
    /// A new list with the safe defaults: enabled, unmonitored, twice daily.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        domain: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            kind: kind.into(),
            target_domain: domain.into(),
            settings: serde_json::Value::Object(serde_json::Map::new()),
            profile_id: None,
            root_folder: None,
            add_monitored: false,
            enabled: true,
            interval_minutes: 720,
            last_synced_at: None,
            last_error: None,
            created_at: Utc::now(),
        }
    }

    /// Whether this list is due a sync at `now`.
    ///
    /// A list that has never synced is always due — otherwise adding a list does
    /// nothing visible until its first interval elapses, and the operator
    /// reasonably concludes it is broken.
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_synced_at {
            None => true,
            Some(last) => {
                now.signed_duration_since(last)
                    >= chrono::Duration::minutes(self.interval_minutes.max(1) as i64)
            }
        }
    }
}

/// An item the operator never wants added, however many lists offer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportListExclusion {
    pub id: String,
    pub target_domain: String,
    /// Which external namespace `external_id` belongs to — `tmdb`, `tvdb`,
    /// `imdb`, `asin`. Without it an IMDb id and a TMDB id could collide as bare
    /// strings.
    pub id_kind: String,
    pub external_id: String,
    /// Kept for the UI: a list of bare ids is unreviewable.
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Import-list storage.
#[async_trait]
pub trait ImportListRepo: Send + Sync {
    async fn list_import_lists(&self) -> Result<Vec<ImportList>>;
    async fn get_import_list(&self, id: &str) -> Result<Option<ImportList>>;
    /// Insert or replace by id.
    async fn upsert_import_list(&self, list: &ImportList) -> Result<()>;
    async fn delete_import_list(&self, id: &str) -> Result<()>;

    /// Record the outcome of a sync run.
    ///
    /// Separate from `upsert_import_list` so a sync cannot clobber an edit the
    /// operator made while it was running — it writes two columns, not the row.
    async fn record_sync(&self, id: &str, at: DateTime<Utc>, error: Option<&str>) -> Result<()>;

    async fn list_exclusions(
        &self,
        target_domain: Option<&str>,
    ) -> Result<Vec<ImportListExclusion>>;
    async fn add_exclusion(&self, e: &ImportListExclusion) -> Result<()>;
    async fn delete_exclusion(&self, id: &str) -> Result<()>;
    /// Whether this external id is excluded for this domain.
    async fn is_excluded(
        &self,
        target_domain: &str,
        id_kind: &str,
        external_id: &str,
    ) -> Result<bool>;
}

#[derive(Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = import_lists)]
#[diesel(check_for_backend(diesel_dualdb::MultiBackend))]
struct ListRow {
    id: String,
    name: String,
    kind: String,
    target_domain: String,
    settings_json: String,
    profile_id: Option<String>,
    root_folder: Option<String>,
    add_monitored: bool,
    enabled: bool,
    interval_minutes: i32,
    last_synced_at: Option<diesel_dualdb::types::Timestamp>,
    last_error: Option<String>,
    created_at: diesel_dualdb::types::Timestamp,
}

impl TryFrom<ListRow> for ImportList {
    type Error = AppError;
    fn try_from(r: ListRow) -> Result<Self> {
        Ok(ImportList {
            id: r.id,
            name: r.name,
            kind: r.kind,
            target_domain: r.target_domain,
            settings: serde_json::from_str(&r.settings_json)
                .map_err(|e| AppError::Internal(format!("import list settings: {e}")))?,
            profile_id: r.profile_id,
            root_folder: r.root_folder,
            add_monitored: r.add_monitored,
            enabled: r.enabled,
            interval_minutes: r.interval_minutes,
            last_synced_at: r.last_synced_at.map(|t| t.0),
            last_error: r.last_error,
            created_at: r.created_at.0,
        })
    }
}

fn list_to_row(l: &ImportList) -> Result<ListRow> {
    Ok(ListRow {
        id: l.id.clone(),
        name: l.name.clone(),
        kind: l.kind.clone(),
        target_domain: l.target_domain.clone(),
        settings_json: serde_json::to_string(&l.settings)
            .map_err(|e| AppError::Internal(format!("import list settings: {e}")))?,
        profile_id: l.profile_id.clone(),
        root_folder: l.root_folder.clone(),
        add_monitored: l.add_monitored,
        enabled: l.enabled,
        interval_minutes: l.interval_minutes,
        last_synced_at: l.last_synced_at.map(diesel_dualdb::types::Timestamp),
        last_error: l.last_error.clone(),
        created_at: diesel_dualdb::types::Timestamp(l.created_at),
    })
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = import_list_exclusions)]
#[diesel(check_for_backend(diesel_dualdb::MultiBackend))]
struct ExclusionRow {
    id: String,
    target_domain: String,
    id_kind: String,
    external_id: String,
    title: Option<String>,
    created_at: diesel_dualdb::types::Timestamp,
}

impl From<ExclusionRow> for ImportListExclusion {
    fn from(r: ExclusionRow) -> Self {
        ImportListExclusion {
            id: r.id,
            target_domain: r.target_domain,
            id_kind: r.id_kind,
            external_id: r.external_id,
            title: r.title,
            created_at: r.created_at.0,
        }
    }
}

#[async_trait]
impl ImportListRepo for Store {
    async fn list_import_lists(&self) -> Result<Vec<ImportList>> {
        self.with_conn(|conn| {
            let rows: Vec<ListRow> = import_lists::table
                .order(import_lists::created_at.asc())
                .select(ListRow::as_select())
                .load(conn)
                .map_err(db_err)?;
            rows.into_iter().map(ImportList::try_from).collect()
        })
        .await
    }

    async fn get_import_list(&self, id: &str) -> Result<Option<ImportList>> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            let row: Option<ListRow> = import_lists::table
                .find(key)
                .select(ListRow::as_select())
                .first(conn)
                .optional()
                .map_err(db_err)?;
            row.map(ImportList::try_from).transpose()
        })
        .await
    }

    async fn upsert_import_list(&self, list: &ImportList) -> Result<()> {
        let row = list_to_row(list)?;
        self.with_conn(move |conn| {
            // Update-then-insert rather than an upsert clause: the ON CONFLICT
            // syntax is not a valid fragment across both backends.
            let updated = diesel::update(import_lists::table.find(&row.id))
                .set(&row)
                .execute(conn)
                .map_err(db_err)?;
            if updated == 0 {
                diesel::insert_into(import_lists::table)
                    .values(&row)
                    .execute(conn)
                    .map_err(db_err)?;
            }
            Ok(())
        })
        .await
    }

    async fn delete_import_list(&self, id: &str) -> Result<()> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::delete(import_lists::table.find(key))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn record_sync(&self, id: &str, at: DateTime<Utc>, error: Option<&str>) -> Result<()> {
        let (key, err) = (id.to_string(), error.map(ToString::to_string));
        self.with_conn(move |conn| {
            // Two columns, not the row: a sync finishing must not overwrite an
            // edit the operator made while it was running.
            diesel::update(import_lists::table.find(key))
                .set((
                    import_lists::last_synced_at.eq(Some(diesel_dualdb::types::Timestamp(at))),
                    import_lists::last_error.eq(err),
                ))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn list_exclusions(
        &self,
        target_domain: Option<&str>,
    ) -> Result<Vec<ImportListExclusion>> {
        let domain = target_domain.map(ToString::to_string);
        self.with_conn(move |conn| {
            let mut q = import_list_exclusions::table
                .order(import_list_exclusions::created_at.desc())
                .select(ExclusionRow::as_select())
                .into_boxed();
            if let Some(d) = domain {
                q = q.filter(import_list_exclusions::target_domain.eq(d));
            }
            let rows: Vec<ExclusionRow> = q.load(conn).map_err(db_err)?;
            Ok(rows.into_iter().map(ImportListExclusion::from).collect())
        })
        .await
    }

    async fn add_exclusion(&self, e: &ImportListExclusion) -> Result<()> {
        let row = ExclusionRow {
            id: e.id.clone(),
            target_domain: e.target_domain.clone(),
            id_kind: e.id_kind.clone(),
            external_id: e.external_id.clone(),
            title: e.title.clone(),
            created_at: diesel_dualdb::types::Timestamp(e.created_at),
        };
        self.with_conn(move |conn| {
            // Excluding something already excluded is a no-op, not an error: the
            // UI cannot know the state of every list's offering, and failing the
            // request would make "exclude this" unreliable for no gain.
            let existing: i64 = import_list_exclusions::table
                .filter(import_list_exclusions::target_domain.eq(&row.target_domain))
                .filter(import_list_exclusions::id_kind.eq(&row.id_kind))
                .filter(import_list_exclusions::external_id.eq(&row.external_id))
                .count()
                .get_result(conn)
                .map_err(db_err)?;
            if existing == 0 {
                diesel::insert_into(import_list_exclusions::table)
                    .values(&row)
                    .execute(conn)
                    .map_err(db_err)?;
            }
            Ok(())
        })
        .await
    }

    async fn delete_exclusion(&self, id: &str) -> Result<()> {
        let key = id.to_string();
        self.with_conn(move |conn| {
            diesel::delete(import_list_exclusions::table.find(key))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn is_excluded(
        &self,
        target_domain: &str,
        id_kind: &str,
        external_id: &str,
    ) -> Result<bool> {
        let (d, k, e) = (
            target_domain.to_string(),
            id_kind.to_string(),
            external_id.to_string(),
        );
        self.with_conn(move |conn| {
            let n: i64 = import_list_exclusions::table
                .filter(import_list_exclusions::target_domain.eq(d))
                .filter(import_list_exclusions::id_kind.eq(k))
                .filter(import_list_exclusions::external_id.eq(e))
                .count()
                .get_result(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }
}
