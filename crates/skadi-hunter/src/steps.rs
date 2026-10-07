//! Runner-free acquisition **step bodies**, shared by the `acquire` and
//! `acquire_release` workflows (SKADI-T-0042).
//!
//! Each step is an `async fn(&mut Context) -> Result<()>` that loads
//! [`AcquireState`] from the JSON context, calls the matching [`crate::pipeline`]
//! function with capabilities pulled from the [`HunterServices`](crate::services)
//! global, writes the appropriate status side effects, then
//! stores the (updated) state back. Returning `skadi_core::Result` keeps them
//! callable both from the thin `#[task]` wrappers in [`crate::workflow`] (which
//! map the error to a Cloacina `TaskError`) **and** in-process from
//! [`crate::worker::start_acquire`] (which runs `search`/`decide` without a
//! runner — see ADR SKADI-A-0002).
//!
//! ## Status writes (unchanged from the original `acquire` task wrappers)
//! - `search` entry: `Searching`.
//! - `decide` no-candidate: `Failed{NoSuitableRelease}`.
//! - `snatch` success: `Grabbed` notify (once) + `Snatched`.
//! - `monitor` `Downloading` poll: `Downloading{progress}` (throttled ≥1%).
//! - `import` success: `Imported`.
//! - a transfer failure (snatch/download/import): `Failed{retry_at}` with a
//!   backoff (the next sweep re-acquires — flaky-source recovery); a hard
//!   download failure also ends the run early (no Cloacina retry storm).
//! - search / no-suitable-release: `Failed{retry_at: None}` (not auto-retried).

use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use cloacina::Context;
use serde_json::Value;

use skadi_core::{AcquisitionStatus, AppError, FailureReason, FileRef, Result};
use skadi_media_probe::MediaProber;
use skadi_store::{BlocklistRepo, DecisionEntry, DecisionHistoryRepo, NewBlocklistEntry};

use crate::pipeline;
use crate::services::{HunterServices, services_for};
use crate::state::{AcquireState, load_state, store_state};
use crate::tracker::{Ownership, TransferWatch};

/// `Downloader::status` polls per `monitor` execution: **one**. Cloacina 0.6's
/// scheduler dispatched tasks *inline and serially* — a task body that sat in
/// a poll loop blocked every other task (and the readiness pass that turns
/// `NotStarted` into `Ready`) for as long as it ran, and the executor's 300 s
/// task timeout then killed it anyway. So `monitor` is a quick check that ends
/// with a retryable error, and the task's retry policy
/// ([`MONITOR_RETRY_DELAY_SECS`] apart, [`MONITOR_RETRY_ATTEMPTS`] deep) paces
/// the watch instead of an in-task sleep (SKADI-T-0388; the design's "bounded
/// retry, then re-execute" model from SKADI-T-0032, taken to its limit).
///
/// Cloacina 0.11 (SKADI-T-0390) dispatches off the scheduler loop, so a slow
/// `monitor` no longer starves its siblings — the one-poll shape is kept
/// because it is still the cheapest way to hold a transfer under watch for
/// hours without pinning an executor slot, not because the runner needs it.
pub const MONITOR_MAX_POLLS: u32 = 1;
/// Interval between polls *within* one execution — irrelevant while
/// [`MONITOR_MAX_POLLS`] is 1, kept for the `acquire` drain path and tests.
pub const MONITOR_POLL_INTERVAL_SECS: u64 = 5;
/// Seconds between `monitor` executions (the task's fixed retry delay).
pub const MONITOR_RETRY_DELAY_SECS: u64 = 30;
/// Total `monitor` executions Cloacina allows one run: 2400 × 30 s = 20 h,
/// comfortably past [`MONITOR_MAX_LIFETIME`] so the lifetime cap — which ends
/// the run cleanly (blocklist + `Failed{retry_at}`) — always fires first.
pub const MONITOR_RETRY_ATTEMPTS: u32 = 2400;

/// Give up on a transfer whose best progress has not moved for this long — a
/// dead torrent, not a slow one (SKADI-T-0388).
pub const MONITOR_STALL_TIMEOUT: chrono::Duration = chrono::Duration::hours(3);
/// Hard ceiling on how long one acquire may keep a transfer under watch,
/// regardless of progress. Past this the run fails terminally and the sweep
/// re-acquires from a better source rather than pinning an executor slot.
pub const MONITOR_MAX_LIFETIME: chrono::Duration = chrono::Duration::hours(18);

/// When to give up on a transfer, resolved from the config plane against the
/// constants above (SKADI-T-0544).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallPolicy {
    pub stall_timeout: chrono::Duration,
    pub max_lifetime: chrono::Duration,
}

impl Default for StallPolicy {
    fn default() -> Self {
        Self {
            stall_timeout: MONITOR_STALL_TIMEOUT,
            max_lifetime: MONITOR_MAX_LIFETIME,
        }
    }
}

impl StallPolicy {
    /// The longest lifetime that still leaves Cloacina's monitor retry budget
    /// room to reach the cap's clean terminal path.
    ///
    /// Past this the budget (`MONITOR_RETRY_ATTEMPTS` x `MONITOR_RETRY_DELAY_SECS`)
    /// would run out *before* the lifetime cap fired, so a zombie would end as a
    /// bare task failure instead of blocklist + `Failed{retry_at}` — a worse
    /// outcome reached by setting a number that looks harmless.
    ///
    /// The margin is exactly the invariant asserted in
    /// `retry_budget_outlasts_the_lifetime_cap` — `budget > lifetime + stall/2`
    /// — rearranged, minus a second so the strict inequality still holds. It is
    /// derived from the *resolved* stall timeout rather than the constant,
    /// because that is a setting too and a longer one eats the same margin.
    ///
    /// Deriving it any tighter would clamp the compiled default: 20 h budget
    /// minus a full 3 h stall timeout is 17 h, below the 18 h default, so a
    /// plausible-looking `budget - stall` would silently shorten every install's
    /// lifetime cap. It did, until a test caught it.
    fn lifetime_ceiling(stall_timeout: chrono::Duration) -> chrono::Duration {
        let budget = chrono::Duration::seconds(
            i64::from(MONITOR_RETRY_ATTEMPTS) * MONITOR_RETRY_DELAY_SECS as i64,
        );
        budget - stall_timeout / 2 - chrono::Duration::seconds(1)
    }

    /// Resolve from raw seconds. `0` or absent means the compiled default —
    /// "give up immediately" is never what an operator means by a timeout, and a
    /// zero lifetime would fail every transfer the moment it was watched.
    #[must_use]
    pub fn resolve(stall_secs: Option<u64>, lifetime_secs: Option<u64>) -> Self {
        let secs = |v: Option<u64>, dflt: chrono::Duration| {
            v.filter(|s| *s > 0)
                .and_then(|s| i64::try_from(s).ok())
                .map_or(dflt, chrono::Duration::seconds)
        };
        let stall_timeout = secs(stall_secs, MONITOR_STALL_TIMEOUT);
        Self {
            stall_timeout,
            max_lifetime: secs(lifetime_secs, MONITOR_MAX_LIFETIME)
                .min(Self::lifetime_ceiling(stall_timeout)),
        }
    }

    /// Read the policy from the config plane. An unreadable config plane means
    /// the compiled defaults, which is the behaviour every install had before
    /// these became settings.
    async fn load(store: &skadi_store::Store) -> Self {
        use skadi_store::ConfigRepo;
        let Ok(entries) = store.list_config().await else {
            return Self::default();
        };
        let get = |k: &str| {
            entries
                .iter()
                .find(|e| e.key == k)
                .and_then(|e| e.value.parse::<u64>().ok())
        };
        Self::resolve(
            get("monitor.stall_timeout_secs"),
            get("monitor.max_lifetime_secs"),
        )
    }
}

/// Backoff before a sweep re-acquires a file whose transfer failed. A flaky
/// source (a proxy/tracker that intermittently 500s then succeeds — exactly what
/// Prowlarr's download proxy does) recovers on the next sweep without hammering;
/// a genuinely-dead release is blocklisted from the UI.
const ACQUIRE_RETRY_BACKOFF_MINS: i64 = 30;

/// Backoff before a sweep re-acquires an item whose **transfer** failed, by
/// consecutive-failure count (SKADI-T-0437). The first failure is a blip and
/// retries in [`ACQUIRE_RETRY_BACKOFF_MINS`]; repeated failures on the same item
/// back off hard, because something about it is broken (a source that always
/// dies, a magnet nobody seeds) and re-grabbing every 30 minutes forever just
/// churns the client and the indexers — Sword of Destiny was grabbed three times
/// in one day on 2026-09-06. Capped rather than terminal: availability changes,
/// so the item keeps getting a chance, just a rarer one.
fn transfer_backoff(attempts: u32) -> chrono::Duration {
    match attempts {
        0 | 1 => chrono::Duration::minutes(ACQUIRE_RETRY_BACKOFF_MINS),
        2 => chrono::Duration::hours(2),
        3 => chrono::Duration::hours(6),
        4 => chrono::Duration::hours(24),
        _ => chrono::Duration::hours(72),
    }
}

