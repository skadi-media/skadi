-- What a transfer was grabbed for, and how its import went (SKADI-T-0689).
--
-- `acquirable_ref` on a download row is the release title (the downloader only
-- sees the release), so the row could not say which item it was for, nor
-- whether the importer took it. The hunter now writes the target when it hands
-- the release over (`target_kind` = the domain's kind string, `target_ref` = the
-- acquirable ref), and the import outcome when the import step ends
-- (`import_state` = `imported` | `failed`, `import_error` = why it failed). The
-- Downloads page offers a manual import from these. NULL on rows from before.
ALTER TABLE downloads ADD COLUMN target_kind TEXT;
ALTER TABLE downloads ADD COLUMN target_ref TEXT;
ALTER TABLE downloads ADD COLUMN import_state TEXT;
ALTER TABLE downloads ADD COLUMN import_error TEXT;
