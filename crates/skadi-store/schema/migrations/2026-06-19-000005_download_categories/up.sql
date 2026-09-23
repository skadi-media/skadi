-- First-class download categories (SKADI-T-0215): qBittorrent-style category objects
-- carrying a save-path and optional per-category seed-policy overrides. A download's
-- `category` string resolves to one of these; NULL override columns fall back to the
-- worker's global settings.
CREATE TABLE download_categories (
    name TEXT PRIMARY KEY NOT NULL,
    save_path TEXT,
    seed_ratio TEXT,
    seed_time_mins BIGINT,
    seed_action TEXT,
    updated_at TIMESTAMP NOT NULL
);
