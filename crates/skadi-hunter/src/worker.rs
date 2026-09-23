//! Trigger surface and the [`HunterWorker`] that drives the acquire workflow.
//!
//! Two ways to start an acquire run:
//! - [`start_acquire`] for on-demand single-run execution (e.g. an API call).
//! - The scheduled sweep inside [`HunterWorker`] that enumerates wanted /
//!   upgradable acquirables (via the domain-supplied [`WantedQuery`]) and
//!   starts one run per item.
//!
//! [`HunterWorker`] implements [`skadi_core::Worker`]: it builds the Cloacina
//! runner, installs the [`HunterServices`] global (so tasks can reach
//! capabilities), runs the sweep on a tokio interval, and on cancel calls
//! `runner.shutdown().await` to drain Cloacina's background services. Cloacina
//! cron scheduling is off (we drive the sweep ourselves; see [`crate::workflow`]
//! and the I-0006 architecture).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use cloacina::executor::WorkflowExecutor;
use cloacina::runner::DefaultRunner;
use cloacina::{Context, WorkflowExecutionResult};
use futures::StreamExt;
use skadi_core::module::BoxFuture;
use skadi_core::{AppError, MediaKind, ProfileId, Result, Worker};
use skadi_importer::AcquirableRef;
use skadi_indexers::{Indexer, Release, indexer_health, normalize_title};
use skadi_store::ConfigRepo;
use tokio_util::sync::CancellationToken;

use crate::services::{HunterServices, set_services};
use crate::state::{AcquireState, SearchSpec};

/// One acquirable the hunter should try to satisfy, as the domain hands it to
/// the trigger surface. Serializable so the seed can sit on a queue if we ever
/// want one.
#[derive(Clone, Debug)]
pub struct AcquireSeed {
    pub acquirable: AcquirableRef,
    pub request: SearchSpec,
    pub profile: ProfileId,
    /// When this seed is an *upgrade* (the acquirable is already `Imported`), the
    /// quality it currently holds — so `decide` only grabs a strictly better
    /// release (SKADI-T-0182). `None` for a first acquisition. The domain's
    /// `upgradable()` sets this from `AcquisitionStatus::Imported { quality }`;
    /// `wanted()` leaves it `None`.
    pub current_quality: Option<skadi_core::QualityId>,
    /// When this seed is an *upgrade*, the aggregate custom-format **score** of
    /// the held file (SKADI-T-0186) — so `decide` also upgrades on the format-score
    /// axis (proper/repack). `upgradable()` sets it from
    /// `AcquisitionStatus::Imported { score }`; `wanted()` leaves it `None`.
    pub current_format_score: Option<i32>,
    /// The held file has been probed and will not direct-play (SKADI-T-0584) —
    /// a DTS-only track, an `m2ts`, a 10-bit H.264. Relaxes the strictly-better
    /// rule so the sweep can actually replace it. `false` for a first
    /// acquisition and for anything not yet probed.
    pub current_unplayable: bool,
}

impl AcquireSeed {
    /// Build the run's initial Cloacina context from this seed, stamped with
    /// the tracker `run_id` it was launched under (SKADI-T-0388 ownership).
    pub fn into_context(self, run_id: Option<String>) -> Result<Context<serde_json::Value>> {
        let mut state = AcquireState::new(self.acquirable, self.request, self.profile)
            .with_current(self.current_quality, self.current_format_score)
            .with_current_unplayable(self.current_unplayable);
        state.run_id = run_id;
        state.into_context()
    }
}

/// How long an acquirable may sit in a non-terminal acquire state before the
/// sweep treats it as wedged and recovers it (SKADI-T-0112). Deliberately well
/// past Cloacina's own stale-claim recovery window (~60–90s, not tuned by skadi):
/// within this grace we defer to Cloacina; past it we assume its recovery did
/// not fire (e.g. an abrupt crash that also took the DB) and recover ourselves.
pub const STALE_ACQUIRE_GRACE: Duration = Duration::from_secs(15 * 60);

/// How long an *adopted* (recovery-replayed, see [`crate::tracker`]) run may go
/// without any sign of life before the sweep stops counting it as in flight
/// (SKADI-T-0388). Comfortably more than one `monitor` execution window
/// (~30 min): a queued-but-live transfer re-enters `monitor` — and touches the
/// tracker — at least that often, while a workflow that failed hard never does.
pub const ADOPTED_RUN_MAX_IDLE: chrono::Duration = chrono::Duration::hours(2);

/// Default cap on concurrent acquire runs the sweep launches per tick
/// (SKADI-T-0040). Conservative so a large library doesn't open hundreds of
/// indexer connections at once and trip trackers; tunable via the config plane
/// (`sweep_max_concurrent`).
/// 8, up from 4 (SKADI-T-0595): now that a lane is freed the moment a grab is
/// handed off, lanes only bound concurrent *searches*, and the indexers'
/// own rate limits and the breaker are the real backstop.
pub const DEFAULT_SWEEP_MAX_CONCURRENT: usize = 8;

/// Title-relevance coverage below which a *chosen* release is called out as a
/// weak match on the trace (SKADI-T-0380). Above the `decide` gate
/// (`MIN_TITLE_COVERAGE`, 0.6) — so this flags grabs that passed the gate but
/// only just, the band where "101 Dalmatians" quietly becomes
/// "101 Dalmatians II". Advisory only: it changes no decision.
const WEAK_MATCH: f32 = 0.8;

/// Domain-supplied source of work for the sweep: wanted / upgradable
/// acquirables and how to search for each.
#[async_trait]
pub trait WantedQuery: Send + Sync {
    /// Items that have nothing yet — find a first release.
    async fn wanted(&self) -> Result<Vec<AcquireSeed>>;

    /// Items that are imported but below cutoff — try to upgrade.
    async fn upgradable(&self) -> Result<Vec<AcquireSeed>> {
        Ok(Vec::new())
    }

