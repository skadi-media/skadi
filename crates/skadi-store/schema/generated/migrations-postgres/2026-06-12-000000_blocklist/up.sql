CREATE TABLE blocklist (id TEXT PRIMARY KEY NOT NULL, release_key TEXT NOT NULL, title TEXT NOT NULL, acquirable_ref TEXT, indexer TEXT, reason TEXT, at TIMESTAMPTZ NOT NULL);
