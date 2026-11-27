//! The download buffer end to end, offline (SKADI-T-0783).
//!
//! The worker runs standalone over a throwaway SQLite database. Each test makes
//! a `.torrent` of a local file and puts the file's bytes where the worker must
//! download it, so librqbit's first check finds the torrent finished with no
//! peer: a worker that looks in the wrong folder never finishes, and the test
//! fails on its timeout. DHT is off; the torrent has no trackers.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use librqbit::spawn_utils::BlockingSpawner;
use librqbit::{CreateTorrentOptions, create_torrent};
use skadi_downloader_worker::{Config, SeedPolicy, run};
use skadi_store::{DownloadJob, DownloadJobRepo, DownloadJobStatus, NewDownloadJob, Store};

const REF: &str = "Some Show S01E01";

struct Layout {
    base: PathBuf,
    incomplete: PathBuf,
    complete: PathBuf,
    buffer: PathBuf,
    payload: Vec<u8>,
    torrent: PathBuf,
}

/// A library tree, a buffer folder, a payload file and its `.torrent`.
async fn layout(tag: &str) -> Layout {
    let base = skadi_core::unique_temp_path(tag);
    let lib = base.join("lib/downloads");
    let src_dir = base.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let payload: Vec<u8> = (0..1_500_000u32).map(|i| (i % 253) as u8).collect();
    let src = src_dir.join("episode.mkv");
    std::fs::write(&src, &payload).unwrap();
    let t = create_torrent(
        &src,
        CreateTorrentOptions {
            name: None,
            trackers: Vec::new(),
            piece_length: Some(65536),
        },
        &BlockingSpawner::new(2),
    )
    .await
    .unwrap();
    let torrent = base.join("episode.torrent");
    std::fs::write(&torrent, t.as_bytes().unwrap()).unwrap();
    Layout {
        incomplete: lib.join("incomplete"),
        complete: lib.join("complete"),
        buffer: base.join("buffer"),
        base,
        payload,
        torrent,
    }
}

fn config(l: &Layout, buffer: Option<PathBuf>, port: u16) -> Config {
    // The lab knob: no public DHT for a test torrent.
    // SAFETY: every test in this binary sets the same value.
    unsafe { std::env::set_var("SKADI_WORKER_DISABLE_DHT", "1") };
    Config {
        download_dir: l.complete.clone(),
        state_dir: l.base.join("lib/downloads/.rqbit-session"),
        buffer_dir: buffer,
        port_range: port..port + 1,
        database_url: format!("sqlite://{}", l.base.join("worker.db").display()),
        worker_id: "buffer-test".into(),
        poll_interval: Duration::from_millis(300),
        tick_interval: Duration::from_millis(300),
        watch_dir: None,
        migrate: true,
        seed_policy: SeedPolicy::unlimited(),
        down_limit_bps: None,
        up_limit_bps: None,
        max_active: None,
        stall_timeout_secs: None,
        metadata_timeout_secs: None,
        lease_secs: 120,
        extra_trackers: Vec::new(),
    }
}

async fn store_of(cfg: &Config) -> Store {
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(s) = Store::connect(&cfg.database_url)
            && s.list_downloads().await.is_ok()
        {
            return s;
        }
    }
}

async fn enqueue(store: &Store, l: &Layout) -> DownloadJob {
    store
        .enqueue(&NewDownloadJob {
            acquirable_ref: REF.into(),
            source: l.torrent.to_string_lossy().into_owned(),
            category: None,
            incomplete_dir: Some(l.incomplete.to_string_lossy().into_owned()),
            complete_dir: Some(l.complete.to_string_lossy().into_owned()),
        })
        .await
        .unwrap()
}

