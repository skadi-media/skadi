CREATE TABLE worker_status (worker_id TEXT PRIMARY KEY NOT NULL, last_seen_at TIMESTAMPTZ NOT NULL, version TEXT NOT NULL);
