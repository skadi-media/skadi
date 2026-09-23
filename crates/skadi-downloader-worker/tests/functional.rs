//! Functional test for the downloader **in isolation** (SKADI-T-0096).
//!
//! Proves the worker's core promise — *given a magnet, a `.torrent` URL, or a
//! file dropped in the watched folder, does it pick up and download the
//! torrent?* — with **no daemon, no Prowlarr, no VPN**: just the worker binary
//! logic (`run`) over a throwaway SQLite DB in standalone mode (`migrate=true`),
//! driven three ways at once.
//!
//! `#[ignore]` — it downloads real bytes from the network. The content is
//! Blender's open movies (Creative Commons, freely distributable) served with
//! HTTP **webseeds** via webtorrent.io, so a download starts deterministically
//! even with no live BitTorrent peers; safe to run from any IP without a VPN.
//! Run it:
//!   cargo test --manifest-path crates/skadi-downloader-worker/Cargo.toml \
//!     --test functional -- --ignored --nocapture
//!
//! NOTE: standalone/no-VPN is **test-only**. Real (copyrighted) acquisitions
//! must run the worker inside gluetun's namespace — there is no kill switch
//! when it runs bare.
//!
//! It leans on a live HTTP webseed, so a transient network/webseed hiccup can
//! fail it; just re-run. The deterministic intake logic (watch-folder scan,
//! local-file/magnet/redirect source resolution) is covered by the offline
//! unit tests in `src/lib.rs`.

use std::path::Path;
use std::time::Duration;

use skadi_downloader_worker::{Config, SeedAction, SeedPolicy, run};
use skadi_store::{DownloadJob, DownloadJobRepo, DownloadJobStatus, NewDownloadJob, Store};

// Webseed-backed (ws=) so they fetch from HTTP even with zero peers.
const SINTEL_MAGNET: &str = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10\
&dn=Sintel\
&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce\
&tr=udp%3A%2F%2Fexplodie.org%3A6969\
&ws=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2F\
&xs=https%3A%2F%2Fwebtorrent.io%2Ftorrents%2Fsintel.torrent";
const BBB_TORRENT_URL: &str = "https://webtorrent.io/torrents/big-buck-bunny.torrent";
const COSMOS_TORRENT_URL: &str = "https://webtorrent.io/torrents/cosmos-laundromat.torrent";

fn unique_base() -> std::path::PathBuf {
    skadi_core::unique_temp_path("fn")
}

async fn enqueue(store: &Store, reff: &str, source: &str, dl: &Path) -> DownloadJob {
    store
        .enqueue(&NewDownloadJob {
            acquirable_ref: reff.into(),
            source: source.into(),
            category: None,
            incomplete_dir: Some(dl.to_string_lossy().into_owned()),
            complete_dir: None,
        })
        .await
        .expect("enqueue")
}