    /// Recover acquirables wedged in a non-terminal acquire state with no live
    /// run — a daemon crash clears the in-memory tracker and Cloacina's
    /// recovery can fail (SKADI-T-0112). Implementations reset anything stuck
    /// longer than [`STALE_ACQUIRE_GRACE`] back to its initial state so the same
    /// sweep's [`wanted`](Self::wanted) re-acquires it. Returns the count
    /// recovered. Default: nothing to reconcile.
    async fn reconcile_stale(&self) -> Result<usize> {
        Ok(0)
    }
}

/// The outcome of a [`start_acquire`] call.
#[derive(Debug)]
pub enum AcquireOutcome {
    /// An `acquire_release` run was started and awaited; carries the workflow's
    /// result.
    Started(WorkflowExecutionResult),
    /// An `acquire_release` run was launched **detached** (SKADI-T-0595): the
    /// workflow — snatch, the download monitor, import — runs on its own task
    /// and the caller's lane is free again. Its result is logged, not returned.
    Launched,
    /// A run for this acquirable was already in flight — nothing was started
    /// (SKADI-T-0039 dedup). Not an error.
    AlreadyInFlight,
    /// In-process search/decide found no suitable release this tick, so no
    /// workflow was launched (SKADI-T-0042 grain). The item's status was set to
    /// `Failed{NoSuitableRelease}` by `decide`. Not an error — it will be
    /// re-evaluated next sweep.
    NoRelease,
}

/// Trigger one acquire run for `seed`, unless one is already in flight for the
/// same acquirable (SKADI-T-0039). The in-flight set ([`crate::tracker`], which
/// also drives the `/activity` view) is checked-and-claimed atomically before
/// executing and released on every exit; a duplicate returns
/// [`AcquireOutcome::AlreadyInFlight`] without double-snatching.
///
/// Process-local: a single daemon is assumed; concurrent runs across two
/// processes against the same DB would still race (Cloacina's task-claim layer
/// is the backstop). Manual grab ([`start_grab`]) is an explicit override and is
/// guarded at the HTTP layer instead.
///
/// **Blocking contract**: this returns only after the run is terminal.
/// `runner.execute` awaits the whole `acquire_release` DAG — including every
/// `monitor_release` retry — so on `Started` the acquirable's status is already
/// whatever `import`/`monitor` wrote (verified against Cloacina's trace in
/// SKADI-T-0384). Note that `Imported` is keyed by the acquirable(s) the
/// **matcher** returns (SKADI-T-0310 fan-out), which is normally but not
/// necessarily this seed's; a run whose file matched a different acquirable
/// leaves its own ref at `Snatched` for `reconcile_stale` to clear.
/// The in-process search/decide phase of a launch, shared by the awaiting and
/// detached launchers (SKADI-T-0595).
enum Prepared {
    /// Nothing to run: already in flight, or no release chosen.
    Early(AcquireOutcome),
    /// A release was chosen; the `acquire_release` workflow is ready to start.
    Ready {
        ctx: Context<serde_json::Value>,
        acquirable_ref: String,
    },
}

async fn prepare_acquire(seed: AcquireSeed) -> Result<Prepared> {
    let acquirable_ref = seed.acquirable.0.clone();
    let kind = seed.request.kind;
    let run_id = uuid::Uuid::new_v4().to_string();
    // Claim the in-flight slot; bail (not an error) if a run already holds it.
    if !crate::tracker::tracker().try_start(run_id.clone(), kind, acquirable_ref.clone()) {
        tracing::info!(acquirable = %acquirable_ref, "acquire already in flight; skipping duplicate");
        return Ok(Prepared::Early(AcquireOutcome::AlreadyInFlight));
    }

    let mut ctx = match seed.into_context(Some(run_id)) {
        Ok(ctx) => ctx,
        Err(e) => {
            crate::tracker::tracker().finish(&acquirable_ref);
            return Err(e);
        }
    };

    // SKADI-T-0042 grain (ADR SKADI-A-0002): run the cheap, idempotent
    // search/decide phase **in-process** and only launch a Cloacina workflow
    // (`acquire_release`) when a release is actually chosen. A no-suitable-release
    // item produces zero workflow rows.
    crate::tracker::tracker().set_stage(&acquirable_ref, "searching");
    if let Err(e) = crate::steps::search(&mut ctx).await {
        // A hard search failure (all indexers down) is a real, retryable error.
        crate::tracker::tracker().finish(&acquirable_ref);
        return Err(e);
    }
    // Trace: how many candidates the search surfaced (SKADI-T-0323) — the first
    // signal of whether a flaky source had anything to offer this sweep.
    if let Ok(state) = crate::state::load_state(&ctx) {
        let n = state.candidates.len();
        // Carry *what was searched for* alongside the count (SKADI-T-0380) —
        // a count alone can't distinguish "no results" from "results for the
        // wrong thing".
        crate::trace::emit(
            kind,
            &acquirable_ref,
            "searching",
            "candidates_found",
            format!("found {n} candidate{}", if n == 1 { "" } else { "s" }),
            crate::trace::search_detail(&state.request),
        )
        .await;
    }
    crate::tracker::tracker().set_stage(&acquirable_ref, "deciding");
    match crate::steps::decide(&mut ctx).await {
        Ok(()) => {}
        // "No suitable release" is an expected outcome, not an error — `decide`
        // already wrote Failed{NoSuitableRelease}. Don't launch a run.
        Err(AppError::NotFound(_)) => {
            // The single most-asked question in a stalled library is "candidates
            // existed — which gate ate them?" Name the busiest one inline and
            // attach the full per-gate tally (SKADI-T-0380).
            let tally = crate::state::load_state(&ctx)
                .ok()
                .and_then(|s| s.tally.clone());
            let message = match tally.as_ref().and_then(|t| t.top_reason()) {
                Some((reason, n)) => {
                    let total = tally.as_ref().map_or(0, |t| t.rejected_total());
                    format!("no suitable release — {total} rejected, mostly {reason} ({n})")
                }
                None => "no suitable release".to_string(),
            };
            crate::trace::emit(
                kind,
                &acquirable_ref,
                "deciding",
                "no_release",
                message,
                tally.as_ref().and_then(crate::trace::tally_detail),
            )
            .await;
            crate::tracker::tracker().finish(&acquirable_ref);
            return Ok(Prepared::Early(AcquireOutcome::NoRelease));
        }
        Err(e) => {
            crate::tracker::tracker().finish(&acquirable_ref);
            return Err(e);
        }
    }

    // Acquisition transparency (SKADI-T-0190): surface what `decide` chose and how
    // many candidates it weighed, so `/activity` shows chosen-vs-considered live.
    if let Ok(state) = crate::state::load_state(&ctx) {
        let candidates = state.candidates.len();
        let chosen_title = state.chosen.as_ref().map(|r| r.title.clone());
        crate::tracker::tracker().set_chosen(&acquirable_ref, chosen_title.clone(), candidates);
        // Trace: what `decide` chose and out of how many (SKADI-T-0323), plus
        // how tightly the chosen title actually matched and what the other
        // candidates died of (SKADI-T-0380).
        if let Some(title) = chosen_title {
            let tally = state.tally.clone();
            let mut message = format!(
                "chose \"{title}\" of {candidates} candidate{}",
                if candidates == 1 { "" } else { "s" }
            );
            // Seeders are what decide now optimises for (SKADI-T-0598); put the
            // number where the operator can see why this one won.
            if let Some(n) = state.chosen.as_ref().and_then(|r| r.seeders) {
                message.push_str(&format!(" ({n} seeder{})", if n == 1 { "" } else { "s" }));
            }
            // A weak match is the signature of a wrong-title grab; put the score
            // in the line an operator scans rather than behind a disclosure.
            if let Some(r) = tally.as_ref().and_then(|t| t.chosen_relevance)
                && r < WEAK_MATCH
            {
                message.push_str(&format!(" — weak title match ({r:.2})"));
            }
            crate::trace::emit(
                kind,
                &acquirable_ref,
                "deciding",
                "decision",
                message,
                tally.as_ref().and_then(crate::trace::tally_detail),
            )
            .await;
        }
    }
    crate::tracker::tracker().set_stage(&acquirable_ref, "grabbing");
    Ok(Prepared::Ready {
        ctx,
        acquirable_ref,
    })
}

