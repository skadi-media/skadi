-- Audiobook editions as acquirable-unit kinds (SKADI-T-0448).
--
-- A Full Cast / Dramatized / Booktrack / alternate-ASIN release is a different
-- *edition of the same work*, not a different book. Today each becomes a second
-- `books` row with its own monitored flag — the root of SKADI-T-0400, where the
-- library re-fetches an edition of a book it already owns.
--
-- Movies model this correctly with `movie_editions(movie_id, kind_id)`, and this
-- brings audiobooks to the same shape.
--
-- **Expand, don't rebuild.** `book_files` declares `UNIQUE(book_id)` as a *table
-- constraint*, which SQLite implements as an implicit index no `DROP INDEX` can
-- remove; the only in-place fix is a table rebuild, and the schema generator
-- cannot express one (SKADI-T-0555 — it follows neither `DROP TABLE` nor
-- `ALTER TABLE ... RENAME TO`, so a rebuild emits either two `book_files` or a
-- stale one). So this creates a **new** table and leaves `book_files` in place;
-- readers move across, and `book_files` is dropped in a later migration once
-- nothing reads it. That is the standard expand/migrate/contract shape and it is
-- also closer to movies, where `movie_editions` is its own table.

CREATE TABLE book_edition_kinds (
    -- The slug is the key. It is stable, readable in a database someone is
    -- debugging by hand, and is what every caller already has — keying on it
    -- removes an id-to-slug lookup on every load.
    slug        TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    -- Exactly one kind is the default: what a plain "add this book" creates and
    -- what every pre-existing file is assumed to be.
    is_default  BOOLEAN NOT NULL
);

-- Seeded here rather than by application code so a fresh database and a migrated
-- one agree.
INSERT INTO book_edition_kinds (slug, name, is_default) VALUES
    ('unabridged', 'Unabridged', TRUE),
    ('abridged',   'Abridged',   FALSE),
    ('full_cast',  'Full Cast',  FALSE),
    ('dramatized', 'Dramatized', FALSE),
    ('booktrack',  'Booktrack',  FALSE);

CREATE TABLE book_editions (
    id              TEXT PRIMARY KEY NOT NULL,
    book_id         TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    kind_slug       TEXT NOT NULL REFERENCES book_edition_kinds(slug),
    status_json     JSON NOT NULL,
    status_kind     TEXT NOT NULL,
    file_path       TEXT,
    quality_id      TEXT,
    format_score    INTEGER NOT NULL,
    updated_at      TIMESTAMP NOT NULL,
    media_info      TEXT,
    -- One row per (book, kind) — the point of the whole ticket.
    UNIQUE(book_id, kind_slug)
);

-- Carry every existing file across as its book's Unabridged edition. That is the
-- honest default: it is what the ingest path produced before kinds existed, and
-- inferring otherwise from a filename would mislabel a whole library in a
-- migration nobody watched.
INSERT INTO book_editions
    (id, book_id, kind_slug, status_json, status_kind, file_path, quality_id,
     format_score, updated_at, media_info)
SELECT
    id, book_id, 'unabridged', status_json, status_kind, file_path, quality_id,
    format_score, updated_at, media_info
FROM book_files;

CREATE INDEX book_editions_book_idx   ON book_editions(book_id);
CREATE INDEX book_editions_status_idx ON book_editions(status_kind);
CREATE INDEX book_editions_kind_idx   ON book_editions(kind_slug);
