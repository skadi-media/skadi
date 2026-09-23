//! C07 retention steps: the age/size purges the hunter tick runs against the
//! store (history, blocklist expiry, download lease reclaim, traces).
use chrono::{Duration, Utc};
use cucumber::{given, then, when};

use skadi_store::{
    BlocklistRepo, DownloadJobRepo, HistoryEntry, HistoryRepo, NewBlocklistEntry, NewDownloadJob,
    TraceEvent, TraceRepo,
};

use crate::bdd_support::World;

fn history(i: usize, age_days: i64) -> HistoryEntry {
    HistoryEntry {
        id: format!("h-{i:03}-{age_days}"),
        at: Utc::now() - Duration::days(age_days),
        kind: "movie".into(),
        acquirable_ref: format!("movie:{i}"),
        label: format!("Movie {i}"),
        event: "grabbed".into(),
        detail: None,
        reason_code: None,
    }
}

#[given(expr = "{int} history rows aged {int} days and {int} rows aged {int} days")]
async fn history_rows(w: &mut World, n_old: usize, old: i64, n_new: usize, new: i64) {
    let store = w.store();
    for i in 0..n_old {
        store.record_history(&history(i, old)).await.unwrap();
    }
    for i in 0..n_new {
        store.record_history(&history(100 + i, new)).await.unwrap();
    }
}

#[when(expr = "history older than {int} days is purged")]
async fn purge_history(w: &mut World, days: i64) {
    let cutoff = Utc::now() - Duration::days(days);
    w.purged = Some(w.store().purge_history_before(cutoff).await.unwrap());
}

#[when(expr = "history is trimmed to the newest {int} rows")]
async fn trim_history(w: &mut World, n: i64) {
    w.purged = Some(w.store().trim_history_to_newest(n).await.unwrap());
}

#[then(expr = "{int} rows were purged and {int} history rows remain")]
async fn history_remaining(w: &mut World, purged: usize, remain: usize) {
    assert_eq!(w.purged, Some(purged));
    assert_eq!(w.store().list_history(1000, 0).await.unwrap().len(), remain);
}

#[given(
    expr = "a blocklist entry {string} that expired {int} minutes ago and a permanent entry {string}"
)]
async fn blocklist_rows(w: &mut World, expired: String, mins: i64, permanent: String) {
    let store = w.store();
    store
        .block(&NewBlocklistEntry {
            release_key: expired.clone(),
            title: expired,
            acquirable_ref: None,
            indexer: None,
            reason: Some("tracker hiccup".into()),
            expires_at: Some(Utc::now() - Duration::minutes(mins)),
        })
        .await
        .unwrap();
    store
        .block(&NewBlocklistEntry {
            release_key: permanent.clone(),
            title: permanent,
            acquirable_ref: None,
            indexer: None,
            reason: Some("manual".into()),
            expires_at: None,
        })
        .await
        .unwrap();
}

#[when("expired blocklist entries are purged")]
async fn purge_blocklist(w: &mut World) {
    w.purged = Some(w.store().purge_expired_blocklist().await.unwrap());
}

#[then(expr = "{string} is no longer blocked and {string} still is")]
async fn blocked_state(w: &mut World, gone: String, kept: String) {
    assert_eq!(w.purged, Some(1));
    let store = w.store();
    assert!(!store.is_blocked(&gone).await.unwrap());
    assert!(store.is_blocked(&kept).await.unwrap());
}

#[given(expr = "a download claimed by worker {string} whose {int}-second lease has lapsed")]
async fn lapsed_lease(w: &mut World, worker: String, _secs: i64) {
    let store = w.store();
    let job = store
        .enqueue(&NewDownloadJob {
            acquirable_ref: "movie:lease".into(),
            source: "magnet:?xt=urn:btih:abc".into(),
            category: None,
            incomplete_dir: None,
            complete_dir: None,
        })
        .await
        .unwrap();
    let claimed = store.claim_next(&worker).await.unwrap().expect("claimed");
    assert_eq!(claimed.id, job.id);
    // A lease that expired one second ago.
    store.heartbeat(&job.id, -1).await.unwrap();
    w.ids.insert("lease-job".into(), job.id);
}

#[when("expired download leases are reclaimed")]
async fn reclaim(w: &mut World) {
    w.purged = Some(w.store().reclaim_expired_downloads().await.unwrap());
}

#[then("the download is queued again with no worker")]
async fn requeued(w: &mut World) {
    assert_eq!(w.purged, Some(1));
    let id = w.ids["lease-job"].clone();
    let job = w.store().get_download(&id).await.unwrap().expect("job");
    assert_eq!(format!("{:?}", job.status), "Queued");
    assert_eq!(job.worker_id, None);
}

#[given(expr = "{int} trace events aged {int} days")]
async fn old_traces(w: &mut World, n: usize, days: i64) {
    let store = w.store();
    for i in 0..n {
        store
            .record_trace(&TraceEvent {
                id: format!("t-{i}"),
                at: Utc::now() - Duration::days(days),
                run_id: None,
                kind: "movie".into(),
                acquirable_ref: format!("movie:{i}"),
                stage: "searching".into(),
                event: "candidates_found".into(),
                message: "old".into(),
                detail: None,
            })
            .await
            .unwrap();
    }
}

/// Sonarr trims its log/history tables on a schedule. `trace_events` is
/// append-only with no purge in `TraceRepo` (`crates/skadi-store/src/trace.rs:52-60`)
/// and no caller in the hunter tick (`crates/skadi-hunter/src/worker.rs:582-604`).
#[then(expr = "trace events older than {int} days can be purged")]
async fn trace_purge(w: &mut World, days: i64) {
    let n = w.store().list_traces(1000, 0).await.unwrap().len();
    panic!(
        "TraceRepo has no retention operation (record/list/traces_for only): {n} trace rows \
         older than {days} days stay forever — the table grows unbounded"
    );
}
