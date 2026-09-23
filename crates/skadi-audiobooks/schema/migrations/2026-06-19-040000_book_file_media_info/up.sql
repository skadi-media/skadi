-- Probed media-info (SKADI-T-0236): real duration/bitrate/codec of the imported audio
-- file, as JSON (skadi_core::MediaInfo). NULL until the post-import probe step runs.
ALTER TABLE book_files ADD COLUMN media_info TEXT;