/// How long an **automatic** blocklist entry lasts (SKADI-T-0436).
///
/// Auto-blocks used to be permanent, which turned every transient misjudgement
/// into durable damage: the 46 releases wrongly failed as stalled on 2026-09-06
/// (SKADI-T-0394) would have stayed unusable until someone cleared them by hand.
/// A month is long enough that a genuinely dead release is not re-grabbed in any
/// practical sweep window, and short enough that the system heals itself.
/// Operator-created blocks are unaffected — they carry whatever TTL the operator
/// chose, including none.
const AUTO_BLOCK_TTL: chrono::Duration = chrono::Duration::days(30);

/// The distinct acquirables in an import outcome, in first-seen order, each represented by
/// its first imported file (SKADI-T-0310). A pack download places files for several library
/// books, so we probe + mark each book `Imported` once, using one representative file.
fn distinct_imported(
    imported: &[skadi_importer::ImportedFile],
) -> Vec<&skadi_importer::ImportedFile> {
    let mut seen = HashSet::new();
    imported
        .iter()
        .filter(|f| seen.insert(f.acquirable.0.clone()))
        .collect()
}

/// Re-check backoff for a **not-found** failure — search returned nothing, or no
/// candidate was suitable (SKADI-T-0177). Unlike a transfer failure (fixed 30 min),
/// a not-found item is re-checked on an **escalating** schedule: soon after a fresh
/// miss, then backing off for chronically-absent titles so we don't hammer indexers
/// (content availability changes over time — new releases, seeders, indexers). The
/// `attempts` arg is the 1-based consecutive not-found count.
fn not_found_backoff(attempts: u32) -> chrono::Duration {
    let hours = match attempts {
        0 | 1 => 6,
        2 => 12,
        3 => 24,
        _ => 72, // cap at 3 days
    };
    chrono::Duration::hours(hours)
}

/// How soon to re-check after a search that *errored* (an indexer blew its
/// budget, the network blipped). Short and flat: an error says nothing about
/// whether the release exists, so it must not climb the not-found ladder.
fn search_error_retry() -> chrono::Duration {
    chrono::Duration::hours(2)
}

/// The `Failed` status for a search error (SKADI-T-0607): re-check in
/// [`search_error_retry`], attempts left where they were. On prod, one slow
/// indexer timing out had marched 151 aired episodes up to the 3-day backoff —
/// the same penalty as "nothing acceptable exists", for a transient fault.
fn failed_after_search_error(
    prior_attempts: u32,
    message: String,
    now: chrono::DateTime<Utc>,
) -> AcquisitionStatus {
    AcquisitionStatus::Failed {
        reason: FailureReason::Other(message),
        retry_at: Some(now + search_error_retry()),
        attempts: prior_attempts,
    }
}

async fn record_search_failed(state: &AcquireState, message: String) {
    let _ = services_for(state.request.kind)
        .status
        .set_status(
            &state.acquirable,
            failed_after_search_error(state.prior_not_found_attempts, message, Utc::now()),
        )
        .await;
}

/// Persist `Failed` with a **retry-after backoff** for a *transfer* failure
/// (snatch/download/import). The next sweep re-acquires the file once `retry_at`
/// elapses ([`crate::WantedQuery`] honors it), recovering a flaky source. We do
/// NOT auto-blocklist here: the release looked good (it was chosen), the transfer
/// just failed — blocklisting on the first failure would permanently park a
/// flaky-but-good release. A genuinely-dead release is blocklisted from the UI.
///
/// Idempotent: if the file is already `Failed{retry_at: Some(..)}`, the existing
/// backoff is preserved — so Cloacina's 48 `monitor` re-runs (or repeated polls)
/// can't keep pushing the deadline out or spam the status history.
async fn record_failed_retryable(state: &AcquireState, reason: FailureReason) {
    let svc = services_for(state.request.kind);
    write_retry_failed(&svc, state, reason).await;
}

/// The idempotent `Failed{retry_at}` status write shared by both retryable-failure
/// helpers ([`record_failed_retryable`] and [`record_failed_and_blocklist`]).
/// Returns without writing if the file is already `Failed{retry_at: Some(..)}` so
/// repeated failures can't keep pushing the deadline out or spam the status log.
async fn write_retry_failed(svc: &HunterServices, state: &AcquireState, reason: FailureReason) {
    write_retry_failed_with(svc, state, reason, false).await;
}

/// [`write_retry_failed`], with `immediate` skipping the backoff (SKADI-T-0529).
async fn write_retry_failed_with(
    svc: &HunterServices,
    state: &AcquireState,
    reason: FailureReason,
    immediate: bool,
) {
    // Keep an existing backoff only while it is still in the FUTURE
    // (SKADI-T-0442). Returning on any `retry_at` preserved one that had already
    // elapsed, so an item whose previous status was `Failed{retry_at: past}` —
    // a manual grab, or anything re-tried after its window — was written back
    // with that stale time and swept again immediately, which is the opposite of
    // a backoff.
    if let Ok(Some(AcquisitionStatus::Failed {
        retry_at: Some(at), ..
    })) = svc.status.get_status(&state.acquirable).await
        && at > Utc::now()
    {
        return;
    }
    // Escalating backoff on consecutive failures of this item (SKADI-T-0437). The
    // count is carried from the prior `Failed.attempts`, read at search entry
    // before the status was overwritten, so it survives the sweep→run round trip.
    let attempts = state.prior_not_found_attempts.saturating_add(1);
    let retry_at = if immediate {
        // Due now: the offending release is blocklisted, so the next sweep picks
        // a different candidate rather than repeating this one.
        Utc::now()
    } else {
        Utc::now() + transfer_backoff(attempts)
    };
    let _ = svc
        .status
        .set_status(
            &state.acquirable,
            AcquisitionStatus::Failed {
                reason,
                retry_at: Some(retry_at),
                attempts,
            },
        )
        .await;
}

/// Blocklist every release `snatch` skipped this run and trace each one
/// (SKADI-T-0589), then clear the list so a retried task does not write them
/// twice. Honours the domain's auto-blocklist policy like the other auto-blocks;
/// best-effort — a failed write is logged, never fails the workflow.
async fn blocklist_snatch_failures(svc: &HunterServices, state: &mut AcquireState) {
    if state.snatch_failures.is_empty() {
        return;
    }
    let policy = blocklist_policy(&svc.store, state.request.kind).await;
    for failure in std::mem::take(&mut state.snatch_failures) {
        let blocked = policy.auto_block && failure.blocklist;
        if blocked {
            let entry = NewBlocklistEntry {
                release_key: failure.release_key.clone(),
                title: failure.title.clone(),
                acquirable_ref: Some(state.acquirable.0.clone()),
                indexer: Some(failure.indexer.clone()),
                reason: Some(format!("SnatchFailed({:?})", failure.error)),
                expires_at: policy.ttl.map(|d| Utc::now() + d),
            };
            if let Err(e) = svc.store.block(&entry).await {
                tracing::warn!(error = %e, "auto-blocklist of unsnatchable release failed (non-fatal)");
            }
        }
        crate::trace::emit(
            state.request.kind,
            &state.acquirable.0,
            "grabbing",
            "snatch_failed",
            format!(
                "could not grab \"{}\"{}; trying the next candidate",
                failure.title,
                if blocked { ", blocklisted" } else { "" }
            ),
            Some(failure.error.clone()),
        )
        .await;
    }
}

