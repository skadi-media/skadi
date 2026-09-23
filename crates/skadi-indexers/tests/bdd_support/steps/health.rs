//! Indexer health accounting (`HealthTracked` + the process-global registry) and
//! the per-indexer token-bucket rate limiter.
use std::time::Duration;

use async_trait::async_trait;
use cucumber::{given, then, when};
use skadi_core::{AppError, IndexerId, MediaKind, Protocol, Result};
use skadi_indexers::{
    Indexer, IndexerCaps, Release, SearchQuery, StubIndexer, TokenBucket, indexer_health,
    indexer_health_tracked, indexer_rate_limited,
};

use crate::bdd_support::World;

/// An indexer whose every network call fails with a fixed message.
struct Failing {
    id: IndexerId,
    msg: String,
}

#[async_trait]
impl Indexer for Failing {
    fn id(&self) -> IndexerId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    fn supports(&self, _kind: MediaKind) -> bool {
        true
    }
    async fn capabilities(&self) -> Result<IndexerCaps> {
        Err(AppError::Network(self.msg.clone()))
    }
    async fn search(&self, _q: &dyn SearchQuery) -> Result<Vec<Release>> {
        Err(AppError::Network(self.msg.clone()))
    }
    async fn rss(&self) -> Result<Vec<Release>> {
        Err(AppError::Network(self.msg.clone()))
    }
    async fn test(&self) -> Result<()> {
        Err(AppError::Network(self.msg.clone()))
    }
}

#[given(regex = r#"^a health-tracked indexer that fails every call with "([^"]*)"$"#)]
fn failing(w: &mut World, msg: String) {
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(indexer_health_tracked(Box::new(Failing { id, msg })));
}

#[given("a health-tracked stub indexer")]
fn tracked_stub(w: &mut World) {
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(indexer_health_tracked(Box::new(StubIndexer::new(id))));
}

#[given(regex = r"^a rate-limited stub indexer allowing (\d+) requests per minute$")]
fn limited_stub(w: &mut World, per_minute: u32) {
    let id = IndexerId::new();
    w.indexer_id = Some(id);
    w.indexer = Some(indexer_rate_limited(
        Box::new(StubIndexer::new(id)),
        per_minute,
    ));
}

#[when("one failure is forgiven")]
fn forgive(w: &mut World) {
    indexer_health().forgive(w.indexer_id.expect("indexer id"));
}

#[then(regex = r"^the indexer health shows (\d+) consecutive failures$")]
fn consecutive(w: &mut World, n: u32) {
    let h = indexer_health()
        .get(w.indexer_id.expect("indexer id"))
        .expect("health recorded");
    assert_eq!(h.consecutive_failures, n, "{h:?}");
}

#[then(regex = r"^the indexer health shows (\d+) successes and (\d+) failures in total$")]
fn totals(w: &mut World, ok: u64, bad: u64) {
    let h = indexer_health()
        .get(w.indexer_id.expect("indexer id"))
        .expect("health recorded");
    assert_eq!((h.success_count, h.failure_count), (ok, bad), "{h:?}");
}

#[then(regex = r#"^the indexer health records the last error "([^"]*)"$"#)]
fn last_error(w: &mut World, msg: String) {
    let h = indexer_health()
        .get(w.indexer_id.expect("indexer id"))
        .expect("health recorded");
    assert_eq!(h.last_error.as_deref(), Some(msg.as_str()));
}

#[then(regex = r"^the indexer is reported (healthy|unhealthy)$")]
fn healthy(w: &mut World, state: String) {
    let h = indexer_health()
        .get(w.indexer_id.expect("indexer id"))
        .expect("health recorded");
    assert_eq!(h.healthy(), state == "healthy", "{h:?}");
    let snap = indexer_health().snapshot();
    let row = snap
        .iter()
        .find(|s| s.indexer == w.indexer_id.unwrap())
        .expect("in snapshot");
    assert_eq!(row.healthy, state == "healthy");
}

#[then("the indexer health has no entry yet")]
fn no_entry(w: &mut World) {
    assert!(
        indexer_health()
            .get(w.indexer_id.expect("indexer id"))
            .is_none()
    );
}

// --- token bucket (pure) ----------------------------------------------------------

#[given(regex = r"^a token bucket of capacity (\d+) refilling (\d+) per second$")]
fn bucket(w: &mut World, cap: u32, refill: u32) {
    w.bucket = Some(TokenBucket::new(f64::from(cap), f64::from(refill), 0.0));
    w.waits.clear();
}

#[when(regex = r"^(\d+) requests are attempted at t=(\d+)s$")]
fn attempt(w: &mut World, n: usize, t: u64) {
    let b = w.bucket.as_mut().expect("bucket");
    for _ in 0..n {
        w.waits.push(b.take(t as f64));
    }
}

#[then(regex = r"^attempts (\d+) through (\d+) pass immediately$")]
fn pass(w: &mut World, from: usize, to: usize) {
    for i in from..=to {
        assert!(
            w.waits[i - 1].is_none(),
            "attempt {i} waited {:?}",
            w.waits[i - 1]
        );
    }
}

#[then(regex = r"^attempt (\d+) must wait about (\d+) ms$")]
fn wait(w: &mut World, i: usize, ms: u64) {
    let got = w.waits[i - 1].expect("attempt should be throttled");
    let want = Duration::from_millis(ms);
    assert!(
        got.abs_diff(want) < Duration::from_millis(5),
        "attempt {i} waited {got:?}, expected ~{want:?}"
    );
}
