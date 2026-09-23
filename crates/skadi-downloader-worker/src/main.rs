//! Entry point for the Skadi download worker. Builds the config from the
//! environment and runs the DB agent (poll the `downloads` queue, drive
//! librqbit, write progress back). See [`crate`] / `lib.rs` for the loop.

use skadi_downloader_worker::{Config, run};

/// jemalloc instead of glibc malloc (SKADI-T-0392). librqbit does its disk IO in
/// `block_in_place` sections; every one that blocks (slow NFS, a Wi-Fi stall)
/// makes tokio hand the core to a fresh blocking-pool thread, and each of those
/// threads touches multi-MiB piece buffers. glibc parks every such thread on
/// its own malloc arena and never gives the freed piece buffers back (non-main
/// arenas only trim from the top; the dynamic mmap threshold climbs past the
/// piece size after the first large frees). Prod was measured at 7.8 GiB RSS
/// sitting in 178 idle glibc arenas with ~130 MiB of live data. jemalloc's
/// decay returns freed pages regardless of which thread freed them, and
/// `narenas` (see `worker.Dockerfile`) keeps the arena count independent of
/// the thread count.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Cap on tokio's blocking pool (default 512). A stalled filesystem turns every
/// in-flight `block_in_place` into a parked thread that lives forever — tokio
/// only retires an idle blocking thread when its 10 s keep-alive expires
/// *without* a wake-up, and the FIFO condvar rotates every `spawn_blocking` /
/// hand-off through the whole pool, so the pool never shrinks below its
/// historical peak (451 threads minted in one minute on prod, SKADI-T-0392).
/// What actually needs the pool: `CONCURRENT_INIT_LIMIT` hash checks, one
/// deferred disk writer, one upload read per active peer, tokio::fs for the
/// session file, `place_complete`. 64 is generous for that and bounds the
/// blowup to ~64 × (stack + arena) if the mount stalls again.
const MAX_BLOCKING_THREADS: usize = 64;

fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,librqbit=info".into())
        // Silence librqbit's benign "error removing <hash>.bitv NotFound" WARN, emitted
        // when forgetting a torrent whose session file was never persisted (a re-attached
        // seed after a hard restart, SKADI-T-0172). Applied on top of RUST_LOG so it holds
        // regardless of the configured level.
        .add_directive(
            "librqbit::session_persistence=error"
                .parse()
                .expect("static directive"),
        );
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    // Resolve config from the shared `config` table (env-seeded), falling back
    // to env/defaults if the table isn't reachable yet (SKADI-T-0103).
    let cfg = Config::resolve().await;
    tracing::info!(
        worker = %cfg.worker_id,
        download_dir = %cfg.download_dir.display(),
        poll_secs = cfg.poll_interval.as_secs(),
        max_blocking_threads = MAX_BLOCKING_THREADS,
        "starting skadi-downloader-worker (db agent)"
    );

    debug_blocking_burst();

    // Stop promptly on SIGTERM (SKADI-T-0476). Without a handler, `docker stop`
    // ran the full grace period and then SIGKILLed the worker — which is how a
    // container ended up with an unkillable zombie PID on 2026-09-07. librqbit's
    // session persistence is incremental, so returning here loses nothing that a
    // kill would have preserved; it just does it deliberately and quickly.
    tokio::select! {
        r = run(cfg) => r,
        signal = wait_for_shutdown_signal() => {
            tracing::info!(signal, "shutdown signal received; stopping");
            Ok(())
        }
    }
}

/// Resolve when the process is asked to stop, returning which signal did it.
///
/// SIGTERM is what an orchestrator sends first (`docker stop`); Ctrl-C is what a
/// developer sends. On non-Unix only Ctrl-C exists.
async fn wait_for_shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return "SIGINT";
            }
        };
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            r = tokio::signal::ctrl_c() => {
                if r.is_err() {
                    std::future::pending::<()>().await;
                }
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

/// Lab-only fault injection (SKADI-T-0392): `SKADI_WORKER_DEBUG_BLOCKING_BURST=N`
/// fires N `spawn_blocking` jobs at startup that each dirty an 8 MiB buffer and
/// hold their thread for 20 s — the aftermath of an IO stall, on demand. Watch
/// `deploy/lab/memwatch.sh`: the thread count must plateau at
/// `MAX_BLOCKING_THREADS` (+ runtime workers), and RSS must fall back within a
/// minute of the burst ending. Unset (the default) does nothing.
fn debug_blocking_burst() {
    let Some(n) = std::env::var("SKADI_WORKER_DEBUG_BLOCKING_BURST")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
    else {
        return;
    };
    tracing::warn!(
        n,
        "DEBUG: firing a blocking-pool burst (SKADI_WORKER_DEBUG_BLOCKING_BURST) — lab use only"
    );
    for i in 0..n {
        tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; 8 * 1024 * 1024];
            // Touch every page so the allocator really hands the memory out.
            for (k, b) in buf.iter_mut().enumerate().step_by(4096) {
                *b = (k ^ i) as u8;
            }
            std::thread::sleep(std::time::Duration::from_secs(20));
            std::hint::black_box(&buf);
        });
    }
}
