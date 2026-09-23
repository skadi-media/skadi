//! Per-indexer request rate limiting (SKADI-T-0189).
//!
//! Public trackers — and the Prowlarr/Jackett proxies in front of them — rate-limit
//! aggressively: too many searches in a short window earns a `429`, a CloudFlare
//! challenge, or (the deploy reality) a churned VPN session. A sweep over a large
//! library fans many searches at the *same* indexer back-to-back, so without a
//! throttle the first real backlog run trips every limiter at once.
//!
//! This module gives each configured indexer its **own** token bucket and wraps it
//! in a [`RateLimited`] decorator that gates the network methods
//! (`search`/`rss`/`capabilities`/`test`) — `id`/`protocol`/`supports` are local and
//! stay unthrottled. The decorator is applied transparently in
//! [`IndexerConfig::build`](crate::IndexerConfig::build), so *every* call path (the
//! hunter sweep, the interactive `…/releases` search, health checks) is limited
//! without any caller opting in.
//!
//! The bucket itself ([`TokenBucket`]) is a clock-agnostic state machine (it takes
//! the current time as an argument) so its refill/burst arithmetic is unit-tested
//! deterministically; [`RateLimiter`] drives it with [`tokio::time`], which means
//! `tokio::time::pause()` makes the *async* spacing deterministic in tests too.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::time::{Duration, Instant, sleep};

use skadi_core::{IndexerId, MediaKind, Protocol, Result};

use crate::{Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery};

/// A clock-agnostic token bucket. One token is one allowed request; the bucket
/// refills continuously at `refill_per_sec` up to `capacity` (the burst size).
///
/// Time is passed in as seconds-since-some-fixed-origin so the logic is testable
/// without a real clock — [`RateLimiter`] supplies it from a monotonic
/// [`tokio::time::Instant`].
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_secs: f64,
}

impl TokenBucket {
    /// A bucket holding `capacity` tokens (full to start, so the first `capacity`
    /// requests burst through), refilling at `refill_per_sec`. `now_secs` is the
    /// origin timestamp.
    #[must_use]
    pub fn new(capacity: f64, refill_per_sec: f64, now_secs: f64) -> Self {
        Self {
            capacity: capacity.max(1.0),
            refill_per_sec: refill_per_sec.max(f64::MIN_POSITIVE),
            tokens: capacity.max(1.0),
            last_secs: now_secs,
        }
    }

    /// Attempt to take one token at `now_secs`. Refills first based on elapsed
    /// time. Returns `None` if a token was available (and consumed), or
    /// `Some(wait)` — the time to wait before one will be — if not (no token is
    /// consumed on failure).
    pub fn take(&mut self, now_secs: f64) -> Option<Duration> {
        // Refill for the elapsed interval (guard against a non-monotonic clock).
        let elapsed = (now_secs - self.last_secs).max(0.0);
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last_secs = now_secs;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let deficit = 1.0 - self.tokens;
            Some(Duration::from_secs_f64(deficit / self.refill_per_sec))
        }
    }
}

/// An async per-indexer limiter: a [`TokenBucket`] behind a mutex, driven by a
/// monotonic [`tokio::time::Instant`]. [`acquire`](Self::acquire) blocks until a
/// token is free, sleeping the bucket-computed wait between attempts.
#[derive(Debug)]
pub struct RateLimiter {
    bucket: Mutex<TokenBucket>,
    origin: Instant,
}

impl RateLimiter {
    /// A limiter allowing `per_minute` requests/min steady-state with a burst of
    /// `burst`. `per_minute` is clamped to ≥1 (a zero rate would never refill;
    /// callers that want "no limit" must not wrap at all — see
    /// [`indexer_rate_limited`]).
    #[must_use]
    pub fn per_minute(per_minute: u32, burst: u32) -> Self {
        let refill_per_sec = f64::from(per_minute.max(1)) / 60.0;
        let capacity = f64::from(burst.max(1));
        Self {
            bucket: Mutex::new(TokenBucket::new(capacity, refill_per_sec, 0.0)),
            origin: Instant::now(),
        }
    }

    fn now_secs(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    /// Block until a request token is available, then consume it.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut b = self.bucket.lock().expect("rate-limiter mutex poisoned");
                b.take(self.now_secs())
            };
            match wait {
                None => return,
                // Add a hair so the post-sleep `take` is past the refill threshold
                // rather than landing a nanosecond short and re-sleeping.
                Some(d) => sleep(d + Duration::from_millis(1)).await,
            }
        }
    }
}

