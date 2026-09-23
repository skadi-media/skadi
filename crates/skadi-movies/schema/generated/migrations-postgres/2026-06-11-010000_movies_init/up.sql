CREATE TABLE edition_kinds (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL UNIQUE, normalized_tag TEXT NOT NULL, match_patterns JSONB NOT NULL, builtin BOOLEAN NOT NULL);

CREATE TABLE movies (id TEXT PRIMARY KEY NOT NULL, tmdb_id BIGINT NOT NULL UNIQUE, imdb_id TEXT UNIQUE, title TEXT NOT NULL, original_title TEXT, year INTEGER, overview TEXT, runtime_minutes INTEGER, monitored BOOLEAN NOT NULL, profile_id TEXT NOT NULL, root_folder_id TEXT NOT NULL, root_folder_path TEXT NOT NULL, added_at TIMESTAMPTZ NOT NULL, last_metadata_refresh TIMESTAMPTZ, poster_url TEXT, backdrop_url TEXT);

CREATE INDEX movies_monitored_idx ON movies(monitored);

CREATE TABLE movie_editions (id TEXT PRIMARY KEY NOT NULL, movie_id TEXT NOT NULL REFERENCES movies (id) ON DELETE CASCADE, kind_id TEXT NOT NULL REFERENCES edition_kinds (id), status_json JSONB NOT NULL, status_kind TEXT NOT NULL, file_path TEXT, quality_id TEXT, format_score INTEGER NOT NULL, updated_at TIMESTAMPTZ NOT NULL, UNIQUE (movie_id, kind_id));

CREATE INDEX movie_editions_movie_idx ON movie_editions(movie_id);

CREATE INDEX movie_editions_status_idx ON movie_editions(status_kind);

INSERT INTO edition_kinds (id, name, normalized_tag, match_patterns, builtin) VALUES
  ('00000000-0000-0000-0000-000000000001', 'Theatrical',     'Theatrical',     '["theatrical"]'::jsonb,                                  TRUE),
  ('00000000-0000-0000-0000-000000000002', 'Extended',       'Extended',       '["extended","extended cut","extended edition"]'::jsonb,  TRUE),
  ('00000000-0000-0000-0000-000000000003', 'Director''s Cut','Directors Cut',  '["director''s cut","directors cut","director.cut"]'::jsonb, TRUE),
  ('00000000-0000-0000-0000-000000000004', 'Ultimate Cut',   'Ultimate Cut',   '["ultimate cut","ultimate edition","ultimate.cut"]'::jsonb,  TRUE),
  ('00000000-0000-0000-0000-000000000005', 'IMAX',           'IMAX',           '["imax"]'::jsonb,                                        TRUE),
  ('00000000-0000-0000-0000-000000000006', 'Remastered',     'Remastered',     '["remastered","remaster"]'::jsonb,                       TRUE)
ON CONFLICT (id) DO NOTHING;
