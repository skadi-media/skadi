//! `skadi-config` — the schema for Skadi's process configuration (SKADI-I-0014).
//!
//! This crate is the **single source of truth** for *what* configuration keys
//! exist, their defaults, their value type, and which tier they belong to. It
//! is pure (no DB, no I/O beyond `std::env`) so both the daemon and the
//! excluded `skadi-downloader-worker` can depend on it.
//!
//! ## The model (operator-specified)
//!
//! `SKADI_*` environment variables are read at launch and written into a
//! `config` table, always overwriting. From then on the table is the source of
//! truth and services read from it (the table/repo + read accessor are later
//! tasks). This crate only defines the **schema** and the **env↔key mapping**;
//! it does not touch the database.
//!
//! ## Two tiers (the bootstrap paradox)
//!
//! - [`Tier::Tier0`] — values needed *to reach* the table, so they can't live
//!   in it: `database_url` (the table is in that DB) and `secret_key`. Read
//!   directly from env by their consumers.
//! - [`Tier::Tier1`] — everything else; seeded from env into the `config` table
//!   on boot. [`read_env`] collects exactly these.
//!
//! ## Naming convention
//!
//! A dotted config `key` maps to an env var by upper-snake-casing it under the
//! `SKADI_` prefix: `worker.download_dir` ⇔ `SKADI_WORKER_DOWNLOAD_DIR`,
//! `bind_addr` ⇔ `SKADI_BIND_ADDR`. The registry is authoritative — the reverse
//! direction ([`key_from_env`]) matches against forward-mapped names rather
//! than guessing where dots vs underscores go.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

mod env_file;
pub use env_file::{
    DATABASE_PASSWORD_ENV, DATABASE_URL_ENV, EnvError, FILE_SUFFIX, database_url,
    database_url_with, env_or_file, env_or_file_with, url_password,
};

/// Prefix every Skadi env var shares.
pub const ENV_PREFIX: &str = "SKADI_";

/// How a config value is parsed by a typed reader (the typed [`ConfigView`]
/// accessor lands in a later task; here it is schema metadata only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    String,
    Bool,
    U16,
    U64,
    Path,
}

/// Default `worker.extra_trackers` (SKADI-T-0597): open trackers that have been
/// up for years. Announcing to them costs one UDP packet per torrent per
/// interval and is what most clients do by default.
pub const DEFAULT_EXTRA_TRACKERS: &str = "udp://tracker.opentrackr.org:1337/announce, \
udp://open.stealth.si:80/announce, \
udp://tracker.torrent.eu.org:451/announce, \
udp://exodus.desync.com:6969/announce, \
udp://open.demonii.com:1337/announce, \
udp://tracker.openbittorrent.com:6969/announce, \
udp://explodie.org:6969/announce, \
http://tracker.opentrackr.org:1337/announce";

/// Which configuration tier a key belongs to (see the crate docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Pre-table: read directly from env, never routed through the `config`
    /// table (it is needed to reach the table).
    Tier0,
    /// Table-backed: seeded from env into the `config` table on boot.
    Tier1,
}

/// One entry in the configuration schema.
#[derive(Clone, Copy, Debug)]
pub struct ConfigKeySpec {
    /// Dotted key, e.g. `worker.download_dir`.
    pub key: &'static str,
    /// How the value is typed.
    pub kind: ValueKind,
    /// Default applied when neither env nor the table provides a value.
    pub default: &'static str,
    /// Tier (pre-table vs table-backed).
    pub tier: Tier,
}

