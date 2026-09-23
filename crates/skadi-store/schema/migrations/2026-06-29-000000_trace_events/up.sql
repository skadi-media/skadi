-- Hunter trace stream (SKADI-T-0323): an append-only, structured per-step event
-- log of what the daemon's acquire pipeline did — candidates found, the decision
-- and why, snatch → downloader, download stalls/failures, import outcome. The
-- richer companion to `history` (coarse grabbed/imported/failed outcomes), built
-- for *diagnosing the hunter* against ephemeral/inconsistent torrent sources.
-- Cross-domain (keyed by the opaque `acquirable_ref`); `detail` holds an optional
-- structured/long payload. Append-only; emitted best-effort off the acquire path.

CREATE TABLE trace_events (
    id TEXT PRIMARY KEY NOT NULL,
    at TIMESTAMP NOT NULL,
    run_id TEXT,
    kind TEXT NOT NULL,
    acquirable_ref TEXT NOT NULL,
    stage TEXT NOT NULL,
    event TEXT NOT NULL,
    message TEXT NOT NULL,
    detail TEXT
);
