-- skadi-audiobooks: known-works catalog (SKADI-I-0018 / SKADI-T-0154). The record
-- of an author's / series' works from the Audible catalog — known but not
-- necessarily owned. Keyed by Audible ASIN so it dedups and links to a library
-- `books` row by asin. Series membership (name/asin/position) lets us derive true
-- completeness ("have N of M") and browse a body of work.

CREATE TABLE works (
    asin                TEXT PRIMARY KEY NOT NULL,
    title               TEXT NOT NULL,
    authors_json        JSON NOT NULL,
    author_asin         TEXT,
    series_name         TEXT,
    series_asin         TEXT,
    series_position     TEXT,
    cover_url           TEXT,
    release_date        TEXT,
    first_seen          TIMESTAMP NOT NULL,
    updated_at          TIMESTAMP NOT NULL
);

CREATE INDEX works_author_idx      ON works(author_asin);
CREATE INDEX works_series_idx      ON works(series_asin);
CREATE INDEX works_series_name_idx ON works(series_name);
