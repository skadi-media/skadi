//! Pipeline stage logic, runner-free.
//!
//! Each acquisition stage is a plain `async fn` over `&mut AcquireState` plus
//! the capabilities it needs (indexers, scoring config). Keeping the logic out
//! of Cloacina's `#[task]` shell makes it unit-testable without a runner; the
//! thin task wrappers (composed into the `acquire` workflow in a later task)
//! just load the context, call these, and store the context back.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;

use skadi_core::{AppError, Protocol, Result};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_importer::{CompletedDownload, Importer};
use skadi_indexers::{
    Category, Indexer, IndexerHealth, Release, ReleaseFetch, indexer_health, release_key,
};
use skadi_notify::{EventPayload, NotificationEvent, Notifier};
use skadi_quality::audiobook::{Abridgement, abridgement_of, to_audiobook_quality};
use skadi_quality::{
    CustomFormat, QualityDefinition, QualityProfile, ReleaseMeta, parse_audiobook, to_quality,
};

use crate::services::AudiobookScoring;
use crate::state::{AcquireState, TvScope};

/// Map a [`ReleaseFetch`] to the [`Protocol`] needed to fetch it. Used by
/// `snatch`/`monitor` to pick a downloader.
pub fn release_protocol(fetch: &ReleaseFetch) -> Protocol {
    match fetch {
        ReleaseFetch::TorrentUrl(_) | ReleaseFetch::Magnet(_) => Protocol::Torrent,
        ReleaseFetch::NzbUrl(_) => Protocol::Usenet,
    }
}

/// Max indexer searches a single `search` call runs **concurrently** (SKADI-T-0202).
/// Bounds open sockets/load when many indexers serve a kind (NFR-INDEXER.1) while
/// still parallelising the fan-out instead of querying them one-by-one. The shared
/// HTTP client's own pool is the second backstop; the per-indexer rate limiter
/// (SKADI-T-0189) throttles each.
const SEARCH_FANOUT: usize = 6;

/// Longest one indexer may take to answer a search before its answer is
/// dropped (SKADI-T-0595). Generous: four aliases through FlareSolverr fit,
/// a dead meta-search site does not.
const INDEXER_SEARCH_BUDGET: Duration = Duration::from_secs(90);

/// Searches one indexer may have in flight at once, across every acquire run
/// (SKADI-T-0672).
///
/// Each indexer's rate limiter waits *inside* its `search`, so under a burst —
/// adding sixteen series queued hundreds of episode searches on 2026-10-02 —
/// a search spent its whole [`INDEXER_SEARCH_BUDGET`] queued behind the others
/// and timed out without the site ever being slow: 283 failures that day. The
/// permit is taken **before** the budget starts, so the queue forms here, the
/// rate limiter only ever holds a couple, and the budget measures the indexer.
const PER_INDEXER_INFLIGHT: usize = 2;

/// The in-flight permits for indexer `id`, created on first use.
fn indexer_permits(id: skadi_core::IndexerId) -> Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::LazyLock<
        std::sync::Mutex<
            std::collections::HashMap<skadi_core::IndexerId, Arc<tokio::sync::Semaphore>>,
        >,
    > = std::sync::LazyLock::new(Default::default);
    PERMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(id)
        .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(PER_INDEXER_INFLIGHT)))
        .clone()
}

/// Defaults for the indexer search **circuit breaker** (SKADI-T-0308). Live values
/// come from the config plane (`indexer_breaker_threshold` /
/// `indexer_breaker_cooldown_secs`) via [`set_circuit_config`], refreshed each hunter
/// sweep tick; these are the fallback before the first refresh / in tests.
const DEFAULT_CIRCUIT_THRESHOLD: u32 = 3;
const DEFAULT_CIRCUIT_COOLDOWN_SECS: i64 = 300;
/// Percent of the recent window that must fail before the circuit opens on
/// **rate** (SKADI-T-0568). `0` disables the rate arm, leaving only the streak.
///
/// 60 % because production showed indexers at ~55 % failure that never tripped
/// the streak arm — they fail more often than they work, with successes
/// interleaved often enough to keep resetting `consecutive_failures`. Set above
/// that so an indexer has to be worse than coin-flip-useless, not merely flaky.
const DEFAULT_CIRCUIT_RATE_PCT: u32 = 60;

/// Live circuit-breaker tuning (process-global so the pure gate reads it cheaply in
/// the search hot path). Threshold: consecutive failures before an indexer is skipped
/// (one dead CloudFlare tracker shouldn't serialise behind FlareSolverr and drag every
/// search to minutes). `0` disables the breaker. Cooldown: seconds before a single
/// half-open re-probe lets a recovered indexer rejoin.
static CIRCUIT_THRESHOLD: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(DEFAULT_CIRCUIT_THRESHOLD);
static CIRCUIT_COOLDOWN_SECS: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(DEFAULT_CIRCUIT_COOLDOWN_SECS);
static CIRCUIT_RATE_PCT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(DEFAULT_CIRCUIT_RATE_PCT);

/// Apply circuit-breaker tuning from the config plane (SKADI-T-0308). The hunter sweep
/// calls this each tick so `indexer_breaker_threshold` / `indexer_breaker_cooldown_secs`
/// take effect live, no rebuild/restart.
pub fn set_circuit_config(threshold: u32, cooldown_secs: i64) {
    set_circuit_config_full(
        threshold,
        cooldown_secs,
        CIRCUIT_RATE_PCT.load(std::sync::atomic::Ordering::Relaxed),
    );
}

/// As [`set_circuit_config`], plus the rate arm (SKADI-T-0568).
pub fn set_circuit_config_full(threshold: u32, cooldown_secs: i64, rate_pct: u32) {
    use std::sync::atomic::Ordering::Relaxed;
    CIRCUIT_THRESHOLD.store(threshold, Relaxed);
    CIRCUIT_COOLDOWN_SECS.store(cooldown_secs, Relaxed);
    CIRCUIT_RATE_PCT.store(rate_pct, Relaxed);
}

/// How [`decide`] trades quality against the chance a download actually
/// completes (SKADI-T-0598). Without inbound peer connections (no VPN port
/// forwarding) a torrent's seeders are only useful if one of them accepts
/// connections, so a release with more seeders is likelier to finish. With
/// `prefer_seeders` on, every candidate whose resolution is at least `floor`
/// is ranked by seeder band **ahead of** quality rank; quality breaks ties
/// within a band. Candidates below the floor rank below all of those and keep
/// the quality-first order among themselves. Audiobooks have no resolution
/// and are always "above the floor".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReachabilityPolicy {
    pub prefer_seeders: bool,
    pub floor: skadi_quality::Resolution,
}

static PREFER_SEEDERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static SEEDER_FLOOR_HEIGHT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(720);

/// Set the live policy from config (`hunter.prefer_seeders`,
/// `hunter.seeder_floor_height`), refreshed by the worker's config tick.
pub fn set_reachability_policy(prefer_seeders: bool, floor_height: u16) {
    use std::sync::atomic::Ordering::Relaxed;
    PREFER_SEEDERS.store(prefer_seeders, Relaxed);
    SEEDER_FLOOR_HEIGHT.store(floor_height, Relaxed);
}

/// The policy [`decide`] applies.
#[must_use]
pub fn reachability_policy() -> ReachabilityPolicy {
    use std::sync::atomic::Ordering::Relaxed;
    ReachabilityPolicy {
        prefer_seeders: PREFER_SEEDERS.load(Relaxed),
        floor: floor_for_height(SEEDER_FLOOR_HEIGHT.load(Relaxed)),
    }
}

/// The lowest resolution whose height is at least `height` (720 → 720p; 1000
/// → 1080p; 0 → SD, i.e. everything qualifies).
#[must_use]
pub fn floor_for_height(height: u16) -> skadi_quality::Resolution {
    use skadi_quality::Resolution::*;
    match height {
        0 => Sd,
        1..=480 => R480p,
        481..=576 => R576p,
        577..=720 => R720p,
        721..=1080 => R1080p,
        _ => R2160p,
    }
}

/// Coarse seeder band for ranking (SKADI-T-0598): the difference between 1 and
/// 2 seeders is noise, between 2 and 30 it is the whole game. Quality decides
/// within a band, so a 1080p with 12 seeders still beats a 720p with 15.
#[must_use]
pub fn seeder_band(seeders: u32) -> u8 {
    match seeders {
        0 => 0,
        1..=2 => 1,
        3..=9 => 2,
        10..=29 => 3,
        30..=99 => 4,
        _ => 5,
    }
}

/// Whether an indexer's search circuit is **open** (skip it). Open when it has
/// `>= threshold` consecutive failures AND its last failure is within the cooldown;
/// once the cooldown elapses the circuit goes *half-open* (returns `false`) so the
/// next search probes it once and a recovered indexer rejoins. Threshold/cooldown are
/// the live config-plane values. Pure w.r.t. `health`/`now` (injected for testability);
/// shared with the RSS fast-pass (`worker::rss_sweep`).
pub(crate) fn search_circuit_open(health: Option<&IndexerHealth>, now: DateTime<Utc>) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let threshold = CIRCUIT_THRESHOLD.load(Relaxed);
    if threshold == 0 {
        return false; // breaker disabled
    }
    let cooldown = CIRCUIT_COOLDOWN_SECS.load(Relaxed);
    let Some(h) = health else { return false };

    // Either arm can open the circuit; the cooldown and the half-open re-probe
    // are shared, so a recovered indexer rejoins the same way whichever tripped.
    let streak_open = threshold > 0 && h.consecutive_failures >= threshold;

    // The rate arm (SKADI-T-0568). The streak arm only catches an indexer that
    // is *down*; production showed ones that are *mostly broken* — ~55 % failure
    // with successes interleaved often enough that `consecutive_failures` never
    // reached 3. Those cost a call every sweep, and since SKADI-T-0488 capped
    // concurrent solves, a solver slot too.
    let rate_pct = CIRCUIT_RATE_PCT.load(Relaxed);
    let rate_open = rate_pct > 0
        && h.recent_failure_rate()
            .is_some_and(|r| r * 100.0 >= rate_pct as f32);

    if !(streak_open || rate_open) {
        return false;
    }
    h.last_failure
        .is_some_and(|lf| now.signed_duration_since(lf) < chrono::Duration::seconds(cooldown))
}

/// `search`: query every enabled indexer that serves the request's kind and
/// collect candidate releases into `state.candidates` (de-duplicated across
/// indexers). Indexers are queried **concurrently** but bounded by
/// [`SEARCH_FANOUT`]. A single indexer failing is tolerated; only an all-failed
/// search (with no results) propagates the error so Cloacina can retry.
pub async fn search(state: &mut AcquireState, indexers: &[Arc<dyn Indexer>]) -> Result<()> {
    let query = state.request.query();
    let kind = state.request.kind;
    let now = Utc::now();
    // Honour `enable_automatic_search` (SKADI-T-0539) — but only for a sweep.
    // An operator turns it off precisely to keep an indexer available for the
    // interactive list and manual grabs, so filtering those too would defeat the
    // setting rather than implement it.
    let automatic = state.request.trigger == crate::state::SearchTrigger::Automatic;
    // Tag scoping (SKADI-T-0556): an indexer tagged `anime` is consulted only for
    // anime-tagged items. Applied to interactive searches too, unlike
    // `enable_automatic_search` above — that setting is about *when* an indexer
    // is polled, whereas a tag says the indexer is irrelevant to this item at
    // all, and an operator hand-searching does not want a French-TV tracker
    // queried for an audiobook either.
    let item_tags = state.request.tags.clone();
    let supported: Vec<&Arc<dyn Indexer>> = indexers
        .iter()
        .filter(|ix| ix.supports(kind))
        .filter(|ix| !automatic || ix.enable_automatic_search())
        // `None` is a free-text search with no item, where scoping means nothing
        // and every indexer is consulted.
        .filter(|ix| match &item_tags {
            Some(tags) => ix.applies_to_tags(tags),
            None => true,
        })
        .collect();

    // Circuit-break known-down indexers (SKADI-T-0308): skip those with an open
    // circuit so a few dead, CloudFlare-banned trackers don't serialise behind the
    // single-threaded FlareSolverr and drag every search to minutes. Half-open after
    // the cooldown re-probes for recovery.
    let mut live: Vec<&Arc<dyn Indexer>> = supported
        .iter()
        .copied()
        .filter(|ix| !search_circuit_open(indexer_health().get(ix.id()).as_ref(), now))
        .collect();
    let skipped = supported.len() - live.len();
    if live.is_empty() && !supported.is_empty() {
        // Every supported indexer is circuit-open — fall back to trying them all
        // rather than failing with a misleading "no indexer serves this kind"; this
        // doubles as the half-open probe for the whole set.
        live = supported.clone();
    } else if skipped > 0 {
        tracing::info!(
            skipped,
            live = live.len(),
            "search: skipping circuit-open (red) indexers"
        );
    }
    let attempted = live.len();

    // Fan out concurrently (bounded). Pre-build the futures (not via a stream
    // `.map` closure) so the `&query` borrow doesn't trip the higher-ranked
    // `Send` bound the Cloacina task wrapper requires; the stream just drives them.
    // Per-indexer time budget (SKADI-T-0595): a slow site — the DHT meta-search
    // indexers answer in minutes, a CloudFlare-fronted tracker waits on a
    // FlareSolverr solve per alias — used to hold the whole fan-out. Past the
    // budget its answer is dropped and the search proceeds with the rest.
    let query = &query;
    let futs: Vec<_> = live
        .iter()
        .map(|ix| async move {
            // Queue for a slot first, outside the budget (SKADI-T-0672).
            let _permit = indexer_permits(ix.id()).acquire_owned().await;
            match tokio::time::timeout(INDEXER_SEARCH_BUDGET, ix.search(query)).await {
                Ok(r) => r,
                Err(_) => {
                    let msg = format!(
                        "indexer {} exceeded the {}s search budget",
                        ix.id(),
                        INDEXER_SEARCH_BUDGET.as_secs()
                    );
                    // The timeout drops the indexer's future, so its own health
                    // decorator never sees the failure; record it here or
                    // `/indexers/health` shows the slowest indexer as healthy.
                    indexer_health().record_failure(ix.id(), msg.clone());
                    Err(AppError::Network(msg))
                }
            }
        })
        .collect();
    let results: Vec<Result<Vec<skadi_indexers::Release>>> = futures::stream::iter(futs)
        .buffer_unordered(SEARCH_FANOUT)
        .collect()
        .await;

    // Shared-outage forgiveness (SKADI-T-0308): if EVERY queried indexer failed, the
    // cause is almost certainly shared infra (the VPN/proxy is down), not the indexers
    // themselves — so undo the failure each just recorded rather than tripping all
    // their circuits at once (which then needs a slow, thundering-herd recovery). A
    // partial failure (some succeeded) is a real per-indexer signal and counts.
    if attempted >= 2 && results.iter().all(|r| r.is_err()) {
        for ix in &live {
            indexer_health().forgive(ix.id());
        }
        tracing::warn!(
            attempted,
            "search: all indexers failed — treating as a shared-infra outage, not penalizing their circuits"
        );
    }

    // Whether any indexer answered at all, even with nothing (SKADI-T-0672).
    let answered = results.iter().any(Result::is_ok);
    let mut found: Vec<skadi_indexers::Release> = Vec::new();
    let mut last_err: Option<AppError> = None;
    for r in results {
        match r {
            Ok(releases) => found.extend(releases),
            Err(e) => {
                tracing::warn!(error = %e, "indexer search failed");
                last_err = Some(e);
            }
        }
    }

    // De-dup cross-indexer duplicates by (title, fetch).
    found.sort_by(|a, b| {
        a.title
            .cmp(&b.title)
            .then_with(|| format!("{:?}", a.fetch).cmp(&format!("{:?}", b.fetch)))
    });
    found.dedup_by(|a, b| a.title == b.title && a.fetch == b.fetch);

    // Every attempted indexer errored → surface the error. When any indexer
    // answered, an empty result is "nothing found" (decide reports it), not the
    // network error of whichever indexer happened to time out — that error sent
    // the item round the network-retry path and named a slow site as the cause
    // of a release that does not exist yet (SKADI-T-0672).
    if found.is_empty() {
        if !answered && let Some(e) = last_err {
            return Err(e);
        }
        if attempted == 0 {
            // Distinguish "nothing serves this kind" from "tag scoping excluded
            // everything" (SKADI-T-0556). Blaming the media kind when the real
            // cause is a tag would send the operator to check indexer categories
            // — the one place the answer is not — while the item silently never
            // searches again.
            if let Some(tags) = &state.request.tags
                && !indexers.is_empty()
                && indexers
                    .iter()
                    .filter(|ix| ix.supports(kind))
                    .all(|ix| !ix.applies_to_tags(tags))
            {
                return Err(AppError::Config(format!(
                    "every indexer that serves this media kind is scoped to tags \
                     this item does not carry (item tags: {tags:?}); tag an \
                     indexer to match, or clear an indexer's tags so it applies \
                     to everything"
                )));
            }
            return Err(AppError::Config(
                "no enabled indexer serves this media kind".into(),
            ));
        }
    }

    if kind == skadi_core::MediaKind::Series {
        reparse_tv(&mut found);
    }

    state.candidates = found;
    Ok(())
}

/// Re-parse TV candidates with the TV parser (SKADI-T-0386).
///
/// The indexer adapters parse every title with the movie-shaped
/// [`skadi_quality::parse`], which splits title from tag soup on the **year** —
/// and most episode names carry none (`Show.S03E02.1080p.WEB.h264-GRP`), so the
/// tags after the episode marker were never scanned: `resolution: None` →
/// `to_quality` fails → "unrecognized quality". Live, 142 of 149 candidates for
/// one episode died that way, leaving only the year-bearing season packs, stray
/// episodes and same-named films for `decide` to pick from. [`skadi_quality::parse_tv`]
/// splits on the episode marker and scans the whole title for quality tags, so
/// quality/format scoring (and the episode gate) see the real shape.
pub fn reparse_tv(releases: &mut [skadi_indexers::Release]) {
    for r in releases {
        r.parsed = skadi_quality::parse_tv(&r.title);
    }
}

/// Scoring configuration `decide` needs: the quality definitions to classify a
/// parsed release, the profile to gate/rank it, and the custom-format registry.
pub struct Scoring<'a> {
    pub definitions: &'a [QualityDefinition],
    pub profile: &'a QualityProfile,
    pub formats: &'a [CustomFormat],
    /// Minimum seeders a torrent release must report to be eligible
    /// (SKADI-T-0113). `0` disables the filter; usenet releases (no seeders)
    /// are never filtered.
    pub min_seeders: u32,
    /// Per-indexer overrides from the indexer's own settings (SKADI-T-0539):
    /// `(priority, minimum_seeders)` by indexer id. Empty ⇒ every indexer uses
    /// the defaults, which is what a caller with no live indexer set gets.
    pub indexer_flags: &'a [(skadi_core::IndexerId, u32, u32)],
    /// Blocklisted release keys (`skadi_indexers::release_key`) to exclude
    /// (SKADI-T-0115). Empty disables the filter. The caller builds this from
    /// the store's blocklist so `decide` and the interactive releases endpoint
    /// share one exclusion rule.
    pub blocklisted: &'a HashSet<String>,
    /// When `Some`, score this release on the **audiobook** axis (SKADI-I-0017):
    /// re-parse the title with `parse_audiobook`, classify against the audiobook
    /// definitions, and apply the abridged-reject floor — instead of the movie
    /// resolution/source path. `None` ⇒ the movie path.
    pub audiobook: Option<&'a AudiobookScoring>,
    /// The quality the acquirable **already holds**, when this is an *upgrade*
    /// run over an already-`Imported` item (SKADI-T-0182). Passed straight into
    /// [`QualityProfile::decide_id`] as `current`: `None` ⇒ first acquisition
    /// (accept any allowed quality); `Some(qid)` ⇒ accept only a *strictly
    /// better* candidate (Upgrade), reject equal-or-worse, and stop once the held
    /// quality meets the cutoff (MeetsCutoff). The domain reads `qid` from
    /// `AcquisitionStatus::Imported { quality }` in its `upgradable()` sweep.
    pub current_quality: Option<skadi_core::QualityId>,
    /// The aggregate custom-format **score** the held file has, on an upgrade run
    /// (SKADI-T-0186). A *second* upgrade axis: a candidate of the **same** quality
    /// with a strictly-higher format score is an Upgrade (proper/repack). `None`
    /// ⇒ first acquisition / no format-score comparison. Read from
    /// `AcquisitionStatus::Imported { score }`.
    pub current_format_score: Option<i32>,
    /// The held file has been probed and **will not direct-play** (SKADI-T-0584).
    ///
    /// Relaxes the strictly-better rule for this run. A 54 GB DTS-only remux sits
    /// at the top of the quality ladder with a high format score, so nothing is
    /// ever "strictly better" than it — yet it plays silently on a phone, which
    /// makes it worse than almost anything that plays. Holding it to the normal
    /// upgrade bar would park it there forever.
    ///
    /// This does not accept *anything*: the candidate must still pass the
    /// profile's allowed-quality gate and every other reject rule. It only stops
    /// the held file's own score from being the floor.
    pub current_unplayable: bool,
}

/// Coarse bucket for *why* a candidate was dropped (SKADI-T-0380).
///
/// The per-release `reason` strings are written for a human reading one
/// decision; they interpolate quality names and counts, so they can't be
/// aggregated. This is the stable axis `decide` tallies on, so "181 candidates,
/// 0 grabbed" can say **which gate** ate them — the question that took a session
/// of manual SQL to answer on 2026-09-01.
///
/// Set at each rejection site rather than inferred from the reason text, so the
/// two can't drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// Indexer tagged it entirely in wrong-media categories (SKADI-T-0360).
    Category,
    /// Gross byte-size outlier for the media kind (SKADI-T-0362).
    Size,
    /// Previously failed/bad, on the blocklist (SKADI-T-0115).
    Blocklisted,
    /// Below the configured seeder floor (SKADI-T-0113).
    Seeders,
    /// Quality not allowed, not an upgrade, or below the custom-format floor.
    Quality,
    /// Audiobook identity gate: author/title tokens don't match (SKADI-T-0359).
    Identity,
    /// Movie/TV title-relevance coverage below the floor (SKADI-T-0374), or a
    /// sequel/numbered-entry marker the wanted title doesn't carry (SKADI-T-0387).
    Relevance,
    /// TV episode identity gate: no season/episode markers, or the wrong
    /// season/episode for the wanted scope (SKADI-T-0386).
    Episode,
    /// Movie year gate: the release names a year outside `request.year ± 1`
    /// (SKADI-T-0387).
    Year,
    /// A dub or machine reading into a language this library does not want:
    /// Russian/Ukrainian dub groups, `rus`/`ukr` tags, speech-synthesis audio
    /// (SKADI-T-0671).
    Language,
}

impl RejectReason {
    /// Short operator-facing label — the word shown in a rejection tally.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::Size => "size",
            Self::Blocklisted => "blocklisted",
            Self::Seeders => "seeders",
            Self::Quality => "quality",
            Self::Identity => "identity",
            Self::Relevance => "relevance",
            Self::Episode => "episode",
            Self::Year => "year",
            Self::Language => "language",
        }
    }

    // Operator-facing *guidance* for each gate ("widen the allowed qualities…")
    // deliberately does NOT live here: it is UI copy about which knob to turn,
    // and the pipeline should not own sentences about its own settings screen.
    // It lives beside the panel that renders it, keyed by `label()` —
    // `skadi_web::activity::remedy_for` (SKADI-T-0381).
}

/// Whether a single release would be accepted, and why — surfaced to the
/// interactive-search UI (SKADI-T-0114) and the shared core of [`decide`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct Verdict {
    pub accepted: bool,
    /// The accepted quality name, or the rejection reason.
    pub reason: String,
}

/// Whether a [`Decision`](skadi_quality::Decision) means "grab this release".
/// `Accept` (first acquisition) and `Upgrade` (strictly better than the held
/// quality) grab; `Reject` (not allowed / not an improvement) and `MeetsCutoff`
/// (the held quality already satisfies the cutoff) do not (SKADI-T-0182).
fn decision_accepts(decision: skadi_quality::Decision) -> bool {
    matches!(
        decision,
        skadi_quality::Decision::Accept | skadi_quality::Decision::Upgrade
    )
}

/// The rejection reason for a candidate that the profile won't grab. When this
/// is an upgrade run (`is_upgrade`), a non-grab means "no better than what we
/// hold"; otherwise it means "not allowed by the profile".
fn not_grabbed_reason(qname: &str, is_upgrade: bool) -> String {
    if is_upgrade {
        format!("{qname} is not an upgrade over the current file")
    } else {
        format!("{qname} is not allowed by the profile")
    }
}

/// A full, serializable explanation of how one release was judged — the "explain
/// a decision" / "test a title" output (SKADI-T-0184). The rich core that
/// [`evaluate`] reduces to its thin `(Verdict, ranked)` — one source of truth so
/// the hot path and the explanation can never disagree.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReleaseExplanation {
    /// The release title that was judged.
    pub title: String,
    /// Whether the release would be grabbed.
    pub accepted: bool,
    /// The accepted quality name, or the rejection reason.
    pub reason: String,
    /// Classified quality name, when the release classified to a known quality
    /// (`None` for pre-quality rejections: blocklist, seeders, ebook, abridged,
    /// unrecognized).
    pub quality: Option<String>,
    /// Rank of that quality in the profile's `allowed` list (lower = worse), when
    /// the quality is allowed.
    pub quality_rank: Option<usize>,
    /// Aggregate custom-format score.
    pub format_score: i32,
    /// Custom formats that matched, each with its contributed score.
    pub matched_formats: Vec<skadi_quality::MatchedFormat>,
    /// The profile decision (`Accept`/`Upgrade`/`Reject`/`MeetsCutoff`) once a
    /// quality is classified; `None` for pre-quality rejections.
    pub decision: Option<String>,
    /// Coarse bucket for the rejection, for aggregation (SKADI-T-0380). `None`
    /// when `accepted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<RejectReason>,
}

fn decision_label(d: skadi_quality::Decision) -> &'static str {
    match d {
        skadi_quality::Decision::Accept => "Accept",
        skadi_quality::Decision::Upgrade => "Upgrade",
        skadi_quality::Decision::Reject => "Reject",
        skadi_quality::Decision::MeetsCutoff => "MeetsCutoff",
    }
}