async fn wait_completed(store: &Store, id: &str, secs: u64) -> DownloadJob {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let row = store.get_download(id).await.unwrap().unwrap();
        match row.status {
            DownloadJobStatus::Completed => return row,
            DownloadJobStatus::Error => panic!("job failed: {:?}", row.error),
            _ if std::time::Instant::now() > deadline => {
                panic!("job not complete after {secs}s: {:?}", row.status)
            }
            _ => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_a_buffer_the_download_is_in_the_buffer_and_copied_to_the_library() {
    let l = layout("buffer-e2e").await;
    // The torrent's bytes are where a buffer download goes.
    let in_buffer = l.buffer.join(REF).join("episode.mkv");
    std::fs::create_dir_all(in_buffer.parent().unwrap()).unwrap();
    std::fs::write(&in_buffer, &l.payload).unwrap();

    let cfg = config(&l, Some(l.buffer.clone()), 27901);
    let agent = tokio::spawn(run(cfg.clone()));
    let store = store_of(&cfg).await;
    let job = enqueue(&store, &l).await;
    let row = wait_completed(&store, &job.id, 90).await;

    // Reported: the copy in the complete folder on the library volume.
    let in_library = l.complete.join(REF).join("episode.mkv");
    assert_eq!(
        row.files,
        vec![in_library.to_string_lossy().into_owned()],
        "the job reports the library copy"
    );
    assert_eq!(std::fs::read(&in_library).unwrap(), l.payload);
    // A new file, not a hard link of the buffer copy.
    let (b, c) = (
        std::fs::metadata(&in_buffer).unwrap(),
        std::fs::metadata(&in_library).unwrap(),
    );
    assert_ne!(b.ino(), c.ino());
    assert_eq!(c.nlink(), 1);
    // No data on the library volume's incomplete side.
    assert!(
        files_under(&l.incomplete).is_empty(),
        "nothing written under the library's incomplete folder: {:?}",
        files_under(&l.incomplete)
    );
    // The session state is on the buffer, not on the library volume.
    assert!(l.buffer.join(".rqbit-session").is_dir());
    assert!(
        !cfg.state_dir.exists(),
        "no session folder on the library volume"
    );
    // No temporary copy left.
    assert!(
        files_under(&l.complete)
            .iter()
            .all(|p| !p.to_string_lossy().ends_with(".skadi-copy"))
    );

    // It goes on seeding from the buffer copy: the row stays Completed and the
    // buffer file stays.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = store.get_download(&job.id).await.unwrap().unwrap();
    assert_eq!(later.status, DownloadJobStatus::Completed);
    assert!(in_buffer.exists());

    agent.abort();
    let _ = std::fs::remove_dir_all(&l.base);
}

/// No buffer: as before, the download is in the incomplete folder and the
/// complete folder holds a hard link of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_buffer_nothing_changes() {
    let l = layout("buffer-off").await;
    let in_incomplete = l.incomplete.join(REF).join("episode.mkv");
    std::fs::create_dir_all(in_incomplete.parent().unwrap()).unwrap();
    std::fs::write(&in_incomplete, &l.payload).unwrap();

    let cfg = config(&l, None, 27911);
    let agent = tokio::spawn(run(cfg.clone()));
    let store = store_of(&cfg).await;
    let job = enqueue(&store, &l).await;
    let row = wait_completed(&store, &job.id, 90).await;

    let in_complete = l.complete.join(REF).join("episode.mkv");
    assert_eq!(row.files, vec![in_complete.to_string_lossy().into_owned()]);
    assert_eq!(
        std::fs::metadata(&in_incomplete).unwrap().ino(),
        std::fs::metadata(&in_complete).unwrap().ino(),
        "a hard link, as before"
    );
    assert!(cfg.state_dir.is_dir(), "the session stays where it was");
    assert!(!l.buffer.exists());

    agent.abort();
    let _ = std::fs::remove_dir_all(&l.base);
}

/// A job that started on the library volume before the buffer was set finishes
/// and seeds there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_already_on_the_library_volume_finishes_there() {
    let l = layout("buffer-old").await;
    let in_incomplete = l.incomplete.join(REF).join("episode.mkv");
    std::fs::create_dir_all(in_incomplete.parent().unwrap()).unwrap();
    std::fs::write(&in_incomplete, &l.payload).unwrap();

    // The row as a worker without a buffer left it: claimed, hash known, some
    // bytes down, then back in the queue after a restart.
    let db = format!("sqlite://{}", l.base.join("worker.db").display());
    let pre = Store::connect(&db).unwrap();
    pre.run_migrations().await.unwrap();
    let job = enqueue(&pre, &l).await;
    pre.claim_next("old-worker").await.unwrap().unwrap();
    pre.update_progress(
        &job.id,
        &skadi_store::DownloadProgress {
            progress_bytes: 65536,
            total_bytes: l.payload.len() as i64,
            info_hash: Some("aa".repeat(20)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    pre.reclaim_orphaned_downloads().await.unwrap();

    let cfg = config(&l, Some(l.buffer.clone()), 27921);
    let agent = tokio::spawn(run(cfg.clone()));
    let store = store_of(&cfg).await;
    let row = wait_completed(&store, &job.id, 90).await;

    let in_complete = l.complete.join(REF).join("episode.mkv");
    assert_eq!(row.files, vec![in_complete.to_string_lossy().into_owned()]);
    assert_eq!(
        std::fs::metadata(&in_incomplete).unwrap().ino(),
        std::fs::metadata(&in_complete).unwrap().ino(),
        "finished where it was, linked as before"
    );
    assert!(files_under(&l.buffer.join(REF)).is_empty());

    agent.abort();
    let _ = std::fs::remove_dir_all(&l.base);
}
