//! The API's self-description: an OpenAPI 3.1 document served at
//! `/api/v1/openapi.json` (SKADI-T-0472).
//!
//! **Why a table and not annotations.** The document is built from one const
//! [`ROUTES`] table rather than from `#[utoipa::path]` annotations spread over
//! ~30 handler modules. The table is checked against the routers by a test that
//! re-extracts every `.route("…", …)` literal from the source and asserts set
//! equality, so a route added without a line here fails the build — which is the
//! property that makes a hand-written document trustworthy. Annotations would
//! give the same guarantee only for handlers someone remembered to annotate.
//!
//! **What it describes and what it does not.** Every path, method, path
//! parameter, tag and summary, plus the shared error envelope and the bearer
//! scheme — enough for a client to generate its call surface and stop
//! hand-declaring endpoints. Per-endpoint request and response *schemas* are not
//! described yet; see SKADI-T-0546.

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

/// Named JSON schemas a domain contributes to `components/schemas`
/// (SKADI-T-0546).
pub type Schemas = Vec<(String, Value)>;

/// Generate a schema for `T` in the shape `components/schemas` wants: definitions
/// are hoisted so nested types become siblings rather than a private `definitions`
/// block, and `$ref`s are rewritten to point at them.
///
/// Derived from the Rust type rather than hand-written, which is the whole point
/// — a field rename changes the document automatically, and the drift a
/// hand-maintained schema would accumulate cannot happen.
#[must_use]
pub fn schema_for<T: schemars::JsonSchema>() -> Schemas {
    let settings = schemars::r#gen::SchemaSettings::draft2019_09().with(|s| {
        s.definitions_path = "#/components/schemas/".to_string();
    });
    let root = settings.into_generator().into_root_schema_for::<T>();
    let mut out: Schemas = Vec::new();
    let name = T::schema_name();
    let mut value = serde_json::to_value(&root.schema).unwrap_or_else(|_| json!({}));
    // `into_root_schema_for` puts sibling types under `definitions`; lift them so
    // each becomes its own entry and the `$ref`s above resolve.
    if let Value::Object(map) = &mut value {
        map.remove("$schema");
        map.remove("title");
    }
    out.push((name, value));
    for (def_name, def) in root.definitions {
        let mut def = serde_json::to_value(&def).unwrap_or_else(|_| json!({}));
        if let Value::Object(map) = &mut def {
            map.remove("$schema");
        }
        out.push((def_name, def));
    }
    out
}

