CREATE TABLE library_files (id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL, acquirable_ref TEXT NOT NULL, path TEXT NOT NULL, dev BIGINT NOT NULL, ino BIGINT NOT NULL, size_bytes BIGINT NOT NULL, disk_bytes BIGINT NOT NULL, links BIGINT NOT NULL, mtime TIMESTAMPTZ NOT NULL, pieces BIGINT, pieces_at TIMESTAMPTZ, checksum TEXT, checksum_at TIMESTAMPTZ, stat_at TIMESTAMPTZ NOT NULL, gone_at TIMESTAMPTZ);

CREATE UNIQUE INDEX library_files_path_idx ON library_files(path);

CREATE INDEX library_files_kind_idx ON library_files(kind);

CREATE INDEX library_files_ref_idx ON library_files(kind,acquirable_ref);

CREATE INDEX library_files_pieces_at_idx ON library_files(pieces_at);

CREATE INDEX library_files_checksum_at_idx ON library_files(checksum_at);

CREATE TABLE library_file_samples (id TEXT PRIMARY KEY NOT NULL, file_id TEXT NOT NULL, at TIMESTAMPTZ NOT NULL, size_bytes BIGINT NOT NULL, links BIGINT NOT NULL, pieces BIGINT);

CREATE INDEX library_file_samples_file_idx ON library_file_samples(file_id,at);

CREATE INDEX library_file_samples_at_idx ON library_file_samples(at);

CREATE TABLE library_orphans (path TEXT PRIMARY KEY NOT NULL, size_bytes BIGINT NOT NULL, mtime TIMESTAMPTZ NOT NULL, first_seen TIMESTAMPTZ NOT NULL, last_seen TIMESTAMPTZ NOT NULL);

CREATE TABLE storage_probe_state (probe TEXT PRIMARY KEY NOT NULL, state TEXT NOT NULL, failed_at TIMESTAMPTZ, reason TEXT, cleared_at TIMESTAMPTZ, cleared_by TEXT);
