-- Tag membership on library items (SKADI-T-0550).
--
-- SKADI-T-0464 built the tag *registry* — `tags` is a settings kind with
-- normalised, unique labels — but nothing could be tagged with one, so the
-- feature was inert. This is the join.
--
-- **Polymorphic, in the shared store**, following `history` and `blocklist`
-- rather than adding a `tags` column to each domain's item table. Tags are one
-- namespace across every enabled domain (that is the point of them, and the
-- vision's departure from *arr's per-app copies), so the membership belongs
-- where the tag registry already lives — not duplicated three times in three
-- per-domain schemas that would each need their own migration.
CREATE TABLE item_tags (
    -- `movie` | `series` | `audiobook`, matching MediaKind's serialised form.
    item_kind  TEXT NOT NULL,
    -- The domain item's id. Not a foreign key: the referent lives in another
    -- crate's schema, exactly as `history.acquirable_ref` does.
    item_id    TEXT NOT NULL,
    -- The settings-record id of the tag (kind `tags`). Also not a foreign key —
    -- settings are one polymorphic table keyed by an opaque id.
    tag_id     TEXT NOT NULL,
    -- Composite key rather than a surrogate id: the row *is* the association,
    -- so tagging twice is a no-op rather than a duplicate to clean up later.
    PRIMARY KEY (item_kind, item_id, tag_id)
);

-- "Which tags does this item have" — the read on every item fetch.
CREATE INDEX item_tags_item_idx ON item_tags(item_kind, item_id);
-- "Which items carry this tag" — the read behind `?tags=` filtering and behind
-- provider scoping, which asks it once per search.
CREATE INDEX item_tags_tag_idx  ON item_tags(tag_id);
