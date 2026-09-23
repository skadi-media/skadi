-- Blocklist of failed/bad releases (SKADI-T-0115). A release is recorded on a
-- download/import failure (or a manual block) and excluded from future
-- search/decide so the sweep stops re-grabbing the same dead torrent.
-- `release_key` is the canonical cross-indexer identity (magnet info-hash or
-- fetch URL); `acquirable_ref` scopes a block to one acquirable (optional).
-- TTL/auto-expiry is deferred.

CREATE TABLE blocklist (
    id TEXT PRIMARY KEY NOT NULL,
    release_key TEXT NOT NULL,
    title TEXT NOT NULL,
    acquirable_ref TEXT,
    indexer TEXT,
    reason TEXT,
    at TIMESTAMP NOT NULL
);
