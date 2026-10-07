//! Skadi-native torrent download worker (SKADI-I-0013).
//!
//! A **DB agent**: it embeds a [`librqbit`] session and drives the `downloads`
//! queue in `skadi-store` (no HTTP). The main Skadi daemon enqueues a `queued`
//! row through its `Downloader` trait; this worker [`claim_next`]s it, adds the
//! torrent, and writes progress / completion / errors back to the row. It also
//! actions `remove_requested` rows. Neither side calls the other — both just
//! reach Postgres — so the queue is the durable, crash-recoverable record.
//!
//! Designed to run in its own container inside gluetun's VPN namespace, writing
//! into a shared `/data/downloads` so the daemon's importer hardlinks. It serves
//! no port; it only needs `SKADI_DATABASE_URL`.
//!
//! [`claim_next`]: skadi_store::DownloadJobRepo::claim_next

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use librqbit::api::{Api, TorrentIdOrHash};
use librqbit::{
    AddTorrent, AddTorrentOptions, Session, SessionOptions, SessionPersistenceConfig, TorrentStats,
    TorrentStatsState,
};
use skadi_store::{
    ConfigRepo, ConfigSource, DownloadCategory, DownloadCategoryRepo, DownloadJob, DownloadJobRepo,
    DownloadJobStatus, DownloadProgress, NewDownloadJob, Store, WorkerStatusRepo,
};

/// This worker build's version, reported in its liveness heartbeat (SKADI-T-0288).
const WORKER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod egress;
pub mod seed_policy;
pub use seed_policy::{SeedAction, SeedPolicy, SeedVerdict};

/// Worker configuration, from the environment.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where torrents are written (shared with the daemon for hardlink import).
    pub download_dir: PathBuf,
    /// librqbit session/resume state directory.
    pub state_dir: PathBuf,
    /// Incoming-peer TCP listen range.
    pub port_range: std::ops::Range<u16>,
    /// The `downloads`-queue database (the same Postgres the daemon uses).
    pub database_url: String,
    /// Identifies this worker when it claims rows.
    pub worker_id: String,
    /// How often to scan for removals + claim new jobs.
    pub poll_interval: Duration,
    /// How often a tracking task writes progress for its torrent.
    pub tick_interval: Duration,
    /// Optional **watched folder**: dropped `.torrent` / `.magnet` files are
    /// ingested into the `downloads` queue each poll (see [`scan_watch_dir`]).
    /// `None` ⇒ no watch folder.
    pub watch_dir: Option<PathBuf>,
    /// Run the store migrations on startup. Off by default so the worker never
    /// races the daemon's migrations in the normal (DB-agent) deployment; turn
    /// on for **standalone** use (worker owns its own SQLite, no daemon).
    pub migrate: bool,
    /// Seeding limits (ratio/time + stop/remove action). Unlimited by default
    /// (SKADI-T-0209).
    pub seed_policy: SeedPolicy,
    /// Global download bandwidth cap (bytes/sec); `None` = unlimited (SKADI-T-0211).
    pub down_limit_bps: Option<u32>,
    /// Global upload bandwidth cap (bytes/sec); `None` = unlimited.
    pub up_limit_bps: Option<u32>,
    /// Max concurrent active downloads; `None` = unlimited (SKADI-T-0212).
    pub max_active: Option<usize>,
    /// Seconds of no-progress-and-no-peers before a download is flagged `stalled`;
    /// `None` = disabled (SKADI-T-0213).
    pub stall_timeout_secs: Option<i64>,
    /// Seconds to wait for a magnet's metadata to resolve before failing the add;
    /// `None` = wait indefinitely (SKADI-T-0307). Shallow-seed torrents (typical for
    /// audiobooks) can take a long time to find peers — erroring them is worse than
    /// waiting, so the default is no timeout.
    pub metadata_timeout_secs: Option<i64>,
    /// Claim-lease length in seconds, refreshed every tick while tracking
    /// (SKADI-T-0214).
    pub lease_secs: i64,
    /// Trackers every torrent is announced to on top of its own
    /// (SKADI-T-0597, `worker.extra_trackers`). Applied at session start.
    pub extra_trackers: Vec<String>,
}

/// Apply a category's per-axis seed overrides onto the global policy (SKADI-T-0215).
/// Each `None`/unset category field keeps the corresponding global value; a category
/// ratio/time of `<= 0` means "unlimited on this axis". Pure.
#[must_use]
pub fn effective_seed_policy(global: SeedPolicy, cat: &DownloadCategory) -> SeedPolicy {
    SeedPolicy {
        ratio_limit: match cat.seed_ratio {
            Some(r) => (r > 0.0).then_some(r),
            None => global.ratio_limit,
        },
        time_limit_secs: match cat.seed_time_mins {
            Some(m) => (m > 0).then_some(m * 60),
            None => global.time_limit_secs,
        },
        action: cat
            .seed_action
            .as_deref()
            .map(SeedAction::parse)
            .unwrap_or(global.action),
    }
}

/// Whether a downloading torrent is stalled: no peers **and** no progress for at
/// least the stall timeout. `None`/`<=0` timeout ⇒ never. Pure (SKADI-T-0213).
#[must_use]
pub fn is_stalled(no_progress_secs: i64, peers: i32, stall_timeout_secs: Option<i64>) -> bool {
    matches!(stall_timeout_secs, Some(t) if t > 0 && peers <= 0 && no_progress_secs >= t)
}

/// How many new downloads to claim this tick: the cap minus the active count
/// (`None` cap ⇒ unbounded). Pure (SKADI-T-0212).
#[must_use]
pub fn claim_budget(active: usize, max_active: Option<usize>) -> usize {
    match max_active {
        Some(max) => max.saturating_sub(active),
        None => usize::MAX,
    }
}

/// Convert a config bytes/sec value to a librqbit limit: `0` ⇒ unlimited (`None`),
/// otherwise clamped into `u32` (the rate-limiter's quota type; ~4 GB/s ceiling is
/// far above any real link). Pure for unit testing (SKADI-T-0211).
#[must_use]
pub fn bps_limit(v: u64) -> Option<u32> {
    (v > 0).then(|| v.min(u64::from(u32::MAX)) as u32)
}

