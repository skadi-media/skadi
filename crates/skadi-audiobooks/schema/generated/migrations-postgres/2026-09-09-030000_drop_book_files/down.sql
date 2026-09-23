-- Recreate the shape SKADI-T-0448 migrated away from. Empty: the data lives in
-- `book_editions` now, and copying it back would create two writable homes for
-- the same rows.
CREATE TABLE book_files (
    id              TEXT PRIMARY KEY NOT NULL,
    book_id         TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    status_json     TEXT NOT NULL,
    status_kind     TEXT NOT NULL,
    file_path       TEXT,
    quality_id      TEXT,
    format_score    INTEGER NOT NULL DEFAULT 0,
    size_bytes      BIGINT,
    added_at        TEXT NOT NULL
);
