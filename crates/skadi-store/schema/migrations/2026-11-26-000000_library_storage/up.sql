-- Storage facts of the library's files (SKADI-T-0772, design SKADI-D-0040).
--
-- In the shared store, not one table per domain: the facts are the same for a
-- film, an episode and an audiobook file, and the dashboard reads them as one
-- library. Format and audio stay in each domain's `media_info`.

-- One row per file the library holds.
CREATE TABLE library_files (
    id             TEXT PRIMARY KEY NOT NULL,
    -- `movie` | `series` | `audiobook`, matching MediaKind's serialised form.
    kind           TEXT NOT NULL,
    -- The edition or episode the file belongs to. Not a foreign key: the
    -- referent lives in another crate's schema, as `history.acquirable_ref` does.
    acquirable_ref TEXT NOT NULL,
    -- As Skadi sees it. The key of an upsert.
    path           TEXT NOT NULL,
    -- File identity, to follow a rename and to find files that share data.
    dev            BIGINT NOT NULL,
    ino            BIGINT NOT NULL,
    -- Length, and space used on disk.
    size_bytes     BIGINT NOT NULL,
    disk_bytes     BIGINT NOT NULL,
    -- Hard link count.
    links          BIGINT NOT NULL,
    -- The file's own modification time.
    mtime          TIMESTAMP NOT NULL,
    -- Extents. `pieces_at` NULL = to be measured (never, or the file changed
    -- since); `pieces` then holds the last known count, if any.
    pieces         BIGINT,
    pieces_at      TIMESTAMP,
    -- The same rule as pieces: `checksum_at` NULL = to be computed.
    checksum       TEXT,
    checksum_at    TIMESTAMP,
    -- When size, links and identity were last read.
    stat_at        TIMESTAMP NOT NULL,
    -- Set when the file was not found; cleared when it is back.
    gone_at        TIMESTAMP
);

CREATE UNIQUE INDEX library_files_path_idx ON library_files(path);
CREATE INDEX library_files_kind_idx ON library_files(kind);
CREATE INDEX library_files_ref_idx ON library_files(kind, acquirable_ref);
-- The two work queues: least recently measured first.
CREATE INDEX library_files_pieces_at_idx ON library_files(pieces_at);
CREATE INDEX library_files_checksum_at_idx ON library_files(checksum_at);

-- Files under the library root that no record points at. A file leaves the
-- table when a record claims it or it is gone from disk.
CREATE TABLE library_orphans (
    path       TEXT PRIMARY KEY NOT NULL,
    size_bytes BIGINT NOT NULL,
    mtime      TIMESTAMP NOT NULL,
    first_seen TIMESTAMP NOT NULL,
    last_seen  TIMESTAMP NOT NULL
);

-- One row per probe kind that has failed at least once. No row = the probe
-- has never failed. `state` is `on` | `failed`; a failed probe stays off until
-- an operator clears it.
CREATE TABLE storage_probe_state (
    probe      TEXT PRIMARY KEY NOT NULL,
    state      TEXT NOT NULL,
    failed_at  TIMESTAMP,
    reason     TEXT,
    cleared_at TIMESTAMP,
    cleared_by TEXT
);