impl Config {
    /// Build from env with sane defaults.
    pub fn from_env() -> Self {
        let download_dir = std::env::var("SKADI_WORKER_DOWNLOAD_DIR")
            .map(PathBuf::from)
            // Default under the single library root's downloads/complete (SKADI-T-0302).
            .unwrap_or_else(|_| PathBuf::from("/data/downloads/complete"));
        let state_dir = std::env::var("SKADI_WORKER_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| download_dir.join(".rqbit-session"));
        let database_url = std::env::var("SKADI_DATABASE_URL")
            .unwrap_or_else(|_| skadi_store::DEFAULT_DATABASE_URL.to_string());
        let worker_id =
            std::env::var("SKADI_WORKER_ID").unwrap_or_else(|_| "skadi-worker".to_string());
        let lo: u16 = std::env::var("SKADI_WORKER_PORT_LO")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16881);
        let hi: u16 = std::env::var("SKADI_WORKER_PORT_HI")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16891);
        let poll_interval = Duration::from_secs(
            std::env::var("SKADI_WORKER_POLL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3),
        );
        let tick_interval = Duration::from_secs(
            std::env::var("SKADI_WORKER_TICK_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1),
        );
        let watch_dir = std::env::var("SKADI_WORKER_WATCH_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let migrate = std::env::var("SKADI_WORKER_MIGRATE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let seed_ratio: f64 = std::env::var("SKADI_WORKER_SEED_RATIO")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let seed_time_mins: u64 = std::env::var("SKADI_WORKER_SEED_TIME_MINS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let seed_action =
            SeedAction::parse(&std::env::var("SKADI_WORKER_SEED_ACTION").unwrap_or_default());
        let env_bps = |k: &str| {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };
        Config {
            download_dir,
            state_dir,
            port_range: lo..hi,
            database_url,
            worker_id,
            poll_interval,
            tick_interval,
            watch_dir,
            migrate,
            seed_policy: SeedPolicy::from_config(seed_ratio, seed_time_mins, seed_action),
            down_limit_bps: bps_limit(env_bps("SKADI_WORKER_DOWN_LIMIT_BPS")),
            up_limit_bps: bps_limit(env_bps("SKADI_WORKER_UP_LIMIT_BPS")),
            max_active: (env_bps("SKADI_WORKER_MAX_ACTIVE") > 0)
                .then(|| env_bps("SKADI_WORKER_MAX_ACTIVE") as usize),
            stall_timeout_secs: (env_bps("SKADI_WORKER_STALL_TIMEOUT_SECS") > 0)
                .then(|| env_bps("SKADI_WORKER_STALL_TIMEOUT_SECS") as i64),
            metadata_timeout_secs: (env_bps("SKADI_WORKER_METADATA_TIMEOUT_SECS") > 0)
                .then(|| env_bps("SKADI_WORKER_METADATA_TIMEOUT_SECS") as i64),
            lease_secs: std::env::var("SKADI_WORKER_LEASE_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(120),
            extra_trackers: parse_tracker_list(
                &std::env::var("SKADI_WORKER_EXTRA_TRACKERS")
                    .unwrap_or_else(|_| skadi_config::DEFAULT_EXTRA_TRACKERS.to_string()),
            ),
        }
    }

    /// Build the worker config from a resolved [`ConfigView`](skadi_config::ConfigView)
    /// over the shared `config` table (SKADI-I-0014). Pure: `database_url` and
    /// `migrate` are pre-table (Tier-0 / bootstrap-control), passed in. Every
    /// other field resolves table value → registry default, so an empty table
    /// (standalone) yields the same defaults as [`from_env`](Self::from_env).
    pub fn from_view(
        view: &skadi_config::ConfigView,
        database_url: String,
        migrate: bool,
    ) -> Result<Self, skadi_config::ConfigError> {
        Ok(Config {
            download_dir: view
                .get_path("worker.download_dir")?
                .unwrap_or_else(|| PathBuf::from("/data/downloads/complete")),
            state_dir: view
                .get_path("worker.state_dir")?
                .unwrap_or_else(|| PathBuf::from("/data/downloads/.rqbit-session")),
            port_range: view.get_u16("worker.port_lo")?..view.get_u16("worker.port_hi")?,
            database_url,
            worker_id: view.get_string("worker.id")?,
            poll_interval: Duration::from_secs(view.get_u64("worker.poll_secs")?),
            tick_interval: Duration::from_secs(view.get_u64("worker.tick_secs")?),
            watch_dir: view.get_path("worker.watch_dir")?,
            migrate,
            seed_policy: SeedPolicy::from_config(
                view.get_string("worker.seed_ratio")?.parse().unwrap_or(0.0),
                view.get_u64("worker.seed_time_mins")?,
                SeedAction::parse(&view.get_string("worker.seed_action")?),
            ),
            down_limit_bps: bps_limit(view.get_u64("worker.down_limit_bps")?),
            up_limit_bps: bps_limit(view.get_u64("worker.up_limit_bps")?),
            max_active: {
                let v = view.get_u64("worker.max_active")?;
                (v > 0).then_some(v as usize)
            },
            stall_timeout_secs: {
                let v = view.get_u64("worker.stall_timeout_secs")?;
                (v > 0).then_some(v as i64)
            },
            metadata_timeout_secs: {
                let v = view.get_u64("worker.metadata_timeout_secs")?;
                (v > 0).then_some(v as i64)
            },
            lease_secs: view.get_u64("worker.lease_secs")? as i64,
            extra_trackers: parse_tracker_list(&view.get_string("worker.extra_trackers")?),
        })
    }

    /// Resolve the worker config from the shared `config` table — the worker on
    /// the same config plane as the daemon (SKADI-T-0103). `database_url` and
    /// `migrate` are read from env directly (pre-table). The worker then seeds
    /// **its own** `SKADI_*` env into the table (env overwrites) so its
    /// container's `SKADI_WORKER_*` vars are authoritative even though the
    /// daemon — a separate container — seeded the table from a different
    /// environment. Falls back to [`from_env`](Self::from_env) if the table
    /// isn't reachable yet (e.g. the worker started before the daemon migrated).
    pub async fn resolve() -> Self {
        let database_url = std::env::var("SKADI_DATABASE_URL")
            .unwrap_or_else(|_| skadi_store::DEFAULT_DATABASE_URL.to_string());
        let migrate = std::env::var("SKADI_WORKER_MIGRATE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        match Self::resolve_io(database_url, migrate).await {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(error = %e, "config table unavailable; using env/defaults");
                Self::from_env()
            }
        }
    }

    async fn resolve_io(database_url: String, migrate: bool) -> anyhow::Result<Self> {
        let store = Store::connect(&database_url)?;
        if migrate {
            store.run_migrations().await?;
        }
        // Seed this process's env into the table (env wins) so the worker
        // container's SKADI_WORKER_* values are authoritative.
        for (key, value) in skadi_config::read_env() {
            store.set_config(key, &value, ConfigSource::Env).await?;
        }
        let view = skadi_config::ConfigView::from_pairs(
            store
                .list_config()
                .await?
                .into_iter()
                .map(|e| (e.key, e.value)),
        );
        Ok(Self::from_view(&view, database_url, migrate)?)
    }
}

/// How many torrents librqbit initialises (hash-checks) at once. librqbit's
/// default is 3 and its init semaphore is FIFO, shared by torrents restored from
/// the session file at startup **and** by fresh adds — so a new grab sits behind
/// every not-yet-checked seed. With fastresume a restored seed only spot-checks
/// a few pieces, so a slightly wider window lets fresh adds (empty files → an
/// instant check) slip through sooner. Kept modest on purpose: a full re-hash is
/// bound by the bind mount at ~5 MB/s total, so more width adds no throughput,
/// only IO-wait on the shared Docker VM (measured 8-wide, SKADI-T-0391).
const CONCURRENT_INIT_LIMIT: usize = 4;

/// Size of librqbit's deferred disk-write queue (MiB) — see `build_api`.
const DEFER_WRITES_MIB: usize = 16;

/// Next librqbit torrent id this worker hands out as `preferred_id` on every add.
///
/// librqbit 8.1's JSON persistence allocates ids as `max(persisted) + 1`, read
/// *before* the add is persisted, so N adds in flight at once all compute the
/// same id; the later ones then hit the session's "id already managed" check and
/// come back `AlreadyManaged` — pointing at a *different* torrent — as `Ok`. The
/// startup re-attach adds every seed concurrently, so after a hard restart that
/// lost the session file 298 of 300 lab seeds "attached" to nothing (every
/// follow-up `start`/`stats` by hash: not found) and two shared one id/bitfield
/// (SKADI-T-0392 lab, 2026-09-06). Handing librqbit unique ids sidesteps the
/// race entirely. Seeded from the restored session by [`seed_torrent_ids`].
static NEXT_TORRENT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Point [`NEXT_TORRENT_ID`] past every id the session restored at startup.
fn seed_torrent_ids(api: &Api) {
    let next = api
        .api_torrent_list()
        .torrents
        .iter()
        .filter_map(|t| t.id)
        .max()
        .map_or(0, |m| m + 1);
    NEXT_TORRENT_ID.store(next, std::sync::atomic::Ordering::SeqCst);
}

/// A fresh, process-unique librqbit torrent id for an add (see [`NEXT_TORRENT_ID`]).
fn next_torrent_id() -> usize {
    NEXT_TORRENT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// `true` if the env var is set to `1`, `true`, `yes` or `on` (case-insensitive).
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Split a `worker.extra_trackers` value — comma, whitespace or newline
/// separated — into distinct tracker URLs, keeping only schemes librqbit can
/// announce to. Pure for unit testing (SKADI-T-0597).
#[must_use]
pub fn parse_tracker_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in raw.split(|c: char| c == ',' || c.is_whitespace() || c == '\\') {
        let t = tok.trim();
        if t.is_empty() || out.iter().any(|o| o == t) {
            continue;
        }
        if t.starts_with("udp://")
            || t.starts_with("http://")
            || t.starts_with("https://")
            || t.starts_with("wss://")
        {
            out.push(t.to_string());
        }
    }
    out
}

/// The parsed set librqbit's session takes; a URL that does not parse is logged
/// and skipped rather than failing startup.
fn extra_tracker_urls(list: &[String]) -> std::collections::HashSet<url::Url> {
    let mut set = std::collections::HashSet::new();
    for t in list {
        match url::Url::parse(t) {
            Ok(u) => {
                set.insert(u);
            }
            Err(e) => tracing::warn!(tracker = %t, "ignoring unparseable extra tracker: {e}"),
        }
    }
    if !set.is_empty() {
        tracing::info!(
            count = set.len(),
            "announcing every torrent to extra trackers"
        );
    }
    set
}

/// The port gluetun forwarded through the VPN, read from the status file named
/// by `SKADI_WORKER_FORWARDED_PORT_FILE` (gluetun's `VPN_PORT_FORWARDING_STATUS_FILE`,
/// a bare decimal). `None` when the variable is unset, the file is missing or
/// empty (forwarding off, or gluetun has not written it yet), or it holds no
/// port — every one of those means "use the configured range" (SKADI-T-0587).
fn forwarded_port_from_env() -> Option<u16> {
    let path = std::env::var("SKADI_WORKER_FORWARDED_PORT_FILE")
        .ok()
        .filter(|p| !p.is_empty())?;
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_forwarded_port(&text),
        // Absent is the normal state with forwarding off: gluetun only writes
        // the file when it has a port. Anything else is worth a warning.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path, "no forwarded-port file; using the configured range");
            None
        }
        Err(e) => {
            tracing::warn!(
                path,
                "forwarded-port file unreadable; using the configured range: {e}"
            );
            None
        }
    }
}

/// A gluetun forwarded-port file holds one decimal port; `0` (no port yet) and
/// junk both mean "none". Pure for unit testing.
#[must_use]
pub fn parse_forwarded_port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

/// Build the librqbit session + `Api` for `cfg` (creates the download/state dirs,
/// resumes any persisted torrents). Returns the `Api` facade.
///
/// `fastresume` is on: librqbit persists each torrent's have-pieces bitfield
/// (mmap-backed, next to the session JSON in `state_dir`) and on restart
/// spot-checks a sample of pieces instead of re-hashing every byte. Without it a
/// restart re-read the whole seeded library — ~207 GiB / 10 h on the production
/// bind mount — and every new download queued behind that (SKADI-T-0391). A
/// bitfield that fails its spot-check is discarded and the torrent gets the full
/// check, so out-of-band file damage still surfaces.
pub async fn build_api(cfg: &Config) -> anyhow::Result<Arc<Api>> {
    std::fs::create_dir_all(&cfg.download_dir)?;
    std::fs::create_dir_all(&cfg.state_dir)?;
    let opts_disable_dht = env_flag("SKADI_WORKER_DISABLE_DHT");
    // A VPN-forwarded port wins over the configured range (SKADI-T-0587): the
    // range is only reachable when something forwards it, and behind a VPN
    // without forwarding no peer ever connects *in* — a one-seeder audiobook
    // seeded from behind another NAT stalls at 0% forever.
    let listen_range = match forwarded_port_from_env() {
        Some(port) => {
            tracing::info!(port, "listening on the VPN-forwarded port");
            port..port.saturating_add(1)
        }
        None => cfg.port_range.clone(),
    };
    let opts = SessionOptions {
        persistence: Some(SessionPersistenceConfig::Json {
            folder: Some(cfg.state_dir.clone()),
        }),
        fastresume: true,
        concurrent_init_limit: Some(CONCURRENT_INIT_LIMIT),
        listen_port_range: Some(listen_range),
        enable_upnp_port_forwarding: false,
        // Announce every torrent to these as well as its own (SKADI-T-0597).
        trackers: extra_tracker_urls(&cfg.extra_trackers),
        // Route piece writes through librqbit's single `disk_writer` task
        // (bounded in-memory queue, ~16 MiB) instead of one `block_in_place` per
        // received piece per peer. On a stalled mount the latter turns every
        // peer into a parked blocking thread (SKADI-T-0392); with the writer,
        // writes back-pressure into the queue and only ONE thread blocks.
        defer_writes_up_to: Some(DEFER_WRITES_MIB),
        // Lab isolation knob (SKADI-T-0393): the lab has no VPN and no real
        // trackers, but librqbit would still announce every synthetic torrent to
        // the public DHT. `SKADI_WORKER_DISABLE_DHT=1` keeps it quiet (and lets
        // SKADI-T-0392 A/B the DHT's memory footprint). Unset in prod.
        disable_dht: opts_disable_dht,
        // Global up/down bandwidth caps (SKADI-T-0211); `None` ⇒ unlimited.
        ratelimits: librqbit::limits::LimitsConfig {
            upload_bps: cfg.up_limit_bps.and_then(std::num::NonZeroU32::new),
            download_bps: cfg.down_limit_bps.and_then(std::num::NonZeroU32::new),
        },
        ..Default::default()
    };
    // Time the restore (SKADI-T-0480). `Session::new_with_opts` awaits an
    // `add_torrent` future per persisted torrent; initialisation (the hash check)
    // is spawned separately and is NOT awaited here, so this measures the adds
    // alone. Over NFS-on-Wi-Fi that was ~4.5 s each — 28 minutes for 366 — and the
    // worker claims nothing until it returns. Logging the count and the elapsed
    // time together is what turns the next slow startup into a number instead of a
    // guess.
    let restore_started = std::time::Instant::now();
    let session: Arc<Session> = Session::new_with_opts(cfg.download_dir.clone(), opts).await?;
    let restore_secs = restore_started.elapsed().as_secs_f64();
    let api = Arc::new(Api::new(session, None));
    seed_torrent_ids(&api);
    // One line that answers "is the restart re-hashing everything?" up front:
    // librqbit logs `Doing initial checksum validation` only for torrents whose
    // fastresume bitfield is missing or failed its spot-check (SKADI-T-0391).
    let restored = api.api_torrent_list().torrents.len();
    if restore_secs > 60.0 {
        tracing::warn!(
            restored,
            restore_secs,
            per_torrent_secs = restore_secs / (restored.max(1) as f64),
            "session restore took over a minute; the worker claimed nothing for that long. \
             Torrents with no live download row are dropped next — see SKADI-T-0480"
        );
    }
    tracing::info!(
        restored,
        restore_secs,
        fastresume = true,
        concurrent_init_limit = CONCURRENT_INIT_LIMIT,
        dht = !opts_disable_dht,
        state_dir = %cfg.state_dir.display(),
        "librqbit session up; torrents restored from session persistence"
    );
    Ok(api)
}

/// The subset of worker config that's **hot-reloaded every tick** (SKADI-T-0291):
/// bandwidth caps (re-applied to the live librqbit session), the active-download
/// cap, and the seed/stall policy. The rest of [`Config`] (paths, ports, id) is
/// fixed at startup, so it's deliberately not re-read here.
#[derive(Clone, Copy, PartialEq)]
struct LiveSettings {
    max_active: Option<usize>,
    up_limit_bps: Option<u32>,
    down_limit_bps: Option<u32>,
    seed_policy: SeedPolicy,
    stall_timeout_secs: Option<i64>,
    metadata_timeout_secs: Option<i64>,
}

impl LiveSettings {
    /// The startup values — the fallback when a re-read can't reach the table.
    fn from_cfg(cfg: &Config) -> Self {
        Self {
            max_active: cfg.max_active,
            up_limit_bps: cfg.up_limit_bps,
            down_limit_bps: cfg.down_limit_bps,
            seed_policy: cfg.seed_policy,
            stall_timeout_secs: cfg.stall_timeout_secs,
            metadata_timeout_secs: cfg.metadata_timeout_secs,
        }
    }

