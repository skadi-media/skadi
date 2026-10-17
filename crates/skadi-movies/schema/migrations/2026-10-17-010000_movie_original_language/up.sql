-- The title's original language, ISO 639-1 (SKADI-T-0673): filled by metadata
-- add and refresh. The grab decision and import read it to tell a title's
-- own audio from a dub. NULL until a refresh reports one.
ALTER TABLE movies ADD COLUMN original_language TEXT;