/// Start one acquire run and **wait for it** — the manual `…/acquire` endpoints
/// and tests want the workflow's result. A sweep lane must not use this: the
/// wait spans the whole download (see [`start_acquire_detached`]).
pub async fn start_acquire(runner: &DefaultRunner, seed: AcquireSeed) -> Result<AcquireOutcome> {
    let (ctx, acquirable_ref) = match prepare_acquire(seed).await? {
        Prepared::Early(outcome) => return Ok(outcome),
        Prepared::Ready {
            ctx,
            acquirable_ref,
        } => (ctx, acquirable_ref),
    };
    let result = runner
        .execute("acquire_release", ctx)
        .await
        .map_err(|e| AppError::Internal(format!("acquire_release workflow execute: {e}")));
    crate::tracker::tracker().finish(&acquirable_ref);
    result.map(AcquireOutcome::Started)
}

/// Start one acquire run and hand it off (SKADI-T-0595): search and decide run
/// here, then the `acquire_release` workflow — snatch, the download monitor
/// polling every 30 s for up to 20 h, import — is spawned on its own task and
/// this returns [`AcquireOutcome::Launched`] at once.
///
/// This is what lets a sweep lane go on to the next item. Before, a lane that
/// grabbed something was held for the entire transfer, and the sweep tick
/// waited for every lane, so one stalled torrent (3 h to time out) throttled a
/// whole domain's searching to nothing. The in-flight tracker still holds the
/// item until the spawned workflow finishes, so no sweep re-searches it.
pub async fn start_acquire_detached(
    runner: Arc<DefaultRunner>,
    seed: AcquireSeed,
) -> Result<AcquireOutcome> {
    let (ctx, acquirable_ref) = match prepare_acquire(seed).await? {
        Prepared::Early(outcome) => return Ok(outcome),
        Prepared::Ready {
            ctx,
            acquirable_ref,
        } => (ctx, acquirable_ref),
    };
    tokio::spawn(async move {
        // Release the in-flight slot (and the release-title claim) however this
        // task ends — a panic inside the workflow, or the runtime dropping the
        // task on shutdown — otherwise the acquirable reads "already in flight"
        // and the release title stays claimed until the next restart.
        let _guard = FinishOnDrop(acquirable_ref.clone());
        match runner.execute("acquire_release", ctx).await {
            Ok(res) => {
                if !matches!(res.status, cloacina::WorkflowStatus::Completed) {
                    tracing::warn!(
                        acquirable = %acquirable_ref,
                        status = ?res.status,
                        "detached acquire run finished non-Completed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                acquirable = %acquirable_ref,
                "detached acquire_release workflow execute failed: {e}"
            ),
        }
    });
    Ok(AcquireOutcome::Launched)
}

/// Calls `tracker().finish` for the acquirable when dropped (SKADI-T-0595).
struct FinishOnDrop(String);

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        crate::tracker::tracker().finish(&self.0);
    }
}

/// Grab a **specific** release (SKADI-T-0114, manual/interactive search): start
/// the acquire workflow with `chosen` pre-populated so `search`/`decide` no-op
/// and the run begins at `snatch`. Same tracker bookkeeping as [`start_acquire`].
pub async fn start_grab(
    runner: &DefaultRunner,
    acquirable: AcquirableRef,
    request: SearchSpec,
    profile: ProfileId,
    chosen: Release,
) -> Result<WorkflowExecutionResult> {
    let acquirable_ref = acquirable.0.clone();
    let kind = request.kind;
    let run_id = uuid::Uuid::new_v4().to_string();
    crate::tracker::tracker().start(run_id.clone(), kind, acquirable_ref.clone());

    let mut state = AcquireState::new(acquirable, request, profile);
    state.run_id = Some(run_id);
    state.candidates = vec![chosen.clone()];
    state.manual = true;
    state.chosen = Some(chosen);
    let ctx = match state.into_context() {
        Ok(ctx) => ctx,
        Err(e) => {
            crate::tracker::tracker().finish(&acquirable_ref);
            return Err(e);
        }
    };
    // Manual grab begins past search/decide (`chosen` is pre-set), so it launches
    // the per-release workflow directly (SKADI-T-0042).
    let result = runner
        .execute("acquire_release", ctx)
        .await
        .map_err(|e| AppError::Internal(format!("grab workflow execute: {e}")));
    crate::tracker::tracker().finish(&acquirable_ref);
    result
}