/// Persist `Failed{retry_at}` **and** blocklist the chosen release — for a
/// *confirmed hard failure where the release itself is at fault* (SKADI-T-0188): a
/// terminal download failure (monitor's `Internal` branch — the transfer errored
/// out, not "still going") or an import failure (the downloaded file is bad).
///
/// Blocklisting by [`skadi_indexers::release_key`] is what closes the re-grab loop:
/// the *acquirable* is still re-swept after the backoff (so a genuinely-better
/// release can still be found later), but `decide` now **skips this dead release**
/// and picks the next-best candidate instead of re-grabbing the same corpse every
/// sweep. Contrast [`record_failed_retryable`], used for transient/flaky-source
/// failures (a busy downloader, a slow-but-live transfer) where the chosen release
/// looked good and must stay grabbable.
///
/// Best-effort blocklist write: a failure is logged, never fails the workflow. If
/// no release was chosen (defensive — these paths always have one), it degrades to
/// the plain retryable write.
async fn record_failed_and_blocklist(state: &AcquireState, reason: FailureReason) {
    let svc = services_for(state.request.kind);
    let policy = blocklist_policy(&svc.store, state.request.kind).await;
    let mut blocked = false;
    if let Some(chosen) = state.chosen.as_ref()
        && policy.auto_block
    {
        let entry = NewBlocklistEntry {
            release_key: skadi_indexers::release_key(chosen),
            title: chosen.title.clone(),
            acquirable_ref: Some(state.acquirable.0.clone()),
            indexer: Some(chosen.indexer.to_string()),
            reason: Some(format!("{reason:?}")),
            // Auto-blocks expire (SKADI-T-0436). They used to be permanent, so a
            // release the hunter misjudged once — the 46 wrongly failed as stalled
            // on 2026-09-06 — was unusable until a human cleared it. The TTL is
            // long enough that a genuinely dead release is not retried in any
            // practical window, and short enough that a mistake heals itself.
            // Operator-settable since SKADI-T-0529; `None` = permanent.
            expires_at: policy.ttl.map(|d| Utc::now() + d),
        };
        match svc.store.block(&entry).await {
            Ok(_) => blocked = true,
            Err(e) => {
                tracing::warn!(error = %e, "auto-blocklist of failed release failed (non-fatal)");
            }
        }
    } else if state.chosen.is_some() {
        tracing::debug!(
            kind = ?state.request.kind,
            "auto-blocklisting is disabled for this domain; leaving the release grabbable"
        );
    }
    // Trace: a confirmed hard failure that blocklisted the release, so the next
    // sweep grabs the next-best candidate (SKADI-T-0323). The stage + label come
    // from which transfer step failed.
    let (stage, event) = match &reason {
        FailureReason::ImportFailed(_) => ("importing", "import_failed"),
        _ => ("downloading", "download_failed"),
    };
    let title = state
        .chosen
        .as_ref()
        .map(|r| r.title.as_str())
        .unwrap_or("");
    crate::trace::emit(
        state.request.kind,
        &state.acquirable.0,
        stage,
        event,
        format!(
            "{} failed, blocklisted \"{title}\"",
            humanize_reason(&reason)
        ),
        Some(format!("{reason:?}")),
    )
    .await;
    // Tell the operator (SKADI-T-0504). `Failed` was defined but never emitted,
    // so a subscriber heard nothing about the one outcome they most wanted told
    // about — a success shows up in the library on its own, a failure does not.
    let _ = crate::pipeline::notify_failed(
        state,
        &format!("{} failed for \"{title}\"", humanize_reason(&reason)),
        &svc.notifiers,
    )
    .await;
    // Sonarr's "Redownload Failed" (SKADI-T-0529): when the release was
    // blocklisted, the next search cannot pick it again, so the backoff has
    // nothing left to protect against — its whole purpose is to avoid hammering
    // the same bad release. Retry immediately and let the sweep take the
    // next-best candidate.
    //
    // When the block did *not* happen (auto-block exempt, or the write failed) the
    // backoff still applies: without it the next sweep would choose the same
    // release and fail the same way, in a tight loop.
    write_retry_failed_with(&svc, state, reason, blocked).await;
}

/// The operator's automatic-blocklist policy for `kind` (SKADI-T-0529).
struct BlocklistPolicy {
    /// Auto-block on a hard failure at all.
    auto_block: bool,
    /// How long an automatic block lasts; `None` = permanent.
    ttl: Option<chrono::Duration>,
}

/// Whether the acquire path is configured to **move** completed downloads into
/// the library rather than hardlink and seed (SKADI-T-0138).
///
/// Read here rather than carried on the run state because the setting is a
/// live config value: a run that started under `seed` and finishes after the
/// operator switched to `move` should honour the setting in force when the
/// files actually land.
///
/// An unreadable config plane means `seed` — the safe direction. Reporting
/// `move` on a database blip would drop a transfer that is still seeding.
async fn move_on_import(store: &skadi_store::Store) -> bool {
    use skadi_store::ConfigRepo;
    let Ok(entries) = store.list_config().await else {
        return false;
    };
    entries
        .iter()
        .find(|e| e.key == "import.placement")
        .is_some_and(|e| e.value.trim().eq_ignore_ascii_case("move"))
}

/// `import.remove_unusable_downloads` (SKADI-T-0592), default on; an unreadable
/// config plane keeps the default.
async fn remove_unusable_downloads(store: &skadi_store::Store) -> bool {
    use skadi_store::ConfigRepo;
    let Ok(entries) = store.list_config().await else {
        return true;
    };
    // Same shape as `move_on_import`: the hunter reads the raw row rather than
    // pulling the config crate in. Anything but an explicit "off" keeps it on.
    entries
        .iter()
        .find(|e| e.key == "import.remove_unusable_downloads")
        .is_none_or(|e| {
            !matches!(
                e.value.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            )
        })
}

async fn blocklist_policy(
    store: &skadi_store::Store,
    kind: skadi_core::MediaKind,
) -> BlocklistPolicy {
    use skadi_store::ConfigRepo;
    // An unreadable config plane leaves the pre-SKADI-T-0529 behaviour, which is
    // the safe direction: a transient database blip must not silently stop
    // blocklisting bad releases.
    let Ok(entries) = store.list_config().await else {
        return BlocklistPolicy {
            auto_block: true,
            ttl: Some(AUTO_BLOCK_TTL),
        };
    };
    let get = |k: &str| entries.iter().find(|e| e.key == k).map(|e| e.value.clone());
    let hours = get("blocklist.auto_ttl_hours")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(AUTO_BLOCK_TTL.num_hours());
    let exempt = get("blocklist.auto_block_exempt_domains").unwrap_or_default();
    let kind_name = format!("{kind:?}").to_ascii_lowercase();
    let auto_block = !exempt
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .any(|s| !s.is_empty() && s == kind_name);
    BlocklistPolicy {
        auto_block,
        ttl: (hours > 0).then(|| chrono::Duration::hours(hours)),
    }
}

/// A short human label for a [`FailureReason`] variant, for trace messages.
fn humanize_reason(reason: &FailureReason) -> &'static str {
    match reason {
        FailureReason::DownloadFailed(_) => "download",
        FailureReason::ImportFailed(_) => "import",
        FailureReason::NoSuitableRelease => "no suitable release",
        FailureReason::Other(_) => "error",
    }
}

/// `search`: query indexers and collect candidates. Skips when `chosen` is
/// already set (manual grab, SKADI-T-0114). Writes `Searching` on entry and
/// `Failed` on a hard (all-indexers) failure.
pub async fn search(context: &mut Context<Value>) -> Result<()> {
    let mut state = load_state(context)?;
    let svc = services_for(state.request.kind);
    if state.chosen.is_some() {
        store_state(context, &state)?;
        return Ok(());
    }
    // Upgrade runs (SKADI-T-0182) start from an already-`Imported` status. Do NOT
    // overwrite it with `Searching` (nor `Failed` on a fruitless search below):
    // an opportunistic "look for something better" must not regress a good
    // imported file to Searching/Failed. The item stays `Imported`; the next
    // sweep's `upgradable()` re-emits it for another try.
    let is_upgrade = state.current_quality.is_some();
    // Carry the consecutive-failure count from the prior status (read BEFORE we
    // overwrite it with `Searching`) so a re-check escalates its backoff on failure
    // — not-found (SKADI-T-0177) and transfer failures (SKADI-T-0437) share the
    // carrier. A fresh/Missing item starts at 0. Read for EVERY run, not
    // just first acquisitions: an upgrade whose transfer keeps failing must back
    // off too. Read before the status is overwritten below.
    state.prior_not_found_attempts = match svc.status.get_status(&state.acquirable).await {
        Ok(Some(AcquisitionStatus::Failed { attempts, .. })) => attempts,
        _ => 0,
    };
    if !is_upgrade {
        let _ = svc
            .status
            .set_status(
                &state.acquirable,
                AcquisitionStatus::Searching {
                    since: Utc::now(),
                    attempts: state.prior_not_found_attempts.saturating_add(1),
                },
            )
            .await;
    }
    // Load the item's tags so the fan-out can scope indexers to it
    // (SKADI-T-0556). Done here rather than in each domain's seed builder for
    // two reasons: the builders are pure sync functions and this is a database
    // read, and there are six of them — one place cannot drift from another.
    //
    // A failed read means *no* tags, which is the safe direction: untagged
    // indexers still serve the item, so a database blip degrades scoping rather
    // than emptying the fan-out.
    state.request.tags = {
        use skadi_store::ItemTagRepo;
        let kind = match state.request.kind {
            skadi_core::MediaKind::Movie => "movie",
            skadi_core::MediaKind::Series => "series",
            skadi_core::MediaKind::Audiobook => "audiobook",
            _ => "",
        };
        Some(
            svc.store
                .tags_for(kind, &state.acquirable.0)
                .await
                .unwrap_or_default(),
        )
    };
    let result = pipeline::search(&mut state, &svc.indexers).await;
    if let Err(ref e) = result
        && !is_upgrade
    {
        record_search_failed(&state, format!("search: {e}")).await;
    }
    // Always write context — even on error, partial candidates aid diagnostics.
    store_state(context, &state)?;
    result
}

/// `decide`: score candidates and pick the best allowed release into `chosen`.
/// Skips when `chosen` is already set. On "no suitable release" writes
/// `Failed{NoSuitableRelease}` and returns the error.
/// `(priority, minimum_seeders)` per indexer, from the live indexer set
/// (SKADI-T-0539). Built per decide rather than cached: the set is rebuilt
/// whenever indexer settings change, and it is a handful of entries.
pub fn indexer_flags(
    indexers: &[std::sync::Arc<dyn skadi_indexers::Indexer>],
) -> Vec<(skadi_core::IndexerId, u32, u32)> {
    indexers
        .iter()
        .map(|ix| (ix.id(), ix.priority(), ix.minimum_seeders()))
        .collect()
}

