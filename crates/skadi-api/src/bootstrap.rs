//! Daemon bootstrap: create the cloacina database (Postgres), then apply every
//! schema migration — `skadi-store`'s plus each registered domain's.
//!
//! This runs once at startup, before the supervisor (SKADI-T-0052) spawns any
//! workers and before the HTTP server accepts traffic. It is idempotent:
//! re-running against an already-migrated database is a no-op.
//!
//! ## Migration delivery (SKADI-T-0051, option A)
//!
//! Each [`DomainModule`] ships its schema as Diesel
//! [`EmbeddedMigrations`](diesel_migrations::EmbeddedMigrations), one set per
//! backend. Bootstrap selects the set matching the configured backend and
//! applies it on a fresh synchronous connection inside `spawn_blocking`
//! (`diesel_migrations` is sync). Domain schemas are independent (no
//! cross-crate foreign keys), so application order between domains doesn't
//! matter; within a crate the timestamped migration dirs are already ordered.

use std::sync::Arc;

use diesel::Connection;
use diesel::pg::PgConnection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness};

use skadi_core::{AppError, DomainModule, Result};
use skadi_store::{ConfigRepo, ConfigSource, DomainState, DomainStateRepo, SettingsRepo, Store};

use crate::config::Config;

/// Config key marking that the one-time mode presets have run, so subsequent
/// boots never re-seed (and never clobber operator edits).
const PRESETS_SEEDED_KEY: &str = "bootstrap.presets_seeded";
/// Comma-separated ids of every default custom format this database has been
/// offered (SKADI-T-0584). The record of what was *seen*, not what is *present* —
/// that distinction is what lets a deliberately deleted format stay deleted while
/// a genuinely new default still arrives.
const FORMATS_SEEDED_KEY: &str = "bootstrap.default_formats_seen";

/// The dedicated database Cloacina runs its workflow tables in on Postgres
/// deployments (see `skadi_hunter`). Bootstrap creates it on demand so the
/// hunter's runner can connect at startup.
const CLOACINA_DATABASE: &str = "cloacina";

/// The migration versions of every registered domain, for the backend of
/// `store` (SKADI-T-0682). [`bootstrap`] applies these sets into the store's
/// database; the `database` health check (`AppState::domain_migrations`) needs
/// them to compare the applied schema with this binary.
pub fn domain_migration_versions(
    store: &Store,
    registry: &[Arc<dyn DomainModule>],
) -> Result<Vec<String>> {
    let mut versions = Vec::new();
    for module in registry {
        versions.extend(
            store.migration_versions(module.sqlite_migrations(), module.postgres_migrations())?,
        );
    }
    versions.sort();
    Ok(versions)
}

/// Bring the database up to date for the configured backend and domain set.
///
/// 1. On Postgres, ensure the `cloacina` database exists (needs `CREATEDB`).
/// 2. Apply `skadi-store`'s migrations.
/// 3. Apply each registered domain's migrations.
pub async fn bootstrap(config: &Config, registry: &[Arc<dyn DomainModule>]) -> Result<()> {
    let url = config.database_url.clone();
    let is_pg = url.starts_with("postgres://") || url.starts_with("postgresql://");

    // Idempotent; no-op on SQLite. (The daemon should also call this *before*
    // building domain modules — see the fn docs — but calling again here keeps
    // `bootstrap` correct when invoked standalone.)
    ensure_cloacina_database(&url).await?;

    // skadi-store's own schema.
    let store = Store::connect(&url)?;
    store.run_migrations().await?;

    // Seed the config plane (SKADI-I-0014): every set Tier-1 `SKADI_*` env var
    // is upserted into the `config` table with `source = env`, overwriting any
    // prior value — env is the boot-time authority. Tier-0 (`database_url`,
    // `secret_key`) is read directly by its consumers, never routed here.
    let seeded = seed_config_from_env(&store).await?;

    // Seed a (disabled) `domains` row for every registered module so the
    // supervisor and the domains API see the full compiled-in set immediately.
    for module in registry {
        if store.get(module.name()).await?.is_none() {
            store.upsert(&DomainState::new(module.name())).await?;
        }
    }

    // Each domain's schema, on a fresh synchronous connection.
    if is_pg {
        let sets: Vec<EmbeddedMigrations> =
            registry.iter().map(|m| m.postgres_migrations()).collect();
        let url = url.clone();
        run_blocking(move || apply_pg_migrations(&url, sets)).await?;
    } else {
        let path = sqlite_path(&url).to_string();
        let sets: Vec<EmbeddedMigrations> =
            registry.iter().map(|m| m.sqlite_migrations()).collect();
        run_blocking(move || apply_sqlite_migrations(&path, sets)).await?;
    }

    // Mode-keyed runtime presets (just-go / testing) — once, non-clobbering.
    let domain_names: Vec<&str> = registry.iter().map(|m| m.name()).collect();
    let mode = seed_mode_presets(&store, &domain_names).await?;

    // Opinionated defaults so a deploy comes up *ready* rather than BYO-providers:
    // the built-in downloader + every public indexer + AudioBookBay, ensured on
    // every startup. Skipped in Testing (hermetic stub indexer instead).
    if mode != skadi_config::Mode::Testing
        && let Err(e) = ensure_default_providers(&store).await
    {
        tracing::warn!("seeding default providers failed (non-fatal): {e}");
    }

    tracing::info!(
        backend = if is_pg { "postgres" } else { "sqlite" },
        domains = registry.len(),
        config_seeded = seeded,
        mode = ?mode,
        "bootstrap complete"
    );
    Ok(())
}

