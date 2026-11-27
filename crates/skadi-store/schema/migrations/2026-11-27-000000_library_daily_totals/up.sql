-- One row per day and kind: what the library held that day (SKADI-T-0776,
-- design SKADI-D-0040). Written at the end of a storage sweep, so a trend is
-- one cheap read. The per-file history (`library_file_samples`) cannot give
-- it: a total at a past moment would need each file's newest sample before
-- that moment, and a file that left the library takes its samples with it.
CREATE TABLE library_daily_totals (
    -- The UTC day, `YYYY-MM-DD`.
    day            TEXT NOT NULL,
    -- `movie` | `series` | `audiobook`.
    kind           TEXT NOT NULL,
    -- Files on disk, their length and the space they take.
    files          BIGINT NOT NULL,
    size_bytes     BIGINT NOT NULL,
    disk_bytes     BIGINT NOT NULL,
    -- Files with more than one hard link.
    linked         BIGINT NOT NULL,
    -- Records whose file was not found.
    gone           BIGINT NOT NULL,
    -- Files with a current piece count, their bytes and their pieces.
    measured       BIGINT NOT NULL,
    measured_bytes BIGINT NOT NULL,
    pieces         BIGINT NOT NULL,
    -- Files with a current checksum.
    checksummed    BIGINT NOT NULL,
    -- When the row was last written; a day's row is replaced by a later pass.
    at             TIMESTAMP NOT NULL,
    PRIMARY KEY (day, kind)
);