/// Explain how a release is judged against the profile + scoring (SKADI-T-0184):
/// classify quality, run the profile decision, score custom formats, apply the
/// floor — returning the full breakdown. The single source of truth for
/// acceptance; [`evaluate`] is a thin reduction of this.
pub fn explain(release: &skadi_indexers::Release, scoring: &Scoring<'_>) -> ReleaseExplanation {
    let title = release.title.clone();
    let pre_reject = |reason: String, category: RejectReason| ReleaseExplanation {
        title: title.clone(),
        accepted: false,
        reason,
        quality: None,
        quality_rank: None,
        format_score: 0,
        matched_formats: Vec::new(),
        decision: None,
        category: Some(category),
    };

    // Apply the decision → rank → floor gates over a classified quality, building
    // the full explanation either way. Shared by both the movie and audiobook
    // axes so they explain identically.
    let finalize = |qname: String,
                    rank: Option<usize>,
                    decision: skadi_quality::Decision,
                    breakdown: skadi_quality::ScoreBreakdown| {
        let mut exp = ReleaseExplanation {
            title: title.clone(),
            accepted: false,
            reason: String::new(),
            quality: Some(qname.clone()),
            quality_rank: rank,
            format_score: breakdown.total,
            matched_formats: breakdown.matched,
            decision: Some(decision_label(decision).to_string()),
            // Every early return below is a quality-axis rejection (not allowed,
            // not an upgrade, format gates, score floor); cleared on acceptance.
            category: Some(RejectReason::Quality),
        };
        // decide_id(candidate, current) contract (skadi_quality): None current ⇒
        // accept any allowed; Some current ⇒ grab only a strictly-better Upgrade,
        // MeetsCutoff/Reject ⇒ don't grab on the *quality* axis (SKADI-T-0182).
        let quality_grab = decision_accepts(decision);
        // Third axis (SKADI-T-0584): we hold something that does not play. Any
        // allowed-quality candidate is a candidate, because the bar it has to
        // clear is "produces sound", not "beats the remux we cannot watch".
        let unplayable_grab =
            scoring.current_unplayable && scoring.profile.upgrade_allowed && rank.is_some();
        // Second upgrade axis — custom-format score (SKADI-T-0186, ADR A-0003 §2):
        // on an upgrade run, a candidate of the **same** quality with a
        // strictly-higher format score is an upgrade (proper/repack). Terminates
        // naturally (bounded by the configured formats' scores).
        let format_grab = scoring.profile.upgrade_allowed
            && match (scoring.current_quality, scoring.current_format_score, rank) {
                (Some(cur_q), Some(cur_fs), Some(cand_rank)) => {
                    let cur_rank = scoring.profile.allowed.iter().position(|q| *q == cur_q);
                    cur_rank == Some(cand_rank) && breakdown.total > cur_fs
                }
                _ => false,
            };
        if !quality_grab && !format_grab && !unplayable_grab {
            exp.reason = not_grabbed_reason(&qname, scoring.current_quality.is_some());
            return exp;
        }
        // Reached acceptance via the format-score axis only ⇒ it's an Upgrade.
        if (format_grab || unplayable_grab) && !quality_grab {
            exp.decision = Some(decision_label(skadi_quality::Decision::Upgrade).to_string());
        }
        if rank.is_none() {
            exp.reason = format!("{qname} is not in the profile");
            return exp;
        }
        // Required/Ignored hard gates (SKADI-T-0185, ADR SKADI-A-0003): applied
        // after the quality decision, before the score floor. A must-not
        // (Ignored) present, or a must-contain (Required) absent, rejects
        // regardless of the aggregate score.
        if let Some(name) = breakdown.present_ignored.first() {
            exp.reason = format!("contains ignored custom format {name:?}");
            return exp;
        }
        if let Some(name) = breakdown.missing_required.first() {
            exp.reason = format!("missing required custom format {name:?}");
            return exp;
        }
        if !scoring.profile.accepts_format_score(breakdown.total) {
            exp.reason = format!("custom-format score {} is below the floor", breakdown.total);
            return exp;
        }
        exp.accepted = true;
        exp.reason = qname;
        exp.category = None;
        exp
    };

    // blocklist (SKADI-T-0115): a previously-failed/bad release is excluded so
    // the sweep stops re-grabbing it.
    if scoring.blocklisted.contains(&release_key(release)) {
        return pre_reject("blocklisted".into(), RejectReason::Blocklisted);
    }
    // min-seeders (torrents only; usenet has no seeders) — SKADI-T-0113.
    //
    // The indexer's own floor **overrides** the profile's when set
    // (SKADI-T-0539): `0` means "use the profile's", anything else is the
    // operator saying this particular tracker's seeder counts need a different
    // bar — which is the point of a per-indexer setting, so a `max` would ignore
    // them asking for a *lower* one.
    let floor = match indexer_flag(scoring.indexer_flags, release.indexer) {
        Some((_, min)) if min > 0 => min,
        _ => scoring.min_seeders,
    };
    if release.seeders.is_some_and(|s| s < floor) {
        return pre_reject(
            format!(
                "too few seeders ({} < {floor})",
                release.seeders.unwrap_or(0)
            ),
            RejectReason::Seeders,
        );
    }
    // Audiobook axis (SKADI-I-0017): the release's `parsed` was produced by the
    // movie parser, so re-parse the title for audiobook fields, then classify on
    // format/bitrate + apply the abridged-reject floor.
    if let Some(ab) = scoring.audiobook {
        // We also search the Books/EBook categories (SKADI-T-0176), which surface
        // the ebook edition of a title — drop those so we don't grab an ebook as an
        // audiobook (esp. now that format-less releases are accepted, SKADI-T-0175).
        if skadi_quality::audiobook::looks_like_ebook(&release.title, release.size) {
            return pre_reject(
                "looks like an ebook, not an audiobook".into(),
                RejectReason::Identity,
            );
        }
        let parsed = parse_audiobook(&release.title);
        if abridgement_of(&parsed) == Abridgement::Abridged && !ab.allow_abridged {
            return pre_reject(
                "abridged (the profile rejects abridged releases)".into(),
                RejectReason::Quality,
            );
        }
        let Some(qid) = to_audiobook_quality(&parsed, &ab.definitions) else {
            return pre_reject(
                "unrecognized audiobook quality".into(),
                RejectReason::Quality,
            );
        };
        let qname = ab
            .definitions
            .iter()
            .find(|d| d.id == qid)
            .map_or("unknown", |d| d.name.as_str())
            .to_string();
        let decision = scoring.profile.decide_id(qid, scoring.current_quality);
        let rank = scoring.profile.allowed.iter().position(|q| *q == qid);
        let meta = ReleaseMeta {
            size_bytes: Some(release.size),
            indexer_flags: &[],
        };
        let breakdown = skadi_quality::score_breakdown(
            &scoring.profile.formats,
            scoring.formats,
            &release.title,
            &parsed,
            &meta,
        );
        return finalize(qname, rank, decision, breakdown);
    }

    // Movie axis.
    let Some(quality) = to_quality(&release.parsed, scoring.definitions) else {
        return pre_reject("unrecognized quality".into(), RejectReason::Quality);
    };
    let qname = scoring
        .definitions
        .iter()
        .find(|d| d.id == quality.id)
        .map_or("unknown", |d| d.name.as_str())
        .to_string();
    let decision = scoring
        .profile
        .decide_id(quality.id, scoring.current_quality);
    let rank = scoring
        .profile
        .allowed
        .iter()
        .position(|q| *q == quality.id);
    let meta = ReleaseMeta {
        size_bytes: Some(release.size),
        indexer_flags: &[],
    };
    let breakdown = skadi_quality::score_breakdown(
        &scoring.profile.formats,
        scoring.formats,
        &release.title,
        &release.parsed,
        &meta,
    );
    finalize(qname, rank, decision, breakdown)
}

/// Evaluate one release against the profile + scoring (SKADI-T-0114). Returns
/// the [`Verdict`] and, when accepted, the `(quality_rank, format_score)` used
/// for ranking. A thin reduction of [`explain`] (its single source of truth) —
/// shared by [`decide`] (picks the best accepted) and `GET …/releases`.
pub fn evaluate(
    release: &skadi_indexers::Release,
    scoring: &Scoring<'_>,
) -> (Verdict, Option<(usize, i32)>) {
    let e = explain(release, scoring);
    // Accepted ⇒ the quality is allowed, so `quality_rank` is `Some`.
    let ranked = if e.accepted {
        e.quality_rank.map(|rank| (rank, e.format_score))
    } else {
        None
    };
    (
        Verdict {
            accepted: e.accepted,
            reason: e.reason,
        },
        ranked,
    )
}

/// Why one candidate was dropped, from the full explanation. `None` when it was
/// accepted. Defaults to [`RejectReason::Quality`] if a rejection somehow
/// carries no category — a tally that undercounts would mislead more than one
/// that lands in the broadest bucket.
fn reject_category(e: &ReleaseExplanation) -> Option<RejectReason> {
    if e.accepted {
        return None;
    }
    Some(e.category.unwrap_or(RejectReason::Quality))
}

/// What [`decide`] weighed and what it threw away (SKADI-T-0380).
///
/// Emitted onto the trace so `/activity` can answer "181 candidates, nothing
/// grabbed — *why*" without a database session.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionTally {
    /// Candidates `decide` looked at.
    pub considered: usize,
    /// Rejections per gate, highest count first when rendered.
    pub rejected: std::collections::BTreeMap<String, usize>,
    /// Title relevance of the release actually chosen (movies/TV), `0.0..=1.0`.
    /// A low value here is the signature of a wrong-title grab — the
    /// "101 Dalmatians" → "101 Dalmatians II" case.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chosen_relevance: Option<f32>,
}

impl DecisionTally {
    fn reject(&mut self, why: RejectReason) {
        *self.rejected.entry(why.label().to_string()).or_default() += 1;
    }

    /// Total rejected across all gates.
    #[must_use]
    pub fn rejected_total(&self) -> usize {
        self.rejected.values().sum()
    }

    /// The gate that ate the most candidates, with its count.
    #[must_use]
    pub fn top_reason(&self) -> Option<(&str, usize)> {
        self.rejected
            .iter()
            // Ties break on the label so the summary is stable across runs.
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(k, v)| (k.as_str(), *v))
    }

    /// One-line summary: `"87 quality, 45 relevance, 12 size"`, busiest gate
    /// first. Empty string when nothing was rejected.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut pairs: Vec<_> = self.rejected.iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        pairs
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Minimum [`title_relevance`](skadi_quality::title_relevance) coverage for an
/// audiobook candidate to be considered for this book (SKADI-T-0180): below this,
/// the requested title is only partially present (a stray-token match on a
/// different work), so the candidate is dropped rather than grabbed.
const MIN_TITLE_COVERAGE: f32 = 0.6;

/// Least share of a pack's title tokens the series name must account for when the
/// author is absent (SKADI-T-0591 follow-up): a real "Dungeon Crawler Carl - Books
/// 1-8" is mostly series; junk that merely contains the series' words is not.
const MIN_AUTHORLESS_PACK_PRECISION: f32 = 0.4;

/// Bucket assigned to a multi-book **pack** in [`decide`] (SKADI-T-0314): above any single's
/// `0..=4` (`precision * 4`), so when the series is wanted a pack is always preferred over
/// individual books (the operator's always-prefer policy). The Phase-1 import fan-out then
/// satisfies every tracked book the pack contains.
const PACK_BUCKET: u8 = 5;

/// `(priority, minimum_seeders)` for `id` (SKADI-T-0539). A linear scan: the live
/// indexer set is a handful of entries, so a map would cost more to build than it
/// saves to search.
fn indexer_flag(
    flags: &[(skadi_core::IndexerId, u32, u32)],
    id: skadi_core::IndexerId,
) -> Option<(u32, u32)> {
    flags
        .iter()
        .find(|(i, _, _)| *i == id)
        .map(|(_, p, m)| (*p, *m))
}

/// The [`decide`] ordering bucket for one audiobook candidate (SKADI-T-0314), or `None` to reject
/// it. A multi-book **pack** is gated on the **series** name (a pack never covers a single book's
/// title) and bucketed above any single ([`PACK_BUCKET`]), so it's always preferred when the series
/// is wanted; a single book is gated on the **book** titles (the series alias excluded) and
/// bucketed by title precision (`0..=4`). Pure, so the pack-preference logic is unit-testable
/// without standing up a full audiobook `Scoring`.
fn audiobook_bucket(
    release_title: &str,
    book_titles: &[String],
    series: Option<&str>,
) -> Option<u8> {
    // Identity gate (SKADI-T-0359): the wanted title must be present (coverage)
    // AND — when we know the author — at least one author token must appear in the
    // release. A real audiobook release names its author; junk that only shares
    // the title word ("Fallen" → "Evanescence – Fallen", "Wasteland" → "Wasteland
    // 3-HOODLUM") has author_hits == 0 and is rejected rather than grabbed.
    // A release that names itself as another kind of media is not a book,
    // whatever its title shares with the wanted one (SKADI-T-0670).
    if not_an_audiobook(release_title) {
        return None;
    }
    let r = skadi_quality::title_relevance(book_titles, release_title);
    let author_ok = r.author_tokens == 0 || r.author_hits >= 1;
    if skadi_quality::parse_audiobook(release_title).book_pack {
        // A pack is gated on the series name, and a one-word series ("Talisman")
        // is covered by anything that says the word — a 10-album FLAC discography
        // was grabbed for a Stephen King anthology, downloaded, rejected by the
        // importer, and re-grabbed from two more trackers before the blocklist
        // caught up (SKADI-T-0587). So a one-token series must also pass the
        // author gate; a multi-token series ("Dungeon Crawler Carl - Books 1-8")
        // is specific enough on its own, as packs often omit the author.
        // A pack without the author's name is trusted only when the series name
        // is more than one token AND explains a fair share of the release title
        // (precision): "Secret Projects" fully covers "Drop Drop Top Secret
        // Gadget Collection-After Effects Projects", which decide chose for a
        // Sanderson book on 2026-09-18 — two common words scattered through a
        // ten-token title are not the series.
        let series = series?;
        let rs = skadi_quality::title_relevance(&[series.to_string()], release_title);
        let specific =
            author_ok || (rs.primary_tokens >= 2 && rs.precision >= MIN_AUTHORLESS_PACK_PRECISION);
        (rs.coverage >= MIN_TITLE_COVERAGE && specific).then_some(PACK_BUCKET)
    } else {
        (r.coverage >= MIN_TITLE_COVERAGE && author_ok).then_some((r.precision * 4.0).round() as u8)
    }
}

/// Whether a release title marks itself as software, video, a game or an
/// episode (SKADI-T-0670). These came through for audiobooks on 2026-10-02 from
/// a general tracker whose results carry no category: "DAEMON Tools Ultra …
/// x64 Pre Cracked", "… Chimera XXX 1080p MP4", "Malice 1993 1080p", "Unsouled
/// NSZ". Whole-token matches only, so a book title that merely contains the
/// letters ("Crackdown", "Episodes") is unaffected.
fn not_an_audiobook(release_title: &str) -> bool {
    const MARKERS: &[&str] = &[
        // Software.
        "x64", "x86", "win64", "win32", "cracked", "crack", "keygen", "portable", "macos",
        // Adult video.
        "xxx", // Video: resolutions, sources and codecs no audiobook carries.
        "480p", "576p", "720p", "1080p", "2160p", "4k", "bluray", "bdrip", "brrip", "webrip",
        "webdl", "hdtv", "dvdrip", "x264", "x265", "h264", "h265", "hevc", "xvid", "remux",
        // Console / PC game images and scene game groups.
        "nsz", "nsp", "xci", "pkg", "codex", "skidrow", "fitgirl", "dodi", "plaza", "hoodlum",
    ];
    let lower = release_title.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    // "WEB-DL" splits into "web" + "dl".
    let web_dl = tokens.windows(2).any(|w| w == ["web", "dl"]);
    // An episode code (S01E02) is TV, never a book.
    let episode = tokens.iter().any(|t| {
        let b = t.as_bytes();
        b.len() >= 6
            && b[0] == b's'
            && b[1..3].iter().all(u8::is_ascii_digit)
            && b[3] == b'e'
            && b[4..6].iter().all(u8::is_ascii_digit)
    });
    web_dl || episode || tokens.iter().any(|t| MARKERS.contains(t))
}

/// What a release name says about its audio language (SKADI-T-0671).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LanguageSignal {
    /// English, multi-language, or nothing said — the normal case.
    Fine,
    /// Tagged only with another language. Kept, but ranked below every
    /// [`Fine`](Self::Fine) release: the library does not record a title's
    /// original language, so a Korean film tagged `KOREAN` must still be
    /// grabbable when that is all there is.
    ForeignOnly,
    /// A dub or machine reading this library never wants.
    Reject,
}

/// Read the audio language from a release name (SKADI-T-0671).
///
/// Rejects outright only on signals that are never a title's original audio
/// here: the Russian/Ukrainian dub groups that a general tracker served for
/// English shows on 2026-10-02 (ColdFilm, LostFilm …), bare `rus`/`ukr` tags,
/// and speech-synthesis readings ("Yandex SpeechKit"). Any English, MULTi or
/// dual-audio marker wins over a foreign tag. The script of the post is not a
/// signal: "Терри Гудкайнд / Terry Goodkind - Confessor [Sam Tsoutsouvas]" is an
/// English reading on a Russian tracker and stays [`Fine`](LanguageSignal::Fine).
fn language_signal(release_title: &str) -> LanguageSignal {
    const DUB_GROUPS: &[&str] = &[
        "coldfilm",
        "lostfilm",
        "newstudio",
        "baibako",
        "alexfilm",
        "jaskier",
        "kubik",
        "hdrezka",
        "rezka",
        "rudub",
        "amedia",
        "novafilm",
        "ideafilm",
    ];
    const SYNTHETIC: &[&str] = &["speechkit", "tts"];
    const RU_UK: &[&str] = &["rus", "ukr", "russian", "ukrainian"];
    const ENGLISH: &[&str] = &["eng", "english", "multi", "dual", "dualaudio", "en"];
    const FOREIGN: &[&str] = &[
        "ita",
        "italian",
        "ger",
        "german",
        "deutsch",
        "fre",
        "french",
        "truefrench",
        "vff",
        "vfq",
        "vf2",
        "spa",
        "spanish",
        "castellano",
        "latino",
        "pol",
        "polish",
        "lektor",
        "cze",
        "czech",
        "hun",
        "hungarian",
        "tur",
        "turkish",
        "hindi",
        "tamil",
        "telugu",
        "portuguese",
        "dublado",
        "nordic",
        "swedish",
        "danish",
        "norwegian",
        "finnish",
        "korean",
        "japanese",
        "chinese",
        "mandarin",
        "cantonese",
        "thai",
        "vietnamese",
        "arabic",
        "hebrew",
        "greek",
        "dutch",
    ];
    let lower = release_title.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let has = |set: &[&str]| tokens.iter().any(|t| set.contains(t));
    if has(DUB_GROUPS) || has(SYNTHETIC) {
        return LanguageSignal::Reject;
    }
    // "Dual-Audio" splits into "dual" + "audio"; "ITA-ENG" into "ita" + "eng".
    if has(ENGLISH) {
        return LanguageSignal::Fine;
    }
    if has(RU_UK) {
        return LanguageSignal::Reject;
    }
    if has(FOREIGN) {
        return LanguageSignal::ForeignOnly;
    }
    LanguageSignal::Fine
}

/// Whether a release's Torznab category is a plausible top-level group for the
/// wanted media kind (SKADI-T-0360). Torznab groups by thousands: 2000 Movies,
/// 3000 Audio, 4000 PC/Games, 5000 TV, 6000 XXX, 7000 Books, 8000 Other. A game
/// (4xxx) or movie (2xxx) returned for an audiobook `q=` search is off-category
/// and gets rejected before it can win on a coincidental title match.
fn category_group_ok(kind: skadi_core::MediaKind, cat: skadi_indexers::Category) -> bool {
    use skadi_core::MediaKind::*;
    let group = cat.0 / 1000;
    match kind {
        Movie => group == 2,
        Series => group == 5,
        // Audiobooks are tagged Audio (3xxx) on audio trackers and Books (7xxx) /
        // Other (8xxx) on book trackers — accept all three; reject games/movies/TV.
        Audiobook | Book => matches!(group, 3 | 7 | 8),
        Music => group == 3,
        Subtitle => true,
    }
}

/// Whether a release's byte size is plausible for the wanted media kind
/// (SKADI-T-0362). Deliberately GENEROUS — only rejects gross outliers (a 20 KB
/// "audiobook" fake, or a 40 GB one that's really a game/movie the category gate
/// missed) so a legitimate long unabridged pack is never dropped. Size `0` means
/// the indexer didn't report it (or a manual paste) → always passes.
/// Nominal runtimes, in minutes, for the per-quality size ceiling
/// (SKADI-T-0439). We do not track per-item runtime, so the ceiling is applied
/// against a generous nominal figure rather than the real one.
///
/// Generous on purpose: a false *rejection* silently loses a legitimate release,
/// while a false *acceptance* only lets a too-large one through to the coarse
/// `size_ok` bound. A 200-minute movie and a 90-minute episode are both well past
/// typical, so the ceiling only catches releases that are wrong by a wide margin —
/// which is exactly what a sanity ceiling should do without real runtime data.
const NOMINAL_MOVIE_MINUTES: u32 = 200;
const NOMINAL_EPISODE_MINUTES: u32 = 90;

/// Whether a classified release's size is plausible for its quality
/// (SKADI-T-0439), the per-quality complement to the coarse [`size_ok`] bound.
///
/// Skipped — returning `true` — whenever we cannot judge fairly:
/// * **Packs.** A season or complete-series pack holds an unknown number of
///   episodes, so any single-item ceiling would reject it outright.
/// * **Remux.** Remux is a modifier, not a source, so a remux classifies to the
///   same Bluray definition as an encode while being several times its size.
/// * **Unknown quality or size.** Nothing to judge against.
fn quality_size_ok(
    release: &skadi_indexers::Release,
    quality: Option<&str>,
    kind: skadi_core::MediaKind,
    is_pack: bool,
    definitions: &[QualityDefinition],
) -> bool {
    use skadi_core::MediaKind::*;
    if release.size == 0 || is_pack {
        return true;
    }
    let minutes = match kind {
        Movie => NOMINAL_MOVIE_MINUTES,
        Series => NOMINAL_EPISODE_MINUTES,
        // Audiobooks, music and subtitles have no runtime-per-quality model;
        // the coarse `size_ok` bound is the whole story for them.
        _ => return true,
    };
    let Some(name) = quality else { return true };
    let Some(def) = definitions.iter().find(|d| d.name == name) else {
        return true;
    };
    if skadi_quality::parse(&release.title)
        .modifiers
        .iter()
        .any(|m| m.eq_ignore_ascii_case("Remux"))
    {
        return true;
    }
    release.size <= skadi_quality::max_size_bytes(def, minutes)
}

fn size_ok(kind: skadi_core::MediaKind, size: u64) -> bool {
    use skadi_core::MediaKind::*;
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if size == 0 {
        return true;
    }
    let (min, max) = match kind {
        // A whole audiobook is at least a few hundred KB; even an 8-book pack is
        // well under 25 GB. Below/above that it isn't a book.
        Audiobook | Book => (64 * KB, 25 * GB),
        Movie => (10 * MB, 300 * GB),
        Series => (10 * MB, 500 * GB),
        Music => (256 * KB, 10 * GB),
        Subtitle => (0, 50 * MB),
    };
    size >= min && size <= max
}

/// How a TV release relates to the wanted [`TvScope`] (SKADI-T-0386).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TvMatch {
    /// Names the wanted episode (`SxxEyy`, an `E`-range containing it, a matching
    /// absolute number, or a matching air date).
    Episode,
    /// A whole-season pack for the wanted season, or a complete-series pack.
    Pack,
}

/// `Complete Series` / `Complete Collection` / `Complete Box Set` — a whole-show
/// pack that carries no season marker at all. Deliberately not bare `COMPLETE`,
/// which season packs also use (`Season.2.COMPLETE`) and which `parse_tv` already
/// resolves via the season marker.
static TV_COMPLETE_SERIES_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:complete|full|entire)[\s._-]+(?:series|collection|box[\s._-]?set|show)\b",
    )
    .unwrap()
});

/// The TV **episode identity gate** (SKADI-T-0386): does `release_title` deliver
/// the wanted season/episode? `None` rejects. A release with no TV markers at all
/// (a same-named feature film — *Defiance*, *Dark Angel 1990*) is rejected, as is
/// any release whose `SxxEyy` names a different season or episode (`S3E2` wanted,
/// `S02E06` offered); specials (`S00Exx`) follow the same rule because the season
/// number is compared exactly. A season pack for the wanted season, or a
/// complete-series pack, is accepted as [`TvMatch::Pack`] — the importer's
/// per-file matcher places every episode it contains. Absolute-numbered (anime)
/// and date-keyed (daily) releases are accepted only when the scope carries the
/// matching `absolute` / `air_date`; a season-pack scope (`episode: None`)
/// accepts only packs.
fn tv_scope_match(release_title: &str, scope: TvScope) -> Option<TvMatch> {
    // A complete-series pack covers every regular season — not the specials.
    if scope.season != 0 && TV_COMPLETE_SERIES_RE.is_match(release_title) {
        return Some(TvMatch::Pack);
    }
    let p = skadi_quality::parse_tv(release_title);
    // Anime absolute numbering — no season to compare; verify by the wanted number.
    if !p.absolute.is_empty() {
        return match scope.absolute {
            Some(a) if scope.episode.is_some() && p.absolute.contains(&a) => Some(TvMatch::Episode),
            _ => None,
        };
    }
    // Daily / date-keyed — verify by the wanted air date.
    if p.season.is_none()
        && let Some(date) = p
            .air_date
            .as_deref()
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    {
        return match scope.air_date {
            Some(d) if scope.episode.is_some() && d == date => Some(TvMatch::Episode),
            _ => None,
        };
    }
    let season = p.season?;
    if season != scope.season {
        return None;
    }
    match scope.episode {
        Some(e) if p.episodes.contains(&e) => Some(TvMatch::Episode),
        // A pack for the wanted season satisfies a single-episode scope too
        // (it contains the episode) and a season-pack scope by definition —
        // except season 0: a "S00" release is a named special or arc
        // (`Archer.2009.S00.Heart.of.Archness` was grabbed for S00E01
        // *Archersaurus*), never a defined set, so specials must match SxxEyy.
        _ if p.full_season && p.episodes.is_empty() && season != 0 => Some(TvMatch::Pack),
        _ => None,
    }
}

/// Why a TV release fails the **episode identity gate** for `scope`, or `None`
/// when it delivers the wanted episode (SKADI-T-0587). The same rule [`decide`]
/// applies, exposed so the interactive `GET …/releases` listing does not mark a
/// different episode of the same show "accepted": a bare-title search returns
/// the show's newest uploads, and the profile gates alone were happy with all
/// of them (Critical Role S04E35 listed 323 accepted rows, none of them S04E35).
pub fn tv_episode_rejection(release_title: &str, scope: TvScope) -> Option<&'static str> {
    match tv_scope_match(release_title, scope) {
        Some(TvMatch::Episode) => None,
        Some(TvMatch::Pack) if scope.episode.is_some() => {
            Some("season pack on a single-episode search")
        }
        Some(TvMatch::Pack) => None,
        None => Some("does not name the wanted episode"),
    }
}

/// Distance in years a movie release may sit from the wanted year and still be
/// the same film (festival-vs-wide dating, re-encode years): `±1` (SKADI-T-0387).
const MOVIE_YEAR_SLACK: u16 = 1;

/// The movie **year gate** (SKADI-T-0387). `Some(true)` = the release names a year
/// within [`MOVIE_YEAR_SLACK`] of `wanted`; `Some(false)` = it names a year that
/// can't be this film (*101 Dalmatians 1961* / *… II (2003)* for the 1996 film);
/// `None` = no year to judge by (many scene names omit it — accepted, but
/// [`decide`] ranks a verified-year match above it). No wanted year ⇒ `None`.
fn movie_year_match(release_title: &str, wanted: Option<u16>) -> Option<bool> {
    let wanted = wanted?;
    let year = skadi_quality::parse(release_title).year?;
    Some(year.abs_diff(wanted) <= MOVIE_YEAR_SLACK)
}

/// Title coverage measured on the release's *show/film title part* only
/// (SKADI-T-0607), not the whole release name. The whole-name check
/// (`title_relevance`) let a release group's tag stand in for the title —
/// `11.22.63.S01E03.720p.HDTV.x264-DIMENSION` covered *Dimension 20* — and it
/// drops number tokens, so the "20" never had to be there. Here the title is
/// what the parser reads before the episode marker (or year), numbers count as
/// words, and the best match over the wanted titles and aliases is taken.
/// Falls back to the whole-name coverage when the parser finds no title.
#[must_use]
pub fn show_title_coverage(wanted_titles: &[String], release_title: &str, tv: bool) -> f32 {
    const STOPWORDS: &[&str] = &["the", "a", "an", "of", "and", "by", "to", "in", "for"];
    fn toks(s: &str) -> HashSet<String> {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| !t.is_empty() && !STOPWORDS.contains(t))
            .filter(|t| t.len() >= 2 || t.bytes().all(|b| b.is_ascii_digit()))
            .map(str::to_string)
            .collect()
    }
    let parsed = if tv {
        skadi_quality::parse_tv(release_title).title
    } else {
        skadi_quality::parse(release_title).title
    };
    let Some(title) = parsed else {
        return skadi_quality::title_relevance(wanted_titles, release_title).coverage;
    };
    let rt = toks(&strip_site_prefix(&title));
    wanted_titles
        .iter()
        .map(|w| {
            let wt = toks(w);
            if wt.is_empty() {
                0.0
            } else {
                wt.intersection(&rt).count() as f32 / wt.len() as f32
            }
        })
        .fold(0.0_f32, f32::max)
}

