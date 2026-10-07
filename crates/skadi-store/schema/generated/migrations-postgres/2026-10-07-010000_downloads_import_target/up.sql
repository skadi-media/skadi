ALTER TABLE downloads ADD COLUMN target_kind TEXT;

ALTER TABLE downloads ADD COLUMN target_ref TEXT;

ALTER TABLE downloads ADD COLUMN import_state TEXT;

ALTER TABLE downloads ADD COLUMN import_error TEXT;