pub async fn decide(context: &mut Context<Value>) -> Result<()> {
    let mut state = load_state(context)?;
    let svc = services_for(state.request.kind);
    if state.chosen.is_some() {
        store_state(context, &state)?;
        return Ok(());
    }
    // Exclude blocklisted releases (SKADI-T-0115). Best-effort: a read failure
    // falls back to an empty set rather than stopping decide.
    let blocklisted = svc.store.blocked_keys().await.unwrap_or_default();
    // Read before the `&mut state` borrow below (SKADI-T-0182/0186): when this run
    // is an upgrade, `decide` only picks a strictly-better release than the held
    // one — on the quality axis (current_quality) and the format-score axis
    // (current_format_score).
    let current_quality = state.current_quality;
    let current_format_score = state.current_format_score;
    let current_unplayable = state.current_unplayable;
    // Score against the profile THIS item was assigned, not the domain-wide one
    // (SKADI-T-0531). The domain default is only a fallback for an item whose
    // profile row is missing or invalid.
    let profile =
        crate::services::resolve_profile_by_id(&svc.store, state.profile, &svc.scoring.definitions)
            .await
            .unwrap_or_else(|| svc.scoring.profile.clone());
    let result = pipeline::decide(
        &mut state,
        &pipeline::Scoring {
            definitions: &svc.scoring.definitions,
            profile: &profile,
            formats: &svc.scoring.formats,
            min_seeders: svc.scoring.min_seeders,
            indexer_flags: &indexer_flags(&svc.indexers),
            blocklisted: &blocklisted,
            audiobook: svc.scoring.audiobook.as_ref(),
            current_quality,
            current_format_score,
            current_unplayable,
        },
    );
    match result {
        Ok(()) => {
            store_state(context, &state)?;
            Ok(())
        }
        Err(e) => {
            // Upgrade run (SKADI-T-0182): "no strictly-better release" is the
            // normal, common outcome — leave the held file `Imported` untouched
            // (don't regress it to `Failed`). The next sweep's `upgradable()`
            // re-emits it for another opportunistic try.
            if state.current_quality.is_none() {
                // First acquisition: no suitable release now — re-check later on
                // an escalating backoff rather than parking forever (SKADI-T-0177).
                let attempts = state.prior_not_found_attempts.saturating_add(1);
                let retry_at = Utc::now() + not_found_backoff(attempts);
                let _ = svc
                    .status
                    .set_status(
                        &state.acquirable,
                        AcquisitionStatus::Failed {
                            reason: FailureReason::NoSuitableRelease,
                            retry_at: Some(retry_at),
                            attempts,
                        },
                    )
                    .await;
            }
            store_state(context, &state)?;
            Err(e)
        }
    }
}

/// Persist the chosen release's decision explanation at the grab boundary
/// (SKADI-T-0187). Re-runs [`pipeline::explain`] on the chosen release with this
/// run's live scoring (incl. the held quality/format-score so an upgrade reads as
/// `Upgrade`) and appends it to the `decision_history`. Best-effort: a recording
/// failure is logged and never fails the acquire.
async fn record_decision_history(state: &AcquireState, svc: &HunterServices) {
    let Some(chosen) = state.chosen.as_ref() else {
        return;
    };
    // The chosen release already passed the blocklist; an empty set keeps explain
    // focused on the quality/format reasoning for the record.
    let no_block = HashSet::new();
    // Same profile decide used (SKADI-T-0531): the item's own, falling back to
    // the domain default. Recording the explanation against a different profile
    // than the one that made the decision would make the history lie.
    let profile =
        crate::services::resolve_profile_by_id(&svc.store, state.profile, &svc.scoring.definitions)
            .await
            .unwrap_or_else(|| svc.scoring.profile.clone());
    let scoring = pipeline::Scoring {
        definitions: &svc.scoring.definitions,
        profile: &profile,
        formats: &svc.scoring.formats,
        min_seeders: svc.scoring.min_seeders,
        indexer_flags: &indexer_flags(&svc.indexers),
        blocklisted: &no_block,
        audiobook: svc.scoring.audiobook.as_ref(),
        current_quality: state.current_quality,
        current_format_score: state.current_format_score,
        // Must mirror `decide`'s inputs exactly: this builds the *explanation*
        // shown on /activity, and a hardcoded `false` here would tell the
        // operator a release was grabbed on the quality axis when it was really
        // grabbed because the held file does not play (SKADI-T-0584).
        current_unplayable: state.current_unplayable,
    };
    let exp = pipeline::explain(chosen, &scoring);
    // Surface the classified decision on the live /activity view (SKADI-T-0190).
    crate::tracker::tracker().set_decision(&state.acquirable.0, exp.decision.clone());
    // The same mapping trace rows use (SKADI-T-0433). This used to send
    // everything that was not an audiobook to "movie", so TV decisions were filed
    // under the movies domain — which is why the 2026-09-06 investigation found
    // no TV rows in `decision_history` at all.
    let kind = crate::trace::kind_str(state.request.kind);
    let entry = DecisionEntry {
        id: uuid::Uuid::new_v4().to_string(),
        at: Utc::now(),
        kind: kind.to_string(),
        acquirable_ref: state.acquirable.0.clone(),
        title: chosen.title.clone(),
        quality: exp.quality.clone(),
        decision: exp.decision.clone(),
        format_score: exp.format_score,
        explanation: serde_json::to_string(&exp).unwrap_or_default(),
        // Stable identity of the grabbed release, for grab→import correlation
        // and blocklist-and-search from a History row (SKADI-T-0196).
        release_key: Some(skadi_indexers::release_key(chosen)),
    };
    if let Err(e) = svc.store.record_decision(&entry).await {
        tracing::warn!(error = %e, "recording decision history failed (non-fatal)");
    }
}

/// Mark the run as entering a Cloacina-driven `stage`, **adopting** it into the
/// in-flight tracker if nothing owns it — a workflow Cloacina recovery replayed
/// after a restart (SKADI-T-0388). Without this the sweep saw no run for the
/// acquirable and started a duplicate over a transfer that was still running.
///
/// Returns who the tracked run belongs to. `Foreign` means *another* workflow
/// for the same acquirable is already in flight — two workflows queued behind a
/// blocked scheduler both get replayed after a restart, and the second must not
/// snatch the release a second time.
fn enter_stage(state: &AcquireState, stage: &str) -> Ownership {
    let ownership = crate::tracker::tracker().adopt(
        state.request.kind,
        &state.acquirable.0,
        stage,
        state.run_id.as_deref(),
    );
    match ownership {
        Ownership::Adopted => {
            tracing::info!(acquirable = %state.acquirable.0, stage, "adopted recovered acquire run");
        }
        Ownership::Foreign => {
            tracing::warn!(
                acquirable = %state.acquirable.0,
                stage,
                run_id = state.run_id.as_deref().unwrap_or("-"),
                "another run for this acquirable is already in flight"
            );
        }
        Ownership::Mine => {}
    }
    ownership
}

/// Release an adopted tracker entry at a terminal point of the workflow. Owned
/// entries are untouched (their `start_acquire` future finishes them), as is a
/// sibling run's entry (this run's id doesn't match it).
fn leave_run(state: &AcquireState) {
    crate::tracker::tracker().finish_adopted(&state.acquirable.0, state.run_id.as_deref());
}

/// Whether a **first-acquire** run (not an upgrade) finds its acquirable already
/// `Imported` — another run got there first, typically the original run after a
/// restart replayed this one from an older checkpoint. Such a run must not
/// write `Snatched`/`Downloading` over the import (the "demoted import" rows of
/// SKADI-T-0388) nor keep a redundant transfer alive. Upgrade runs and manual
/// grabs legitimately operate on an `Imported` row and are exempt.
async fn already_imported_elsewhere(svc: &HunterServices, state: &AcquireState) -> bool {
    if state.current_quality.is_some() || state.manual {
        return false;
    }
    matches!(
        svc.status.get_status(&state.acquirable).await,
        Ok(Some(AcquisitionStatus::Imported { .. }))
    )
}

/// Why a run is being ended in favour of another one — see [`end_superseded`].
#[derive(Clone, Copy)]
enum Superseded {
    /// The acquirable is already `Imported` ([`already_imported_elsewhere`]).
    AlreadyImported,
    /// Another workflow for the same acquirable is in flight
    /// ([`Ownership::Foreign`] at `snatch`).
    RunInFlight,
}

impl Superseded {
    fn detail(self) -> &'static str {
        match self {
            Self::AlreadyImported => {
                "already imported by another run; this run ended without changing status"
            }
            Self::RunInFlight => {
                "another run is already acquiring this; this duplicate ended without grabbing"
            }
        }
    }
}

/// End a run that another run has made redundant: cancel any transfer it owns,
/// flag it terminal so the remaining tasks no-op, and leave the row's status
/// alone (SKADI-T-0388).
async fn end_superseded(
    context: &mut Context<Value>,
    state: &mut AcquireState,
    step: &str,
    why: Superseded,
) -> Result<()> {
    tracing::warn!(
        acquirable = %state.acquirable.0,
        step,
        why = why.detail(),
        "ending superseded run without touching status (SKADI-T-0388)"
    );
    let svc = services_for(state.request.kind);
    pipeline::cancel_transfer(state, &svc.downloaders).await;
    crate::trace::emit(
        state.request.kind,
        &state.acquirable.0,
        step,
        "superseded",
        why.detail().to_string(),
        None,
    )
    .await;
    state.terminal_failure = true;
    store_state(context, state)?;
    leave_run(state);
    Ok(())
}