/// The identity gates of [`decide`] for the interactive release lists
/// (SKADI-T-0607): does this release name the wanted title at all, and — for
/// a film — the right year and not a sequel; for TV, not a spin-off. `evaluate`
/// judges the quality profile only, so without this an interactive "accepted"
/// meant less than the automatic one: on prod a by-hand grab for *The Guest*
/// was handed *The Adam Project* because it was the best-seeded 1080p in the
/// search results. Returns the reason to show, or `None` when it is this title.
#[must_use]
pub fn identity_rejection(
    kind: skadi_core::MediaKind,
    wanted_titles: &[String],
    year: Option<u16>,
    tv: Option<TvScope>,
    release: &skadi_indexers::Release,
) -> Option<String> {
    // Off-category first: a one-word title ("The Guest") covers any release
    // that contains the word, and the adult and software categories are full
    // of them. `decide` drops these before it ever looks at the title.
    if !release.categories.is_empty()
        && !release
            .categories
            .iter()
            .any(|c| category_group_ok(kind, *c))
    {
        return Some("off-category for this kind".into());
    }
    if !size_ok(kind, release.size) {
        return Some("implausible size for this kind".into());
    }
    if show_title_coverage(wanted_titles, &release.title, tv.is_some()) < MIN_TITLE_COVERAGE {
        return Some("does not name the wanted title".into());
    }
    if tv.is_some() {
        return tv_wrong_show_rejection(wanted_titles, &release.title);
    }
    if movie_year_match(&release.title, year) == Some(false) {
        return Some("names a different year".into());
    }
    if sequel_marker_mismatch(wanted_titles, &release.title) {
        return Some("names a sequel or other entry".into());
    }
    None
}

/// Interactive-path twin of the wrong-show gate in [`decide`] (SKADI-T-0607):
/// the reason a release list shows under a spin-off's row, or `None`.
#[must_use]
pub fn tv_wrong_show_rejection(wanted_titles: &[String], release_title: &str) -> Option<String> {
    tv_extra_title_tokens(wanted_titles, release_title)
        .map(|extra| format!("names a different show ({})", extra.join(" ")))
}

/// Drop a leading `www <site> <tld>` / `<site> <tld>` stamp from a parsed
/// title (SKADI-T-0607): "Www UIndex org Critical Role" → "Critical Role".
fn strip_site_prefix(title: &str) -> String {
    static SITE_PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^\s*(?:www[\s._-]+)?[a-z0-9-]+[\s._-]+(?:org|com|net|to|cc|me|io|ws|st|fun)\b[\s._-]*")
            .unwrap()
    });
    SITE_PREFIX.replace(title, "").into_owned()
}

/// Words in a TV release's *show* title that none of the wanted titles carry
/// (SKADI-T-0607): the show part of `Mystery Science Theater 3000 The Return
/// S01E01` has "return" over the wanted `Mystery Science Theater 3000`, so it
/// is a different show sharing a prefix — the revival TVDB files as season 11,
/// not the 1989 season 1. Coverage is 1.0 (every wanted token is present), so
/// the relevance gate alone lets such spin-offs through; on prod that grabbed
/// *LEGO Star Wars: Rebuild the Galaxy* for the umbrella *LEGO Star Wars*
/// entry and would have grabbed *Critical Role Cooldown* for *Critical Role*.
/// Years, country markers and release-state words are not evidence of a
/// different show. `None` when the title cannot be parsed or nothing is extra.
fn tv_extra_title_tokens(wanted_titles: &[String], release_title: &str) -> Option<Vec<String>> {
    const STOPWORDS: &[&str] = &["the", "a", "an", "of", "and", "by", "to", "in", "for"];
    const HARMLESS: &[&str] = &[
        "us",
        "uk",
        "au",
        "ca",
        "nz",
        "usa",
        "gb",
        "tv",
        "complete",
        "internal",
        "proper",
        "repack",
        "uncut",
        "extended",
        "remastered",
        "restored",
        "dual",
        "multi",
        "final",
        "season",
        "series",
        "part",
        "vol",
        "ep",
        "episode",
        "sub",
        "subbed",
        "dubbed",
    ];
    fn toks(s: &str) -> Vec<String> {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| {
                t.len() >= 2 && !t.bytes().all(|b| b.is_ascii_digit()) && !STOPWORDS.contains(t)
            })
            .map(str::to_string)
            .collect()
    }
    let title = skadi_quality::parse_tv(release_title).title?;
    // Indexers prepend their own name ("Www UIndex org Critical Role S03E018",
    // "www torrenting com …"); that is the site talking, not the show.
    let title = strip_site_prefix(&title);
    let wanted: HashSet<String> = wanted_titles.iter().flat_map(|t| toks(t)).collect();
    let mut extra: Vec<String> = toks(&title)
        .into_iter()
        .filter(|t| !wanted.contains(t) && !HARMLESS.contains(&t.as_str()))
        .collect();
    extra.dedup();
    (!extra.is_empty()).then_some(extra)
}

/// Roman numerals / ordinals that mark a sequel or numbered entry when they
/// appear in a release title but in none of the wanted titles (SKADI-T-0387):
/// *101 Dalmatians* wanted, *101 Dalmatians II* offered. Title coverage is 1.0
/// (every wanted token is present) so the relevance gate alone lets it through
/// when the sequel carries no year.
fn sequel_marker_mismatch(wanted_titles: &[String], release_title: &str) -> bool {
    fn is_marker(tok: &str) -> bool {
        matches!(
            tok,
            "ii" | "iii" | "iv" | "v" | "vi" | "vii" | "viii" | "ix" | "x"
        ) || tok.parse::<u8>().is_ok_and(|n| (2..=20).contains(&n))
    }
    let Some(title) = skadi_quality::parse(release_title).title else {
        return false;
    };
    let wanted_tokens: HashSet<String> = wanted_titles
        .iter()
        .flat_map(|t| {
            t.split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
        })
        .collect();
    // Only a marker that *follows* a wanted token counts, so a leading number
    // ("2 Fast 2 Furious" wanted as "Fast Furious") isn't misread; in practice
    // sequel markers trail the shared title.
    let mut seen_wanted = false;
    for tok in title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
    {
        if wanted_tokens.contains(&tok) {
            seen_wanted = true;
        } else if seen_wanted && is_marker(&tok) {
            return true;
        }
    }
    false
}

/// Age tier score for a release (SKADI-T-0378, Radarr/Sonarr-style): newer
/// releases are preferred as a tiebreaker because they typically have fresher
/// seeders and are more likely to complete. Returns a score in descending tiers.
fn age_score(published: DateTime<Utc>) -> u32 {
    let age = Utc::now().signed_duration_since(published);
    let hours = age.num_hours().max(0) as u64;
    match hours {
        0..=1 => 5000,    // < 1 hour: strongly prefer
        2..=24 => 2500,   // < 1 day: prefer
        25..=168 => 1000, // < 1 week: slight prefer
        169..=720 => 500, // < 1 month: minor prefer
        _ => 0,           // older: no bonus
    }
}

/// `decide`: score the candidates and pick the best *allowed* release into
/// `state.chosen`. A candidate qualifies via [`evaluate`] (allowed quality +
/// format floor + min-seeders), off-category + size-sanity gates, and a
/// title-relevance gate (all kinds). Best = title relevance bucket, then
/// quality rank, format score, seeders, release age. No qualifying candidate ⇒ error.
/// The Sonarr/Radarr *revision* of a release: a PROPER or REPACK is a re-release
/// that fixes the original, so at the same quality it is strictly preferred
/// (SKADI-T-0438).
///
/// Without this, the two differ only in seeders and age, so the original — which
/// has had longer to accumulate seeders — reliably beats the fix. Ranked below
/// the quality tier (a PROPER never drags us down a tier) and above the custom
/// format score, matching Sonarr, where revision is part of the quality
/// comparison rather than a scoring bonus.
fn revision_rank(title: &str) -> u8 {
    let m = &skadi_quality::parse(title).modifiers;
    let has = |name: &str| m.iter().any(|x| x.eq_ignore_ascii_case(name));
    // REPACK and PROPER both mean "this supersedes the first upload"; neither is
    // consistently the later fix, so they share a rank rather than inventing an
    // ordering the trackers don't have.
    u8::from(has("Proper") || has("Repack"))
}

pub fn decide(state: &mut AcquireState, scoring: &Scoring<'_>) -> Result<()> {
    decide_with(state, scoring, reachability_policy())
}

/// [`decide`] under an explicit [`ReachabilityPolicy`] (SKADI-T-0598).
pub fn decide_with(
    state: &mut AcquireState,
    scoring: &Scoring<'_>,
    policy: ReachabilityPolicy,
) -> Result<()> {
    // (relevance bucket, rank, fscore, seeders, age, idx). The relevance bucket is
    // coverage+precision scored for ALL media kinds (SKADI-T-0374): audiobooks use
    // additional identity gating (author tokens), while movies/TV rely on title
    // coverage + precision to reject/rank text-fallback results. Age (SKADI-T-0378)
    // is a tiebreaker: newer releases are preferred for fresher seeders.
    // `priority` (SKADI-T-0539) sits below the format score: it breaks ties
    // between otherwise-equal candidates, never justifies a worse release.
    // `revision` (SKADI-T-0438) sits just below the quality tier: a PROPER/REPACK
    // at the same quality wins, but never drags us down a tier.
    // `year_ok` (SKADI-T-0387) sits between the relevance bucket and quality rank:
    // a movie whose release year verifiably matches outranks a yearless one, so a
    // yearless same-title release can't win on encode quality alone. Always `1`
    // for audiobooks/TV (not year-gated).
    // Ranking key, greatest-first: (bucket, year_ok, above_floor, seeder_band,
    // rank, revision, fscore, priority, seeders, age). `above_floor` and
    // `seeder_band` are 0 for everything when the reachability policy is off,
    // which collapses the key to the quality-first order (SKADI-T-0598).
    // `lang_ok` (SKADI-T-0671) leads: a release tagged only with a foreign
    // language loses to any other, however relevant or high-quality it is.
    type Key = (u8, u8, u8, u8, u8, usize, u8, i32, u32, u32, u32);
    let mut best: Option<(Key, usize)> = None;
    let mut accepted: Vec<(Key, usize)> = Vec::new();
    let want_titles = &state.request.titles;
    let audiobook = scoring.audiobook.is_some();
    // The series alias (SKADI-T-0313) is only for finding/gating **packs** — exclude it from the
    // per-book gate so a single release is still judged on the book title (per-book precision).
    let series = state.request.series.as_deref();
    let book_titles: Vec<String> = want_titles
        .iter()
        .filter(|t| Some(t.as_str()) != series)
        .cloned()
        .collect();

    let kind = state.request.kind;
    // Tally every gate's victims alongside the ranking (SKADI-T-0380). Counted
    // in the same pass as the decision so the two can never disagree about why a
    // candidate was dropped.
    let mut tally = DecisionTally {
        considered: state.candidates.len(),
        ..Default::default()
    };
    // Relevance of the currently-best candidate, carried so the chosen release's
    // score lands on the trace without re-scoring it afterwards.
    let mut best_relevance: Option<f32> = None;
    for (idx, release) in state.candidates.iter().enumerate() {
        // Off-category gate (SKADI-T-0360): reject a release the indexer tagged
        // entirely in wrong-media categories (a Game/Movie for an audiobook query).
        // Untagged releases (empty categories) pass — the identity/quality gates
        // handle those.
        if !release.categories.is_empty()
            && !release
                .categories
                .iter()
                .any(|c| category_group_ok(kind, *c))
        {
            tally.reject(RejectReason::Category);
            continue;
        }
        // Size-sanity gate (SKADI-T-0362): a gross size outlier isn't our media.
        if !size_ok(kind, release.size) {
            tally.reject(RejectReason::Size);
            continue;
        }
        // Language (SKADI-T-0671): dubs this library never wants are dropped;
        // a release tagged only with another language ranks below every other.
        let lang_ok: u8 = match language_signal(&release.title) {
            LanguageSignal::Reject => {
                tally.reject(RejectReason::Language);
                continue;
            }
            LanguageSignal::ForeignOnly => 0,
            LanguageSignal::Fine => 1,
        };
        let explanation = explain(release, scoring);
        // Accepted ⇒ the quality is allowed, so `quality_rank` is `Some`.
        let ranked = if explanation.accepted {
            explanation
                .quality_rank
                .map(|rank| (rank, explanation.format_score))
        } else {
            None
        };
        let Some((rank, fscore)) = ranked else {
            tally.reject(reject_category(&explanation).unwrap_or(RejectReason::Quality));
            continue;
        };
        let mut relevance = None;
        let mut year_ok: u8 = 1;
        let bucket = if audiobook {
            match audiobook_bucket(&release.title, &book_titles, series) {
                Some(b) => b,
                None => {
                    tally.reject(RejectReason::Identity);
                    continue;
                }
            }
        } else {
            // Movie/TV title relevance gate (SKADI-T-0374): apply the same coverage/precision
            // scoring used for audiobooks. ID search (TMDB/IMDB) is usually precise, but the
            // title-fallback tier can grab unrelated releases sharing a partial title. The
            // coverage gate rejects those; precision ranks candidates by match tightness.
            let r = skadi_quality::title_relevance(want_titles, &release.title);
            // Coverage on the parsed title part, numbers included (SKADI-T-0607):
            // a group tag or a stray word elsewhere in the name is not the title.
            if show_title_coverage(want_titles, &release.title, state.request.tv.is_some())
                < MIN_TITLE_COVERAGE
            {
                tally.reject(RejectReason::Relevance);
                continue;
            }
            relevance = Some(r.coverage);
            match state.request.tv {
                // TV episode identity gate (SKADI-T-0386): the release must deliver
                // the wanted season/episode (or a pack containing it). Full coverage
                // of the show's title says nothing about *which* episode — or whether
                // it's an episode at all rather than a same-named film.
                Some(scope) => {
                    // Wrong-show gate (SKADI-T-0607): a spin-off or revival that
                    // shares the show's title as a prefix names the same episode
                    // codes; the extra words are the tell.
                    if tv_extra_title_tokens(want_titles, &release.title).is_some() {
                        tally.reject(RejectReason::Identity);
                        continue;
                    }
                    match tv_scope_match(&release.title, scope) {
                        // Sonarr parity (SKADI-T-0402): a whole-season (or complete
                        // series) pack is only a candidate for a **season-scoped**
                        // search. On a single-episode search it is rejected — grabbing
                        // a 65 GB pack for one missing episode is never what the
                        // operator asked for, and because every missing episode of a
                        // season raised its own run, the same pack was snatched once
                        // per episode. The TV wanted query already emits a dedicated
                        // season-pack seed once enough of a season is missing.
                        Some(TvMatch::Pack) if scope.episode.is_some() => {
                            tally.reject(RejectReason::Episode);
                            continue;
                        }
                        Some(TvMatch::Pack) => PACK_BUCKET,
                        Some(TvMatch::Episode) => (r.precision * 4.0).round() as u8,
                        None => {
                            tally.reject(RejectReason::Episode);
                            continue;
                        }
                    }
                }
                None if kind == skadi_core::MediaKind::Movie => {
                    // Movie year gate (SKADI-T-0387): a release naming a year far from
                    // the wanted one is a different film sharing the title (remake,
                    // sequel, original). Yearless releases pass but rank below a
                    // verified-year match; a sequel marker the wanted title lacks
                    // (`… II`, `… 3`) is rejected outright.
                    match movie_year_match(&release.title, state.request.year) {
                        Some(false) => {
                            tally.reject(RejectReason::Year);
                            continue;
                        }
                        Some(true) => {}
                        None => year_ok = 0,
                    }
                    if sequel_marker_mismatch(want_titles, &release.title) {
                        tally.reject(RejectReason::Relevance);
                        continue;
                    }
                    (r.precision * 4.0).round() as u8
                }
                None => (r.precision * 4.0).round() as u8,
            }
        };
        // Per-quality size ceiling (SKADI-T-0439). Runs here, after the quality is
        // classified and after the bucket tells us whether this is a pack — the
        // coarse `size_ok` above only knows the media kind, so a 250 GB "720p
        // HDTV" cleared it comfortably.
        if !quality_size_ok(
            release,
            explanation.quality.as_deref(),
            kind,
            bucket == PACK_BUCKET,
            scoring.definitions,
        ) {
            tally.reject(RejectReason::Size);
            continue;
        }
        let seeders = release.seeders.unwrap_or(0);
        let age = age_score(release.published);
        let revision = revision_rank(&release.title);
        // Indexer priority (SKADI-T-0539), Sonarr's 1-50 with lower meaning more
        // trusted. Inverted here because the tuple is compared greatest-first, and
        // placed BELOW the format score: it is a tie-break between otherwise-equal
        // candidates — the operator preferring one tracker's releases — not a
        // reason to take a worse release from a favoured indexer.
        let priority =
            u32::MAX - indexer_flag(scoring.indexer_flags, release.indexer).map_or(25, |(p, _)| p);
        // Reachability over quality (SKADI-T-0598): above the resolution floor,
        // the seeder band outranks the quality rank. Audiobooks have no
        // resolution and always count as above the floor.
        let (above_floor, band) = if policy.prefer_seeders {
            let above = audiobook
                || explanation
                    .quality
                    .as_deref()
                    .and_then(|name| scoring.definitions.iter().find(|d| d.name == name))
                    .is_some_and(|d| d.resolution >= policy.floor);
            (
                u8::from(above),
                if above { seeder_band(seeders) } else { 0 },
            )
        } else {
            (0, 0)
        };
        let key: Key = (
            lang_ok,
            bucket,
            year_ok,
            above_floor,
            band,
            rank,
            revision,
            fscore,
            priority,
            seeders,
            age,
        );
        if best.is_none_or(|(b, _)| key > b) {
            best = Some((key, idx));
            best_relevance = relevance;
        }
        accepted.push((key, idx));
    }

    tally.chosen_relevance = best_relevance;
    state.tally = Some(tally);
    // The full accepted order, best first, for `snatch` to fall back along
    // (SKADI-T-0589). Same key as the `best` comparison; the index breaks ties
    // in favour of the earlier candidate, matching the strict `>` above.
    accepted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    state.ranked = accepted.iter().map(|c| c.1).collect();

    match best {
        Some((.., idx)) => {
            state.chosen = Some(state.candidates[idx].clone());
            Ok(())
        }
        None => Err(AppError::NotFound(
            "no suitable release: no candidate is allowed by the profile and clears the \
             custom-format floor"
                .into(),
        )),
    }
}

/// `snatch`: hand the chosen release to a matching `Downloader` and store the
/// returned `DownloadHandle`. Idempotent on retry (skips if a handle is already
/// recorded — avoids re-adding the same torrent). The category is taken from
/// the request (first entry, falling back to `Category(2000)` = movies).
pub async fn snatch(
    state: &mut AcquireState,
    downloaders: &[Arc<dyn Downloader>],
    indexers: &[Arc<dyn Indexer>],
) -> Result<()> {
    if state.handle.is_some() {
        return Ok(()); // already snatched (resumed run); leave alone
    }
    let chosen = state
        .chosen
        .clone()
        .ok_or_else(|| AppError::Internal("snatch called with no chosen release".into()))?;
    let category = state
        .request
        .categories
        .first()
        .copied()
        .unwrap_or(Category(2000));
    // The chosen release first, then the rest of `decide`'s order (SKADI-T-0589).
    // A grab that fails before a transfer exists — a detail page that yields no
    // torrent, a downloader refusing the link — used to fail the whole run; the
    // item then retried on its backoff, picked the same dead release again and
    // looped (every LimeTorrents decision on 2026-09-17). Now it moves on to the
    // next-best candidate, and the wrapper blocklists the ones it skipped.
    let chosen_key = skadi_indexers::release_key(&chosen);
    let mut queue: Vec<Release> = vec![chosen];
    queue.extend(
        state
            .ranked
            .iter()
            .filter_map(|&i| state.candidates.get(i).cloned())
            .filter(|r| skadi_indexers::release_key(r) != chosen_key),
    );
    let mut last_err: Option<AppError> = None;
    for release in queue.into_iter().take(SNATCH_MAX_ATTEMPTS) {
        match try_snatch(&release, downloaders, indexers, &category).await {
            Ok(handle) => {
                if skadi_indexers::release_key(&release) != chosen_key {
                    tracing::warn!(
                        acquirable = %state.acquirable.0,
                        release = %release.title,
                        skipped = state.snatch_failures.len(),
                        "snatch: fell through to the next-best candidate"
                    );
                    state.chosen = Some(release);
                }
                state.handle = Some(handle);
                return Ok(());
            }
            Err(failed) => {
                let (blocklist, e) = match failed {
                    SnatchAttemptError::Resolve(e) => (true, e),
                    SnatchAttemptError::Downloader(e) => (false, e),
                };
                tracing::warn!(
                    acquirable = %state.acquirable.0,
                    release = %release.title,
                    release_at_fault = blocklist,
                    "snatch failed, trying the next candidate: {e}"
                );
                state.snatch_failures.push(crate::state::SnatchFailure {
                    release_key: skadi_indexers::release_key(&release),
                    title: release.title.clone(),
                    indexer: release.indexer.to_string(),
                    error: e.to_string(),
                    blocklist,
                });
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::Internal("snatch: nothing to try".into())))
}

/// How many candidates one `snatch` walks before giving up (SKADI-T-0589). Each
/// try can cost a detail-page fetch through FlareSolverr, so this bounds the
/// step's wall-clock; a run that exhausts it fails as before and the next sweep
/// starts from the candidates that are left.
const SNATCH_MAX_ATTEMPTS: usize = 5;

/// Why one grab attempt failed: the release's link could not be resolved (the
/// release is at fault, blocklist it) or the downloader refused the add (a
/// client problem — the release stays grabbable, SKADI-T-0188).
enum SnatchAttemptError {
    Resolve(AppError),
    Downloader(AppError),
}

/// One grab attempt: resolve the release's fetch through its indexer and hand
/// it to a downloader of the matching protocol.
async fn try_snatch(
    release: &Release,
    downloaders: &[Arc<dyn Downloader>],
    indexers: &[Arc<dyn Indexer>],
    category: &Category,
) -> std::result::Result<DownloadHandle, SnatchAttemptError> {
    // Give the originating indexer a chance to resolve the fetch just before the
    // grab — e.g. a cardigann download block that turns a detail-page URL into a
    // magnet (SKADI-T-0306). A no-op for the usual already-final magnet/`.torrent`.
    let fetch = match indexers.iter().find(|ix| ix.id() == release.indexer) {
        Some(ix) => ix
            .resolve_fetch(&release.fetch)
            .await
            .map_err(SnatchAttemptError::Resolve)?,
        None => release.fetch.clone(),
    };
    let release = Release {
        fetch,
        ..release.clone()
    };
    let needed = release_protocol(&release.fetch);
    let downloader = downloaders
        .iter()
        .find(|d| d.protocol() == needed)
        .ok_or_else(|| {
            SnatchAttemptError::Downloader(AppError::Config(format!(
                "no enabled downloader supports protocol {needed:?} for the chosen release"
            )))
        })?;
    downloader
        .add(&release, category)
        .await
        .map_err(SnatchAttemptError::Downloader)
}

/// Receives live download progress from [`monitor`] so the caller can surface
/// `Downloading{progress}` while the transfer runs (SKADI-T-0038). The
/// workflow's task wrapper implements this over the status sink; tests use a
/// recorder. Reports are best-effort — implementations must not fail the poll.
#[async_trait::async_trait]
pub trait ProgressSink: Send + Sync {
    async fn report(&self, progress: f32);
}

/// `monitor`: poll the downloader for `state.handle` until the transfer
/// completes (or fails). On `Completed`, record the file paths in
/// `state.completed_paths` for `import`. Each `Downloading` poll reports its
/// progress to `progress` (when given). Polls up to `max_polls` times with
/// `interval` between checks; if the cap is reached while still downloading,
/// returns a retryable error so a Cloacina-driven re-run can resume the poll
/// (matches the design's "bounded retry, then re-execute" model).
pub async fn monitor(
    state: &mut AcquireState,
    downloaders: &[Arc<dyn Downloader>],
    interval: Duration,
    max_polls: u32,
    progress: Option<&dyn ProgressSink>,
) -> Result<()> {
    let handle = state
        .handle
        .clone()
        .ok_or_else(|| AppError::Internal("monitor called with no download handle".into()))?;
    let release = state
        .chosen
        .as_ref()
        .ok_or_else(|| AppError::Internal("monitor called with no chosen release".into()))?;
    let downloader = downloader_for(release, downloaders)?;

    for poll in 0..max_polls.max(1) {
        match downloader.status(&handle).await? {
            DownloadStatus::Completed { files } => {
                state.completed_paths = Some(files);
                return Ok(());
            }
            DownloadStatus::Failed { reason } => {
                // Non-retryable: T-0033's task wrapper maps this to
                // `FailureReason::DownloadFailed`.
                return Err(AppError::Internal(format!("download failed: {reason}")));
            }
            DownloadStatus::Removed => {
                // Terminal like `Failed`, but the operator took it down: the
                // caller must not blame the release (SKADI-T-0691).
                state.transfer_removed = true;
                return Err(AppError::Internal("download removed".into()));
            }
            DownloadStatus::Downloading { progress: p } => {
                if let Some(sink) = progress {
                    sink.report(p).await;
                }
                if poll + 1 < max_polls.max(1) {
                    tokio::time::sleep(interval).await;
                }
            }
            DownloadStatus::Queued => {
                if poll + 1 < max_polls.max(1) {
                    tokio::time::sleep(interval).await;
                }
            }
        }
    }
    Err(AppError::Network(
        "download still in progress after poll budget exhausted; will retry".into(),
    ))
}

/// The enabled downloader that speaks `release`'s protocol.
fn downloader_for<'a>(
    release: &Release,
    downloaders: &'a [Arc<dyn Downloader>],
) -> Result<&'a Arc<dyn Downloader>> {
    let needed = release_protocol(&release.fetch);
    downloaders
        .iter()
        .find(|d| d.protocol() == needed)
        .ok_or_else(|| {
            AppError::Config(format!(
                "no enabled downloader supports protocol {needed:?} for the chosen release"
            ))
        })
}

/// Drop the run's transfer from its download client (data included) — used when
/// `monitor` gives up on a stalled download (SKADI-T-0388) so the client isn't
/// left seeding/waiting on a torrent nobody will import. Best-effort: a
/// missing handle or an already-gone download is not an error worth failing on.
pub async fn cancel_transfer(state: &AcquireState, downloaders: &[Arc<dyn Downloader>]) {
    let (Some(handle), Some(release)) = (state.handle.as_ref(), state.chosen.as_ref()) else {
        return;
    };
    match downloader_for(release, downloaders) {
        Ok(d) => {
            if let Err(e) = d.remove(handle, true).await {
                tracing::warn!(error = %e, acquirable = %state.acquirable.0, "removing stalled download failed (non-fatal)");
            }
        }
        Err(e) => tracing::warn!(error = %e, "no downloader to cancel stalled transfer"),
    }
}

