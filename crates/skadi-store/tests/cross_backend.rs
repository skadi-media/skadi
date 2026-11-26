//! Cross-backend conformance harness.
//!
//! Runs the same migration and repository assertions against every available
//! backend, so SQLite and Postgres are proven to behave identically. SQLite
//! always runs (temp file); Postgres runs when `SKADI_TEST_DATABASE_URL` is set
//! (e.g. the `docker compose` postgres service) and is skipped — not failed —
//! otherwise, so the default local `cargo test` needs no database.
//!
//! Bring up the Postgres backend with:
//! ```text
//! docker compose up -d postgres
//! export SKADI_TEST_DATABASE_URL=postgres://skadi:skadi@127.0.0.1:5433/skadi
//! ```
//!
//! The URL names an admin database; the harness never runs against it. It
//! creates one throwaway database per run and drops it at the end
//! (SKADI-T-0714): in a workspace run the domain crates' tests share the
//! same Postgres and apply their own migrations, so the schema this harness
//! reverts and re-applies is not the one it expects.

use chrono::{DateTime, Duration, TimeZone, Utc};
use diesel::{Connection, PgConnection, RunQueryDsl};
use skadi_store::{
    ConfigRepo, ConfigSource, DomainState, DomainStateRepo, LibraryFileRepo, LibraryFileStat,
    ProbeState, StorageProbeRepo, Store,
};

fn unique_sqlite_url() -> String {
    let path = skadi_core::unique_temp_path("xbtest").with_extension("db");
    format!("sqlite://{}", path.display())
}

/// The backends under test, and the throwaway Postgres database that lives
/// as long as this value.
struct Backends {
    list: Vec<(&'static str, String)>,
    _scratch: Option<ScratchPostgres>,
}

fn backends() -> Backends {
    let mut list = vec![("sqlite", unique_sqlite_url())];
    let scratch = match std::env::var("SKADI_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let scratch = ScratchPostgres::create(&url);
            list.push(("postgres", scratch.url.clone()));
            Some(scratch)
        }
        _ => {
            eprintln!("SKADI_TEST_DATABASE_URL unset — skipping Postgres backend");
            None
        }
    };
    Backends {
        list,
        _scratch: scratch,
    }
}

/// A Postgres database created from the admin URL for one run, dropped on
/// `Drop`. The same pattern as `TestDb` in `skadi-testsupport`, which this
/// crate cannot depend on without a cycle.
struct ScratchPostgres {
    admin_url: String,
    db_name: String,
    url: String,
}

impl ScratchPostgres {
    fn create(admin_url: &str) -> Self {
        let db_name = format!("skadi_xb_{}", uuid::Uuid::new_v4().simple());
        let mut conn =
            PgConnection::establish(admin_url).expect("connect to the admin postgres database");
        // Other test binaries create their databases at the same time, and
        // concurrent CREATE DATABASE statements contend on template1 with
        // "source database … is being accessed by other users". Retry those.
        for attempt in 0u64..10 {
            match diesel::sql_query(format!("CREATE DATABASE {db_name}")).execute(&mut conn) {
                Ok(_) => break,
                Err(e)
                    if e.to_string().contains("being accessed by other users") && attempt < 9 =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
                }
                Err(e) => panic!("CREATE DATABASE {db_name}: {e} (does the role have CREATEDB?)"),
            }
        }
        Self {
            admin_url: admin_url.to_string(),
            url: with_database(admin_url, &db_name),
            db_name,
        }
    }
}

impl Drop for ScratchPostgres {
    fn drop(&mut self) {
        // Best effort, and never a panic in Drop. The store's pool may still
        // hold a connection while it winds down, so refuse new connections,
        // terminate the open ones, then drop, with a short retry.
        let Ok(mut conn) = PgConnection::establish(&self.admin_url) else {
            return;
        };
        let db_name = &self.db_name;
        let _ = diesel::sql_query(format!(
            "UPDATE pg_database SET datallowconn = false WHERE datname = '{db_name}'"
        ))
        .execute(&mut conn);
        for attempt in 0u64..20 {
            let _ = diesel::sql_query(format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = '{db_name}' AND pid <> pg_backend_pid()"
            ))
            .execute(&mut conn);
            match diesel::sql_query(format!("DROP DATABASE IF EXISTS {db_name}")).execute(&mut conn)
            {
                Ok(_) => return,
                Err(e) if e.to_string().contains("being accessed by other users") => {
                    std::thread::sleep(std::time::Duration::from_millis(25 * (attempt + 1)));
                }
                Err(_) => return,
            }
        }
    }
}

