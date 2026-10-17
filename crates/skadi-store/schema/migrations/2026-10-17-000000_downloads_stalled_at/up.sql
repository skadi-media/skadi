-- When the transfer went stalled (SKADI-T-0708): set by the worker on the tick
-- that flips the row to `stalled`, cleared when it leaves that state. The
-- Downloads page's Stalled badge says how long from it; `updated_at` cannot,
-- since every progress tick moves it. NULL while the row is not stalled.
ALTER TABLE downloads ADD COLUMN stalled_at TIMESTAMP;