/// Every title key a feed entry can be matched on (SKADI-T-0431).
///
/// The RSS fast pass keys entries on the parser's extracted title so a wanted
/// "Sintel" matches the feed entry "Sintel 2010 1080p BluRay x264-GRP". That
/// works for movies. For an episode the **movie** parse keeps the episode tag —
/// `Lioness.S03E02.1080p.WEB-DL.h264-GRP` yields "lioness s03e02" — which no
/// wanted show title ever equals, so the fast pass silently did nothing for
/// series: every new episode waited for the next full sweep.
///
/// Parsing the raw name as TV as well yields the show title. The TV parser keeps
/// a year that is part of the release name (`Lioness.2023.S03E02` → "Lioness
/// 2023"), and a wanted title is usually the bare show name, so the year-stripped
/// form is offered too. Keys are normalised; duplicates are harmless because the
/// caller collects them into a set.
pub fn feed_title_keys(raw: &str, parsed_title: Option<&str>) -> Vec<String> {
    let mut keys = Vec::with_capacity(3);
    if let Some(t) = parsed_title {
        keys.push(normalize_title(t));
    }
    if let Some(tv) = skadi_quality::parse_tv(raw).title {
        keys.push(normalize_title(&tv));
        // Drop a trailing disambiguating year: "lioness 2023" → "lioness".
        let words: Vec<&str> = tv.split_whitespace().collect();
        if words.len() > 1
            && words
                .last()
                .is_some_and(|w| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))
        {
            keys.push(normalize_title(&words[..words.len() - 1].join(" ")));
        }
    }
    keys
}

/// Enumerate wanted + upgradable items via `query` and start one acquire run
/// per item, **at most `max_concurrent` in flight at once** (SKADI-T-0040).
/// Errors from individual runs are logged and do not stop the sweep; returns the
/// number of runs started.
pub async fn sweep_once(
    runner: &DefaultRunner,
    query: &dyn WantedQuery,
    max_concurrent: usize,
) -> Result<usize> {
    sweep_with(query, max_concurrent, |seed| start_acquire(runner, seed)).await
}

/// [`sweep_once`] with detached launches (SKADI-T-0595): the lanes bound the
/// **searching**, and each grab's workflow runs on its own task. This is the
/// worker's sweep; the awaiting form stays for tests and manual drains.
pub async fn sweep_once_detached(
    runner: Arc<DefaultRunner>,
    query: &dyn WantedQuery,
    max_concurrent: usize,
) -> Result<usize> {
    sweep_with(query, max_concurrent, move |seed| {
        start_acquire_detached(Arc::clone(&runner), seed)
    })
    .await
}

/// One sweep over every due item, launching each through `launch` with at
/// most `max_concurrent` launches in flight at once (SKADI-T-0040).
async fn sweep_with<F, Fut>(
    query: &dyn WantedQuery,
    max_concurrent: usize,
    launch: F,
) -> Result<usize>
where
    F: Fn(AcquireSeed) -> Fut,
    Fut: std::future::Future<Output = Result<AcquireOutcome>>,
{
    // Forget adopted (recovery-replayed) runs that have gone quiet — their
    // workflow died without reaching a step that could finish them — so the
    // dedup below stops treating them as in flight (SKADI-T-0388).
    let expired = crate::tracker::tracker().expire_adopted(ADOPTED_RUN_MAX_IDLE);
    if !expired.is_empty() {
        tracing::warn!(count = expired.len(), refs = ?expired, "expired quiet recovered acquire runs");
    }
    // First, recover anything wedged in a non-terminal state with no live run
    // (SKADI-T-0112) — reset to its initial state so `wanted()` below re-acquires
    // it in this same sweep. Best-effort: a reconciliation error doesn't stop the
    // sweep.
    match query.reconcile_stale().await {
        Ok(n) if n > 0 => {
            tracing::warn!(recovered = n, "reconciled wedged acquires (re-acquiring)")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "stale-acquire reconciliation failed"),
    }
    let mut seeds = query.wanted().await?;
    seeds.extend(query.upgradable().await?);
    // Dedup duplicate seeds within this tick (SKADI-T-0039) — `start_acquire`
    // also guards concurrent/cross-tick duplicates via the in-flight tracker,
    // but this drops obvious in-tick duplicates before we even launch.
    let mut seen = HashSet::new();
    let unique: Vec<AcquireSeed> = seeds
        .into_iter()
        .filter(|s| seen.insert(s.acquirable.0.clone()))
        .collect();

    // Launch runs bounded-concurrently (SKADI-T-0040): at most `max_concurrent`
    // acquire runs in flight at once so a large sweep can't flood indexers /
    // trip trackers. `buffer_unordered` drives the futures on this task (no
    // spawn → the borrowed `runner` is fine).
    let max = max_concurrent.max(1);
    let outcomes = futures::stream::iter(unique)
        .map(launch)
        .buffer_unordered(max)
        .collect::<Vec<_>>()
        .await;

    let mut count = 0;
    for outcome in outcomes {
        match outcome {
            Ok(AcquireOutcome::Started(res)) => {
                count += 1;
                if !matches!(res.status, cloacina::WorkflowStatus::Completed) {
                    tracing::warn!(status = ?res.status, "acquire run finished non-Completed");
                }
            }
            Ok(AcquireOutcome::Launched) => count += 1,
            Ok(AcquireOutcome::AlreadyInFlight) => {
                tracing::debug!("acquire already in flight; skipped by sweep");
            }
            Ok(AcquireOutcome::NoRelease) => {
                tracing::debug!("no suitable release this tick; no run launched");
            }
            Err(e) => tracing::warn!(error = %e, "acquire run failed to start"),
        }
    }
    Ok(count)
}