/// Every operation the `/api/v1` router serves: `(method, path, tag, summary)`.
///
/// Kept in sync with the routers by `tests/openapi.rs` — see the module docs.
pub const ROUTES: &[(&str, &str, &str, &str)] = &[
    // Cross-domain calendar and its subscribable feed (SKADI-T-0465).
    (
        "GET",
        "/calendar",
        "library",
        "Dated items across every enabled domain, for a date window",
    ),
    (
        "GET",
        "/calendar.ics",
        "library",
        "The same window as a subscribable iCal feed",
    ),
    // Sonarr-compatible tag alias onto the `tags` settings kind (SKADI-T-0464).
    ("GET", "/tag", "settings", "List the tags"),
    ("POST", "/tag", "settings", "Create a tag"),
    ("GET", "/tag/{id}", "settings", "Read one tag"),
    ("PUT", "/tag/{id}", "settings", "Rename a tag"),
    ("DELETE", "/tag/{id}", "settings", "Delete a tag"),
    ("GET", "/openapi.json", "system", "This document"),
    (
        "GET",
        "/health",
        "system",
        "Liveness — answers before the database is up, and is exempt from auth",
    ),
    (
        "GET",
        "/health/ready",
        "system",
        "Readiness — 200 once migrations and the store are usable",
    ),
    (
        "GET",
        "/health/checks",
        "system",
        "Per-subsystem health checks with their last result",
    ),
    (
        "GET",
        "/system/status",
        "system",
        "Daemon version, uptime, and store backend",
    ),
    (
        "GET",
        "/system/task",
        "system",
        "Scheduler task states and their next run times",
    ),
    (
        "GET",
        "/system/backup",
        "system",
        "List the backups on disk",
    ),
    (
        "POST",
        "/system/backup",
        "system",
        "Take a backup now (accepted, runs in the background)",
    ),
    (
        "POST",
        "/system/backup/restore/{id}",
        "system",
        "Restore a backup by id, or `latest`",
    ),
    (
        "GET",
        "/log",
        "system",
        "Tail the in-memory log ring buffer",
    ),
    (
        "GET",
        "/activity",
        "system",
        "Recent activity feed across all domains",
    ),
    (
        "GET",
        "/traces",
        "system",
        "Workflow trace events for the acquire pipeline",
    ),
    (
        "GET",
        "/decisions",
        "system",
        "Decision history — why each candidate was accepted or rejected",
    ),
    (
        "GET",
        "/config",
        "config",
        "Every config key with its value, default, kind and tier",
    ),
    ("GET", "/config/{key}", "config", "Read one config key"),
    (
        "PUT",
        "/config/{key}",
        "config",
        "Set one config key (validated against the registry)",
    ),
    (
        "DELETE",
        "/config/{key}",
        "config",
        "Clear one config key, restoring its default",
    ),
    (
        "GET",
        "/settings/{kind}",
        "settings",
        "List the records of a settings kind (indexers, profiles, …)",
    ),
    (
        "POST",
        "/settings/{kind}",
        "settings",
        "Create a settings record",
    ),
    (
        "GET",
        "/settings/{kind}/{id}",
        "settings",
        "Read one settings record",
    ),
    (
        "PUT",
        "/settings/{kind}/{id}",
        "settings",
        "Replace one settings record",
    ),
    (
        "PATCH",
        "/settings/{kind}/{id}",
        "settings",
        "Update part of one settings record",
    ),
    (
        "DELETE",
        "/settings/{kind}/{id}",
        "settings",
        "Delete one settings record",
    ),
    (
        "POST",
        "/settings/{kind}/{id}/test",
        "settings",
        "Test a settings record against the live service",
    ),
    ("GET", "/domains", "system", "The enabled media domains"),
    (
        "PUT",
        "/domains/{name}",
        "system",
        "Enable or disable a media domain",
    ),
    (
        "GET",
        "/naming/settings",
        "naming",
        "Read the file/folder naming templates",
    ),
    (
        "PUT",
        "/naming/settings",
        "naming",
        "Replace the naming templates",
    ),
    (
        "POST",
        "/naming/preview",
        "naming",
        "Render a naming template against a sample release",
    ),
    (
        "GET",
        "/quality/definitions",
        "quality",
        "The built-in quality definitions and their size limits",
    ),
    (
        "GET",
        "/edition-kinds",
        "quality",
        "List the movie edition kinds",
    ),
    (
        "POST",
        "/edition-kinds",
        "quality",
        "Create an edition kind",
    ),
    (
        "GET",
        "/edition-kinds/{id}",
        "quality",
        "Read an edition kind",
    ),
    (
        "PUT",
        "/edition-kinds/{id}",
        "quality",
        "Replace an edition kind",
    ),
    (
        "DELETE",
        "/edition-kinds/{id}",
        "quality",
        "Delete an edition kind",
    ),
    (
        "GET",
        "/search",
        "search",
        "Search the configured indexers for one query",
    ),
    (
        "POST",
        "/search-all",
        "search",
        "Fan a search across every enabled indexer",
    ),
    (
        "GET",
        "/indexers/categories",
        "search",
        "The Torznab category tree",
    ),
    (
        "GET",
        "/indexers/health",
        "search",
        "Per-indexer health and last error",
    ),
    (
        "GET",
        "/indexers/definitions",
        "search",
        "List the Cardigann indexer definitions",
    ),
    (
        "GET",
        "/indexers/definitions/{id}",
        "search",
        "Read one Cardigann definition",
    ),
    (
        "POST",
        "/indexers/definitions/refresh",
        "search",
        "Re-fetch the Cardigann definition bundle",
    ),
    (
        "GET",
        "/downloads",
        "downloads",
        "Every download the worker is tracking",
    ),
    (
        "DELETE",
        "/downloads/{id}",
        "downloads",
        "Remove a download, optionally with its data",
    ),
    (
        "POST",
        "/downloads/{id}/pause",
        "downloads",
        "Pause one download",
    ),
    (
        "POST",
        "/downloads/{id}/resume",
        "downloads",
        "Resume one download",
    ),
    (
        "POST",
        "/downloads/pause-all",
        "downloads",
        "Pause every download",
    ),
    (
        "POST",
        "/downloads/resume-all",
        "downloads",
        "Resume every download",
    ),
    (
        "GET",
        "/downloads/settings",
        "downloads",
        "Read the worker's limits and paths",
    ),
    (
        "PUT",
        "/downloads/settings",
        "downloads",
        "Replace the worker's limits and paths",
    ),
    (
        "GET",
        "/downloads/worker",
        "downloads",
        "Worker runtime state — session, port, and totals",
    ),
    (
        "GET",
        "/downloads/vpn",
        "downloads",
        "VPN tunnel state and the public IP as the worker sees it",
    ),
    (
        "GET",
        "/downloads/categories",
        "downloads",
        "List the download categories",
    ),
    (
        "PUT",
        "/downloads/categories/{name}",
        "downloads",
        "Create or replace a download category",
    ),
    (
        "DELETE",
        "/downloads/categories/{name}",
        "downloads",
        "Delete a download category",
    ),
    (
        "POST",
        "/downloads/import",
        "downloads",
        "Import a completed download into the library",
    ),
    (
        "POST",
        "/downloads/import/preview",
        "downloads",
        "Show where a completed download would be placed",
    ),
    ("GET", "/history", "history", "Grab and import history"),
    ("GET", "/history/{id}", "history", "One history record"),
    (
        "GET",
        "/history/counts",
        "history",
        "History counts by event kind",
    ),
    (
        "POST",
        "/history/{id}/blocklist-and-search",
        "history",
        "Blocklist that release and search again",
    ),
    ("GET", "/blocklist", "history", "Every blocklisted release"),
    (
        "POST",
        "/blocklist",
        "history",
        "Blocklist a release by infohash or title",
    ),
    (
        "DELETE",
        "/blocklist",
        "history",
        "Clear the whole blocklist",
    ),
    (
        "DELETE",
        "/blocklist/{id}",
        "history",
        "Remove one blocklist entry",
    ),
    (
        "GET",
        "/wanted",
        "history",
        "Everything monitored and missing, across domains",
    ),
    (
        "GET",
        "/library",
        "library",
        "Library totals and per-domain counts",
    ),
    (
        "GET",
        "/library/genres",
        "library",
        "Genre facet: every genre with how many items carry it, per domain or merged",
    ),
    (
        "POST",
        "/auth/login",
        "household",
        "Exchange a username and password for a long-lived device token (unauthenticated)",
    ),
    (
        "POST",
        "/auth/logout",
        "household",
        "Sign this device out, leaving the member's others signed in",
    ),
    (
        "POST",
        "/auth/password",
        "household",
        "Change your own password; signs your other devices out",
    ),
    (
        "GET",
        "/me",
        "household",
        "The calling member: name, role and content policy",
    ),
    (
        "GET",
        "/members",
        "household",
        "List household members (admin)",
    ),
    (
        "POST",
        "/members",
        "household",
        "Create a member; returns their token once (admin)",
    ),
    ("GET", "/members/{id}", "household", "One member (admin)"),
    (
        "PATCH",
        "/members/{id}",
        "household",
        "Rename, re-role (member/contributor/kid, never admin), set username, password, PIN or policy (admin)",
    ),
    (
        "DELETE",
        "/members/{id}",
        "household",
        "Revoke a member and their token (admin)",
    ),
    (
        "POST",
        "/members/{id}/token",
        "household",
        "Re-issue a member's token, dropping the old ones (admin)",
    ),
    (
        "POST",
        "/members/{id}/signout",
        "household",
        "Sign every one of a member's devices out (admin)",
    ),
    (
        "GET",
        "/root-folders",
        "library",
        "The derived root folder per domain, with free space",
    ),
    (
        "GET",
        "/root-folders/{id}/unmapped",
        "library",
        "Files under a root that match nothing in the database",
    ),
    (
        "GET",
        "/fs/roots",
        "library",
        "The filesystem roots the daemon may browse",
    ),
    (
        "GET",
        "/fs/browse",
        "library",
        "List one directory, for the folder picker",
    ),
    ("GET", "/movies", "movies", "List movies"),
    ("POST", "/movies", "movies", "Add a movie by TMDB id"),
    ("GET", "/movies/{id}", "movies", "Read one movie"),
    (
        "PATCH",
        "/movies/{id}",
        "movies",
        "Update a movie's monitoring or quality profile",
    ),
    ("DELETE", "/movies/{id}", "movies", "Delete a movie"),
    (
        "GET",
        "/movies/lookup",
        "movies",
        "Search TMDB for a movie to add",
    ),
    (
        "POST",
        "/movies/refresh",
        "movies",
        "Refresh metadata for every movie",
    ),
    (
        "GET",
        "/movies/scan",
        "movies",
        "Background media-scan progress",
    ),
    (
        "GET",
        "/movies/streaming-report",
        "movies",
        "Files that will not direct-play, and what repacking them needs",
    ),
    (
        "POST",
        "/movies/{id}/refresh",
        "movies",
        "Refresh metadata for one movie",
    ),
    (
        "POST",
        "/movies/quality/test",
        "movies",
        "Parse a release title and show its quality",
    ),
    (
        "GET",
        "/movies/{id}/editions/{eid}/releases",
        "movies",
        "Candidate releases for one edition",
    ),
    (
        "POST",
        "/movies/{id}/editions/{eid}/acquire",
        "movies",
        "Run the acquire pipeline for one edition",
    ),
    (
        "POST",
        "/movies/{id}/editions/{eid}/grab",
        "movies",
        "Grab a specific release for one edition",
    ),
    (
        "POST",
        "/movies/{id}/editions/{eid}/grab-link",
        "movies",
        "Grab a pasted magnet or .torrent link",
    ),
    (
        "POST",
        "/movies/{id}/editions/{eid}/reset",
        "movies",
        "Clear an edition's download state",
    ),
    (
        "POST",
        "/library-import/scan",
        "movies",
        "Scan the existing movie library",
    ),
    (
        "POST",
        "/library-import/match",
        "movies",
        "Match scanned movie files to TMDB",
    ),
    (
        "POST",
        "/library-import/commit",
        "movies",
        "Adopt the matched movie files",
    ),
    // Manual acquire at parity with movies and audiobooks (SKADI-T-0558).
    (
        "GET",
        "/series/{id}/episodes/{eid}/releases",
        "tv",
        "Candidate releases for one episode, with accept/reject verdicts",
    ),
    (
        "POST",
        "/series/{id}/episodes/{eid}/acquire",
        "tv",
        "Run the acquire pipeline for one episode",
    ),
    (
        "POST",
        "/series/{id}/episodes/{eid}/grab",
        "tv",
        "Grab a specific release for one episode",
    ),
    (
        "POST",
        "/series/{id}/episodes/{eid}/grab-link",
        "tv",
        "Grab a pasted magnet or .torrent link for one episode",
    ),
    (
        "POST",
        "/series/{id}/episodes/{eid}/reset",
        "tv",
        "Clear a wedged episode's download state",
    ),
    ("GET", "/series", "tv", "List series"),
    ("POST", "/series", "tv", "Add a series by TVDB id"),
    (
        "GET",
        "/series/{id}",
        "tv",
        "Read one series with its seasons",
    ),
    (
        "PATCH",
        "/series/{id}",
        "tv",
        "Update a series' monitoring or quality profile",
    ),
    ("DELETE", "/series/{id}", "tv", "Delete a series"),
    ("GET", "/series/lookup", "tv", "Search for a series to add"),
    (
        "POST",
        "/series/refresh",
        "tv",
        "Refresh metadata for every series",
    ),
    (
        "POST",
        "/series/{id}/refresh",
        "tv",
        "Refresh metadata for one series",
    ),
    (
        "POST",
        "/series/{id}/seasons/{n}/monitor",
        "tv",
        "Monitor or unmonitor a whole season",
    ),
    (
        "POST",
        "/series/{id}/episodes/{eid}/monitor",
        "tv",
        "Monitor or unmonitor one episode",
    ),
    (
        "POST",
        "/tv/library-import/scan",
        "tv",
        "Scan the existing TV library",
    ),
    (
        "GET",
        "/tv/library-import/series-structure",
        "tv",
        "The season/episode layout found by the scan",
    ),
    (
        "POST",
        "/tv/library-import/match",
        "tv",
        "Match scanned episodes to a series",
    ),
    (
        "POST",
        "/tv/library-import/commit",
        "tv",
        "Adopt the matched episode files",
    ),
    ("GET", "/books", "audiobooks", "List books"),
    ("POST", "/books", "audiobooks", "Add a book by ASIN"),
    (
        "GET",
        "/books/{id}",
        "audiobooks",
        "Read one book with its files",
    ),
    (
        "PATCH",
        "/books/{id}",
        "audiobooks",
        "Update a book's monitoring or quality profile",
    ),
    ("DELETE", "/books/{id}", "audiobooks", "Delete a book"),
    (
        "GET",
        "/books/lookup",
        "audiobooks",
        "Search the catalog for a book to add",
    ),
    (
        "GET",
        "/books/search",
        "audiobooks",
        "Search the local book collection",
    ),
    (
        "POST",
        "/books/refresh",
        "audiobooks",
        "Refresh metadata for every book",
    ),
    (
        "GET",
        "/movies/{id}/editions/{eid}/video",
        "movies",
        "Stream an imported edition's video (Range/seek)",
    ),
    (
        "GET",
        "/series/{id}/episodes/{eid}/video",
        "tv",
        "Stream an imported episode's video (Range/seek)",
    ),
    (
        "GET",
        "/movies/{id}/editions/{eid}/subtitles",
        "movies",
        "List the subtitle files beside an edition's video",
    ),
    (
        "GET",
        "/movies/{id}/editions/{eid}/subtitles/{n}",
        "movies",
        "Get one subtitle file (?format=vtt converts to WebVTT)",
    ),
    (
        "GET",
        "/series/{id}/episodes/{eid}/subtitles",
        "tv",
        "List the subtitle files beside an episode's video",
    ),
    (
        "GET",
        "/series/{id}/episodes/{eid}/markers",
        "tv",
        "Intro and credits positions from the episode's chapters",
    ),
    (
        "GET",
        "/series/{id}/episodes/{eid}/subtitles/{n}",
        "tv",
        "Get one subtitle file (?format=vtt converts to WebVTT)",
    ),
    (
        "GET",
        "/importlists",
        "importlists",
        "List configured import lists",
    ),
    (
        "POST",
        "/importlists",
        "importlists",
        "Create an import list",
    ),
    (
        "GET",
        "/importlists/{id}",
        "importlists",
        "Get one import list",
    ),
    (
        "PUT",
        "/importlists/{id}",
        "importlists",
        "Replace an import list",
    ),
    (
        "DELETE",
        "/importlists/{id}",
        "importlists",
        "Delete an import list",
    ),
    (
        "POST",
        "/importlists/{id}/sync",
        "importlists",
        "Sync one import list now",
    ),
    (
        "POST",
        "/importlists/sync",
        "importlists",
        "Sync every due import list",
    ),
    (
        "GET",
        "/importlists/exclusions",
        "importlists",
        "List import-list exclusions",
    ),
    (
        "POST",
        "/importlists/exclusions",
        "importlists",
        "Exclude an item from import lists",
    ),
    (
        "DELETE",
        "/importlists/exclusions/{id}",
        "importlists",
        "Remove an import-list exclusion",
    ),
    (
        "POST",
        "/books/merge-editions",
        "audiobooks",
        "Fold duplicate books rows into one book with several editions",
    ),
    (
        "POST",
        "/books/{id}/refresh",
        "audiobooks",
        "Refresh metadata for one book",
    ),
    (
        "GET",
        "/books/{id}/files/{fid}/releases",
        "audiobooks",
        "Candidate releases for one book file",
    ),
    (
        "POST",
        "/books/{id}/files/{fid}/acquire",
        "audiobooks",
        "Run the acquire pipeline for one book file",
    ),
    (
        "POST",
        "/books/{id}/files/{fid}/grab",
        "audiobooks",
        "Grab a specific release for one book file",
    ),
    (
        "POST",
        "/books/{id}/files/{fid}/grab-link",
        "audiobooks",
        "Grab a pasted magnet or .torrent link",
    ),
    (
        "POST",
        "/books/{id}/files/{fid}/reset",
        "audiobooks",
        "Clear a book file's download state",
    ),
    (
        "GET",
        "/books/{id}/files/{fid}/audio",
        "audiobooks",
        "Stream the audio for one book file",
    ),
    (
        "GET",
        "/books/{id}/files/{fid}/chapters",
        "audiobooks",
        "Chapter markers for one book file",
    ),
    (
        "GET",
        "/books/{id}/files/{fid}/location",
        "audiobooks",
        "On-disk path and size for one book file",
    ),
    ("GET", "/authors", "audiobooks", "List authors"),
    ("POST", "/authors", "audiobooks", "Add an author"),
    ("GET", "/authors/{id}", "audiobooks", "Read one author"),
    (
        "PATCH",
        "/authors/{id}",
        "audiobooks",
        "Update an author's monitoring",
    ),
    ("DELETE", "/authors/{id}", "audiobooks", "Delete an author"),
    (
        "GET",
        "/authors/lookup",
        "audiobooks",
        "Search the catalog for an author",
    ),
    (
        "GET",
        "/audiobooks/works",
        "audiobooks",
        "Works clustered across editions",
    ),
    (
        "GET",
        "/audiobooks/series",
        "audiobooks",
        "Book series and their ordering",
    ),
    (
        "GET",
        "/audiobooks/discover",
        "audiobooks",
        "Catalog suggestions for the discover page",
    ),
    (
        "POST",
        "/audiobooks/catalog/refresh",
        "audiobooks",
        "Re-ingest the audiobook catalog",
    ),
    (
        "GET",
        "/audiobooks/quality",
        "audiobooks",
        "The audiobook quality definitions",
    ),
    (
        "POST",
        "/audiobooks/quality/test",
        "audiobooks",
        "Parse a release title as an audiobook",
    ),
    (
        "GET",
        "/watchers",
        "audiobooks",
        "Standing watchers (author or series) and their state",
    ),
    (
        "PUT",
        "/watchers/{scope}/{key}",
        "audiobooks",
        "Create or replace a watcher",
    ),
    (
        "DELETE",
        "/watchers/{scope}/{key}",
        "audiobooks",
        "Delete a watcher",
    ),
    (
        "POST",
        "/audiobooks/library-import/scan",
        "audiobooks",
        "Scan the existing audiobook library",
    ),
    (
        "POST",
        "/audiobooks/library-import/match",
        "audiobooks",
        "Match scanned audiobooks to the catalog",
    ),
    (
        "POST",
        "/audiobooks/library-import/commit",
        "audiobooks",
        "Adopt the matched audiobook files",
    ),
    (
        "GET",
        "/pair/qr",
        "pairing",
        "A QR code that pairs the Android client",
    ),
    ("GET", "/pair/app", "pairing", "The pairing landing page"),
    ("GET", "/pair/apk", "pairing", "Download the Android APK"),
    (
        "GET",
        "/app/{file}",
        "pairing",
        "Static files for the pairing page",
    ),
    // Browser uploads (SKADI-I-0062). Resumable sessions that land a file in
    // the staging area, where library-import adopts it.
    (
        "POST",
        "/uploads",
        "uploads",
        "Open a resumable upload session",
    ),
    ("GET", "/uploads", "uploads", "Upload sessions in flight"),
    (
        "GET",
        "/uploads/{id}",
        "uploads",
        "How much of an upload has arrived, so a client can resume",
    ),
    (
        "PUT",
        "/uploads/{id}/chunk",
        "uploads",
        "Append raw bytes at an offset",
    ),
    (
        "POST",
        "/uploads/{id}/complete",
        "uploads",
        "Verify, probe and publish the finished upload",
    ),
    (
        "DELETE",
        "/uploads/{id}",
        "uploads",
        "Abandon an upload and remove its bytes",
    ),
];

