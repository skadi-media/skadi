-- Probed media-info (SKADI-T-0236): real resolution/codec/duration/audio of the imported
-- file, as JSON (skadi_core::MediaInfo). NULL until the post-import probe step runs.
ALTER TABLE movie_editions ADD COLUMN media_info TEXT;