/// The complete configuration schema. Defaults preserve today's behavior so
/// migrating readers onto this is a no-op for an unchanged environment.
pub const REGISTRY: &[ConfigKeySpec] = &[
    // --- Tier 0: pre-table (read directly) ---
    ConfigKeySpec {
        key: "database_url",
        kind: ValueKind::String,
        // Matches skadi_store::DEFAULT_DATABASE_URL.
        default: "sqlite://./skadi.db",
        tier: Tier::Tier0,
    },
    ConfigKeySpec {
        key: "secret_key",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier0,
    },
    // --- Tier 1: table-backed (seeded from env on boot) ---
    ConfigKeySpec {
        key: "bind_addr",
        kind: ValueKind::String,
        default: "127.0.0.1:8080",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "api_token",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "tmdb_api_key",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "mode",
        kind: ValueKind::String,
        default: "production",
        tier: Tier::Tier1,
    },
    // Directory holding the synced + bundled Cardigann tracker definitions
    // (SKADI-I-0036). Seeded with the bundled set on an empty first run.
    ConfigKeySpec {
        key: "cardigann_definitions_dir",
        kind: ValueKind::String,
        default: "./definitions",
        tier: Tier::Tier1,
    },
    // FlareSolverr endpoint for solving CloudFlare/DDoS-Guard challenges on
    // native cardigann trackers (SKADI-I-0036). Empty ⇒ disabled (direct only).
    ConfigKeySpec {
        key: "flaresolverr_url",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    // How many CloudFlare solves may be in flight at once (SKADI-T-0488).
    // FlareSolverr drives a single Chromium, so parallel challenges contend for
    // it and each gets slower — a burst of twelve (2026-09-06) pushed them all
    // past the healthcheck timeout and autoheal killed the browser mid-solve.
    // Small on purpose; raise only with a solver that can actually parallelise.
    ConfigKeySpec {
        key: "flaresolverr_max_concurrent",
        kind: ValueKind::U16,
        default: "2",
        tier: Tier::Tier1,
    },
    // HTTP proxy for native cardigann tracker requests (SKADI-I-0036). In the
    // deploy stack this points at gluetun's HTTP proxy so indexer searches egress
    // via the VPN — the same exit IP FlareSolverr solves from, keeping
    // `cf_clearance` valid. Empty ⇒ direct (no proxy).
    ConfigKeySpec {
        key: "cardigann_proxy_url",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    // Scheduled upstream sync of the cardigann definition library (SKADI-T-0263),
    // mirroring Prowlarr's daily definition refresh. `enabled` toggles the
    // background worker; `interval_secs` is the sync cadence (default daily);
    // `revision` pins the Prowlarr/Indexers git ref to pull (`master` tracks tip).
    ConfigKeySpec {
        key: "cardigann_sync_enabled",
        kind: ValueKind::Bool,
        default: "true",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "cardigann_sync_interval_secs",
        kind: ValueKind::U64,
        default: "86400",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "cardigann_definitions_revision",
        kind: ValueKind::String,
        default: "master",
        tier: Tier::Tier1,
    },
    // The curated public trackers (SKADI-T-0703): when true (the default), each
    // boot and each definition sync adds every curated public tracker that is
    // missing and prunes auto-seeded ones that left the scope. false opts out:
    // nothing is added and nothing is pruned.
    ConfigKeySpec {
        key: "default_indexers",
        kind: ValueKind::Bool,
        default: "true",
        tier: Tier::Tier1,
    },
    // Acquisition policy. `min_seeders`: the fewest seeders a *torrent* release
    // may report and still be eligible in `decide` (SKADI-T-0113). `0` disables
    // the filter; usenet/NZB releases carry no seeders and are never filtered.
    ConfigKeySpec {
        key: "min_seeders",
        kind: ValueKind::U16,
        default: "1",
        tier: Tier::Tier1,
    },
    // Reachability over quality (SKADI-T-0598). Without inbound peer
    // connections a grab only completes if one of its seeders accepts
    // connections, so among candidates at or above `seeder_floor_height`
    // `decide` ranks by seeder band before quality. Off ⇒ quality-first.
    ConfigKeySpec {
        key: "hunter.prefer_seeders",
        kind: ValueKind::Bool,
        default: "true",
        tier: Tier::Tier1,
    },
    // Lowest resolution (by height) that counts as "good enough" for the
    // seeders-first ranking; anything below it is a last resort.
    ConfigKeySpec {
        key: "hunter.seeder_floor_height",
        kind: ValueKind::U16,
        default: "720",
        tier: Tier::Tier1,
    },
    // `sweep_max_concurrent`: max acquire runs the hunter sweep launches at once
    // per tick (SKADI-T-0040), so a large library doesn't flood indexers/trackers.
    ConfigKeySpec {
        key: "sweep_max_concurrent",
        kind: ValueKind::U16,
        default: "8",
        tier: Tier::Tier1,
    },
    // Metadata refresh (SKADI-T-0041): how often the refresh worker scans
    // (`metadata_refresh_interval_secs`), how stale a movie may get before it's
    // refreshed (`metadata_staleness_secs`), and how many consecutive provider
    // failures trip the circuit breaker (`metadata_breaker_threshold`).
    ConfigKeySpec {
        key: "metadata_refresh_interval_secs",
        kind: ValueKind::U64,
        default: "21600",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "metadata_staleness_secs",
        kind: ValueKind::U64,
        default: "604800",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "metadata_breaker_threshold",
        kind: ValueKind::U16,
        default: "5",
        tier: Tier::Tier1,
    },
    // When the hunter gives up on a transfer (SKADI-T-0388, made settings in
    // SKADI-T-0544). `monitor.stall_timeout_secs` is how long best progress may
    // sit flat while the client reports the transfer LIVE;
    // `monitor.max_lifetime_secs` is the hard ceiling regardless of progress.
    //
    // The lifetime is **clamped** below Cloacina's retry budget for the monitor
    // task (MONITOR_RETRY_ATTEMPTS x MONITOR_RETRY_DELAY_SECS = 20 h). Past that
    // the retry budget would run out first and the run would end as a bare task
    // failure instead of the cap's clean terminal path (blocklist +
    // `Failed{retry_at}`), so a larger value is silently reduced rather than
    // honoured — the two are coupled, and only one of them is a setting.
    ConfigKeySpec {
        key: "monitor.stall_timeout_secs",
        kind: ValueKind::U64,
        default: "10800",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "monitor.max_lifetime_secs",
        kind: ValueKind::U64,
        default: "64800",
        tier: Tier::Tier1,
    },
    // How often the hunter sweeps for wanted/upgradable items, and how often the
    // RSS fast pass runs (SKADI-T-0544). Both are re-read live: a change rebuilds
    // the ticker from the *next* scheduled fire rather than firing at once.
    //
    // `0` on the sweep means "leave it alone" — it is floored to the constructor
    // value rather than honoured, because an interval of zero would spin the
    // sweep continuously, which is never what an operator means. `0` on RSS
    // means **off**, which is a real thing to want (the fast pass costs a round
    // of indexer queries) and matches `rss_interval: None`.
    ConfigKeySpec {
        key: "sweep_interval_secs",
        kind: ValueKind::U64,
        default: "900",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "rss_interval_secs",
        kind: ValueKind::U64,
        default: "60",
        tier: Tier::Tier1,
    },
    // Indexer search circuit breaker (SKADI-T-0308): after `indexer_breaker_threshold`
    // consecutive failures an indexer is SKIPPED in the search fan-out (so one dead,
    // CloudFlare-banned tracker can't serialize behind FlareSolverr and drag every
    // search to minutes); after `indexer_breaker_cooldown_secs` a single half-open
    // re-probe lets a recovered indexer rejoin. `0` threshold disables the breaker.
    ConfigKeySpec {
        key: "indexer_breaker_threshold",
        kind: ValueKind::U16,
        default: "3",
        tier: Tier::Tier1,
    },
    // The rate arm of the same breaker (SKADI-T-0568). The streak arm only catches
    // an indexer that is *down*; this catches one that is *mostly broken* — in
    // production, ~55 % failure with successes interleaved often enough that the
    // consecutive counter never reached its threshold. Percent of the last 32
    // calls that must fail; `0` disables the rate arm.
    ConfigKeySpec {
        key: "indexer_breaker_rate_pct",
        kind: ValueKind::U16,
        default: "60",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "indexer_breaker_cooldown_secs",
        kind: ValueKind::U64,
        default: "300",
        tier: Tier::Tier1,
    },
    // The single library root (SKADI-T-0302). skadi owns the layout under this one
    // mounted filesystem: each domain writes to `<library.root>/<subfolder>` (movie,
    // television, audiobook) and downloads live at `<library.root>/downloads/...`.
    // Replaces the operator-managed `root_folders` settings list — there is no root
    // picker anywhere. `SKADI_LIBRARY_ROOT`.
    ConfigKeySpec {
        key: "library.root",
        kind: ValueKind::Path,
        default: "/data",
        tier: Tier::Tier1,
    },
    // Refuse to import unless `library.root` carries a `.skadi-root` marker file
    // (SKADI-T-0417). Off by default because the marker is never created for you:
    // creating it automatically would write onto whatever filesystem is mounted at
    // that moment, which — if the mount were already down — is the local disk, and
    // that is precisely the failure this exists to catch. An operator turns it on
    // after running `touch <library.root>/.skadi-root` while the mount is up.
    ConfigKeySpec {
        key: "library.require_root_marker",
        kind: ValueKind::Bool,
        default: "false",
        tier: Tier::Tier1,
    },
    // Sonarr/Radarr "Minimum Free Space" (Media Management), in MB (SKADI-T-0416).
    // An import that would leave the destination filesystem with less than this
    // free is rejected rather than half-filling the library. `0` disables the
    // reserve, leaving only the literal won't-fit check.
    ConfigKeySpec {
        key: "import.min_free_mb",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Reject source files smaller than this many bytes as extras/samples
    // (SKADI-T-0416). `0` (the default) keeps the name-based `looks_like_sample`
    // reject as the only universal filter; domains keep their own thresholds.
    ConfigKeySpec {
        key: "import.min_file_bytes",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Route daemon egress (Torznab, metadata, notifiers, definition sync) through
    // this proxy (SKADI-T-0522). Empty = direct. Set it to gluetun's HTTP proxy
    // to put indexer and metadata traffic on the VPN: cardigann already builds its
    // own proxied client, so without this an operator running a VPN specifically
    // to hide indexer traffic was leaking most of it.
    ConfigKeySpec {
        key: "http.proxy_url",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    // Comma-separated hosts that bypass `http.proxy_url` (SKADI-T-0522), matched
    // as suffixes. Loopback is always bypassed and need not be listed.
    ConfigKeySpec {
        key: "http.no_proxy",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    // How long an *automatic* blocklist entry lasts, in hours (SKADI-T-0529).
    // `0` = permanent. Default 720 (30 days), preserving SKADI-T-0436's constant:
    // long enough that a genuinely dead release is not re-grabbed in any practical
    // sweep window, short enough that a misjudgement heals itself rather than
    // needing a human. Operator-created blocks are unaffected — they carry
    // whatever TTL the operator chose.
    ConfigKeySpec {
        key: "blocklist.auto_ttl_hours",
        kind: ValueKind::U64,
        default: "720",
        tier: Tier::Tier1,
    },
    // Media kinds exempt from *automatic* blocklisting, comma-separated
    // (SKADI-T-0529), e.g. `audiobook`. Empty = auto-block everywhere.
    //
    // Exists because a "failed" grab does not mean the same thing in every
    // domain: for audiobooks it is usually a naming or matching problem rather
    // than a bad release, so blocking the release punishes the wrong thing and
    // hides a file that was fine.
    ConfigKeySpec {
        key: "blocklist.auto_block_exempt_domains",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    // How long acquisition history, trace events and decision history are kept
    // (SKADI-T-0440). Was a 90-day constant for history and *forever* for the
    // other two — every acquire run appends to all three and nothing removed a
    // row, so a long-lived install grew them without bound. Diagnostic data has a
    // shelf life: a trace from six months ago explains nothing about today.
    // `0` disables the purge.
    ConfigKeySpec {
        key: "history.retention_days",
        kind: ValueKind::U64,
        default: "90",
        tier: Tier::Tier1,
    },
    // Sonarr's "search for undated episodes" (SKADI-T-0446). An episode with no
    // air date has not aired, so it is not searched: those rows are the least
    // likely to have a real release (a provider placeholder, a season stub), each
    // costs a round of indexer queries on every sweep, and a "match" for an
    // episode that does not exist yet is by definition wrong. Turn this on for a
    // catalogue whose provider dates episodes badly but whose releases are real.
    ConfigKeySpec {
        key: "tv.search_undated_episodes",
        kind: ValueKind::Bool,
        default: "false",
        tier: Tier::Tier1,
    },
    // How a completed download is placed into the library (SKADI-T-0138):
    // `seed` (default) hardlinks and leaves the source, so the torrent keeps
    // seeding — the ratio-friendly, *arr-standard behaviour, and on a single
    // shared export it costs no extra disk because both names share an inode.
    // `move` removes the source after the link lands, for an operator who does
    // not seed and wants the download directory to stay clean.
    //
    // Applies to the **acquire** path only. Library-import always hardlinks: its
    // source is the operator's existing library, which must never be moved or
    // deleted (SKADI-T-0424).
    ConfigKeySpec {
        key: "import.placement",
        kind: ValueKind::String,
        default: "seed",
        tier: Tier::Tier1,
    },
    // Sonarr/Radarr "Recycling Bin" (SKADI-T-0418). When set, a file superseded by
    // an upgrade is moved here instead of deleted, so a bad grab — a mislabelled
    // 2160p that is really an upscale, a broken remux — is recoverable. Empty (the
    // default) keeps the delete-outright behaviour.
    ConfigKeySpec {
        key: "import.recycle_bin",
        kind: ValueKind::Path,
        default: "",
        tier: Tier::Tier1,
    },
    // How long a recycled file is kept (SKADI-T-0418). `0` (the default) never
    // expires anything: an unbounded bin is a disk-space problem, but deleting an
    // operator's only copy of a superseded file on a default they never chose
    // would be worse.
    ConfigKeySpec {
        key: "import.recycle_retention_days",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Drop a download — data included — when its import placed nothing because
    // no file in it was usable media (SKADI-T-0592): a fake release that is one
    // `.exe` named like the episode, a pack of screencaps. Left alone it seeds
    // junk indefinitely. Off keeps the pre-0592 behaviour (the transfer stays
    // for the operator to inspect). Never fires when a file was quarantined or
    // a placement errored — those downloads hold something worth keeping.
    ConfigKeySpec {
        key: "import.remove_unusable_downloads",
        kind: ValueKind::Bool,
        default: "true",
        tier: Tier::Tier1,
    },
    // Worker (the `SKADI_WORKER_*` surface; defaults match its current
    // `Config::from_env`).
    ConfigKeySpec {
        key: "worker.download_dir",
        kind: ValueKind::Path,
        default: "/data/downloads/complete",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.state_dir",
        kind: ValueKind::Path,
        default: "/data/downloads/.rqbit-session",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.watch_dir",
        kind: ValueKind::Path,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.id",
        kind: ValueKind::String,
        default: "skadi-worker",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.port_lo",
        kind: ValueKind::U16,
        default: "16881",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.port_hi",
        kind: ValueKind::U16,
        default: "16891",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.poll_secs",
        kind: ValueKind::U64,
        default: "3",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.tick_secs",
        kind: ValueKind::U64,
        default: "1",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.migrate",
        kind: ValueKind::Bool,
        default: "false",
        tier: Tier::Tier1,
    },
    // Seed-policy (SKADI-T-0209). `0` = unlimited on either axis; the action when a
    // limit is reached is `stop` (terminal, keep seeding off) or `remove`.
    ConfigKeySpec {
        key: "worker.seed_ratio",
        kind: ValueKind::String, // a float ("1.5"); 0 = unlimited
        default: "0",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.seed_time_mins",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "worker.seed_action",
        kind: ValueKind::String, // "stop" | "remove"
        default: "stop",
        tier: Tier::Tier1,
    },
    // Claim-lease length in seconds (SKADI-T-0214): a tracked download's lease is
    // extended every tick; once it expires (worker died) the row is reclaimed.
    ConfigKeySpec {
        key: "worker.lease_secs",
        kind: ValueKind::U64,
        default: "120",
        tier: Tier::Tier1,
    },
    // Seconds of no progress with no peers before a download is flagged `stalled`
    // (SKADI-T-0213); `0` = never flag (disabled).
    ConfigKeySpec {
        key: "worker.stall_timeout_secs",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Seconds to wait for a magnet's metadata to resolve before failing the add
    // (SKADI-T-0307); `0` = wait indefinitely. Audiobook torrents often have very
    // shallow seeds and take a long time to find peers — auto-failing them is worse
    // than waiting, so the default is no timeout.
    ConfigKeySpec {
        key: "worker.metadata_timeout_secs",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Max concurrent active downloads (SKADI-T-0212); `0` = unlimited.
    ConfigKeySpec {
        key: "worker.max_active",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Global bandwidth caps in bytes/sec (SKADI-T-0211); `0` = unlimited.
    ConfigKeySpec {
        key: "worker.down_limit_bps",
        kind: ValueKind::U64,
        default: "0",
        tier: Tier::Tier1,
    },
    // Opinionated default: cap upload at ~500 KB/s so the box seeds back politely
    // forever (seed_ratio/time default to unlimited) without saturating the home
    // link. `0` = unlimited; operator-overridable in Downloads → Settings.
    //
    // Was 100 KB/s (SKADI-T-0516). That was low enough to be a *silent ratio
    // limiter* — with download unlimited, a private tracker's ratio target could
    // be unreachable for reasons an operator would never think to look for in a
    // config default. 500 KB/s stays polite on a home link while leaving ratio a
    // matter of seed policy rather than a hidden throughput ceiling.
    ConfigKeySpec {
        key: "worker.up_limit_bps",
        kind: ValueKind::U64,
        default: "500000",
        tier: Tier::Tier1,
    },
    // Extra trackers every torrent is announced to, on top of what its magnet
    // or .torrent carries (SKADI-T-0597). Comma- or whitespace-separated URLs.
    // A grab from a DHT meta-search site is a bare magnet with no tracker at
    // all and lived on the DHT alone; a handful of long-lived open trackers
    // finds it more peers. Applied at worker startup. Empty ⇒ none.
    ConfigKeySpec {
        key: "worker.extra_trackers",
        kind: ValueKind::String,
        default: DEFAULT_EXTRA_TRACKERS,
        tier: Tier::Tier1,
    },
    // Library naming templates (SKADI-T-0227, the Naming/Path Engine). Empty default ⇒
    // the domain's built-in template (which reproduces the current layout); operators
    // override with `{Token}` rename templates. `naming.space` is the whitespace
    // replacement char (`_` keeps the no-spaces layout; ` ` preserves spaces).
    ConfigKeySpec {
        key: "naming.movie_folder",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "naming.movie_file",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "naming.audiobook_folder",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "naming.audiobook_file",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
    ConfigKeySpec {
        key: "naming.space",
        kind: ValueKind::String,
        default: "_",
        tier: Tier::Tier1,
    },
    // Browser uploads (SKADI-T-0629): a comma-separated extension list that
    // REPLACES the built-in per-kind set when non-empty. Empty (the default)
    // means the built-in list, which mirrors what skadi-media-probe can
    // actually read. Config rather than a constant so an operator with one odd
    // file can get it in without waiting for a rebuild.
    ConfigKeySpec {
        key: "uploads.allowed_extensions",
        kind: ValueKind::String,
        default: "",
        tier: Tier::Tier1,
    },
];

/// Errors from schema lookups / parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("unknown config key: {0:?}")]
    UnknownKey(String),
    #[error("invalid mode {0:?} (expected production | just-go | testing)")]
    BadMode(String),
    #[error("config key {key:?} value {value:?} is not a valid {kind}")]
    Parse {
        key: String,
        value: String,
        kind: &'static str,
    },
    /// A write rejected before it reached the table (SKADI-T-0524) — a wrong
    /// type, or a value that contradicts another key.
    #[error("config key {key:?}: {message}")]
    Invalid { key: String, message: String },
}

/// Help text for the keys an operator most often has to look up
/// (SKADI-T-0699), shown under the input on the web Config page. Kept out of
/// [`ConfigKeySpec`] so the registry rows stay one fact each; a key with no
/// entry has no help. Every key here is in [`REGISTRY`] (a test checks it).
pub const HELP: &[(&str, &str)] = &[
    (
        "default_indexers",
        "true (the default) keeps every curated public tracker (English; movies, TV, books, audiobooks; no adult, no anime) registered: each boot and each definition sync adds the missing ones, and a removed one comes back. false stops this: nothing is added or pruned. Set it with SKADI_DEFAULT_INDEXERS.",
    ),
    (
        "mode",
        "production, just-go or testing. testing uses a stub indexer and downloader; do not use it on a real library.",
    ),
    (
        "library.root",
        "The folder that holds the whole library. Each media kind writes to a subfolder of it, and downloads go to <root>/downloads.",
    ),
    (
        "library.require_root_marker",
        "When on, imports stop unless the file .skadi-root is in the library root. Create it with touch while the mount is up, then turn this on.",
    ),
    (
        "min_seeders",
        "The fewest seeders a torrent release can have and still be grabbed. 0 turns the filter off.",
    ),
    (
        "flaresolverr_url",
        "The URL of FlareSolverr, for trackers behind CloudFlare. Empty means no FlareSolverr.",
    ),
    (
        "cardigann_proxy_url",
        "The HTTP proxy for native tracker requests, for example the gluetun proxy, so that searches go through the VPN. Empty means a direct connection.",
    ),
    (
        "http.proxy_url",
        "The HTTP proxy for the other outgoing requests of the daemon (Torznab, metadata, notifiers). Empty means a direct connection.",
    ),
    (
        "http.no_proxy",
        "Hosts that do not use the proxy, separated by commas. A host matches by its end. Loopback never uses the proxy.",
    ),
    (
        "import.placement",
        "seed keeps the download and hard-links the file into the library, so the torrent continues to seed. move removes the download after the import.",
    ),
    (
        "import.recycle_bin",
        "A folder for the files that an upgrade replaces. Empty means that the old file is deleted.",
    ),
    (
        "import.min_free_mb",
        "An import that leaves less than this free space (MB) on the library disk is refused. 0 turns the reserve off.",
    ),
    (
        "history.retention_days",
        "How many days history, trace events and decisions are kept. 0 keeps them for ever.",
    ),
    (
        "blocklist.auto_ttl_hours",
        "How many hours an automatic blocklist entry lasts. 0 makes it permanent.",
    ),
    (
        "sweep_interval_secs",
        "Seconds between two searches for wanted and upgradable items.",
    ),
    (
        "rss_interval_secs",
        "Seconds between two RSS checks of the indexers. 0 turns RSS off.",
    ),
    (
        "monitor.stall_timeout_secs",
        "Seconds that a download can make no progress before the hunter stops it and tries a different release.",
    ),
    (
        "worker.seed_ratio",
        "The ratio at which a torrent stops seeding. 0 means no limit.",
    ),
    (
        "worker.seed_time_mins",
        "The minutes after which a torrent stops seeding. 0 means no limit.",
    ),
    (
        "worker.seed_action",
        "What to do at a seed limit: stop (keep the files, stop seeding) or remove.",
    ),
    (
        "worker.max_active",
        "The maximum number of downloads at the same time. 0 means no limit.",
    ),
    (
        "worker.down_limit_bps",
        "The download speed limit, in bytes per second. 0 means no limit.",
    ),
    (
        "worker.up_limit_bps",
        "The upload speed limit, in bytes per second. 0 means no limit.",
    ),
    (
        "worker.extra_trackers",
        "Tracker URLs that every torrent also announces to, separated by commas or spaces. They help a magnet with no tracker find peers.",
    ),
    (
        "naming.space",
        "The character that replaces a space in file and folder names. Use a space to keep spaces.",
    ),
    (
        "uploads.allowed_extensions",
        "File extensions that a browser upload accepts, separated by commas. Empty means the built-in list.",
    ),
];

/// The help text for `key` ([`HELP`]), if it has one.
#[must_use]
pub fn help(key: &str) -> Option<&'static str> {
    HELP.iter().find(|(k, _)| *k == key).map(|(_, h)| *h)
}

/// The [`ConfigKeySpec`] for `key`, if it exists in the registry.
#[must_use]
pub fn spec(key: &str) -> Option<&'static ConfigKeySpec> {
    REGISTRY.iter().find(|s| s.key == key)
}

/// The default value for `key` (registry default), or an error if unknown.
pub fn default_for(key: &str) -> Result<&'static str, ConfigError> {
    spec(key)
        .map(|s| s.default)
        .ok_or_else(|| ConfigError::UnknownKey(key.to_string()))
}

/// The env var name for a config `key`: upper-snake under `SKADI_`.
/// `worker.download_dir` → `SKADI_WORKER_DOWNLOAD_DIR`.
#[must_use]
pub fn env_name(key: &str) -> String {
    format!("{ENV_PREFIX}{}", key.replace('.', "_").to_uppercase())
}

/// The config key for an env var name, matched against the registry (so a key
/// segment containing `_` like `download_dir` is never mis-split). `None` for
/// names that aren't Skadi config keys.
#[must_use]
pub fn key_from_env(env: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|s| env_name(s.key) == env)
        .map(|s| s.key)
}

/// Collect every set, non-empty **Tier-1** `SKADI_*` env var as `(key, value)`,
/// for the boot seeder to upsert into the `config` table. Tier-0 keys are
/// excluded (their consumers read env directly).
///
/// Each key may come from `SKADI_X_FILE` instead of `SKADI_X` (SKADI-T-0702,
/// see [`env_or_file`]); setting both, or naming an unreadable file, is an
/// error rather than a silently skipped key.
pub fn read_env() -> Result<Vec<(&'static str, String)>, EnvError> {
    read_from(
        |name| std::env::var(name).ok(),
        |p| std::fs::read_to_string(p),
    )
}

/// [`read_env`] over an injected getter and file reader — keeps the
/// registry-walk logic pure and unit-testable without touching the process
/// environment.
fn read_from(
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&std::path::Path) -> std::io::Result<String>,
) -> Result<Vec<(&'static str, String)>, EnvError> {
    let mut out = Vec::new();
    for s in REGISTRY.iter().filter(|s| s.tier == Tier::Tier1) {
        if let Some(v) = env_or_file_with(&env_name(s.key), &get, &read)?
            && !v.is_empty()
        {
            out.push((s.key, v));
        }
    }
    Ok(out)
}

/// The operational preset, stored under the `mode` config key. Drives the
/// boot-time seeding of runtime config (a later task): `Production` seeds
/// nothing, `JustGo` seeds a minimal working baseline, `Testing` seeds a
/// fully self-contained system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Production,
    JustGo,
    Testing,
}

impl FromStr for Mode {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Tolerant: case-insensitive, `-`/`_`/space-insensitive.
        let norm: String = s
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .collect();
        match norm.as_str() {
            "production" | "prod" => Ok(Mode::Production),
            "justgo" => Ok(Mode::JustGo),
            "testing" | "test" => Ok(Mode::Testing),
            _ => Err(ConfigError::BadMode(s.to_string())),
        }
    }
}

/// A read-only, typed view over resolved config values. Holds a snapshot of the
/// `config` table (loaded by the daemon/worker from `ConfigRepo`) and resolves
/// each key as **table value → registry default**, parsed to the requested
/// type. Pure: the snapshot is just `(key, value)` strings, so this crate stays
/// free of any DB dependency. Build a fresh view on the supervisor tick for a
/// **hot** consumer, or once at startup for a **lazy** one.
#[derive(Clone, Debug, Default)]
pub struct ConfigView {
    values: std::collections::HashMap<String, String>,
}

impl ConfigView {
    /// Build a view from a snapshot of `(key, value)` pairs (e.g.
    /// `store.list_config()`); keys not present fall back to registry defaults.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            values: pairs.into_iter().collect(),
        }
    }

    /// Resolved raw string for `key`: the snapshot value, else the registry
    /// default. Unknown (unregistered) key → [`ConfigError::UnknownKey`].
    pub fn raw(&self, key: &str) -> Result<&str, ConfigError> {
        match self.values.get(key) {
            Some(v) => Ok(v.as_str()),
            None => default_for(key),
        }
    }

    /// Resolved string value.
    pub fn get_string(&self, key: &str) -> Result<String, ConfigError> {
        self.raw(key).map(str::to_string)
    }

    /// Resolved string, or `None` when empty (e.g. an unset `api_token`).
    pub fn get_opt_string(&self, key: &str) -> Result<Option<String>, ConfigError> {
        let v = self.raw(key)?;
        Ok((!v.is_empty()).then(|| v.to_string()))
    }

    /// Resolved boolean (`true`/`1`/`yes` vs `false`/`0`/`no`/empty).
    pub fn get_bool(&self, key: &str) -> Result<bool, ConfigError> {
        let v = self.raw(key)?;
        match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            _ => Err(self.parse_err(key, v, "bool")),
        }
    }

    /// Resolved `u16`.
    pub fn get_u16(&self, key: &str) -> Result<u16, ConfigError> {
        let v = self.raw(key)?;
        v.parse().map_err(|_| self.parse_err(key, v, "u16"))
    }

    /// Resolved `u64`.
    pub fn get_u64(&self, key: &str) -> Result<u64, ConfigError> {
        let v = self.raw(key)?;
        v.parse().map_err(|_| self.parse_err(key, v, "u64"))
    }

    /// Resolved filesystem path, or `None` when the value is empty (e.g. an
    /// unset `worker.watch_dir`).
    pub fn get_path(&self, key: &str) -> Result<Option<std::path::PathBuf>, ConfigError> {
        let v = self.raw(key)?;
        Ok((!v.is_empty()).then(|| std::path::PathBuf::from(v)))
    }

    /// The resolved operational [`Mode`].
    pub fn mode(&self) -> Result<Mode, ConfigError> {
        self.raw("mode")?.parse()
    }

    fn parse_err(&self, key: &str, value: &str, kind: &'static str) -> ConfigError {
        ConfigError::Parse {
            key: key.to_string(),
            value: value.to_string(),
            kind,
        }
    }
}

/// The operator-settable guards the importer applies before placing a file
/// (SKADI-T-0417, SKADI-T-0416).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportGuards {
    /// Library roots the importer may write into. Empty ⇒ the root check is off.
    pub library_roots: Vec<std::path::PathBuf>,
    /// Require a `.skadi-root` marker in the owning root.
    pub require_root_marker: bool,
    /// Free bytes to keep on the destination filesystem (Sonarr "Minimum Free
    /// Space", stored as MB).
    pub min_free_bytes: u64,
    /// Reject source files smaller than this as extras/samples.
    pub min_file_bytes: u64,
    /// Retire superseded files here instead of deleting them. `None` ⇒ delete.
    pub recycle_bin: Option<std::path::PathBuf>,
    /// Remove the source after a successful place (SKADI-T-0138). `false` ⇒
    /// hardlink and keep it, so the torrent keeps seeding.
    pub move_on_import: bool,
    /// Drop a download (with its data) whose import placed nothing because no
    /// file was usable media (SKADI-T-0592).
    pub remove_unusable_downloads: bool,
}