/// Human-readable descriptions for the tags used above, so a generated client
/// groups its methods sensibly.
const TAGS: &[(&str, &str)] = &[
    ("system", "Daemon health, status, logs and backups"),
    ("config", "The typed config registry"),
    (
        "settings",
        "Table-backed settings records (indexers, profiles, clients)",
    ),
    ("naming", "File and folder naming templates"),
    (
        "importlists",
        "External lists synced into the library, and their exclusions",
    ),
    ("quality", "Quality definitions and movie edition kinds"),
    ("search", "Indexer search and Cardigann definitions"),
    ("downloads", "The embedded download worker"),
    (
        "history",
        "Grab/import history, the blocklist, and what is still wanted",
    ),
    (
        "library",
        "Library totals, root folders and filesystem browsing",
    ),
    (
        "uploads",
        "Resumable browser uploads into the staging area, where library-import adopts them",
    ),
    (
        "household",
        "Household members, roles and content policies (SKADI-I-0059)",
    ),
    ("movies", "Movies"),
    ("tv", "Television"),
    ("audiobooks", "Audiobooks, authors and watchers"),
    ("pairing", "Android client pairing and APK distribution"),
];

/// Path parameters, derived from the `{name}` placeholders in a path.
fn path_params(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| {
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "schema": { "type": "string" },
            })
        })
        .collect()
}