/// Default interval for the RSS fast pass (SKADI-T-0192): far shorter than the
/// full sweep, since pulling each indexer's recent feed is one cheap request and
/// the whole point is to catch a new release within minutes, not hours.
pub const DEFAULT_RSS_INTERVAL: Duration = Duration::from_secs(60);

/// How often the worker re-reads the config plane (SKADI-T-0544).
///
/// The live settings used to be refreshed inside the sweep arm, which meant a
/// change took up to one *sweep* interval to apply — on a 15-minute sweep that
/// is a 15-minute wait to find out whether a setting works, and on a longer one
/// it is worse. A dedicated short ticker decouples "how often we look at
/// config" from "how often we sweep", so every live setting applies within this
/// window regardless of how the sweep is tuned.
pub const CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Rebuild `ticker` to run at `desired`, but **only if the period changed**.
///
/// Returns whether it rebuilt, which is what makes the "only on change" rule
/// testable rather than a comment.
///
/// Two things matter here and both are easy to get wrong:
///
/// * Rebuilding unconditionally would reset the ticker's phase on every config
///   refresh. With a sweep interval longer than the refresh interval the timer
///   would then never fire at all — the sweep would stop, silently, because a
///   setting was being read.
/// * The new ticker starts at `now + desired`, not now. A change takes effect
///   from the next scheduled fire; otherwise editing a setting would trigger an
///   immediate sweep as a side effect, and repeatedly saving a form would hammer
///   the indexers.
fn retune(current: &mut Duration, ticker: &mut tokio::time::Interval, desired: Duration) -> bool {
    if *current == desired {
        return false;
    }
    *current = desired;
    *ticker = tokio::time::interval_at(tokio::time::Instant::now() + desired, desired);
    true
}

/// The interval settings in force, resolved from the config plane against the
/// values the worker was constructed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Intervals {
    sweep: Duration,
    /// `None` ⇒ the RSS fast pass is off.
    rss: Option<Duration>,
}

impl Intervals {
    /// Resolve from raw config values, falling back to the constructed ones.
    ///
    /// `sweep = 0` falls back rather than being honoured: a zero interval spins
    /// the sweep continuously against every indexer, which is never what an
    /// operator means by "how often should this run" — the same reasoning that
    /// floors `sweep_max_concurrent` at 1 (SKADI-T-0440).
    ///
    /// `rss = 0` genuinely means **off**, because that is a thing to want: the
    /// fast pass costs a round of indexer queries and an operator may not want
    /// it at all. That asymmetry is deliberate — "never sweep" would break the
    /// daemon's whole job, "never RSS" is a supported configuration that already
    /// exists as `rss_interval: None`.
    fn resolve(sweep_secs: Option<u64>, rss_secs: Option<u64>, fallback: Intervals) -> Self {
        Self {
            sweep: sweep_secs
                .filter(|s| *s > 0)
                .map_or(fallback.sweep, Duration::from_secs),
            rss: match rss_secs {
                Some(0) => None,
                Some(s) => Some(Duration::from_secs(s)),
                None => fallback.rss,
            },
        }
    }
}

/// Default age (days) the acquisition history is retained before the sweep purges
/// older rows (SKADI-T-0199), so the table stays bounded without a separate
/// scheduler. A long-but-finite window — operators rarely need year-old events.
pub const DEFAULT_HISTORY_RETENTION_DAYS: i64 = 90;

/// An **RSS-driven fast pass** (SKADI-T-0192): pull each enabled indexer's recent
/// feed once, then start acquire runs **only** for wanted items whose title shows
/// up in the feed — catching newly-posted releases within minutes without a full
/// per-item search fan-out. `start_acquire` then re-runs search/decide to confirm
/// and pick the best release, so the RSS pass is purely a *cheap trigger*: it never
/// grabs a feed item blindly, it just tells the hunter which wanted items are worth
/// searching right now.
///
/// Per-indexer feed failures are tolerated (RSS is best-effort reactivity — the
/// full sweep remains the source of truth). Returns the number of runs started.
pub async fn rss_sweep(
    runner: &DefaultRunner,
    indexers: &[Arc<dyn Indexer>],
    query: &dyn WantedQuery,
    kind: MediaKind,
    max_concurrent: usize,
) -> Result<usize> {
    rss_sweep_with(indexers, query, kind, max_concurrent, |seed| {
        start_acquire(runner, seed)
    })
    .await
}

/// [`rss_sweep`] with detached launches (SKADI-T-0595) — the worker's form, so
/// an RSS-triggered grab does not park the worker loop for the download.
pub async fn rss_sweep_detached(
    runner: Arc<DefaultRunner>,
    indexers: &[Arc<dyn Indexer>],
    query: &dyn WantedQuery,
    kind: MediaKind,
    max_concurrent: usize,
) -> Result<usize> {
    rss_sweep_with(indexers, query, kind, max_concurrent, move |seed| {
        start_acquire_detached(Arc::clone(&runner), seed)
    })
    .await
}

