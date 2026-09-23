-- Logical schema for skadi-store (diesel-dualdb, SKADI-I-0015). ONE source of
-- truth in logical column types; the diesel-dualdb-schema CLI generates the
-- unified schema.rs + per-backend migrations. Regenerate, never hand-edit the
-- generated tree. All ids are TEXT (UUIDs are stored as strings).

CREATE TABLE domains (
    domain_name TEXT PRIMARY KEY NOT NULL,
    enabled BOOLEAN NOT NULL,
    enabled_at TIMESTAMP,
    settings_json JSON NOT NULL
);

CREATE TABLE credentials (
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    secret BYTEA NOT NULL,
    nonce BYTEA,
    PRIMARY KEY (owner_kind, owner_id)
);

CREATE TABLE settings (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    body JSON NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    PRIMARY KEY (kind, id)
);

CREATE TABLE acquisition_history (
    id TEXT PRIMARY KEY NOT NULL,
    at TIMESTAMP NOT NULL,
    kind TEXT NOT NULL,
    acquirable_ref TEXT NOT NULL,
    label TEXT NOT NULL,
    event TEXT NOT NULL,
    detail TEXT
);

CREATE TABLE downloads (
    id TEXT PRIMARY KEY NOT NULL,
    acquirable_ref TEXT NOT NULL,
    source TEXT NOT NULL,
    category TEXT,
    status TEXT NOT NULL,
    info_hash TEXT,
    progress_bytes BIGINT NOT NULL,
    total_bytes BIGINT NOT NULL,
    files JSON NOT NULL,
    error TEXT,
    worker_id TEXT,
    delete_data BOOLEAN NOT NULL,
    incomplete_dir TEXT,
    complete_dir TEXT,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE TABLE config (
    "key" TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    source TEXT NOT NULL,
    updated_at TIMESTAMP NOT NULL
);