/// `postgres://…/<db>[?query]` with the database part replaced.
fn with_database(url: &str, db_name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let rebuilt = match base.rsplit_once('/') {
        Some((prefix, _db)) => format!("{prefix}/{db_name}"),
        None => format!("{base}/{db_name}"),
    };
    match query {
        Some(q) => format!("{rebuilt}?{q}"),
        None => rebuilt,
    }
}

/// Migration round-trip (up → down → up) leaving a clean, empty schema.
async fn migrate_clean(store: &Store, label: &str) {
    store.run_migrations().await.unwrap();
    let reverted = store.revert_migrations().await.unwrap();
    assert!(reverted >= 1, "{label}: revert should undo >= 1 migration");
    let reapplied = store.run_migrations().await.unwrap();
    assert_eq!(
        reapplied, reverted,
        "{label}: re-applying should match the reverted count"
    );
}

/// The behavioral contract `DomainStateRepo` must satisfy on every backend.
async fn assert_domain_state_contract(store: &Store, label: &str) {
    assert!(
        store.list().await.unwrap().is_empty(),
        "{label}: starts empty"
    );

    let mut movies = DomainState::new("movies");
    movies.settings = serde_json::json!({ "root": "/movies", "quality": 1080 });
    store.upsert(&movies).await.unwrap();
    assert_eq!(
        store.get("movies").await.unwrap(),
        Some(movies.clone()),
        "{label}: round-trips settings JSON"
    );

    assert_eq!(store.get("music").await.unwrap(), None, "{label}: missing");

    let enabled = store.set_enabled("movies", true).await.unwrap();
    assert!(enabled.enabled, "{label}: enabled flag set");
    assert!(enabled.enabled_at.is_some(), "{label}: enabled_at stamped");
    assert_eq!(enabled.settings, movies.settings, "{label}: settings kept");

    // Creating-on-enable for an absent domain.
    store.set_enabled("books", true).await.unwrap();

    let all = store.list().await.unwrap();
    assert_eq!(all.len(), 2, "{label}: two domains");
    assert_eq!(all[0].name, "books", "{label}: ordered by name");
    assert_eq!(all[1].name, "movies");

    let disabled = store.set_enabled("movies", false).await.unwrap();
    assert!(!disabled.enabled, "{label}: disabled");
    assert!(disabled.enabled_at.is_none(), "{label}: enabled_at cleared");
}

/// The behavioral contract `ConfigRepo` must satisfy on every backend
/// (SKADI-T-0099).
async fn assert_config_contract(store: &Store, label: &str) {
    assert!(
        store.list_config().await.unwrap().is_empty(),
        "{label}: config starts empty"
    );
    assert_eq!(
        store.get_config("mode").await.unwrap(),
        None,
        "{label}: missing key is None"
    );

    // Insert (source=env).
    let e = store
        .set_config("mode", "testing", ConfigSource::Env)
        .await
        .unwrap();
    assert_eq!(e.key, "mode");
    assert_eq!(e.value, "testing");
    assert_eq!(e.source, ConfigSource::Env);
    let fp1 = store.config_fingerprint().await.unwrap();

    // Fingerprint is stable across a re-read (no write).
    assert_eq!(
        store.config_fingerprint().await.unwrap(),
        fp1,
        "{label}: fingerprint stable without a write"
    );

    // Upsert same key (source=runtime) overwrites value + source and bumps fp.
    let e2 = store
        .set_config("mode", "production", ConfigSource::Runtime)
        .await
        .unwrap();
    assert_eq!(e2.value, "production");
    assert_eq!(e2.source, ConfigSource::Runtime);
    assert_eq!(
        store.list_config().await.unwrap().len(),
        1,
        "{label}: upsert, not insert"
    );
    assert_ne!(
        store.config_fingerprint().await.unwrap(),
        fp1,
        "{label}: fingerprint changes on a value write"
    );

    // A second key; list is ordered by key.
    store
        .set_config("bind_addr", "0.0.0.0:9000", ConfigSource::Env)
        .await
        .unwrap();
    let all = store.list_config().await.unwrap();
    assert_eq!(
        all.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        vec!["bind_addr", "mode"],
        "{label}: list ordered by key"
    );

    // Delete.
    assert!(
        store.delete_config("mode").await.unwrap(),
        "{label}: deleted"
    );
    assert!(
        !store.delete_config("mode").await.unwrap(),
        "{label}: second delete is a no-op"
    );
    assert_eq!(store.get_config("mode").await.unwrap(), None);
}