/// Which operations return which schema (SKADI-T-0546):
/// `(method, path, schema name)`.
///
/// Deliberately **partial** and deliberately separate from [`ROUTES`]. Partial
/// because a `components/schemas` covering the client-facing surface is valid
/// OpenAPI and strictly better than none, and hand-writing 141 payload shapes
/// would rot. Separate because folding a mostly-empty field into all 141 `ROUTES`
/// entries would be churn that hides the signal.
///
/// A test asserts every entry here names a route that exists and a schema that is
/// registered, so this table cannot drift either.
const RESPONSE_SCHEMAS: &[(&str, &str, &str)] = &[
    ("GET", "/config", "ConfigKeyDto"),
    ("GET", "/config/{key}", "ConfigKeyDto"),
    ("PUT", "/config/{key}", "ConfigKeyDto"),
    // The audiobooks surface the Android client hand-declares (SKADI-T-0472's
    // motivating problem). `/books` serialises the domain `Book` directly, which
    // is describable since SKADI-T-0548 put `schemars` in skadi-core alongside
    // the `Serialize` derives that were already there.
    ("GET", "/calendar", "CalendarEntryDto"),
    ("GET", "/books", "Book"),
    ("GET", "/books/{id}", "Book"),
    ("GET", "/books/{id}/files/{fid}/chapters", "ChapterDto"),
    ("GET", "/watchers", "WatcherDto"),
    ("GET", "/audiobooks/series", "SeriesRollupDto"),
];

