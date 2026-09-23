//! A per-key circuit breaker (SKADI-T-0041).
//!
//! Stops hammering an external dependency (e.g. a metadata provider) that's
//! failing: after `failure_threshold` consecutive failures a key trips **open**
//! for a cool-off that grows with repeated trips (capped at `max_cooloff`).
//! While open, [`allow`](CircuitBreaker::allow) returns `false` — except for a
//! single **probe** once the cool-off elapses. The caller reports the outcome
//! via [`record_success`](CircuitBreaker::record_success) /
//! [`record_failure`](CircuitBreaker::record_failure): a success closes the
//! breaker; a failed probe re-trips it with a longer cool-off.
//!
//! Process-local (single daemon) and intended for a sequential per-key caller
//! (the refresh worker handles one provider at a time); the probe is not
//! hardened against many concurrent callers racing the same key.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

// `tokio::time::Instant`, not `std`'s (SKADI-T-0534): it honours
// `tokio::time::pause()`/`advance()`, so the cool-off tests can assert the
// breaker's *logic* against virtual time instead of racing real 20 ms sleeps.
// Outside a paused runtime it is the system clock, so production behaviour is
// unchanged.
use tokio::time::Instant;

/// Per-key failure/trip state.
#[derive(Debug, Default)]
struct KeyState {
    consecutive_failures: u32,
    /// When set and in the future, the key is open (blocked) until then.
    open_until: Option<Instant>,
    /// How many times the key has tripped (drives cool-off growth).
    trips: u32,
}

/// A keyed circuit breaker. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct CircuitBreaker {
    failure_threshold: u32,
    base_cooloff: Duration,
    max_cooloff: Duration,
    state: Mutex<HashMap<String, KeyState>>,
}

impl CircuitBreaker {
    /// Trip after `failure_threshold` consecutive failures; first cool-off is
    /// `base_cooloff`, doubling per re-trip up to `max_cooloff`.
    #[must_use]
    pub fn new(failure_threshold: u32, base_cooloff: Duration, max_cooloff: Duration) -> Self {
        Self {
            failure_threshold: failure_threshold.max(1),
            base_cooloff,
            max_cooloff,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a call for `key` may proceed now: `true` if closed, or if open
    /// but the cool-off has elapsed (a probe). `false` while cooling off.
    #[must_use]
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let guard = self.state.lock().expect("breaker lock poisoned");
        match guard.get(key).and_then(|s| s.open_until) {
            Some(open_until) => now >= open_until, // probe once cool-off elapses
            None => true,
        }
    }

    /// Whether `key` is currently cooling off (open and not yet at probe time).
    /// For diagnostics/tests.
    #[must_use]
    pub fn is_open(&self, key: &str) -> bool {
        let now = Instant::now();
        self.state
            .lock()
            .expect("breaker lock poisoned")
            .get(key)
            .and_then(|s| s.open_until)
            .is_some_and(|t| now < t)
    }

    /// Record a successful call: closes the breaker for `key`.
    pub fn record_success(&self, key: &str) {
        let mut guard = self.state.lock().expect("breaker lock poisoned");
        guard.insert(key.to_string(), KeyState::default());
    }

    /// Record a failed call. Trips (or re-trips with a longer cool-off) once
    /// consecutive failures reach the threshold.
    pub fn record_failure(&self, key: &str) {
        let mut guard = self.state.lock().expect("breaker lock poisoned");
        let s = guard.entry(key.to_string()).or_default();
        s.consecutive_failures += 1;
        if s.consecutive_failures >= self.failure_threshold {
            s.trips += 1;
            let factor = 2u32.saturating_pow(s.trips.saturating_sub(1));
            let cooloff = self
                .base_cooloff
                .saturating_mul(factor)
                .min(self.max_cooloff);
            s.open_until = Some(Instant::now() + cooloff);
            s.consecutive_failures = 0; // counted toward this trip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn trips_after_threshold_then_recovers_on_probe() {
        // Virtual time (SKADI-T-0534): `sleep` returns instantly and advances the
        // clock exactly, so the assertions test the breaker's logic rather than
        // racing the scheduler. The old real-clock version left 5-15 ms of slack,
        // which full-suite load ate.
        let b = CircuitBreaker::new(2, Duration::from_millis(30), Duration::from_secs(1));
        // Closed initially.
        assert!(b.allow("tmdb"));
        // One failure: still closed (below threshold).
        b.record_failure("tmdb");
        assert!(b.allow("tmdb"));
        // Second failure: trips open.
        b.record_failure("tmdb");
        assert!(!b.allow("tmdb"), "open after threshold");
        assert!(b.is_open("tmdb"));
        // After the cool-off, a probe is allowed.
        tokio::time::sleep(Duration::from_millis(31)).await;
        assert!(b.allow("tmdb"), "probe after cool-off");
        // A successful probe closes it.
        b.record_success("tmdb");
        assert!(b.allow("tmdb"));
        assert!(!b.is_open("tmdb"));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_probe_retrips_with_longer_cooloff() {
        let b = CircuitBreaker::new(1, Duration::from_millis(20), Duration::from_secs(1));
        b.record_failure("p"); // threshold 1 → trips immediately
        assert!(!b.allow("p"));

        // Exactly at the cool-off, not "a bit after": with virtual time the
        // boundary is testable, so assert it rather than sleeping past it.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(b.allow("p"), "first probe once the cool-off has elapsed");

        b.record_failure("p"); // probe failed → re-trip with a longer cool-off
        assert!(!b.allow("p"));
        // The first cool-off's length is now provably not enough...
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!b.allow("p"), "second cool-off is longer than the first");
        // ...and the doubled one is.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(b.allow("p"), "second probe after the doubled cool-off");
    }

    #[test]
    fn keys_are_independent() {
        let b = CircuitBreaker::new(1, Duration::from_secs(10), Duration::from_secs(10));
        b.record_failure("a");
        assert!(!b.allow("a"));
        assert!(b.allow("b"), "other key unaffected");
    }
}