fn t(minute: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap() + Duration::minutes(minute)
}

fn stat(kind: &str, path: &str, size_bytes: i64, links: i64) -> LibraryFileStat {
    LibraryFileStat {
        kind: kind.into(),
        acquirable_ref: format!("ref-{path}"),
        path: path.into(),
        dev: 64,
        ino: path.len() as i64,
        size_bytes,
        disk_bytes: size_bytes,
        links,
        mtime: t(0),
    }
}

const GB: i64 = 1_000_000_000;

/// The behavioral contract of the library storage tables on every backend
/// (SKADI-T-0772).
async fn assert_library_files_contract(store: &Store, label: &str) {
    // A new file: a row, its first sample, nothing measured.
    let a = store
        .record_library_file_stat(&stat("movie", "/lib/movie/a.mkv", 10 * GB, 1), t(1))
        .await
        .unwrap();
    assert_eq!(a.size_bytes, 10 * GB, "{label}");
    assert_eq!((a.pieces, a.pieces_at, a.checksum_at), (None, None, None));
    assert_eq!(a.stat_at, t(1), "{label}: timestamps round-trip");
    assert_eq!(a.mtime, t(0), "{label}");
    assert_eq!(
        store.library_file_samples(&a.id).await.unwrap().len(),
        1,
        "{label}: a new file gets its first sample"
    );

    // The same facts again: the row is read again, no sample.
    let again = store
        .record_library_file_stat(&stat("movie", "/lib/movie/a.mkv", 10 * GB, 1), t(2))
        .await
        .unwrap();
    assert_eq!(again.id, a.id, "{label}: the path is the key");
    assert_eq!(again.stat_at, t(2), "{label}: stat_at moves");
    assert_eq!(
        store.library_file_samples(&a.id).await.unwrap().len(),
        1,
        "{label}: nothing changed, no sample"
    );

    // Pieces and a checksum arrive; a changed piece count is one sample.
    assert!(
        store
            .record_library_file_pieces(&a.id, 5000, t(3))
            .await
            .unwrap()
    );
    assert!(
        store
            .record_library_file_checksum(&a.id, "abc", t(3))
            .await
            .unwrap()
    );
    assert!(
        store
            .record_library_file_pieces(&a.id, 5000, t(4))
            .await
            .unwrap()
    );
    let samples = store.library_file_samples(&a.id).await.unwrap();
    assert_eq!(samples.len(), 2, "{label}: same count again, no sample");
    assert_eq!(samples[1].pieces, Some(5000), "{label}");
    assert_eq!(samples[1].at, t(3), "{label}: oldest first");
    assert!(
        !store
            .record_library_file_pieces("no-such-id", 1, t(4))
            .await
            .unwrap(),
        "{label}: unknown file"
    );

    // Only the link count changes: one sample, and the measurements stay
    // current, because the content did not change.
    let linked = store
        .record_library_file_stat(&stat("movie", "/lib/movie/a.mkv", 10 * GB, 2), t(5))
        .await
        .unwrap();
    assert_eq!(
        linked.pieces_at,
        Some(t(4)),
        "{label}: pieces still current"
    );
    assert_eq!(linked.checksum_at, Some(t(3)), "{label}");
    let samples = store.library_file_samples(&a.id).await.unwrap();
    assert_eq!(samples.len(), 3, "{label}: changed links, one sample");
    assert_eq!(
        (samples[2].links, samples[2].pieces),
        (2, Some(5000)),
        "{label}: the sample carries the last known pieces"
    );

    // The size changes: one sample, and both measurements are queued again
    // with the last known values kept.
    let grown = store
        .record_library_file_stat(&stat("movie", "/lib/movie/a.mkv", 12 * GB, 2), t(6))
        .await
        .unwrap();
    assert_eq!(
        (grown.pieces, grown.pieces_at),
        (Some(5000), None),
        "{label}"
    );
    assert_eq!(
        (grown.checksum.as_deref(), grown.checksum_at),
        (Some("abc"), None),
        "{label}"
    );
    assert_eq!(
        store.library_file_samples(&a.id).await.unwrap().len(),
        4,
        "{label}: changed size, one sample"
    );

    // Gone, then found: gone_at is set once, and cleared by the next stat.
    assert!(store.mark_library_file_gone(&a.id, t(7)).await.unwrap());
    assert!(store.mark_library_file_gone(&a.id, t(8)).await.unwrap());
    let gone = store
        .library_file_by_path("/lib/movie/a.mkv")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gone.gone_at, Some(t(7)), "{label}: the first time is kept");
    assert!(
        !store
            .mark_library_file_gone("no-such-id", t(8))
            .await
            .unwrap(),
        "{label}"
    );
    let back = store
        .record_library_file_stat(&stat("movie", "/lib/movie/a.mkv", 12 * GB, 2), t(9))
        .await
        .unwrap();
    assert_eq!(back.gone_at, None, "{label}: found again");

    // Orphans: first_seen is kept, a file row's path is refused, and a stat of
    // the path claims the orphan.
    let o = "/lib/tv/Show/extra.mkv";
    assert!(
        store
            .record_library_orphan(o, 3 * GB, t(0), t(10))
            .await
            .unwrap()
    );
    assert!(
        store
            .record_library_orphan(o, 3 * GB, t(0), t(11))
            .await
            .unwrap()
    );
    assert!(
        store
            .record_library_orphan("/lib/tv/small.nfo", 10, t(0), t(11))
            .await
            .unwrap()
    );
    assert!(
        !store
            .record_library_orphan("/lib/movie/a.mkv", 12 * GB, t(0), t(11))
            .await
            .unwrap(),
        "{label}: a path a record holds is not an orphan"
    );
    let orphans = store.list_library_orphans(10, 0).await.unwrap();
    assert_eq!(orphans.len(), 2, "{label}");
    assert_eq!(orphans[0].path, o, "{label}: largest first");
    assert_eq!(
        (orphans[0].first_seen, orphans[0].last_seen),
        (t(10), t(11)),
        "{label}: first_seen kept, last_seen moved"
    );
    assert_eq!(
        store.list_library_orphans(10, 1).await.unwrap().len(),
        1,
        "{label}: offset"
    );
    let claimed = store
        .record_library_file_stat(&stat("series", o, 3 * GB, 1), t(12))
        .await
        .unwrap();
    let orphans = store.list_library_orphans(10, 0).await.unwrap();
    assert_eq!(
        orphans.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(),
        vec!["/lib/tv/small.nfo"],
        "{label}: a claimed path leaves the orphans"
    );
    assert_eq!(
        store.prune_library_orphans(t(12)).await.unwrap(),
        1,
        "{label}: not seen since the pass began"
    );
    assert!(store.list_library_orphans(10, 0).await.unwrap().is_empty());

    // More files for the queries. Pieces per GB: b 500, a 5000/12, c 100,
    // d equal to c. e is measured and gone; f has no size.
    let mut ids = std::collections::HashMap::new();
    for (path, kind, size, pieces) in [
        ("/lib/movie/b.mkv", "movie", 2 * GB, Some(1000)),
        ("/lib/tv/c.mkv", "series", 10 * GB, Some(1000)),
        ("/lib/tv/d.mkv", "series", 20 * GB, Some(2000)),
        ("/lib/tv/e.mkv", "series", GB, Some(900_000)),
        ("/lib/book/f.m4b", "audiobook", 0, Some(5)),
        ("/lib/book/g.m4b", "audiobook", GB, None),
    ] {
        let f = store
            .record_library_file_stat(&stat(kind, path, size, 1), t(20))
            .await
            .unwrap();
        if let Some(p) = pieces {
            store
                .record_library_file_pieces(&f.id, p, t(21))
                .await
                .unwrap();
        }
        ids.insert(path, f.id);
    }
    store
        .mark_library_file_gone(&ids["/lib/tv/e.mkv"], t(22))
        .await
        .unwrap();
    store
        .record_library_file_pieces(&a.id, 5000, t(23))
        .await
        .unwrap();

    let worst = store.worst_library_files(None, 10).await.unwrap();
    assert_eq!(
        worst.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec![
            "/lib/movie/b.mkv",
            "/lib/movie/a.mkv",
            "/lib/tv/c.mkv",
            "/lib/tv/d.mkv"
        ],
        "{label}: worst pieces per GB first, ties by path; gone, unmeasured and empty left out"
    );
    let worst = store.worst_library_files(Some("series"), 1).await.unwrap();
    assert_eq!(
        worst.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["/lib/tv/c.mkv"],
        "{label}: kind filter and limit"
    );

    let totals = store.library_totals_by_kind().await.unwrap();
    assert_eq!(
        totals.iter().map(|k| k.kind.as_str()).collect::<Vec<_>>(),
        vec!["audiobook", "movie", "series"],
        "{label}: kind order"
    );
    let movie = &totals[1];
    assert_eq!(
        (movie.files, movie.size_bytes, movie.linked, movie.gone),
        (2, 14 * GB, 1, 0),
        "{label}"
    );
    assert_eq!(
        (movie.measured, movie.measured_bytes, movie.pieces),
        (2, 14 * GB, 6000),
        "{label}"
    );
    let series = &totals[2];
    // c, d and the claimed orphan are on disk; e is gone and counts only there.
    assert_eq!(
        (series.files, series.size_bytes, series.gone),
        (3, 33 * GB, 1),
        "{label}"
    );
    assert_eq!(
        (series.measured, series.measured_bytes, series.pieces),
        (2, 30 * GB, 3000),
        "{label}: a gone file's pieces are not counted"
    );
    assert_eq!(
        (totals[0].files, totals[0].measured, totals[0].checksummed),
        (2, 1, 0),
        "{label}"
    );

    // The queues. Pieces: the claimed orphan (stat at 12), then g (20); the
    // gone file is not asked for. Checksum: nothing has a current one.
    let waiting = store.library_files_awaiting_pieces(10).await.unwrap();
    assert_eq!(
        waiting.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec![o, "/lib/book/g.m4b"],
        "{label}: unmeasured files, longest unread first"
    );
    assert_eq!(claimed.pieces_at, None);
    let waiting = store.library_files_awaiting_checksum(3).await.unwrap();
    assert_eq!(
        waiting.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["/lib/movie/a.mkv", o, "/lib/book/f.m4b"],
        "{label}: checksum queue, limit 3"
    );
    let oldest = store.library_files_oldest_pieces(2).await.unwrap();
    assert_eq!(
        oldest.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["/lib/book/f.m4b", "/lib/movie/b.mkv"],
        "{label}: oldest measurement first, ties by path"
    );
}