/// Drop the run's transfer from its download client **without** deleting data,
/// after a move-mode import has already relocated the files (SKADI-T-0545).
///
/// `delete_data: false` is the whole point. The files have been *moved*: the
/// path the client knows is empty and the library copy is the only one left, so
/// asking the client to delete its data is at best a no-op and at worst — on a
/// client that resolves the path differently, or one that followed the move —
/// a request to delete the library copy. [`cancel_transfer`] passes `true`
/// because there the download is being abandoned and its data is genuinely junk.
///
/// Best-effort, like `cancel_transfer`: the import has already succeeded and is
/// recorded, so a client we cannot reach leaves a stale entry, never a lost
/// acquisition.
pub async fn drop_transfer_after_move(state: &AcquireState, downloaders: &[Arc<dyn Downloader>]) {
    let (Some(handle), Some(release)) = (state.handle.as_ref(), state.chosen.as_ref()) else {
        return;
    };
    match downloader_for(release, downloaders) {
        Ok(d) => {
            if let Err(e) = d.remove(handle, false).await {
                tracing::warn!(
                    error = %e,
                    acquirable = %state.acquirable.0,
                    "move-on-import: removing the transfer failed (the import is recorded)"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "move-on-import: no downloader to drop the transfer"),
    }
}

/// `import`: hand the completed transfer to the importer with the domain's
/// `AcquirableMatcher` (held inside the `Importer` impl) and record the
/// outcome. An import that produced zero placed files is a hard failure
/// (T-0033's task wrapper maps to `FailureReason::ImportFailed`).
pub async fn import(state: &mut AcquireState, importer: &dyn Importer) -> Result<()> {
    let handle = state
        .handle
        .clone()
        .ok_or_else(|| AppError::Internal("import called with no download handle".into()))?;
    let files = state
        .completed_paths
        .clone()
        .ok_or_else(|| AppError::Internal("import called before monitor recorded files".into()))?;
    let category = state
        .request
        .categories
        .first()
        .copied()
        .unwrap_or(Category(2000));
    let completed = CompletedDownload {
        handle,
        files,
        category: format!("{}", category.0),
    };
    let mut outcome = importer.import(completed).await?;
    if outcome.imported.is_empty() {
        // Nothing new landed, but the matched destinations were already occupied
        // (SKADI-T-0385): this is a re-download of something an earlier run
        // imported — the duplicate-run loop — and the library already holds the
        // file. Count those as satisfied so the acquirable is marked `Imported`
        // instead of `Failed` and re-grabbed next sweep.
        if !outcome.already_present.is_empty() {
            tracing::info!(
                acquirable = %state.acquirable.0,
                files = outcome.already_present.len(),
                "import: destination already holds this item; treating as imported"
            );
            outcome.imported = std::mem::take(&mut outcome.already_present);
        } else {
            // Keep the outcome on a failure too (SKADI-T-0592): the task wrapper
            // reads it to tell "nothing here was media" from "something broke".
            let summary = import_failure_summary(&outcome);
            state.outcome = Some(outcome);
            return Err(AppError::Internal(format!(
                "import produced no placed files ({summary})"
            )));
        }
    }
    state.outcome = Some(outcome);
    Ok(())
}

/// Whether a failed import says the download held **nothing usable**
/// (SKADI-T-0592): every file was rejected outright — not media, a sample, no
/// matching item — and none was quarantined or failed placement. A quarantined
/// file is probable media set aside for review; a failed placement is our own
/// problem (a dropped mount, a full disk). Both mean the data is worth keeping.
#[must_use]
pub fn import_was_unusable(outcome: &skadi_importer::ImportOutcome) -> bool {
    outcome.imported.is_empty()
        && outcome.already_present.is_empty()
        && outcome.quarantined.is_empty()
        && outcome.failed.is_empty()
        && !outcome.rejected.is_empty()
}

/// Drop a download whose import placed nothing usable (SKADI-T-0592), data
/// included — the fake `.exe` release, the pack of screencaps. Best-effort like
/// [`cancel_transfer`]; the release is already blocklisted by the caller.
pub async fn drop_unusable_download(state: &AcquireState, downloaders: &[Arc<dyn Downloader>]) {
    let (Some(handle), Some(release)) = (state.handle.as_ref(), state.chosen.as_ref()) else {
        return;
    };
    match downloader_for(release, downloaders) {
        Ok(d) => match d.remove(handle, true).await {
            Ok(()) => tracing::info!(
                acquirable = %state.acquirable.0,
                release = %release.title,
                "removed a download that held no usable media (data deleted)"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                acquirable = %state.acquirable.0,
                "removing an unusable download failed (non-fatal)"
            ),
        },
        Err(e) => tracing::warn!(error = %e, "no downloader to drop the unusable download"),
    }
}

/// Why an import placed nothing, compactly: per-bucket counts plus the distinct
/// reject/fail reasons (SKADI-T-0385) — so the `import_failed` trace says *what*
/// the importer objected to instead of a bare count.
fn import_failure_summary(outcome: &skadi_importer::ImportOutcome) -> String {
    let mut reasons: Vec<&str> = outcome
        .rejected
        .iter()
        .chain(outcome.failed.iter())
        .map(|(_, why)| why.as_str())
        .collect();
    reasons.sort_unstable();
    reasons.dedup();
    let mut parts = vec![format!("rejected: {}", outcome.rejected.len())];
    if !outcome.failed.is_empty() {
        parts.push(format!("failed: {}", outcome.failed.len()));
    }
    if !outcome.quarantined.is_empty() {
        parts.push(format!("quarantined: {}", outcome.quarantined.len()));
    }
    if !reasons.is_empty() {
        parts.push(format!("reasons: {}", reasons.join("; ")));
    }
    parts.join(", ")
}

/// Build the notification payload for the current run (title/year from the
/// request, quality from the chosen release).
fn event_payload(state: &AcquireState) -> EventPayload {
    EventPayload {
        title: state.request.titles.first().cloned().unwrap_or_default(),
        year: state.request.year,
        quality: state
            .chosen
            .as_ref()
            .and_then(|r| r.parsed.resolution.clone()),
        message: None,
    }
}

/// Fan one event out to every notifier that wants its kind. Notifier failures
/// are non-fatal (logged and swallowed) — a broken webhook must never block the
/// pipeline.
async fn fan_event(
    event: &NotificationEvent,
    notifiers: &[Arc<dyn Notifier>],
    item_tags: Option<&[String]>,
) {
    for n in notifiers {
        // Tag scoping (SKADI-T-0560). `None` means no item context — the same
        // distinction the search fan-out draws (SKADI-T-0556): an event not tied
        // to a library item cannot be scoped against one, so every notifier
        // hears it.
        if let Some(tags) = item_tags
            && !n.applies_to_tags(tags)
        {
            continue;
        }
        if n.wants(event)
            && let Err(e) = n.notify(event).await
        {
            tracing::warn!(error = %e, "notifier failed (non-fatal)");
        }
    }
}

/// `notify`: emit `Imported` — or `Upgraded` when this import replaced a file we
/// already held (SKADI-T-0538).
///
/// The distinction comes from `ImportOutcome.replaced`, never from "a file
/// existed at the destination": a re-import of the same release also finds a file
/// there and would produce a false `Upgraded` for something that upgraded
/// nothing. `replaced` is populated only by the supersede path, which is the
/// actual fact.
///
/// The payload's `message` names what it replaced, because "upgraded to 2160p" is
/// only useful next to what it replaced.
pub async fn notify(state: &AcquireState, notifiers: &[Arc<dyn Notifier>]) -> Result<()> {
    let replaced: &[std::path::PathBuf] = state
        .outcome
        .as_ref()
        .map_or(&[], |o| o.replaced.as_slice());
    let event = if replaced.is_empty() {
        NotificationEvent::Imported(event_payload(state))
    } else {
        let mut payload = event_payload(state);
        payload.message = Some(format!(
            "replaced {}",
            replaced
                .iter()
                .filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        NotificationEvent::Upgraded(payload)
    };
    fan_event(
        &event,
        notifiers,
        Some(&state.request.tags.clone().unwrap_or_default()),
    )
    .await;
    Ok(())
}

/// Emit a `Failed` event when an acquire run gives up (SKADI-T-0504).
///
/// `Failed` was defined but never produced, so an operator subscribed to it
/// heard nothing — and a run that fails is precisely the case they wanted told
/// about, since a success shows up in the library on its own. `reason` is carried
/// in the payload's `message`, which is the only place a receiver can learn *why*
/// without reading the daemon's logs.
///
/// Same non-fatal fan-out as [`notify`]: a notifier that is down must not turn a
/// failed acquire into a failed *pipeline*, which would mask the original cause.
pub async fn notify_failed(
    state: &AcquireState,
    reason: &str,
    notifiers: &[Arc<dyn Notifier>],
) -> Result<()> {
    let mut payload = event_payload(state);
    payload.message = Some(reason.to_string());
    fan_event(
        &NotificationEvent::Failed(payload),
        notifiers,
        Some(&state.request.tags.clone().unwrap_or_default()),
    )
    .await;
    Ok(())
}

/// Emit a `Grabbed` event at the snatch boundary — a "started downloading X"
/// signal independent of whether the download/import later succeeds
/// (SKADI-T-0037). Same non-fatal fan-out as [`notify`].
pub async fn notify_grabbed(state: &AcquireState, notifiers: &[Arc<dyn Notifier>]) -> Result<()> {
    fan_event(
        &NotificationEvent::Grabbed(event_payload(state)),
        notifiers,
        Some(&state.request.tags.clone().unwrap_or_default()),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod snatch_monitor_tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use skadi_core::{DownloaderId, ExternalIds, IndexerId, MediaKind, ProfileId, Protocol};
    use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
    use skadi_importer::AcquirableRef;
    use skadi_indexers::{Release, ReleaseFetch};
    use skadi_quality::parse;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use crate::state::SearchSpec;

    #[test]
    fn circuit_closed_for_healthy_or_below_threshold() {
        let now = Utc::now();
        // Untracked indexer → closed (always searched).
        assert!(!search_circuit_open(None, now));
        // Below the failure threshold → closed.
        let h = IndexerHealth {
            consecutive_failures: DEFAULT_CIRCUIT_THRESHOLD - 1,
            last_failure: Some(now),
            ..Default::default()
        };
        assert!(!search_circuit_open(Some(&h), now));
    }

    #[test]
    fn circuit_open_within_cooldown() {
        let now = Utc::now();
        let h = IndexerHealth {
            consecutive_failures: DEFAULT_CIRCUIT_THRESHOLD,
            last_failure: Some(now - chrono::Duration::seconds(10)),
            ..Default::default()
        };
        assert!(
            search_circuit_open(Some(&h), now),
            "tripped + recent failure → skip"
        );
    }

    #[test]
    fn circuit_half_open_after_cooldown() {
        let now = Utc::now();
        let h = IndexerHealth {
            consecutive_failures: DEFAULT_CIRCUIT_THRESHOLD + 5,
            last_failure: Some(now - chrono::Duration::seconds(DEFAULT_CIRCUIT_COOLDOWN_SECS + 1)),
            ..Default::default()
        };
        assert!(
            !search_circuit_open(Some(&h), now),
            "cooldown elapsed → half-open re-probe"
        );
    }

    fn release_for(fetch: ReleaseFetch) -> Release {
        Release {
            indexer: IndexerId::new(),
            title: "Movie.2020.1080p.BluRay.x264-GRP".into(),
            fetch,
            size: 8_000_000_000,
            published: Utc::now(),
            seeders: Some(10),
            categories: Vec::new(),
            parsed: parse("Movie.2020.1080p.BluRay.x264-GRP"),
        }
    }

    fn state_with_chosen(fetch: ReleaseFetch) -> AcquireState {
        let mut s = AcquireState::new(
            AcquirableRef("ed-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![Category(2000)],
                tv: None,
                series: None,
                tags: None,
            },
            ProfileId::new(),
        );
        s.chosen = Some(release_for(fetch));
        s
    }

    /// A mock downloader scripted with a sequence of statuses to return on each
    /// poll, plus an optional injection of an error on the Nth `add`/`status`.
    struct MockDownloader {
        id: DownloaderId,
        protocol: Protocol,
        sequence: Mutex<Vec<DownloadStatus>>,
        add_calls: Mutex<u32>,
        next_id: Mutex<u32>,
    }

    impl MockDownloader {
        fn new(protocol: Protocol, sequence: Vec<DownloadStatus>) -> Self {
            Self {
                id: DownloaderId::new(),
                protocol,
                sequence: Mutex::new(sequence),
                add_calls: Mutex::new(0),
                next_id: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Downloader for MockDownloader {
        fn id(&self) -> DownloaderId {
            self.id
        }
        fn protocol(&self) -> Protocol {
            self.protocol
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn add(&self, _r: &Release, c: &Category) -> Result<DownloadHandle> {
            *self.add_calls.lock().unwrap() += 1;
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            Ok(DownloadHandle {
                native_id: format!("h{}", *next),
                category: format!("{}", c.0),
            })
        }
        async fn status(&self, _h: &DownloadHandle) -> Result<DownloadStatus> {
            let mut seq = self.sequence.lock().unwrap();
            if seq.is_empty() {
                Ok(DownloadStatus::Failed {
                    reason: "no more scripted statuses".into(),
                })
            } else {
                Ok(seq.remove(0))
            }
        }
        async fn remove(&self, _h: &DownloadHandle, _delete: bool) -> Result<()> {
            Ok(())
        }
    }

    fn d(downloader: MockDownloader) -> Arc<dyn Downloader> {
        Arc::new(downloader)
    }

    #[tokio::test]
    async fn snatch_picks_matching_protocol_and_stores_handle() {
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        let torrent = d(MockDownloader::new(Protocol::Torrent, vec![]));
        let downloaders = vec![torrent.clone()];

        snatch(&mut state, &downloaders, &[]).await.unwrap();
        assert!(state.handle.is_some());
        let h = state.handle.as_ref().unwrap();
        assert_eq!(h.category, "2000");
        assert_eq!(h.native_id, "h1");
    }

    /// An indexer whose `resolve_fetch` refuses links containing "dead" — the
    /// LimeTorrents shape, where the detail page yields no torrent.
    struct ResolvingIndexer {
        id: IndexerId,
    }

    #[async_trait]
    impl skadi_indexers::Indexer for ResolvingIndexer {
        fn id(&self) -> IndexerId {
            self.id
        }
        fn protocol(&self) -> Protocol {
            Protocol::Torrent
        }
        fn supports(&self, _kind: MediaKind) -> bool {
            true
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn capabilities(&self) -> Result<skadi_indexers::IndexerCaps> {
            unreachable!("not searched in this test")
        }
        async fn search(&self, _query: &dyn skadi_indexers::SearchQuery) -> Result<Vec<Release>> {
            Ok(Vec::new())
        }
        async fn resolve_fetch(&self, fetch: &ReleaseFetch) -> Result<ReleaseFetch> {
            match fetch {
                ReleaseFetch::TorrentUrl(u) if u.contains("dead") => Err(AppError::Network(
                    format!("{u} returned 33476 bytes that are not a torrent file"),
                )),
                other => Ok(other.clone()),
            }
        }
    }

    /// The production loop (SKADI-T-0589): the best-ranked release's detail page
    /// yields no torrent. The run used to fail there and re-pick the same
    /// release next sweep; now it falls through to the next-ranked candidate,
    /// records the skip for the wrapper to blocklist, and `chosen` follows.
    #[tokio::test]
    async fn snatch_falls_through_to_the_next_ranked_candidate() {
        let indexer = Arc::new(ResolvingIndexer {
            id: IndexerId::new(),
        });
        let dead = Release {
            indexer: indexer.id,
            ..release_for(ReleaseFetch::TorrentUrl(
                "https://lime/dead-page.html".into(),
            ))
        };
        let good = Release {
            indexer: indexer.id,
            title: "Movie.2020.1080p.WEB-GOOD".into(),
            ..release_for(ReleaseFetch::Magnet("magnet:?xt=urn:btih:good".into()))
        };
        let mut state = state_with_chosen(ReleaseFetch::TorrentUrl(
            "https://lime/dead-page.html".into(),
        ));
        state.chosen = Some(dead.clone());
        state.candidates = vec![good.clone(), dead.clone()];
        state.ranked = vec![1, 0];
        let torrent = d(MockDownloader::new(Protocol::Torrent, vec![]));
        let indexers: Vec<Arc<dyn skadi_indexers::Indexer>> = vec![indexer];

        snatch(&mut state, &[torrent], &indexers).await.unwrap();

        assert!(state.handle.is_some(), "the fallback was grabbed");
        assert_eq!(state.chosen.as_ref().unwrap().title, good.title);
        assert_eq!(state.snatch_failures.len(), 1);
        let f = &state.snatch_failures[0];
        assert_eq!(f.release_key, skadi_indexers::release_key(&dead));
        assert!(f.error.contains("not a torrent file"), "{}", f.error);
        assert!(f.blocklist, "an unresolvable link is the release's fault");
    }

    /// A downloader refusing the add is a client problem, not the release's
    /// (SKADI-T-0188): the skip is recorded for the trace but not blocklisted.
    #[tokio::test]
    async fn snatch_does_not_blame_the_release_for_a_refusing_downloader() {
        struct Refusing;
        #[async_trait]
        impl Downloader for Refusing {
            fn id(&self) -> DownloaderId {
                DownloaderId::new()
            }
            fn protocol(&self) -> Protocol {
                Protocol::Torrent
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
            async fn add(&self, _r: &Release, _c: &Category) -> Result<DownloadHandle> {
                Err(AppError::Network("client busy".into()))
            }
            async fn status(&self, _h: &DownloadHandle) -> Result<DownloadStatus> {
                Ok(DownloadStatus::Queued)
            }
            async fn remove(&self, _h: &DownloadHandle, _d: bool) -> Result<()> {
                Ok(())
            }
        }
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        let err = snatch(&mut state, &[Arc::new(Refusing)], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
        assert_eq!(state.snatch_failures.len(), 1);
        assert!(
            !state.snatch_failures[0].blocklist,
            "the release stays grabbable"
        );
    }

    /// With nothing left to fall back on the step still fails, with the last
    /// error, so the wrapper's `Failed{retry_at}` path is unchanged.
    #[tokio::test]
    async fn snatch_fails_when_every_candidate_is_unsnatchable() {
        let indexer = Arc::new(ResolvingIndexer {
            id: IndexerId::new(),
        });
        let dead = Release {
            indexer: indexer.id,
            ..release_for(ReleaseFetch::TorrentUrl(
                "https://lime/dead-page.html".into(),
            ))
        };
        let mut state = state_with_chosen(ReleaseFetch::TorrentUrl(
            "https://lime/dead-page.html".into(),
        ));
        state.chosen = Some(dead.clone());
        state.candidates = vec![dead];
        state.ranked = vec![0];
        let torrent = d(MockDownloader::new(Protocol::Torrent, vec![]));
        let indexers: Vec<Arc<dyn skadi_indexers::Indexer>> = vec![indexer];
        let err = snatch(&mut state, &[torrent], &indexers).await.unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
        assert!(state.handle.is_none());
        assert_eq!(state.snatch_failures.len(), 1);
    }

    #[tokio::test]
    async fn snatch_is_idempotent_when_handle_already_set() {
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "preset".into(),
            category: "2000".into(),
        });
        // A mock that would panic if `add` were called twice via counter check.
        let torrent = MockDownloader::new(Protocol::Torrent, vec![]);
        let downloaders = vec![Arc::new(torrent) as Arc<dyn Downloader>];
        snatch(&mut state, &downloaders, &[]).await.unwrap();
        assert_eq!(state.handle.as_ref().unwrap().native_id, "preset");
    }

    #[tokio::test]
    async fn snatch_errors_when_no_downloader_matches_protocol() {
        let mut state = state_with_chosen(ReleaseFetch::NzbUrl("http://x/y.nzb".into()));
        // Only Torrent enabled, but release needs Usenet.
        let torrent = d(MockDownloader::new(Protocol::Torrent, vec![]));
        let err = snatch(&mut state, &[torrent], &[]).await.unwrap_err();
        assert!(matches!(err, AppError::Config(_)));
    }

    #[tokio::test]
    async fn monitor_reaches_completed_via_intermediate_polls() {
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        let downloader = d(MockDownloader::new(
            Protocol::Torrent,
            vec![
                DownloadStatus::Queued,
                DownloadStatus::Downloading { progress: 0.5 },
                DownloadStatus::Completed {
                    files: vec![PathBuf::from("/dl/movie.mkv")],
                },
            ],
        ));
        monitor(
            &mut state,
            &[downloader],
            Duration::from_millis(1),
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            state.completed_paths.as_deref(),
            Some(&[PathBuf::from("/dl/movie.mkv")][..])
        );
    }

    #[tokio::test]
    async fn monitor_returns_retryable_when_poll_budget_exhausted() {
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        let downloader = d(MockDownloader::new(
            Protocol::Torrent,
            vec![
                DownloadStatus::Downloading { progress: 0.1 },
                DownloadStatus::Downloading { progress: 0.2 },
            ],
        ));
        let err = monitor(&mut state, &[downloader], Duration::from_millis(1), 2, None)
            .await
            .unwrap_err();
        // Network = retryable, distinguishing from a hard download failure.
        assert!(matches!(err, AppError::Network(_)));
        assert!(state.completed_paths.is_none());
    }

    #[tokio::test]
    async fn monitor_with_a_single_poll_never_sleeps() {
        // The live `monitor` step polls ONCE per Cloacina execution and lets the
        // task's retry delay pace the watch (SKADI-T-0388): a single poll must
        // return immediately, not sit out the interval first.
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        let downloader = d(MockDownloader::new(
            Protocol::Torrent,
            vec![DownloadStatus::Downloading { progress: 0.3 }],
        ));
        let started = std::time::Instant::now();
        let err = monitor(&mut state, &[downloader], Duration::from_secs(30), 1, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancel_transfer_is_a_no_op_without_a_handle_and_tolerates_remove_errors() {
        struct FailingRemove(MockDownloader);
        #[async_trait]
        impl Downloader for FailingRemove {
            fn id(&self) -> DownloaderId {
                self.0.id()
            }
            fn protocol(&self) -> Protocol {
                self.0.protocol()
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
            async fn add(&self, r: &Release, c: &Category) -> Result<DownloadHandle> {
                self.0.add(r, c).await
            }
            async fn status(&self, h: &DownloadHandle) -> Result<DownloadStatus> {
                self.0.status(h).await
            }
            async fn remove(&self, _h: &DownloadHandle, _delete: bool) -> Result<()> {
                Err(AppError::Network("client went away".into()))
            }
        }
        let downloader: Arc<dyn Downloader> = Arc::new(FailingRemove(MockDownloader::new(
            Protocol::Torrent,
            vec![],
        )));

        // Nothing snatched yet: nothing to cancel, must not panic.
        let state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        cancel_transfer(&state, std::slice::from_ref(&downloader)).await;

        // Snatched, but the client refuses the removal: best-effort, still returns.
        let mut state = state;
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        cancel_transfer(&state, &[downloader]).await;
    }

    #[tokio::test]
    async fn drop_after_move_removes_the_transfer_but_never_its_data() {
        // The whole point of SKADI-T-0545 is the `delete_data` flag, so the mock
        // has to record it — `MockDownloader::remove` ignores it, which would
        // make this test pass against a `true`.
        struct RecordingRemove {
            inner: MockDownloader,
            deletes: Mutex<Vec<bool>>,
        }
        #[async_trait]
        impl Downloader for RecordingRemove {
            fn id(&self) -> DownloaderId {
                self.inner.id()
            }
            fn protocol(&self) -> Protocol {
                self.inner.protocol()
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
            async fn add(&self, r: &Release, c: &Category) -> Result<DownloadHandle> {
                self.inner.add(r, c).await
            }
            async fn status(&self, h: &DownloadHandle) -> Result<DownloadStatus> {
                self.inner.status(h).await
            }
            async fn remove(&self, _h: &DownloadHandle, delete: bool) -> Result<()> {
                self.deletes.lock().unwrap().push(delete);
                Ok(())
            }
        }

        let rec = Arc::new(RecordingRemove {
            inner: MockDownloader::new(Protocol::Torrent, vec![]),
            deletes: Mutex::new(Vec::new()),
        });
        let downloaders: Vec<Arc<dyn Downloader>> = vec![rec.clone()];

        // No handle yet ⇒ nothing to drop, and no call.
        let state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        drop_transfer_after_move(&state, &downloaders).await;
        assert!(
            rec.deletes.lock().unwrap().is_empty(),
            "nothing was snatched, so nothing should have been removed"
        );

        let mut state = state;
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });

        drop_transfer_after_move(&state, &downloaders).await;
        assert_eq!(
            *rec.deletes.lock().unwrap(),
            vec![false],
            "a moved import must drop the entry without deleting data — the files \
             are gone from that path and the library copy is the only one left"
        );

        // Contrast with the abandon path, which genuinely wants the data gone.
        // If these two ever collapse into one call, this assertion catches it.
        cancel_transfer(&state, &downloaders).await;
        assert_eq!(*rec.deletes.lock().unwrap(), vec![false, true]);
    }

    #[tokio::test]
    async fn drop_after_move_tolerates_a_client_that_refuses() {
        struct FailingRemove(MockDownloader);
        #[async_trait]
        impl Downloader for FailingRemove {
            fn id(&self) -> DownloaderId {
                self.0.id()
            }
            fn protocol(&self) -> Protocol {
                self.0.protocol()
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
            async fn add(&self, r: &Release, c: &Category) -> Result<DownloadHandle> {
                self.0.add(r, c).await
            }
            async fn status(&self, h: &DownloadHandle) -> Result<DownloadStatus> {
                self.0.status(h).await
            }
            async fn remove(&self, _h: &DownloadHandle, _delete: bool) -> Result<()> {
                Err(AppError::Network("client went away".into()))
            }
        }
        let downloader: Arc<dyn Downloader> = Arc::new(FailingRemove(MockDownloader::new(
            Protocol::Torrent,
            vec![],
        )));
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        // The import is already recorded; an unreachable client is clutter, never
        // a failure. This returns rather than panicking.
        drop_transfer_after_move(&state, &[downloader]).await;
    }

    #[tokio::test]
    async fn monitor_returns_hard_error_on_failed_status() {
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        let downloader = d(MockDownloader::new(
            Protocol::Torrent,
            vec![DownloadStatus::Failed {
                reason: "tracker rejected".into(),
            }],
        ));
        let err = monitor(&mut state, &[downloader], Duration::from_millis(1), 5, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test]
    async fn monitor_reports_progress_to_the_sink() {
        struct Recorder(std::sync::Mutex<Vec<f32>>);
        #[async_trait::async_trait]
        impl ProgressSink for Recorder {
            async fn report(&self, progress: f32) {
                self.0.lock().unwrap().push(progress);
            }
        }

        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        let downloader = d(MockDownloader::new(
            Protocol::Torrent,
            vec![
                DownloadStatus::Queued,
                DownloadStatus::Downloading { progress: 0.25 },
                DownloadStatus::Downloading { progress: 0.75 },
                DownloadStatus::Completed { files: vec![] },
            ],
        ));
        let recorder = Recorder(std::sync::Mutex::new(vec![]));
        monitor(
            &mut state,
            &[downloader],
            Duration::from_millis(1),
            10,
            Some(&recorder),
        )
        .await
        .unwrap();
        assert_eq!(
            *recorder.0.lock().unwrap(),
            vec![0.25, 0.75],
            "each Downloading poll reported; Queued/Completed did not"
        );
    }

    // --- import / notify ---

    use skadi_importer::{
        AcquirableMatch, AcquirableMatcher, AcquirableRef as AR2, DefaultImporter,
    };
    use skadi_notify::{
        EventPayload as EP2, NotificationEvent as NE2, NotificationKind, Notifier as N2,
    };
    use skadi_quality::ParsedRelease;

    struct ToTmp(PathBuf);
    impl AcquirableMatcher for ToTmp {
        fn match_file(
            &self,
            _parsed: &ParsedRelease,
            source: &std::path::Path,
            _completed: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            vec![AcquirableMatch::new(
                AR2("ed-1".into()),
                self.0.join(source.file_name().unwrap()),
            )]
        }
    }

    #[tokio::test]
    async fn import_calls_importer_and_records_outcome() {
        // Make a real on-disk source so DefaultImporter's hardlink+copy works.
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("Movie.mkv");
        std::fs::write(&src, b"x").unwrap();

        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        state.completed_paths = Some(vec![src.clone()]);

        let importer: Arc<dyn skadi_importer::Importer> =
            Arc::new(DefaultImporter::new(ToTmp(dst_dir.path().to_path_buf())));
        super::import(&mut state, importer.as_ref()).await.unwrap();

        let outcome = state.outcome.as_ref().unwrap();
        assert_eq!(outcome.imported.len(), 1);
        assert!(dst_dir.path().join("Movie.mkv").exists());
    }

    #[test]
    fn import_was_unusable_needs_only_outright_rejections() {
        use skadi_importer::ImportOutcome;
        let rejected = |n: usize| -> Vec<(PathBuf, String)> {
            (0..n)
                .map(|i| {
                    (
                        PathBuf::from(format!("/dl/f{i}.exe")),
                        "no matching acquirable".into(),
                    )
                })
                .collect()
        };
        let mut o = ImportOutcome {
            rejected: rejected(2),
            ..Default::default()
        };
        assert!(import_was_unusable(&o), "all rejected ⇒ unusable");
        o.quarantined.push(PathBuf::from("/review/maybe.mkv"));
        assert!(
            !import_was_unusable(&o),
            "quarantined media is worth keeping"
        );
        o.quarantined.clear();
        o.failed
            .push((PathBuf::from("/dl/f0.mkv"), "disk full".into()));
        assert!(
            !import_was_unusable(&o),
            "a placement error is our fault, not the data's"
        );
        assert!(
            !import_was_unusable(&ImportOutcome::default()),
            "an empty download is not 'rejected'"
        );
    }

    #[tokio::test]
    async fn import_errors_when_no_files_placed() {
        struct RejectAll;
        impl AcquirableMatcher for RejectAll {
            fn match_file(
                &self,
                _: &ParsedRelease,
                _: &std::path::Path,
                _: &CompletedDownload,
            ) -> Vec<AcquirableMatch> {
                vec![]
            }
        }
        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        state.completed_paths = Some(vec![PathBuf::from("/nowhere/Movie.mkv")]);
        let importer: Arc<dyn skadi_importer::Importer> = Arc::new(DefaultImporter::new(RejectAll));
        let err = super::import(&mut state, importer.as_ref())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
        // The outcome survives the failure so the wrapper can classify it
        // (SKADI-T-0592).
        let outcome = state.outcome.as_ref().expect("outcome kept on failure");
        assert!(
            import_was_unusable(outcome),
            "every file rejected ⇒ unusable"
        );
        // SKADI-T-0385: the failure names the importer's reasons, not just a count.
        let msg = err.to_string();
        assert!(msg.contains("rejected: 1"), "{msg}");
        assert!(msg.contains("no matching acquirable"), "{msg}");
    }

    /// SKADI-T-0385: a re-download of something already imported hits the `Skip`
    /// collision policy — nothing new is placed, but the destination holds the
    /// file. That is a satisfied import (→ `Imported`), not a failure that would
    /// re-grab the same release next sweep.
    #[tokio::test]
    async fn import_treats_already_present_destination_as_imported() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("Movie.mkv");
        std::fs::write(&src, b"x").unwrap();
        let dest = dst_dir.path().join("Movie.mkv");
        std::fs::write(&dest, b"already here").unwrap();

        let mut state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        state.handle = Some(DownloadHandle {
            native_id: "h1".into(),
            category: "2000".into(),
        });
        state.completed_paths = Some(vec![src]);

        let importer: Arc<dyn skadi_importer::Importer> =
            Arc::new(DefaultImporter::new(ToTmp(dst_dir.path().to_path_buf())));
        super::import(&mut state, importer.as_ref()).await.unwrap();

        let outcome = state.outcome.as_ref().unwrap();
        assert_eq!(outcome.imported.len(), 1);
        assert_eq!(outcome.imported[0].acquirable, AR2("ed-1".into()));
        assert_eq!(outcome.imported[0].file.path, dest);
        assert!(outcome.already_present.is_empty(), "moved into imported");
        assert!(outcome.rejected.is_empty());
        // The existing library file was left alone (Skip policy).
        assert_eq!(std::fs::read(&dest).unwrap(), b"already here");
    }

    /// A notifier that records every event delivered to it. Wants every kind.
    struct RecordingNotifier {
        id: skadi_core::NotifierId,
        seen: Mutex<Vec<NE2>>,
    }
    #[async_trait]
    impl N2 for RecordingNotifier {
        fn id(&self) -> skadi_core::NotifierId {
            self.id
        }
        fn channels(&self) -> &[NotificationKind] {
            &[
                NotificationKind::Grabbed,
                NotificationKind::Imported,
                NotificationKind::Upgraded,
                NotificationKind::Failed,
                NotificationKind::Health,
            ]
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn notify(&self, event: &NE2) -> Result<()> {
            self.seen.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    /// A notifier that only wants the `Failed` channel — should NOT receive
    /// `Imported` events.
    struct FailedOnlyNotifier {
        id: skadi_core::NotifierId,
        seen: Mutex<Vec<NE2>>,
    }
    #[async_trait]
    impl N2 for FailedOnlyNotifier {
        fn id(&self) -> skadi_core::NotifierId {
            self.id
        }
        fn channels(&self) -> &[NotificationKind] {
            &[NotificationKind::Failed]
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn notify(&self, event: &NE2) -> Result<()> {
            self.seen.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    /// A notifier that always errors — proves notify failures are non-fatal.
    struct FailingNotifier {
        id: skadi_core::NotifierId,
    }
    #[async_trait]
    impl N2 for FailingNotifier {
        fn id(&self) -> skadi_core::NotifierId {
            self.id
        }
        fn channels(&self) -> &[NotificationKind] {
            &[NotificationKind::Imported]
        }
        async fn test(&self) -> Result<()> {
            // Mirrors its notify behavior: this mock models a down receiver.
            Err(AppError::Network("webhook down".into()))
        }
        async fn notify(&self, _event: &NE2) -> Result<()> {
            Err(AppError::Network("webhook down".into()))
        }
    }

    #[tokio::test]
    async fn notify_fans_imported_to_wanting_notifiers_only_and_is_non_fatal() {
        let state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        let want = Arc::new(RecordingNotifier {
            id: skadi_core::NotifierId::new(),
            seen: Mutex::new(vec![]),
        });
        let dont = Arc::new(FailedOnlyNotifier {
            id: skadi_core::NotifierId::new(),
            seen: Mutex::new(vec![]),
        });
        let broken = Arc::new(FailingNotifier {
            id: skadi_core::NotifierId::new(),
        });
        let notifiers: Vec<Arc<dyn N2>> = vec![want.clone(), dont.clone(), broken];
        // Should succeed despite `broken` returning an error.
        super::notify(&state, &notifiers).await.unwrap();

        assert_eq!(want.seen.lock().unwrap().len(), 1, "wanting notifier got 1");
        assert_eq!(
            dont.seen.lock().unwrap().len(),
            0,
            "Failed-only notifier should skip Imported"
        );
        match &want.seen.lock().unwrap()[0] {
            NE2::Imported(EP2 { title, year, .. }) => {
                assert_eq!(title, "Movie");
                assert_eq!(*year, Some(2020));
            }
            other => panic!("expected Imported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn notify_grabbed_fans_a_grabbed_event_to_wanting_notifiers() {
        let state = state_with_chosen(ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()));
        let want = Arc::new(RecordingNotifier {
            id: skadi_core::NotifierId::new(),
            seen: Mutex::new(vec![]),
        });
        let dont = Arc::new(FailedOnlyNotifier {
            id: skadi_core::NotifierId::new(),
            seen: Mutex::new(vec![]),
        });
        let notifiers: Vec<Arc<dyn N2>> = vec![want.clone(), dont.clone()];
        super::notify_grabbed(&state, &notifiers).await.unwrap();

        assert_eq!(want.seen.lock().unwrap().len(), 1, "wanting notifier got 1");
        assert_eq!(
            dont.seen.lock().unwrap().len(),
            0,
            "Failed-only notifier should skip Grabbed"
        );
        match &want.seen.lock().unwrap()[0] {
            NE2::Grabbed(EP2 { title, year, .. }) => {
                assert_eq!(title, "Movie");
                assert_eq!(*year, Some(2020));
            }
            other => panic!("expected Grabbed, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use skadi_core::{ExternalIds, IndexerId, MediaKind, ProfileId, Protocol};
    use skadi_importer::AcquirableRef;
    use skadi_indexers::{IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch};
    use skadi_quality::{QualityProfile, parse};
    use std::collections::BTreeSet;

    use crate::state::SearchSpec;

    #[test]
    fn audiobook_bucket_prefers_packs_and_keeps_per_book_precision() {
        // The per-book gate sees only the book title (the series alias is excluded by `decide`).
        let book_titles: Vec<String> = vec![
            "The Dungeon Anarchist's Cookbook".into(),
            "The Dungeon Anarchist's Cookbook Matt Dinniman".into(),
        ];
        let series = Some("Dungeon Crawler Carl");

        // A pack covering the series → accepted and bucketed above any single.
        let pack = audiobook_bucket(
            "Dungeon Crawler Carl - Books 1-8 [M4B]",
            &book_titles,
            series,
        );
        assert_eq!(pack, Some(PACK_BUCKET));
        // The right single book → accepted at a single bucket (strictly below the pack).
        let single = audiobook_bucket(
            "Matt Dinniman - The Dungeon Anarchist's Cookbook [M4B]",
            &book_titles,
            series,
        );
        assert!(matches!(single, Some(b) if b < PACK_BUCKET));
        assert!(
            pack.unwrap() > single.unwrap(),
            "a pack must outrank a single"
        );

        // A DIFFERENT book in the same series must NOT pass the single gate — the series alias is
        // excluded from `book_titles`, so a sibling has no book title to match.
        assert_eq!(
            audiobook_bucket("Matt Dinniman - Dungeon Crawler Carl", &book_titles, series),
            None
        );
        // A pack with no series context can't be placed.
        assert_eq!(
            audiobook_bucket("Some Series - Books 1-8", &book_titles, None),
            None
        );
        // A pack for a *different* series is rejected.
        assert_eq!(
            audiobook_bucket("The Wheel of Time - Books 1-14", &book_titles, series),
            None
        );
    }

    /// SKADI-T-0670: the releases grabbed in production, each for a book whose
    /// title it shared one word with. With the author alias every one is
    /// rejected; the name check rejects the four that announce another medium
    /// even without it; the real releases still pass.
    #[test]
    fn audiobook_bucket_rejects_the_production_junk() {
        let w = |t: &str, a: &str| vec![t.to_string(), format!("{t} {a}")];
        let cases = [
            (
                "Daemon",
                "Daniel Suarez",
                "DAEMON Tools Ultra 1072 x64 Pre Cracked 2025 New",
                "Daemon - Daniel Suarez",
            ),
            (
                "Chimera",
                "Mira Grant",
                "Milfty 25 12 19 Kyaa Chimera XXX 1080p MP4 WRB [XC]",
                "Mira Grant - Chimera (Parasitology 3) [M4B]",
            ),
            (
                "Scourged",
                "Kevin Hearne",
                "[TR24][OF][LDR] Pathogenic Virulence - Scourged - 2024 (Slam/Brutal Death Metal)",
                "Kevin Hearne - Staked & Scourged (Iron Druid 8-9)",
            ),
            (
                "Malice",
                "John Gwynne",
                "Malice 1993 1080p",
                "John Gwynne - Malice (The Faithful and the Fallen 1)",
            ),
            (
                "Unsouled",
                "Will Wight",
                "Unsouled NSZ",
                "Will Wight - Unsouled, Cradle Book 1",
            ),
        ];
        for (title, author, junk, real) in cases {
            assert_eq!(
                audiobook_bucket(junk, &w(title, author), None),
                None,
                "{junk}"
            );
            assert!(
                audiobook_bucket(real, &w(title, author), None).is_some(),
                "{real}"
            );
        }
        // Without the author alias the author gate is waived — the name check
        // is what stops these.
        for junk in [
            "DAEMON Tools Ultra 1072 x64 Pre Cracked 2025 New",
            "Milfty 25 12 19 Kyaa Chimera XXX 1080p MP4 WRB [XC]",
            "Malice 1993 1080p",
            "Unsouled NSZ",
        ] {
            assert!(not_an_audiobook(junk), "{junk}");
        }
        for book in [
            "Daemon - Daniel Suarez",
            "Crackdown - unabridged",
            "Episodes - a novel [M4B]",
            "The Expanse S01 (Leviathan Wakes, Caliban's War) MP3",
        ] {
            assert!(!not_an_audiobook(book), "{book}");
        }
        assert!(not_an_audiobook("True Detective S03E01 1080p ColdFilm"));
        assert!(not_an_audiobook("Tokyo Vice S02 COMPLETE WEB-DL"));
    }

    #[test]
    fn audiobook_bucket_rejects_coincidental_title_junk() {
        // Identity gate (SKADI-T-0359): a short/common title makes coverage trivially
        // 1.0, so the OLD coverage-only gate grabbed unrelated media that shared the
        // title word. The author gate rejects it.
        let fallen: Vec<String> = vec!["Fallen".into(), "Fallen Karen Chance".into()];
        // Music album — shares only the title word, wrong author → rejected.
        assert_eq!(
            audiobook_bucket("Evanescence - Fallen [2003]", &fallen, None),
            None
        );
        // The real audiobook (names the author) → accepted.
        assert!(
            audiobook_bucket("Fallen - Karen Chance - Unabridged M4B", &fallen, None).is_some()
        );

        let wasteland: Vec<String> = vec!["Wasteland".into(), "Wasteland W Scott Poole".into()];
        // A PC game grabbing the title word → rejected.
        assert_eq!(
            audiobook_bucket("Wasteland 3-HOODLUM", &wasteland, None),
            None
        );
        assert!(
            audiobook_bucket("Wasteland - W. Scott Poole [MP3]", &wasteland, None).is_some(),
            "the real book by the wanted author is still accepted"
        );
    }

    #[test]
    fn category_group_gate_maps_top_level_torznab_groups() {
        use skadi_core::MediaKind::*;
        use skadi_indexers::Category;
        // Audiobook wants audio (3xxx) OR books (7xxx/8xxx); a game (4xxx), movie
        // (2xxx) or TV (5xxx) is off-category → rejected (SKADI-T-0360).
        assert!(category_group_ok(Audiobook, Category(3030))); // Audio/Audiobook
        assert!(category_group_ok(Audiobook, Category(7020))); // Books/EBook tracker
        assert!(!category_group_ok(Audiobook, Category(4050))); // PC/Games
        assert!(!category_group_ok(Audiobook, Category(2000))); // Movies
        assert!(!category_group_ok(Audiobook, Category(5040))); // TV
        // Movies only accept the Movies group.
        assert!(category_group_ok(Movie, Category(2040)));
        assert!(!category_group_ok(Movie, Category(3010)));
    }

    #[test]
    fn size_sanity_gate_rejects_only_gross_outliers() {
        use skadi_core::MediaKind::Audiobook;
        const MB: u64 = 1024 * 1024;
        const GB: u64 = 1024 * MB;
        assert!(size_ok(Audiobook, 0)); // unknown → pass
        assert!(size_ok(Audiobook, 300 * MB)); // a normal audiobook
        assert!(size_ok(Audiobook, 9 * GB)); // an 8-book pack — still fine
        assert!(!size_ok(Audiobook, 20 * 1024)); // 20 KB "audiobook" fake
        assert!(!size_ok(Audiobook, 40 * GB)); // 40 GB → a game/movie, not a book
    }

    /// The audiobook acquisition-quality corpus (SKADI-T-0361): a graded table of
    /// REAL cases — the junk the operator actually saw grabbed, plus legit books —
    /// run through the combined decide gates (off-category `category_group_ok` +
    /// the identity `audiobook_bucket`). This is the *arr discipline: matching
    /// quality is measurable and every future gate change is regression-checked.
    /// Grow it whenever a new false-accept or false-reject is found in the wild.
    #[test]
    fn audiobook_decide_corpus() {
        use skadi_core::MediaKind::Audiobook;
        use skadi_indexers::Category;
        // (wanted book title, wanted author, release title, torznab cat, EXPECT accept)
        let cases: &[(&str, &str, &str, u32, bool)] = &[
            // --- junk the operator saw grabbed → must REJECT ---
            (
                "Fallen",
                "Karen Chance",
                "Evanescence - Fallen [2003] [Deluxe Edition]",
                3010,
                false,
            ),
            (
                "Wasteland",
                "W Scott Poole",
                "Wasteland 3-HOODLUM",
                4050,
                false,
            ),
            (
                "Fallen",
                "Karen Chance",
                "WUCHANG: Fallen Feathers [ITA + ENG]",
                4050,
                false,
            ),
            (
                "Twelve Months",
                "Jim Butcher",
                "Desus and Mero S02E54 Twelve Months of Papi",
                5040,
                false,
            ),
            (
                "Into the Darkness",
                "Barry Eisler",
                "Straight Into Darkness (2004) [1080p] [BluRay]",
                2000,
                false,
            ),
            ("Bus Bound", "Some Author", "Bus Bound-RUNE", 4050, false),
            // Wrong author, right-ish category → identity gate rejects.
            (
                "Fallen",
                "Karen Chance",
                "Fallen - Lauren Kate - Unabridged M4B",
                3030,
                false,
            ),
            // --- legit audiobooks → must ACCEPT ---
            (
                "Project Hail Mary",
                "Andy Weir",
                "Project Hail Mary - Andy Weir [M4B]",
                3030,
                true,
            ),
            (
                "The Gate of the Feral Gods",
                "Matt Dinniman",
                "The Gate of the Feral Gods - Matt Dinniman - Unabridged (M4B)",
                3030,
                true,
            ),
            (
                "Fallen",
                "Karen Chance",
                "Fallen - Karen Chance - Unabridged M4B",
                3030,
                true,
            ),
            // A book tracker tags audiobooks in the Books (7xxx) group — still accepted.
            (
                "Wasteland",
                "W Scott Poole",
                "Wasteland The Great War and the Origins of Modern Horror - W. Scott Poole",
                7020,
                true,
            ),
        ];

        for &(title, author, release, cat, expect) in cases {
            let book_titles = vec![title.to_string(), format!("{title} {author}")];
            let category_ok = category_group_ok(Audiobook, Category(cat));
            let identity_ok = audiobook_bucket(release, &book_titles, None).is_some();
            let accepted = category_ok && identity_ok;
            assert_eq!(
                accepted, expect,
                "case '{release}' (cat {cat}) wanted={title}/{author}: \
                 category_ok={category_ok} identity_ok={identity_ok}, expected accept={expect}"
            );
        }
    }

    fn state_for(titles: &[&str]) -> AcquireState {
        // Use "Movie" as the wanted title so it matches the release naming convention
        // (Movie.2020.1080p...) used throughout these tests.
        AcquireState::new(
            AcquirableRef("ed-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Movie".into()],
                year: Some(2020),
                external_ids: ExternalIds::default(),
                categories: vec![],
                tv: None,
                series: None,
                tags: None,
            },
            ProfileId::new(),
        )
        .tap_candidates(titles)
    }

    impl AcquireState {
        fn tap_candidates(mut self, titles: &[&str]) -> Self {
            self.candidates = titles.iter().map(|t| release(t, 10)).collect();
            self
        }
    }

    fn release(title: &str, seeders: u32) -> Release {
        Release {
            indexer: IndexerId::new(),
            title: title.to_string(),
            fetch: ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{title}")),
            size: 8_000_000_000,
            published: Utc::now(),
            seeders: Some(seeders),
            categories: Vec::new(),
            parsed: parse(title),
        }
    }

    /// A mock indexer returning a fixed set of releases (or an error).
    struct MockIndexer {
        id: IndexerId,
        kind: MediaKind,
        result: std::result::Result<Vec<Release>, ()>,
    }

    #[async_trait]
    impl Indexer for MockIndexer {
        fn id(&self) -> IndexerId {
            self.id
        }
        fn protocol(&self) -> Protocol {
            Protocol::Torrent
        }
        fn supports(&self, kind: MediaKind) -> bool {
            kind == self.kind
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn capabilities(&self) -> Result<IndexerCaps> {
            Ok(IndexerCaps {
                supports_rss: true,
                supports_search: true,
                id_params: BTreeSet::new(),
                supports_aggregate_ids: false,
                text_search: TextSearch::Raw,
                categories: vec![],
            })
        }
        async fn search(&self, _query: &dyn SearchQuery) -> Result<Vec<Release>> {
            self.result
                .clone()
                .map_err(|()| AppError::Network("mock indexer down".into()))
        }
    }

    fn ix(kind: MediaKind, releases: Vec<Release>) -> Arc<dyn Indexer> {
        Arc::new(MockIndexer {
            id: IndexerId::new(),
            kind,
            result: Ok(releases),
        })
    }

    /// A mock whose tag scope can be set (SKADI-T-0556).
    struct TaggedIndexer {
        inner: MockIndexer,
        tags: Vec<String>,
    }

    #[async_trait]
    impl Indexer for TaggedIndexer {
        fn id(&self) -> IndexerId {
            self.inner.id()
        }
        fn protocol(&self) -> Protocol {
            self.inner.protocol()
        }
        fn supports(&self, kind: MediaKind) -> bool {
            self.inner.supports(kind)
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn capabilities(&self) -> Result<IndexerCaps> {
            self.inner.capabilities().await
        }
        async fn search(&self, q: &dyn SearchQuery) -> Result<Vec<Release>> {
            self.inner.search(q).await
        }
        fn applies_to_tags(&self, item_tags: &[String]) -> bool {
            self.tags.is_empty() || self.tags.iter().any(|t| item_tags.contains(t))
        }
    }

    fn tagged_ix(kind: MediaKind, tags: &[&str], releases: Vec<Release>) -> Arc<dyn Indexer> {
        Arc::new(TaggedIndexer {
            inner: MockIndexer {
                id: IndexerId::new(),
                kind,
                result: Ok(releases),
            },
            tags: tags.iter().map(|s| (*s).to_string()).collect(),
        })
    }

    fn failing_ix(kind: MediaKind) -> Arc<dyn Indexer> {
        Arc::new(MockIndexer {
            id: IndexerId::new(),
            kind,
            result: Err(()),
        })
    }

    /// An indexer whose `search` sleeps while tracking the peak number of
    /// simultaneously-running searches — proves the fan-out is concurrent.
    struct ConcurrentProbe {
        id: IndexerId,
        inflight: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
        release: Release,
    }
    #[async_trait]
    impl Indexer for ConcurrentProbe {
        fn id(&self) -> IndexerId {
            self.id
        }
        fn protocol(&self) -> Protocol {
            Protocol::Torrent
        }
        fn supports(&self, kind: MediaKind) -> bool {
            kind == MediaKind::Movie
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn capabilities(&self) -> Result<IndexerCaps> {
            Ok(IndexerCaps {
                supports_rss: true,
                supports_search: true,
                id_params: BTreeSet::new(),
                supports_aggregate_ids: false,
                text_search: TextSearch::Raw,
                categories: vec![],
            })
        }
        async fn search(&self, _query: &dyn SearchQuery) -> Result<Vec<Release>> {
            use std::sync::atomic::Ordering::SeqCst;
            let now = self.inflight.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(now, SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.inflight.fetch_sub(1, SeqCst);
            Ok(vec![self.release.clone()])
        }
    }

    #[tokio::test]
    async fn an_untagged_indexer_is_queried_for_a_tagged_item() {
        // The regression that would break every existing install (SKADI-T-0556):
        // every indexer out there today is untagged, so if an empty tag set meant
        // "applies to nothing" the fan-out would silently empty on upgrade.
        let mut state = state_for(&[]);
        state.request.tags = Some(vec!["anime".into()]);
        let indexers = vec![tagged_ix(
            MediaKind::Movie,
            &[],
            vec![release("Movie.1999.1080p.BluRay.x264-GRP", 10)],
        )];
        search(&mut state, &indexers).await.unwrap();
        assert_eq!(
            state.candidates.len(),
            1,
            "an untagged indexer serves everything"
        );
    }

    #[tokio::test]
    async fn a_tagged_indexer_is_skipped_for_an_item_that_does_not_share_a_tag() {
        let mut state = state_for(&[]);
        state.request.tags = Some(vec!["uhd".into()]);
        let indexers = vec![tagged_ix(
            MediaKind::Movie,
            &["anime"],
            vec![release("Movie.1999.1080p.BluRay.x264-GRP", 10)],
        )];
        // Every indexer scoped away means no search happened — and the error
        // must say *that*, not blame the media kind, or the operator goes
        // looking at categories while the item silently never searches again.
        let err = search(&mut state, &indexers).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("scoped to tags"),
            "an anime-tagged indexer must not be queried for a uhd-tagged item, \
             and the error must name the cause: {msg}"
        );
        assert!(state.candidates.is_empty());
    }

    #[tokio::test]
    async fn a_tagged_indexer_is_queried_when_a_tag_is_shared() {
        let mut state = state_for(&[]);
        state.request.tags = Some(vec!["anime".into(), "other".into()]);
        let indexers = vec![tagged_ix(
            MediaKind::Movie,
            &["anime"],
            vec![release("Movie.1999.1080p.BluRay.x264-GRP", 10)],
        )];
        search(&mut state, &indexers).await.unwrap();
        assert_eq!(state.candidates.len(), 1);
    }

    #[tokio::test]
    async fn a_free_text_search_consults_tagged_indexers_too() {
        // `None` is "no item", not "an item with no tags". Collapsing the two
        // would make a manual search silently skip every tagged indexer — which
        // is exactly the tracker an operator reaches for when hand-searching.
        let mut state = state_for(&[]);
        state.request.tags = None;
        let indexers = vec![tagged_ix(
            MediaKind::Movie,
            &["anime"],
            vec![release("Movie.1999.1080p.BluRay.x264-GRP", 10)],
        )];
        search(&mut state, &indexers).await.unwrap();
        assert_eq!(
            state.candidates.len(),
            1,
            "a free-text search has no item to scope against"
        );
    }

    #[tokio::test]
    async fn an_untagged_item_still_excludes_tagged_indexers() {
        // The other direction: Some(vec![]) is a real item that has no tags, so a
        // tagged indexer genuinely does not apply to it.
        let mut state = state_for(&[]);
        state.request.tags = Some(vec![]);
        let indexers = vec![tagged_ix(
            MediaKind::Movie,
            &["anime"],
            vec![release("Movie.1999.1080p.BluRay.x264-GRP", 10)],
        )];
        let err = search(&mut state, &indexers).await.unwrap_err();
        assert!(format!("{err}").contains("scoped to tags"));
        assert!(state.candidates.is_empty());
    }

    #[tokio::test]
    async fn search_fans_out_concurrently() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let mut state = state_for(&[]);
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let indexers: Vec<Arc<dyn Indexer>> = (0..4)
            .map(|i| {
                Arc::new(ConcurrentProbe {
                    id: IndexerId::new(),
                    inflight: inflight.clone(),
                    peak: peak.clone(),
                    release: release(&format!("Movie.{i}.1999.1080p.BluRay.x264-G{i}"), 10),
                }) as Arc<dyn Indexer>
            })
            .collect();

        search(&mut state, &indexers).await.unwrap();
        assert_eq!(state.candidates.len(), 4, "all four results collected");
        assert!(
            peak.load(SeqCst) >= 2,
            "indexers must be queried concurrently, not one-by-one; peak = {}",
            peak.load(SeqCst)
        );
    }

    #[tokio::test]
    async fn search_collects_and_dedups_across_indexers() {
        let mut state = state_for(&[]);
        let a = release("The.Matrix.1999.1080p.BluRay.x264-AAA", 10);
        let dup = a.clone();
        let b = release("The.Matrix.1999.720p.WEB.x264-BBB", 5);
        let indexers = vec![
            ix(MediaKind::Movie, vec![a, b]),
            ix(MediaKind::Movie, vec![dup]), // duplicate of `a`
            ix(MediaKind::Music, vec![release("unrelated", 1)]), // wrong kind, skipped
        ];
        search(&mut state, &indexers).await.unwrap();
        assert_eq!(
            state.candidates.len(),
            2,
            "duplicate and wrong-kind dropped"
        );
    }

    #[tokio::test]
    async fn search_tolerates_one_failure_but_errors_if_all_fail() {
        // One good + one failing → still succeeds with the good results.
        let mut state = state_for(&[]);
        let indexers = vec![
            failing_ix(MediaKind::Movie),
            ix(MediaKind::Movie, vec![release("ok", 1)]),
        ];
        search(&mut state, &indexers).await.unwrap();
        assert_eq!(state.candidates.len(), 1);

        // All failing, nothing returned → error (retryable).
        let mut state = state_for(&[]);
        let indexers = vec![failing_ix(MediaKind::Movie), failing_ix(MediaKind::Movie)];
        assert!(matches!(
            search(&mut state, &indexers).await,
            Err(AppError::Network(_))
        ));
    }

    /// An indexer that answers after `delay`, counting how many of its searches
    /// run at once (SKADI-T-0672).
    struct SlowIndexer {
        id: IndexerId,
        delay: Duration,
        result: Vec<Release>,
        inflight: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Indexer for SlowIndexer {
        fn id(&self) -> IndexerId {
            self.id
        }
        fn protocol(&self) -> Protocol {
            Protocol::Torrent
        }
        fn supports(&self, kind: MediaKind) -> bool {
            kind == MediaKind::Movie
        }
        async fn test(&self) -> Result<()> {
            Ok(())
        }
        async fn capabilities(&self) -> Result<IndexerCaps> {
            Ok(IndexerCaps {
                supports_rss: true,
                supports_search: true,
                id_params: BTreeSet::new(),
                supports_aggregate_ids: false,
                text_search: TextSearch::Raw,
                categories: vec![],
            })
        }
        async fn search(&self, _query: &dyn SearchQuery) -> Result<Vec<Release>> {
            use std::sync::atomic::Ordering::SeqCst;
            let now = self.inflight.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(now, SeqCst);
            tokio::time::sleep(self.delay).await;
            self.inflight.fetch_sub(1, SeqCst);
            Ok(self.result.clone())
        }
    }

    fn slow_ix(delay_secs: u64, result: Vec<Release>) -> Arc<SlowIndexer> {
        Arc::new(SlowIndexer {
            id: IndexerId::new(),
            delay: Duration::from_secs(delay_secs),
            result,
            inflight: Arc::default(),
            peak: Arc::default(),
        })
    }

    /// SKADI-T-0672: one indexer timing out while another answers with nothing
    /// is "nothing found", not a network error; the timeout is in the health
    /// telemetry; and a timeout with nothing else answering still errors.
    #[tokio::test(start_paused = true)]
    async fn a_timeout_beside_an_empty_answer_is_nothing_found_and_is_recorded() {
        let slow = slow_ix(600, vec![]);
        let slow_id = slow.id;
        let mut state = state_for(&[]);
        let indexers: Vec<Arc<dyn Indexer>> = vec![slow.clone(), ix(MediaKind::Movie, vec![])];
        search(&mut state, &indexers).await.unwrap();
        assert!(state.candidates.is_empty());
        let health = indexer_health()
            .get(slow_id)
            .expect("the timeout is recorded");
        assert!(health.failure_count >= 1, "{health:?}");

        let mut state = state_for(&[]);
        let only_slow: Vec<Arc<dyn Indexer>> = vec![slow_ix(600, vec![])];
        assert!(matches!(
            search(&mut state, &only_slow).await,
            Err(AppError::Network(_))
        ));
    }

    /// SKADI-T-0672: a burst of searches against one indexer runs at most
    /// `PER_INDEXER_INFLIGHT` at a time, and the queueing does not count against
    /// the budget — every search completes although together they take far
    /// longer than 90 s.
    #[tokio::test(start_paused = true)]
    async fn a_burst_queues_outside_the_budget() {
        use std::sync::atomic::Ordering::SeqCst;
        let probe = slow_ix(40, vec![release("Movie.1999.1080p.BluRay.x264-GRP", 10)]);
        let indexers: Vec<Arc<dyn Indexer>> = vec![probe.clone()];
        let runs: Vec<_> = (0..8)
            .map(|_| {
                let indexers = indexers.clone();
                tokio::spawn(async move {
                    let mut state = state_for(&[]);
                    search(&mut state, &indexers)
                        .await
                        .map(|()| state.candidates.len())
                })
            })
            .collect();
        for r in runs {
            assert_eq!(
                r.await.unwrap().unwrap(),
                1,
                "no search timed out in the queue"
            );
        }
        assert_eq!(probe.peak.load(SeqCst), PER_INDEXER_INFLIGHT);
    }

    // --- decide ---

    use skadi_quality::default_definitions;

    /// SKADI-T-0184: `explain` returns the full breakdown — classified quality +
    /// rank, the decision, the aggregate format score, and every matched format —
    /// for both an accepted (format-boosted) and a rejected (must-not) release.
    #[test]
    fn explain_returns_the_full_decision_breakdown() {
        use skadi_quality::{CustomFormat, CustomFormatScore, FormatRule};
        let (mut profile, defs) = profile_with_two_ranks();
        let hevc = skadi_core::CustomFormatId::new();
        let remux = skadi_core::CustomFormatId::new();
        let registry = vec![
            CustomFormat {
                id: hevc,
                name: "x265".into(),
                rules: vec![FormatRule::Codec("x265".into())],
            },
            CustomFormat {
                id: remux,
                name: "no-remux".into(),
                rules: vec![FormatRule::TitleRegex(r"\bremux\b".into())],
            },
        ];
        profile.formats = vec![
            CustomFormatScore {
                format: hevc,
                score: 200,
                mode: skadi_quality::FormatMode::Preferred,
            },
            CustomFormatScore {
                format: remux,
                score: -1000,
                mode: skadi_quality::FormatMode::Preferred,
            },
        ];
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &registry,
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        // Accepted, format-boosted: x265 1080p.
        let e = explain(&release("Movie.2020.1080p.BluRay.x265-A", 10), &scoring);
        assert!(e.accepted, "{}", e.reason);
        assert_eq!(e.quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(e.decision.as_deref(), Some("Accept"));
        assert_eq!(e.format_score, 200);
        assert_eq!(e.matched_formats.len(), 1);
        assert_eq!(e.matched_formats[0].name, "x265");
        assert_eq!(e.matched_formats[0].score, 200);
        assert!(e.quality_rank.is_some());

        // Rejected by the must-not floor: the breakdown still explains why.
        let r = explain(&release("Movie.2020.1080p.BluRay.Remux-C", 10), &scoring);
        assert!(!r.accepted);
        assert!(r.reason.contains("floor"), "{}", r.reason);
        assert_eq!(r.format_score, -1000);
        assert_eq!(r.matched_formats[0].name, "no-remux");
        assert_eq!(r.decision.as_deref(), Some("Accept")); // quality allowed; gated by floor

        // Pre-quality rejection has no quality/decision.
        let nq = explain(&release("Some.Random.Junk", 10), &scoring);
        assert!(!nq.accepted && nq.quality.is_none() && nq.decision.is_none());
    }

    /// SKADI-T-0584: when the held file does not direct-play, the strictly-better
    /// bar is relaxed.
    ///
    /// Without this the feature is inert. A 54 GB DTS-only remux sits at the
    /// cutoff quality with a high format score, so every candidate is rejected as
    /// "not strictly better" — the sweep would dutifully re-search it forever and
    /// never grab anything.
    #[test]
    fn an_unplayable_held_file_accepts_a_lower_scoring_candidate() {
        use skadi_quality::{CustomFormat, CustomFormatScore, FormatMode, FormatRule};
        let (mut profile, defs) = profile_with_two_ranks();
        let remux = skadi_core::CustomFormatId::new();
        let registry = vec![CustomFormat {
            id: remux,
            name: "remux".into(),
            rules: vec![FormatRule::TitleRegex(r"\bremux\b".into())],
        }];
        profile.formats = vec![CustomFormatScore {
            format: remux,
            score: 500,
            mode: FormatMode::Preferred,
        }];
        let hi = profile.cutoff;
        let no_block = HashSet::new();
        let scoring = |unplayable: bool| Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &registry,
            min_seeders: 0,
            blocklisted: &no_block,
            audiobook: None,
            // Held: the cutoff quality AND a high format score — the remux.
            current_quality: Some(hi),
            current_format_score: Some(500),
            current_unplayable: unplayable,
        };

        // A plain 1080p release: same quality, format score 0. Strictly worse on
        // both existing axes.
        let candidate = release("Movie.2020.1080p.BluRay-A", 10);

        // Playable held file → correctly refused. This is the existing behaviour
        // and it must not change.
        let refused = explain(&candidate, &scoring(false));
        assert!(
            !refused.accepted,
            "a worse release must not replace a good file"
        );

        // Unplayable held file → accepted, and labelled an Upgrade.
        let accepted = explain(&candidate, &scoring(true));
        assert!(
            accepted.accepted,
            "a silent file must be replaceable by one that plays: {}",
            accepted.reason
        );
        assert_eq!(accepted.decision.as_deref(), Some("Upgrade"));
    }

    /// The relaxation is not a free pass: everything else still gates.
    #[test]
    fn an_unplayable_held_file_still_respects_the_profile() {
        let (profile, defs) = profile_with_two_ranks();
        let no_block = HashSet::new();
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &no_block,
            audiobook: None,
            current_quality: Some(profile.cutoff),
            current_format_score: Some(500),
            current_unplayable: true,
        };
        // A quality that is not in the profile's ladder at all.
        let out = explain(&release("Movie.2020.CAM-A", 10), &scoring);
        assert!(
            !out.accepted,
            "an out-of-profile release is still refused: {}",
            out.reason
        );
    }

    /// SKADI-T-0185: Required/Ignored hard gates reject independent of the score
    /// floor — a missing Required format and a present Ignored format both reject,
    /// with the gate named in the reason; otherwise the release is accepted.
    /// SKADI-T-0186: the format-score upgrade axis — on an upgrade run, a
    /// same-quality candidate with a strictly-higher format score (a proper/repack)
    /// is an Upgrade; an equal-or-lower format at the same quality is not.
    #[test]
    fn explain_format_score_upgrade_grabs_a_proper_of_the_same_quality() {
        use skadi_quality::{CustomFormat, CustomFormatScore, FormatMode, FormatRule};
        let (mut profile, defs) = profile_with_two_ranks(); // 720p/1080p, cutoff 1080p, upgrade on
        let proper = skadi_core::CustomFormatId::new();
        let registry = vec![CustomFormat {
            id: proper,
            name: "proper".into(),
            rules: vec![FormatRule::TitleRegex(r"\bproper\b".into())],
        }];
        profile.formats = vec![CustomFormatScore {
            format: proper,
            score: 200,
            mode: FormatMode::Preferred,
        }];
        let hi = profile.cutoff; // Bluray-1080p
        let no_block = HashSet::new();
        // Held: Bluray-1080p with format score `cur_fs`.
        let scoring = |cur_fs: i32| Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &registry,
            min_seeders: 0,
            blocklisted: &no_block,
            audiobook: None,
            current_quality: Some(hi),
            current_format_score: Some(cur_fs),
            current_unplayable: false,
        };

        // Held a non-proper (fs 0): a PROPER of the same quality → format upgrade.
        let up = explain(
            &release("Movie.2020.1080p.BluRay.PROPER-A", 10),
            &scoring(0),
        );
        assert!(up.accepted, "{}", up.reason);
        assert_eq!(up.decision.as_deref(), Some("Upgrade"));
        assert_eq!(up.format_score, 200);

        // A non-proper of the same quality → not strictly higher → not grabbed.
        let same = explain(&release("Movie.2020.1080p.BluRay-B", 10), &scoring(0));
        assert!(
            !same.accepted,
            "equal format at same quality is not an upgrade"
        );

        // Already holding a proper (fs 200): another proper (200) is not strictly
        // higher → not grabbed (terminates without a cutoff).
        let plateau = explain(
            &release("Movie.2020.1080p.BluRay.PROPER-C", 10),
            &scoring(200),
        );
        assert!(
            !plateau.accepted,
            "no strictly-higher format score available"
        );
    }

    #[test]
    fn explain_applies_required_and_ignored_gates() {
        use skadi_quality::{CustomFormat, CustomFormatScore, FormatMode, FormatRule};
        let (mut profile, defs) = profile_with_two_ranks();
        let hevc = skadi_core::CustomFormatId::new();
        let remux = skadi_core::CustomFormatId::new();
        let registry = vec![
            CustomFormat {
                id: hevc,
                name: "x265".into(),
                rules: vec![FormatRule::Codec("x265".into())],
            },
            CustomFormat {
                id: remux,
                name: "remux".into(),
                rules: vec![FormatRule::TitleRegex(r"\bremux\b".into())],
            },
        ];
        // Require x265, ignore remux.
        profile.formats = vec![
            CustomFormatScore {
                format: hevc,
                score: 0,
                mode: FormatMode::Required,
            },
            CustomFormatScore {
                format: remux,
                score: 0,
                mode: FormatMode::Ignored,
            },
        ];
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &registry,
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        // x265, non-remux → both gates satisfied → accepted.
        let ok = explain(&release("Movie.2020.1080p.BluRay.x265-A", 10), &scoring);
        assert!(ok.accepted, "{}", ok.reason);

        // Non-x265 → missing required → rejected (score floor is 0, so only the
        // gate can reject here).
        let miss = explain(&release("Movie.2020.1080p.BluRay.x264-B", 10), &scoring);
        assert!(!miss.accepted);
        assert!(miss.reason.contains("required"), "{}", miss.reason);

        // x265 but remux present → ignored gate → rejected.
        let ig = explain(
            &release("Movie.2020.1080p.BluRay.Remux.x265-C", 10),
            &scoring,
        );
        assert!(!ig.accepted);
        assert!(ig.reason.contains("ignored"), "{}", ig.reason);
    }

    /// SKADI-T-0184: `explain` works on the **audiobook axis** too — classifying
    /// against the audiobook ladder and applying the abridged-reject floor.
    #[test]
    fn explain_scores_the_audiobook_axis() {
        use crate::services::AudiobookScoring;
        use skadi_quality::audiobook::default_audiobook_definitions;

        let defs = default_audiobook_definitions();
        let m4b128 = defs.iter().find(|d| d.name == "M4B-128").unwrap().id;
        let profile = QualityProfile {
            id: ProfileId::new(),
            name: "ab".into(),
            allowed: vec![m4b128],
            cutoff: m4b128,
            upgrade_allowed: false,
            formats: vec![],
            min_format_score: 0,
        };
        let ab = AudiobookScoring {
            definitions: defs.clone(),
            allow_abridged: false,
        };
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &[],
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: Some(&ab),
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        // Accepted M4B-128 → quality + Accept decision in the breakdown.
        let e = explain(
            &release("Andy Weir - Project Hail Mary (2021) [M4B 128kbps]", 10),
            &scoring,
        );
        assert!(e.accepted, "{}", e.reason);
        assert_eq!(e.quality.as_deref(), Some("M4B-128"));
        assert_eq!(e.decision.as_deref(), Some("Accept"));

        // Abridged → pre-quality rejection (no quality, no decision).
        let r = explain(
            &release("Andy Weir - Project Hail Mary {Abridged} M4B 128kbps", 10),
            &scoring,
        );
        assert!(!r.accepted && r.reason.contains("abridged"));
        assert!(r.quality.is_none() && r.decision.is_none());
    }

    /// Integration of the quality decision with **retrieval** (SKADI-T-0183): a
    /// real `Indexer::search` populates the candidate set, then `decide` applies
    /// quality + custom-format scoring over it. Proves the two halves compose —
    /// a positively-scored format wins among retrieved releases, and a "must-not"
    /// (negative) format gates a retrieved release out via the floor.
    #[tokio::test]
    async fn search_then_decide_applies_custom_formats_over_retrieved_releases() {
        use skadi_quality::{CustomFormat, CustomFormatScore, FormatRule};
        let (mut profile, defs) = profile_with_two_ranks(); // allows Bluray-720p/1080p, floor 0
        let hevc = skadi_core::CustomFormatId::new();
        let remux = skadi_core::CustomFormatId::new();
        let registry = vec![
            CustomFormat {
                id: hevc,
                name: "x265".into(),
                rules: vec![FormatRule::Codec("x265".into())],
            },
            CustomFormat {
                id: remux,
                name: "remux".into(),
                rules: vec![FormatRule::TitleRegex(r"\bremux\b".into())],
            },
        ];
        profile.formats = vec![
            CustomFormatScore {
                format: hevc,
                score: 200,
                mode: skadi_quality::FormatMode::Preferred,
            },
            CustomFormatScore {
                format: remux,
                score: -1000, // must-not: pushes below the min_format_score floor (0)
                mode: skadi_quality::FormatMode::Preferred,
            },
        ];

        // Retrieval: one indexer returns three same-quality (Bluray-1080p)
        // releases differing only in custom-format-matchable attributes.
        let mut state = state_for(&[]);
        let indexers = vec![ix(
            MediaKind::Movie,
            vec![
                release("Movie.2020.1080p.BluRay.x265-A", 10),
                release("Movie.2020.1080p.BluRay.x264-B", 10),
                release("Movie.2020.1080p.BluRay.Remux-C", 10),
            ],
        )];
        search(&mut state, &indexers).await.unwrap();
        assert_eq!(state.candidates.len(), 3, "all three releases retrieved");

        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &registry,
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        // The must-not (remux) release is gated out by the format floor — even
        // though its quality is allowed.
        let (v_remux, _) = evaluate(&release("Movie.2020.1080p.BluRay.Remux-C", 10), &scoring);
        assert!(
            !v_remux.accepted && v_remux.reason.contains("floor"),
            "remux must-not should be rejected by the floor, got {:?}",
            v_remux.reason
        );

        // decide over the retrieved set picks the x265 (highest format score).
        decide(&mut state, &scoring).unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("x265"),
            "format-preferred release chosen from retrieval, got {}",
            state.chosen.as_ref().unwrap().title
        );
    }

    /// Build a profile over two ranks from the default definitions, picking
    /// quality ids by name (matches the pattern in `skadi-quality`'s own tests).
    /// The pre-SKADI-T-0598 order, for tests about other gates.
    const QUALITY_FIRST: ReachabilityPolicy = ReachabilityPolicy {
        prefer_seeders: false,
        floor: skadi_quality::Resolution::R720p,
    };

    fn profile_with_two_ranks() -> (QualityProfile, Vec<skadi_quality::QualityDefinition>) {
        let defs = default_definitions();
        let find = |name: &str| defs.iter().find(|q| q.name == name).expect(name).id;
        let low = find("Bluray-720p");
        let high = find("Bluray-1080p");
        let profile = QualityProfile {
            id: ProfileId::new(),
            name: "test".into(),
            allowed: vec![low, high],
            cutoff: high,
            upgrade_allowed: true,
            formats: vec![],
            min_format_score: 0,
        };
        (profile, defs)
    }

    /// SKADI-T-0539: `enable_automatic_search` is honoured for a sweep and
    /// **ignored** for an operator-driven search. Turning it off is precisely how
    /// an operator keeps an indexer for the interactive list and manual grabs, so
    /// filtering those too would defeat the setting rather than implement it.
    #[tokio::test]
    async fn automatic_search_flag_is_ignored_for_an_interactive_search() {
        struct NoAuto(IndexerId);
        #[async_trait::async_trait]
        impl Indexer for NoAuto {
            fn id(&self) -> IndexerId {
                self.0
            }
            fn protocol(&self) -> skadi_core::Protocol {
                skadi_core::Protocol::Torrent
            }
            fn supports(&self, _kind: MediaKind) -> bool {
                true
            }
            fn enable_automatic_search(&self) -> bool {
                false
            }
            async fn capabilities(&self) -> Result<skadi_indexers::IndexerCaps> {
                Ok(skadi_indexers::IndexerCaps {
                    supports_rss: false,
                    supports_search: true,
                    id_params: std::collections::BTreeSet::new(),
                    supports_aggregate_ids: false,
                    text_search: skadi_indexers::TextSearch::Raw,
                    categories: vec![Category(2000)],
                })
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _q: &dyn skadi_indexers::SearchQuery,
            ) -> Result<Vec<skadi_indexers::Release>> {
                Ok(vec![release("Movie.2020.1080p.BluRay.x264-GRP", 50)])
            }
        }
        let ix: Vec<Arc<dyn Indexer>> = vec![Arc::new(NoAuto(IndexerId::new()))];

        // A sweep skips it …
        let mut auto = state_for(&[]);
        auto.candidates.clear();
        auto.request.trigger = crate::state::SearchTrigger::Automatic;
        assert!(
            search(&mut auto, &ix).await.is_err(),
            "an automatic sweep must skip an indexer with automatic search off"
        );

        // … the operator's own search does not.
        let mut manual = state_for(&[]);
        manual.candidates.clear();
        manual.request.trigger = crate::state::SearchTrigger::Interactive;
        search(&mut manual, &ix).await.unwrap();
        assert_eq!(manual.candidates.len(), 1);
    }

    /// SKADI-T-0539: a per-indexer seeder floor **overrides** the profile's.
    /// `0` means "use the profile's"; anything else is the operator saying this
    /// particular tracker's seeder counts need a different bar — so a `max` would
    /// ignore them asking for a *lower* one.
    #[test]
    fn a_per_indexer_seeder_floor_overrides_the_profiles() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&["Movie.2020.1080p.BluRay.x264-HIGH"]);
        state.candidates[0].seeders = Some(10);
        let ix = state.candidates[0].indexer;

        let no_block = HashSet::new();
        // A plain fn rather than a closure: the returned `Scoring` borrows both
        // the flags and the locals, which a closure's inferred signature cannot
        // express.
        fn scoring<'a>(
            flags: &'a [(skadi_core::IndexerId, u32, u32)],
            defs: &'a [skadi_quality::QualityDefinition],
            profile: &'a QualityProfile,
            no_block: &'a HashSet<String>,
        ) -> Scoring<'a> {
            Scoring {
                indexer_flags: flags,
                definitions: defs,
                profile,
                formats: &[],
                min_seeders: 5,
                blocklisted: no_block,
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            }
        }
        // The profile's floor of 5 accepts a 10-seeder release.
        let mut s1 = state.clone();
        decide(&mut s1, &scoring(&[], &defs, &profile, &no_block)).unwrap();
        assert!(s1.chosen.is_some(), "profile floor 5 accepts 10 seeders");

        // The indexer's floor of 20 rejects it.
        let higher = [(ix, 25u32, 20u32)];
        let mut s2 = state.clone();
        // `decide` reports "nothing qualified" as an error, not `Ok(None)`.
        let err = decide(&mut s2, &scoring(&higher, &defs, &profile, &no_block))
            .expect_err("indexer floor 20 rejects 10 seeders");
        assert!(
            format!("{err}").contains("no suitable release"),
            "unexpected error: {err}"
        );
        assert!(s2.chosen.is_none());

        // And a *lower* indexer floor overrides downward, which `max` would not.
        let lower = [(ix, 25u32, 2u32)];
        let mut s3 = state.clone();
        s3.candidates[0].seeders = Some(3);
        decide(&mut s3, &scoring(&lower, &defs, &profile, &no_block)).unwrap();
        assert!(
            s3.chosen.is_some(),
            "indexer floor 2 accepts 3 seeders despite the profile's 5"
        );
    }

    /// SKADI-T-0539: indexer priority breaks a tie between otherwise-equal
    /// candidates (lower number = more trusted, Sonarr's convention) — and never
    /// promotes a worse release, which is why it sits below the format score in
    /// the ranking tuple.
    #[test]
    fn indexer_priority_breaks_ties_but_does_not_beat_quality() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&[
            "Movie.2020.1080p.BluRay.x264-AAA",
            "Movie.2020.1080p.BluRay.x264-BBB",
        ]);
        // Same quality, same seeders — only the indexer differs.
        let a = skadi_core::IndexerId::new();
        let b = skadi_core::IndexerId::new();
        state.candidates[0].indexer = a;
        state.candidates[1].indexer = b;
        state.candidates[0].seeders = Some(50);
        state.candidates[1].seeders = Some(50);

        // B is the more trusted indexer (lower priority number).
        let flags: Vec<(skadi_core::IndexerId, u32, u32)> = vec![(a, 40, 0), (b, 5, 0)];
        let mut s1 = state.clone();
        decide(
            &mut s1,
            &Scoring {
                indexer_flags: &flags,
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        assert!(
            s1.chosen.as_ref().unwrap().title.contains("BBB"),
            "the more trusted indexer wins a tie, got {}",
            s1.chosen.unwrap().title
        );

        // But a favoured indexer must NOT drag a lower-quality release above a
        // better one — priority ranks below quality, so 1080p from the distrusted
        // indexer still beats 720p from the trusted one.
        let mut s2 = state_for(&[
            "Movie.2020.1080p.BluRay.x264-AAA",
            "Movie.2020.720p.BluRay.x264-BBB",
        ]);
        s2.candidates[0].indexer = a;
        s2.candidates[1].indexer = b;
        decide(
            &mut s2,
            &Scoring {
                indexer_flags: &flags,
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        assert!(
            s2.chosen.as_ref().unwrap().title.contains("1080p"),
            "quality outranks indexer priority, got {}",
            s2.chosen.unwrap().title
        );
    }

    /// SKADI-T-0538: `Upgraded` fires only when the import actually replaced a
    /// file. A re-import of the same release also finds a file at the
    /// destination, so inferring the event from "a file existed" would report an
    /// upgrade that upgraded nothing — the fact has to come from
    /// `ImportOutcome.replaced`.
    #[tokio::test]
    async fn upgraded_is_emitted_only_when_something_was_replaced() {
        use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
        use std::sync::Mutex;

        #[derive(Default)]
        struct Spy(Mutex<Vec<NotificationKind>>);
        #[async_trait::async_trait]
        impl Notifier for Spy {
            fn id(&self) -> skadi_core::NotifierId {
                skadi_core::NotifierId::new()
            }
            fn channels(&self) -> &[NotificationKind] {
                &[NotificationKind::Imported, NotificationKind::Upgraded]
            }
            async fn notify(&self, event: &NotificationEvent) -> Result<()> {
                self.0.lock().unwrap().push(event.kind());
                Ok(())
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
        }

        // A first import replaces nothing → Imported.
        let spy = Arc::new(Spy::default());
        let notifiers: Vec<Arc<dyn Notifier>> = vec![spy.clone()];
        let mut state = state_for(&["Movie.2020.1080p.BluRay.x264-GRP"]);
        state.outcome = Some(skadi_importer::ImportOutcome::default());
        notify(&state, &notifiers).await.unwrap();
        assert_eq!(
            spy.0.lock().unwrap().as_slice(),
            &[NotificationKind::Imported]
        );

        // An import that superseded a held file → Upgraded, naming what it replaced.
        let spy2 = Arc::new(Spy::default());
        let notifiers2: Vec<Arc<dyn Notifier>> = vec![spy2.clone()];
        let mut upgraded = state_for(&["Movie.2020.2160p.BluRay.x265-GRP"]);
        upgraded.outcome = Some(skadi_importer::ImportOutcome {
            replaced: vec![std::path::PathBuf::from(
                "/lib/Movie (2020)/Movie.1080p.mkv",
            )],
            ..Default::default()
        });
        notify(&upgraded, &notifiers2).await.unwrap();
        assert_eq!(
            spy2.0.lock().unwrap().as_slice(),
            &[NotificationKind::Upgraded]
        );
    }

    #[test]
    fn decide_picks_highest_allowed_quality() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&[
            "Movie.2020.720p.BluRay.x264-LOW",
            "Movie.2020.1080p.BluRay.x264-HIGH",
        ]);
        decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        let chosen = state.chosen.unwrap();
        assert!(chosen.title.contains("1080p"), "chose {}", chosen.title);
    }

    /// `decide` records every accepted candidate best-first (SKADI-T-0589), so
    /// `snatch` can fall back along the same order it would have chosen.
    #[test]
    fn decide_records_the_ranked_order() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&[
            "Movie.2020.720p.BluRay.x264-LOW",
            "Movie.2020.1080p.BluRay.x264-HIGH",
            "Movie.2020.720p.BluRay.x264-ALSO",
        ]);
        decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        assert_eq!(state.ranked.len(), 3, "every accepted candidate is ranked");
        assert_eq!(state.ranked[0], 1, "the 1080p release ranks first");
        assert_eq!(
            state.candidates[state.ranked[0]].title,
            state.chosen.as_ref().unwrap().title,
            "chosen is the head of the ranked order"
        );
        // Equal 720p candidates keep search order (earlier index first).
        assert_eq!(&state.ranked[1..], &[0, 2]);
    }

    /// SKADI-T-0598: with the policy on, a well-seeded 720p beats a lone-seeder
    /// 1080p; an SD rip never beats anything above the floor however many
    /// seeders it has; with the policy off, quality-first order is unchanged.
    #[test]
    fn decide_prefers_reachable_releases_above_the_resolution_floor() {
        use skadi_quality::Resolution;
        let mut defs = default_definitions();
        defs.sort_by_key(|d| d.resolution);
        let profile = QualityProfile {
            id: ProfileId::new(),
            name: "all".into(),
            allowed: defs.iter().map(|d| d.id).collect(),
            cutoff: defs.last().unwrap().id,
            upgrade_allowed: true,
            formats: vec![],
            min_format_score: 0,
        };
        let mut state = state_for(&[]);
        state.candidates = vec![
            // Small enough for the DVD size ceiling; the seeders are the point.
            Release {
                size: 1_500_000_000,
                ..release("Movie.2020.480p.DVDRip.x264-GRP", 500)
            },
            release("Movie.2020.1080p.BluRay.x264-GRP", 1),
            release("Movie.2020.720p.WEB-DL.x264-GRP", 40),
        ];
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };
        let on = ReachabilityPolicy {
            prefer_seeders: true,
            floor: Resolution::R720p,
        };
        decide_with(&mut state, &scoring, on).unwrap();
        assert_eq!(
            state.ranked.len(),
            3,
            "all three classify and are allowed: {:?} / dvd explain: {:?}",
            state.tally.as_ref().and_then(|t| t.top_reason()),
            explain(&state.candidates[0], &scoring).reason
        );
        let chosen = state.chosen.clone().unwrap().title;
        assert!(chosen.contains("720p"), "reachable 720p wins: {chosen}");
        let order: Vec<String> = state
            .ranked
            .iter()
            .map(|&i| state.candidates[i].title.clone())
            .collect();
        assert!(
            order[1].contains("1080p") && order[2].contains("480p"),
            "{order:?}"
        );

        // Raising the floor to 1080p puts the 720p below it again.
        let strict = ReachabilityPolicy {
            prefer_seeders: true,
            floor: Resolution::R1080p,
        };
        decide_with(&mut state, &scoring, strict).unwrap();
        assert!(state.chosen.clone().unwrap().title.contains("1080p"));

        let off = ReachabilityPolicy {
            prefer_seeders: false,
            floor: Resolution::R720p,
        };
        decide_with(&mut state, &scoring, off).unwrap();
        assert!(state.chosen.clone().unwrap().title.contains("1080p"));

        assert_eq!(floor_for_height(720), Resolution::R720p);
        assert_eq!(floor_for_height(1000), Resolution::R1080p);
        assert_eq!(floor_for_height(0), Resolution::Sd);
        assert_eq!(seeder_band(0), 0);
        assert_eq!(seeder_band(2), 1);
        assert!(seeder_band(12) > seeder_band(9));
    }

    // --- movie/TV title relevance gate (SKADI-T-0374) ---

    /// Movie/TV candidates are gated by title relevance (SKADI-T-0374): releases
    /// with coverage below `MIN_TITLE_COVERAGE` are rejected. This prevents the
    /// title-fallback tier from grabbing unrelated media that shares no tokens.
    #[test]
    fn identity_rejection_matches_the_automatic_gates() {
        use skadi_core::MediaKind::{Movie, Series};
        let w = |t: &str| vec![t.to_string()];
        let rel = |t: &str| release(t, 5);
        assert!(
            identity_rejection(
                Movie,
                &w("The Guest"),
                Some(2014),
                None,
                &rel("The.Adam.Project.2022.1080p.NF.WEBRip.DDP5.1.Atmos.x264-TEPES")
            )
            .is_some()
        );
        assert!(
            identity_rejection(
                Movie,
                &w("The Guest"),
                Some(2014),
                None,
                &rel("The.Guest.2014.1080p.BluRay.x264-GRP")
            )
            .is_none()
        );
        assert_eq!(
            identity_rejection(
                Movie,
                &w("The Guest"),
                Some(2014),
                None,
                &rel("The.Guest.1992.DVDRip")
            )
            .as_deref(),
            Some("names a different year")
        );
        assert!(
            identity_rejection(
                Movie,
                &w("101 Dalmatians"),
                Some(1996),
                None,
                &rel("101.Dalmatians.II.Patchs.London.Adventure.2003")
            )
            .is_some()
        );
        // An adult-category release that happens to contain the word: off-category, before the title is even read.
        let mut xxx = rel("Entertaining the Guest [MP4 1080p]");
        xxx.categories = vec![skadi_indexers::Category(6000)];
        assert_eq!(
            identity_rejection(Movie, &w("The Guest"), Some(2014), None, &xxx).as_deref(),
            Some("off-category for this kind")
        );
        let scope = TvScope {
            season: 1,
            episode: Some(1),
            absolute: None,
            air_date: None,
        };
        assert!(
            identity_rejection(
                Series,
                &w("Mystery Science Theater 3000"),
                None,
                Some(scope),
                &rel("Mystery.Science.Theater.3000.The.Return.S01E01.XviD-AFG")
            )
            .is_some()
        );
        assert!(
            identity_rejection(
                Series,
                &w("Mystery Science Theater 3000"),
                None,
                Some(scope),
                &rel("Mystery.Science.Theater.3000.S01E01.XviD")
            )
            .is_none()
        );
        assert!(
            identity_rejection(
                Series,
                &w("Firefly"),
                None,
                Some(scope),
                &rel("Some.Other.Show.S01E01.720p")
            )
            .is_some()
        );
        // A release group's tag is not the show, and the number in the name counts.
        let s1e3 = TvScope {
            season: 1,
            episode: Some(3),
            absolute: None,
            air_date: None,
        };
        assert_eq!(
            identity_rejection(
                Series,
                &w("Dimension 20"),
                None,
                Some(s1e3),
                &rel("11 22 63 S01E03 720p HDTV X264-DIMENSION[ettv]")
            )
            .as_deref(),
            Some("does not name the wanted title")
        );
        assert!(
            identity_rejection(
                Series,
                &w("Dimension 20"),
                None,
                Some(s1e3),
                &rel("Dimension.20.S01E03.720p.WEB.h264-GRP")
            )
            .is_none()
        );
        assert!(
            identity_rejection(
                Series,
                &w("The 100"),
                None,
                Some(scope),
                &rel("The.100.S01E01.1080p.WEB.H264-GRP")
            )
            .is_none()
        );
        assert!(
            identity_rejection(
                Series,
                &w("Doctor Who (2005)"),
                None,
                Some(scope),
                &rel("Doctor.Who.S01E01.720p")
            )
            .is_none()
        );
        assert!(
            identity_rejection(
                Movie,
                &w("2012"),
                Some(2009),
                None,
                &rel("2012.2009.1080p.BluRay.x264")
            )
            .is_none()
        );
    }

    #[test]
    fn tv_extra_title_tokens_flags_spin_offs_but_not_markers() {
        let w = |t: &str| vec![t.to_string()];
        assert_eq!(
            tv_extra_title_tokens(
                &w("Mystery Science Theater 3000"),
                "Mystery.Science.Theater.3000.The.Return.S01E01.XviD-AFG"
            ),
            Some(vec!["return".to_string()])
        );
        assert_eq!(
            tv_extra_title_tokens(
                &w("LEGO Star Wars"),
                "LEGO Star Wars Rebuild the Galaxy S01E01 1080p WEB h264-DOLORES"
            ),
            Some(vec!["rebuild".to_string(), "galaxy".to_string()])
        );
        assert!(
            tv_extra_title_tokens(
                &w("Critical Role"),
                "Critical Role Cooldown S02E012 C4 E012 720p WEB-DL"
            )
            .is_some()
        );
        assert!(
            tv_extra_title_tokens(
                &w("Critical Role"),
                "Critical Role S03E011 Chasing Nightmares 720p WEB-DL AAC2 0 H 264-Kitsune"
            )
            .is_none()
        );
        assert!(
            tv_extra_title_tokens(&w("The Office (US)"), "The.Office.US.S05E01.720p.HDTV")
                .is_none()
        );
        assert!(
            tv_extra_title_tokens(
                &w("Doctor Who (2005)"),
                "Doctor.Who.2005.S01E01.1080p.BluRay"
            )
            .is_none()
        );
        assert!(
            tv_extra_title_tokens(
                &w("Spider-Man: The New Animated Series"),
                "Spider-Man.The.New.Animated.Series.S01E01.DVDRip.XviD"
            )
            .is_none()
        );
        // An indexer's site stamp is not part of the show's name.
        assert!(
            tv_extra_title_tokens(
                &w("Critical Role"),
                "Www UIndex org Critical Role S03E018 Hungry Jungle 720p WEB DL AAC2 264 Kitsune"
            )
            .is_none()
        );
        assert!(
            tv_extra_title_tokens(
                &w("Critical Role"),
                "www.torrenting.com - Critical Role S03E018 720p WEB-DL"
            )
            .is_none()
        );
        // Aliases count as wanted words too.
        assert!(
            tv_extra_title_tokens(
                &["MST3K".into(), "Mystery Science Theater 3000".into()],
                "MST3K.S05E01.XviD"
            )
            .is_none()
        );
    }

    #[test]
    fn decide_gates_movies_by_title_relevance() {
        let (profile, defs) = profile_with_two_ranks();
        // Create a state wanting "The Matrix" (year 1999)
        let mut state = AcquireState::new(
            AcquirableRef("ed-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["The Matrix".into()],
                year: Some(1999),
                external_ids: ExternalIds::default(),
                categories: vec![],
                tv: None,
                series: None,
                tags: None,
            },
            ProfileId::new(),
        );

        // Candidates: one matches ("Matrix" in title), two don't match at all
        state.candidates = vec![
            // Good match: has "Matrix" in title
            release("The.Matrix.1999.1080p.BluRay.x264-GRP", 10),
            // No match: completely unrelated (more seeders, but should be rejected)
            release("Blade.Runner.2049.1080p.BluRay.x264-XYZ", 100),
            // No match: also unrelated (even more seeders)
            release("Inception.2010.1080p.BluRay.x264-ABC", 200),
        ];

        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        decide(&mut state, &scoring).unwrap();
        let chosen = state.chosen.unwrap();
        // Should pick The.Matrix, NOT Blade.Runner/Inception despite more seeders,
        // because the unrelated titles fail the coverage gate (0 matching tokens)
        assert!(
            chosen.title.contains("Matrix"),
            "expected Matrix title, chose {}",
            chosen.title
        );
    }

    /// Movies with no matching title at all result in a NotFound error.
    #[test]
    fn decide_errors_when_no_title_matches() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = AcquireState::new(
            AcquirableRef("ed-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["Inception".into()],
                year: Some(2010),
                external_ids: ExternalIds::default(),
                categories: vec![],
                tv: None,
                series: None,
                tags: None,
            },
            ProfileId::new(),
        );
        // All candidates are unrelated titles
        state.candidates = vec![
            release("Blade.Runner.2049.1080p.BluRay.x264-XYZ", 20),
            release("Interstellar.2014.1080p.BluRay.x264-ABC", 15),
        ];

        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        let err = decide(&mut state, &scoring).unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    // --- TV episode identity gate (SKADI-T-0386) ---

    fn scope(season: u16, episode: Option<u16>) -> TvScope {
        TvScope {
            season,
            episode,
            absolute: None,
            air_date: None,
        }
    }

    /// The production grab (SKADI-T-0587): "Other Worlds Than These" (Stephen
    /// King) carries the one-word series alias "Talisman"; a music discography
    /// naming that word was a `book_pack` with full series coverage and no author
    /// check at all.
    #[test]
    fn audiobook_bucket_holds_packs_to_the_author_gate() {
        let titles: Vec<String> = vec![
            "Other Worlds Than These".into(),
            "Other Worlds Than These Stephen King".into(),
        ];
        let series = Some("Talisman");
        assert_eq!(
            audiobook_bucket(
                "Talisman - Discography 1990-2006 (Japanese editions, 10 albums), FLAC",
                &titles,
                series,
            ),
            None,
            "a pack that never names the author is not this series"
        );
        // A real pack of the series, by the author → still the pack bucket.
        assert_eq!(
            audiobook_bucket("Stephen King - Talisman - Books 1-2 [M4B]", &titles, series),
            Some(PACK_BUCKET)
        );
        // A two-word series covered by scattered common words (2026-09-18): an
        // After Effects template pack was chosen for a Sanderson book.
        let sanderson: Vec<String> = vec![
            "Isles of the Emberdark".into(),
            "Isles of the Emberdark Brandon Sanderson".into(),
        ];
        assert_eq!(
            audiobook_bucket(
                "Drop Drop Top Secret Gadget Collection-After Effects Projects{h33t}{tpsj}",
                &sanderson,
                Some("Secret Projects"),
            ),
            None
        );
        // The genuine author-less series pack still passes: the series explains
        // most of its title.
        assert_eq!(
            audiobook_bucket(
                "Secret Projects - Books 1-4 [M4B]",
                &sanderson,
                Some("Secret Projects")
            ),
            Some(PACK_BUCKET)
        );
    }

    #[test]
    fn tv_episode_rejection_mirrors_decide() {
        let s4e35 = scope(4, Some(35));
        assert_eq!(
            tv_episode_rejection("Critical.Role.S04E35.1080p.WEB.h264-GRP", s4e35),
            None
        );
        assert!(tv_episode_rejection("Critical.Role.S04E01.1080p.WEB.h264-GRP", s4e35).is_some());
        assert!(tv_episode_rejection("Critical Role S04 Complete 1080p", s4e35).is_some());
        // A season-scoped search accepts the pack.
        assert_eq!(
            tv_episode_rejection("Critical Role S04 Complete 1080p", scope(4, None)),
            None
        );
    }

    #[test]
    fn tv_scope_match_verifies_season_and_episode() {
        let s3e2 = scope(3, Some(2));
        // The wanted episode, in the common shapes.
        assert_eq!(
            tv_scope_match("Lioness.2023.S03E02.1080p.WEB.h264-GRP", s3e2),
            Some(TvMatch::Episode)
        );
        assert_eq!(
            tv_scope_match("Lioness 3x02 720p HDTV", s3e2),
            Some(TvMatch::Episode)
        );
        assert_eq!(
            tv_scope_match("Lioness.S03E01-E04.1080p.WEB", s3e2),
            Some(TvMatch::Episode),
            "an episode range containing the wanted episode"
        );
        // The live failure: a different episode of a different season.
        assert_eq!(
            tv_scope_match("Lioness 2023 S02E06 Kitsune 1080p AMZN WEB-DL", s3e2),
            None
        );
        // Right season, wrong episode.
        assert_eq!(tv_scope_match("Lioness.S03E03.1080p.WEB", s3e2), None);
        // Movie-shaped: no TV markers at all (Defiance / Dark Angel / Dino Time).
        assert_eq!(
            tv_scope_match("Les Insurgés (2008) Defiance 1080p BluRay", s3e2),
            None
        );
        assert_eq!(
            tv_scope_match("Dark.Angel.1990.REMASTERED.1080p.BluRay.x264", s3e2),
            None
        );
        // Specials: wanted S00E11 must not accept S02E06 (or S00E03).
        let s0e11 = scope(0, Some(11));
        assert_eq!(tv_scope_match("Lioness.S02E06.1080p.WEB", s0e11), None);
        assert_eq!(tv_scope_match("Lioness.S00E03.1080p.WEB", s0e11), None);
        assert_eq!(
            tv_scope_match("Lioness.S00E11.Behind.The.Scenes.1080p.WEB", s0e11),
            Some(TvMatch::Episode)
        );
    }

    #[test]
    fn tv_scope_match_accepts_packs_for_the_wanted_season_only() {
        let s3e2 = scope(3, Some(2));
        assert_eq!(
            tv_scope_match("Lioness.S03.1080p.WEB.h264-GRP", s3e2),
            Some(TvMatch::Pack)
        );
        assert_eq!(
            tv_scope_match("Lioness Season 3 COMPLETE 1080p WEB", s3e2),
            Some(TvMatch::Pack)
        );
        assert_eq!(
            tv_scope_match("Lioness Complete Series 1080p WEB-DL", s3e2),
            Some(TvMatch::Pack)
        );
        // A pack for another season is not this episode.
        assert_eq!(tv_scope_match("Lioness.S02.1080p.WEB", s3e2), None);
        // Season-pack scope (`episode: None`) takes packs, not singles.
        let s3 = scope(3, None);
        assert_eq!(
            tv_scope_match("Lioness.S03.1080p.WEB", s3),
            Some(TvMatch::Pack)
        );
        assert_eq!(tv_scope_match("Lioness.S03E02.1080p.WEB", s3), None);
        // Specials never take the pack shortcut: a "S00" release is a named
        // special/arc with no defined membership (live: `S00.Heart.of.Archness`
        // grabbed for S00E01), and a complete-series pack omits specials.
        let s0e1 = scope(0, Some(1));
        assert_eq!(
            tv_scope_match(
                "Archer.2009.S00.Heart.of.Archness.1080p.BluRay.REMUX.AVC-NOGRP",
                s0e1
            ),
            None
        );
        assert_eq!(
            tv_scope_match("Archer Complete Series 1080p WEB-DL", s0e1),
            None
        );
        assert_eq!(
            tv_scope_match("Archer.2009.S00E01.Archersaurus.720p.WEB", s0e1),
            Some(TvMatch::Episode)
        );
        assert_eq!(tv_scope_match("Archer.S00.1080p.WEB", scope(0, None)), None);
    }

    #[test]
    fn tv_scope_match_verifies_absolute_and_air_date_only_when_known() {
        // Anime absolute numbering: accepted only when the scope knows the number.
        let mut anime = scope(1, Some(14));
        assert_eq!(
            tv_scope_match("[SubsPlease] Frieren - 14 (1080p)", anime),
            None
        );
        anime.absolute = Some(14);
        assert_eq!(
            tv_scope_match("[SubsPlease] Frieren - 14 (1080p)", anime),
            Some(TvMatch::Episode)
        );
        assert_eq!(
            tv_scope_match("[SubsPlease] Frieren - 15 (1080p)", anime),
            None
        );
        // Daily shows: accepted only on the wanted air date.
        let mut daily = scope(2024, Some(45));
        assert_eq!(
            tv_scope_match("The.Daily.Show.2024.03.01.720p.WEB", daily),
            None
        );
        daily.air_date = chrono::NaiveDate::from_ymd_opt(2024, 3, 1);
        assert_eq!(
            tv_scope_match("The.Daily.Show.2024.03.01.720p.WEB", daily),
            Some(TvMatch::Episode)
        );
        assert_eq!(
            tv_scope_match("The.Daily.Show.2024.03.02.720p.WEB", daily),
            None
        );
    }

    /// A TV state whose candidates went through `search`'s TV re-parse, as live
    /// ones do.
    fn tv_state(titles: &[&str], tv: TvScope) -> AcquireState {
        let mut state = AcquireState::new(
            AcquirableRef("ep-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Series,
                titles: vec!["Lioness".into()],
                year: Some(2023),
                external_ids: ExternalIds::default(),
                categories: vec![],
                tv: Some(tv),
                series: None,
                tags: None,
            },
            ProfileId::new(),
        )
        .tap_candidates(titles);
        reparse_tv(&mut state.candidates);
        state
    }

    /// The upstream cause of the live mis-grabs: the adapters' movie-shaped parse
    /// found no quality in a yearless episode name (it split on the year), so
    /// `explain` rejected 142/149 candidates as "unrecognized quality" and only
    /// year-bearing packs/films survived. The TV re-parse gives the candidate
    /// its real quality tags plus the TV fields (the movie parser's own yearless
    /// fallback, SKADI-T-0387, also covers the tags — belt and braces).
    #[test]
    fn reparse_tv_recovers_quality_from_yearless_episode_names() {
        let (profile, defs) = profile_with_two_ranks();
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };
        let mut rel = release("Lioness.S03E02.1080p.BluRay.x264-GRP", 10);
        // The movie parser knows nothing of episodes: `S03E02` lands in the title.
        assert_eq!(rel.parsed.title.as_deref(), Some("Lioness S03E02"));
        assert_eq!(rel.parsed.season, None);
        reparse_tv(std::slice::from_mut(&mut rel));
        assert_eq!(rel.parsed.title.as_deref(), Some("Lioness"));
        assert_eq!(rel.parsed.season, Some(3));
        assert_eq!(rel.parsed.episodes, vec![2]);
        assert_eq!(rel.parsed.resolution.as_deref(), Some("1080p"));
        assert_eq!(rel.parsed.source.as_deref(), Some("BluRay"));
        assert!(explain(&rel, &scoring).accepted);
        // Bracketed anime quality only the TV parser reads: `(1080p)`.
        let mut anime = release("[SubsPlease] Frieren - 14 (1080p) [ABCD1234].mkv", 10);
        reparse_tv(std::slice::from_mut(&mut anime));
        assert_eq!(anime.parsed.resolution.as_deref(), Some("1080p"));
        assert_eq!(anime.parsed.absolute, vec![14]);
    }

    /// SKADI-T-0671: the release names served on 2026-10-02.
    #[test]
    fn language_signal_reads_the_production_names() {
        use LanguageSignal::*;
        for dub in [
            "True Detective S03E01 1080p ColdFilm",
            "The Terror S02E02 1080p rus LostFilm TV mkv",
            "Patriot S01E06 1080p rus LostFilm TV mkv",
            "Into the Badlands S03E14 720p WEB rus LostFilm TV",
            "The Eleventh Rule of the Wizard, or the Confessor - Terry Goodkind [Audiobook, Yandex SpeechKit (Filipp), 2020]",
        ] {
            assert_eq!(language_signal(dub), Reject, "{dub}");
        }
        for english in [
            "Терри Гудкайнд / Terry Goodkind - Одиннадцатое Правило, Исповедница / Confessor [Sam Tsoutsouvas, 2007]",
            "Terry Goodkind - The Law of Nines / Терри Гудкайнд - Закон девяток [Марк Дикенс , 2009 , 128 kbps]",
            "True Detective S03E02 Kiss Tomorrow Goodbye REPACK 720p AMZN WEBRip DDP5 1 x264-NTb",
            "PLUTO S01 1080p NF WEB-DL DDP5.1 H 264-VARYG (Dual-Audio, Multi-Subs)",
            "Paradise.2025.S01.1080p.ITA-ENG.MULTI.WEBRip.x265.AAC-V3SP4EV3R",
            "The Terror S02E03 iTALiAN MULTi 1080p WEB x264 M109",
            "Lazarus S01E06 DUBBED 1080p WEB H264-SuccessfulCrab EZTV",
        ] {
            assert_eq!(language_signal(english), Fine, "{english}");
        }
        assert_eq!(
            language_signal("The.Wailing.2016.KOREAN.1080p.BluRay.x264"),
            ForeignOnly
        );
        assert_eq!(
            language_signal("Some Show S01E01 iTALiAN 1080p WEB"),
            ForeignOnly
        );
    }

    /// SKADI-T-0671 end to end: the Russian dub is rejected even with ten times
    /// the seeders, and a release tagged only Italian loses to an English one
    /// of lower quality — but wins when it is all there is.
    #[test]
    fn decide_drops_dubs_and_ranks_foreign_only_releases_last() {
        let (profile, defs) = profile_with_two_ranks();
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };
        let mut state = tv_state(
            &[
                "Lioness.S03E02.1080p.BluRay.x264-ColdFilm",
                "Lioness.S03E02.1080p.BluRay.x264.iTALiAN-GRP",
                "Lioness.S03E02.720p.BluRay.x264-RIGHT",
            ],
            scope(3, Some(2)),
        );
        state.candidates[0].seeders = Some(500);
        decide(&mut state, &scoring).unwrap();
        assert!(state.chosen.as_ref().unwrap().title.contains("RIGHT"));
        assert_eq!(
            state.tally.as_ref().unwrap().rejected.get("language"),
            Some(&1)
        );

        let mut state = tv_state(
            &["Lioness.S03E02.1080p.BluRay.x264.iTALiAN-GRP"],
            scope(3, Some(2)),
        );
        decide(&mut state, &scoring).unwrap();
        assert!(state.chosen.as_ref().unwrap().title.contains("iTALiAN"));
    }

    /// End to end through `decide`: the live 2026-09-05 shape. Wanted S3E2; the
    /// indexer offers the wrong episode (better seeded), a same-named film, the
    /// right episode (yearless, as most episode names are), and a season pack.
    ///
    /// Sonarr parity (SKADI-T-0402): on a **single-episode** search the right
    /// episode wins and the season pack is rejected along with the wrong ones —
    /// three `episode` rejections. Before this, the pack outranked every single,
    /// so each missing episode of a season snatched the same pack.
    #[test]
    fn decide_rejects_wrong_episodes_and_the_season_pack_on_an_episode_search() {
        let (profile, defs) = profile_with_two_ranks();
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };
        let mut state = tv_state(
            &[
                "Lioness.2023.S02E06.Kitsune.1080p.BluRay.x264-WRONG",
                "Lioness.2019.1080p.BluRay.x264-FILM",
                "Lioness.S03E02.720p.BluRay.x264-RIGHT",
                "Lioness.S03.720p.BluRay.x264-PACK",
            ],
            scope(3, Some(2)),
        );
        state.candidates[0].seeders = Some(500);
        decide(&mut state, &scoring).unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("RIGHT"),
            "chose {}",
            state.chosen.as_ref().unwrap().title
        );
        let tally = state.tally.as_ref().unwrap();
        // wrong episode, same-named film, and the season pack.
        assert_eq!(tally.rejected.get("episode"), Some(&3), "{tally:?}");

        // With no pack on offer the exact episode wins over the wrong-but-1080p one.
        let mut state = tv_state(
            &[
                "Lioness.2023.S02E06.Kitsune.1080p.BluRay.x264-WRONG",
                "Lioness.S03E02.720p.BluRay.x264-RIGHT",
            ],
            scope(3, Some(2)),
        );
        decide(&mut state, &scoring).unwrap();
        assert!(state.chosen.as_ref().unwrap().title.contains("RIGHT"));

        // Only wrong episodes/films ⇒ no release, and the tally says why.
        let mut state = tv_state(
            &[
                "Lioness.2023.S02E06.Kitsune.1080p.BluRay.x264-WRONG",
                "Lioness.2019.1080p.BluRay.x264-FILM",
            ],
            scope(3, Some(2)),
        );
        let err = decide(&mut state, &scoring).unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
        assert_eq!(
            state.tally.as_ref().unwrap().rejected.get("episode"),
            Some(&2)
        );
    }

    // --- movie year gate (SKADI-T-0387) ---

    #[test]
    fn movie_year_match_allows_one_year_of_slack_and_no_year() {
        assert_eq!(
            movie_year_match("101.Dalmatians.1996.1080p.BluRay.x264", Some(1996)),
            Some(true)
        );
        assert_eq!(
            movie_year_match("101.Dalmatians.1997.1080p.BluRay.x264", Some(1996)),
            Some(true),
            "±1 covers festival vs wide-release dating"
        );
        assert_eq!(
            movie_year_match("101.Dalmatians.1961.1080p.BluRay.x264", Some(1996)),
            Some(false)
        );
        assert_eq!(
            movie_year_match(
                "101.Dalmatians.II.Patchs.London.Adventure.2003.1080p.WEB",
                Some(1996)
            ),
            Some(false)
        );
        assert_eq!(
            movie_year_match("101.Dalmatians.1080p.BluRay.x264", Some(1996)),
            None,
            "yearless is not a mismatch"
        );
        assert_eq!(
            movie_year_match("101.Dalmatians.1961.1080p.BluRay.x264", None),
            None,
            "no wanted year, nothing to gate on"
        );
    }

    #[test]
    fn sequel_marker_mismatch_catches_numbered_entries_the_wanted_title_lacks() {
        let wanted = vec!["101 Dalmatians".to_string()];
        assert!(sequel_marker_mismatch(
            &wanted,
            "101.Dalmatians.II.Patchs.London.Adventure.1080p.WEB"
        ));
        assert!(!sequel_marker_mismatch(
            &wanted,
            "101.Dalmatians.1996.1080p.BluRay.x264"
        ));
        let toy = vec!["Toy Story".to_string()];
        assert!(sequel_marker_mismatch(&toy, "Toy.Story.3.1080p.BluRay"));
        assert!(sequel_marker_mismatch(&toy, "Toy Story 4 (2019) 2160p"));
        // The wanted title carries the number itself → not a mismatch.
        let toy3 = vec!["Toy Story 3".to_string()];
        assert!(!sequel_marker_mismatch(&toy3, "Toy.Story.3.1080p.BluRay"));
        // Roman numeral that IS part of the wanted title.
        let rocky4 = vec!["Rocky IV".to_string()];
        assert!(!sequel_marker_mismatch(
            &rocky4,
            "Rocky.IV.1985.1080p.BluRay"
        ));
        let rocky = vec!["Rocky".to_string()];
        assert!(sequel_marker_mismatch(&rocky, "Rocky.IV.1985.1080p.BluRay"));
    }

    /// End to end: the live 101 Dalmatians (1996) shape. The 1961 original and
    /// the 2003 sequel are better encodes; both are rejected under `year`, a
    /// yearless sequel under `relevance`, and among the survivors the
    /// verified-1996 release outranks the yearless one even at lower quality.
    #[test]
    fn decide_gates_movies_by_year_and_prefers_a_verified_year() {
        let (profile, defs) = profile_with_two_ranks();
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };
        let mk = |titles: &[&str]| {
            AcquireState::new(
                AcquirableRef("ed-1".into()),
                SearchSpec {
                    trigger: Default::default(),
                    kind: MediaKind::Movie,
                    titles: vec!["101 Dalmatians".into()],
                    year: Some(1996),
                    external_ids: ExternalIds::default(),
                    categories: vec![],
                    tv: None,
                    series: None,
                    tags: None,
                },
                ProfileId::new(),
            )
            .tap_candidates(titles)
        };
        let mut state = mk(&[
            "101.Dalmatians.1961.1080p.BluRay.x264-OLD",
            "101.Dalmatians.II.Patchs.London.Adventure.2003.1080p.BluRay.x264-SEQ",
            "101.Dalmatians.II.Patchs.London.Adventure.1080p.BluRay.x264-SEQNOYEAR",
            "101.Dalmatians.1080p.BluRay.x264-NOYEAR",
            "101.Dalmatians.1996.720p.BluRay.x264-RIGHT",
        ]);
        decide(&mut state, &scoring).unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("RIGHT"),
            "chose {}",
            state.chosen.as_ref().unwrap().title
        );
        let tally = state.tally.as_ref().unwrap();
        assert_eq!(tally.rejected.get("year"), Some(&2), "{tally:?}");
        assert_eq!(tally.rejected.get("relevance"), Some(&1), "{tally:?}");

        // Yearless alone is still acceptable (many scene names omit the year).
        let mut state = mk(&["101.Dalmatians.1080p.BluRay.x264-NOYEAR"]);
        decide(&mut state, &scoring).unwrap();
        assert!(state.chosen.as_ref().unwrap().title.contains("NOYEAR"));

        // Only wrong-year films ⇒ no release.
        let mut state = mk(&[
            "101.Dalmatians.1961.1080p.BluRay.x264-OLD",
            "101.Dalmatians.II.Patchs.London.Adventure.2003.1080p.BluRay.x264-SEQ",
        ]);
        assert!(matches!(
            decide(&mut state, &scoring).unwrap_err(),
            AppError::NotFound(_)
        ));
    }

    // --- release age weighting (SKADI-T-0378) ---

    #[test]
    fn age_score_tiered_values() {
        let now = Utc::now();
        // < 1 hour: 5000
        assert_eq!(super::age_score(now - chrono::Duration::minutes(30)), 5000);
        // < 1 day: 2500
        assert_eq!(super::age_score(now - chrono::Duration::hours(12)), 2500);
        // < 1 week: 1000
        assert_eq!(super::age_score(now - chrono::Duration::days(3)), 1000);
        // < 1 month: 500
        assert_eq!(super::age_score(now - chrono::Duration::days(15)), 500);
        // older: 0
        assert_eq!(super::age_score(now - chrono::Duration::days(60)), 0);
    }

    #[test]
    fn decide_uses_age_as_tiebreaker() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&[]);
        let now = Utc::now();

        // Two releases with same quality, same seeders, but different ages
        let mut newer = release("Movie.2020.1080p.BluRay.x264-NEW", 10);
        newer.published = now - chrono::Duration::hours(1); // < 1 hour old

        let mut older = release("Movie.2020.1080p.BluRay.x264-OLD", 10);
        older.published = now - chrono::Duration::days(30); // 30 days old

        state.candidates = vec![older.clone(), newer.clone()];

        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        decide(&mut state, &scoring).unwrap();
        let chosen = state.chosen.unwrap();
        // Should prefer the newer release as a tiebreaker
        assert!(
            chosen.title.contains("NEW"),
            "expected newer release, chose {}",
            chosen.title
        );
    }

    // --- upgrade-until-cutoff (SKADI-T-0182) ---

    /// With `current_quality` set, `evaluate` grabs only a strictly-better
    /// release: an equal-quality candidate is rejected as "not an upgrade", a
    /// higher one is accepted, and the very same equal candidate IS accepted on a
    /// first acquisition (`current = None`).
    #[test]
    fn evaluate_with_current_grabs_only_strict_upgrades() {
        let (profile, defs) = profile_with_two_ranks();
        let low = defs.iter().find(|d| d.name == "Bluray-720p").unwrap().id;
        let no_block = HashSet::new();
        let scoring = |current| Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &no_block,
            audiobook: None,
            current_quality: current,
            current_format_score: None,
            current_unplayable: false,
        };

        // Holding 720p: a 720p candidate is no upgrade → rejected with the
        // upgrade-specific reason; ranked carries nothing.
        let (v_eq, ranked) = evaluate(
            &release("Movie.2020.720p.BluRay.x264-EQ", 10),
            &scoring(Some(low)),
        );
        assert!(
            !v_eq.accepted,
            "equal quality must not be grabbed as an upgrade"
        );
        assert!(
            v_eq.reason.contains("not an upgrade"),
            "reason: {}",
            v_eq.reason
        );
        assert!(ranked.is_none());

        // Holding 720p: a 1080p candidate IS a strict upgrade → accepted.
        let (v_up, ranked) = evaluate(
            &release("Movie.2020.1080p.BluRay.x264-UP", 10),
            &scoring(Some(low)),
        );
        assert!(
            v_up.accepted,
            "1080p over 720p is an upgrade: {}",
            v_up.reason
        );
        assert!(ranked.is_some());

        // First acquisition (current = None): the same 720p is accepted — proving
        // it's the `current` lever, not the quality, that changed the verdict.
        let (v_first, _) = evaluate(
            &release("Movie.2020.720p.BluRay.x264-EQ", 10),
            &scoring(None),
        );
        assert!(v_first.accepted, "720p is a valid first acquisition");
    }

    /// `decide` on an upgrade run picks a strictly-better candidate, and errors
    /// (no grab) when only an equal-or-worse candidate exists.
    #[test]
    fn decide_upgrade_picks_strictly_better_else_errors() {
        let (profile, defs) = profile_with_two_ranks();
        let low = defs.iter().find(|d| d.name == "Bluray-720p").unwrap().id;
        let no_block = HashSet::new();
        let sc = |current| Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &no_block,
            audiobook: None,
            current_quality: current,
            current_format_score: None,
            current_unplayable: false,
        };

        // Holding 720p, candidates {720p, 1080p} → upgrade to 1080p.
        let mut state = state_for(&[
            "Movie.2020.720p.BluRay.x264-LOW",
            "Movie.2020.1080p.BluRay.x264-HIGH",
        ]);
        state.current_quality = Some(low);
        decide(&mut state, &sc(Some(low))).unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("1080p"),
            "upgraded to the strictly-better release"
        );

        // Holding 720p, only an equal 720p candidate → nothing strictly better →
        // NotFound (the steps layer then leaves the item Imported).
        let mut state = state_for(&["Movie.2020.720p.BluRay.x264-LOW"]);
        state.current_quality = Some(low);
        assert!(matches!(
            decide(&mut state, &sc(Some(low))),
            Err(AppError::NotFound(_))
        ));
        assert!(state.chosen.is_none(), "no equal-quality re-grab");
    }

    /// SKADI-T-0183: with custom formats wired, `decide` ranks a release that
    /// matches a positively-scored format above an otherwise-identical twin that
    /// doesn't (same quality, same seeders → format score is the tiebreaker).
    #[test]
    fn decide_ranks_a_format_matching_release_above_a_non_matching_twin() {
        use skadi_quality::{CustomFormat, CustomFormatScore, FormatRule};
        let (mut profile, defs) = profile_with_two_ranks();
        // A format that rewards x265, assigned +100 by the profile.
        let fmt_id = skadi_core::CustomFormatId::new();
        let registry = vec![CustomFormat {
            id: fmt_id,
            name: "x265".into(),
            rules: vec![FormatRule::Codec("x265".into())],
        }];
        profile.formats = vec![CustomFormatScore {
            format: fmt_id,
            score: 100,
            mode: skadi_quality::FormatMode::Preferred,
        }];

        // Same quality (1080p) + same seeders; only the codec differs.
        let mut state = state_for(&[
            "Movie.2020.1080p.BluRay.x264-B",
            "Movie.2020.1080p.BluRay.x265-A",
        ]);
        decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &registry,
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("x265"),
            "format-matching release should win on custom-format score, chose {}",
            state.chosen.as_ref().unwrap().title
        );
    }

    #[test]
    fn decide_errors_when_nothing_allowed() {
        let (profile, defs) = profile_with_two_ranks();
        // A quality not in `allowed` (e.g. a SDTV/DVD name that won't classify to
        // an allowed id) and an unparseable one.
        let mut state = state_for(&["Some.Random.Cam.Rip", "Movie.2020.480p.DVD-NOPE"]);
        assert!(matches!(
            decide(
                &mut state,
                &Scoring {
                    indexer_flags: &[],
                    definitions: &defs,
                    profile: &profile,
                    formats: &[],
                    min_seeders: 0,
                    blocklisted: &HashSet::new(),
                    audiobook: None,
                    current_quality: None,
                    current_format_score: None,
                    current_unplayable: false,
                },
            ),
            Err(AppError::NotFound(_))
        ));
        assert!(state.chosen.is_none());
    }

    #[test]
    fn decide_skips_torrents_below_min_seeders() {
        // A higher-quality release with too few seeders should be skipped in
        // favour of a lower-quality but well-seeded one (SKADI-T-0113). With the
        // filter off (min_seeders = 0) the higher-quality dead torrent wins —
        // proving the filter is what changes the outcome.
        let (profile, defs) = profile_with_two_ranks();
        let mk = || {
            let mut s = state_for(&["x"]);
            s.candidates = vec![
                release("Movie.2020.1080p.BluRay.x264-DEAD", 0),
                release("Movie.2020.720p.BluRay.x264-LIVE", 5),
            ];
            s
        };

        // Filter on: the 0-seeder 1080p is dropped → the 720p live one is chosen.
        let mut state = mk();
        // Policy off (SKADI-T-0598): this test is about the min_seeders *filter*,
        // not the reachability ranking, which would also pick the seeded 720p.
        decide_with(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 1,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
            QUALITY_FIRST,
        )
        .unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("720p"),
            "with the filter on, the well-seeded 720p should win"
        );

        // Filter off: the higher-quality 1080p wins despite 0 seeders.
        let mut state = mk();
        // Policy off (SKADI-T-0598): this test is about the min_seeders *filter*,
        // not the reachability ranking, which would also pick the seeded 720p.
        decide_with(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
            QUALITY_FIRST,
        )
        .unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("1080p"),
            "with the filter off, the highest-quality release wins"
        );
    }

    #[test]
    fn evaluate_accepts_allowed_and_explains_each_rejection() {
        // The interactive-search verdict (SKADI-T-0114): every candidate gets an
        // accept/reject + a human reason, instead of being silently dropped.
        let (profile, defs) = profile_with_two_ranks();
        let no_block = HashSet::new();
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 2,
            blocklisted: &no_block,
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        // Accepted: an allowed quality with enough seeders. Reason names the
        // quality; ranked carries (rank, score).
        let (v, ranked) = evaluate(&release("Movie.2020.1080p.BluRay.x264-OK", 10), &scoring);
        assert!(v.accepted, "1080p/10-seed should be accepted: {}", v.reason);
        assert!(ranked.is_some());

        // Rejected: too few seeders (below the threshold). Ranked is None.
        let (v, ranked) = evaluate(&release("Movie.2020.1080p.BluRay.x264-DEAD", 1), &scoring);
        assert!(!v.accepted);
        assert!(v.reason.contains("seeders"), "reason was {:?}", v.reason);
        assert!(ranked.is_none());

        // Rejected: a quality not in the profile (480p DVD isn't in allowed).
        let (v, _) = evaluate(&release("Movie.2020.480p.DVD-NOPE", 10), &scoring);
        assert!(!v.accepted);
    }

    #[test]
    fn evaluate_rejects_blocklisted_release() {
        // A release whose key is in the blocklist is rejected before any quality
        // check (SKADI-T-0115), even if it would otherwise be accepted.
        let (profile, defs) = profile_with_two_ranks();
        let good = release("Movie.2020.1080p.BluRay.x264-OK", 10);
        let mut blocked = HashSet::new();
        blocked.insert(release_key(&good));
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &blocked,
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };
        let (v, ranked) = evaluate(&good, &scoring);
        assert!(!v.accepted);
        assert_eq!(v.reason, "blocklisted");
        assert!(ranked.is_none());
    }

    #[test]
    fn decide_skips_blocklisted_and_picks_next_best() {
        // The best release is blocklisted → decide falls through to the next.
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&[]);
        let blocked_release = release("Movie.2020.1080p.BluRay.x264-BAD", 10);
        let ok_release = release("Movie.2020.720p.BluRay.x264-OK", 10);
        state.candidates = vec![blocked_release.clone(), ok_release];
        let mut blocked = HashSet::new();
        blocked.insert(release_key(&blocked_release));
        decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &blocked,
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        assert!(
            state.chosen.as_ref().unwrap().title.contains("720p"),
            "blocklisted 1080p skipped, 720p chosen"
        );
    }

    #[test]
    fn decide_does_not_filter_usenet_releases_without_seeders() {
        // A usenet/NZB release carries no seeders (None); the min-seeders filter
        // must not exclude it even at a high threshold.
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&["x"]);
        let mut nzb = release("Movie.2020.1080p.BluRay.x264-USENET", 0);
        nzb.seeders = None;
        state.candidates = vec![nzb];
        decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 5,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        assert!(
            state.chosen.is_some(),
            "usenet release must not be filtered"
        );
    }

    // --- audiobook scoring (SKADI-I-0017) ---

    #[test]
    fn evaluate_scores_audiobooks_and_applies_the_abridged_floor() {
        use crate::services::AudiobookScoring;
        use skadi_quality::audiobook::default_audiobook_definitions;

        let defs = default_audiobook_definitions();
        let m4b128 = defs.iter().find(|d| d.name == "M4B-128").unwrap().id;
        let profile = QualityProfile {
            id: ProfileId::new(),
            name: "ab".into(),
            allowed: vec![m4b128],
            cutoff: m4b128,
            upgrade_allowed: false,
            formats: vec![],
            min_format_score: 0,
        };
        let no_block = HashSet::new();
        let ab = AudiobookScoring {
            definitions: defs.clone(),
            allow_abridged: false,
        };
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &[],
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &no_block,
            audiobook: Some(&ab),
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        // Unabridged M4B 128kbps → classified to M4B-128 and accepted.
        let (v, ranked) = evaluate(
            &release("Andy Weir - Project Hail Mary (2021) [M4B 128kbps]", 10),
            &scoring,
        );
        assert!(v.accepted, "M4B-128 should be accepted: {}", v.reason);
        assert!(ranked.is_some());

        // Abridged → rejected by the floor (profile disallows abridged).
        let (v, _) = evaluate(
            &release("Andy Weir - Project Hail Mary {Abridged} M4B 128kbps", 10),
            &scoring,
        );
        assert!(!v.accepted && v.reason.contains("abridged"), "{}", v.reason);

        // A format/bitrate with no matching definition → unrecognized.
        let (v, _) = evaluate(&release("Andy Weir - PHM FLAC 320kbps", 10), &scoring);
        assert!(!v.accepted, "FLAC-320 has no definition");
    }

    // --- decide rejection tally (SKADI-T-0380) ---
    //
    // Diagnosing "181 candidates, nothing grabbed" took a session of manual SQL
    // on 2026-09-01 because the count was recorded and the *cause* was not.
    // These pin the tally that replaces that dig.

    /// The gate that dropped each candidate is counted, so a stalled item can
    /// say which gate to loosen.
    #[test]
    fn decide_tallies_rejections_by_gate() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = AcquireState::new(
            AcquirableRef("ed-1".into()),
            SearchSpec {
                trigger: Default::default(),
                kind: MediaKind::Movie,
                titles: vec!["The Matrix".into()],
                year: Some(1999),
                external_ids: ExternalIds::default(),
                categories: vec![],
                tv: None,
                series: None,
                tags: None,
            },
            ProfileId::new(),
        );
        state.candidates = vec![
            // Grabbable.
            release("The.Matrix.1999.1080p.BluRay.x264-GRP", 10),
            // Right shape, wrong film → relevance gate.
            release("Blade.Runner.2049.1080p.BluRay.x264-XYZ", 100),
            release("Inception.2010.1080p.BluRay.x264-ABC", 200),
            // Matches the title but classifies to no known quality → quality gate.
            release("The.Matrix.1999.PotatoCam.x264-BAD", 50),
        ];
        let scoring = Scoring {
            indexer_flags: &[],
            definitions: &defs,
            profile: &profile,
            formats: &[],
            min_seeders: 0,
            blocklisted: &HashSet::new(),
            audiobook: None,
            current_quality: None,
            current_format_score: None,
            current_unplayable: false,
        };

        decide(&mut state, &scoring).unwrap();
        let tally = state.tally.expect("decide records a tally");

        assert_eq!(tally.considered, 4);
        assert_eq!(tally.rejected.get("relevance"), Some(&2));
        assert_eq!(tally.rejected.get("quality"), Some(&1));
        assert_eq!(tally.rejected_total(), 3, "one of four was grabbable");
    }

    /// The tally is recorded on the *failure* path too — that is the case the
    /// operator actually needs explained.
    #[test]
    fn decide_tallies_even_when_nothing_is_grabbable() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&["Totally.Unrelated.2020.1080p.BluRay-X"]);
        let err = decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        );
        assert!(err.is_err(), "nothing should be grabbable");
        let tally = state.tally.expect("tally survives the no-release path");
        assert_eq!(tally.considered, 1);
        assert_eq!(tally.rejected_total(), 1);
        assert_eq!(tally.top_reason().map(|(r, _)| r), Some("relevance"));
    }

    /// Blocklisted and under-seeded candidates land in their own buckets rather
    /// than being lumped under "quality" — they have different remedies.
    #[test]
    fn decide_tally_separates_blocklist_and_seeder_rejections() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&[
            "Movie.2020.1080p.BluRay.x264-BLOCKED",
            "Movie.2020.1080p.BluRay.x264-THIN",
        ]);
        // First candidate blocklisted; second falls under the seeder floor.
        state.candidates[1].seeders = Some(1);
        let blocked: HashSet<String> = [release_key(&state.candidates[0])].into_iter().collect();

        let err = decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 5,
                blocklisted: &blocked,
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        );
        assert!(err.is_err());
        let tally = state.tally.expect("tally recorded");
        assert_eq!(tally.rejected.get("blocklisted"), Some(&1));
        assert_eq!(tally.rejected.get("seeders"), Some(&1));
    }

    /// A grab that only just cleared the relevance gate records its weak score,
    /// so the "101 Dalmatians" → "101 Dalmatians II" shape is visible.
    #[test]
    fn decide_records_chosen_title_relevance() {
        let (profile, defs) = profile_with_two_ranks();
        let mut state = state_for(&["Movie.2020.1080p.BluRay.x264-GRP"]);
        decide(
            &mut state,
            &Scoring {
                indexer_flags: &[],
                definitions: &defs,
                profile: &profile,
                formats: &[],
                min_seeders: 0,
                blocklisted: &HashSet::new(),
                audiobook: None,
                current_quality: None,
                current_format_score: None,
                current_unplayable: false,
            },
        )
        .unwrap();
        let r = state
            .tally
            .expect("tally recorded")
            .chosen_relevance
            .expect("movies record the chosen release's relevance");
        // Wanted title "Movie" is fully present in the release title.
        assert!((0.0..=1.0).contains(&r), "relevance out of range: {r}");
        assert!(
            r >= MIN_TITLE_COVERAGE,
            "a grabbed release cleared the gate"
        );
    }

    /// `summary()` leads with the gate that ate the most, because that is the
    /// one worth loosening; ties fall back to the label so the line is stable.
    #[test]
    fn tally_summary_orders_busiest_gate_first() {
        let mut t = DecisionTally {
            considered: 144,
            ..Default::default()
        };
        for _ in 0..12 {
            t.reject(RejectReason::Size);
        }
        for _ in 0..87 {
            t.reject(RejectReason::Quality);
        }
        for _ in 0..45 {
            t.reject(RejectReason::Relevance);
        }
        assert_eq!(t.summary(), "87 quality, 45 relevance, 12 size");
        assert_eq!(t.top_reason(), Some(("quality", 87)));
        assert_eq!(t.rejected_total(), 144);
    }

    /// An empty tally has no top reason and renders as nothing — callers fall
    /// back to a plain message rather than printing "mostly  (0)".
    #[test]
    fn empty_tally_has_no_summary() {
        let t = DecisionTally::default();
        assert_eq!(t.summary(), "");
        assert_eq!(t.top_reason(), None);
        assert_eq!(t.rejected_total(), 0);
    }
}

