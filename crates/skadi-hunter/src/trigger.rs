//! On-demand sweep trigger (SKADI-T-0193).
//!
//! The scheduled [`HunterWorker`](crate::worker::HunterWorker) sweeps on a timer;
//! this is the **manual** "search all wanted now" control behind `POST /search-all`.
//! The API layer has no handle on the per-domain runners/workers, so the trigger is
//! a process-global like [`tracker`](crate::tracker) — the API pokes it, and every
//! running worker observes it.
//!
//! Backed by a [`tokio::sync::watch`] channel of a monotonically-increasing
//! generation counter: each [`request_sweep`] bumps it, and every worker holds a
//! [`Receiver`](watch::Receiver) it `changed().await`s in its select loop. `watch`
//! is the right primitive here — it buffers the latest value (so a poke while a
//! worker is mid-sweep isn't lost) and fans out to **all** receivers (so one
//! `/search-all` wakes every domain's worker), unlike `Notify::notify_waiters`.

use std::sync::OnceLock;

use tokio::sync::watch;

static SWEEP_TX: OnceLock<watch::Sender<u64>> = OnceLock::new();

fn channel() -> &'static watch::Sender<u64> {
    SWEEP_TX.get_or_init(|| watch::channel(0u64).0)
}

/// Request an immediate sweep on every running worker (manual "search all").
/// Best-effort and non-blocking: it bumps the generation the workers watch. Pokes
/// that arrive while a worker is already sweeping collapse into one follow-up sweep.
pub fn request_sweep() {
    channel().send_modify(|n| *n = n.wrapping_add(1));
}

/// A subscriber a worker holds to await manual sweep requests. The first
/// [`changed`](watch::Receiver::changed) resolves on the next [`request_sweep`]
/// after subscribing (the initial value is marked seen).
#[must_use]
pub fn subscribe() -> watch::Receiver<u64> {
    channel().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_sweep_wakes_subscribers() {
        let mut rx = subscribe();
        // No request yet → no change pending.
        assert!(!rx.has_changed().unwrap());

        request_sweep();
        // changed() resolves promptly after a request.
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
            .await
            .expect("changed() should resolve after request_sweep")
            .expect("sender alive");

        // A second subscriber created *after* a request sees no stale change until
        // the next request (initial value marked seen) — so workers don't sweep on
        // spin-up just because someone poked earlier.
        let late = subscribe();
        assert!(!late.has_changed().unwrap());
        request_sweep();
        assert!(late.has_changed().unwrap());
    }
}
