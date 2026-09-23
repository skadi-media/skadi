//! Background library media scan (SKADI-T-0583).
//!
//! Every imported file gets probed once, in the background, so the library can
//! answer a question it previously could not: **what in here will not stream?**
//!
//! Media info was only ever written at import time, so a library built before
//! that step — or imported by any earlier version — carries none. Measured on
//! the live library: **1 of 1,818** movie editions and **13 of 24,014** episodes
//! had it. A report built on that is not a report.
//!
//! ## Shape
//!
//! A [`ScanSource`] per domain (movies, television) supplies unscanned items and
//! stores results; [`LibraryScanWorker`] drives them. The worker is deliberately
//! dull — fetch a bounded batch, probe it off the async runtime, store, repeat —
//! because the interesting constraint is not throughput but **not hurting the
//! rest of the daemon** while it walks tens of thousands of files on a network
//! share.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use skadi_core::{MediaInfo, Result};
use tokio_util::sync::CancellationToken;

/// One library file to profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanItem {
    /// Domain-scoped id, opaque to the scanner; handed back to
    /// [`ScanSource::store`].
    pub id: String,
    pub path: PathBuf,
}

/// How many scanned, out of how many scannable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanProgress {
    pub scanned: i64,
    pub total: i64,
}

impl ScanProgress {
    #[must_use]
    pub fn remaining(&self) -> i64 {
        (self.total - self.scanned).max(0)
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.remaining() == 0
    }
}

/// A domain that owns scannable files.
#[async_trait]
pub trait ScanSource: Send + Sync {
    /// What this source is called, for logs and the progress report.
    fn name(&self) -> &'static str;

    /// Up to `limit` imported items that have a file on disk and no media info.
    ///
    /// Bounded because the alternative is loading every path in a 24,000-episode
    /// library into memory to process a handful.
    async fn unscanned(&self, limit: i64) -> Result<Vec<ScanItem>>;

    /// Store a probe result against `id`.
    ///
    /// A `None` info means the file could not be read. Implementations should
    /// still record *something*, or the item comes straight back on the next
    /// batch and the scan livelocks on its first unreadable file.
    async fn store(&self, id: &str, info: Option<MediaInfo>) -> Result<()>;

    /// Scanned and scannable counts.
    async fn progress(&self) -> Result<ScanProgress>;
}

/// Probes files. Abstracted so tests do not need real media on disk.
pub trait Prober: Send + Sync + 'static {
    fn probe(&self, path: &std::path::Path) -> Option<MediaInfo>;
}

/// The real prober.
pub struct DefaultProber;

impl Prober for DefaultProber {
    fn probe(&self, path: &std::path::Path) -> Option<MediaInfo> {
        skadi_media_probe::MediaProber::probe(&skadi_media_probe::DefaultProber, path)
    }
}

/// How many items to claim per batch.
const BATCH: i64 = 64;

/// Pause between batches.
///
/// The scan is explicitly a background citizen: it is competing with streaming
/// reads off the same share, and finishing in four hours instead of two is worth
/// far more than a stutter in a film someone is watching.
const BATCH_PAUSE: Duration = Duration::from_millis(500);

/// How many files to probe at once.
///
/// Measured on the live library: a single probe is ~1.6 s of `ffprobe` plus a
/// header read, almost all of it **waiting on NFS** with the CPU idle. Serially
/// that put a 26,000-file library at roughly 33 hours. Concurrency is nearly
/// free here for the same reason it is usually dangerous — the work is I/O-bound,
/// so four in flight cost four idle waits rather than four busy cores.
///
/// Four, not forty: the share is also serving whatever someone is watching.
const CONCURRENCY: usize = 4;

/// Pause when there is nothing to do, before asking again.
const IDLE_PAUSE: Duration = Duration::from_secs(300);

/// Longest a single file may take to probe before the scan gives up on it.
///
/// Generous, because this share is slow and a real probe of a large Matroska
/// file legitimately takes tens of seconds. The point is not to be strict — it is
/// that no single file can stop the scan.
///
/// Note the blocking thread is NOT cancelled by this (nothing can interrupt a
/// read on a `hard` mount); the timeout frees the *worker* to move on, and the
/// thread is reclaimed whenever the read finally returns.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Walks the library and profiles everything not yet profiled.
pub struct LibraryScanWorker {
    sources: Vec<Arc<dyn ScanSource>>,
    prober: Arc<dyn Prober>,
    /// Files probed since the daemon started, for logging.
    probed: Arc<AtomicU64>,
    batch_pause: Duration,
    idle_pause: Duration,
    concurrency: usize,
    probe_timeout: Duration,
}

