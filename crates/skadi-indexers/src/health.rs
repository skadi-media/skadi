//! Per-indexer health telemetry (SKADI-T-0204).
//!
//! A process-global, in-memory registry of each indexer's recent search outcomes —
//! last success/failure, cumulative counts, consecutive-failure streak, and the last
//! error — so the API's `/indexers/health` view reaches Prowlarr's "Indexer Health"
//! parity. Recorded transparently by a [`HealthTracked`] decorator applied at the
//! same build seam as the rate limiter (SKADI-T-0189), so every call path (hunter
//! sweep, RSS, manual `/search`, health checks) feeds it without caller opt-in.
//!
//! In-memory only (like the hunter's in-flight tracker): a restart clears it and the
//! next searches repopulate it. It's observability, not a source of truth — the
//! `consecutive_failures`/`healthy` signal is advisory, not an auto-disable (which
//! would belong with the decision/config layer).

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use skadi_core::{IndexerId, MediaKind, Protocol, Result};

use crate::{Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery};

/// Recent health of one indexer.
#[derive(Clone, Debug, Default, Serialize)]
pub struct IndexerHealth {
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
    pub success_count: u64,
    pub failure_count: u64,
    /// Failures since the last success — the "is it down right now" signal.
    pub consecutive_failures: u32,
    /// Outcomes of the last up-to-32 calls as a bitmask — bit 0 is the most
    /// recent, `1` means failure (SKADI-T-0568).
    ///
    /// A **window**, because the lifetime counts below cannot express "failing
    /// now": an indexer healthy for ten thousand calls would need thousands of
    /// failures to move its lifetime rate, by which point the operator has long
    /// since noticed. A `u32` rather than a `VecDeque` so this stays `Copy` and
    /// allocation-free on the search hot path.
    ///
    /// `pub` deliberately: every field here is, and `skadi-hunter` builds this
    /// struct literally in its tests — a private field breaks that with E0451,
    /// which is how the first attempt at this was caught.
    #[serde(skip)]
    pub recent_outcomes: u32,
    /// How many of those 32 slots are filled (saturating).
    #[serde(skip)]
    pub recent_len: u8,
    /// The most recent error message, if the last call failed.
    pub last_error: Option<String>,
    /// Fetches this indexer served **without** hitting a challenge
    /// (SKADI-T-0552).
    pub direct_count: u64,
    /// Fetches that hit a challenge and were solved by FlareSolverr.
    pub solved_count: u64,
    /// Fetches that hit a challenge nothing could solve — the solver was
    /// unconfigured, unreachable, or failed.
    pub unsolved_count: u64,
}

impl IndexerHealth {
    /// Failure rate over the recent window, or `None` below
    /// [`MIN_RATE_SAMPLES`] (SKADI-T-0568).
    ///
    /// `None` rather than `0.0` for a small sample: an indexer that has failed
    /// its only two calls is not yet evidence of anything, and treating a thin
    /// sample as a rate would rest a brand-new indexer on a bad first minute.
    #[must_use]
    pub fn recent_failure_rate(&self) -> Option<f32> {
        if usize::from(self.recent_len) < MIN_RATE_SAMPLES {
            return None;
        }
        let n = u32::from(self.recent_len);
        let failures = (self.recent_outcomes & mask(self.recent_len)).count_ones();
        Some(failures as f32 / n as f32)
    }

    /// Whether the indexer looks healthy: no calls yet, or the last call succeeded.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.consecutive_failures == 0
    }
}

/// How a single fetch was served (SKADI-T-0552).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SolvePath {
    /// The tracker answered normally.
    Direct,
    /// A challenge was returned and FlareSolverr solved it.
    Solved,
    /// A challenge was returned and nothing solved it.
    Unsolved,
}

/// One indexer's health, tagged with its id (the `/indexers/health` row).
#[derive(Clone, Debug, Serialize)]
pub struct IndexerHealthSnapshot {
    pub indexer: IndexerId,
    pub healthy: bool,
    #[serde(flatten)]
    pub health: IndexerHealth,
}

/// Calls needed before [`IndexerHealth::recent_failure_rate`] reports anything
/// (SKADI-T-0568). Below this a "rate" is noise, and resting an indexer on it
/// would punish a new one for a bad first minute.
pub const MIN_RATE_SAMPLES: usize = 10;

/// Window size for the recent-outcome bitmask — the width of a `u32`.
pub const RATE_WINDOW: usize = 32;

/// Bits in use for `len` filled slots.
fn mask(len: u8) -> u32 {
    if usize::from(len) >= RATE_WINDOW {
        u32::MAX
    } else {
        (1u32 << len) - 1
    }
}