/// `snatch`: hand the chosen release to a matching downloader. Fires `Grabbed`
/// exactly once (on the call that first sets the handle, SKADI-T-0037) and
/// writes `Snatched`. On failure writes `Failed{retry_at}` **without** blocklisting
/// (a busy downloader is transient — the release stays grabbable; SKADI-T-0188).
pub async fn snatch(context: &mut Context<Value>) -> Result<()> {
    let mut state = load_state(context)?;
    let svc = services_for(state.request.kind);
    // A second replayed workflow for the same acquirable ends here, before any
    // transfer exists: nothing to cancel, nothing grabbed twice. A run that
    // already holds a handle is past that point and just carries on.
    if enter_stage(&state, "snatching") == Ownership::Foreign && state.handle.is_none() {
        end_superseded(context, &mut state, "snatch", Superseded::RunInFlight).await?;
        return Ok(());
    }
    if already_imported_elsewhere(&svc, &state).await {
        end_superseded(context, &mut state, "snatch", Superseded::AlreadyImported).await?;
        return Ok(());
    }
    // The same release is already being fetched for a different acquirable
    // (SKADI-T-0402): two runs, one download. The tracker's dedupe is per
    // acquirable, so nothing stopped House of the Dragon S03 being sent to the
    // client twice within a minute. Skip rather than add a duplicate transfer —
    // the other run's import fans the files out to every acquirable the download
    // satisfies, and if it fails this item is swept again.
    if state.handle.is_none()
        && let Some(chosen) = state.chosen.as_ref()
        && !crate::tracker::tracker().claim_release(&state.acquirable.0, &chosen.title)
    {
        tracing::info!(
            acquirable = %state.acquirable.0,
            release = %chosen.title,
            "release already in flight for another item; not grabbing it twice"
        );
        end_superseded(context, &mut state, "snatch", Superseded::RunInFlight).await?;
        return Ok(());
    }
    // Idempotent (skips if a handle is already recorded); track whether THIS
    // call set it so Grabbed fires exactly once, not on retry.
    let had_handle = state.handle.is_some();
    let result = pipeline::snatch(&mut state, &svc.downloaders, &svc.indexers).await;
    // Whatever the outcome, the candidates snatch skipped are dead links for
    // this item: blocklist them so the next sweep does not walk them again
    // (SKADI-T-0589).
    blocklist_snatch_failures(&svc, &mut state).await;
    match &result {
        Ok(()) => {
            if !had_handle && state.handle.is_some() {
                // The grab may have fallen through to a later candidate
                // (SKADI-T-0589): show the release actually handed over.
                crate::tracker::tracker().set_chosen(
                    &state.acquirable.0,
                    state.chosen.as_ref().map(|r| r.title.clone()),
                    state.candidates.len(),
                );
                let _ = pipeline::notify_grabbed(&state, &svc.notifiers).await;
                // Persist *why* this release was grabbed (SKADI-T-0187): the
                // chosen release's full explanation, for the historical "why"
                // view. Best-effort — never fail the grab on a history write.
                record_decision_history(&state, &svc).await;
            }
            if let (Some(release), Some(handle)) = (&state.chosen, &state.handle) {
                let downloader_id = svc
                    .downloaders
                    .iter()
                    .find(|d| d.protocol() == pipeline::release_protocol(&release.fetch))
                    .map(|d| d.id());
                if let Some(downloader) = downloader_id {
                    let _ = svc
                        .status
                        .set_status(
                            &state.acquirable,
                            AcquisitionStatus::Snatched {
                                release: skadi_core::ReleaseId::new(),
                                downloader,
                                at: Utc::now(),
                            },
                        )
                        .await;
                    // Trace: handed off to the downloader (SKADI-T-0323).
                    crate::trace::emit(
                        state.request.kind,
                        &state.acquirable.0,
                        "grabbing",
                        "snatched",
                        format!("handed \"{}\" to {downloader}", release.title),
                        None,
                    )
                    .await;
                    // Tell the download row what it is for (SKADI-T-0689): its own
                    // `acquirable_ref` is the release title, so without this the
                    // Downloads page cannot offer a manual import of it.
                    record_download_target(&svc, &state, &handle.native_id).await;
                }
            }
        }
        Err(e) => {
            record_failed_retryable(
                &state,
                FailureReason::DownloadFailed(format!("snatch: {e}")),
            )
            .await;
            // The workflow fails here (no retries on snatch): notify won't run.
            leave_run(&state);
        }
    }
    store_state(context, &state)?;
    result
}

/// Writes `Downloading{progress}` through the status sink as `monitor` polls,
/// throttled to ≥1% changes so the 30 s cadence doesn't spam the DB
/// (SKADI-T-0038) — but flushed at least every [`LIVE_HEARTBEAT`] regardless, so
/// a slow or stalled-but-live transfer keeps its row's `updated_at` fresh and
/// `reconcile_stale` never mistakes it for a wedged run and starts a duplicate
/// acquire over it (SKADI-T-0388). The throttle state lives in the tracker's
/// run entry because each poll is its own Cloacina execution.
struct LiveProgress {
    kind: skadi_core::MediaKind,
    acquirable: skadi_importer::AcquirableRef,
    release: skadi_core::ReleaseId,
    /// The progress this execution's poll reported, for the stall verdict.
    seen: std::sync::Mutex<Option<f32>>,
}

/// Longest a live transfer goes without a status heartbeat — well inside
/// `STALE_ACQUIRE_GRACE` (15 min).
const LIVE_HEARTBEAT: chrono::Duration = chrono::Duration::minutes(5);

#[async_trait::async_trait]
impl pipeline::ProgressSink for LiveProgress {
    async fn report(&self, progress: f32) {
        *self.seen.lock().expect("progress lock poisoned") = Some(progress);
        // Every poll is a sign of life for the run and feeds its transfer watch;
        // an untracked run (tests) always flushes.
        let flush = crate::tracker::tracker()
            .observe_progress(&self.acquirable.0, progress, LIVE_HEARTBEAT)
            .is_none_or(|o| o.flush);
        if !flush {
            return;
        }
        let _ = services_for(self.kind)
            .status
            .set_status(
                &self.acquirable,
                AcquisitionStatus::Downloading {
                    release: self.release,
                    progress,
                },
            )
            .await;
    }
}

/// `monitor`: poll the downloader **once**, reporting live progress; while the
/// transfer is still running, return a retryable error so Cloacina re-runs this
/// step after the task's retry delay (see [`MONITOR_MAX_POLLS`]). On a hard
/// download failure — or a transfer the watch has given up on — writes `Failed`
/// + blocklists the release and ends the run.
pub async fn monitor(context: &mut Context<Value>) -> Result<()> {
    let mut state = load_state(context)?;
    let svc = services_for(state.request.kind);
    // Snatch may already have ended the run as superseded (it returns Ok so the
    // DAG completes); don't start watching a transfer that was cancelled.
    if state.terminal_failure {
        return Ok(());
    }
    enter_stage(&state, "downloading");
    if already_imported_elsewhere(&svc, &state).await {
        end_superseded(context, &mut state, "monitor", Superseded::AlreadyImported).await?;
        return Ok(());
    }
    let live = LiveProgress {
        kind: state.request.kind,
        acquirable: state.acquirable.clone(),
        release: skadi_core::ReleaseId::new(),
        seen: std::sync::Mutex::new(None),
    };
    let mut result = pipeline::monitor(
        &mut state,
        &svc.downloaders,
        Duration::from_secs(MONITOR_POLL_INTERVAL_SECS),
        MONITOR_MAX_POLLS,
        Some(&live),
    )
    .await;
    // Stall watch (SKADI-T-0388): a still-running transfer whose best progress
    // hasn't moved for MONITOR_STALL_TIMEOUT, or that has been under watch
    // longer than MONITOR_MAX_LIFETIME, is a dead download — terminal below —
    // instead of being re-polled for the rest of the retry budget (live: 16
    // such zombies starved every snatch for days).
    //
    // Whether the client has the transfer LIVE is the sink's report: it fires
    // only for a `Downloading` poll, so `seen == None` means the client answered
    // `Queued` — hash-check queue or metadata limbo. Such a poll no longer folds
    // in as "0% progress" (it did, and permanently blocklisted 46 releases on
    // 2026-09-06); it feeds the watch as not-live, leaving only the lifetime cap
    // to end a genuine zombie (SKADI-T-0394).
    if matches!(result, Err(AppError::Network(_))) {
        let reported = live.seen.lock().map(|s| *s).unwrap_or(None);
        let watch = crate::tracker::tracker()
            .observe_progress_live(
                &state.acquirable.0,
                reported.unwrap_or(0.0),
                LIVE_HEARTBEAT,
                reported.is_some(),
            )
            .map(|o| o.watch);
        let policy = StallPolicy::load(&svc.store).await;
        if let Some(why) = watch.and_then(|w| stall_verdict(&w, chrono::Utc::now(), policy)) {
            tracing::warn!(acquirable = %state.acquirable.0, %why, "giving up on transfer (SKADI-T-0388)");
            pipeline::cancel_transfer(&state, &svc.downloaders).await;
            result = Err(AppError::Internal(why));
        }
    }
    if let Err(ref e) = result {
        // A retryable Network ("still downloading") error is returned so
        // Cloacina re-runs monitor after the retry delay to keep watching.
        // A hard download failure (Internal) is TERMINAL: record `Failed` with a
        // retry-after backoff, flag the run terminal, and return `Ok` — otherwise
        // Cloacina would burn the whole retry budget re-polling a dead download,
        // each re-writing the failure (the activity-log storm). The next sweep
        // re-acquires after the backoff (flaky-source recovery).
        if let AppError::Internal(_) = e {
            // Terminal download failure: the release is at fault — blocklist it so
            // the next sweep grabs the next-best candidate, not this dead one
            // (SKADI-T-0188).
            record_failed_and_blocklist(&state, FailureReason::DownloadFailed(format!("{e}")))
                .await;
            state.terminal_failure = true;
            store_state(context, &state)?;
            leave_run(&state);
            return Ok(());
        }
    }
    store_state(context, &state)?;
    result
}

