//! Blocklist of failed/bad releases (SKADI-T-0115).
//!
//! When a download or import fails, the acquire pipeline records the offending
//! release here (a manual `POST /blocklist` adds one too). `search`/`decide`
//! then exclude blocklisted releases so the sweep stops re-grabbing the same
//! dead torrent (the live e2e showed `decide` re-picking a 0-seeder release
//! every cycle — the min-seeders filter helps, the blocklist closes the loop).
//!
//! Identity is [`release_key`](skadi_indexers::release_key): the magnet
//! info-hash or fetch URL, stable across indexers. [`block`](BlocklistRepo::block)
//! is idempotent per `release_key` (a re-block refreshes the existing row rather
//! than duplicating), so [`blocked_keys`](BlocklistRepo::blocked_keys) is a clean
//! set and [`unblock`](BlocklistRepo::unblock) by id is unambiguous. A block may
//! carry an optional `expires_at` (SKADI-T-0198): once lapsed it stops vetoing
//! ([`blocked_keys`]/[`is_blocked`] exclude it) and is removed by
//! [`purge_expired_blocklist`](BlocklistRepo::purge_expired_blocklist); `None` =
//! permanent (the default for an auto-block).

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_dualdb::types::Timestamp;

use skadi_core::Result;

use crate::schema::blocklist;
use crate::{Store, db_err};

/// A request to block a release. The repo generates the id + `at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewBlocklistEntry {
    /// Canonical cross-indexer identity (`skadi_indexers::release_key`).
    pub release_key: String,
    /// Human-readable release title (for the UI).
    pub title: String,
    /// The acquirable this block came from, if any (scopes a per-item view).
    pub acquirable_ref: Option<String>,
    /// Originating indexer name, if known.
    pub indexer: Option<String>,
    /// Why it was blocked (failure reason or "manual").
    pub reason: Option<String>,
    /// Optional expiry (SKADI-T-0198): after this instant the block stops vetoing
    /// and is eligible for purge. `None` = permanent.
    pub expires_at: Option<DateTime<Utc>>,
}

/// One blocklist row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlocklistEntry {
    pub id: String,
    pub release_key: String,
    pub title: String,
    pub acquirable_ref: Option<String>,
    pub indexer: Option<String>,
    pub reason: Option<String>,
    pub at: DateTime<Utc>,
    /// Optional expiry; `None` = permanent (SKADI-T-0198).
    pub expires_at: Option<DateTime<Utc>>,
}

/// Record, query, and clear blocklisted releases.
#[async_trait]
pub trait BlocklistRepo: Send + Sync {
    /// Block a release (idempotent per `release_key`: a re-block replaces the
    /// prior row). Returns the stored entry.
    async fn block(&self, req: &NewBlocklistEntry) -> Result<BlocklistEntry>;
    /// Whether a `release_key` is currently blocked.
    async fn is_blocked(&self, release_key: &str) -> Result<bool>;
    /// All blocked `release_key`s, as a set for fast `decide`/`search` filtering.
    async fn blocked_keys(&self) -> Result<HashSet<String>>;
    /// All blocklist entries, newest first.
    async fn list_blocklist(&self) -> Result<Vec<BlocklistEntry>>;

    /// One page of the blocklist, bounded **in the query** (SKADI-T-0494).
    ///
    /// The handler used to load every row and `.skip().take()` it, so paging
    /// shrank the response and left the scan alone. The blocklist only grows.
    async fn list_blocklist_page(
        &self,
        limit: Option<i64>,
        offset: i64,
    ) -> Result<Vec<BlocklistEntry>>;

