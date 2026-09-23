ALTER TABLE downloads ADD COLUMN down_speed_bps BIGINT;

ALTER TABLE downloads ADD COLUMN up_speed_bps BIGINT;

ALTER TABLE downloads ADD COLUMN uploaded_bytes BIGINT;

ALTER TABLE downloads ADD COLUMN peers INTEGER;

ALTER TABLE downloads ADD COLUMN peers_seen INTEGER;

ALTER TABLE downloads ADD COLUMN eta_seconds BIGINT;