/// The daemon's egress proxy settings (SKADI-T-0522): the proxy URL (empty ⇒
/// direct) and the hosts that bypass it.
#[must_use]
pub fn egress_proxy(view: &ConfigView) -> (Option<String>, Vec<String>) {
    let url = view
        .get_string("http.proxy_url")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let no_proxy = view
        .get_string("http.no_proxy")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (url, no_proxy)
}

/// Read the importer's guards from the config plane.
///
/// Every field degrades to "off" when its key is missing or unreadable. That is
/// deliberate: refusing imports because a config key could not be parsed would be
/// a worse failure than the behaviour these guards replace.
#[must_use]
pub fn import_guards(view: &ConfigView) -> ImportGuards {
    let library_roots = view
        .get_path("library.root")
        .ok()
        .flatten()
        .into_iter()
        .collect();
    ImportGuards {
        library_roots,
        require_root_marker: view
            .get_bool("library.require_root_marker")
            .unwrap_or(false),
        // Stored in MB because that is the unit operators think in (and the unit
        // Sonarr's field uses); converted once, here, so nothing downstream has to
        // remember which unit it holds.
        min_free_bytes: view
            .get_u64("import.min_free_mb")
            .unwrap_or(0)
            .saturating_mul(1024 * 1024),
        min_file_bytes: view.get_u64("import.min_file_bytes").unwrap_or(0),
        recycle_bin: view.get_path("import.recycle_bin").ok().flatten(),
        // Anything other than an explicit `move` is `seed`: an unrecognised value
        // must not silently start deleting sources.
        move_on_import: view
            .get_string("import.placement")
            .map(|v| v.trim().eq_ignore_ascii_case("move"))
            .unwrap_or(false),
        remove_unusable_downloads: view
            .get_bool("import.remove_unusable_downloads")
            .unwrap_or(true),
    }
}