/// An [`Indexer`] decorator that gates the network methods through a
/// [`RateLimiter`]. Local methods (`id`/`protocol`/`supports`) pass straight
/// through.
pub struct RateLimited {
    inner: Box<dyn Indexer>,
    limiter: Arc<RateLimiter>,
}

impl RateLimited {
    /// Wrap `inner`, throttling it to `per_minute` requests/min with `burst`.
    #[must_use]
    pub fn new(inner: Box<dyn Indexer>, per_minute: u32, burst: u32) -> Self {
        Self {
            inner,
            limiter: Arc::new(RateLimiter::per_minute(per_minute, burst)),
        }
    }
}

#[async_trait]
impl Indexer for RateLimited {
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
        self.limiter.acquire().await;
        self.inner.capabilities().await
    }
    async fn search(&self, query: &dyn SearchQuery) -> Result<Vec<Release>> {
        self.limiter.acquire().await;
        self.inner.search(query).await
    }
    async fn rss(&self) -> Result<Vec<Release>> {
        self.limiter.acquire().await;
        self.inner.rss().await
    }
    async fn test(&self) -> Result<()> {
        self.limiter.acquire().await;
        self.inner.test().await
    }
    async fn resolve_fetch(&self, fetch: &ReleaseFetch) -> Result<ReleaseFetch> {
        // Resolve may fetch a detail page (cardigann download block, SKADI-T-0306),
        // so it's a rate-limited network call like the others; delegate to the inner
        // resolver rather than the defaulted no-op that would shield it.
        self.limiter.acquire().await;
        self.inner.resolve_fetch(fetch).await
    }
}

/// Default steady-state request rate (per minute) applied to a network indexer
/// when its config doesn't specify one. Deliberately gentle: an *arr replacement
/// hammering a public tracker is how accounts get banned and VPN sessions get
/// churned (the deploy reality). Operators can raise it per-indexer.
pub const DEFAULT_RATE_PER_MINUTE: u32 = 60;
/// Default burst (bucket capacity): a handful of back-to-back requests are fine;
/// a *sweep* of dozens is not.
pub const DEFAULT_BURST: u32 = 5;

