CREATE TABLE import_lists (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL, target_domain TEXT NOT NULL, settings_json TEXT NOT NULL, profile_id TEXT, root_folder TEXT, add_monitored BOOLEAN NOT NULL DEFAULT false, enabled BOOLEAN NOT NULL DEFAULT true, interval_minutes INTEGER NOT NULL DEFAULT 720, last_synced_at TIMESTAMPTZ, last_error TEXT, created_at TIMESTAMPTZ NOT NULL, UNIQUE (name));

CREATE INDEX import_lists_enabled_idx ON import_lists(enabled);

CREATE TABLE import_list_exclusions (id TEXT PRIMARY KEY NOT NULL, target_domain TEXT NOT NULL, id_kind TEXT NOT NULL, external_id TEXT NOT NULL, title TEXT, created_at TIMESTAMPTZ NOT NULL, UNIQUE (target_domain, id_kind, external_id));

CREATE INDEX import_list_exclusions_lookup_idx ON import_list_exclusions(target_domain,id_kind,external_id);
