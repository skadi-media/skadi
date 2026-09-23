//! Network-gated integration tests for the worker (SKADI-T-0084 / T-0086).
//!
//! Both are `#[ignore]` — they download from a live torrent swarm. Run them:
//!   cargo test --manifest-path crates/skadi-downloader-worker/Cargo.toml -- --ignored
//!
//! `downloads_via_worker_api` checks the raw librqbit wiring; `worker_db_queue_…`
//! drives the whole DB-agent loop (enqueue → claim → progress → remove) the way
//! the daemon will. The DB-queue test defaults to a temp SQLite file but honors
//! `SKADI_DATABASE_URL` / `SKADI_TEST_DATABASE_URL` (prefer Postgres to avoid
//! SQLite's single-writer lock when the test and the agent both hold a pool).

use std::time::Duration;

use librqbit::AddTorrent;
use librqbit::api::TorrentIdOrHash;
use skadi_downloader_worker::{Config, build_api, run};
use skadi_store::{DownloadJobRepo, DownloadJobStatus, NewDownloadJob, Store};

const UBUNTU_TORRENT: &str =
    "https://releases.ubuntu.com/24.04/ubuntu-24.04.3-desktop-amd64.iso.torrent";

#[tokio::test]
#[ignore = "network: downloads from a live torrent swarm"]
async fn downloads_via_worker_api() {
    let tmp = skadi_core::unique_temp_path("worker-it");
    let cfg = Config {
        download_dir: tmp.join("dl"),
        state_dir: tmp.join("state"),
        port_range: 26881..26891,
        database_url: skadi_store::DEFAULT_DATABASE_URL.to_string(),
        worker_id: "it-worker".to_string(),
        poll_interval: Duration::from_secs(3),
        tick_interval: Duration::from_secs(1),
        watch_dir: None,
        migrate: false,
        seed_policy: skadi_downloader_worker::SeedPolicy::unlimited(),
        down_limit_bps: None,
        up_limit_bps: None,
        max_active: None,
        stall_timeout_secs: None,
        metadata_timeout_secs: None,
        lease_secs: 120,
        extra_trackers: Vec::new(),
    };

    let api = build_api(&cfg).await.expect("build_api");
    let resp = api
        .api_add_torrent(AddTorrent::from_url(UBUNTU_TORRENT), None)
        .await
        .expect("add_torrent");
    let hash = resp.details.info_hash.clone();
    assert_eq!(hash.len(), 40, "info_hash is 40 hex chars");
    let id = TorrentIdOrHash::parse(&hash).expect("parse hash");

    // Poll up to ~30s for the file list + some download progress.
    let mut progressed = false;
    let mut files_listed = false;
    for _ in 0..30 {
        let s = api.api_stats_v1(id).expect("stats");
        let d = api.api_torrent_details(id).expect("details");
        if d.files.as_ref().map(|f| !f.is_empty()).unwrap_or(false) {
            files_listed = true;
        }
        if s.progress_bytes > 0 {
            progressed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(files_listed, "metadata resolved an output file list");
    assert!(progressed, "downloaded some bytes from peers");

    // Remove + clean up.
    api.api_torrent_action_delete(id).await.expect("delete");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
#[ignore = "network: downloads from a live torrent swarm"]
async fn worker_db_queue_claim_track_remove() {
    let tmp = skadi_core::unique_temp_path("worker-dbq");
    std::fs::create_dir_all(&tmp).expect("tmp dir");

    // The daemon would own the schema; the test stands in for it. Prefer an
    // injected Postgres URL; fall back to a temp SQLite file.
    let db_url = std::env::var("SKADI_DATABASE_URL")
        .or_else(|_| std::env::var("SKADI_TEST_DATABASE_URL"))
        .unwrap_or_else(|_| format!("sqlite://{}", tmp.join("skadi.db").display()));

    let store = Store::connect(&db_url).expect("connect store");
    store.run_migrations().await.expect("migrations");

    // Enqueue a real torrent (as the daemon's DbDownloader will), pointing the
    // per-job download paths at temp dirs.
    let job = store
        .enqueue(&NewDownloadJob {
            acquirable_ref: "it-ref".into(),
            source: UBUNTU_TORRENT.into(),
            category: Some("movies".into()),
            incomplete_dir: Some(tmp.join("incomplete").to_string_lossy().into_owned()),
            complete_dir: Some(tmp.join("complete").to_string_lossy().into_owned()),
        })
        .await
        .expect("enqueue");

    // Run the agent in the background against the same database.
    let cfg = Config {
        download_dir: tmp.join("dl"),
        state_dir: tmp.join("state"),
        port_range: 26901..26911,
        database_url: db_url.clone(),
        worker_id: "it-worker".into(),
        poll_interval: Duration::from_secs(1),
        tick_interval: Duration::from_secs(1),
        watch_dir: None,
        migrate: false,
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

    // The agent should claim the job (queued → downloading), resolve the
    // info-hash, and report progress.
    let mut progressed = false;
    for _ in 0..60 {
        let j = store
            .get_download(&job.id)
            .await
            .expect("get")
            .expect("row");
        if j.status == DownloadJobStatus::Error {
            panic!("job errored: {:?}", j.error);
        }
        if j.status == DownloadJobStatus::Downloading
            && j.progress_bytes > 0
            && j.info_hash.is_some()
        {
            progressed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(
        progressed,
        "agent claimed the queued job and reported progress"
    );

    // Request removal; the agent should action librqbit and mark it removed.
    store
        .request_remove(&job.id, true)
        .await
        .expect("request_remove");
    let mut removed = false;
    for _ in 0..30 {
        let j = store
            .get_download(&job.id)
            .await
            .expect("get")
            .expect("row");
        if j.status == DownloadJobStatus::Removed {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(
        removed,
        "agent actioned remove_requested and marked it removed"
    );

    agent.abort();
    let _ = std::fs::remove_dir_all(&tmp);
}
