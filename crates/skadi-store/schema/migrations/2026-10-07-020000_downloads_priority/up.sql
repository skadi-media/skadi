-- Per-download priority (SKADI-T-0692).
--
-- The worker claims queued rows by `priority` descending, then by age (oldest
-- first), so the operator can push one transfer ahead of the queue. Every row
-- starts at 0, which keeps the old FIFO order until someone moves a row.
ALTER TABLE downloads ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
