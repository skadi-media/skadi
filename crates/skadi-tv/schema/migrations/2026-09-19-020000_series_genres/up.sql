-- TMDB genres on the series (SKADI-T-0605): a JSON array of names. Same shape
-- and reasoning as movies.genres; NULL until a metadata refresh populates it.
ALTER TABLE series ADD COLUMN genres TEXT;