/// Ensure the opinionated default providers exist so a deploy comes up ready: the
/// built-in DB-queue downloader, every **public** (no-login) catalog indexer, and
/// **AudioBookBay**. Idempotent and run on every (non-testing) startup — a fresh or
/// upgraded deploy always has them, and a deleted one returns next start (they are
/// "always on"). Non-fatal: a catalog/HTTP failure logs and leaves the set as-is.
/// Egress for these rides the configured VPN proxy (the deploy wires it).
pub async fn ensure_default_providers(store: &Store) -> Result<()> {
    use skadi_indexers::definitions::DefinitionStore;

    // 1) The built-in DB-queue downloader (the worker that drains the queue). Its
    // incomplete/complete dirs are derived from the single library root
    // (SKADI-T-0302/0305): `<library.root>/downloads/{incomplete,complete}` — same
    // mount as the library, so the importer hardlinks. Without this the downloader
    // would fall back to the generic `/data/downloads/*` defaults and ignore the
    // deploy's actual mount.
    let have_downloader = store
        .list_settings("downloaders")
        .await?
        .iter()
        .any(|s| s.body.get("kind").and_then(|v| v.as_str()) == Some("skadi"));
    if !have_downloader {
        let library_root = load_config_view(store)
            .await
            .ok()
            .and_then(|v| v.get_path("library.root").ok().flatten())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/data".into());
        store
            .put_setting(
                "downloaders",
                &uuid::Uuid::new_v4().to_string(),
                &serde_json::json!({
                    "kind": "skadi",
                    "name": "built-in",
                    "incomplete_dir": format!("{library_root}/downloads/incomplete"),
                    "complete_dir": format!("{library_root}/downloads/complete"),
                }),
            )
            .await?;
        tracing::info!("seeded the built-in downloader");
    }

    // 2) Every public indexer + AudioBookBay from the catalog (defaults filled in).
    let dir = load_config_view(store)
        .await
        .ok()
        .and_then(|v| v.get_string("cardigann_definitions_dir").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "./definitions".into());
    let http = match skadi_http::HttpClient::new(std::time::Duration::from_secs(30)) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("default-providers: http client: {e}");
            return Ok(());
        }
    };
    let catalog = match DefinitionStore::new(dir, http).load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("default-providers: catalog load: {e}");
            return Ok(());
        }
    };
    let existing_settings = store.list_settings("indexers").await?;
    let existing: std::collections::HashSet<String> = existing_settings
        .iter()
        .filter_map(|s| {
            s.body
                .get("definition_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();

    // Curated scope (operator decision): **English**, **Movies / TV / Books /
    // Audiobooks**, **public** trackers only — no porn, no anime, no foreign-language.
    // Adult is caught two ways (all-XXX categories *and* a name/description keyword,
    // since some adult trackers file hentai under Books); anime by keyword.
    const ADULT_KW: &[&str] = &[
        "xxx", "porn", "adult", "hentai", "jav", "sukebei", "sex", "nsfw", "rape", "incest",
    ];
    const ANIME_KW: &[&str] = &[
        "anime",
        "bangumi",
        "nyaa",
        "tokyotosho",
        "toshokan",
        "shana",
        "nekobt",
        "anisource",
        "acg",
    ];
    let adult_cat = |c: &str| {
        let c = c.to_ascii_lowercase();
        c.starts_with("xxx") || c.contains("porn") || c.contains("adult")
    };
    let wanted_cat = |c: &str| {
        let c = c.to_ascii_lowercase();
        c.starts_with("movies")
            || c.starts_with("tv")
            || c.starts_with("books")
            || c.starts_with("audio/audiobook")
    };

    // The curated definition ids that should be registered right now.
    let wanted: std::collections::HashSet<String> = catalog
        .list()
        .into_iter()
        .filter(|entry| {
            let blob =
                format!("{} {} {}", entry.id, entry.name, entry.description).to_ascii_lowercase();
            let public = entry.privacy == "public" && !entry.needs_login;
            let english = entry.language.to_ascii_lowercase().starts_with("en");
            let has_content = entry.categories.iter().any(|c| wanted_cat(c));
            let adult = (!entry.categories.is_empty()
                && entry.categories.iter().all(|c| adult_cat(c)))
                || ADULT_KW.iter().any(|k| blob.contains(k));
            let anime = ANIME_KW.iter().any(|k| blob.contains(k));
            public && english && has_content && !adult && !anime
        })
        .map(|e| e.id.clone())
        .collect();

    // Seed any missing curated tracker, marked `default_seeded` so the prune below
    // can distinguish our auto-seeds from operator-added trackers.
    let mut added = 0;
    for entry in catalog.list() {
        if !wanted.contains(&entry.id) || existing.contains(&entry.id) {
            continue;
        }
        // Pre-fill non-secret settings with their definition defaults (e.g. TPB's
        // `apiurl=apibay.org`); secrets aren't needed for public trackers.
        let mut settings = serde_json::Map::new();
        for s in &entry.settings {
            if s.kind != "password"
                && let Some(def) = &s.default
            {
                settings.insert(s.name.clone(), serde_json::Value::String(def.clone()));
            }
        }
        store
            .put_setting(
                "indexers",
                &uuid::Uuid::new_v4().to_string(),
                &serde_json::json!({
                    "kind": "cardigann",
                    "name": entry.name,
                    "definition_id": entry.id,
                    "settings": serde_json::Value::Object(settings),
                    "default_seeded": true,
                }),
            )
            .await?;
        added += 1;
    }

    // Prune any **auto-seeded** indexer no longer in scope (the filter narrowed, or
    // upstream dropped it). Operator-added trackers (no `default_seeded` marker) are
    // never touched — so a hand-added private tracker is safe.
    let mut pruned = 0;
    for s in &existing_settings {
        let auto = s
            .body
            .get("default_seeded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let still_wanted = s
            .body
            .get("definition_id")
            .and_then(|v| v.as_str())
            .map(|d| wanted.contains(d))
            .unwrap_or(true);
        if auto && !still_wanted {
            store.delete_setting("indexers", &s.id).await?;
            pruned += 1;
        }
    }

    if added > 0 || pruned > 0 {
        tracing::info!(added, pruned, "curated default indexers reconciled");
    }
    Ok(())
}

/// Upsert every set, non-empty Tier-1 `SKADI_*` env var into the `config` table
/// with `source = env`. Idempotent across restarts (it's an upsert); env always
/// overwrites whatever was there (including a prior `runtime` value). Returns
/// the number of keys seeded. Tier-0 keys are excluded by `read_env`.
pub async fn seed_config_from_env(store: &Store) -> Result<usize> {
    let mut n = 0;
    for (key, value) in skadi_config::read_env() {
        store.set_config(key, &value, ConfigSource::Env).await?;
        n += 1;
    }
    Ok(n)
}

/// Load a [`ConfigView`](skadi_config::ConfigView) — a typed read snapshot of
/// the `config` table (SKADI-I-0014). Build a fresh one on the supervisor tick
/// for a **hot** consumer, or once at startup for a **lazy** one.
pub async fn load_config_view(store: &Store) -> Result<skadi_config::ConfigView> {
    let pairs = store
        .list_config()
        .await?
        .into_iter()
        .map(|e| (e.key, e.value));
    Ok(skadi_config::ConfigView::from_pairs(pairs))
}

/// Seed runtime provider settings based on the `mode` config key, **once**
/// (SKADI-I-0014, SKADI-T-0102) — the "spin up and it works" payoff:
///
/// - **all modes**: the built-in default quality-profile set (Any / SD /
///   HD-720p / HD-1080p / HD-720p/1080p / Ultra-HD), if the operator has none —
///   so a fresh install ships with profiles to select from (SKADI-T-0145).
/// - `production` (default): seeds nothing *beyond* those profiles.
/// - `just-go`: also adds a default root folder (if absent).
/// - `testing`: everything `just-go` does **plus** the built-in skadi
///   downloader and every registered domain enabled, with throwaway paths.
///
/// Idempotent and non-clobbering: it only fills a settings kind that is empty,
/// and runs the whole pass exactly once (guarded by [`PRESETS_SEEDED_KEY`]), so
/// later operator edits are never overwritten. Returns the resolved mode.
///
/// NOTE: `testing` is not yet *fully* self-contained — it still needs an
/// indexer. A built-in stub/canned indexer is tracked separately (SKADI-T-0104).
/// Seed default custom formats this database has **never been offered**
/// (SKADI-T-0584).
///
/// Keyed on a seen-set rather than on what is currently present, so:
/// - a default added in a later version reaches an existing install;
/// - a default the operator **deleted** is never resurrected;
/// - a default the operator **edited** is never overwritten.
///
/// The first boot records every current default as seen, which is why an install
/// that already had the original four does not suddenly gain them back — it
/// gains only the ones that did not exist when it was seeded.
async fn seed_new_default_formats(store: &Store) -> Result<()> {
    let seen: std::collections::HashSet<String> = store
        .get_config(FORMATS_SEEDED_KEY)
        .await?
        .map(|e| {
            e.value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // An install that predates this key has already been seeded with whatever
    // defaults existed then. Treat what it currently HOLDS as seen, so the first
    // run of this code adds the genuinely-new formats without resurrecting
    // anything the operator removed earlier.
    let present: std::collections::HashSet<String> = store
        .list_settings("custom_formats")
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
    let mut seen: std::collections::HashSet<String> = seen.union(&present).cloned().collect();

    let defaults = skadi_quality::default_formats();
    for f in &defaults {
        let id = f.id.to_string();
        if seen.contains(&id) {
            continue;
        }
        let body = serde_json::json!({
            "domain": "Movie",
            "name": f.name,
            "rules": f.rules,
        });
        store.put_setting("custom_formats", &id, &body).await?;
        tracing::info!(format = %f.name, "seeded new default custom format");
        seen.insert(id);
    }

    let mut ids: Vec<String> = seen.into_iter().collect();
    // Sorted so the stored value is stable and diffable rather than reordering
    // on every boot.
    ids.sort();
    store
        .set_config(FORMATS_SEEDED_KEY, &ids.join(","), ConfigSource::Runtime)
        .await?;
    Ok(())
}

pub async fn seed_mode_presets(store: &Store, domain_names: &[&str]) -> Result<skadi_config::Mode> {
    let mode = match store.get_config("mode").await? {
        Some(e) => e.value.parse().unwrap_or_default(),
        None => skadi_config::Mode::default(),
    };
    // Custom-format defaults are reconciled on EVERY boot, before the presets
    // guard below (SKADI-T-0584).
    //
    // The presets pass runs once per database, which meant the default format set
    // could never grow: an install seeded with the original four would never
    // receive a later default, so the direct-play formats added for the re-grab
    // work would have reached new installs only.
    //
    // It seeds ids this database has NEVER SEEN, not ids that are merely absent.
    // Those are different: a format the operator deleted on purpose must stay
    // deleted, and blanket-restoring every missing default would resurrect it on
    // the next restart.
    seed_new_default_formats(store).await?;

    // Run the presets exactly once across the lifetime of this database.
    if store.get_config(PRESETS_SEEDED_KEY).await?.is_some() {
        return Ok(mode);
    }

    // Default quality profiles — seeded on first boot in EVERY mode (including
    // production), only if the operator has none, so a fresh install ships with a
    // usable set (Any / SD / HD-720p / HD-1080p / HD-720p/1080p / Ultra-HD) the
    // add/import dropdowns can select from (SKADI-T-0145). Operators can edit or
    // delete them; the once-guard means they're never re-seeded.
    if store.list_settings("profiles").await?.is_empty() {
        let defs = skadi_quality::default_definitions();
        for p in skadi_quality::default_profiles(&defs) {
            let body = serde_json::json!({
                "name": p.name,
                "allowed": p.allowed.iter().map(|q| q.to_string()).collect::<Vec<_>>(),
                "cutoff": p.cutoff.to_string(),
                "upgrade_allowed": p.upgrade_allowed,
                "min_format_score": p.min_format_score,
            });
            store
                .put_setting("profiles", &uuid::Uuid::new_v4().to_string(), &body)
                .await?;
        }
    }

    // Default custom formats — seeded on first boot if the operator has none
    // (SKADI-T-0183), so a fresh install ships an editable starter set (Remux /
    // x265-HEVC / Repack-Proper / Freeleech). Domain-keyed: these are movie
    // formats. The row id is the format's stable id so profiles can reference it;
    // the once-guard means they're never re-seeded.
    if store.list_settings("custom_formats").await?.is_empty() {
        for f in skadi_quality::default_formats() {
            let body = serde_json::json!({
                "domain": "Movie",
                "name": f.name,
                "rules": f.rules,
            });
            store
                .put_setting("custom_formats", &f.id.to_string(), &body)
                .await?;
        }
    }

    // Beyond the profile set, seeding is dev-convenience only — production wires
    // its own providers / domains, so it stops here.
    //
    // No root folder is seeded any more (SKADI-T-0302): skadi owns one
    // `library.root` (config-plane, default `/data`) and derives every domain's
    // root as `<library.root>/<subfolder>` — there is no operator root list.

    if mode == skadi_config::Mode::Testing {
        // The built-in DB-queue downloader (only if none) — paths default.
        if store.list_settings("downloaders").await?.is_empty() {
            store
                .put_setting(
                    "downloaders",
                    &uuid::Uuid::new_v4().to_string(),
                    &serde_json::json!({ "kind": "skadi", "name": "built-in" }),
                )
                .await?;
        }
        // The built-in stub indexer (SKADI-T-0104) — canned open-movie releases,
        // so testing mode drives a real acquire with no external indexer.
        if store.list_settings("indexers").await?.is_empty() {
            store
                .put_setting(
                    "indexers",
                    &uuid::Uuid::new_v4().to_string(),
                    &serde_json::json!({ "kind": "stub", "name": "built-in (testing)" }),
                )
                .await?;
        }
        // Enable every registered domain.
        for name in domain_names {
            store.set_enabled(name, true).await?;
        }
    }

    store
        .set_config(PRESETS_SEEDED_KEY, "true", ConfigSource::Runtime)
        .await?;
    tracing::info!(mode = ?mode, "seeded mode presets");
    Ok(mode)
}

/// Strip the `sqlite://` scheme to the bare path diesel wants.
fn sqlite_path(url: &str) -> &str {
    url.strip_prefix("sqlite://").unwrap_or(url)
}

/// Run a synchronous, possibly-blocking migration closure on the blocking pool,
/// flattening the join error.
async fn run_blocking<F>(f: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Internal(format!("migration task panicked: {e}")))?
}

/// `CREATE DATABASE cloacina`, tolerating "already exists".
///
/// `CREATE DATABASE` cannot run inside a transaction, so this issues it on a
/// plain connection to the user's database (any database on the server works as
/// the entry point; the statement creates a sibling).
///
/// Public so the daemon can create the database **before** constructing domain
/// modules: a module's Cloacina runner connects to the `cloacina` database at
/// construction time (see `MoviesModule::new`), which is *earlier* than
/// [`bootstrap`] runs. On SQLite the runner's sibling file is auto-created so
/// ordering doesn't matter; on Postgres the database must already exist. The
/// call is idempotent, so [`bootstrap`] still invokes it too. No-op for SQLite
/// URLs.
pub async fn ensure_cloacina_database(url: &str) -> Result<()> {
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return Ok(());
    }
    let url = url.to_string();
    run_blocking(move || {
        use diesel::RunQueryDsl;
        let mut conn = PgConnection::establish(&url).map_err(|e| {
            AppError::Config(format!("connect to postgres for CREATE DATABASE: {e}"))
        })?;
        match diesel::sql_query(format!("CREATE DATABASE {CLOACINA_DATABASE}")).execute(&mut conn) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("already exists") => Ok(()),
            Err(e) => Err(AppError::Internal(format!(
                "CREATE DATABASE {CLOACINA_DATABASE}: {e} (the configured role needs CREATEDB)"
            ))),
        }
    })
    .await
}

fn apply_sqlite_migrations(path: &str, sets: Vec<EmbeddedMigrations>) -> Result<()> {
    let mut conn = SqliteConnection::establish(path)
        .map_err(|e| AppError::Config(format!("open sqlite database {path:?}: {e}")))?;
    for set in sets {
        // `MigrationSource` is implemented for `EmbeddedMigrations` by value, so
        // the owned handle is consumed here.
        conn.run_pending_migrations(set)
            .map_err(|e| AppError::Internal(format!("domain sqlite migration failed: {e}")))?;
    }
    Ok(())
}

fn apply_pg_migrations(url: &str, sets: Vec<EmbeddedMigrations>) -> Result<()> {
    let mut conn = PgConnection::establish(url)
        .map_err(|e| AppError::Config(format!("connect to postgres: {e}")))?;
    for set in sets {
        conn.run_pending_migrations(set)
            .map_err(|e| AppError::Internal(format!("domain postgres migration failed: {e}")))?;
    }
    Ok(())
}