impl LibraryScanWorker {
    #[must_use]
    pub fn new(sources: Vec<Arc<dyn ScanSource>>, prober: Arc<dyn Prober>) -> Self {
        Self {
            sources,
            prober,
            probed: Arc::new(AtomicU64::new(0)),
            batch_pause: BATCH_PAUSE,
            idle_pause: IDLE_PAUSE,
            concurrency: CONCURRENCY,
            probe_timeout: PROBE_TIMEOUT,
        }
    }

    /// Longest one file may take before the scan moves on. Injectable so the
    /// stall test does not have to wait out the real two minutes.
    #[must_use]
    pub fn with_probe_timeout(mut self, t: Duration) -> Self {
        self.probe_timeout = t;
        self
    }

    /// How many files to probe at once. Clamped to at least 1, so a bad config
    /// value cannot deadlock the scan on a zero-permit semaphore.
    #[must_use]
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Shorten the pauses. Tests only — a real scan wants to stay out of the way.
    #[must_use]
    pub fn with_pauses(mut self, batch: Duration, idle: Duration) -> Self {
        self.batch_pause = batch;
        self.idle_pause = idle;
        self
    }

    /// Probe one batch from one source. Returns how many were handled.
    async fn scan_batch(&self, source: &Arc<dyn ScanSource>) -> usize {
        let items = match source.unscanned(BATCH).await {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!(source = source.name(), error = %e, "library scan: could not list");
                return 0;
            }
        };
        if items.is_empty() {
            return 0;
        }
        let n = items.len();
        let limit = Arc::new(tokio::sync::Semaphore::new(self.concurrency));
        let mut tasks = tokio::task::JoinSet::new();
        for item in items {
            let prober = Arc::clone(&self.prober);
            let source = Arc::clone(source);
            let probed = Arc::clone(&self.probed);
            let limit = Arc::clone(&limit);
            let probe_timeout = self.probe_timeout;
            tasks.spawn(async move {
                // Held for probe AND store, so the in-flight count is what the
                // share actually sees rather than what it sees on average.
                let _permit = limit.acquire_owned().await.ok();
                let path = item.path.clone();
                // The probe opens and parses a file on a network share: blocking,
                // and occasionally slow. Off the async runtime it cannot stall
                // the daemon's HTTP handlers.
                //
                // BOUNDED, because an unbounded one stalls the whole worker
                // (SKADI-T-0587). `ffprobe` has its own timeout, but the
                // in-process matroska/mp4 readers have none — and a `hard` NFS
                // mount retries indefinitely, so one file on a slow share parks
                // the batch forever. Observed in production: the movie scan sat
                // at 533/1818 for hours while television kept going, because
                // `join_next()` was waiting on a single read that never returned.
                let info = match tokio::time::timeout(
                    probe_timeout,
                    tokio::task::spawn_blocking(move || prober.probe(&path)),
                )
                .await
                {
                    Ok(joined) => joined.unwrap_or(None),
                    Err(_) => {
                        // WARN, not debug: this is a file we genuinely could not
                        // read in a reasonable time, and a library full of them
                        // means the share is in trouble.
                        tracing::warn!(
                            path = ?item.path,
                            secs = probe_timeout.as_secs(),
                            "library scan: probe timed out; recording it as unreadable"
                        );
                        None
                    }
                };
                if info.is_none() {
                    // Debug, not warn: a library legitimately holds files no
                    // reader handles, and one warning per such file per scan
                    // would drown the log.
                    tracing::debug!(path = ?item.path, "library scan: nothing probed");
                }
                // Store even the failure. Without this the item reappears in the
                // next batch forever and the scan never reaches the rest of the
                // library.
                if let Err(e) = source.store(&item.id, info).await {
                    tracing::warn!(source = source.name(), id = %item.id, error = %e,
                        "library scan: could not store result");
                }
                probed.fetch_add(1, Ordering::Relaxed);
            });
        }
        // Drain before returning: the batch pause is meant to be a pause between
        // batches, and returning early would let the next `unscanned` run against
        // rows this batch has not written yet — handing back the same files.
        while tasks.join_next().await.is_some() {}
        n
    }

    /// Run one pass over every source; returns total handled.
    pub async fn run_once(&self) -> usize {
        let mut total = 0;
        for source in &self.sources {
            total += self.scan_batch(source).await;
        }
        total
    }
}