/// Push one outcome into the window, most-recent-first.
fn push_outcome(h: &mut IndexerHealth, failed: bool) {
    h.recent_outcomes = (h.recent_outcomes << 1) | u32::from(failed);
    h.recent_len = h.recent_len.saturating_add(1).min(RATE_WINDOW as u8);
}

/// The process-global per-indexer health registry.
#[derive(Default)]
pub struct IndexerHealthRegistry {
    by_id: RwLock<HashMap<IndexerId, IndexerHealth>>,
}

impl IndexerHealthRegistry {
    /// Record a successful network call: bumps success, clears the failure streak.
    pub fn record_success(&self, id: IndexerId) {
        if let Ok(mut m) = self.by_id.write() {
            let h = m.entry(id).or_default();
            h.last_success = Some(Utc::now());
            h.success_count += 1;
            h.consecutive_failures = 0;
            h.last_error = None;
            push_outcome(h, false);
        }
    }

    /// Record a failed network call with its error message.
    pub fn record_failure(&self, id: IndexerId, error: impl Into<String>) {
        if let Ok(mut m) = self.by_id.write() {
            let h = m.entry(id).or_default();
            h.last_failure = Some(Utc::now());
            h.failure_count += 1;
            h.consecutive_failures = h.consecutive_failures.saturating_add(1);
            h.last_error = Some(error.into());
            push_outcome(h, true);
        }
    }

    /// Undo one recorded failure's contribution to the consecutive-failure streak —
    /// for when a failure was a **shared-infra outage** (every queried indexer failed
    /// at once, e.g. the VPN/proxy was down) rather than the indexer's own fault, so
    /// the search circuit breaker shouldn't trip it (SKADI-T-0308). Decrements the
    /// streak (saturating); counts + timestamps are left untouched.
    pub fn forgive(&self, id: IndexerId) {
        if let Ok(mut m) = self.by_id.write()
            && let Some(h) = m.get_mut(&id)
        {
            h.consecutive_failures = h.consecutive_failures.saturating_sub(1);
        }
    }

    /// Record how a fetch was served (SKADI-T-0552).
    ///
    /// This is the measurement that makes the "can we drop the FlareSolverr
    /// container" question answerable with evidence instead of a guess about
    /// someone else's trackers: an indexer whose `solved_count` stays zero over
    /// real use never needed the solver, and one whose `unsolved_count` climbs
    /// is failing in a way the current setup cannot fix.
    pub fn record_path(&self, id: IndexerId, path: SolvePath) {
        if let Ok(mut m) = self.by_id.write() {
            let h = m.entry(id).or_default();
            match path {
                SolvePath::Direct => h.direct_count += 1,
                SolvePath::Solved => h.solved_count += 1,
                SolvePath::Unsolved => h.unsolved_count += 1,
            }
        }
    }