    /// Re-read the hot subset from the shared config table (same keys/parsing as
    /// [`Config::from_view`]). A UI write lands here on the next tick — no restart.
    async fn reload(store: &Store) -> anyhow::Result<Self> {
        let view = skadi_config::ConfigView::from_pairs(
            store
                .list_config()
                .await?
                .into_iter()
                .map(|e| (e.key, e.value)),
        );
        Ok(Self {
            max_active: {
                let v = view.get_u64("worker.max_active")?;
                (v > 0).then_some(v as usize)
            },
            up_limit_bps: bps_limit(view.get_u64("worker.up_limit_bps")?),
            down_limit_bps: bps_limit(view.get_u64("worker.down_limit_bps")?),
            seed_policy: SeedPolicy::from_config(
                view.get_string("worker.seed_ratio")?.parse().unwrap_or(0.0),
                view.get_u64("worker.seed_time_mins")?,
                SeedAction::parse(&view.get_string("worker.seed_action")?),
            ),
            stall_timeout_secs: {
                let v = view.get_u64("worker.stall_timeout_secs")?;
                (v > 0).then_some(v as i64)
            },
            metadata_timeout_secs: {
                let v = view.get_u64("worker.metadata_timeout_secs")?;
                (v > 0).then_some(v as i64)
            },
        })
    }

    /// Push the bandwidth caps onto the running session (`None` ⇒ unlimited).
    fn apply_ratelimits(&self, api: &Api) {
        api.session()
            .ratelimits
            .set_upload_bps(self.up_limit_bps.and_then(std::num::NonZeroU32::new));
        api.session()
            .ratelimits
            .set_download_bps(self.down_limit_bps.and_then(std::num::NonZeroU32::new));
    }
}

/// Run the DB agent: build the librqbit `Api` + connect the store, then loop —
/// action removals, claim queued jobs, and spawn a tracking task per claim. Runs
/// forever (until the process is stopped).
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    // Before anything else (SKADI-T-0479). `build_api` restores the librqbit
    // session, which for a few hundred seeds takes minutes — the process is
    // working the whole time, but the container healthcheck reads this marker, so
    // writing it after the restore made a busy worker look dead for its entire
    // startup. This is the first thing the worker does.
    touch_heartbeat_file();
    let api = build_api(&cfg).await?;
    // The daemon owns the schema (runs migrations); the worker only reads/writes
    // the `downloads` table, so it does NOT migrate in the normal deployment
    // (avoids a concurrent-migrate race against the daemon). In standalone mode
    // (`SKADI_WORKER_MIGRATE`) there is no daemon, so the worker owns the schema.
    let store = Store::connect(&cfg.database_url)?;
    if cfg.migrate {
        store.run_migrations().await?;
        tracing::info!("ran store migrations (standalone mode)");
    }
    tracing::info!(
        worker = %cfg.worker_id,
        download_dir = %cfg.download_dir.display(),
        watch_dir = ?cfg.watch_dir,
        "skadi-downloader-worker (db agent) started"
    );

    // Re-claim downloads orphaned by a previous worker instance: reset any
    // `downloading` row to `queued` so the drain loop re-claims + re-tracks it
    // (librqbit resumed them from session persistence already). SKADI-T-0168.
    match store.reclaim_orphaned_downloads().await {
        Ok(n) if n > 0 => tracing::info!("re-queued {n} orphaned download(s) on startup"),
        Ok(_) => {}
        Err(e) => tracing::warn!("reclaim_orphaned_downloads failed: {e}"),
    }

    // Drop restored torrents we are no longer tracking, before re-attaching the
    // ones we are (SKADI-T-0480). Every one dropped here is one fewer torrent the
    // next startup has to re-add.
    reconcile_session(&api, &store).await;

    // Re-attach seeding torrents (Completed rows) to librqbit so the worker resumes
    // live tracking and can action removals after a restart (SKADI-T-0172). De-dupe
    // by info_hash — duplicate rows for one real torrent only need one tracker.
    match store.list_downloads().await {
        Ok(jobs) => {
            let mut seen = std::collections::HashSet::new();
            let seeds: Vec<DownloadJob> = jobs
                .into_iter()
                .filter(|j| j.status == DownloadJobStatus::Completed)
                .filter(|j| match j.info_hash.as_deref() {
                    Some(h) => seen.insert(h.to_string()),
                    None => false, // no hash → nothing to re-attach
                })
                .collect();
            if !seeds.is_empty() {
                tracing::info!("re-attaching {} seeding torrent(s) on startup", seeds.len());
                for job in seeds {
                    tokio::spawn(reattach_seed(
                        api.clone(),
                        store.clone(),
                        job,
                        cfg.tick_interval,
                        cfg.seed_policy,
                        cfg.metadata_timeout_secs,
                    ));
                }
            }
        }
        Err(e) => tracing::warn!("list_downloads (seed re-attach) failed: {e}"),
    }

    // Last-applied live settings, so we log only on an actual change (not every tick).
    let mut last_live: Option<LiveSettings> = None;
    // The egress IP the heartbeat carries (SKADI-T-0683): asked of gluetun on
    // loopback, at most once a minute (see `egress`).
    let mut egress = egress::EgressProbe::from_env();
    loop {
        // Liveness heartbeat (SKADI-T-0288): upsert our last-seen every tick,
        // independent of claimed jobs, so the daemon can tell a healthy-idle worker
        // from a dead one (the per-job lease can't — no job, no signal). It carries
        // the egress IP this worker sees, for the daemon's `vpn` check.
        let egress_ip = egress.current().await;
        if let Err(e) = store
            .heartbeat_worker(&cfg.worker_id, WORKER_VERSION, egress_ip.as_deref())
            .await
        {
            tracing::debug!("worker heartbeat failed: {e}");
        }
        // The same liveness, as a file the container healthcheck can read
        // (SKADI-T-0479): the worker has no HTTP port and the image has no psql,
        // so the database row above is invisible to Docker. Written once at
        // startup too, before the session restore.
        touch_heartbeat_file();

        // Hot-reload the live settings (SKADI-T-0291): a UI change to rate caps /
        // max-active / seed policy takes effect here, no restart. Rate caps push
        // onto the running session immediately; the cap + seed/stall drive this
        // tick's claims and spawns. A failed read keeps the last-good (startup) set.
        let live = LiveSettings::reload(&store).await.unwrap_or_else(|e| {
            tracing::debug!("live-settings reload failed; keeping startup values: {e}");
            LiveSettings::from_cfg(&cfg)
        });
        if last_live != Some(live) {
            tracing::info!(
                max_active = ?live.max_active,
                up_limit_bps = ?live.up_limit_bps,
                down_limit_bps = ?live.down_limit_bps,
                seed_ratio = ?live.seed_policy.ratio_limit,
                seed_time_secs = ?live.seed_policy.time_limit_secs,
                seed_action = ?live.seed_policy.action,
                "applied live worker settings"
            );
            last_live = Some(live);
        }
        live.apply_ratelimits(&api);

        handle_removals(&api, &store).await;
        handle_pauses(&api, &store).await;
        // Before the claim budget is computed, so the cap is enforced over what is
        // actually live — including torrents the session restore started on its own
        // (SKADI-T-0489).
        enforce_active_cap(&api, &store, live.max_active).await;
        scan_watch_dir(&store, &cfg).await;

        // Reclaim downloads whose claim lease lapsed — a worker that died mid-transfer
        // without un-claiming (SKADI-T-0214). Live rows heartbeat every tick, so this
        // only ever touches genuinely abandoned ones.
        match store.reclaim_expired_downloads().await {
            Ok(n) if n > 0 => tracing::warn!("reclaimed {n} download(s) with a lapsed lease"),
            Ok(_) => {}
            Err(e) => tracing::warn!("reclaim_expired_downloads failed: {e}"),
        }

        // Drain the queue, bounded by `max_active` (SKADI-T-0212): claim at most
        // `budget = max_active - active` queued rows this tick. Each claim flips its
        // row to `downloading`, so decrementing the budget per claim keeps the bound
        // exact within the tick; the next tick re-reads the active count.
        let active = store.count_active_downloads().await.unwrap_or(0).max(0) as usize;
        let mut budget = claim_budget(active, live.max_active);
        while budget > 0 {
            match store.claim_next(&cfg.worker_id).await {
                Ok(Some(job)) => {
                    tracing::info!(job = %job.id, source = %job.source, "claimed download");
                    tokio::spawn(run_job(
                        api.clone(),
                        store.clone(),
                        job,
                        cfg.tick_interval,
                        live.seed_policy,
                        live.stall_timeout_secs,
                        live.metadata_timeout_secs,
                        cfg.lease_secs,
                    ));
                    budget -= 1;
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("claim_next failed: {e}");
                    break;
                }
            }
        }

        tokio::time::sleep(cfg.poll_interval).await;
    }
}

/// Action every `remove_requested` row: tell librqbit to forget (keep data) or
/// delete (drop data) by info-hash, then mark the row `removed`.
async fn handle_removals(api: &Arc<Api>, store: &Store) {
    let pending = match store.list_remove_requested().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("list_remove_requested failed: {e}");
            return;
        }
    };
    for job in pending {
        if let Some(hash) = &job.info_hash
            && let Ok(id) = TorrentIdOrHash::parse(hash)
        {
            let res = if job.delete_data {
                api.api_torrent_action_delete(id).await
            } else {
                api.api_torrent_action_forget(id).await
            };
            if let Err(e) = res {
                // Already gone / unknown is fine — still mark it removed.
                tracing::warn!(job = %job.id, "librqbit remove action failed (continuing): {e}");
            }
        }
        // librqbit only knows the incomplete tree it downloaded into; the finished
        // files it mirrored into `complete_dir` on completion are hardlinks it has
        // never heard of, so a delete-data removal left a full copy behind
        // (SKADI-T-0592: a fake 1 GB `.exe` release stayed on disk after
        // "removed delete_data=true"). Drop that mirror too.
        if job.delete_data {
            let files = job.files.clone();
            let complete_dir = job.complete_dir.clone();
            let job_id = job.id.clone();
            let removed = tokio::task::spawn_blocking(move || {
                remove_complete_mirror(&files, complete_dir.as_deref())
            })
            .await
            .unwrap_or(0);
            if removed > 0 {
                tracing::info!(job = %job_id, files = removed, "removed the complete-dir mirror of a deleted download");
            }
        }
        match store.mark_removed(&job.id).await {
            Ok(()) => tracing::info!(job = %job.id, delete_data = job.delete_data, "removed"),
            Err(e) => tracing::warn!(job = %job.id, "mark_removed failed: {e}"),
        }
    }
}

/// Info-hashes the worker is still responsible for: any row that is not terminal
/// (SKADI-T-0480). `Completed` counts — it means "downloaded and still seeding".
///
/// Pure over the job list so the reconcile rule is unit-testable without a
/// librqbit session.
#[must_use]
pub fn live_info_hashes(jobs: &[DownloadJob]) -> std::collections::HashSet<String> {
    jobs.iter()
        .filter(|j| {
            !matches!(
                j.status,
                DownloadJobStatus::Seeded | DownloadJobStatus::Removed
            )
        })
        .filter_map(|j| j.info_hash.as_ref().map(|h| h.to_ascii_lowercase()))
        .collect()
}

/// Forget session-restored torrents the worker is no longer tracking
/// (SKADI-T-0480).
///
/// librqbit's session file is append-only from our side: a torrent that finished
/// seeding is dropped from the session by the seed policy *only while its job is
/// being tracked*. A row that went terminal any other way — `Seeded` via a
/// restart, a deleted row, a job removed out-of-band — leaves its torrent in the
/// session file forever, and every subsequent startup re-adds it. That is how a
/// worker accumulated 366 restored torrents and a 28-minute startup on a slow
/// mount: not one slow restore, but years of restores that never shrank.
///
/// `forget` keeps the files on disk — this only detaches the torrent — so the
/// worst case of a wrong call is that a seed stops seeding, not data loss.
async fn reconcile_session(api: &Arc<Api>, store: &Store) {
    let jobs = match store.list_downloads().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("list_downloads (session reconcile) failed: {e}");
            return;
        }
    };
    let live = live_info_hashes(&jobs);
    let restored = api.api_torrent_list().torrents;
    let mut forgotten = 0usize;
    for t in restored {
        let hash = t.info_hash.to_ascii_lowercase();
        if live.contains(&hash) {
            continue;
        }
        match TorrentIdOrHash::parse(&hash) {
            Ok(id) => match api.api_torrent_action_forget(id).await {
                Ok(_) => forgotten += 1,
                Err(e) => tracing::warn!(%hash, "could not forget untracked torrent: {e}"),
            },
            Err(e) => tracing::warn!(%hash, "unparseable restored info_hash: {e}"),
        }
    }
    if forgotten > 0 {
        tracing::info!(
            forgotten,
            remaining = live.len(),
            "dropped session-restored torrents with no live download row; \
             the session file shrinks by this much for the next restart"
        );
    }
}

