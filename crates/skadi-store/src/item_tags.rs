//! Tag membership on library items (SKADI-T-0550).
//!
//! SKADI-T-0464 built the tag *registry* — `tags` is a settings kind with
//! normalised, unique labels — but nothing could be tagged, so the feature was
//! inert. This is the join between the two.
//!
//! Cross-domain by design, following [`crate::history`] and
//! [`crate::blocklist`]: rows carry an opaque `(item_kind, item_id)` rather than
//! a foreign key, because the referent lives in another crate's schema. Tags are
//! one namespace across every enabled domain — the vision's departure from
//! *arr's per-app copies — so membership belongs beside the registry rather than
//! duplicated into three per-domain schemas.

use async_trait::async_trait;
use diesel::prelude::*;

use skadi_core::Result;

use crate::schema::item_tags;
use crate::{Store, db_err};

/// Tag membership reads and writes.
#[async_trait]
pub trait ItemTagRepo: Send + Sync {
    /// The tag ids on one item.
    async fn tags_for(&self, item_kind: &str, item_id: &str) -> Result<Vec<String>>;

    /// Replace an item's tags with exactly `tag_ids`.
    ///
    /// A replace rather than add/remove primitives: the caller is a PATCH
    /// carrying the full set, and expressing "these are the tags now" as a diff
    /// the caller computes is how two concurrent edits silently lose one.
    async fn set_tags(&self, item_kind: &str, item_id: &str, tag_ids: &[String]) -> Result<()>;

    /// Item ids of `item_kind` carrying **any** of `tag_ids`.
    ///
    /// Any rather than all, matching Sonarr: a filter for `anime, 4k` means
    /// "show me anything in either bucket", which is what an operator picking
    /// two tags from a list expects. An empty `tag_ids` returns nothing rather
    /// than everything — "filter by no tags" is a caller bug, and silently
    /// returning the whole library would hide it.
    async fn items_with_any_tag(&self, item_kind: &str, tag_ids: &[String]) -> Result<Vec<String>>;

    /// Add `tag_id` to every item in `item_ids` (SKADI-T-0560).
    ///
    /// A dedicated method rather than a read-modify-write loop over `set_tags`:
    /// doing it in the handler makes a bulk edit N round-trips, and a partial
    /// failure halfway through leaves some of the selection tagged and the rest
    /// not, with nothing recording which. One transaction is all-or-nothing.
    async fn add_tag_to(&self, item_kind: &str, item_ids: &[String], tag_id: &str)
    -> Result<usize>;

    /// Remove `tag_id` from every item in `item_ids`.
    async fn remove_tag_from(
        &self,
        item_kind: &str,
        item_ids: &[String],
        tag_id: &str,
    ) -> Result<usize>;

    /// Every membership row for one tag, used when a tag is deleted.
    async fn clear_tag(&self, tag_id: &str) -> Result<usize>;

    /// Drop every tag from one item, used when the item is deleted.
    ///
    /// Without this, deleting an item leaves its rows behind and a later item
    /// that happened to reuse the id would inherit them. *arr silently orphans
    /// these; the vision calls that out as something skadi does not do.
    async fn clear_item(&self, item_kind: &str, item_id: &str) -> Result<usize>;
}

#[derive(Insertable)]
#[diesel(table_name = item_tags)]
struct NewRow {
    item_kind: String,
    item_id: String,
    tag_id: String,
}

#[async_trait]
impl ItemTagRepo for Store {
    async fn tags_for(&self, item_kind: &str, item_id: &str) -> Result<Vec<String>> {
        let (kind, id) = (item_kind.to_string(), item_id.to_string());
        self.with_conn(move |conn| {
            item_tags::table
                .filter(item_tags::item_kind.eq(&kind))
                .filter(item_tags::item_id.eq(&id))
                .select(item_tags::tag_id)
                .load::<String>(conn)
                .map_err(db_err)
        })
        .await
    }