/// Poll a specific job id until it is `downloading` with bytes on the wire.
async fn wait_progress_by_id(store: &Store, id: &str, secs: u64) -> bool {
    for _ in 0..secs {
        let j = store.get_download(id).await.expect("get").expect("row");
        if j.status == DownloadJobStatus::Error {
            panic!("job {id} errored: {:?}", j.error);
        }
        if j.status == DownloadJobStatus::Downloading && j.progress_bytes > 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

/// Poll for a job the worker auto-created from the watch folder (found by its
/// `watch:<filename>` acquirable_ref) and confirm it downloads.
async fn wait_progress_by_ref(store: &Store, reff: &str, secs: u64) -> bool {
    for _ in 0..secs {
        if let Some(j) = store
            .list_downloads()
            .await
            .expect("list")
            .into_iter()
            .find(|j| j.acquirable_ref == reff)
        {
            if j.status == DownloadJobStatus::Error {
                panic!("watch job errored: {:?}", j.error);
            }
            if j.status == DownloadJobStatus::Downloading && j.progress_bytes > 0 {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "network: downloads webseed-backed open movies from webtorrent.io"]
async fn picks_up_and_downloads_from_magnet_url_and_watch_folder() {
    let base = unique_base();
    let dl = base.join("downloads");
    let watch = base.join("watch");
    std::fs::create_dir_all(&watch).expect("watch dir");
    let db_url = format!("sqlite://{}", base.join("worker.db").display());

    // The worker, standalone: owns its SQLite schema, watches `watch/`.
    let cfg = Config {
        download_dir: dl.clone(),
        state_dir: base.join("state"),
        port_range: 27881..27891,
        database_url: db_url.clone(),
        worker_id: "fn-worker".into(),
        poll_interval: Duration::from_secs(1),
        tick_interval: Duration::from_secs(1),
        watch_dir: Some(watch.clone()),
        migrate: true,
        seed_policy: skadi_downloader_worker::SeedPolicy::unlimited(),
        down_limit_bps: None,
        up_limit_bps: None,
        max_active: None,
        stall_timeout_secs: None,
        metadata_timeout_secs: None,
        lease_secs: 120,
        extra_trackers: Vec::new(),
    };
    let agent = tokio::spawn(run(cfg));

    // Let the worker create the schema (standalone migrate) before we enqueue.
    let store = loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(s) = Store::connect(&db_url)
            && s.list_downloads().await.is_ok()
        {
            break s;
        }
    };

    // Mode 1 — magnet via the queue. Each download gets its own folder (real
    // jobs are per-acquirable; these open movies all carry a top-level
    // poster.jpg that would otherwise collide).
    let magnet_job = enqueue(&store, "magnet-mode", SINTEL_MAGNET, &dl.join("sintel")).await;
    // Mode 2 — .torrent URL via the queue (also exercises redirect-resolve).
    let url_job = enqueue(&store, "url-mode", BBB_TORRENT_URL, &dl.join("bbb")).await;
    // Mode 3 — a .torrent FILE dropped in the watched folder.
    let bytes = reqwest::get(COSMOS_TORRENT_URL)
        .await
        .expect("fetch cosmos .torrent")
        .bytes()
        .await
        .expect("cosmos bytes");
    std::fs::write(watch.join("cosmos.torrent"), &bytes).expect("drop watch file");

    // All three intake modes reach `downloading` with real bytes.
    assert!(
        wait_progress_by_id(&store, &magnet_job.id, 120).await,
        "magnet → picked up and downloading"
    );
    assert!(
        wait_progress_by_id(&store, &url_job.id, 120).await,
        "torrent URL → picked up and downloading"
    );
    assert!(
        wait_progress_by_ref(&store, "watch:cosmos.torrent", 120).await,
        "watched-folder .torrent → ingested and downloading"
    );

    // The watched file was archived out of the drop zone (not re-ingested).
    assert!(
        watch.join(".processed/cosmos.torrent").exists(),
        "watch file moved to .processed"
    );

    // Tear the transfers down and stop the agent.
    for id in [&magnet_job.id, &url_job.id] {
        store
            .request_remove(id, true)
            .await
            .expect("request_remove");
    }
    agent.abort();
    let _ = std::fs::remove_dir_all(&base);
}

/// Poll a job until it reaches `target` (or fail on `error`), up to `secs`.
async fn wait_status_by_id(store: &Store, id: &str, target: DownloadJobStatus, secs: u64) -> bool {
    for _ in 0..secs {
        let j = store.get_download(id).await.expect("get").expect("row");
        if j.status == DownloadJobStatus::Error {
            panic!("job {id} errored: {:?}", j.error);
        }
        if j.status == target {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

/// SKADI-T-0210: with a seed **time limit of 0**, the worker must stop seeding the
/// instant a download finishes — Big Buck Bunny goes `downloading → completed →
/// seeded` end-to-end, and the data is kept on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "network: downloads Big Buck Bunny (webseed) to completion"]
async fn seed_time_limit_stops_seeding_after_completion() {
    let base = unique_base();
    let dl = base.join("downloads");
    std::fs::create_dir_all(&dl).expect("dl dir");
    let db_url = format!("sqlite://{}", base.join("worker.db").display());

    // A zero-second seed-time limit: any finished torrent is immediately past it,
    // so the worker stops seeding the moment the download completes.
    let cfg = Config {
        download_dir: dl.clone(),
        state_dir: base.join("state"),
        port_range: 28881..28891,
        database_url: db_url.clone(),
        worker_id: "seed-fn".into(),
        poll_interval: Duration::from_secs(1),
        tick_interval: Duration::from_secs(1),
        watch_dir: None,
        migrate: true,
        seed_policy: SeedPolicy {
            ratio_limit: None,
            time_limit_secs: Some(0),
            action: SeedAction::Stop,
        },
        down_limit_bps: None,
        up_limit_bps: None,
        max_active: None,
        stall_timeout_secs: None,
        metadata_timeout_secs: None,
        lease_secs: 120,
        extra_trackers: Vec::new(),
    };
    let agent = tokio::spawn(run(cfg));

    let store = loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(s) = Store::connect(&db_url)
            && s.list_downloads().await.is_ok()
        {
            break s;
        }
    };

    let job = enqueue(&store, "seed-bbb", BBB_TORRENT_URL, &dl.join("bbb")).await;
    assert!(
        wait_progress_by_id(&store, &job.id, 120).await,
        "BBB picked up and downloading"
    );

    // Download to completion → seeding starts → 0s time limit trips → Seeded.
    assert!(
        wait_status_by_id(&store, &job.id, DownloadJobStatus::Seeded, 600).await,
        "BBB should reach Seeded once it finishes (seed limit reached)"
    );

    let final_job = store.get_download(&job.id).await.unwrap().unwrap();
    assert_eq!(final_job.status, DownloadJobStatus::Seeded);
    assert!(final_job.completed_at.is_some(), "completed_at stamped");
    assert!(
        !final_job.files.is_empty(),
        "completed file paths recorded (data kept)"
    );

    agent.abort();
    let _ = std::fs::remove_dir_all(&base);
}