/// Bring the number of live transfers back under `max_active` (SKADI-T-0489).
///
/// librqbit restarts **every** persisted torrent when the session is restored,
/// with no notion of our cap, so a worker whose queue had grown while the cap was
/// unset comes back up running all of them at once — which is how a 4 GB NAS
/// ended up with 38 concurrent downloads and swapped past its 1 GiB fuse
/// (SKADI-T-0392). The claim loop's budget cannot help: those transfers were never
/// claimed this run, they were simply already there.
///
/// The excess is paused in librqbit (which keeps the downloaded data) and its rows
/// are returned to `queued`, so the ordinary claim loop picks them back up as
/// slots free and `start_if_paused` un-pauses them. Oldest-claimed rows are kept,
/// matching `claim_next`'s FIFO order, so the queue's discipline is unchanged.
async fn enforce_active_cap(api: &Arc<Api>, store: &Store, max_active: Option<usize>) {
    let Some(max) = max_active else { return };
    let released = match store.release_active_beyond(max).await {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("release_active_beyond failed: {e}");
            return;
        }
    };
    if released.is_empty() {
        return;
    }
    tracing::warn!(
        released = released.len(),
        max_active = max,
        "more transfers were live than worker.max_active allows; re-queueing the excess"
    );
    for job in released {
        if let Some(hash) = &job.info_hash
            && let Ok(id) = TorrentIdOrHash::parse(hash)
            && let Err(e) = api.api_torrent_action_pause(id).await
        {
            // The row is already back in `queued`, so the job is not lost — it is
            // just still transferring until the next claim re-tracks it.
            tracing::warn!(job = %job.id, "could not pause over-cap transfer (continuing): {e}");
        }
    }
}

/// Pause librqbit for every `paused` row (operator action, SKADI-T-0168). The
/// tracking task for the job already stops when the row leaves `downloading`; this
/// tells librqbit to actually halt the transfer. Idempotent (re-pausing is a
/// no-op), and a no-op for a queued/not-yet-added job (no info_hash).
async fn handle_pauses(api: &Arc<Api>, store: &Store) {
    let jobs = match store.list_downloads().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("list_downloads (pauses) failed: {e}");
            return;
        }
    };
    for job in jobs
        .into_iter()
        .filter(|j| j.status == DownloadJobStatus::Paused)
    {
        if let Some(hash) = &job.info_hash
            && let Ok(id) = TorrentIdOrHash::parse(hash)
            && let Err(e) = api.api_torrent_action_pause(id).await
        {
            tracing::warn!(job = %job.id, "pause action failed (continuing): {e}");
        }
    }
}

/// Add a claimed job's torrent and track it to completion, writing progress to
/// the row each `tick`. Stops if the row leaves `downloading` (e.g. a removal
/// landed). Errors are written to the row, not propagated.
/// Subdirectory of the watch folder where ingested files are moved so they're
/// not picked up twice. `.torrent` files stay here for `run_job` to read.
const WATCH_PROCESSED_DIR: &str = ".processed";

/// Ingest a **watched folder**: each dropped `.torrent` / `.magnet` file is
/// turned into a `downloads` queue row, then moved into `.processed/` so it's
/// not re-ingested (the `.torrent` stays there for [`resolve_source`] to read
/// its bytes; a `.magnet` is just an archived marker). Everything else is left
/// alone. Best-effort — a bad file is logged and skipped, never fatal.
///
/// Feeding the queue (rather than adding to librqbit directly) keeps **one**
/// download path: watch-dropped and daemon-enqueued jobs are identical
/// downstream (progress, removal, completion).
async fn scan_watch_dir(store: &Store, cfg: &Config) {
    let Some(watch) = cfg.watch_dir.as_ref() else {
        return;
    };
    let processed = watch.join(WATCH_PROCESSED_DIR);
    if let Err(e) = tokio::fs::create_dir_all(&processed).await {
        tracing::warn!(dir = %processed.display(), "cannot prepare watch dir: {e}");
        return;
    }

    let mut entries = match tokio::fs::read_dir(watch).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %watch.display(), "cannot read watch dir: {e}");
            return;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.')
            || !entry
                .file_type()
                .await
                .map(|t| t.is_file())
                .unwrap_or(false)
        {
            continue; // skip .processed/, hidden, and non-files
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        // Determine the queue source per file kind.
        let source = match ext.as_deref() {
            Some("magnet") => match tokio::fs::read_to_string(&path).await {
                Ok(s) if s.trim().starts_with("magnet:") => s.trim().to_string(),
                Ok(_) => {
                    tracing::warn!(file = %name, "watch: .magnet file has no magnet: URI; skipping");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(file = %name, "watch: cannot read .magnet: {e}");
                    continue;
                }
            },
            // The .torrent file is moved into .processed and its new path is the
            // queue source (resolve_source reads the bytes from there).
            Some("torrent") => processed.join(name).to_string_lossy().into_owned(),
            _ => continue, // not a torrent input
        };

        // Move the dropped file into .processed first, so a crash mid-enqueue
        // never re-ingests it (the .torrent's bytes must survive there).
        let dest = processed.join(name);
        if let Err(e) = tokio::fs::rename(&path, &dest).await {
            tracing::warn!(file = %name, "watch: cannot archive into .processed: {e}");
            continue;
        }

        let job = NewDownloadJob {
            acquirable_ref: format!("watch:{name}"),
            source,
            category: None,
            // Standalone/watch downloads land in the worker's default dir; no
            // separate complete dir (no importer in this path).
            incomplete_dir: Some(cfg.download_dir.to_string_lossy().into_owned()),
            complete_dir: None,
        };
        match store.enqueue(&job).await {
            Ok(row) => tracing::info!(file = %name, job = %row.id, "watch: enqueued"),
            Err(e) => tracing::warn!(file = %name, "watch: enqueue failed: {e}"),
        }
    }
}

/// Maximum HTTP redirects to follow when resolving a torrent source URL.
const MAX_SOURCE_REDIRECTS: usize = 8;

/// Resolve a download `source` into an [`AddTorrent`] librqbit can consume.
///
/// - `magnet:` (or a bare info-hash / non-http scheme) → pass straight through.
/// - `http(s)://` → fetch following redirects **manually**, because indexer and
///   Prowlarr `/download` links typically 30x-redirect either to a `magnet:`
///   URI (a non-HTTP scheme that reqwest's auto-follow rejects) or to the real
///   `.torrent` file. A redirect whose `Location` is a magnet becomes a magnet
///   add; a `2xx` body becomes a torrent-file-bytes add.
/// - A `2xx` body that is **not** a torrent file is rejected (SKADI-T-0497).
///
/// A 2xx is not automatically a torrent. A tracker behind Cloudflare answers a
/// bare `.torrent` link with an HTML challenge page, and a login-gated one
/// answers with a sign-in page — both 200. Those bodies used to go straight to
/// librqbit, which reported "error decoding torrent" without saying which link
/// or why; that is the exact error the production stack hit on 2026-09-06.
///
/// A torrent file is bencode and always begins with a dictionary, so a body that
/// does not start with `d` is not one. The error names the URL and what actually
/// came back, because the fix is always at the indexer end.
pub fn reject_non_torrent_body(url: &str, content_type: &str, bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.first() == Some(&b'd') {
        return Ok(());
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(120)]).replace('\n', " ");
    anyhow::bail!(
        "{url} did not return a torrent file (content-type {content_type:?}, {} bytes, starts with {:?}). \
         A challenge or login page is the usual cause: the link needs the indexer's own session, \
         which the worker does not have.",
        bytes.len(),
        head.trim()
    )
}

/// What a download's `source` string actually is (SKADI-T-0515).
///
/// Split out of [`resolve_source`] so the decision can be tested without a
/// network or a filesystem — the classification is pure, only the *fetch* is not,
/// and tangling them is why SKADI-T-0497's HTML-body rejection had no offline
/// coverage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TorrentSource {
    /// A `.torrent` on disk, from the watched folder.
    LocalFile(std::path::PathBuf),
    /// A magnet, info-hash, or anything else librqbit takes verbatim.
    Direct(String),
    /// An http(s) URL that has to be fetched (and may redirect).
    Http(String),
}

/// Classify `source` (SKADI-T-0515). Pure apart from one `is_file` check, which
/// is what distinguishes a local path from a magnet.
#[must_use]
pub fn classify_source(source: &str) -> TorrentSource {
    let is_http = source.starts_with("http://") || source.starts_with("https://");
    if is_http {
        return TorrentSource::Http(source.to_string());
    }
    // A path that exists is a file to read; anything else — a magnet, a bare
    // info-hash, a path that has since been moved — goes to librqbit verbatim,
    // which gives a better error for a missing file than we would.
    if std::path::Path::new(source).is_file() {
        return TorrentSource::LocalFile(std::path::PathBuf::from(source));
    }
    TorrentSource::Direct(source.to_string())
}

async fn resolve_source(source: &str) -> anyhow::Result<AddTorrent<'static>> {
    let source = match classify_source(source) {
        TorrentSource::LocalFile(path) => {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| anyhow::anyhow!("reading torrent file {}: {e}", path.display()))?;
            return Ok(AddTorrent::from_bytes(bytes));
        }
        TorrentSource::Direct(s) => return Ok(AddTorrent::from_url(s)),
        TorrentSource::Http(s) => s,
    };
    let source = source.as_str();

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let mut url = source.to_string();
    for _ in 0..MAX_SOURCE_REDIRECTS {
        let resp = client.get(&url).send().await?;
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("redirect {status} with no Location header"))?
                .to_string();
            // A redirect to a magnet (or any non-http scheme) is the terminal
            // answer — hand the magnet to librqbit.
            if !(location.starts_with("http://") || location.starts_with("https://")) {
                return Ok(AddTorrent::from_url(location));
            }
            url = location;
            continue;
        }
        let resp = resp.error_for_status()?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes().await?;
        // A 2xx body is not automatically a torrent (SKADI-T-0497). A tracker
        // behind Cloudflare answers a bare `.torrent` link with an HTML challenge
        // page, and a login-gated one answers with a sign-in page — both 200. We
        // used to hand those straight to librqbit, which reported "error decoding
        // torrent" with nothing to say which link or why. A torrent file is
        // bencode and always starts with a dictionary, so anything else is
        // rejected here, naming the URL and what actually came back.
        reject_non_torrent_body(&url, &content_type, &bytes)?;
        return Ok(AddTorrent::from_bytes(bytes));
    }
    anyhow::bail!("too many redirects (>{MAX_SOURCE_REDIRECTS}) resolving {source}")
}

/// The per-download output folder: each torrent gets its own folder under the
/// job's incomplete dir, named after the tracked item — `<incomplete_dir>/<item>`
/// — so files aren't dumped flat and mixed across downloads (operator request).
/// The `.torrent`'s internal name isn't known at add time for magnets, so the
/// item/release title skadi tracked is used. `None` (the session default) when the
/// job carries no incomplete dir.
fn job_output_folder(job: &DownloadJob) -> Option<String> {
    job.incomplete_dir.as_deref().map(|base| {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            sanitize_folder(&job.acquirable_ref)
        )
    })
}