async fn rss_sweep_with<F, Fut>(
    indexers: &[Arc<dyn Indexer>],
    query: &dyn WantedQuery,
    kind: MediaKind,
    max_concurrent: usize,
    launch: F,
) -> Result<usize>
where
    F: Fn(AcquireSeed) -> Fut,
    Fut: std::future::Future<Output = Result<AcquireOutcome>>,
{
    // 1. Pull recent feeds across the indexers that serve this kind. Skip
    //    circuit-open (known-down) indexers (SKADI-T-0308) so the fast RSS tick
    //    doesn't keep hammering dead trackers every cycle.
    let now = Utc::now();
    let mut feed: Vec<Release> = Vec::new();
    let mut probed: Vec<skadi_core::IndexerId> = Vec::new();
    let mut failed = 0usize;
    for idx in indexers {
        if !idx.supports(kind) {
            continue;
        }
        // The operator turned RSS off for this indexer (SKADI-T-0505) — how a
        // slow or rate-limited tracker is kept for explicit searches without
        // being polled every few minutes.
        if !idx.enable_rss() {
            continue;
        }
        if crate::pipeline::search_circuit_open(indexer_health().get(idx.id()).as_ref(), now) {
            continue;
        }
        probed.push(idx.id());
        match idx.rss().await {
            Ok(rs) => feed.extend(rs),
            Err(e) => {
                failed += 1;
                tracing::warn!(error = %e, "indexer RSS feed failed (skipped)");
            }
        }
    }
    // Shared-outage forgiveness (SKADI-T-0308): every probed feed failing at once is a
    // shared-infra outage (VPN/proxy down), not the indexers — don't let the fast RSS
    // tick trip all their circuits. Mirrors `pipeline::search`.
    if probed.len() >= 2 && failed == probed.len() {
        for id in &probed {
            indexer_health().forgive(*id);
        }
    }
    if feed.is_empty() {
        return Ok(0);
    }

    // 2. The set of (normalized) clean titles present in the feed. We match on the
    //    parser's extracted title (`parsed.title`), not the raw release name, so a
    //    wanted "Sintel" matches a feed entry "Sintel 2010 1080p BluRay x264-GRP".
    let present: HashSet<String> = feed
        .iter()
        .flat_map(|r| feed_title_keys(&r.title, r.parsed.title.as_deref()))
        .collect();

    // 3. Wanted items whose title appears in the feed — the only ones worth a
    //    search this tick. (Upgrades are left to the full sweep; RSS is about
    //    catching *new* acquisitions fast.)
    let mut seen = HashSet::new();
    let matched: Vec<AcquireSeed> = query
        .wanted()
        .await?
        .into_iter()
        .filter(|s| {
            s.request
                .titles
                .iter()
                .any(|t| present.contains(&normalize_title(t)))
        })
        .filter(|s| seen.insert(s.acquirable.0.clone()))
        .collect();
    if matched.is_empty() {
        return Ok(0);
    }

    // 4. Launch bounded-concurrently, reusing the normal acquire path.
    let max = max_concurrent.max(1);
    let outcomes = futures::stream::iter(matched)
        .map(launch)
        .buffer_unordered(max)
        .collect::<Vec<_>>()
        .await;

    let mut count = 0;
    for outcome in outcomes {
        match outcome {
            Ok(AcquireOutcome::Started(_) | AcquireOutcome::Launched) => count += 1,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "RSS-triggered acquire failed to start"),
        }
    }
    Ok(count)
}

/// The daemon-side background worker that runs the sweep on a tokio interval
/// until cancelled, sharing the Cloacina runner (via `Arc`) with the domain's
/// HTTP module so manual and swept acquire runs go through one registry.
pub struct HunterWorker {
    services: Arc<HunterServices>,
    runner: Arc<DefaultRunner>,
    sweep_interval: Duration,
    query: Arc<dyn WantedQuery>,
    /// Max concurrent acquire runs per sweep tick (SKADI-T-0040).
    max_concurrent: usize,
    /// Optional RSS fast-pass interval (SKADI-T-0192). `None` ⇒ no RSS tick (the
    /// full sweep still runs). Domains opt in via [`HunterWorker::with_rss`].
    rss_interval: Option<Duration>,
}

impl HunterWorker {
    /// Construct a worker with an already-built runner + the services the
    /// pipeline tasks will reach via the global. `max_concurrent` bounds how many
    /// acquire runs a sweep tick launches at once (SKADI-T-0040;
    /// [`DEFAULT_SWEEP_MAX_CONCURRENT`] is a sensible default).
    ///
    /// The runner is shared (`Arc`): the sweep only ever borrows it
    /// ([`sweep_once`] takes `&DefaultRunner`) and `shutdown` takes `&self`, so
    /// a domain can hand the same runner to both this worker and its HTTP
    /// manual-acquire endpoint without the worker needing exclusive ownership.
    #[must_use]
    pub fn new(
        services: Arc<HunterServices>,
        runner: Arc<DefaultRunner>,
        sweep_interval: Duration,
        query: Arc<dyn WantedQuery>,
        max_concurrent: usize,
    ) -> Self {
        Self {
            services,
            runner,
            sweep_interval,
            query,
            max_concurrent,
            rss_interval: None,
        }
    }

    /// Enable the RSS fast pass (SKADI-T-0192) at `interval` (use
    /// [`DEFAULT_RSS_INTERVAL`]). Builder form so the existing call sites stay
    /// unchanged; without this the worker runs only the full sweep.
    #[must_use]
    pub fn with_rss(mut self, interval: Duration) -> Self {
        self.rss_interval = Some(interval);
        self
    }
}

impl Worker for HunterWorker {
    fn name(&self) -> &str {
        "hunter"
    }

    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            // Install services so the workflow tasks can reach them. Production
            // installs once per process; tests reset between cases.
            set_services(self.services.clone());

            let mut ticker = tokio::time::interval(self.sweep_interval);
            // Skip the first immediate tick so a fresh start doesn't sweep at t=0
            // before the rest of the system has settled.
            ticker.tick().await;

            // Optional RSS fast pass on its own (shorter) cadence (SKADI-T-0192).
            let mut rss_ticker = self.rss_interval.map(tokio::time::interval);
            if let Some(t) = rss_ticker.as_mut() {
                t.tick().await; // skip the immediate t=0 tick too
            }

            // Manual "search all" trigger (SKADI-T-0193): a poke from `POST
            // /search-all` runs a sweep immediately, without waiting for the timer.
            let mut sweep_trigger = crate::trigger::subscribe();

            // The config plane is polled on its own short cadence rather than
            // inside the sweep arm (SKADI-T-0544), so a setting applies within
            // CONFIG_REFRESH_INTERVAL no matter how the sweep is tuned. Its
            // first tick is immediate, which is wanted here: the stored settings
            // should win from the start, not one refresh in.
            let mut config_ticker = tokio::time::interval(CONFIG_REFRESH_INTERVAL);
            // The periods currently installed on the two tickers above, so
            // `retune` can tell a real change from a no-op.
            let constructed = Intervals {
                sweep: self.sweep_interval,
                rss: self.rss_interval,
            };
            let mut live = constructed;

