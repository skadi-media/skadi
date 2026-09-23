-- Import lists (SKADI-T-0511, component C19).
--
-- Nothing existed: no provider trait, no storage, no sync. Sonarr and Radarr both
-- have this, and it is how most people actually populate a library — follow a
-- TMDB collection or a Trakt list and let the daemon add what appears.
--
-- **In the shared store, not per-domain**, for the same reason as `item_tags`:
-- a list adds to whichever domain it targets, and three copies of this table in
-- three schemas would need three migrations to change one thing. The `kind`
-- column says which provider, `target_domain` says where items land.
CREATE TABLE import_lists (
    id                  TEXT PRIMARY KEY NOT NULL,
    -- Operator-facing name, unique so a list can be spoken about.
    name                TEXT NOT NULL,
    -- Provider slug: `tmdb_collection`, `tmdb_list`, `trakt_list`, …
    kind                TEXT NOT NULL,
    -- `movie` | `series` | `audiobook`, matching MediaKind's serialised form.
    target_domain       TEXT NOT NULL,
    -- Provider-specific settings (list id, url, credentials reference).
    -- Opaque JSON so a new provider does not need a migration.
    settings_json       TEXT NOT NULL,
    -- Defaults applied to everything this list adds.
    profile_id          TEXT,
    root_folder         TEXT,
    -- Whether items arrive monitored. Defaults FALSE: a list that silently
    -- starts acquiring on its first sync is how someone wakes up to a full disk.
    -- Opting in is one toggle; undoing a hundred grabs is not.
    add_monitored       BOOLEAN NOT NULL DEFAULT FALSE,
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    -- How often to sync, in minutes.
    interval_minutes    INTEGER NOT NULL DEFAULT 720,
    last_synced_at      TIMESTAMP,
    -- Why the last sync failed, if it did. Kept on the row so the list page can
    -- show a broken list without cross-referencing the log.
    last_error          TEXT,
    created_at          TIMESTAMP NOT NULL,
    UNIQUE(name)
);

CREATE INDEX import_lists_enabled_idx ON import_lists(enabled);

-- Items the operator never wants added, however many lists offer them.
--
-- Keyed by external id rather than by an internal item id, because the whole
-- point is to exclude something that is *not* in the library — there is no local
-- row to hang it off. This is what stops a removed item from being re-added on
-- the next sync, which without it is an infinite loop between the operator and
-- the daemon.
CREATE TABLE import_list_exclusions (
    id             TEXT PRIMARY KEY NOT NULL,
    -- `movie` | `series` | `audiobook`.
    target_domain  TEXT NOT NULL,
    -- Which external namespace the id belongs to: `tmdb`, `tvdb`, `imdb`, `asin`.
    -- Without this an IMDb id and a TMDB id could collide as bare strings.
    id_kind        TEXT NOT NULL,
    external_id    TEXT NOT NULL,
    -- Kept for the UI: an exclusion list of bare ids is unreviewable.
    title          TEXT,
    created_at     TIMESTAMP NOT NULL,
    UNIQUE(target_domain, id_kind, external_id)
);

CREATE INDEX import_list_exclusions_lookup_idx
    ON import_list_exclusions(target_domain, id_kind, external_id);
