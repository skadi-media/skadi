-- Structured failure reason code (SKADI-T-0200): the machine-filterable companion
-- to the free-text `detail`, so History can group/filter failures (e.g. all
-- import_failed) without parsing prose. NULL for non-failure events.
ALTER TABLE acquisition_history ADD COLUMN reason_code TEXT;