impl skadi_core::module::Worker for LibraryScanWorker {
    fn name(&self) -> &str {
        "library-scan"
    }

    fn run(
        self: Box<Self>,
        cancel: CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        Box::pin(async move {
            tracing::info!(sources = self.sources.len(), "library scan: started");
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                let handled = self.run_once().await;
                let pause = if handled == 0 {
                    // Nothing left. Sleep long, but keep looping: new imports
                    // land continuously and they need profiling too.
                    self.idle_pause
                } else {
                    let done = self.probed.load(Ordering::Relaxed);
                    if done % 512 < handled as u64 {
                        tracing::info!(probed = done, "library scan: progress");
                    }
                    self.batch_pause
                };
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(pause) => {}
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSource {
        pending: Mutex<Vec<ScanItem>>,
        stored: Mutex<Vec<(String, bool)>>,
    }

    #[async_trait]
    impl ScanSource for FakeSource {
        fn name(&self) -> &'static str {
            "fake"
        }
        async fn unscanned(&self, limit: i64) -> Result<Vec<ScanItem>> {
            let pending = self.pending.lock().unwrap();
            Ok(pending.iter().take(limit as usize).cloned().collect())
        }
        async fn store(&self, id: &str, info: Option<MediaInfo>) -> Result<()> {
            self.stored
                .lock()
                .unwrap()
                .push((id.into(), info.is_some()));
            // Mimic a real repo: a stored item is no longer unscanned.
            self.pending.lock().unwrap().retain(|i| i.id != id);
            Ok(())
        }
        async fn progress(&self) -> Result<ScanProgress> {
            Ok(ScanProgress::default())
        }
    }

    struct OkProber;
    impl Prober for OkProber {
        fn probe(&self, _: &std::path::Path) -> Option<MediaInfo> {
            Some(MediaInfo {
                duration_secs: Some(100),
                ..Default::default()
            })
        }
    }

    struct FailProber;
    impl Prober for FailProber {
        fn probe(&self, _: &std::path::Path) -> Option<MediaInfo> {
            None
        }
    }

    fn items(n: usize) -> Vec<ScanItem> {
        (0..n)
            .map(|i| ScanItem {
                id: format!("id-{i}"),
                path: PathBuf::from(format!("/library/{i}.mkv")),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_pass_probes_and_stores_everything_in_the_batch() {
        let source = Arc::new(FakeSource {
            pending: Mutex::new(items(3)),
            ..Default::default()
        });
        let worker = LibraryScanWorker::new(vec![source.clone()], Arc::new(OkProber));
        assert_eq!(worker.run_once().await, 3);
        let stored = source.stored.lock().unwrap();
        assert_eq!(stored.len(), 3);
        assert!(stored.iter().all(|(_, ok)| *ok));
    }

    /// The livelock guard. An unreadable file must still be recorded, or it comes
    /// back in the next batch forever and the scan never reaches anything else.
    #[tokio::test]
    async fn an_unreadable_file_is_still_recorded_so_the_scan_advances() {
        let source = Arc::new(FakeSource {
            pending: Mutex::new(items(2)),
            ..Default::default()
        });
        let worker = LibraryScanWorker::new(vec![source.clone()], Arc::new(FailProber));
        assert_eq!(worker.run_once().await, 2);
        assert_eq!(
            source.stored.lock().unwrap().len(),
            2,
            "failures recorded too"
        );
        // The decisive part: a second pass finds nothing, rather than the same
        // two files again.
        assert_eq!(
            worker.run_once().await,
            0,
            "a failed probe must not be retried forever"
        );
    }

    /// Concurrency is **bounded**, not unlimited.
    ///
    /// Without the assertion an off-by-one or a dropped permit would let the
    /// scanner open 64 files at once against the same NFS share the library is
    /// streamed from — which is the one thing this worker is supposed not to do.
    #[tokio::test]
    async fn no_more_than_the_configured_number_of_probes_run_at_once() {
        use std::sync::atomic::AtomicUsize;

        struct CountingProber {
            live: AtomicUsize,
            peak: Arc<AtomicUsize>,
        }
        impl Prober for CountingProber {
            fn probe(&self, _: &std::path::Path) -> Option<MediaInfo> {
                let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                // Long enough that overlapping probes genuinely overlap; without
                // this every probe finishes before the next starts and the peak
                // reads 1 no matter how broken the bound is.
                std::thread::sleep(Duration::from_millis(40));
                self.live.fetch_sub(1, Ordering::SeqCst);
                Some(MediaInfo::default())
            }
        }

        let peak = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(FakeSource {
            pending: Mutex::new(items(24)),
            ..Default::default()
        });
        let worker = LibraryScanWorker::new(
            vec![source.clone()],
            Arc::new(CountingProber {
                live: AtomicUsize::new(0),
                peak: Arc::clone(&peak),
            }),
        )
        .with_concurrency(4);

        assert_eq!(worker.run_once().await, 24);
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak <= 4, "bound exceeded: {peak} probes ran at once");
        assert!(
            peak > 1,
            "nothing ran concurrently ({peak}); the bound is untested and the \
             scan is still serial"
        );
    }

    /// A file that never finishes reading must not stop the scan
    /// (SKADI-T-0587).
    ///
    /// This is the production failure: the movie scan sat at 533/1818 for hours
    /// while television kept going, because one `spawn_blocking` read on a
    /// `hard` NFS mount never returned and `join_next()` waited on it forever.
    /// The ffprobe path had a timeout; the in-process readers did not.
    #[tokio::test]
    async fn one_unreadable_file_cannot_stall_the_whole_batch() {
        struct HangingProber;
        impl Prober for HangingProber {
            fn probe(&self, path: &std::path::Path) -> Option<MediaInfo> {
                // Only the first file hangs; the rest are fine. A scan that
                // stalls on one bad file is the bug, not a scan that is slow.
                if path.to_string_lossy().ends_with("/0.mkv") {
                    // Long relative to the 200ms probe timeout, but finite: the
                    // runtime waits for blocking threads at shutdown, so an
                    // "infinite" hang here would hang the test process itself —
                    // the very property this test is about.
                    std::thread::sleep(Duration::from_secs(3));
                }
                Some(MediaInfo::default())
            }
        }

        let source = Arc::new(FakeSource {
            pending: Mutex::new(items(3)),
            ..Default::default()
        });
        let worker = LibraryScanWorker::new(vec![source.clone()], Arc::new(HangingProber))
            .with_concurrency(2)
            .with_probe_timeout(Duration::from_millis(200));

        // Without the timeout this never returns. With it, the batch completes
        // and the hung file is recorded so the scan can move past it.
        let handled = tokio::time::timeout(Duration::from_secs(20), worker.run_once())
            .await
            .expect("the batch must finish even though one file never reads");
        assert_eq!(handled, 3);
        assert_eq!(
            source.stored.lock().unwrap().len(),
            3,
            "every item is recorded, including the one that timed out"
        );
    }

    #[tokio::test]
    async fn an_empty_library_is_not_an_error() {
        let source = Arc::new(FakeSource::default());
        let worker = LibraryScanWorker::new(vec![source], Arc::new(OkProber));
        assert_eq!(worker.run_once().await, 0);
    }

    #[tokio::test]
    async fn every_source_is_visited_in_one_pass() {
        let a = Arc::new(FakeSource {
            pending: Mutex::new(items(2)),
            ..Default::default()
        });
        let b = Arc::new(FakeSource {
            pending: Mutex::new(items(3)),
            ..Default::default()
        });
        let worker = LibraryScanWorker::new(vec![a.clone(), b.clone()], Arc::new(OkProber));
        assert_eq!(worker.run_once().await, 5, "movies AND television");
        assert_eq!(a.stored.lock().unwrap().len(), 2);
        assert_eq!(b.stored.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn cancellation_stops_the_loop() {
        use skadi_core::module::Worker as _;
        let source = Arc::new(FakeSource {
            pending: Mutex::new(items(1)),
            ..Default::default()
        });
        let worker = Box::new(
            LibraryScanWorker::new(vec![source], Arc::new(OkProber))
                .with_pauses(Duration::from_millis(5), Duration::from_millis(5)),
        );
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(worker.run(cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        // If the worker ignored cancellation this would hang until the test
        // harness killed it.
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("worker must stop promptly on cancel")
            .unwrap();
    }

    #[test]
    fn progress_reports_what_is_left() {
        let p = ScanProgress {
            scanned: 30,
            total: 100,
        };
        assert_eq!(p.remaining(), 70);
        assert!(!p.complete());
        assert!(
            ScanProgress {
                scanned: 100,
                total: 100
            }
            .complete()
        );
        // A source that somehow reports more scanned than total must not produce
        // a negative "remaining" in the API.
        assert_eq!(
            ScanProgress {
                scanned: 5,
                total: 3
            }
            .remaining(),
            0
        );
    }
}
