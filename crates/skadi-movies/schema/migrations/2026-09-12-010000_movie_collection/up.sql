-- TMDB collection membership (SKADI-T-0581): the franchise a film belongs to,
-- e.g. "The Taken Collection" for Taken / Taken 2 / Taken 3.
--
-- Stored on the movie rather than in a `collections` table on purpose. TMDB's
-- collection is a flat label with an id and a name; there is no collection-level
-- state for skadi to own (no monitoring, no profile, no root folder), so a table
-- would add a join and a lifecycle to carry two columns.
--
-- NULL until a metadata refresh populates it, which is why both are nullable and
-- why nothing reads them as required: the existing library was imported before
-- this column existed.
ALTER TABLE movies ADD COLUMN collection_tmdb_id BIGINT;
ALTER TABLE movies ADD COLUMN collection_name TEXT;
