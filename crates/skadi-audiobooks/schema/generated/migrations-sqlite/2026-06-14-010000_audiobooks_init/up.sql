CREATE TABLE authors (id TEXT PRIMARY KEY NOT NULL, asin TEXT UNIQUE, name TEXT NOT NULL, description TEXT, image_url TEXT, monitored INTEGER NOT NULL, added_at TEXT NOT NULL, last_metadata_refresh TEXT);

CREATE INDEX authors_monitored_idx ON authors(monitored);

CREATE TABLE book_series (id TEXT PRIMARY KEY NOT NULL, asin TEXT UNIQUE, name TEXT NOT NULL UNIQUE);

CREATE TABLE books (id TEXT PRIMARY KEY NOT NULL, asin TEXT UNIQUE, title TEXT NOT NULL, subtitle TEXT, author_id TEXT REFERENCES authors (id) ON DELETE SET NULL, authors_json TEXT NOT NULL, narrators_json TEXT NOT NULL, series_id TEXT REFERENCES book_series (id) ON DELETE SET NULL, series_position TEXT, year INTEGER, overview TEXT, runtime_minutes INTEGER, cover_url TEXT, release_date TEXT, monitored INTEGER NOT NULL, profile_id TEXT NOT NULL, root_folder_id TEXT NOT NULL, root_folder_path TEXT NOT NULL, added_at TEXT NOT NULL, last_metadata_refresh TEXT);

CREATE INDEX books_monitored_idx ON books(monitored);

CREATE INDEX books_author_idx ON books(author_id);

CREATE TABLE book_files (id TEXT PRIMARY KEY NOT NULL, book_id TEXT NOT NULL REFERENCES books (id) ON DELETE CASCADE, status_json TEXT NOT NULL, status_kind TEXT NOT NULL, file_path TEXT, quality_id TEXT, format_score INTEGER NOT NULL, updated_at TEXT NOT NULL, UNIQUE (book_id));

CREATE INDEX book_files_book_idx ON book_files(book_id);

CREATE INDEX book_files_status_idx ON book_files(status_kind);
