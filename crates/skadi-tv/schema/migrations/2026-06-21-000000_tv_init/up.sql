-- skadi-tv logical schema (diesel-dualdb, SKADI-I-0037 / T-0265). One source of
-- truth; the CLI generates the unified schema.rs + per-backend migrations. All
-- ids are TEXT (UUIDs as strings); air_date is a TEXT ISO date (YYYY-MM-DD).

CREATE TABLE series (
    id                      TEXT PRIMARY KEY NOT NULL,
    -- TV is TheTVDB-native (Sonarr/Skyhook): tvdb_id is the required key; tmdb/imdb
    -- are carried along when the source provides them.
    tvdb_id                 BIGINT NOT NULL UNIQUE,
    tmdb_id                 BIGINT,
    imdb_id                 TEXT,
    title                   TEXT NOT NULL,
    year                    INTEGER,
    overview                TEXT,
    status                  TEXT,
    network                 TEXT,
    runtime_minutes         INTEGER,
    series_type             TEXT NOT NULL,
    monitored               BOOLEAN NOT NULL,
    profile_id              TEXT NOT NULL,
    root_folder_id          TEXT NOT NULL,
    root_folder_path        TEXT NOT NULL,
    added_at                TIMESTAMP NOT NULL,
    last_metadata_refresh   TIMESTAMP,
    poster_url              TEXT,
    backdrop_url            TEXT
);

CREATE INDEX series_monitored_idx ON series(monitored);

CREATE TABLE seasons (
    id              TEXT PRIMARY KEY NOT NULL,
    series_id       TEXT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    number          INTEGER NOT NULL,
    monitored       BOOLEAN NOT NULL,
    episode_count   INTEGER NOT NULL,
    aired_count     INTEGER NOT NULL,
    UNIQUE(series_id, number)
);

CREATE INDEX seasons_series_idx ON seasons(series_id);

CREATE TABLE episodes (
    id                  TEXT PRIMARY KEY NOT NULL,
    series_id           TEXT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season              INTEGER NOT NULL,
    number              INTEGER NOT NULL,
    absolute_number     INTEGER,
    scene_season        INTEGER,
    scene_episode       INTEGER,
    title               TEXT,
    air_date            TEXT,
    monitored           BOOLEAN NOT NULL,
    status_json         JSON NOT NULL,
    status_kind         TEXT NOT NULL,
    file_path           TEXT,
    quality_id          TEXT,
    format_score        INTEGER NOT NULL,
    updated_at          TIMESTAMP NOT NULL,
    UNIQUE(series_id, season, number)
);

CREATE INDEX episodes_series_idx ON episodes(series_id);
CREATE INDEX episodes_status_idx ON episodes(status_kind);