/// Make a string safe as a single path component: map separators/reserved chars,
/// collapse whitespace, trim leading/trailing dots+spaces. Never empty.
fn sanitize_folder(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        let mapped = match c {
            '/' | '\\' => '-',
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => ' ',
            c => c,
        };
        if mapped == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(mapped);
            prev_space = false;
        }
    }
    let trimmed = out.trim().trim_matches('.').trim().to_string();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    api: Arc<Api>,
    store: Store,
    job: DownloadJob,
    tick: Duration,
    policy: SeedPolicy,
    stall_timeout_secs: Option<i64>,
    metadata_timeout_secs: Option<i64>,
    lease_secs: i64,
) {
    let id = job.id.clone();

    // Resolve per-category overrides once (SKADI-T-0215): a category can redirect the
    // completed-files save-path and override the seed policy. Absent/unknown category
    // → the worker's global settings.
    let mut job = job;
    let policy = match job.category.as_deref() {
        Some(name) => match store.get_category(name).await {
            Ok(Some(cat)) => {
                if cat.save_path.is_some() {
                    job.complete_dir = cat.save_path.clone();
                }
                effective_seed_policy(policy, &cat)
            }
            Ok(None) => policy,
            Err(e) => {
                tracing::warn!(job = %id, "get_category failed (using global policy): {e}");
                policy
            }
        },
        None => policy,
    };

    // Download (and seed) into the job's configured `incomplete_dir`; fall back
    // to the session's default output folder when the job carries none.
    // `overwrite: true` so a re-acquire **adopts** any leftover files from a prior
    // `Remove (keep files)` of the same content — librqbit re-checks them and only
    // fetches missing/changed pieces, instead of refusing with `File exists (os
    // error 17)` (SKADI-T-0173). "Remove keep files" exists precisely so the data
    // can be reused, not re-downloaded.
    let opts = Some(AddTorrentOptions {
        output_folder: job_output_folder(&job),
        overwrite: true,
        preferred_id: Some(next_torrent_id()),
        ..Default::default()
    });

    // Pre-resolve the source: indexer/Prowlarr "download" links commonly
    // 30x-redirect to the real `.torrent` or to a `magnet:`, which librqbit's
    // own add-by-URL does not follow (it surfaced the 301 as an error).
    let add = match resolve_source(&job.source).await {
        Ok(a) => a,
        Err(e) => {
            mark_error(&store, &id, &format!("resolving source: {e}")).await;
            return;
        }
    };

    // `api_add_torrent` blocks until magnet metadata resolves. Shallow-seed torrents
    // (typical for audiobooks) can take a long time to find a peer; a positive
    // `metadata_timeout_secs` caps that and fails the row, but the default is no
    // timeout — wait as long as it takes rather than wrongly failing a slow grab
    // (SKADI-T-0307). The row stays `downloading` with a NULL lease throughout, so
    // `reclaim_expired_downloads` leaves it alone (no double-add) however long it
    // waits. A genuinely dead magnet then holds a `max_active` slot until removed —
    // set a positive cap to bound that.
    let add_fut = api.api_add_torrent(add, opts);
    let added = match metadata_timeout_secs {
        Some(t) if t > 0 => {
            match tokio::time::timeout(Duration::from_secs(t as u64), add_fut).await {
                Ok(r) => r,
                Err(_) => {
                    mark_error(
                        &store,
                        &id,
                        &format!(
                            "torrent metadata did not resolve within {t}s (no peers / dead magnet?)"
                        ),
                    )
                    .await;
                    return;
                }
            }
        }
        _ => add_fut.await,
    };
    let info_hash = match added {
        Ok(resp) => resp.details.info_hash,
        Err(e) => {
            mark_error(&store, &id, &format!("add failed: {e}")).await;
            return;
        }
    };
    let torrent_id = match TorrentIdOrHash::parse(&info_hash) {
        Ok(t) => t,
        Err(e) => {
            mark_error(&store, &id, &format!("bad info_hash {info_hash}: {e}")).await;
            return;
        }
    };
    // Ensure the torrent is live: a resumed job (or a re-add of a paused one) may
    // still be paused in librqbit. (SKADI-T-0168)
    start_if_paused(&api, torrent_id, &id, "").await;
    tracing::info!(job = %id, info_hash = %info_hash, "tracking download");

    track(
        api,
        store,
        id,
        job,
        torrent_id,
        info_hash,
        tick,
        false,
        policy,
        stall_timeout_secs,
        lease_secs,
    )
    .await;
}

/// Issue `start` only if librqbit reports the torrent `Paused` (SKADI-T-0392).
///
/// `start` is NOT idempotent while a torrent is `Initializing`: librqbit's
/// `ManagedTorrent::start` spawns a *second* `initialize_and_start` task for an
/// initializing torrent, so it gets hash-checked twice — the second check queues
/// on the init semaphore behind everything else (doubling the re-hash load on the
/// NAS), and whichever finishes last fails renaming its `.bitv.tmp` ("error
/// storing initial check bitfield"). Every fresh `api_add_torrent` and every
/// session-restored torrent starts out `Initializing`, so the old unconditional
/// `start` after add double-checked *every* seed on every restart. `Live` needs
/// nothing; `Error` is left for `track` to report.
async fn start_if_paused(api: &Api, torrent_id: TorrentIdOrHash, job: &str, ctx: &str) {
    let paused = match api.api_stats_v1(torrent_id) {
        Ok(s) => matches!(s.state, TorrentStatsState::Paused),
        Err(e) => {
            tracing::warn!(job = %job, "{ctx}stats before start failed (continuing): {e}");
            return;
        }
    };
    if !paused {
        return;
    }
    if let Err(e) = api.api_torrent_action_start(torrent_id).await {
        tracing::warn!(job = %job, "{ctx}start action failed (continuing): {e}");
    }
}

/// Re-attach a `Completed` (seeding) row to librqbit on worker startup
/// (SKADI-T-0172). librqbit's JSON session persistence only flushes on graceful
/// shutdown, so a hard restart (e.g. `docker compose` recreate → SIGKILL) can drop
/// torrents it was seeding — leaving the seed with no live metrics and unremovable
/// (`forget` fails, no handle). This reconciles the DB against librqbit: if the
/// torrent is still loaded, just resume tracking; otherwise re-add it from source
/// into its `incomplete_dir` and seed.
///
/// Best-effort: a resolve/add failure logs and stops tracking this seed — it must
/// **never** flip an already-completed row to `error` over a transient hiccup.
async fn reattach_seed(
    api: Arc<Api>,
    store: Store,
    job: DownloadJob,
    tick: Duration,
    policy: SeedPolicy,
    metadata_timeout_secs: Option<i64>,
) {
    let id = job.id.clone();
    let Some(info_hash) = job.info_hash.clone() else {
        return; // can't reconcile a completed row with no resolved hash
    };
    let torrent_id = match TorrentIdOrHash::parse(&info_hash) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(job = %id, "re-attach: bad info_hash {info_hash}: {e}");
            return;
        }
    };

    // Already loaded (session persistence survived) → just resume + track.
    let already_loaded = api.api_stats_v1(torrent_id).is_ok();
    if !already_loaded {
        // Not loaded — re-add from source into the same output folder so librqbit
        // re-checks the existing files and seeds them (no re-download). The seed's
        // files already exist on disk, so `overwrite: true` is REQUIRED — otherwise
        // librqbit refuses ("File exists (os error 17)") and the seed never
        // re-attaches (SKADI-T-0172).
        let opts = Some(AddTorrentOptions {
            output_folder: job_output_folder(&job),
            overwrite: true,
            preferred_id: Some(next_torrent_id()),
            ..Default::default()
        });
        let add = match resolve_source(&job.source).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(job = %id, "re-attach: resolving source failed (leaving completed): {e}");
                return;
            }
        };
        let add_fut = api.api_add_torrent(add, opts);
        let added = match metadata_timeout_secs {
            Some(t) if t > 0 => {
                match tokio::time::timeout(Duration::from_secs(t as u64), add_fut).await {
                    Ok(r) => r,
                    Err(_) => {
                        tracing::warn!(job = %id, "re-attach: metadata timeout (leaving completed)");
                        return;
                    }
                }
            }
            _ => add_fut.await,
        };
        match added {
            // librqbit answers `Ok` with whatever torrent it thinks matches; make
            // sure it is ours before tracking by hash (see `NEXT_TORRENT_ID`).
            Ok(r) if !r.details.info_hash.eq_ignore_ascii_case(&info_hash) => {
                tracing::warn!(
                    job = %id,
                    got = %r.details.info_hash,
                    "re-attach: add returned a different torrent (leaving completed)"
                );
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(job = %id, "re-attach: add failed (leaving completed): {e}");
                return;
            }
        }
    }

    start_if_paused(&api, torrent_id, &id, "re-attach: ").await;
    tracing::info!(
        job = %id,
        info_hash = %info_hash,
        via = if already_loaded { "session" } else { "re-add" },
        "re-attached seed; tracking"
    );
    // Seeding rows aren't lease-tracked (`lease_secs` unused on the seeding path).
    track(
        api, store, id, job, torrent_id, info_hash, tick, true, policy, None, 0,
    )
    .await;
}

/// Track a live torrent to the row's terminal state, writing progress each `tick`.
/// Shared by the download path ([`run_job`], `seeding=false`) and the startup seed
/// re-attach ([`reattach_seed`], `seeding=true`). Once the download finishes we keep
/// tracking while it **seeds**, writing live upload/ratio/peers so the Downloads
/// view shows seeding torrents as "under active management" (SKADI-T-0169). Tracking
/// ends when the row leaves the tracked states (pause / remove).
#[allow(clippy::too_many_arguments)]
async fn track(
    api: Arc<Api>,
    store: Store,
    id: String,
    job: DownloadJob,
    torrent_id: TorrentIdOrHash,
    info_hash: String,
    tick: Duration,
    mut seeding: bool,
    policy: SeedPolicy,
    stall_timeout_secs: Option<i64>,
    lease_secs: i64,
) {
    // Stall tracking (SKADI-T-0213): remember the last progress and when it changed,
    // so a no-progress-no-peers stretch flips the row to `stalled` (and back).
    let mut last_progress = job.progress_bytes;
    let mut last_progress_at = std::time::Instant::now();
    let mut stalled_marked = job.status == DownloadJobStatus::Stalled;
    loop {
        // Re-read the row each tick: confirm it's still in a tracked state and keep
        // the latest `completed_at` for the seed-time check.
        let current = match store.get_download(&id).await {
            // A downloading row may be flagged `stalled` (still actively tracked).
            Ok(Some(j))
                if !seeding
                    && matches!(
                        j.status,
                        DownloadJobStatus::Downloading | DownloadJobStatus::Stalled
                    ) =>
            {
                Some(j)
            }
            Ok(Some(j)) if seeding && j.status == DownloadJobStatus::Completed => Some(j),
            Ok(Some(j)) => {
                tracing::info!(job = %id, status = ?j.status, "row left tracked state; stop tracking");
                return;
            }
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(job = %id, "get_download failed: {e}");
                None
            }
        };

        let stats = match api.api_stats_v1(torrent_id) {
            Ok(s) => s,
            Err(e) => {
                // A seed we couldn't keep a handle on: log + stop, but don't fail
                // an already-completed row. A fresh download losing stats is a real
                // error.
                if seeding {
                    tracing::warn!(job = %id, "seed stats unavailable; stop tracking: {e}");
                } else {
                    mark_error(&store, &id, &format!("stats: {e}")).await;
                }
                return;
            }
        };

        let progress = progress_from_stats(&stats, &info_hash);
        if let Err(e) = store.update_progress(&id, &progress).await {
            tracing::warn!(job = %id, "update_progress failed: {e}");
        }

        if stats.finished && !seeding {
            // The absolute paths in the incomplete tree (where librqbit keeps
            // seeding from). Hardlink them into the complete dir if configured;
            // report whichever paths the importer should hardlink into the
            // library.
            let incomplete_files = collect_files(&api, torrent_id);
            let report = place_complete(
                incomplete_files,
                job.incomplete_dir.as_deref(),
                job.complete_dir.as_deref(),
            )
            .await;
            match store.mark_complete(&id, &report).await {
                Ok(()) => {
                    tracing::info!(job = %id, files = report.len(), "download complete; seeding")
                }
                Err(e) => tracing::error!(job = %id, "mark_complete failed: {e}"),
            }
            // Keep going — track the seed (don't return).
            seeding = true;
        }

        // Stall detection (SKADI-T-0213): while still downloading, flip to `stalled`
        // after a no-progress-and-no-peers stretch, and back to `downloading` once it
        // recovers. A no-op unless `stall_timeout_secs` is configured.
        if !seeding {
            // Refresh the claim lease so this live download isn't reclaimed as
            // abandoned (SKADI-T-0214).
            if let Err(e) = store.heartbeat(&id, lease_secs).await {
                tracing::warn!(job = %id, "heartbeat failed: {e}");
            }
            if stats.progress_bytes as i64 > last_progress {
                last_progress = stats.progress_bytes as i64;
                last_progress_at = std::time::Instant::now();
                if stalled_marked {
                    let _ = store.set_download_stalled(&id, false).await;
                    stalled_marked = false;
                }
            } else if !stalled_marked {
                let no_progress_secs = last_progress_at.elapsed().as_secs() as i64;
                let peers = progress.peers.unwrap_or(0);
                if is_stalled(no_progress_secs, peers, stall_timeout_secs) {
                    tracing::info!(job = %id, no_progress_secs, "download stalled (no progress, no peers)");
                    let _ = store.set_download_stalled(&id, true).await;
                    stalled_marked = true;
                }
            }
        }

        // Seed-policy enforcement (SKADI-T-0210): once seeding, stop or remove the
        // torrent when its ratio/time limit is reached. `verdict` is the pure,
        // unit-tested decision (SKADI-T-0209); here we just act on it.
        if seeding && !policy.is_unlimited() {
            let uploaded = stats.uploaded_bytes as i64;
            let downloaded = stats.total_bytes as i64;
            let seeded_secs = current
                .as_ref()
                .and_then(|j| j.completed_at)
                .map(|c| (Utc::now() - c).num_seconds())
                .unwrap_or(0);
            match policy.verdict(uploaded, downloaded, seeded_secs) {
                SeedVerdict::Continue => {}
                SeedVerdict::Stop => {
                    tracing::info!(job = %id, uploaded, seeded_secs, "seed limit reached; stopping seed");
                    // Drop the torrent from the session (keeps the files) so it stops
                    // seeding, then mark the row terminal.
                    let _ = api.api_torrent_action_forget(torrent_id).await;
                    if let Err(e) = store.mark_seeded(&id).await {
                        tracing::warn!(job = %id, "mark_seeded failed: {e}");
                    }
                    return;
                }
                SeedVerdict::Remove => {
                    tracing::info!(job = %id, uploaded, seeded_secs, "seed limit reached; removing");
                    // Let the removal sweep tear it down (forget + optional delete).
                    if let Err(e) = store.request_remove(&id, false).await {
                        tracing::warn!(job = %id, "request_remove (seed limit) failed: {e}");
                    }
                    return;
                }
            }
        }

        if !seeding && matches!(stats.state, TorrentStatsState::Error) {
            let reason = stats
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".into());
            mark_error(&store, &id, &reason).await;
            return;
        }

        tokio::time::sleep(tick).await;
    }
}