/// The behavioral contract of the probe state on every backend (SKADI-T-0772).
async fn assert_storage_probe_contract(store: &Store, label: &str) {
    assert_eq!(
        store.storage_probe_status("pieces").await.unwrap(),
        None,
        "{label}: a probe that never failed has no row"
    );
    assert!(
        !store
            .clear_storage_probe("pieces", "robin", t(0))
            .await
            .unwrap(),
        "{label}: nothing to clear"
    );

    store
        .fail_storage_probe("pieces", "filefrag: not found", t(1))
        .await
        .unwrap();
    let s = store.storage_probe_status("pieces").await.unwrap().unwrap();
    assert_eq!(s.state, ProbeState::Failed, "{label}");
    assert_eq!(s.failed_at, Some(t(1)), "{label}");
    assert_eq!(s.reason.as_deref(), Some("filefrag: not found"), "{label}");

    assert!(
        store
            .clear_storage_probe("pieces", "robin", t(2))
            .await
            .unwrap()
    );
    let s = store.storage_probe_status("pieces").await.unwrap().unwrap();
    assert_eq!(s.state, ProbeState::On, "{label}: cleared, may run");
    assert_eq!(
        (s.cleared_at, s.cleared_by.as_deref()),
        (Some(t(2)), Some("robin")),
        "{label}"
    );
    assert_eq!(
        s.failed_at,
        Some(t(1)),
        "{label}: the failure stays on record"
    );
    assert!(
        !store
            .clear_storage_probe("pieces", "sam", t(3))
            .await
            .unwrap(),
        "{label}: a second clear writes nothing"
    );

    // A new failure is not cleared by the old clear.
    store
        .fail_storage_probe("pieces", "host key changed", t(4))
        .await
        .unwrap();
    let s = store.storage_probe_status("pieces").await.unwrap().unwrap();
    assert_eq!(s.state, ProbeState::Failed, "{label}");
    assert_eq!((s.cleared_at, s.cleared_by), (None, None), "{label}");
    assert_eq!(s.reason.as_deref(), Some("host key changed"), "{label}");

    // Probes are separate.
    assert_eq!(
        store.storage_probe_status("checksum").await.unwrap(),
        None,
        "{label}: one probe's failure is not another's"
    );
}

#[tokio::test]
async fn backends_behave_identically() {
    let backends = backends();
    for (label, url) in &backends.list {
        let store = Store::connect(url).expect("connect");
        // up/down/up leaves an empty schema, which the repo contract then assumes.
        migrate_clean(&store, label).await;
        assert_domain_state_contract(&store, label).await;
        assert_config_contract(&store, label).await;
        assert_library_files_contract(&store, label).await;
        assert_storage_probe_contract(&store, label).await;
    }
}