    /// Total blocklist rows, ignoring paging, so a client can size page controls
    /// without paying for a full load.
    async fn count_blocklist(&self) -> Result<i64>;
    /// Blocklist entries for one acquirable, newest first.
    async fn list_blocklist_for(&self, acquirable_ref: &str) -> Result<Vec<BlocklistEntry>>;
    /// Remove one entry by id. Returns `true` if a row was deleted.
    async fn unblock(&self, id: &str) -> Result<bool>;
    /// Remove many entries by id; returns the number actually removed
    /// (SKADI-T-0195). Unknown ids are ignored.
    async fn unblock_many(&self, ids: &[String]) -> Result<usize>;
    /// Remove **every** entry (Clear-all); returns the number removed.
    async fn clear_blocklist(&self) -> Result<usize>;
    /// Delete all entries whose `expires_at` is in the past (SKADI-T-0198);
    /// returns the number purged. Permanent (`NULL`) entries are never purged.
    async fn purge_expired_blocklist(&self) -> Result<usize>;
}

/// The stored row.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = blocklist)]
struct Row {
    id: String,
    release_key: String,
    title: String,
    acquirable_ref: Option<String>,
    indexer: Option<String>,
    reason: Option<String>,
    at: Timestamp,
    expires_at: Option<Timestamp>,
}

impl From<Row> for BlocklistEntry {
    fn from(r: Row) -> Self {
        BlocklistEntry {
            id: r.id,
            release_key: r.release_key,
            title: r.title,
            acquirable_ref: r.acquirable_ref,
            indexer: r.indexer,
            reason: r.reason,
            at: r.at.0,
            expires_at: r.expires_at.map(|t| t.0),
        }
    }
}

#[async_trait]
impl BlocklistRepo for Store {
    async fn block(&self, req: &NewBlocklistEntry) -> Result<BlocklistEntry> {
        let row = Row {
            id: uuid::Uuid::new_v4().to_string(),
            release_key: req.release_key.clone(),
            title: req.title.clone(),
            acquirable_ref: req.acquirable_ref.clone(),
            indexer: req.indexer.clone(),
            reason: req.reason.clone(),
            at: Timestamp(Utc::now()),
            expires_at: req.expires_at.map(Timestamp),
        };
        let id = row.id.clone();
        let key = row.release_key.clone();
        self.with_conn(move |conn| {
            // Idempotent per release_key: drop any prior block for the same
            // release, then insert the fresh one (portable, no upsert needed).
            diesel::delete(blocklist::table.filter(blocklist::release_key.eq(&key)))
                .execute(conn)
                .map_err(db_err)?;
            diesel::insert_into(blocklist::table)
                .values(&row)
                .execute(conn)
                .map_err(db_err)?;
            let stored: Row = blocklist::table
                .find(&id)
                .select(Row::as_select())
                .first(conn)
                .map_err(db_err)?;
            Ok(stored.into())
        })
        .await
    }

