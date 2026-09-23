-- Live torrent metrics for the downloads queue (SKADI-T-0166). The worker writes
-- these each poll tick from librqbit's live stats; the API surfaces them on the
-- Downloaders page. All nullable (NULL = unknown / not-yet-reported), so the
-- daemon's enqueue path and existing rows need no change.

ALTER TABLE downloads ADD COLUMN down_speed_bps BIGINT;
ALTER TABLE downloads ADD COLUMN up_speed_bps   BIGINT;
ALTER TABLE downloads ADD COLUMN uploaded_bytes BIGINT;
ALTER TABLE downloads ADD COLUMN peers          INTEGER;
ALTER TABLE downloads ADD COLUMN peers_seen     INTEGER;
ALTER TABLE downloads ADD COLUMN eta_seconds    BIGINT;