    async fn set_tags(&self, item_kind: &str, item_id: &str, tag_ids: &[String]) -> Result<()> {
        let (kind, id) = (item_kind.to_string(), item_id.to_string());
        // Dedupe the input. The composite key makes a *repeat call* idempotent,
        // but a single call carrying the same id twice would violate it — and a
        // caller sending ["a", "a"] means "tag a", not an error worth failing a
        // PATCH over. Found by the test for exactly that.
        let mut seen = std::collections::HashSet::new();
        let rows: Vec<NewRow> = tag_ids
            .iter()
            .filter(|t| seen.insert((*t).clone()))
            .map(|t| NewRow {
                item_kind: kind.clone(),
                item_id: id.clone(),
                tag_id: t.clone(),
            })
            .collect();
        self.with_conn(move |conn| {
            // Delete-then-insert in one transaction. Without the transaction a
            // concurrent read lands in the gap and sees the item untagged, which
            // for provider scoping means a search silently skipping every
            // indexer for the duration of an unrelated edit.
            conn.transaction(|conn| {
                diesel::delete(
                    item_tags::table
                        .filter(item_tags::item_kind.eq(&kind))
                        .filter(item_tags::item_id.eq(&id)),
                )
                .execute(conn)?;
                // One row at a time: diesel's batch insert is not a valid
                // fragment across both backends (SQLite and Postgres disagree on
                // the DEFAULT keyword in multi-row VALUES), and a tag set is a
                // handful of rows inside a transaction either way.
                for row in &rows {
                    diesel::insert_into(item_tags::table)
                        .values(row)
                        .execute(conn)?;
                }
                diesel::result::QueryResult::Ok(())
            })
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn items_with_any_tag(&self, item_kind: &str, tag_ids: &[String]) -> Result<Vec<String>> {
        if tag_ids.is_empty() {
            return Ok(Vec::new());
        }
        let kind = item_kind.to_string();
        let tags = tag_ids.to_vec();
        self.with_conn(move |conn| {
            item_tags::table
                .filter(item_tags::item_kind.eq(&kind))
                .filter(item_tags::tag_id.eq_any(&tags))
                .select(item_tags::item_id)
                .distinct()
                .load::<String>(conn)
                .map_err(db_err)
        })
        .await
    }

    async fn add_tag_to(
        &self,
        item_kind: &str,
        item_ids: &[String],
        tag_id: &str,
    ) -> Result<usize> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let kind = item_kind.to_string();
        let tag = tag_id.to_string();
        // Dedupe: the same id twice in one call would violate the composite key,
        // the way it did for `set_tags`.
        let mut seen = std::collections::HashSet::new();
        let rows: Vec<NewRow> = item_ids
            .iter()
            .filter(|id| seen.insert((*id).clone()))
            .map(|id| NewRow {
                item_kind: kind.clone(),
                item_id: id.clone(),
                tag_id: tag.clone(),
            })
            .collect();
        self.with_conn(move |conn| {
            let n = conn
                .transaction(|conn| {
                    let mut added = 0;
                    for row in &rows {
                        // Already-tagged items are skipped rather than failing the
                        // batch: "add this tag to these ten" must not error because
                        // three of them already had it.
                        // Checked with a SELECT rather than
                        // `on_conflict_do_nothing`, which is not a valid fragment
                        // across both backends — SQLite and Postgres disagree on
                        // the upsert clause. Inside the transaction, so the check
                        // and the insert cannot race.
                        let exists: i64 = item_tags::table
                            .filter(item_tags::item_kind.eq(&row.item_kind))
                            .filter(item_tags::item_id.eq(&row.item_id))
                            .filter(item_tags::tag_id.eq(&row.tag_id))
                            .count()
                            .get_result(conn)?;
                        if exists == 0 {
                            added += diesel::insert_into(item_tags::table)
                                .values(row)
                                .execute(conn)?;
                        }
                    }
                    diesel::result::QueryResult::Ok(added)
                })
                .map_err(db_err)?;
            Ok(n)
        })
        .await
    }

    async fn remove_tag_from(
        &self,
        item_kind: &str,
        item_ids: &[String],
        tag_id: &str,
    ) -> Result<usize> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let kind = item_kind.to_string();
        let tag = tag_id.to_string();
        let ids = item_ids.to_vec();
        self.with_conn(move |conn| {
            diesel::delete(
                item_tags::table
                    .filter(item_tags::item_kind.eq(&kind))
                    .filter(item_tags::tag_id.eq(&tag))
                    .filter(item_tags::item_id.eq_any(&ids)),
            )
            .execute(conn)
            .map_err(db_err)
        })
        .await
    }

    async fn clear_tag(&self, tag_id: &str) -> Result<usize> {
        let tag = tag_id.to_string();
        self.with_conn(move |conn| {
            diesel::delete(item_tags::table.filter(item_tags::tag_id.eq(&tag)))
                .execute(conn)
                .map_err(db_err)
        })
        .await
    }

    async fn clear_item(&self, item_kind: &str, item_id: &str) -> Result<usize> {
        let (kind, id) = (item_kind.to_string(), item_id.to_string());
        self.with_conn(move |conn| {
            diesel::delete(
                item_tags::table
                    .filter(item_tags::item_kind.eq(&kind))
                    .filter(item_tags::item_id.eq(&id)),
            )
            .execute(conn)
            .map_err(db_err)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Store {
        let path = skadi_core::unique_temp_path("item-tags").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    #[tokio::test]
    async fn set_tags_replaces_rather_than_accumulates() {
        let s = store().await;
        s.set_tags("movie", "m1", &["a".into(), "b".into()])
            .await
            .unwrap();
        let mut got = s.tags_for("movie", "m1").await.unwrap();
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);

        // The PATCH carries the full set, so a second write is the new truth —
        // not a union with the old one.
        s.set_tags("movie", "m1", &["c".into()]).await.unwrap();
        assert_eq!(s.tags_for("movie", "m1").await.unwrap(), vec!["c"]);

        // Clearing to empty is a legitimate edit, not a no-op.
        s.set_tags("movie", "m1", &[]).await.unwrap();
        assert!(s.tags_for("movie", "m1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tagging_twice_is_idempotent() {
        let s = store().await;
        // The composite primary key means the row *is* the association, so a
        // repeat cannot create a duplicate to clean up later.
        s.set_tags("movie", "m1", &["a".into(), "a".into()])
            .await
            .unwrap();
        assert_eq!(s.tags_for("movie", "m1").await.unwrap(), vec!["a"]);
    }

    #[tokio::test]
    async fn kinds_are_separate_namespaces() {
        let s = store().await;
        // A movie and a series may share an id — they come from different
        // domains — so the kind has to be part of the key, not decoration.
        s.set_tags("movie", "x", &["a".into()]).await.unwrap();
        s.set_tags("series", "x", &["b".into()]).await.unwrap();
        assert_eq!(s.tags_for("movie", "x").await.unwrap(), vec!["a"]);
        assert_eq!(s.tags_for("series", "x").await.unwrap(), vec!["b"]);
    }

    #[tokio::test]
    async fn filtering_matches_any_tag_and_refuses_an_empty_filter() {
        let s = store().await;
        s.set_tags("movie", "m1", &["anime".into()]).await.unwrap();
        s.set_tags("movie", "m2", &["uhd".into()]).await.unwrap();
        s.set_tags("movie", "m3", &["other".into()]).await.unwrap();
        s.set_tags("series", "s1", &["anime".into()]).await.unwrap();

        let mut got = s
            .items_with_any_tag("movie", &["anime".into(), "uhd".into()])
            .await
            .unwrap();
        got.sort();
        assert_eq!(got, vec!["m1".to_string(), "m2".to_string()]);

        // "Filter by no tags" is a caller bug. Returning the whole library would
        // hide it — and for provider scoping would mean an untagged provider
        // silently matching everything through the wrong code path.
        assert!(
            s.items_with_any_tag("movie", &[]).await.unwrap().is_empty(),
            "an empty filter must not mean 'everything'"
        );
    }

    #[tokio::test]
    async fn bulk_add_is_idempotent_and_reports_what_it_changed() {
        let s = store().await;
        s.set_tags("movie", "m1", &["anime".into()]).await.unwrap();

        // m1 already has it, m2 and m3 do not.
        let ids = vec!["m1".to_string(), "m2".to_string(), "m3".to_string()];
        let added = s.add_tag_to("movie", &ids, "anime").await.unwrap();
        assert_eq!(added, 2, "only the two that lacked it count as changed");

        // Re-running changes nothing and does not error. "Add this tag to these
        // ten" must not fail because three already had it.
        assert_eq!(s.add_tag_to("movie", &ids, "anime").await.unwrap(), 0);
        for id in &ids {
            assert_eq!(s.tags_for("movie", id).await.unwrap(), vec!["anime"]);
        }
    }

    #[tokio::test]
    async fn bulk_add_dedupes_its_input() {
        // The same id twice in one call would violate the composite key, the way
        // it did for `set_tags` before it deduped.
        let s = store().await;
        let ids = vec!["m1".to_string(), "m1".to_string()];
        assert_eq!(s.add_tag_to("movie", &ids, "anime").await.unwrap(), 1);
        assert_eq!(s.tags_for("movie", "m1").await.unwrap(), vec!["anime"]);
    }

    #[tokio::test]
    async fn bulk_remove_touches_only_the_named_tag_and_items() {
        let s = store().await;
        s.set_tags("movie", "m1", &["anime".into(), "uhd".into()])
            .await
            .unwrap();
        s.set_tags("movie", "m2", &["anime".into()]).await.unwrap();
        s.set_tags("movie", "m3", &["anime".into()]).await.unwrap();

        let removed = s
            .remove_tag_from("movie", &["m1".into(), "m2".into()], "anime")
            .await
            .unwrap();
        assert_eq!(removed, 2);
        // m1 keeps its other tag — removing one tag is not clearing the item.
        assert_eq!(s.tags_for("movie", "m1").await.unwrap(), vec!["uhd"]);
        assert!(s.tags_for("movie", "m2").await.unwrap().is_empty());
        // m3 was not in the selection.
        assert_eq!(s.tags_for("movie", "m3").await.unwrap(), vec!["anime"]);
    }

    #[tokio::test]
    async fn an_empty_selection_is_a_no_op_not_an_error() {
        // A UI can send an empty selection when nothing is ticked; that is a
        // no-op, not a failure the operator has to understand.
        let s = store().await;
        assert_eq!(s.add_tag_to("movie", &[], "anime").await.unwrap(), 0);
        assert_eq!(s.remove_tag_from("movie", &[], "anime").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn deleting_a_tag_or_an_item_leaves_nothing_orphaned() {
        let s = store().await;
        s.set_tags("movie", "m1", &["a".into(), "b".into()])
            .await
            .unwrap();
        s.set_tags("movie", "m2", &["a".into()]).await.unwrap();

        assert_eq!(s.clear_tag("a").await.unwrap(), 2);
        assert_eq!(s.tags_for("movie", "m1").await.unwrap(), vec!["b"]);
        assert!(s.tags_for("movie", "m2").await.unwrap().is_empty());

        // Deleting the item takes its rows with it. Otherwise a later item that
        // reused the id would inherit them — *arr silently orphans these, and
        // the vision calls that out as something skadi does not do.
        assert_eq!(s.clear_item("movie", "m1").await.unwrap(), 1);
        assert!(s.tags_for("movie", "m1").await.unwrap().is_empty());
    }
}
