CREATE TABLE works (asin TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, authors_json JSONB NOT NULL, author_asin TEXT, series_name TEXT, series_asin TEXT, series_position TEXT, cover_url TEXT, release_date TEXT, first_seen TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL);

CREATE INDEX works_author_idx ON works(author_asin);

CREATE INDEX works_series_idx ON works(series_asin);

CREATE INDEX works_series_name_idx ON works(series_name);