            // Re-read each tick; `None` until the first successful config read.
            let mut retention_days: Option<i64> = None;
            // `sweep_max_concurrent` was documented as config-plane tunable but
            // only ever came from the constructor argument, so setting it did
            // nothing (SKADI-T-0440). The stored value now wins when present;
            // the constructor value is the fallback.
            let mut live_concurrent: Option<usize> = None;

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = config_ticker.tick() => {
                        // Refresh the indexer circuit-breaker tuning from the config
                        // plane so `indexer_breaker_threshold` /
                        // `indexer_breaker_cooldown_secs` take effect live (SKADI-T-0308).
                        if let Ok(entries) = self.services.store.list_config().await {
                            let get = |k: &str| entries.iter().find(|e| e.key == k)
                                .and_then(|e| e.value.parse::<i64>().ok());
                            // Fallbacks match the registered config defaults.
                            let threshold = get("indexer_breaker_threshold").unwrap_or(3).max(0) as u32;
                            let cooldown = get("indexer_breaker_cooldown_secs").unwrap_or(300).max(0);
                            // The rate arm (SKADI-T-0568), refreshed on the same
                            // tick so it tunes live like the other two.
                            let rate_pct = get("indexer_breaker_rate_pct").unwrap_or(60).clamp(0, 100) as u32;
                            crate::pipeline::set_circuit_config_full(threshold, cooldown, rate_pct);
                            // Reachability-over-quality policy (SKADI-T-0598), live-tunable.
                            let prefer = entries
                                .iter()
                                .find(|e| e.key == "hunter.prefer_seeders")
                                .is_none_or(|e| {
                                    !matches!(
                                        e.value.trim().to_ascii_lowercase().as_str(),
                                        "false" | "0" | "no" | "off"
                                    )
                                });
                            let floor = get("hunter.seeder_floor_height").map_or(720, |h| h.clamp(0, 4320) as u16);
                            crate::pipeline::set_reachability_policy(prefer, floor);
                            // Retention is read the same way, so a change applies
                            // without a restart (SKADI-T-0440).
                            retention_days = get("history.retention_days").map(|d| d.max(0));
                            live_concurrent = get("sweep_max_concurrent")
                                // `0` would stop the sweep entirely, which is
                                // never what an operator means by a concurrency
                                // limit — floor it at 1.
                                .map(|c| usize::try_from(c.max(1)).unwrap_or(1));

                            // Interval retuning (SKADI-T-0544). Unlike the values
                            // above — which are merely *consulted* inside a tick —
                            // an interval decides when the tick happens, so a
                            // change means replacing the ticker.
                            let secs = |k: &str| get(k).and_then(|v| u64::try_from(v).ok());
                            let desired = Intervals::resolve(
                                secs("sweep_interval_secs"),
                                secs("rss_interval_secs"),
                                constructed,
                            );
                            if retune(&mut live.sweep, &mut ticker, desired.sweep) {
                                tracing::info!(secs = desired.sweep.as_secs(), "sweep interval retuned");
                            }
                            match (desired.rss, rss_ticker.as_mut()) {
                                // Period changed on a running fast pass.
                                (Some(d), Some(t)) => {
                                    let mut cur = live.rss.unwrap_or(d);
                                    if retune(&mut cur, t, d) {
                                        tracing::info!(secs = d.as_secs(), "RSS interval retuned");
                                    }
                                    live.rss = Some(cur);
                                }
                                // Turned on at runtime, where there was no ticker
                                // at all — `retune` cannot help, there is nothing
                                // to rebuild.
                                (Some(d), None) => {
                                    rss_ticker = Some(tokio::time::interval_at(
                                        tokio::time::Instant::now() + d,
                                        d,
                                    ));
                                    live.rss = Some(d);
                                    tracing::info!(secs = d.as_secs(), "RSS fast pass enabled");
                                }
                                // Turned off: drop the ticker so the select arm
                                // goes back to `pending()` forever.
                                (None, Some(_)) => {
                                    rss_ticker = None;
                                    live.rss = None;
                                    tracing::info!("RSS fast pass disabled");
                                }
                                (None, None) => {}
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        // Opportunistically purge lapsed blocklist entries (SKADI-T-0198)
                        // so expired blocks don't accumulate. Best-effort.
                        match skadi_store::BlocklistRepo::purge_expired_blocklist(&self.services.store).await {
                            Ok(n) if n > 0 => tracing::debug!(purged = n, "purged expired blocklist entries"),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "blocklist purge failed"),
                        }
                        // Age-based retention (SKADI-T-0199, made an operator
                        // setting in SKADI-T-0440). Best-effort, and applied to
                        // all three append-only tables — `trace_events` and
                        // `decision_history` previously grew forever, since every
                        // acquire run appends to them and nothing removed a row.
                        //
                        // `0` disables the purge entirely, for an operator who
                        // wants to keep everything and manage it themselves.
                        let days = retention_days.unwrap_or(DEFAULT_HISTORY_RETENTION_DAYS);
                        if days > 0 {
                            let cutoff = Utc::now() - chrono::Duration::days(days);
                            match skadi_store::HistoryRepo::purge_history_before(&self.services.store, cutoff).await {
                                Ok(n) if n > 0 => tracing::debug!(purged = n, "purged old history rows"),
                                Ok(_) => {}
                                Err(e) => tracing::warn!(error = %e, "history retention purge failed"),
                            }
                            match skadi_store::TraceRepo::purge_traces_before(&self.services.store, cutoff).await {
                                Ok(n) if n > 0 => tracing::debug!(purged = n, "purged old trace events"),
                                Ok(_) => {}
                                Err(e) => tracing::warn!(error = %e, "trace retention purge failed"),
                            }
                            match skadi_store::DecisionHistoryRepo::purge_decisions_before(&self.services.store, cutoff).await {
                                Ok(n) if n > 0 => tracing::debug!(purged = n, "purged old decision rows"),
                                Ok(_) => {}
                                Err(e) => tracing::warn!(error = %e, "decision retention purge failed"),
                            }
                        }
                        let concurrent = live_concurrent.unwrap_or(self.max_concurrent);
                        if let Err(e) = sweep_once_detached(Arc::clone(&self.runner), self.query.as_ref(), concurrent).await {
                            tracing::warn!(error = %e, "hunter sweep failed");
                        }
                    }
                    _ = sweep_trigger.changed() => {
                        tracing::info!("manual sweep requested (/search-all)");
                        let concurrent = live_concurrent.unwrap_or(self.max_concurrent);
                        if let Err(e) = sweep_once_detached(Arc::clone(&self.runner), self.query.as_ref(), concurrent).await {
                            tracing::warn!(error = %e, "manual hunter sweep failed");
                        }
                    }
                    // An RSS tick fires only when enabled; `pending()` makes this
                    // arm never ready when `rss_ticker` is `None`.
                    _ = async {
                        match rss_ticker.as_mut() {
                            Some(t) => { t.tick().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        match rss_sweep_detached(
                            Arc::clone(&self.runner),
                            &self.services.indexers,
                            self.query.as_ref(),
                            self.services.kind,
                            live_concurrent.unwrap_or(self.max_concurrent),
                        ).await {
                            Ok(n) if n > 0 => tracing::info!(started = n, "RSS fast pass triggered acquisitions"),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "hunter RSS pass failed"),
                        }
                    }
                }
            }