    async fn is_blocked(&self, release_key: &str) -> Result<bool> {
        let key = release_key.to_string();
        self.with_conn(move |conn| {
            // An expired block doesn't veto (SKADI-T-0198): permanent (NULL) or
            // not-yet-expired only.
            let now = Timestamp(Utc::now());
            let n: i64 = blocklist::table
                .filter(blocklist::release_key.eq(&key))
                .filter(
                    blocklist::expires_at
                        .is_null()
                        .or(blocklist::expires_at.gt(now)),
                )
                .count()
                .get_result(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }

    async fn blocked_keys(&self) -> Result<HashSet<String>> {
        self.with_conn(|conn| {
            // Exclude expired entries so a lapsed block stops vetoing decide/search
            // even before the purge sweep removes it (SKADI-T-0198).
            let now = Timestamp(Utc::now());
            let keys: Vec<String> = blocklist::table
                .filter(
                    blocklist::expires_at
                        .is_null()
                        .or(blocklist::expires_at.gt(now)),
                )
                .select(blocklist::release_key)
                .load(conn)
                .map_err(db_err)?;
            Ok(keys.into_iter().collect())
        })
        .await
    }

    async fn list_blocklist(&self) -> Result<Vec<BlocklistEntry>> {
        self.with_conn(|conn| {
            let rows: Vec<Row> = blocklist::table
                .order(blocklist::at.desc())
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(BlocklistEntry::from).collect())
        })
        .await
    }

    async fn list_blocklist_page(
        &self,
        limit: Option<i64>,
        offset: i64,
    ) -> Result<Vec<BlocklistEntry>> {
        self.with_conn(move |conn| {
            // Same order as `list_blocklist`: a page has to come off the ordering
            // the unpaged call would have produced, or pages overlap and skip.
            let mut q = blocklist::table
                .order(blocklist::at.desc())
                .select(Row::as_select())
                .into_boxed();
            // SQLite rejects OFFSET without LIMIT, so an offset-only request has
            // to carry a nominal bound. Postgres accepts either; the explicit
            // limit keeps one query shape across both backends.
            if let Some(l) = limit {
                q = q.limit(l);
            } else if offset > 0 {
                q = q.limit(i64::MAX);
            }
            if offset > 0 {
                q = q.offset(offset);
            }
            let rows: Vec<Row> = q.load(conn).map_err(db_err)?;
            Ok(rows.into_iter().map(BlocklistEntry::from).collect())
        })
        .await
    }

    async fn count_blocklist(&self) -> Result<i64> {
        self.with_conn(|conn| blocklist::table.count().get_result(conn).map_err(db_err))
            .await
    }

    async fn list_blocklist_for(&self, acquirable_ref: &str) -> Result<Vec<BlocklistEntry>> {
        let acq = acquirable_ref.to_string();
        self.with_conn(move |conn| {
            let rows: Vec<Row> = blocklist::table
                .filter(blocklist::acquirable_ref.eq(&acq))
                .order(blocklist::at.desc())
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows.into_iter().map(BlocklistEntry::from).collect())
        })
        .await
    }

    async fn unblock(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        self.with_conn(move |conn| {
            let n = diesel::delete(blocklist::table.find(&id))
                .execute(conn)
                .map_err(db_err)?;
            Ok(n > 0)
        })
        .await
    }

    async fn unblock_many(&self, ids: &[String]) -> Result<usize> {
        let ids = ids.to_vec();
        self.with_conn(move |conn| {
            if ids.is_empty() {
                return Ok(0);
            }
            let n = diesel::delete(blocklist::table.filter(blocklist::id.eq_any(&ids)))
                .execute(conn)
                .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn clear_blocklist(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let n = diesel::delete(blocklist::table)
                .execute(conn)
                .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn purge_expired_blocklist(&self) -> Result<usize> {
        self.with_conn(|conn| {
            let now = Timestamp(Utc::now());
            let n = diesel::delete(
                blocklist::table
                    .filter(blocklist::expires_at.is_not_null())
                    .filter(blocklist::expires_at.le(now)),
            )
            .execute(conn)
            .map_err(db_err)?;
            Ok(n)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("blocklist").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn a_page_comes_off_the_same_ordering_as_the_full_list() {
        let store = temp_store().await;
        for i in 0..5 {
            store
                .block(&new_entry(&format!("k{i}"), &format!("t{i}")))
                .await
                .unwrap();
        }

        let all = store.list_blocklist().await.unwrap();
        assert_eq!(store.count_blocklist().await.unwrap(), 5);

        // A page must be the slice of the unpaged ordering it claims to be —
        // otherwise pages silently overlap and skip entries as a client walks
        // them, which is worse than not paging at all.
        let page = store.list_blocklist_page(Some(2), 1).await.unwrap();
        assert_eq!(
            page.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            all[1..3].iter().map(|e| e.id.clone()).collect::<Vec<_>>()
        );

        // No limit means "from the offset to the end", not "nothing".
        let rest = store.list_blocklist_page(None, 3).await.unwrap();
        assert_eq!(rest.len(), 2);
        // An offset past the end is empty, not an error.
        assert!(
            store
                .list_blocklist_page(Some(2), 99)
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn new_entry(key: &str, title: &str) -> NewBlocklistEntry {
        NewBlocklistEntry {
            release_key: key.into(),
            title: title.into(),
            acquirable_ref: Some("ed-1".into()),
            indexer: Some("torznab".into()),
            reason: Some("download failed".into()),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn block_then_query_round_trips() {
        let store = temp_store().await;
        let e = store
            .block(&new_entry("btih:abc", "The Matrix 1080p"))
            .await
            .unwrap();
        assert_eq!(e.release_key, "btih:abc");
        assert_eq!(e.acquirable_ref.as_deref(), Some("ed-1"));

        assert!(store.is_blocked("btih:abc").await.unwrap());
        assert!(!store.is_blocked("btih:nope").await.unwrap());

        let keys = store.blocked_keys().await.unwrap();
        assert!(keys.contains("btih:abc"));

        let listed = store.list_blocklist().await.unwrap();
        assert_eq!(listed.len(), 1);
        let per = store.list_blocklist_for("ed-1").await.unwrap();
        assert_eq!(per.len(), 1);
        assert!(store.list_blocklist_for("ed-2").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn block_is_idempotent_per_release_key() {
        let store = temp_store().await;
        store.block(&new_entry("btih:abc", "first")).await.unwrap();
        let second = store.block(&new_entry("btih:abc", "second")).await.unwrap();
        // Only one row for the key; the latest wins.
        let listed = store.list_blocklist().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[0].title, "second");
    }

    #[tokio::test]
    async fn expired_blocks_do_not_veto_and_purge_removes_them() {
        let store = temp_store().await;

        let mut expired = new_entry("btih:expired", "Expired");
        expired.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        store.block(&expired).await.unwrap();

        let mut active = new_entry("btih:active", "Active");
        active.expires_at = Some(Utc::now() + chrono::Duration::hours(1));
        store.block(&active).await.unwrap();

        // No expiry = permanent.
        store.block(&new_entry("btih:perm", "Perm")).await.unwrap();

        // An expired block no longer vetoes; the future and permanent ones do.
        assert!(!store.is_blocked("btih:expired").await.unwrap());
        assert!(store.is_blocked("btih:active").await.unwrap());
        assert!(store.is_blocked("btih:perm").await.unwrap());
        let keys = store.blocked_keys().await.unwrap();
        assert!(
            !keys.contains("btih:expired"),
            "expired excluded from veto set"
        );
        assert!(keys.contains("btih:active"));
        assert!(keys.contains("btih:perm"));

        // It's still listed (for the management view) until purged.
        assert_eq!(store.list_blocklist().await.unwrap().len(), 3);

        // Purge removes only the lapsed entry; permanent/future survive.
        assert_eq!(store.purge_expired_blocklist().await.unwrap(), 1);
        assert_eq!(store.list_blocklist().await.unwrap().len(), 2);
        // Nothing left to purge.
        assert_eq!(store.purge_expired_blocklist().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn unblock_many_and_clear_all() {
        let store = temp_store().await;
        let a = store.block(&new_entry("btih:a", "A")).await.unwrap();
        let b = store.block(&new_entry("btih:b", "B")).await.unwrap();
        store.block(&new_entry("btih:c", "C")).await.unwrap();

        // Bulk remove two by id; an unknown id is ignored.
        let removed = store
            .unblock_many(&[a.id.clone(), b.id.clone(), "nope".into()])
            .await
            .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.list_blocklist().await.unwrap().len(), 1);

        // Empty id list is a no-op.
        assert_eq!(store.unblock_many(&[]).await.unwrap(), 0);

        // Clear-all removes the remainder.
        assert_eq!(store.clear_blocklist().await.unwrap(), 1);
        assert!(store.list_blocklist().await.unwrap().is_empty());
        // Clearing an empty table removes nothing.
        assert_eq!(store.clear_blocklist().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn unblock_removes_by_id() {
        let store = temp_store().await;
        let e = store.block(&new_entry("btih:abc", "x")).await.unwrap();
        assert!(store.unblock(&e.id).await.unwrap());
        assert!(!store.is_blocked("btih:abc").await.unwrap());
        // Second unblock is a no-op (already gone).
        assert!(!store.unblock(&e.id).await.unwrap());
    }
}