/// Say whether to give up on a still-running transfer given what the watch
/// has seen of it (SKADI-T-0388): its best progress flat for
/// [`MONITOR_STALL_TIMEOUT`], or under watch longer than
/// [`MONITOR_MAX_LIFETIME`]. Returns the human-readable reason, or `None` to
/// let Cloacina retry `monitor` as before.
fn stall_verdict(
    watch: &TransferWatch,
    now: chrono::DateTime<chrono::Utc>,
    policy: StallPolicy,
) -> Option<String> {
    let pct = (watch.best_progress * 100.0).round();
    // The stall clock only runs while the client has the transfer LIVE
    // (SKADI-T-0394). A transfer queued behind the client's hash checks, or a
    // magnet still fetching metadata, reports 0% for as long as it waits — that
    // is not a dead download, and treating it as one permanently blocklisted 46
    // releases on 2026-09-06. Such a transfer is bounded by the lifetime cap
    // below, which still ends a genuine zombie.
    if watch.live_since.is_some() {
        let flat_for = now - watch.progress_at;
        if flat_for >= policy.stall_timeout {
            return Some(format!(
                "download stalled at {pct}% for {}h",
                flat_for.num_hours()
            ));
        }
    }
    let watched_for = now - watch.since;
    if watched_for >= policy.max_lifetime {
        return Some(format!(
            "download still at {pct}% after {}h under watch",
            watched_for.num_hours()
        ));
    }
    None
}

/// Write the grab target on the download row behind `native_id` (SKADI-T-0689).
/// Best-effort: a row the store does not hold (a test fake's handle) is a no-op,
/// and a write error is logged, never failing the grab.
async fn record_download_target(svc: &HunterServices, state: &AcquireState, native_id: &str) {
    use skadi_store::DownloadJobRepo;
    let kind = crate::trace::kind_str(state.request.kind);
    if let Err(e) = svc
        .store
        .set_download_target(native_id, kind, &state.acquirable.0)
        .await
    {
        tracing::warn!(error = %e, "recording the download target failed (non-fatal)");
    }
}

/// Write the import outcome on the run's download row (SKADI-T-0689): `None` =
/// imported, `Some(reason)` = failed. Best-effort, like [`record_download_target`].
async fn record_import_outcome(svc: &HunterServices, state: &AcquireState, failure: Option<&str>) {
    use skadi_store::{DownloadJobRepo, ImportState};
    let Some(handle) = state.handle.as_ref() else {
        return;
    };
    let import_state = if failure.is_some() {
        ImportState::Failed
    } else {
        ImportState::Imported
    };
    if let Err(e) = svc
        .store
        .set_download_import(&handle.native_id, import_state, failure)
        .await
    {
        tracing::warn!(error = %e, "recording the import outcome failed (non-fatal)");
    }
}

/// `import`: hand the completed transfer to the importer (resolving a per-run
/// importer via the `ImporterFactory` if a domain installed one) and write
/// `Imported`. On failure writes `Failed` + blocklists the release.
pub async fn import(context: &mut Context<Value>) -> Result<()> {
    let mut state = load_state(context)?;
    // The run already terminally failed in `monitor` (which returned `Ok` to stop
    // Cloacina's retries); the file is `Failed{retry_at}`. Don't try to import a
    // dead download — no-op so the workflow completes cleanly.
    if state.terminal_failure {
        store_state(context, &state)?;
        return Ok(());
    }
    enter_stage(&state, "importing");
    let svc = services_for(state.request.kind);
    // Resolve a per-run Importer if a domain installed an ImporterFactory
    // (movies does — its MovieMatcher needs the per-run snapshot). Else fall
    // back to the static services.importer.
    let per_run_importer = if let Some(factory) = svc.importer_factory.as_ref() {
        match factory.for_acquirable(&state.acquirable).await {
            Ok(i) => Some(i),
            Err(e) => {
                record_import_outcome(&svc, &state, Some(&format!("importer factory: {e}"))).await;
                record_failed_retryable(
                    &state,
                    FailureReason::ImportFailed(format!("importer factory: {e}")),
                )
                .await;
                store_state(context, &state)?;
                return Err(e);
            }
        }
    } else {
        None
    };
    let importer: &dyn skadi_importer::Importer = per_run_importer
        .as_ref()
        .map(|a| a.as_ref())
        .unwrap_or_else(|| svc.importer.as_ref());
    let result = pipeline::import(&mut state, importer).await;
    record_import_outcome(
        &svc,
        &state,
        result.as_ref().err().map(|e| e.to_string()).as_deref(),
    )
    .await;
    match &result {
        Ok(()) => {
            if let Some(outcome) = &state.outcome {
                // The grabbed release's aggregate custom-format score (SKADI-T-0186) is
                // **release-level** — the same for every book a pack satisfies — so compute it
                // once. Persisted so the next upgrade sweep can compare on the format-score axis
                // (a proper/repack of the same quality scores higher). Audiobooks re-parse the
                // title for the audiobook fields.
                let format_score = state.chosen.as_ref().map_or(0, |r| {
                    let meta = skadi_quality::ReleaseMeta {
                        size_bytes: Some(r.size),
                        indexer_flags: &[],
                    };
                    if svc.scoring.audiobook.is_some() {
                        let parsed = skadi_quality::parse_audiobook(&r.title);
                        skadi_quality::aggregate_score(
                            &svc.scoring.profile.formats,
                            &svc.scoring.formats,
                            &r.title,
                            &parsed,
                            &meta,
                        )
                    } else {
                        skadi_quality::aggregate_score(
                            &svc.scoring.profile.formats,
                            &svc.scoring.formats,
                            &r.title,
                            &r.parsed,
                            &meta,
                        )
                    }
                });

                // Fan out across **every distinct imported book** (SKADI-T-0310): a pack
                // download satisfies several library books, so probe + re-score + persist for
                // each — not just the run's primary acquirable, which used to leave the rest
                // `Wanted` (and re-acquired). One representative file per book.
                for imported in distinct_imported(&outcome.imported) {
                    // Probe the placed file FIRST (SKADI-T-0237/0241): its real streams are
                    // ground truth — used both to make the stored quality honest and to
                    // persist media-info. Best-effort + strictly **non-fatal**.
                    let probe_path = imported.file.path.clone();
                    let media_info = match tokio::task::spawn_blocking(move || {
                        skadi_media_probe::DefaultProber.probe(&probe_path)
                    })
                    .await
                    {
                        Ok(info) => info,
                        Err(e) => {
                            tracing::warn!(acquirable = %imported.acquirable.0, "media probe task panicked: {e}");
                            None
                        }
                    };

                    // Re-score against the probe (SKADI-T-0241): override the title-parsed
                    // resolution/bitrate with the file's real values, so a release titled
                    // `2160p` whose file is really 1080p is stored as 1080p and can't fake a
                    // cutoff (the next sweep keeps looking for a genuine 4K).
                    let probed_resolution = media_info
                        .as_ref()
                        .and_then(|m| m.video.as_ref())
                        .map(|v| v.resolution_tier());
                    let probed_bitrate = media_info
                        .as_ref()
                        .and_then(|m| m.audio.as_ref())
                        .and_then(|a| a.bitrate_kbps);
                    let reconciled = state.chosen.as_ref().map(|r| {
                        skadi_quality::reconcile_with_probe(
                            &r.parsed,
                            probed_resolution,
                            probed_bitrate,
                        )
                    });

                    let quality_id = if let Some(ab) = svc.scoring.audiobook.as_ref() {
                        // Audiobooks: the imported file's container is ground truth
                        // (SKADI-T-0148). Reconcile the release-title-parsed quality
                        // against the actual file's format so an MP3 release advertised
                        // as M4B doesn't get stored as M4B (which used to happen because
                        // the movie `to_quality` returned None → profile cutoff).
                        let parsed = reconciled.as_ref().and_then(|p| {
                            skadi_quality::audiobook::to_audiobook_quality(p, &ab.definitions)
                        });
                        let file_fmt = imported
                            .file
                            .path
                            .extension()
                            .and_then(|e| e.to_str())
                            .and_then(skadi_quality::AudioFormat::from_token);
                        match file_fmt {
                            Some(fmt) => skadi_quality::reconcile_quality_with_format(
                                parsed,
                                fmt,
                                &ab.definitions,
                            )
                            .or(parsed)
                            .unwrap_or_else(skadi_quality::audiobook::unknown_audiobook_id),
                            None => parsed
                                .unwrap_or_else(skadi_quality::audiobook::unknown_audiobook_id),
                        }
                    } else {
                        // Nothing matched a definition (unparseable title, no probe, or an
                        // SD tier the ladder doesn't carry): store UNKNOWN rather than
                        // substituting the profile cutoff. The cutoff fabricated a
                        // "satisfied" item that no upgrade would ever replace, the mirror
                        // image of the adoption SDTV floor (SKADI-T-0412 / SKADI-T-0399).
                        // The upgrade sweeps skip Unknown instead of ranking it lowest.
                        reconciled
                            .as_ref()
                            .and_then(|p| skadi_quality::to_quality(p, &svc.scoring.definitions))
                            .map(|q| q.id)
                            .unwrap_or(skadi_quality::UNKNOWN_QUALITY_ID)
                    };

                    let _ = svc
                        .status
                        .set_status(
                            &imported.acquirable,
                            AcquisitionStatus::Imported {
                                file: FileRef {
                                    path: imported.file.path.clone(),
                                },
                                quality: quality_id,
                                score: format_score,
                                at: Utc::now(),
                            },
                        )
                        .await;

                    // Persist the probed media-info (SKADI-T-0237) alongside the now-honest
                    // quality. Non-fatal.
                    if let Some(info) = media_info {
                        let _ = svc.status.set_media_info(&imported.acquirable, info).await;
                    }

                    // Trace: a file landed in the library (SKADI-T-0323). Keyed by the
                    // imported book's own ref so a pack fans out one trace per book.
                    let file_name = imported
                        .file
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    crate::trace::emit(
                        state.request.kind,
                        &imported.acquirable.0,
                        "importing",
                        "imported",
                        format!("imported {file_name}"),
                        None,
                    )
                    .await;
                }
            }

            // Move-mode import: the importer has relocated the files, so the
            // client is holding an entry for data that is no longer where it
            // thinks — it would fail its next re-check and sit there as an error
            // an operator has to clear by hand (SKADI-T-0545).
            //
            // Deliberately **after** every status write above. If the daemon dies
            // between the two, an un-removed transfer is clutter; a removed
            // transfer with no recorded import is a lost acquisition.
            if move_on_import(&svc.store).await {
                pipeline::drop_transfer_after_move(&state, &svc.downloaders).await;
            }
        }
        Err(e) => {
            // Import failure: the downloaded file is bad — blocklist the release so
            // a re-sweep picks a different candidate (SKADI-T-0188). (The
            // importer-*factory* error above is our own infra, not the release, so
            // it stays on the plain retryable path.)
            record_failed_and_blocklist(&state, FailureReason::ImportFailed(format!("{e}"))).await;
            // Nothing in the download was media at all (a fake `.exe` release, a
            // pack of screencaps): drop it, data included, instead of leaving it
            // seeding junk (SKADI-T-0592). Only when every file was rejected
            // outright — quarantined or placement-failed data is kept.
            if state
                .outcome
                .as_ref()
                .is_some_and(pipeline::import_was_unusable)
                && remove_unusable_downloads(&svc.store).await
            {
                pipeline::drop_unusable_download(&state, &svc.downloaders).await;
                crate::trace::emit(
                    state.request.kind,
                    &state.acquirable.0,
                    "importing",
                    "download_removed",
                    format!(
                        "removed \"{}\" — nothing in it was usable media",
                        state
                            .chosen
                            .as_ref()
                            .map(|r| r.title.as_str())
                            .unwrap_or("?")
                    ),
                    None,
                )
                .await;
            }
            // The workflow fails here (no retries on import): notify won't run.
            leave_run(&state);
        }
    }
    store_state(context, &state)?;
    result
}

