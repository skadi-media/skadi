CREATE TABLE download_categories (name TEXT PRIMARY KEY NOT NULL, save_path TEXT, seed_ratio TEXT, seed_time_mins BIGINT, seed_action TEXT, updated_at TIMESTAMPTZ NOT NULL);
