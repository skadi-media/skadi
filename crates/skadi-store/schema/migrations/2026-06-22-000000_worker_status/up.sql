-- Worker liveness heartbeat (SKADI-T-0288). The download worker upserts its row
-- every tick (independent of claimed jobs), so the daemon can tell a healthy-idle
-- worker from a dead one — the gap the per-job lease couldn't fill. Liveness =
-- last_seen_at within a freshness window.
CREATE TABLE worker_status (
    worker_id     TEXT PRIMARY KEY NOT NULL,
    last_seen_at  TIMESTAMP NOT NULL,
    version       TEXT NOT NULL
);