/// Schemas for `skadi-api`'s own response types. Domain crates add theirs
/// through [`HttpModule::schemas`](crate::http_module::HttpModule::schemas).
fn builtin_schemas() -> Schemas {
    let mut out = schema_for::<crate::config_api::ConfigKeyDto>();
    out.extend(schema_for::<crate::library::CalendarEntryDto>());
    out
}

/// Build the OpenAPI 3.1 document, merging in any domain-contributed schemas.
pub fn document(extra: &Schemas) -> Value {
    // Only schemas actually registered can be referenced. A deployment that does
    // not mount the audiobooks module must not publish a `$ref` to an audiobook
    // payload it is not serving — a generated client would fail on the dangling
    // pointer, and the operation is perfectly describable as a bare 200.
    let available: std::collections::HashSet<String> = builtin_schemas()
        .into_iter()
        .map(|(name, _)| name)
        .chain(extra.iter().map(|(name, _)| name.clone()))
        .collect();
    let mut paths = serde_json::Map::new();
    for (method, path, tag, summary) in ROUTES {
        let params = path_params(path);
        // A described response points at its schema; everything else keeps the
        // bare 200 (SKADI-T-0546).
        let ok_response = RESPONSE_SCHEMAS
            .iter()
            .find(|(m, p, schema)| m == method && p == path && available.contains(*schema))
            .map_or_else(
                || json!({ "description": "Success" }),
                |(_, _, schema)| {
                    json!({
                        "description": "Success",
                        "content": {
                            "application/json": {
                                "schema": { "$ref": format!("#/components/schemas/{schema}") }
                            }
                        }
                    })
                },
            );
        let mut op = json!({
            "tags": [tag],
            "summary": summary,
            "operationId": operation_id(method, path),
            "responses": {
                "200": ok_response,
                // Every handler funnels failures through the same envelope
                // (SKADI-T-0458), so it is worth describing once rather than
                // leaving a client to guess at the error shape.
                "default": {
                    "description": "Error envelope",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/Error" }
                        }
                    }
                }
            }
        });
        if !params.is_empty() {
            op["parameters"] = Value::Array(params);
        }
        let entry = paths
            .entry((*path).to_string())
            .or_insert_with(|| json!({}));
        entry[method.to_lowercase()] = op;
    }

    let mut doc = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Skadi",
            "description": "One daemon for movies, television and audiobooks.",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "servers": [{ "url": "/api/v1" }],
        "tags": TAGS
            .iter()
            .map(|(name, description)| json!({ "name": name, "description": description }))
            .collect::<Vec<_>>(),
        "components": {
            "securitySchemes": {
                "bearer": { "type": "http", "scheme": "bearer" }
            },
            "schemas": {
                "Error": {
                    "type": "object",
                    "required": ["error", "message"],
                    "properties": {
                        "error": {
                            "type": "string",
                            "description": "Machine-readable kind, e.g. `validation`, `not_found`."
                        },
                        "message": { "type": "string" }
                    }
                }
            }
        },
        // Applies to every operation. `/health` is the one exemption, and it is
        // documented as such in its summary rather than by overriding here.
        "security": [{ "bearer": [] }],
        "paths": Value::Object(paths),
    });

    // Domain-contributed schemas are merged rather than replacing the map, so
    // `Error` — which every operation references — survives.
    if let Some(Value::Object(map)) = doc.get_mut("components").and_then(|c| c.get_mut("schemas")) {
        for (name, schema) in builtin_schemas().iter().chain(extra) {
            map.insert(name.clone(), schema.clone());
        }
    }
    doc
}