/// `notify`: emit an `Imported` event to every notifier that wants it. Always
/// `Ok` — `pipeline::notify` swallows individual notifier errors.
pub async fn notify(context: &mut Context<Value>) -> Result<()> {
    let state = load_state(context)?;
    // A terminally-failed run reaches here only because `monitor` returned `Ok`
    // to stop Cloacina's retries; there's nothing imported to announce.
    if state.terminal_failure {
        leave_run(&state);
        return Ok(());
    }
    enter_stage(&state, "notifying");
    let svc = services_for(state.request.kind);
    let result = pipeline::notify(&state, &svc.notifiers).await;
    // Last task of the DAG: a recovered run has no one else to finish it.
    leave_run(&state);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SKADI-T-0529: the policy read from the config plane.
    #[test]
    fn stall_policy_defaults_ignores_zero_and_clamps_the_lifetime() {
        // Nothing configured ⇒ exactly the compiled behaviour every install had
        // before these became settings (SKADI-T-0544).
        assert_eq!(StallPolicy::resolve(None, None), StallPolicy::default());

        // `0` is not "give up immediately" — a zero lifetime would fail every
        // transfer the moment it came under watch. It means "unset".
        assert_eq!(
            StallPolicy::resolve(Some(0), Some(0)),
            StallPolicy::default()
        );

        // Real values are honoured.
        let p = StallPolicy::resolve(Some(600), Some(7200));
        assert_eq!(p.stall_timeout, chrono::Duration::seconds(600));
        assert_eq!(p.max_lifetime, chrono::Duration::seconds(7200));

        // A lifetime past the retry budget is silently reduced, not honoured.
        // Left alone it would let the budget run out *before* the cap fired, so
        // a zombie would end as a bare task failure instead of the cap's clean
        // blocklist + `Failed{retry_at}` — a worse outcome reached by typing a
        // number that looks harmless.
        let huge = StallPolicy::resolve(None, Some(60 * 60 * 24 * 30));
        let budget = chrono::Duration::seconds(
            i64::from(MONITOR_RETRY_ATTEMPTS) * MONITOR_RETRY_DELAY_SECS as i64,
        );
        assert!(
            huge.max_lifetime < budget,
            "a configured lifetime must stay inside the retry budget"
        );
        // The same invariant the constants are asserted against, now holding for
        // an operator-supplied value too.
        assert!(budget > huge.max_lifetime + huge.stall_timeout / 2);
    }

    #[tokio::test]
    async fn move_on_import_defaults_to_seed_and_only_move_flips_it() {
        use skadi_store::{ConfigRepo, ConfigSource, Store};

        let path = skadi_core::unique_temp_path("placement").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();

        // Nothing configured ⇒ seed. An install that never touches the key keeps
        // hardlink-and-seed and never has a transfer dropped out from under it.
        assert!(!move_on_import(&store).await);

        // Anything that is not an explicit `move` is seed. A typo must not start
        // dropping transfers, for the same reason it must not start deleting
        // sources (SKADI-T-0138).
        for wrong in ["seed", "hardlnk", "", "mov"] {
            store
                .set_config("import.placement", wrong, ConfigSource::Runtime)
                .await
                .unwrap();
            assert!(!move_on_import(&store).await, "{wrong:?} must mean seed");
        }

        // Case and surrounding whitespace should not decide whether a transfer
        // survives.
        for right in ["move", "MOVE", " move "] {
            store
                .set_config("import.placement", right, ConfigSource::Runtime)
                .await
                .unwrap();
            assert!(move_on_import(&store).await, "{right:?} must mean move");
        }
    }

    #[tokio::test]
    async fn blocklist_policy_reads_ttl_and_domain_exemptions() {
        use skadi_store::{ConfigRepo, ConfigSource, Store};

        let path = skadi_core::unique_temp_path("blpolicy").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();

        // Nothing configured ⇒ the pre-SKADI-T-0529 behaviour: auto-block on, with
        // the constant's TTL. An install that never touches these keys is unchanged.
        let p = blocklist_policy(&store, skadi_core::MediaKind::Movie).await;
        assert!(p.auto_block);
        assert_eq!(p.ttl, Some(AUTO_BLOCK_TTL));

        // `0` hours means permanent, not "expire immediately" — an expiry of now
        // would make the block useless the moment it was written.
        store
            .set_config("blocklist.auto_ttl_hours", "0", ConfigSource::Runtime)
            .await
            .unwrap();
        assert_eq!(
            blocklist_policy(&store, skadi_core::MediaKind::Movie)
                .await
                .ttl,
            None
        );

        store
            .set_config("blocklist.auto_ttl_hours", "48", ConfigSource::Runtime)
            .await
            .unwrap();
        assert_eq!(
            blocklist_policy(&store, skadi_core::MediaKind::Movie)
                .await
                .ttl,
            Some(chrono::Duration::hours(48))
        );

        // Per-domain exemption: a "failed" audiobook grab is usually a naming
        // problem, so blocking the release punishes the wrong thing.
        store
            .set_config(
                "blocklist.auto_block_exempt_domains",
                " Audiobook , ",
                ConfigSource::Runtime,
            )
            .await
            .unwrap();
        assert!(
            !blocklist_policy(&store, skadi_core::MediaKind::Audiobook)
                .await
                .auto_block,
            "matched case-insensitively, with surrounding whitespace and a trailing comma"
        );
        assert!(
            blocklist_policy(&store, skadi_core::MediaKind::Movie)
                .await
                .auto_block,
            "other domains are unaffected"
        );
    }
    use super::{
        AUTO_BLOCK_TTL, MONITOR_MAX_LIFETIME, MONITOR_MAX_POLLS, MONITOR_RETRY_ATTEMPTS,
        MONITOR_RETRY_DELAY_SECS, MONITOR_STALL_TIMEOUT, not_found_backoff, stall_verdict,
        transfer_backoff,
    };
    use crate::tracker::TransferWatch;

    #[test]
    fn monitor_budget_is_one_quick_poll_per_execution_with_headroom_past_the_cap() {
        // Cloacina 0.6 runs task bodies inline in its scheduler loop: anything
        // but a single quick poll blocks all other acquire runs (SKADI-T-0388).
        assert_eq!(MONITOR_MAX_POLLS, 1);
        // The retry budget must outlast the lifetime cap so the cap's clean
        // terminal path (blocklist + Failed{retry_at}) fires before Cloacina
        // gives up on the task with a bare Failed.
        let budget = chrono::Duration::seconds(
            i64::from(MONITOR_RETRY_ATTEMPTS) * MONITOR_RETRY_DELAY_SECS as i64,
        );
        assert!(budget > MONITOR_MAX_LIFETIME + MONITOR_STALL_TIMEOUT / 2);
    }

    /// SKADI-T-0394: the stall clock only runs while the client has the transfer
    /// live. A transfer queued behind the client's hash checks (or a magnet still
    /// fetching metadata) sits at 0% for hours through no fault of the release —
    /// 46 releases were permanently blocklisted for exactly that on 2026-09-06.
    #[test]
    fn a_transfer_the_client_has_not_started_never_stalls() {
        let since = chrono::Utc::now();
        let queued = TransferWatch {
            since,
            best_progress: 0.0,
            progress_at: since,
            live_since: None,
        };
        // Five hours in the client's queue: not a stall, and not yet the cap.
        assert_eq!(
            stall_verdict(
                &queued,
                since + chrono::Duration::hours(5),
                StallPolicy::default()
            ),
            None
        );
        // The lifetime cap still ends a genuine zombie.
        let why = stall_verdict(
            &queued,
            since + MONITOR_MAX_LIFETIME,
            StallPolicy::default(),
        )
        .expect("lifetime cap");
        assert!(why.contains("under watch"), "{why}");
    }

    /// The clock starts when the client reports the transfer live, so time spent
    /// waiting in the queue does not count towards the stall timeout.
    #[test]
    fn the_stall_clock_starts_when_the_transfer_goes_live() {
        let since = chrono::Utc::now();
        let went_live = since + chrono::Duration::hours(4);
        let watch = TransferWatch {
            since,
            best_progress: 0.0,
            progress_at: went_live,
            live_since: Some(went_live),
        };
        // Four hours queued plus one hour live: under the 3 h stall timeout.
        assert_eq!(
            stall_verdict(
                &watch,
                went_live + chrono::Duration::hours(1),
                StallPolicy::default()
            ),
            None
        );
        // Flat for the whole timeout *after* going live: terminal.
        assert!(
            stall_verdict(
                &watch,
                went_live + MONITOR_STALL_TIMEOUT,
                StallPolicy::default()
            )
            .is_some()
        );
    }

    /// SKADI-T-0442: an existing backoff is kept only while it is still ahead of
    /// us. This is the pure half of the guard in `write_retry_failed` — the check
    /// that turned "there is a retry_at" into "there is a retry_at we should
    /// still be waiting on".
    #[test]
    fn only_a_future_retry_time_suppresses_a_rewrite() {
        let now = chrono::Utc::now();
        let future = now + chrono::Duration::minutes(30);
        let past = now - chrono::Duration::minutes(30);
        assert!(future > now, "a live backoff is preserved");
        assert!(
            past <= now,
            "an elapsed backoff must not be preserved — keeping it re-sweeps the \
             item immediately, which is the opposite of backing off"
        );
    }

    /// SKADI-T-0437: consecutive transfer failures on one item back off hard
    /// instead of retrying every 30 minutes forever (Sword of Destiny was grabbed
    /// three times in one day). Capped, not terminal — availability changes.
    #[test]
    fn transfer_backoff_escalates_and_caps() {
        assert_eq!(transfer_backoff(1), chrono::Duration::minutes(30));
        assert_eq!(transfer_backoff(2), chrono::Duration::hours(2));
        assert_eq!(transfer_backoff(3), chrono::Duration::hours(6));
        assert_eq!(transfer_backoff(4), chrono::Duration::hours(24));
        assert_eq!(transfer_backoff(5), chrono::Duration::hours(72));
        assert_eq!(transfer_backoff(50), chrono::Duration::hours(72));
        // Strictly increasing until the cap, so an item never retries *sooner*
        // after failing again.
        for n in 1..5u32 {
            assert!(transfer_backoff(n) < transfer_backoff(n + 1), "{n}");
        }
    }

    /// SKADI-T-0436: an automatic block expires, so a release the hunter misjudged
    /// once heals itself instead of staying unusable until someone clears it.
    #[test]
    fn automatic_blocks_carry_a_ttl() {
        assert!(AUTO_BLOCK_TTL > chrono::Duration::days(7), "long enough");
        assert!(AUTO_BLOCK_TTL <= chrono::Duration::days(90), "not forever");
    }
    #[test]
    fn stall_verdict_gives_up_when_progress_stays_flat_for_the_timeout() {
        let since = chrono::Utc::now();
        let watch = TransferWatch {
            since,
            best_progress: 0.42,
            progress_at: since,
            live_since: Some(since),
        };
        // Flat, but not for long enough: keep retrying.
        assert_eq!(
            stall_verdict(
                &watch,
                since + MONITOR_STALL_TIMEOUT - chrono::Duration::seconds(1),
                StallPolicy::default()
            ),
            None
        );
        // Flat for the whole timeout: terminal, naming the frozen progress.
        let why = stall_verdict(
            &watch,
            since + MONITOR_STALL_TIMEOUT,
            StallPolicy::default(),
        )
        .expect("stalled");
        assert!(why.contains("stalled at 42%"), "{why}");
        assert!(
            why.contains(&format!("{}h", MONITOR_STALL_TIMEOUT.num_hours())),
            "{why}"
        );
    }

    #[test]
    fn stall_verdict_lets_a_crawling_transfer_run_until_the_lifetime_cap() {
        let since = chrono::Utc::now();
        let now = since + MONITOR_MAX_LIFETIME;
        // Progress moved recently (a slow-but-live transfer), but it has been
        // under watch for the whole cap.
        let watch = TransferWatch {
            since,
            best_progress: 0.31,
            progress_at: now - chrono::Duration::minutes(1),
            live_since: Some(since),
        };
        let why = stall_verdict(&watch, now, StallPolicy::default()).expect("expired");
        assert!(why.contains("31%"), "{why}");
        assert!(
            why.contains(&format!("{}h", MONITOR_MAX_LIFETIME.num_hours())),
            "{why}"
        );
        // One second short of the cap is still retryable.
        assert_eq!(
            stall_verdict(
                &watch,
                now - chrono::Duration::seconds(1),
                StallPolicy::default()
            ),
            None
        );
    }

    #[test]
    fn search_error_keeps_the_not_found_ladder_where_it_was() {
        let now = Utc::now();
        match failed_after_search_error(6, "search: boom".into(), now) {
            AcquisitionStatus::Failed {
                reason,
                retry_at,
                attempts,
            } => {
                assert_eq!(attempts, 6, "an error is not a not-found attempt");
                assert_eq!(retry_at, Some(now + search_error_retry()));
                assert!(matches!(reason, FailureReason::Other(m) if m == "search: boom"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            search_error_retry() < not_found_backoff(1),
            "flat and shorter than the first rung"
        );
    }

    #[test]
    fn not_found_backoff_escalates_then_caps() {
        let h = |a| not_found_backoff(a).num_hours();
        assert_eq!(h(0), 6); // defensive: 0 treated as a first miss
        assert_eq!(h(1), 6);
        assert_eq!(h(2), 12);
        assert_eq!(h(3), 24);
        assert_eq!(h(4), 72); // cap
        assert_eq!(h(99), 72); // stays capped
        // Monotonic non-decreasing.
        let seq: Vec<i64> = (0..6).map(h).collect();
        assert!(seq.windows(2).all(|w| w[0] <= w[1]), "{seq:?}");
    }

    #[test]
    fn distinct_imported_keeps_one_file_per_book_in_order() {
        use super::distinct_imported;
        use skadi_core::FileRef;
        use skadi_importer::{AcquirableRef, ImportedFile};
        let mk = |a: &str, p: &str| ImportedFile {
            acquirable: AcquirableRef(a.to_string()),
            file: FileRef { path: p.into() },
        };
        // A pack outcome: books A (2 files), B (2 files), C (1) interleaved — the status
        // fan-out (SKADI-T-0310) must mark each book once, using its first file.
        let imported = vec![
            mk("A", "a1"),
            mk("A", "a2"),
            mk("B", "b1"),
            mk("C", "c1"),
            mk("B", "b2"),
        ];
        let got: Vec<(&str, &str)> = distinct_imported(&imported)
            .iter()
            .map(|f| (f.acquirable.0.as_str(), f.file.path.to_str().unwrap()))
            .collect();
        assert_eq!(got, vec![("A", "a1"), ("B", "b1"), ("C", "c1")]);
    }
}
