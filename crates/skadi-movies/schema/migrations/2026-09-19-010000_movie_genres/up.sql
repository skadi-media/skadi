-- TMDB genres on the movie (SKADI-T-0605): a JSON array of names, e.g.
-- ["Science Fiction","Drama"], so the library can be browsed by genre.
--
-- One text column rather than a genres table + join: a film has a handful of
-- short labels that are only ever read together, and the facet
-- (`GET /library/genres`) is a count over a library that fits in memory.
--
-- NULL until a metadata refresh populates it; readers treat NULL as empty.
ALTER TABLE movies ADD COLUMN genres TEXT;
