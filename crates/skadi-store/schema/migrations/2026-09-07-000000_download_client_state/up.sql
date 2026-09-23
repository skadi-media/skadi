-- Client-reported transfer state (SKADI-T-0394): what the download client says
-- the transfer is doing right now — `initializing` (queued behind the client's
-- hash/init work, or fetching magnet metadata), `live`, `paused`, `error`.
--
-- The row's own `status` says what *skadi* thinks (queued/downloading/...); it
-- cannot distinguish "the client has it live but no peers" from "the client has
-- not started it yet", and the hunter's stall watch failed 46 releases on
-- 2026-09-06 for sitting at 0% while they were merely queued behind a re-hash.
-- NULL means the client did not report a state (older rows, non-skadi clients).
ALTER TABLE downloads ADD COLUMN client_state TEXT;
