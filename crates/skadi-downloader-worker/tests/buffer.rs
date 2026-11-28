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
        buffer_limits: skadi_downloader_worker::buffer::Limits::none(),
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

// ---- Limits, eviction and the seed limit (SKADI-T-0784) ----

/// Another payload and its `.torrent` beside `l`'s, under `name`.
async fn another_torrent(l: &Layout, name: &str, salt: u8) -> (PathBuf, Vec<u8>) {
    let dir = l.base.join(format!("src-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let payload: Vec<u8> = (0..1_500_000u32)
        .map(|i| ((i % 241) as u8).wrapping_add(salt))
        .collect();
    let src = dir.join("episode.mkv");
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
    let torrent = l.base.join(format!("{name}.torrent"));
    std::fs::write(&torrent, t.as_bytes().unwrap()).unwrap();
    (torrent, payload)
}

async fn enqueue_as(store: &Store, l: &Layout, reff: &str, torrent: &Path) -> DownloadJob {
    store
        .enqueue(&NewDownloadJob {
            acquirable_ref: reff.into(),
            source: torrent.to_string_lossy().into_owned(),
            category: None,
            incomplete_dir: Some(l.incomplete.to_string_lossy().into_owned()),
            complete_dir: Some(l.complete.to_string_lossy().into_owned()),
        })
        .await
        .unwrap()
}

async fn wait_status(store: &Store, id: &str, want: DownloadJobStatus, secs: u64) -> DownloadJob {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let row = store.get_download(id).await.unwrap().unwrap();
        if row.status == want {
            return row;
        }
        assert!(
            row.status != DownloadJobStatus::Error,
            "job failed: {:?}",
            row.error
        );
        assert!(
            std::time::Instant::now() < deadline,
            "job is {:?}, not {want:?}, after {secs}s",
            row.status
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn place(dir: &Path, payload: &[u8]) -> PathBuf {
    let f = dir.join("episode.mkv");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&f, payload).unwrap();
    f
}

/// A download bigger than the cap is held as `buffer_too_small` with the
/// reason, nothing of it stays in the buffer, and a resume tries it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_bigger_than_the_cap_is_held_with_the_reason() {
    let l = layout("buffer-cap").await;
    let mut cfg = config(&l, Some(l.buffer.clone()), 27931);
    cfg.buffer_limits = skadi_downloader_worker::buffer::Limits {
        cap_bytes: Some(1_000_000),
        floor_bytes: None,
    };
    let agent = tokio::spawn(run(cfg.clone()));
    let store = store_of(&cfg).await;
    let job = enqueue(&store, &l).await;
    let held = wait_status(&store, &job.id, DownloadJobStatus::BufferTooSmall, 60).await;
    let reason = held.error.clone().unwrap_or_default();
    assert!(reason.contains("1.5 MB"), "{reason}");
    assert!(reason.contains("cap is 1.0 MB"), "{reason}");
    assert!(reason.contains("SKADI_WORKER_BUFFER_MAX_GB"), "{reason}");
    assert!(files_under(&l.complete).is_empty());
    assert!(files_under(&l.buffer.join(REF)).is_empty());

    // A person's retry: back in the queue, checked again under the same
    // limits, and held again.
    store.resume(&job.id).await.unwrap();
    let back = store.get_download(&job.id).await.unwrap().unwrap();
    assert_eq!(back.error, None);
    wait_status(&store, &job.id, DownloadJobStatus::BufferTooSmall, 60).await;

    agent.abort();
    let _ = std::fs::remove_dir_all(&l.base);
}

/// The cap holds one download: the second evicts the first's seeding copy.
/// The first stops seeding (`seeded`), its buffer copy goes, its library copy
/// stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_download_evicts_the_oldest_seed_and_the_library_copy_stays() {
    let l = layout("buffer-evict").await;
    let (second, second_payload) = another_torrent(&l, "second", 7).await;
    let first_in_buffer = place(&l.buffer.join(REF), &l.payload);
    let second_in_buffer = place(&l.buffer.join("Second Show S01E01"), &second_payload);

    let mut cfg = config(&l, Some(l.buffer.clone()), 27941);
    cfg.buffer_limits = skadi_downloader_worker::buffer::Limits {
        cap_bytes: Some(2_000_000),
        floor_bytes: None,
    };
    let agent = tokio::spawn(run(cfg.clone()));
    let store = store_of(&cfg).await;
    let first = enqueue(&store, &l).await;
    wait_completed(&store, &first.id, 90).await;
    let first_in_library = l.complete.join(REF).join("episode.mkv");
    assert!(first_in_library.exists());

    let second_job = enqueue_as(&store, &l, "Second Show S01E01", &second).await;
    wait_completed(&store, &second_job.id, 90).await;

    let evicted = wait_status(&store, &first.id, DownloadJobStatus::Seeded, 30).await;
    assert_eq!(evicted.status, DownloadJobStatus::Seeded);
    assert!(
        !first_in_buffer.exists(),
        "the evicted buffer copy is deleted"
    );
    assert_eq!(
        std::fs::read(&first_in_library).unwrap(),
        l.payload,
        "the library copy stays"
    );
    assert!(
        second_in_buffer.exists(),
        "the new download seeds from the buffer"
    );
    assert_eq!(
        std::fs::read(l.complete.join("Second Show S01E01/episode.mkv")).unwrap(),
        second_payload
    );

    agent.abort();
    let _ = std::fs::remove_dir_all(&l.base);
}

/// At the seed limit the buffer copy is deleted and the library copy stays.
/// The seed time is set in minutes, so this test takes a little over one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_seed_limit_deletes_the_buffer_copy_and_the_library_copy_stays() {
    use skadi_store::{ConfigRepo, ConfigSource};
    let l = layout("buffer-seedlimit").await;
    let in_buffer = place(&l.buffer.join(REF), &l.payload);
    let cfg = config(&l, Some(l.buffer.clone()), 27951);
    let agent = tokio::spawn(run(cfg.clone()));
    let store = store_of(&cfg).await;
    // The seed policy is live config: one minute, then stop.
    store
        .set_config("worker.seed_time_mins", "1", ConfigSource::Env)
        .await
        .unwrap();
    let job = enqueue(&store, &l).await;
    wait_completed(&store, &job.id, 90).await;
    let in_library = l.complete.join(REF).join("episode.mkv");
    assert!(in_buffer.exists());

    wait_status(&store, &job.id, DownloadJobStatus::Seeded, 120).await;
    assert!(
        !in_buffer.exists(),
        "the buffer copy is deleted at the seed limit"
    );
    assert!(!l.buffer.join(REF).exists());
    assert_eq!(
        std::fs::read(&in_library).unwrap(),
        l.payload,
        "the library copy stays"
    );

    agent.abort();
    let _ = std::fs::remove_dir_all(&l.base);
}