/// Validate one config write against the key's declared type (SKADI-T-0524).
///
/// Config values were only ever parsed on the **read** path, so a typo went into
/// the table happily and surfaced later as a startup failure or a silent fallback
/// to the default — far from the operator who typed it, with nothing pointing
/// back at the key. Validating on write puts the error where the mistake is.
///
/// An unregistered key is accepted: the table also carries per-provider and
/// domain-specific keys the registry does not enumerate, and rejecting those
/// would break writes the registry was never meant to police.
pub fn validate_write(key: &str, value: &str) -> Result<(), ConfigError> {
    let Some(spec) = spec(key) else { return Ok(()) };
    let v = value.trim();
    let bad = |want: &str| ConfigError::Invalid {
        key: key.to_string(),
        message: format!("expected {want}, got {value:?}"),
    };
    match spec.kind {
        ValueKind::Bool => {
            if !matches!(v, "true" | "false") {
                return Err(bad("true or false"));
            }
        }
        ValueKind::U16 => {
            v.parse::<u16>().map_err(|_| bad("a number 0-65535"))?;
        }
        ValueKind::U64 => {
            v.parse::<u64>().map_err(|_| bad("a non-negative number"))?;
        }
        // A path or a free string cannot be wrong at this layer: emptiness is
        // meaningful for several keys (an unset `import.recycle_bin` is how the
        // recycle bin stays off), so it is not an error here.
        ValueKind::Path | ValueKind::String => {}
    }
    Ok(())
}