            if let Err(e) = self.runner.shutdown().await {
                tracing::warn!(error = %e, "hunter runner shutdown failed");
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::{ExternalIds, MediaKind};
    use skadi_indexers::Category;

    /// A stub query that returns a configurable number of seeds, all with the
    /// same `acquirable` (so tests don't need to thread distinct refs through).
    struct StubQuery {
        seeds: Vec<AcquireSeed>,
    }

    impl StubQuery {
        fn with_count(n: usize, profile: ProfileId) -> Self {
            let seeds = (0..n)
                .map(|i| AcquireSeed {
                    acquirable: AcquirableRef(format!("ed-{i}")),
                    request: SearchSpec {
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
                    profile,
                    current_quality: None,
                    current_format_score: None,
                    current_unplayable: false,
                })
                .collect();
            Self { seeds }
        }
    }

    #[async_trait]
    impl WantedQuery for StubQuery {
        async fn wanted(&self) -> Result<Vec<AcquireSeed>> {
            Ok(self.seeds.clone())
        }
    }

    #[test]
    fn acquire_seed_round_trip() {
        // An upgrade seed: `current_quality` is Some and must survive the
        // serialize-into-context round trip (SKADI-T-0182).
        let held = skadi_core::QualityId::new();
        let seed = AcquireSeed {
            acquirable: AcquirableRef("ed-1".into()),
            request: SearchSpec {
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
            profile: ProfileId::new(),
            current_quality: Some(held),
            current_format_score: Some(125),
            current_unplayable: false,
        };
        let ctx = seed.clone().into_context(None).unwrap();
        let loaded = crate::state::load_state(&ctx).unwrap();
        assert_eq!(loaded.current_quality, Some(held));
        assert_eq!(loaded.current_format_score, Some(125));
        assert_eq!(loaded.acquirable, seed.acquirable);
        assert_eq!(loaded.profile, seed.profile);
        assert_eq!(loaded.request.titles, seed.request.titles);
    }

    #[test]
    fn resolve_falls_back_and_treats_the_two_zeroes_differently() {
        let fallback = Intervals {
            sweep: Duration::from_secs(900),
            rss: Some(Duration::from_secs(60)),
        };

        // Nothing stored ⇒ exactly what the worker was constructed with.
        assert_eq!(Intervals::resolve(None, None, fallback), fallback);

        // Stored values win.
        assert_eq!(
            Intervals::resolve(Some(300), Some(30), fallback),
            Intervals {
                sweep: Duration::from_secs(300),
                rss: Some(Duration::from_secs(30)),
            }
        );

        // `0` sweep is nonsense — a zero interval spins continuously against
        // every indexer — so it falls back rather than being honoured.
        assert_eq!(
            Intervals::resolve(Some(0), None, fallback).sweep,
            fallback.sweep
        );

        // `0` RSS means off, which is a supported configuration. This asymmetry
        // is the point: "never sweep" would break the daemon, "never RSS" is a
        // choice.
        assert_eq!(Intervals::resolve(None, Some(0), fallback).rss, None);

        // A worker constructed without RSS stays without it when nothing is set.
        let no_rss = Intervals {
            rss: None,
            ..fallback
        };
        assert_eq!(Intervals::resolve(None, None, no_rss).rss, None);
    }

    #[tokio::test]
    async fn retune_rebuilds_only_on_change_and_never_fires_immediately() {
        tokio::time::pause();

        let mut period = Duration::from_secs(900);
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await; // consume the immediate t=0 tick

        // Same value ⇒ no rebuild. This is the assertion that matters: a
        // config refresh that rebuilt unconditionally would reset the phase
        // every 30s and a 900s sweep would then never fire at all.
        assert!(!retune(&mut period, &mut ticker, Duration::from_secs(900)));

        // A real change rebuilds.
        assert!(retune(&mut period, &mut ticker, Duration::from_secs(60)));
        assert_eq!(period, Duration::from_secs(60));

        // ...and the new ticker does not fire immediately — the change takes
        // effect from the next scheduled fire, so saving a settings form does
        // not itself trigger a sweep.
        assert!(
            tokio::time::timeout(Duration::from_secs(59), ticker.tick())
                .await
                .is_err(),
            "a retuned ticker fired before its first full period elapsed"
        );
        // But it does fire once the new period is up.
        tokio::time::timeout(Duration::from_secs(2), ticker.tick())
            .await
            .expect("the retuned ticker should fire after its period");
    }

    #[tokio::test]
    async fn wanted_query_default_upgradable_is_empty() {
        let q = StubQuery::with_count(3, ProfileId::new());
        assert_eq!(q.wanted().await.unwrap().len(), 3);
        assert!(q.upgradable().await.unwrap().is_empty());
    }
}
