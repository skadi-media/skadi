-- Probed media-info (SKADI-T-0451): real resolution/codec/duration/audio of the
-- imported file, as JSON (skadi_core::MediaInfo). NULL until the post-import probe
-- runs. Movies and audiobooks already persist this; TV dropped it on the floor.
ALTER TABLE episodes ADD COLUMN media_info TEXT;