/// Validate constraints that span more than one key (SKADI-T-0524).
///
/// Applied to the config as it *would be* after a write, so an individually-valid
/// value that contradicts another key is caught before it is stored — a port
/// range whose low end is above its high end parses fine key-by-key and leaves
/// the worker unable to bind.
pub fn validate_cross_key(view: &ConfigView) -> Result<(), ConfigError> {
    if let (Ok(lo), Ok(hi)) = (
        view.get_u16("worker.port_lo"),
        view.get_u16("worker.port_hi"),
    ) && lo > hi
    {
        return Err(ConfigError::Invalid {
            key: "worker.port_lo".to_string(),
            message: format!("port range low end {lo} is above its high end {hi}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_help_entry_names_a_registry_key_once() {
        let mut seen = std::collections::HashSet::new();
        for (key, text) in HELP {
            assert!(spec(key).is_some(), "help for unknown key {key}");
            assert!(seen.insert(*key), "help for {key} twice");
            assert!(!text.trim().is_empty(), "empty help for {key}");
        }
        assert_eq!(
            help("library.root"),
            HELP.iter()
                .find(|(k, _)| *k == "library.root")
                .map(|(_, h)| *h)
        );
        assert_eq!(help("worker.id"), None);
    }

    #[test]
    fn registry_keys_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for s in REGISTRY {
            assert!(seen.insert(s.key), "duplicate key {}", s.key);
            assert!(!s.key.is_empty());
            // Defaults are present (may be empty string, but the field exists).
            let _ = s.default;
        }
        // Tier-0 is exactly the pre-table pair.
        let tier0: Vec<_> = REGISTRY
            .iter()
            .filter(|s| s.tier == Tier::Tier0)
            .map(|s| s.key)
            .collect();
        assert_eq!(tier0, vec!["database_url", "secret_key"]);
    }

    #[test]
    fn import_guards_read_the_operator_keys() {
        // MB in, bytes out — the conversion happens once, here, so nothing
        // downstream has to remember which unit it holds (SKADI-T-0416).
        let view = ConfigView::from_pairs([
            ("library.root".to_string(), "/library".to_string()),
            (
                "library.require_root_marker".to_string(),
                "true".to_string(),
            ),
            ("import.min_free_mb".to_string(), "512".to_string()),
            ("import.min_file_bytes".to_string(), "50000000".to_string()),
        ]);
        let g = import_guards(&view);
        assert_eq!(g.library_roots, vec![std::path::PathBuf::from("/library")]);
        assert!(g.require_root_marker);
        assert_eq!(g.min_free_bytes, 512 * 1024 * 1024);
        assert_eq!(g.min_file_bytes, 50_000_000);
    }

    #[test]
    fn import_guards_fall_back_to_registry_defaults() {
        // `library.root` has a registry default, so the root check is *on* even
        // with an empty config plane — which is what we want in production, where
        // the deploy always mounts a root. The two thresholds default to 0 (off):
        // rejecting imports on an unconfigured limit would be a worse failure than
        // the unguarded behaviour they replace.
        let g = import_guards(&ConfigView::from_pairs([]));
        assert_eq!(g.library_roots, vec![std::path::PathBuf::from("/data")]);
        assert!(!g.require_root_marker);
        assert_eq!(g.min_free_bytes, 0);
        assert_eq!(g.min_file_bytes, 0);
    }

    #[test]
    fn the_new_import_keys_are_env_settable() {
        assert_eq!(env_name("import.min_free_mb"), "SKADI_IMPORT_MIN_FREE_MB");
        assert_eq!(
            env_name("library.require_root_marker"),
            "SKADI_LIBRARY_REQUIRE_ROOT_MARKER"
        );
    }

    #[test]
    fn env_name_matches_existing_conventions() {
        assert_eq!(env_name("database_url"), "SKADI_DATABASE_URL");
        assert_eq!(env_name("bind_addr"), "SKADI_BIND_ADDR");
        assert_eq!(env_name("api_token"), "SKADI_API_TOKEN");
        assert_eq!(env_name("secret_key"), "SKADI_SECRET_KEY");
        assert_eq!(env_name("tmdb_api_key"), "SKADI_TMDB_API_KEY");
        assert_eq!(env_name("mode"), "SKADI_MODE");
        // The key-segment underscore (download_dir) survives the round to env.
        assert_eq!(env_name("worker.download_dir"), "SKADI_WORKER_DOWNLOAD_DIR");
        assert_eq!(env_name("worker.poll_secs"), "SKADI_WORKER_POLL_SECS");
        assert_eq!(env_name("worker.id"), "SKADI_WORKER_ID");
    }

    #[test]
    fn env_and_key_round_trip_for_every_registered_key() {
        for s in REGISTRY {
            assert_eq!(
                key_from_env(&env_name(s.key)),
                Some(s.key),
                "round-trip failed for {}",
                s.key
            );
        }
        assert_eq!(key_from_env("SKADI_NOT_A_KEY"), None);
        assert_eq!(key_from_env("PATH"), None);
    }

    #[test]
    fn default_for_known_and_unknown() {
        assert_eq!(default_for("bind_addr").unwrap(), "127.0.0.1:8080");
        assert_eq!(default_for("mode").unwrap(), "production");
        assert_eq!(
            default_for("nope"),
            Err(ConfigError::UnknownKey("nope".into()))
        );
    }

    #[test]
    fn mode_parses_tolerantly() {
        assert_eq!(Mode::from_str("production").unwrap(), Mode::Production);
        assert_eq!(Mode::from_str("PROD").unwrap(), Mode::Production);
        assert_eq!(Mode::from_str("just-go").unwrap(), Mode::JustGo);
        assert_eq!(Mode::from_str("Just_Go").unwrap(), Mode::JustGo);
        assert_eq!(Mode::from_str("testing").unwrap(), Mode::Testing);
        assert_eq!(Mode::from_str(" Test ").unwrap(), Mode::Testing);
        assert_eq!(Mode::default(), Mode::Production);
        assert!(Mode::from_str("staging").is_err());
        // serde uses kebab-case.
        assert_eq!(serde_json::to_string(&Mode::JustGo).unwrap(), "\"just-go\"");
    }

    #[test]
    fn config_view_resolves_table_then_default_typed() {
        // Snapshot overrides a couple of keys; the rest fall back to defaults.
        let view = ConfigView::from_pairs([
            ("bind_addr".to_string(), "0.0.0.0:9000".to_string()),
            ("worker.port_lo".to_string(), "20000".to_string()),
            ("worker.migrate".to_string(), "true".to_string()),
            ("api_token".to_string(), String::new()),
        ]);

        // Snapshot value.
        assert_eq!(view.get_string("bind_addr").unwrap(), "0.0.0.0:9000");
        assert_eq!(view.get_u16("worker.port_lo").unwrap(), 20000);
        assert!(view.get_bool("worker.migrate").unwrap());

        // Registry default fallback (not in the snapshot).
        assert_eq!(view.get_u16("worker.port_hi").unwrap(), 16891);
        assert_eq!(view.get_u64("worker.poll_secs").unwrap(), 3);
        assert_eq!(view.mode().unwrap(), Mode::Production);
        assert_eq!(
            view.get_path("worker.download_dir").unwrap(),
            Some(std::path::PathBuf::from("/data/downloads/complete"))
        );
        // The single library root (SKADI-T-0302) falls back to its registry default.
        assert_eq!(
            view.get_path("library.root").unwrap(),
            Some(std::path::PathBuf::from("/data"))
        );

        // Empty → None for opt-string and path.
        assert_eq!(view.get_opt_string("api_token").unwrap(), None);
        assert_eq!(view.get_path("worker.watch_dir").unwrap(), None);

        // Unknown key and a bad parse are typed errors.
        assert_eq!(
            view.get_string("nope"),
            Err(ConfigError::UnknownKey("nope".into()))
        );
        let bad = ConfigView::from_pairs([("worker.port_lo".to_string(), "x".to_string())]);
        assert!(matches!(
            bad.get_u16("worker.port_lo"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn config_view_empty_uses_all_defaults() {
        let view = ConfigView::default();
        assert_eq!(view.get_string("bind_addr").unwrap(), "127.0.0.1:8080");
        assert_eq!(view.get_opt_string("api_token").unwrap(), None);
        assert_eq!(view.mode().unwrap(), Mode::Production);
    }

    #[test]
    fn read_from_collects_only_set_nonempty_tier1_keys() {
        // A fake environment: two known Tier-1 vars (one empty → skipped), a
        // Tier-0 var (excluded), and an unknown var (ignored).
        let env = |name: &str| -> Option<String> {
            match name {
                "SKADI_API_TOKEN" => Some("tok".to_string()),
                "SKADI_MODE" => Some("testing".to_string()),
                "SKADI_TMDB_API_KEY" => Some(String::new()), // empty → skipped
                "SKADI_DATABASE_URL" => Some("postgres://x".to_string()), // Tier 0
                "SKADI_SOMETHING_ELSE" => Some("nope".to_string()),
                _ => None,
            }
        };
        let mut got = read_from(env, |_| Err(std::io::ErrorKind::NotFound.into())).unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("api_token", "tok".to_string()),
                ("mode", "testing".to_string())
            ]
        );
    }

    /// SKADI-T-0702: a Tier-1 secret may come from `SKADI_X_FILE`, so the API
    /// token reaches the `config` table without being in the environment.
    #[test]
    fn read_from_takes_a_tier1_key_from_its_file_variable() {
        let env = |name: &str| -> Option<String> {
            (name == "SKADI_API_TOKEN_FILE").then(|| "/run/secrets/skadi_api_token".to_string())
        };
        let read = |p: &std::path::Path| -> std::io::Result<String> {
            assert_eq!(p, std::path::Path::new("/run/secrets/skadi_api_token"));
            Ok("tok-from-file\n".into())
        };
        assert_eq!(
            read_from(env, read).unwrap(),
            vec![("api_token", "tok-from-file".to_string())]
        );
    }

    #[test]
    fn read_from_rejects_a_key_set_both_ways() {
        let env = |name: &str| -> Option<String> {
            match name {
                "SKADI_API_TOKEN" => Some("plain".into()),
                "SKADI_API_TOKEN_FILE" => Some("/f".into()),
                _ => None,
            }
        };
        let err = read_from(env, |_| Ok("file".into())).unwrap_err();
        assert_eq!(
            err,
            EnvError::BothSet {
                name: "SKADI_API_TOKEN".into()
            }
        );
    }
}