#[cfg(test)]
mod circuit_rate_arm_tests {
    use super::*;
    use skadi_indexers::{IndexerHealth, MIN_RATE_SAMPLES};

    fn health(outcomes: &[bool], last_failure: Option<DateTime<Utc>>) -> IndexerHealth {
        let mut h = IndexerHealth {
            last_failure,
            ..Default::default()
        };
        for &failed in outcomes {
            h.recent_outcomes = (h.recent_outcomes << 1) | u32::from(failed);
            h.recent_len = h.recent_len.saturating_add(1).min(32);
            if failed {
                h.consecutive_failures += 1;
            } else {
                h.consecutive_failures = 0;
            }
        }
        h
    }

    /// Fail, fail, succeed — 67 % failure with a longest streak of **2**, one
    /// below the streak threshold. This is the shape production showed and the
    /// shape the streak arm is blind to by construction.
    fn mostly_failing() -> Vec<bool> {
        (0..21).map(|i| i % 3 != 2).collect()
    }

    /// Restore the defaults so these tests don't leak into the process-global
    /// tuning other tests read.
    fn reset() {
        set_circuit_config_full(
            DEFAULT_CIRCUIT_THRESHOLD,
            DEFAULT_CIRCUIT_COOLDOWN_SECS,
            DEFAULT_CIRCUIT_RATE_PCT,
        );
    }