/// Best-effort `mark_error` (logs if the write itself fails).
async fn mark_error(store: &Store, id: &str, reason: &str) {
    tracing::warn!(job = %id, "download error: {reason}");
    if let Err(e) = store.mark_error(id, reason).await {
        tracing::error!(job = %id, "mark_error write failed: {e}");
    }
}

/// Enumerate a finished torrent's absolute output file paths (the locations the
/// daemon's importer will hardlink). Returns an empty list if details fail.
/// Where the liveness marker lives. Container-local on purpose: this answers
/// "is this process alive", so it must not depend on the shared library mount —
/// which is one of the things that can be broken while the process is fine.
/// Overridable for tests and for anyone running the worker outside a container.
pub fn heartbeat_path() -> std::path::PathBuf {
    std::env::var("SKADI_WORKER_HEARTBEAT_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("skadi-worker-heartbeat"))
}

/// Refresh the on-disk liveness marker the container healthcheck watches
/// (SKADI-T-0479).
///
/// Deliberately not under the worker's state dir: that path is resolved from
/// settings and defaults to a container-local directory that is *not* the mounted
/// one, so a marker written there was invisible to a healthcheck reading the
/// mount — which is exactly the mismatch that made a healthy worker look dead.
///
/// Best-effort: a worker that cannot write its marker is still a working worker,
/// and failing a tick over it would be worse than a stale healthcheck.
fn touch_heartbeat_file() {
    let path = heartbeat_path();
    // Rewriting the file is the portable way to bump mtime; the content is the
    // timestamp so an operator reading it by hand sees something useful.
    let _ = std::fs::write(&path, chrono::Utc::now().to_rfc3339());
}

fn collect_files(api: &Arc<Api>, torrent_id: TorrentIdOrHash) -> Vec<String> {
    match api.api_torrent_details(torrent_id) {
        Ok(details) => {
            let output_folder = details.output_folder;
            details
                .files
                .unwrap_or_default()
                .into_iter()
                .map(|f| file_abs_path(&output_folder, &f.components))
                .collect()
        }
        Err(e) => {
            tracing::warn!("torrent details on finish failed: {e}");
            Vec::new()
        }
    }
}

/// Join a torrent's `output_folder` with a file's path `components` into one
/// absolute path (the location the daemon's importer will hardlink).
pub fn file_abs_path(output_folder: &str, components: &[String]) -> String {
    let mut p = PathBuf::from(output_folder);
    for c in components {
        p.push(c);
    }
    p.to_string_lossy().into_owned()
}

/// On completion, hardlink the finished files from the incomplete tree into the
/// configured `complete_dir` (mirroring each path relative to `incomplete_dir`)
/// and return the paths the importer should hardlink into the library.
///
/// Best effort: with no `incomplete`/`complete` config, or if a hardlink fails,
/// the incomplete path is reported instead — import still works, the file just
/// isn't mirrored into the clean "complete" view. Seeding is undisturbed:
/// hardlinks don't move or modify librqbit's files.
async fn place_complete(
    incomplete_files: Vec<String>,
    incomplete_dir: Option<&str>,
    complete_dir: Option<&str>,
) -> Vec<String> {
    let (Some(inc), Some(comp)) = (incomplete_dir, complete_dir) else {
        return incomplete_files;
    };
    let (inc, comp) = (inc.to_string(), comp.to_string());
    let fallback = incomplete_files.clone();
    tokio::task::spawn_blocking(move || hardlink_into_complete(incomplete_files, &inc, &comp))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("place_complete task panicked: {e}");
            fallback
        })
}

/// The reported files of a job that live under its `complete_dir` — the mirror
/// [`hardlink_into_complete`] made — and only those: a report that fell back to
/// an incomplete path (or any path outside the mirror) is never deleted here,
/// because librqbit's own delete is responsible for its tree and nothing else
/// may be touched. Pure for unit testing.
#[must_use]
pub fn complete_mirror_paths(files: &[String], complete_dir: Option<&str>) -> Vec<PathBuf> {
    let Some(comp) = complete_dir.filter(|c| !c.trim().is_empty()) else {
        return Vec::new();
    };
    let comp = std::path::Path::new(comp);
    files
        .iter()
        .map(PathBuf::from)
        .filter(|p| {
            p.strip_prefix(comp)
                .is_ok_and(|rel| !rel.as_os_str().is_empty())
        })
        .collect()
}

/// Blocking: delete a removed job's mirrored files under `complete_dir` and
/// prune the directories that emptied, stopping at `complete_dir` itself.
/// Returns how many files were removed. Best-effort — a path already gone is
/// not an error, and a failure is logged rather than propagated.
fn remove_complete_mirror(files: &[String], complete_dir: Option<&str>) -> usize {
    let paths = complete_mirror_paths(files, complete_dir);
    let Some(comp) = complete_dir.map(std::path::Path::new) else {
        return 0;
    };
    let mut removed = 0;
    for p in &paths {
        match std::fs::remove_file(p) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %p.display(), "removing mirrored file failed: {e}"),
        }
        // Prune emptied parents up to (never including) the complete dir.
        let mut dir = p.parent();
        while let Some(d) = dir {
            if d == comp || d.strip_prefix(comp).is_err() {
                break;
            }
            if std::fs::remove_dir(d).is_err() {
                break; // not empty (or gone) — stop climbing
            }
            dir = d.parent();
        }
    }
    removed
}

/// Map an absolute path under `incomplete_dir` to its mirror under
/// `complete_dir`. `None` if `abs` isn't under `incomplete_dir`.
pub fn map_to_complete(abs: &str, incomplete_dir: &str, complete_dir: &str) -> Option<String> {
    let rel = std::path::Path::new(abs)
        .strip_prefix(incomplete_dir)
        .ok()?;
    Some(
        std::path::Path::new(complete_dir)
            .join(rel)
            .to_string_lossy()
            .into_owned(),
    )
}

/// Blocking: hardlink each file into the complete dir, returning the path to
/// report per file (the complete path on success, else the original).
fn hardlink_into_complete(
    files: Vec<String>,
    incomplete_dir: &str,
    complete_dir: &str,
) -> Vec<String> {
    files
        .into_iter()
        .map(|abs| {
            let Some(dest) = map_to_complete(&abs, incomplete_dir, complete_dir) else {
                return abs; // not under incomplete_dir — report as-is
            };
            if let Some(parent) = std::path::Path::new(&dest).parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                tracing::warn!("create_dir_all {parent:?} failed: {e}; using incomplete path");
                return abs;
            }
            match std::fs::hard_link(&abs, &dest) {
                Ok(()) => dest,
                // Already placed (e.g. a re-run) — treat as done.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => dest,
                Err(e) => {
                    tracing::warn!("hardlink {abs} -> {dest} failed: {e}; using incomplete path");
                    abs
                }
            }
        })
        .collect()
}

/// Map librqbit's [`TorrentStats`] into a [`DownloadProgress`] with live metrics
/// (SKADI-T-0166). librqbit's `Speed.mbps` is actually **MiB/s** (its Display is
/// `MiB/s`), so we convert to bytes/sec. `uploaded_bytes` is top-level (drives the
/// seed ratio). Per-torrent ETA isn't public in librqbit, so we derive it from
/// remaining bytes ÷ download speed. Metric fields are `Some` only while the
/// torrent is `live` (cleared to `None` otherwise so stale speeds don't linger).
/// librqbit's `Speed.mbps` in bytes/sec (SKADI-T-0166, SKADI-T-0322).
///
/// The field is named `mbps` but is **MiB/s** — its `Display` prints `MiB/s`.
/// Reading it as megabits would under-report by ~8x, which looks like a slow seed
/// rather than a bug. A negative or NaN input clamps to 0 so a bad reading cannot
/// become a huge positive through the cast.
fn mibps_to_bps(mib_s: f64) -> i64 {
    let bytes = (mib_s * 1024.0 * 1024.0).round();
    if bytes.is_finite() && bytes > 0.0 {
        bytes as i64
    } else {
        0
    }
}

fn progress_from_stats(stats: &TorrentStats, info_hash: &str) -> DownloadProgress {
    let progress_bytes = stats.progress_bytes as i64;
    let total_bytes = stats.total_bytes as i64;
    let mut p = DownloadProgress {
        progress_bytes,
        total_bytes,
        info_hash: Some(info_hash.to_string()),
        uploaded_bytes: Some(stats.uploaded_bytes as i64),
        // What the client is actually doing (SKADI-T-0394). Without this the
        // daemon cannot tell a transfer queued behind the session's hash checks
        // from a live one that has found no peers, and the stall watch failed 46
        // releases for sitting at 0% while they were merely waiting their turn.
        client_state: Some(state_str(stats.state).to_string()),
        ..Default::default()
    };
    if let Some(live) = stats.live.as_ref() {
        let down = mibps_to_bps(live.download_speed.mbps);
        p.down_speed_bps = Some(down);
        p.up_speed_bps = Some(mibps_to_bps(live.upload_speed.mbps));
        p.peers = Some(live.snapshot.peer_stats.live as i32);
        p.peers_seen = Some(live.snapshot.peer_stats.seen as i32);
        p.eta_seconds = (down > 0 && total_bytes > progress_bytes)
            .then(|| (total_bytes - progress_bytes) / down);
    }
    p
}