/// Wrap `inner` in a [`RateLimited`] decorator unless `per_minute == 0`, which
/// means "no limit" — then `inner` is returned untouched. The single seam the
/// provider factory uses so the policy lives in one place.
#[must_use]
pub fn indexer_rate_limited(inner: Box<dyn Indexer>, per_minute: u32) -> Box<dyn Indexer> {
    if per_minute == 0 {
        inner
    } else {
        Box::new(RateLimited::new(inner, per_minute, DEFAULT_BURST))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- TokenBucket: pure, deterministic, no clock ---

    #[test]
    fn full_bucket_bursts_then_throttles() {
        // capacity 3, refill 1/sec.
        let mut b = TokenBucket::new(3.0, 1.0, 0.0);
        // Three immediate takes succeed (the burst), all at t=0.
        assert!(b.take(0.0).is_none());
        assert!(b.take(0.0).is_none());
        assert!(b.take(0.0).is_none());
        // Fourth at t=0 fails and asks to wait ~1s (one token / 1 per sec).
        let wait = b.take(0.0).expect("should throttle");
        assert!(
            (wait.as_secs_f64() - 1.0).abs() < 1e-6,
            "expected ~1s wait, got {wait:?}"
        );
    }

    #[test]
    fn refills_over_time_and_caps_at_capacity() {
        let mut b = TokenBucket::new(2.0, 1.0, 0.0);
        // Drain the burst.
        assert!(b.take(0.0).is_none());
        assert!(b.take(0.0).is_none());
        assert!(b.take(0.0).is_some());
        // After 1s, exactly one token refilled → one success, next throttles.
        assert!(b.take(1.0).is_none());
        assert!(b.take(1.0).is_some());
        // Idle for 100s: tokens cap at capacity (2), not 100.
        assert!(b.take(101.0).is_none());
        assert!(b.take(101.0).is_none());
        assert!(b.take(101.0).is_some(), "must not exceed capacity");
    }

    #[test]
    fn non_monotonic_clock_does_not_overfill_or_panic() {
        let mut b = TokenBucket::new(1.0, 1.0, 10.0);
        assert!(b.take(10.0).is_none());
        // Clock jumps backwards: treated as zero elapsed (no refill), still safe.
        let wait = b.take(5.0).expect("no token yet");
        assert!(wait.as_secs_f64() > 0.0);
    }

    #[test]
    fn wait_scales_inversely_with_rate() {
        // 120/min = 2/sec → empty-bucket wait is ~0.5s.
        let mut b = TokenBucket::new(1.0, 2.0, 0.0);
        assert!(b.take(0.0).is_none());
        let wait = b.take(0.0).unwrap();
        assert!(
            (wait.as_secs_f64() - 0.5).abs() < 1e-6,
            "expected ~0.5s, got {wait:?}"
        );
    }

    // --- RateLimiter / RateLimited: async spacing under paused time ---

    struct CountingIndexer {
        id: IndexerId,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Indexer for CountingIndexer {
        fn id(&self) -> IndexerId {
            self.id
        }
        fn protocol(&self) -> Protocol {
            Protocol::Torrent
        }
        fn supports(&self, kind: MediaKind) -> bool {
            kind == MediaKind::Movie
        }
        async fn capabilities(&self) -> Result<IndexerCaps> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(IndexerCaps {
                supports_rss: true,
                supports_search: true,
                id_params: std::collections::BTreeSet::new(),
                supports_aggregate_ids: false,
                text_search: crate::TextSearch::Raw,
                categories: vec![],
            })
        }
        async fn search(&self, _q: &dyn SearchQuery) -> Result<Vec<Release>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        }
        async fn rss(&self) -> Result<Vec<Release>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        }
        async fn test(&self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Q;
    impl SearchQuery for Q {
        fn kind(&self) -> MediaKind {
            MediaKind::Movie
        }
        fn titles(&self) -> &[String] {
            &[]
        }
        fn year(&self) -> Option<u16> {
            None
        }
        fn external_ids(&self) -> &skadi_core::ExternalIds {
            // A leaked default keeps the signature `&ExternalIds` without storing one.
            static IDS: std::sync::OnceLock<skadi_core::ExternalIds> = std::sync::OnceLock::new();
            IDS.get_or_init(skadi_core::ExternalIds::default)
        }
        fn categories(&self) -> &[crate::Category] {
            &[]
        }
    }

    #[tokio::test(start_paused = true)]
    async fn decorator_passes_through_local_methods_and_throttles_network() {
        let calls = Arc::new(AtomicUsize::new(0));
        let id = IndexerId::new();
        let inner = Box::new(CountingIndexer {
            id,
            calls: calls.clone(),
        });
        // 60/min = 1/sec, burst of 2.
        let rl = RateLimited::new(inner, 60, 2);

        // Local methods are untouched (no throttle, identity pass-through).
        assert_eq!(rl.id(), id);
        assert_eq!(rl.protocol(), Protocol::Torrent);
        assert!(rl.supports(MediaKind::Movie));

        // Burst: the first two searches go straight through (virtual time ~unmoved).
        let start = Instant::now();
        rl.search(&Q).await.unwrap();
        rl.search(&Q).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "burst should not wait"
        );

        // Third must wait ~1s for a refill (auto-advanced under paused time).
        rl.search(&Q).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(
            start.elapsed() >= Duration::from_secs(1),
            "third call should have waited for a refill, elapsed {:?}",
            start.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_when_rate_is_zero() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = Box::new(CountingIndexer {
            id: IndexerId::new(),
            calls: calls.clone(),
        });
        let idx = indexer_rate_limited(inner, 0);
        let start = Instant::now();
        for _ in 0..50 {
            idx.search(&Q).await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 50);
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "rate 0 must not throttle"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn all_network_methods_draw_from_one_bucket() {
        // Every network method (capabilities/search/rss/test) MUST be gated by the
        // same limiter — not just `search`. With a burst of 1, the first call bursts
        // and each subsequent method must wait for a refill, proving they share it.
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = Box::new(CountingIndexer {
            id: IndexerId::new(),
            calls: calls.clone(),
        });
        let rl = RateLimited::new(inner, 60, 1); // 1/sec, burst 1

        let start = Instant::now();
        rl.capabilities().await.unwrap(); // bursts immediately
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "first call bursts"
        );

        // Three more methods, each waiting ~1s for a refill from the shared bucket.
        rl.search(&Q).await.unwrap();
        rl.rss().await.unwrap();
        rl.test().await.unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "all four network methods reached the inner indexer"
        );
        assert!(
            start.elapsed() >= Duration::from_secs(3),
            "rss/test/capabilities are throttled alongside search, elapsed {:?}",
            start.elapsed()
        );
    }
}