    #[test]
    fn the_rate_arm_opens_what_the_streak_arm_cannot_see() {
        reset();
        let now = Utc::now();
        // The production shape (SKADI-T-0568): ~55 % failure with successes
        // interleaved, so the streak never reaches the threshold of 3.
        let alternating: Vec<bool> = (0..MIN_RATE_SAMPLES * 2).map(|i| i % 2 == 0).collect();
        let h = health(&alternating, Some(now));
        assert!(
            h.consecutive_failures < DEFAULT_CIRCUIT_THRESHOLD,
            "the streak arm must be blind to this, or the test proves nothing"
        );
        assert!(
            h.recent_failure_rate().unwrap() * 100.0 < f32::from(60u8),
            "50 % is under the 60 % default — it should NOT open"
        );
        assert!(!search_circuit_open(Some(&h), now));

        // Now worse than the threshold: fail, fail, succeed — 67 % failing with a
        // longest streak of 2, deliberately under the streak threshold of 3 so
        // only the rate arm can open it.
        let h = health(&mostly_failing(), Some(now));
        assert!(
            h.consecutive_failures < DEFAULT_CIRCUIT_THRESHOLD,
            "streak {} must stay under {DEFAULT_CIRCUIT_THRESHOLD} or this tests the wrong arm",
            h.consecutive_failures
        );
        assert!(h.recent_failure_rate().unwrap() * 100.0 >= f32::from(60u8));
        assert!(search_circuit_open(Some(&h), now), "the rate arm opens it");
        reset();
    }

