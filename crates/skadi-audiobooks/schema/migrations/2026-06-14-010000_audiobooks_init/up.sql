-- skadi-audiobooks logical schema (diesel-dualdb, SKADI-I-0017). One source of
-- truth; the CLI generates the unified schema.rs + per-backend migrations.
-- All ids are TEXT (UUIDs stored as strings) — matching skadi-store. The model
-- is Author -> Series -> Book, with one audiobook (book_file) per book. There is
-- no edition-kind registry: abridged-vs-unabridged is a quality-axis concern,
-- not a separate acquirable.

CREATE TABLE authors (
    id                      TEXT PRIMARY KEY NOT NULL,
    asin                    TEXT UNIQUE,
    name                    TEXT NOT NULL,
    description             TEXT,
    image_url               TEXT,
    monitored               BOOLEAN NOT NULL,
    added_at                TIMESTAMP NOT NULL,
    last_metadata_refresh   TIMESTAMP
);

CREATE INDEX authors_monitored_idx ON authors(monitored);

CREATE TABLE book_series (
    id      TEXT PRIMARY KEY NOT NULL,
    asin    TEXT UNIQUE,
    name    TEXT NOT NULL UNIQUE
);

CREATE TABLE books (
    id                      TEXT PRIMARY KEY NOT NULL,
    asin                    TEXT UNIQUE,
    title                   TEXT NOT NULL,
    subtitle                TEXT,
    author_id               TEXT REFERENCES authors(id) ON DELETE SET NULL,
    authors_json            JSON NOT NULL,
    narrators_json          JSON NOT NULL,
    series_id               TEXT REFERENCES book_series(id) ON DELETE SET NULL,
    series_position         TEXT,
    year                    INTEGER,
    overview                TEXT,
    runtime_minutes         INTEGER,
    cover_url               TEXT,
    release_date            TEXT,
    monitored               BOOLEAN NOT NULL,
    profile_id              TEXT NOT NULL,
    root_folder_id          TEXT NOT NULL,
    root_folder_path        TEXT NOT NULL,
    added_at                TIMESTAMP NOT NULL,
    last_metadata_refresh   TIMESTAMP
);

CREATE INDEX books_monitored_idx ON books(monitored);
CREATE INDEX books_author_idx   ON books(author_id);

CREATE TABLE book_files (
    id              TEXT PRIMARY KEY NOT NULL,
    book_id         TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    status_json     JSON NOT NULL,
    status_kind     TEXT NOT NULL,
    file_path       TEXT,
    quality_id      TEXT,
    format_score    INTEGER NOT NULL,
    updated_at      TIMESTAMP NOT NULL,
    UNIQUE(book_id)
);

CREATE INDEX book_files_book_idx   ON book_files(book_id);
CREATE INDEX book_files_status_idx ON book_files(status_kind);
