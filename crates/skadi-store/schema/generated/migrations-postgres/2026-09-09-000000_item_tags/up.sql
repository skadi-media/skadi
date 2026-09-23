CREATE TABLE item_tags (item_kind TEXT NOT NULL, item_id TEXT NOT NULL, tag_id TEXT NOT NULL, PRIMARY KEY (item_kind, item_id, tag_id));

CREATE INDEX item_tags_item_idx ON item_tags(item_kind,item_id);

CREATE INDEX item_tags_tag_idx ON item_tags(tag_id);