    #[test]
    fn a_healthy_indexer_is_never_opened_by_the_rate_arm() {
        reset();
        let now = Utc::now();
        let mut v = vec![false; 19];
        v.push(true);
        let h = health(&v, Some(now));
        assert!(!search_circuit_open(Some(&h), now), "5 % must not rest it");
        reset();
    }

    #[test]
    fn the_streak_arm_still_works() {
        reset();
        let now = Utc::now();
        // A straight losing streak, too short for the rate window to report.
        let h = health(&[true, true, true], Some(now));
        assert_eq!(h.recent_failure_rate(), None, "below the sample floor");
        assert!(
            search_circuit_open(Some(&h), now),
            "the original arm must be untouched"
        );
        reset();
    }

    #[test]
    fn the_cooldown_and_half_open_probe_are_shared() {
        reset();
        let now = Utc::now();
        let old = now - chrono::Duration::seconds(DEFAULT_CIRCUIT_COOLDOWN_SECS + 1);
        let h = health(&mostly_failing(), Some(old));
        // Whichever arm tripped, a recovered indexer rejoins the same way.
        assert!(
            !search_circuit_open(Some(&h), now),
            "past the cooldown the circuit is half-open for one probe"
        );
        reset();
    }

    #[test]
    fn setting_the_rate_to_zero_disables_that_arm() {
        reset();
        let now = Utc::now();
        let h = health(&mostly_failing(), Some(now));
        assert!(search_circuit_open(Some(&h), now));
        set_circuit_config_full(DEFAULT_CIRCUIT_THRESHOLD, DEFAULT_CIRCUIT_COOLDOWN_SECS, 0);
        assert!(
            !search_circuit_open(Some(&h), now),
            "0 must mean off, not 'open on any failure'"
        );
        reset();
    }
}