    /// A snapshot of every tracked indexer's health.
    #[must_use]
    pub fn snapshot(&self) -> Vec<IndexerHealthSnapshot> {
        self.by_id
            .read()
            .map(|m| {
                m.iter()
                    .map(|(id, h)| IndexerHealthSnapshot {
                        indexer: *id,
                        healthy: h.healthy(),
                        health: h.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Health for one indexer, if it has been tracked.
    #[must_use]
    pub fn get(&self, id: IndexerId) -> Option<IndexerHealth> {
        self.by_id.read().ok().and_then(|m| m.get(&id).cloned())
    }
}

static REGISTRY: OnceLock<IndexerHealthRegistry> = OnceLock::new();

/// The process-global indexer-health registry.
pub fn indexer_health() -> &'static IndexerHealthRegistry {
    REGISTRY.get_or_init(IndexerHealthRegistry::default)
}

/// An [`Indexer`] decorator that records each network call's outcome into the
/// global [`indexer_health`] registry **and** emits a structured `tracing` event
/// per call (indexer id, op, latency, result count or error cause — SKADI-T-0204 /
/// SKADI-T-0207). Local methods pass straight through.
pub struct HealthTracked {
    inner: Box<dyn Indexer>,
}

impl HealthTracked {
    #[must_use]
    pub fn new(inner: Box<dyn Indexer>) -> Self {
        Self { inner }
    }

    /// Record health + emit one structured observability event for a completed
    /// call. `count` is the result length for search/rss (`None` for `test`).
    fn finish<T>(
        &self,
        op: &'static str,
        start: std::time::Instant,
        count: Option<usize>,
        r: Result<T>,
    ) -> Result<T> {
        let id = self.inner.id();
        let latency_ms = start.elapsed().as_millis();
        match &r {
            Ok(_) => {
                indexer_health().record_success(id);
                tracing::debug!(indexer = %id, op, latency_ms, count = count.unwrap_or(0), "indexer call ok");
            }
            Err(e) => {
                indexer_health().record_failure(id, e.to_string());
                tracing::warn!(indexer = %id, op, latency_ms, error = %e, "indexer call failed");
            }
        }
        r
    }
}

#[async_trait]
impl Indexer for HealthTracked {
    // Forward the per-indexer flags (SKADI-T-0505): a decorator that swallowed
    // them would silently re-enable an indexer the operator turned off, since
    // every built indexer is wrapped.
    fn enable_rss(&self) -> bool {
        self.inner.enable_rss()
    }

    fn enable_automatic_search(&self) -> bool {
        self.inner.enable_automatic_search()
    }

    // Forwarded, not defaulted (SKADI-T-0556). Production wraps every indexer in
    // these decorators, so a method left to the trait default here makes tag
    // scoping a silent no-op: the flag is stored, the UI shows it, and nothing
    // filters.
    fn applies_to_tags(&self, item_tags: &[String]) -> bool {
        self.inner.applies_to_tags(item_tags)
    }

    fn priority(&self) -> u32 {
        self.inner.priority()
    }

    fn minimum_seeders(&self) -> u32 {
        self.inner.minimum_seeders()
    }

    fn id(&self) -> IndexerId {
        self.inner.id()
    }
    fn protocol(&self) -> Protocol {
        self.inner.protocol()
    }
    fn supports(&self, kind: MediaKind) -> bool {
        self.inner.supports(kind)
    }
    async fn capabilities(&self) -> Result<IndexerCaps> {
        self.inner.capabilities().await
    }
    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        let start = std::time::Instant::now();
        let r = self.inner.search(query).await;
        let count = r.as_ref().ok().map(Vec::len);
        self.finish("search", start, count, r)
    }
    async fn rss(&self) -> Result<Vec<Release>> {
        let start = std::time::Instant::now();
        let r = self.inner.rss().await;
        let count = r.as_ref().ok().map(Vec::len);
        self.finish("rss", start, count, r)
    }
    async fn test(&self) -> Result<()> {
        let start = std::time::Instant::now();
        let r = self.inner.test().await;
        self.finish("test", start, None, r)
    }
    async fn resolve_fetch(&self, fetch: &ReleaseFetch) -> Result<ReleaseFetch> {
        // Delegate — a defaulted trait method would otherwise no-op here and shield
        // the inner indexer's real resolver (e.g. cardigann's download block,
        // SKADI-T-0306). Not routed through `finish()`: a grab-time resolve failure
        // must not trip the search-health circuit.
        self.inner.resolve_fetch(fetch).await
    }
}

/// Wrap `inner` so its search/rss/test outcomes feed the health registry. The
/// single seam the provider factory uses (applied outside the rate limiter so it
/// records the *real* search result, post-throttle).
#[must_use]
pub fn indexer_health_tracked(inner: Box<dyn Indexer>) -> Box<dyn Indexer> {
    Box::new(HealthTracked::new(inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubIndexer;

    #[tokio::test]
    async fn records_success_and_failure_streaks() {
        let reg = IndexerHealthRegistry::default();
        let id = IndexerId::new();

        // Two successes then two failures.
        reg.record_success(id);
        reg.record_success(id);
        let h = reg.get(id).unwrap();
        assert_eq!(h.success_count, 2);
        assert_eq!(h.consecutive_failures, 0);
        assert!(h.healthy());
        assert!(h.last_success.is_some());

        reg.record_failure(id, "boom");
        reg.record_failure(id, "boom again");
        let h = reg.get(id).unwrap();
        assert_eq!(h.failure_count, 2);
        assert_eq!(h.consecutive_failures, 2);
        assert!(!h.healthy());
        assert_eq!(h.last_error.as_deref(), Some("boom again"));

        // A success clears the streak.
        reg.record_success(id);
        let h = reg.get(id).unwrap();
        assert_eq!(h.consecutive_failures, 0);
        assert!(h.healthy());
        assert!(h.last_error.is_none());

        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].indexer, id);
        assert!(snap[0].healthy);

        // forgive() decrements the streak (shared-outage undo), saturating at 0,
        // without recording a success (SKADI-T-0308).
        reg.record_failure(id, "a");
        reg.record_failure(id, "b");
        assert_eq!(reg.get(id).unwrap().consecutive_failures, 2);
        reg.forgive(id);
        assert_eq!(reg.get(id).unwrap().consecutive_failures, 1);
        reg.forgive(id);
        reg.forgive(id); // already 0 → stays 0
        assert_eq!(reg.get(id).unwrap().consecutive_failures, 0);
    }

    #[tokio::test]
    async fn decorator_delegates_resolve_fetch_to_inner() {
        // Regression (SKADI-T-0306): `resolve_fetch` is a *defaulted* trait method, so
        // a decorator that doesn't explicitly delegate silently no-ops it — shielding
        // the inner indexer's real resolver (cardigann's download block). Prove the
        // wrapped call reaches the inner.
        struct Resolver;
        #[async_trait::async_trait]
        impl Indexer for Resolver {
            fn id(&self) -> IndexerId {
                IndexerId::new()
            }
            fn protocol(&self) -> Protocol {
                Protocol::Torrent
            }
            fn supports(&self, _kind: MediaKind) -> bool {
                true
            }
            async fn capabilities(&self) -> Result<IndexerCaps> {
                unreachable!("not exercised")
            }
            async fn search(&self, _query: &dyn SearchQuery) -> Result<Vec<Release>> {
                Ok(Vec::new())
            }
            async fn test(&self) -> Result<()> {
                Ok(())
            }
            async fn resolve_fetch(&self, _fetch: &ReleaseFetch) -> Result<ReleaseFetch> {
                Ok(ReleaseFetch::Magnet("magnet:?xt=urn:btih:deadbeef".into()))
            }
        }
        let tracked = indexer_health_tracked(Box::new(Resolver));
        let out = tracked
            .resolve_fetch(&ReleaseFetch::TorrentUrl("https://page/x".into()))
            .await
            .unwrap();
        assert_eq!(
            out,
            ReleaseFetch::Magnet("magnet:?xt=urn:btih:deadbeef".into()),
            "decorator must delegate resolve_fetch, not no-op it"
        );
    }

    #[tokio::test]
    async fn decorator_records_search_outcome_into_global_registry() {
        // The stub matches "Sintel" for movies; an unknown title returns empty (Ok).
        let id = IndexerId::new();
        let tracked = indexer_health_tracked(Box::new(StubIndexer::new(id)));

        struct Q(String);
        impl SearchQuery for Q {
            fn kind(&self) -> MediaKind {
                MediaKind::Movie
            }
            fn titles(&self) -> &[String] {
                std::slice::from_ref(&self.0)
            }
            fn year(&self) -> Option<u16> {
                None
            }
            fn external_ids(&self) -> &skadi_core::ExternalIds {
                static IDS: std::sync::OnceLock<skadi_core::ExternalIds> =
                    std::sync::OnceLock::new();
                IDS.get_or_init(skadi_core::ExternalIds::default)
            }
            fn categories(&self) -> &[crate::Category] {
                &[]
            }
        }

        // Hold a binding so the query outlives the borrow.
        let q = Q("Sintel".to_string());
        tracked.search(&q).await.unwrap();
        let h = indexer_health().get(id).expect("recorded");
        assert_eq!(h.success_count, 1, "a successful search is recorded");
        assert!(h.healthy());
    }
}

#[cfg(test)]
mod solve_path_tests {
    use super::*;

    /// The counters exist to answer "can the FlareSolverr container go?" with
    /// evidence rather than a guess about someone else's trackers
    /// (SKADI-T-0552), so they have to tell the three outcomes apart.
    #[test]
    fn the_three_paths_are_counted_separately() {
        let reg = IndexerHealthRegistry::default();
        let id = IndexerId::new();

        reg.record_path(id, SolvePath::Direct);
        reg.record_path(id, SolvePath::Direct);
        reg.record_path(id, SolvePath::Solved);
        reg.record_path(id, SolvePath::Unsolved);

        let snap = reg.snapshot();
        let row = snap.iter().find(|s| s.indexer == id).expect("tracked");
        assert_eq!(row.health.direct_count, 2);
        assert_eq!(row.health.solved_count, 1);
        assert_eq!(row.health.unsolved_count, 1);
    }

    #[test]
    fn counting_a_path_does_not_change_the_healthy_verdict() {
        // The paths are diagnostics, not failures: a tracker that needs solving
        // on every request is working, just expensively. Conflating the two
        // would trip the search circuit breaker (SKADI-T-0308) on a healthy
        // indexer.
        let reg = IndexerHealthRegistry::default();
        let id = IndexerId::new();
        reg.record_path(id, SolvePath::Unsolved);
        let snap = reg.snapshot();
        let row = snap.iter().find(|s| s.indexer == id).expect("tracked");
        assert!(
            row.healthy,
            "a solve path must not mark the indexer unhealthy"
        );
        assert_eq!(row.health.failure_count, 0);
    }

    #[test]
    fn an_indexer_that_never_challenges_reports_only_direct() {
        // This is the reading that would justify dropping the container: an
        // indexer whose solved_count stays zero over real use never needed it.
        let reg = IndexerHealthRegistry::default();
        let id = IndexerId::new();
        for _ in 0..5 {
            reg.record_path(id, SolvePath::Direct);
        }
        let snap = reg.snapshot();
        let row = snap.iter().find(|s| s.indexer == id).expect("tracked");
        assert_eq!(row.health.direct_count, 5);
        assert_eq!(row.health.solved_count, 0);
        assert_eq!(row.health.unsolved_count, 0);
    }
}

#[cfg(test)]
mod failure_rate_tests {
    use super::*;

    fn with(outcomes: &[bool]) -> IndexerHealth {
        let mut h = IndexerHealth::default();
        for &failed in outcomes {
            push_outcome(&mut h, failed);
        }
        h
    }

    #[test]
    fn a_thin_sample_has_no_rate() {
        // An indexer that failed its only two calls is not yet evidence.
        // Reporting a rate here would rest a new indexer on a bad first minute.
        assert_eq!(with(&[true, true]).recent_failure_rate(), None);
        assert_eq!(
            with(&[true; MIN_RATE_SAMPLES - 1]).recent_failure_rate(),
            None
        );
        assert!(
            with(&[true; MIN_RATE_SAMPLES])
                .recent_failure_rate()
                .is_some()
        );
    }

    #[test]
    fn the_production_shape_is_detected() {
        // The case the streak arm cannot see (SKADI-T-0568): failures and
        // successes interleaved, so `consecutive_failures` never reaches 3, but
        // the indexer fails more often than it works.
        let alternating: Vec<bool> = (0..20).map(|i| i % 2 == 0).collect();
        let h = with(&alternating);
        assert!(h.consecutive_failures == 0 || h.consecutive_failures < 3);
        assert_eq!(h.recent_failure_rate(), Some(0.5));

        // And the real ratio observed: 213 failures to 172 successes ≈ 55 %.
        let mostly: Vec<bool> = (0..20).map(|i| i % 20 < 11).collect();
        assert!(mostly.iter().filter(|f| **f).count() == 11);
        assert_eq!(with(&mostly).recent_failure_rate(), Some(11.0 / 20.0));
    }

    #[test]
    fn a_healthy_indexer_with_the_odd_blip_stays_low() {
        // Must not rest something that works: one failure in twenty is 5 %.
        let mut v = vec![false; 19];
        v.push(true);
        assert_eq!(with(&v).recent_failure_rate(), Some(0.05));
    }

    #[test]
    fn the_window_forgets_so_recovery_is_visible() {
        // The whole reason for a window rather than the lifetime counters: an
        // indexer that was failing and now works must stop looking broken.
        let mut h = with(&[true; RATE_WINDOW]);
        assert_eq!(h.recent_failure_rate(), Some(1.0));
        for _ in 0..RATE_WINDOW {
            push_outcome(&mut h, false);
        }
        assert_eq!(
            h.recent_failure_rate(),
            Some(0.0),
            "a full window of successes must clear it"
        );
    }

    #[test]
    fn the_window_is_bounded_and_counts_only_filled_slots() {
        let h = with(&[true; RATE_WINDOW * 3]);
        assert_eq!(usize::from(h.recent_len), RATE_WINDOW, "len saturates");
        assert_eq!(h.recent_failure_rate(), Some(1.0));

        // Exactly the window, half failing.
        let half: Vec<bool> = (0..RATE_WINDOW).map(|i| i % 2 == 0).collect();
        assert_eq!(with(&half).recent_failure_rate(), Some(0.5));
    }

    #[test]
    fn recording_through_the_registry_feeds_the_window() {
        // The fields are only useful if the real record paths fill them.
        let (r, id) = (IndexerHealthRegistry::default(), IndexerId::new());
        for _ in 0..MIN_RATE_SAMPLES {
            r.record_failure(id, "blocked");
        }
        let h = r.get(id).expect("recorded");
        assert_eq!(h.recent_failure_rate(), Some(1.0));

        r.record_success(id);
        let h = r.get(id).expect("recorded");
        assert!(h.recent_failure_rate().unwrap() < 1.0, "a success moves it");
        assert_eq!(h.consecutive_failures, 0);
    }
}