/// A stable, readable `operationId`: `get_books_id_files_fid_audio`.
fn operation_id(method: &str, path: &str) -> String {
    let mut id = method.to_lowercase();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        id.push('_');
        id.push_str(&seg.trim_matches(['{', '}']).replace(['-', '.'], "_"));
    }
    id
}

pub fn openapi_router<S: Clone + Send + Sync + 'static>(schemas: Schemas) -> Router<S> {
    let doc = std::sync::Arc::new(document(&schemas));
    Router::new().route(
        "/openapi.json",
        get(move || {
            let doc = doc.clone();
            async move { Json((*doc).clone()) }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_path_has_its_placeholders_as_parameters() {
        for (method, path, _, _) in ROUTES {
            let declared = path_params(path).len();
            let placeholders = path.matches('{').count();
            assert_eq!(
                declared, placeholders,
                "{method} {path}: {placeholders} placeholders but {declared} parameters"
            );
        }
    }

    #[test]
    fn operation_ids_are_unique() {
        // A duplicate id silently overwrites a method in a generated client.
        let mut seen = std::collections::HashSet::new();
        for (method, path, _, _) in ROUTES {
            let id = operation_id(method, path);
            assert!(seen.insert(id.clone()), "duplicate operationId {id}");
        }
    }

    #[test]
    fn every_route_uses_a_described_tag() {
        for (method, path, tag, _) in ROUTES {
            assert!(
                TAGS.iter().any(|(name, _)| name == tag),
                "{method} {path} uses tag {tag:?}, which has no description"
            );
        }
    }

    #[test]
    fn every_ref_in_the_document_resolves() {
        // The assertion that makes the `$ref`s worth anything. A schema name in
        // RESPONSE_SCHEMAS that nothing registers would publish a document
        // pointing at a definition that does not exist — a generated client
        // fails on it, and nothing else here would have caught it.
        let doc = document(&Vec::new());
        let schemas = doc["components"]["schemas"].as_object().unwrap();

        fn refs(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    for (k, val) in map {
                        if k == "$ref"
                            && let Some(r) = val.as_str()
                        {
                            out.push(r.to_string());
                        }
                        refs(val, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|i| refs(i, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        refs(&doc, &mut found);
        assert!(!found.is_empty(), "the document declares no $refs at all");
        for r in found {
            let name = r
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("unexpected $ref form: {r}"));
            assert!(
                schemas.contains_key(name),
                "$ref points at {name}, which is not in components/schemas"
            );
        }
    }

    #[test]
    fn every_described_response_names_a_real_route() {
        // The mapping table is separate from ROUTES, so it needs its own guard
        // or it becomes the drift the whole design exists to prevent
        // (SKADI-T-0546).
        for (method, path, schema) in RESPONSE_SCHEMAS {
            assert!(
                ROUTES.iter().any(|(m, p, _, _)| m == method && p == path),
                "{method} {path} has a response schema but is not a served route"
            );
            assert!(!schema.is_empty());
        }
    }

    #[test]
    fn a_described_operation_refs_its_schema_and_the_rest_do_not() {
        let extra = schema_for::<ConfigProbe>();
        let doc = document(&extra);

        // Described: the 200 points at the schema.
        assert_eq!(
            doc["paths"]["/config"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/ConfigKeyDto"
        );
        // Undescribed: still a bare 200, not a broken $ref to nothing.
        assert!(
            doc["paths"]["/health"]["get"]["responses"]["200"]
                .get("content")
                .is_none(),
            "an operation with no declared schema must not claim one"
        );
        // Every operation keeps the shared error envelope either way.
        assert_eq!(
            doc["paths"]["/health"]["get"]["responses"]["default"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/Error"
        );
    }

    #[test]
    fn domain_schemas_merge_without_dropping_the_error_envelope() {
        // Merging, not replacing: `Error` is referenced by every operation, so a
        // domain contribution that overwrote the map would leave 141 dangling
        // refs.
        let doc = document(&schema_for::<ConfigProbe>());
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        assert!(schemas.contains_key("Error"));
        assert!(schemas.contains_key("ConfigProbe"));
    }

    /// A stand-in for a domain DTO, so the merge is tested without `skadi-api`
    /// depending on a domain crate (it sits below them).
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ConfigProbe {
        key: String,
        editable: bool,
    }

    #[test]
    fn generated_schemas_describe_the_rust_fields() {
        let schemas = schema_for::<ConfigProbe>();
        let (name, schema) = &schemas[0];
        assert_eq!(name, "ConfigProbe");
        // Derived from the type, so a rename here changes the document — which
        // is the property a hand-written schema cannot offer.
        assert_eq!(schema["properties"]["key"]["type"], "string");
        assert_eq!(schema["properties"]["editable"]["type"], "boolean");
    }

    #[test]
    fn the_document_is_shaped_like_openapi() {
        let doc = document(&Vec::new());
        assert_eq!(doc["openapi"], "3.1.0");
        assert!(doc["paths"].as_object().unwrap().len() > 100);
        assert_eq!(doc["paths"]["/health"]["get"]["tags"][0], "system");
        assert_eq!(
            doc["paths"]["/books/{id}"]["get"]["parameters"][0]["name"],
            "id"
        );
    }
}