/// Human-readable librqbit transfer state, for logging.
#[must_use]
pub fn state_str(s: TorrentStatsState) -> &'static str {
    match s {
        TorrentStatsState::Initializing => "initializing",
        TorrentStatsState::Live => "live",
        TorrentStatsState::Paused => "paused",
        TorrentStatsState::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn complete_mirror_paths_keeps_only_files_under_the_complete_dir() {
        let files = vec![
            "/dl/complete/Show S01/ep.mkv".to_string(),
            "/dl/incomplete/Show S01/ep.mkv".to_string(),
            "/dl/complete".to_string(),
            "/elsewhere/ep.mkv".to_string(),
        ];
        let got = complete_mirror_paths(&files, Some("/dl/complete"));
        assert_eq!(got, vec![PathBuf::from("/dl/complete/Show S01/ep.mkv")]);
        assert!(complete_mirror_paths(&files, None).is_empty());
        assert!(complete_mirror_paths(&files, Some("")).is_empty());
    }

    /// The SKADI-T-0592 leftover: a delete-data removal also drops the mirror
    /// and prunes its emptied folder, but never climbs into the complete dir
    /// itself or touches a sibling download.
    #[test]
    fn remove_complete_mirror_deletes_files_and_prunes_emptied_dirs() {
        let tmp = std::env::temp_dir().join(format!(
            "skadi-worker-mirror-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let comp = tmp.join("complete");
        let mine = comp.join("Fake Release");
        let other = comp.join("Other Release");
        std::fs::create_dir_all(mine.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(mine.join("sub").join("junk.exe"), b"x").unwrap();
        std::fs::write(mine.join("readme.nfo"), b"x").unwrap();
        std::fs::write(other.join("keep.mkv"), b"x").unwrap();
        let files = [
            mine.join("sub")
                .join("junk.exe")
                .to_string_lossy()
                .into_owned(),
            mine.join("readme.nfo").to_string_lossy().into_owned(),
            other.join("keep.mkv").to_string_lossy().into_owned(), // not this job's? still listed — it IS under complete_dir, so it goes
        ];
        // A job only ever reports its own files; the third entry proves the
        // routine trusts the report rather than the folder name.
        let removed = remove_complete_mirror(&files[..2], Some(&comp.to_string_lossy()));
        assert_eq!(removed, 2);
        assert!(!mine.exists(), "the emptied release folder is pruned");
        assert!(
            other.join("keep.mkv").exists(),
            "a sibling download is untouched"
        );
        assert!(comp.exists(), "the complete dir itself is never removed");
        // Idempotent: already-gone files are not errors.
        assert_eq!(
            remove_complete_mirror(&files[..2], Some(&comp.to_string_lossy())),
            0
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn tracker_list_parses_separators_and_drops_junk() {
        let got = parse_tracker_list(
            "udp://a:1/announce, http://b/announce\n  udp://a:1/announce \\\n ftp://nope wss://c/ann",
        );
        assert_eq!(
            got,
            vec!["udp://a:1/announce", "http://b/announce", "wss://c/ann"]
        );
        assert!(parse_tracker_list("").is_empty());
        // The shipped default parses in full.
        let d = parse_tracker_list(skadi_config::DEFAULT_EXTRA_TRACKERS);
        assert!(d.len() >= 6, "{d:?}");
        assert_eq!(extra_tracker_urls(&d).len(), d.len());
    }

    #[test]
    fn forwarded_port_file_parses_a_bare_port_only() {
        assert_eq!(parse_forwarded_port("51413\n"), Some(51413));
        assert_eq!(parse_forwarded_port("  6881 "), Some(6881));
        assert_eq!(
            parse_forwarded_port("0"),
            None,
            "gluetun writes 0 before it has a port"
        );
        assert_eq!(parse_forwarded_port(""), None);
        assert_eq!(parse_forwarded_port("not a port"), None);
    }

    use super::*;

    #[test]
    fn a_challenge_page_is_not_accepted_as_a_torrent() {
        // SKADI-T-0497: the production symptom was librqbit's bare "error
        // decoding torrent" for a link that had actually returned a Cloudflare
        // interstitial with HTTP 200.
        let html = b"<html><head><title>Just a moment...</title></head></html>";
        let err = reject_non_torrent_body("https://tracker.test/dl/1.torrent", "text/html", html)
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not return a torrent file"), "{err}");
        assert!(err.contains("dl/1.torrent"), "names the link: {err}");
        assert!(err.contains("text/html"), "names what came back: {err}");

        // A real torrent starts with a bencode dictionary and is accepted.
        assert!(reject_non_torrent_body("u", "application/x-bittorrent", b"d8:announce").is_ok());
        // An empty body is not a torrent either.
        assert!(reject_non_torrent_body("u", "", b"").is_err());
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        // A counter, not a timestamp: the clock is coarse enough that two tests
        // starting together mint the same path (SKADI-T-0527/0530).
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "skadi-worker-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    async fn tmp_store() -> Store {
        let path = tmp_dir("db").join("worker.db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    fn watch_cfg(watch: PathBuf, download: PathBuf) -> Config {
        Config {
            download_dir: download,
            state_dir: PathBuf::from("/tmp/state"),
            port_range: 16881..16891,
            database_url: "sqlite://./x.db".into(),
            worker_id: "test".into(),
            poll_interval: Duration::from_secs(1),
            tick_interval: Duration::from_secs(1),
            watch_dir: Some(watch),
            migrate: false,
            seed_policy: SeedPolicy::unlimited(),
            down_limit_bps: None,
            up_limit_bps: None,
            max_active: None,
            stall_timeout_secs: None,
            metadata_timeout_secs: None,
            lease_secs: 120,
            extra_trackers: Vec::new(),
        }
    }

    #[test]
    fn config_from_view_uses_table_then_registry_defaults() {
        // Snapshot overrides a couple of worker keys; the rest fall back.
        let view = skadi_config::ConfigView::from_pairs([
            ("worker.download_dir".to_string(), "/mnt/dl".to_string()),
            ("worker.port_lo".to_string(), "30000".to_string()),
            ("worker.watch_dir".to_string(), "/mnt/watch".to_string()),
            (
                "worker.metadata_timeout_secs".to_string(),
                "1800".to_string(),
            ),
        ]);
        let cfg = Config::from_view(&view, "sqlite://x".to_string(), true).unwrap();

        // From the table.
        assert_eq!(cfg.download_dir, PathBuf::from("/mnt/dl"));
        assert_eq!(cfg.port_range.start, 30000);
        assert_eq!(cfg.watch_dir, Some(PathBuf::from("/mnt/watch")));
        // A positive metadata timeout parses to Some (SKADI-T-0307).
        assert_eq!(cfg.metadata_timeout_secs, Some(1800));
        // Tier-0 / control flags passed through.
        assert_eq!(cfg.database_url, "sqlite://x");
        assert!(cfg.migrate);

        // Registry defaults for unset keys (parity with from_env).
        assert_eq!(cfg.port_range.end, 16891);
        assert_eq!(cfg.worker_id, "skadi-worker");
        assert_eq!(cfg.poll_interval, Duration::from_secs(3));
        assert_eq!(cfg.tick_interval, Duration::from_secs(1));

        // An empty view yields all defaults (standalone / no daemon).
        let empty = Config::from_view(
            &skadi_config::ConfigView::default(),
            "sqlite://y".to_string(),
            false,
        )
        .unwrap();
        assert_eq!(
            empty.download_dir,
            PathBuf::from("/data/downloads/complete")
        );
        assert_eq!(empty.watch_dir, None);
        assert_eq!(empty.port_range, 16881..16891);
        // Seed policy defaults to unlimited (SKADI-T-0209).
        assert!(empty.seed_policy.is_unlimited());
        // Metadata timeout defaults to no timeout — wait indefinitely (SKADI-T-0307).
        assert_eq!(empty.metadata_timeout_secs, None);
    }

    #[test]
    fn config_from_view_parses_seed_policy() {
        let view = skadi_config::ConfigView::from_pairs([
            ("worker.seed_ratio".to_string(), "1.5".to_string()),
            ("worker.seed_time_mins".to_string(), "120".to_string()),
            ("worker.seed_action".to_string(), "remove".to_string()),
        ]);
        let cfg = Config::from_view(&view, "sqlite://x".to_string(), false).unwrap();
        assert_eq!(cfg.seed_policy.ratio_limit, Some(1.5));
        assert_eq!(cfg.seed_policy.time_limit_secs, Some(7200)); // 120 min
        assert_eq!(cfg.seed_policy.action, SeedAction::Remove);
        assert!(!cfg.seed_policy.is_unlimited());
        // Download is uncapped by default; upload defaults to the opinionated
        // ~500 KB/s cap (SKADI-T-0211/seeding policy, raised from 100 KB/s in
        // SKADI-T-0516) for unset keys.
        assert_eq!(cfg.down_limit_bps, None);
        assert_eq!(cfg.up_limit_bps, Some(500_000));
    }

    #[test]
    fn config_from_view_parses_bandwidth_caps() {
        let view = skadi_config::ConfigView::from_pairs([
            ("worker.down_limit_bps".to_string(), "5000000".to_string()),
            ("worker.up_limit_bps".to_string(), "0".to_string()),
        ]);
        let cfg = Config::from_view(&view, "sqlite://x".to_string(), false).unwrap();
        assert_eq!(cfg.down_limit_bps, Some(5_000_000));
        assert_eq!(cfg.up_limit_bps, None, "0 = unlimited");
    }

    #[test]
    fn bps_limit_zero_is_unlimited_and_clamps_to_u32() {
        assert_eq!(bps_limit(0), None);
        assert_eq!(bps_limit(1_000_000), Some(1_000_000));
        // Above u32::MAX clamps rather than overflowing.
        assert_eq!(bps_limit(u64::MAX), Some(u32::MAX));
    }

    #[test]
    fn effective_seed_policy_overrides_per_axis() {
        let global = SeedPolicy::from_config(1.0, 60, SeedAction::Stop); // 1.0x / 60min / stop
        // No overrides → global unchanged.
        let same = effective_seed_policy(global, &DownloadCategory::named("x"));
        assert_eq!(same, global);
        // Per-axis override: ratio + action set, time inherited.
        let cat = DownloadCategory {
            seed_ratio: Some(3.0),
            seed_action: Some("remove".into()),
            ..DownloadCategory::named("movies")
        };
        let eff = effective_seed_policy(global, &cat);
        assert_eq!(eff.ratio_limit, Some(3.0));
        assert_eq!(eff.time_limit_secs, Some(3600), "inherited 60min");
        assert_eq!(eff.action, SeedAction::Remove);
        // A category `0` means unlimited on that axis (not "inherit").
        let unlimited = DownloadCategory {
            seed_ratio: Some(0.0),
            seed_time_mins: Some(0),
            ..DownloadCategory::named("a")
        };
        let eff = effective_seed_policy(global, &unlimited);
        assert!(eff.is_unlimited());
    }

    #[test]
    fn is_stalled_requires_no_peers_no_progress_and_a_timeout() {
        // Disabled (None / 0) → never stalled.
        assert!(!is_stalled(9999, 0, None));
        assert!(!is_stalled(9999, 0, Some(0)));
        // Peers present → not stalled even with no progress.
        assert!(!is_stalled(9999, 3, Some(60)));
        // No peers but not past the timeout yet → not stalled.
        assert!(!is_stalled(59, 0, Some(60)));
        // No peers and past the timeout → stalled.
        assert!(is_stalled(60, 0, Some(60)));
        assert!(is_stalled(120, 0, Some(60)));
    }

    #[test]
    fn config_from_view_parses_stall_timeout() {
        let view = skadi_config::ConfigView::from_pairs([(
            "worker.stall_timeout_secs".to_string(),
            "90".to_string(),
        )]);
        let cfg = Config::from_view(&view, "sqlite://x".to_string(), false).unwrap();
        assert_eq!(cfg.stall_timeout_secs, Some(90));
        let empty = Config::from_view(
            &skadi_config::ConfigView::default(),
            "sqlite://y".to_string(),
            false,
        )
        .unwrap();
        assert_eq!(empty.stall_timeout_secs, None);
        // Lease length defaults to the registry value (SKADI-T-0214).
        assert_eq!(empty.lease_secs, 120);
    }

    #[test]
    fn live_info_hashes_keeps_seeding_and_drops_terminal() {
        // SKADI-T-0480. `Completed` means "downloaded and still seeding", so it is
        // live; only `Seeded` (limit reached, detached) and `Removed` are terminal.
        let job = |id: &str, hash: Option<&str>, status| DownloadJob {
            id: id.into(),
            acquirable_ref: "a".into(),
            source: "magnet:?x".into(),
            category: None,
            status,
            info_hash: hash.map(str::to_string),
            progress_bytes: 0,
            total_bytes: 0,
            down_speed_bps: None,
            up_speed_bps: None,
            uploaded_bytes: None,
            peers: None,
            peers_seen: None,
            eta_seconds: None,
            files: Vec::new(),
            error: None,
            worker_id: None,
            delete_data: false,
            incomplete_dir: None,
            complete_dir: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            completed_at: None,
            lease_expires_at: None,
            client_state: None,
        };
        let jobs = vec![
            job("1", Some("AAAA"), DownloadJobStatus::Downloading),
            job("2", Some("bbbb"), DownloadJobStatus::Completed),
            job("3", Some("cccc"), DownloadJobStatus::Seeded),
            job("4", Some("dddd"), DownloadJobStatus::Removed),
            job("5", Some("eeee"), DownloadJobStatus::Stalled),
            // No hash: nothing in the session to match, so it cannot keep one alive.
            job("6", None, DownloadJobStatus::Queued),
        ];
        let live = live_info_hashes(&jobs);
        // Hashes are compared lowercased — librqbit and our rows disagree on case.
        assert!(live.contains("aaaa"), "downloading is live");
        assert!(live.contains("bbbb"), "completed means still seeding");
        assert!(live.contains("eeee"), "stalled is still tracked");
        assert!(!live.contains("cccc"), "seeded is terminal");
        assert!(!live.contains("dddd"), "removed is terminal");
        assert_eq!(live.len(), 3);
    }

    /// SKADI-T-0322: the operator reported "the downloader doesn't show upload
    /// rate". The UI hides `↑` when the value is 0/None, so an unpopulated or
    /// mis-scaled `up_speed_bps` is indistinguishable from a genuinely idle seed —
    /// which is why the absence went unnoticed for months. The conversion is the
    /// part that can silently be wrong, so it is covered here.
    ///
    /// librqbit's `Speed.mbps` is **MiB/s** despite the name (its `Display` prints
    /// `MiB/s`), so a naive read as megabits would under-report by ~8x — small
    /// enough to look like a slow seed rather than a bug.
    #[test]
    fn librqbit_speeds_convert_from_mib_per_second() {
        assert_eq!(mibps_to_bps(0.5), 512 * 1024);
        assert_eq!(mibps_to_bps(2.0), 2 * 1024 * 1024);
        assert_eq!(mibps_to_bps(0.0), 0);
        // Negative is not physical, but a NaN/negative from the source must not
        // become a huge positive via the cast.
        assert_eq!(mibps_to_bps(-1.0), 0);
        assert_eq!(mibps_to_bps(f64::NAN), 0);
    }

    /// SKADI-T-0515: the classification is pure, so it is testable without a
    /// network. It was tangled inside `resolve_source`'s fetch, which is why
    /// SKADI-T-0497's HTML-body rejection had no offline coverage.
    #[test]
    fn classify_source_separates_files_magnets_and_urls() {
        let dir = tmp_dir("classify");
        let file = dir.join("x.torrent");
        std::fs::write(&file, b"d4:infod").unwrap();

        assert_eq!(
            classify_source(file.to_str().unwrap()),
            TorrentSource::LocalFile(file.clone())
        );
        assert_eq!(
            classify_source("magnet:?xt=urn:btih:abc"),
            TorrentSource::Direct("magnet:?xt=urn:btih:abc".into())
        );
        assert_eq!(
            classify_source("https://x/t.torrent"),
            TorrentSource::Http("https://x/t.torrent".into())
        );
        // A path that does not exist is Direct, not LocalFile: librqbit gives a
        // better error for a missing file than a synthesised one would.
        assert_eq!(
            classify_source("/gone/x.torrent"),
            TorrentSource::Direct("/gone/x.torrent".into())
        );
        // An http URL is never treated as a path even if one happens to exist.
        assert!(matches!(
            classify_source("http://host/a"),
            TorrentSource::Http(_)
        ));
    }

    /// SKADI-T-0497's rejection, now reachable offline (SKADI-T-0515).
    ///
    /// A tracker behind Cloudflare answers a `.torrent` link with an HTML
    /// challenge page and a login-gated one answers with a sign-in page — both
    /// **200**. Handing those to librqbit produced "error decoding torrent" with
    /// nothing saying which link or why.
    #[test]
    fn an_html_body_is_rejected_with_the_url_and_what_came_back() {
        // Bencode always starts with a dictionary, so this is the whole test.
        assert!(
            reject_non_torrent_body("https://x/t", "application/x-bittorrent", b"d4:info").is_ok()
        );

        let err = reject_non_torrent_body(
            "https://tracker.test/download/1",
            "text/html; charset=utf-8",
            b"<!DOCTYPE html><html><head><title>Just a moment...</title>",
        )
        .expect_err("an HTML page is not a torrent");
        let text = err.to_string();
        // The URL, because the fix is always at the indexer end and an operator
        // needs to know *which* link.
        assert!(text.contains("https://tracker.test/download/1"), "{text}");
        assert!(text.contains("text/html"), "{text}");
        assert!(text.contains("Just a moment"), "{text}");
    }

    /// An empty body is rejected too, and must not panic on the slice.
    #[test]
    fn an_empty_body_is_rejected_without_panicking() {
        let err = reject_non_torrent_body("https://x/t", "", b"")
            .expect_err("an empty body is not a torrent");
        assert!(err.to_string().contains("0 bytes"), "{err}");
    }

    #[test]
    fn claim_budget_respects_the_cap() {
        // Unlimited cap → effectively unbounded.
        assert_eq!(claim_budget(100, None), usize::MAX);
        // Room left under the cap.
        assert_eq!(claim_budget(1, Some(3)), 2);
        // At/over the cap → claim nothing (saturates, no underflow).
        assert_eq!(claim_budget(3, Some(3)), 0);
        assert_eq!(claim_budget(5, Some(3)), 0);
    }

    #[test]
    fn config_from_view_parses_max_active() {
        let view = skadi_config::ConfigView::from_pairs([(
            "worker.max_active".to_string(),
            "2".to_string(),
        )]);
        let cfg = Config::from_view(&view, "sqlite://x".to_string(), false).unwrap();
        assert_eq!(cfg.max_active, Some(2));
        // Unset → unlimited.
        let empty = Config::from_view(
            &skadi_config::ConfigView::default(),
            "sqlite://y".to_string(),
            false,
        )
        .unwrap();
        assert_eq!(empty.max_active, None);
    }

    #[tokio::test]
    async fn resolve_source_reads_a_local_torrent_file_as_bytes() {
        let dir = tmp_dir("resolve");
        let file = dir.join("x.torrent");
        std::fs::write(&file, b"d8:announce...e").unwrap();
        let add = resolve_source(file.to_str().unwrap()).await.unwrap();
        assert!(
            matches!(add, AddTorrent::TorrentFileBytes(_)),
            "local .torrent path resolves to bytes"
        );
    }

    #[tokio::test]
    async fn resolve_source_passes_magnet_through() {
        let add = resolve_source("magnet:?xt=urn:btih:abc").await.unwrap();
        assert!(matches!(add, AddTorrent::Url(_)), "magnet stays a URL add");
    }

    #[tokio::test]
    async fn watch_dir_ingests_magnet_and_torrent_into_the_queue() {
        let store = tmp_store().await;
        let watch = tmp_dir("watch");
        let download = tmp_dir("dl");
        let cfg = watch_cfg(watch.clone(), download.clone());

        // Drop a .magnet (a magnet: URI) and a .torrent (bytes).
        std::fs::write(watch.join("film.magnet"), "magnet:?xt=urn:btih:deadbeef\n").unwrap();
        std::fs::write(watch.join("film.torrent"), b"d8:announce..e").unwrap();
        // Noise that must be ignored.
        std::fs::write(watch.join("notes.txt"), b"ignore me").unwrap();

        scan_watch_dir(&store, &cfg).await;

        // Both inputs became queued rows; the .txt did not.
        let mut sources = Vec::new();
        while let Some(job) = store.claim_next("test").await.unwrap() {
            sources.push(job.source);
        }
        assert_eq!(sources.len(), 2, "magnet + torrent enqueued, txt skipped");
        assert!(sources.iter().any(|s| s == "magnet:?xt=urn:btih:deadbeef"));
        let processed = watch.join(WATCH_PROCESSED_DIR);
        assert!(
            sources
                .iter()
                .any(|s| s == &processed.join("film.torrent").to_string_lossy()),
            "torrent source points at the archived file: {sources:?}"
        );

        // Files were archived out of the drop zone (not re-ingested next pass).
        assert!(!watch.join("film.magnet").exists());
        assert!(!watch.join("film.torrent").exists());
        assert!(
            processed.join("film.torrent").exists(),
            "torrent bytes kept"
        );
        assert!(
            watch.join("notes.txt").exists(),
            "non-torrent left in place"
        );

        // A second scan finds nothing new.
        scan_watch_dir(&store, &cfg).await;
        assert!(store.claim_next("test").await.unwrap().is_none());
    }

    #[test]
    fn abs_path_joins_components_onto_output_folder() {
        assert_eq!(
            file_abs_path("/data/downloads", &["Movie".into(), "movie.mkv".into()]),
            "/data/downloads/Movie/movie.mkv"
        );
        assert_eq!(
            file_abs_path("/data/downloads", &["single.iso".into()]),
            "/data/downloads/single.iso"
        );
    }

    /// SKADI-T-0394: the worker records what the *client* is doing, so the daemon
    /// can tell "queued behind the session's hash checks" from "live but finding
    /// no peers". Only the latter may be failed as stalled.
    #[test]
    fn progress_carries_the_client_transfer_state() {
        let stats = |state| TorrentStats {
            state,
            file_progress: vec![],
            error: None,
            progress_bytes: 0,
            uploaded_bytes: 0,
            total_bytes: 65_000_000_000,
            finished: false,
            live: None,
        };
        assert_eq!(
            progress_from_stats(&stats(TorrentStatsState::Initializing), "abc").client_state,
            Some("initializing".to_string())
        );
        assert_eq!(
            progress_from_stats(&stats(TorrentStatsState::Live), "abc").client_state,
            Some("live".to_string())
        );
        assert_eq!(
            progress_from_stats(&stats(TorrentStatsState::Paused), "abc").client_state,
            Some("paused".to_string())
        );
    }
    #[test]
    fn state_str_covers_all_variants() {
        assert_eq!(state_str(TorrentStatsState::Initializing), "initializing");
        assert_eq!(state_str(TorrentStatsState::Live), "live");
        assert_eq!(state_str(TorrentStatsState::Paused), "paused");
        assert_eq!(state_str(TorrentStatsState::Error), "error");
    }

    #[test]
    fn map_to_complete_mirrors_relative_path() {
        let inc = "/mnt/storage/skadi-torrent/incomplete";
        let comp = "/mnt/storage/skadi-torrent/complete";
        // Single file directly under the incomplete dir.
        assert_eq!(
            map_to_complete(&format!("{inc}/movie.mkv"), inc, comp).as_deref(),
            Some("/mnt/storage/skadi-torrent/complete/movie.mkv")
        );
        // Multi-file torrent in its own subfolder.
        assert_eq!(
            map_to_complete(&format!("{inc}/The Movie/movie.mkv"), inc, comp).as_deref(),
            Some("/mnt/storage/skadi-torrent/complete/The Movie/movie.mkv")
        );
        // A path not under the incomplete dir maps to nothing (reported as-is).
        assert_eq!(map_to_complete("/elsewhere/x.mkv", inc, comp), None);
    }
}
