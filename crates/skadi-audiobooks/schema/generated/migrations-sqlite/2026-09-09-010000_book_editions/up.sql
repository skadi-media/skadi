CREATE TABLE book_edition_kinds (slug TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, is_default INTEGER NOT NULL);

INSERT INTO book_edition_kinds (slug, name, is_default) VALUES ('unabridged', 'Unabridged', true), ('abridged', 'Abridged', false), ('full_cast', 'Full Cast', false), ('dramatized', 'Dramatized', false), ('booktrack', 'Booktrack', false);

CREATE TABLE book_editions (id TEXT PRIMARY KEY NOT NULL, book_id TEXT NOT NULL REFERENCES books (id) ON DELETE CASCADE, kind_slug TEXT NOT NULL REFERENCES book_edition_kinds (slug), status_json TEXT NOT NULL, status_kind TEXT NOT NULL, file_path TEXT, quality_id TEXT, format_score INTEGER NOT NULL, updated_at TEXT NOT NULL, media_info TEXT, UNIQUE (book_id, kind_slug));

INSERT INTO book_editions (id, book_id, kind_slug, status_json, status_kind, file_path, quality_id, format_score, updated_at, media_info) SELECT id, book_id, 'unabridged', status_json, status_kind, file_path, quality_id, format_score, updated_at, media_info FROM book_files;

CREATE INDEX book_editions_book_idx ON book_editions(book_id);

CREATE INDEX book_editions_status_idx ON book_editions(status_kind);

CREATE INDEX book_editions_kind_idx ON book_editions(kind_slug);
