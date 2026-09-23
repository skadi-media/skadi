//! The `downloads` queue protocol as the worker drives it (claim / lease /
//! progress / terminal states / removals / restarts) and the worker's pure
//! decision helpers (seed policy, stall, claim budget, bandwidth caps, paths,
//! config resolution).
use std::time::Duration;

use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use skadi_downloader_worker::seed_policy::{SeedAction, SeedPolicy, SeedVerdict};
use skadi_downloader_worker::{
    Config, bps_limit, claim_budget, effective_seed_policy, file_abs_path, is_stalled,
    map_to_complete,
};
use skadi_store::{
    DownloadCategory, DownloadJobRepo, DownloadJobStatus, DownloadProgress, NewDownloadJob,
};

use crate::bdd_support::{World, temp_store};

fn status_of(s: &str) -> DownloadJobStatus {
    match s {
        "queued" => DownloadJobStatus::Queued,
        "downloading" => DownloadJobStatus::Downloading,
        "paused" => DownloadJobStatus::Paused,
        "stalled" => DownloadJobStatus::Stalled,
        "completed" => DownloadJobStatus::Completed,
        "seeded" => DownloadJobStatus::Seeded,
        "error" => DownloadJobStatus::Error,
        "remove_requested" => DownloadJobStatus::RemoveRequested,
        "removed" => DownloadJobStatus::Removed,
        other => panic!("unknown status {other:?}"),
    }
}

// --- the queue -----------------------------------------------------------------------

#[given("an empty downloads queue")]
async fn empty_queue(w: &mut World) {
    w.store = Some(temp_store().await);
}

#[given(regex = r#"^the daemon enqueued "([^"]+)" from "([^"]*)"$"#)]
async fn enqueue(w: &mut World, name: String, source: String) {
    let row = w
        .store()
        .enqueue(&NewDownloadJob {
            acquirable_ref: name.clone(),
            source,
            category: Some("2000".into()),
            incomplete_dir: Some("/data/incomplete".into()),
            complete_dir: Some("/data/complete".into()),
        })
        .await
        .expect("enqueue");
    w.names.insert(row.id.clone(), name.clone());
    w.jobs.insert(name, row.id);
    // Keep created_at strictly ordered on fast machines (second granularity is
    // enough for SQLite's ordering here; a tiny sleep avoids ties).
    tokio::time::sleep(Duration::from_millis(5)).await;
}

#[when(regex = r#"^worker "([^"]+)" claims the next job$"#)]
async fn claim(w: &mut World, worker: String) {
    let got = w.store().claim_next(&worker).await.expect("claim_next");
    w.claimed = Some(got);
}

#[then(regex = r#"^the claim yields "([^"]+)" now downloading for worker "([^"]+)"$"#)]
async fn claim_yields(w: &mut World, name: String, worker: String) {
    let job = w
        .claimed
        .as_ref()
        .expect("a claim was made")
        .as_ref()
        .expect("expected a job, got none");
    assert_eq!(w.names.get(&job.id), Some(&name), "claimed {}", job.id);
    assert_eq!(job.status, DownloadJobStatus::Downloading);
    assert_eq!(job.worker_id.as_deref(), Some(worker.as_str()));
    let fresh = w.job(&name).await;
    assert_eq!(fresh.status, DownloadJobStatus::Downloading);
    assert!(
        fresh.lease_expires_at.is_none(),
        "lease is NULL until the first heartbeat"
    );
}

#[then("the claim yields nothing")]
fn claim_none(w: &mut World) {
    assert!(
        matches!(w.claimed, Some(None)),
        "{:?}",
        w.claimed.as_ref().map(|c| c.as_ref().map(|j| &j.id))
    );
}

#[when(regex = r#"^the worker heartbeats "([^"]+)" with a lease of (-?\d+) seconds$"#)]
async fn heartbeat(w: &mut World, name: String, secs: i64) {
    let id = w.id(&name).to_string();
    w.store().heartbeat(&id, secs).await.expect("heartbeat");
}

#[when("the worker sweeps lapsed leases")]
async fn sweep(w: &mut World) {
    w.reclaimed = Some(
        w.store()
            .reclaim_expired_downloads()
            .await
            .expect("reclaim"),
    );
}

#[when("a fresh worker starts up and re-queues orphaned transfers")]
async fn startup(w: &mut World) {
    w.reclaimed = Some(
        w.store()
            .reclaim_orphaned_downloads()
            .await
            .expect("reclaim"),
    );
}

#[then(regex = r"^(\d+) jobs? (?:was|were) re-queued$")]
fn n_reclaimed(w: &mut World, n: usize) {
    assert_eq!(w.reclaimed, Some(n));
}

#[when(
    regex = r#"^the worker reports "([^"]+)" at (\d+) of (\d+) bytes with info hash "([^"]*)" and (\d+) peers$"#
)]
async fn progress(w: &mut World, name: String, done: i64, total: i64, hash: String, peers: i32) {
    let id = w.id(&name).to_string();
    w.store()
        .update_progress(
            &id,
            &DownloadProgress {
                progress_bytes: done,
                total_bytes: total,
                info_hash: Some(hash),
                peers: Some(peers),
                peers_seen: Some(peers),
                down_speed_bps: Some(1_000_000),
                uploaded_bytes: Some(0),
                // What `progress_from_stats` writes for a transfer the client has
                // not made live yet (SKADI-T-0394); the unit test
                // `progress_carries_the_client_transfer_state` covers the mapping
                // from librqbit's state, this covers the row round-trip.
                client_state: Some("initializing".into()),
                ..Default::default()
            },
        )
        .await
        .expect("update_progress");
}

#[when(regex = r#"^the worker marks "([^"]+)" complete with files "([^"]*)"$"#)]
async fn complete(w: &mut World, name: String, files: String) {
    let id = w.id(&name).to_string();
    let files: Vec<String> = files.split(',').map(str::trim).map(String::from).collect();
    w.store()
        .mark_complete(&id, &files)
        .await
        .expect("mark_complete");
}

#[when(regex = r#"^the worker marks "([^"]+)" seeded$"#)]
async fn seeded(w: &mut World, name: String) {
    let id = w.id(&name).to_string();
    w.store().mark_seeded(&id).await.expect("mark_seeded");
}

#[when(regex = r#"^the worker marks "([^"]+)" failed with "([^"]*)"$"#)]
async fn failed(w: &mut World, name: String, reason: String) {
    let id = w.id(&name).to_string();
    w.store()
        .mark_error(&id, &reason)
        .await
        .expect("mark_error");
}

#[when(regex = r#"^the worker flags "([^"]+)" (stalled|unstalled)$"#)]
async fn stalled(w: &mut World, name: String, which: String) {
    let id = w.id(&name).to_string();
    w.store()
        .set_download_stalled(&id, which == "stalled")
        .await
        .expect("set_download_stalled");
}

#[when(regex = r#"^the operator pauses "([^"]+)"$"#)]
async fn pause(w: &mut World, name: String) {
    let id = w.id(&name).to_string();
    w.store().request_pause(&id).await.expect("request_pause");
}

#[when(regex = r#"^the operator resumes "([^"]+)"$"#)]
async fn resume(w: &mut World, name: String) {
    let id = w.id(&name).to_string();
    w.store().resume(&id).await.expect("resume");
}

#[when(regex = r#"^the daemon requests removal of "([^"]+)" (keeping|deleting) its data$"#)]
async fn request_remove(w: &mut World, name: String, mode: String) {
    let id = w.id(&name).to_string();
    w.store()
        .request_remove(&id, mode == "deleting")
        .await
        .expect("request_remove");
}

#[when(regex = r#"^the worker tears down "([^"]+)"$"#)]
async fn teardown(w: &mut World, name: String) {
    let id = w.id(&name).to_string();
    w.store().mark_removed(&id).await.expect("mark_removed");
}

#[then(
    regex = r#"^"([^"]+)" is (queued|downloading|paused|stalled|completed|seeded|error|remove_requested|removed)$"#
)]
async fn is_status(w: &mut World, name: String, status: String) {
    let job = w.job(&name).await;
    assert_eq!(job.status, status_of(&status), "{job:?}");
}

#[then(regex = r#"^"([^"]+)" has no worker and no lease$"#)]
async fn no_worker(w: &mut World, name: String) {
    let job = w.job(&name).await;
    assert_eq!(job.worker_id, None, "{job:?}");
    assert_eq!(job.lease_expires_at, None, "{job:?}");
}

#[then(regex = r#"^"([^"]+)" still belongs to worker "([^"]+)"$"#)]
async fn belongs(w: &mut World, name: String, worker: String) {
    let job = w.job(&name).await;
    assert_eq!(job.worker_id.as_deref(), Some(worker.as_str()), "{job:?}");
}

#[then(regex = r#"^"([^"]+)" has a lease in the future$"#)]
async fn lease_future(w: &mut World, name: String) {
    let job = w.job(&name).await;
    let lease = job.lease_expires_at.expect("a lease");
    assert!(lease > chrono::Utc::now(), "{lease}");
}

#[then(regex = r#"^"([^"]+)" keeps info hash "([^"]*)" and (\d+) progress bytes$"#)]
async fn keeps(w: &mut World, name: String, hash: String, bytes: i64) {
    let job = w.job(&name).await;
    assert_eq!(job.info_hash.as_deref(), Some(hash.as_str()), "{job:?}");
    assert_eq!(job.progress_bytes, bytes, "{job:?}");
}

#[then(regex = r#"^"([^"]+)" shows (\d+) peers and a download speed$"#)]
async fn shows_peers(w: &mut World, name: String, peers: i32) {
    let job = w.job(&name).await;
    assert_eq!(job.peers, Some(peers), "{job:?}");
    assert!(job.down_speed_bps.is_some(), "{job:?}");
}

#[then(regex = r#"^"([^"]+)" has files "([^"]*)" and a completion time$"#)]
async fn has_files(w: &mut World, name: String, files: String) {
    let job = w.job(&name).await;
    let want: Vec<String> = files.split(',').map(str::trim).map(String::from).collect();
    assert_eq!(job.files, want, "{job:?}");
    assert!(job.completed_at.is_some(), "{job:?}");
}

#[then(regex = r#"^"([^"]+)" carries the error "([^"]*)"$"#)]
async fn has_error(w: &mut World, name: String, reason: String) {
    let job = w.job(&name).await;
    assert_eq!(job.error.as_deref(), Some(reason.as_str()), "{job:?}");
}

#[then(regex = r#"^the removal sweep lists "([^"]+)" with delete_data (true|false)$"#)]
async fn removal_lists(w: &mut World, name: String, delete: String) {
    let pending = w.store().list_remove_requested().await.expect("list");
    let id = w.id(&name);
    let row = pending
        .iter()
        .find(|j| j.id == id)
        .unwrap_or_else(|| panic!("{name} not in removal sweep: {pending:?}"));
    assert_eq!(row.delete_data, delete == "true");
}

#[then("the removal sweep is empty")]
async fn removal_empty(w: &mut World) {
    let pending = w.store().list_remove_requested().await.expect("list");
    assert!(pending.is_empty(), "{pending:?}");
}

#[then(regex = r"^(\d+) transfers? count(?:s)? as active$")]
async fn active(w: &mut World, n: i64) {
    assert_eq!(w.store().count_active_downloads().await.expect("count"), n);
}

/// SKADI-T-0394: the daemon cannot tell "waiting in librqbit's init/hash queue"
/// from "live with no peers"; the row needs the client state.
#[then(regex = r#"^"([^"]+)" records the client state "([^"]*)"$"#)]
async fn client_state(w: &mut World, name: String, state: String) {
    let job = w.job(&name).await;
    let dbg = format!("{job:?}");
    assert!(
        dbg.contains("client_state") && dbg.contains(&state),
        "the downloads row carries no client_state (want {state:?}): {dbg}"
    );
}

// --- pure decision helpers --------------------------------------------------------------

#[given(
    regex = r#"^a seed policy of ratio ([0-9.]+) and (\d+) minutes with action "(stop|remove)"$"#
)]
fn seed_policy(w: &mut World, ratio: f64, mins: u64, action: String) {
    w.policy = Some(SeedPolicy::from_config(
        ratio,
        mins,
        SeedAction::parse(&action),
    ));
}

#[given(regex = r#"^the category overrides ratio "([^"]*)" time "([^"]*)" action "([^"]*)"$"#)]
fn category_override(w: &mut World, ratio: String, mins: String, action: String) {
    let cat = DownloadCategory {
        name: "movies".into(),
        save_path: None,
        seed_ratio: ratio.parse().ok(),
        seed_time_mins: mins.parse().ok(),
        seed_action: (!action.is_empty()).then_some(action),
    };
    let global = w.policy.expect("a global policy");
    w.policy = Some(effective_seed_policy(global, &cat));
}

#[when(regex = r"^a seed has uploaded (\d+) of (\d+) downloaded bytes after (\d+) seconds$")]
fn verdict(w: &mut World, up: i64, down: i64, secs: i64) {
    w.verdict = Some(w.policy.expect("policy").verdict(up, down, secs));
}

#[then(regex = r#"^the seed verdict is "(continue|stop|remove)"$"#)]
fn verdict_is(w: &mut World, want: String) {
    let got = match w.verdict.expect("verdict") {
        SeedVerdict::Continue => "continue",
        SeedVerdict::Stop => "stop",
        SeedVerdict::Remove => "remove",
    };
    assert_eq!(got, want, "policy {:?}", w.policy);
}

#[then(
    regex = r"^the effective policy is ratio ([0-9.]+|unlimited) and time ([0-9]+|unlimited) seconds$"
)]
fn effective(w: &mut World, ratio: String, secs: String) {
    let p = w.policy.expect("policy");
    let want_ratio = ratio.parse::<f64>().ok();
    let want_secs = secs.parse::<i64>().ok();
    assert_eq!(p.ratio_limit, want_ratio, "{p:?}");
    assert_eq!(p.time_limit_secs, want_secs, "{p:?}");
}

#[then(
    regex = r"^a transfer with (\d+) peers and (\d+) seconds without progress under a (\d+) second stall timeout (is|is not) stalled$"
)]
fn stalled_rule(_w: &mut World, peers: i32, secs: i64, timeout: i64, yes: String) {
    let t = (timeout > 0).then_some(timeout);
    assert_eq!(is_stalled(secs, peers, t), yes == "is");
}

#[then(
    regex = r"^with (\d+) active transfers and a cap of (\d+) the worker claims at most (\d+) more$"
)]
fn budget(_w: &mut World, active: usize, cap: usize, want: usize) {
    assert_eq!(claim_budget(active, Some(cap)), want);
}

#[then(regex = r"^with (\d+) active transfers and no cap the worker claims without bound$")]
fn budget_unbounded(_w: &mut World, active: usize) {
    assert_eq!(claim_budget(active, None), usize::MAX);
}

#[then(regex = r"^a configured cap of (\d+) bytes per second becomes (unlimited|\d+)$")]
fn bps(_w: &mut World, v: u64, want: String) {
    let got = bps_limit(v);
    if want == "unlimited" {
        assert_eq!(got, None);
    } else {
        assert_eq!(got, Some(want.parse::<u32>().unwrap()));
    }
}

#[then(regex = r#"^the file "([^"]*)" with components "([^"]*)" resolves to "([^"]*)"$"#)]
fn abs_path(_w: &mut World, folder: String, comps: String, want: String) {
    let comps: Vec<String> = comps.split('/').map(String::from).collect();
    assert_eq!(file_abs_path(&folder, &comps), want);
}

#[when(regex = r#"^the finished file "([^"]*)" is mapped from "([^"]*)" to "([^"]*)"$"#)]
fn map_complete(w: &mut World, abs: String, from: String, to: String) {
    w.path_out = Some(map_to_complete(&abs, &from, &to));
}

#[then(regex = r#"^the mapped path is "([^"]*)"$"#)]
fn mapped(w: &mut World, want: String) {
    assert_eq!(w.path_out.clone().flatten().as_deref(), Some(want.as_str()));
}

#[then("the file is left where it is because it is outside the incomplete tree")]
fn not_mapped(w: &mut World) {
    assert_eq!(w.path_out.clone().flatten(), None);
}

// --- config resolution ----------------------------------------------------------------------

#[given("the shared config table holds:")]
fn config_table(w: &mut World, step: &Step) {
    let pairs: Vec<(String, String)> = step
        .table()
        .expect("a key/value table")
        .rows
        .iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect();
    let view = skadi_config::ConfigView::from_pairs(pairs);
    match Config::from_view(&view, "sqlite://x".into(), false) {
        Ok(c) => {
            w.config = Some(c);
            w.config_error = None;
        }
        Err(e) => w.config_error = Some(e.to_string()),
    }
}

#[given("an empty shared config table")]
fn empty_config(w: &mut World) {
    let view = skadi_config::ConfigView::default();
    w.config = Some(Config::from_view(&view, "sqlite://x".into(), false).expect("defaults"));
}

#[then(regex = r#"^the worker downloads into "([^"]*)" and watches "([^"]*)"$"#)]
fn cfg_dirs(w: &mut World, dl: String, watch: String) {
    let c = w.config.as_ref().expect("config");
    assert_eq!(c.download_dir.to_string_lossy(), dl);
    assert_eq!(
        c.watch_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        (!watch.is_empty()).then_some(watch)
    );
}

#[then(
    regex = r"^the worker caps (\d+) active transfers, stalls after (\d+) seconds and leases for (\d+) seconds$"
)]
fn cfg_caps(w: &mut World, active: usize, stall: i64, lease: i64) {
    let c = w.config.as_ref().expect("config");
    assert_eq!(c.max_active, (active > 0).then_some(active));
    assert_eq!(c.stall_timeout_secs, (stall > 0).then_some(stall));
    assert_eq!(c.lease_secs, lease);
}

#[then(
    regex = r#"^the worker seed policy is ratio ([0-9.]+|unlimited), (\d+|unlimited) minutes, action "(stop|remove)"$"#
)]
fn cfg_seed(w: &mut World, ratio: String, mins: String, action: String) {
    let c = w.config.as_ref().expect("config");
    assert_eq!(c.seed_policy.ratio_limit, ratio.parse::<f64>().ok());
    assert_eq!(
        c.seed_policy.time_limit_secs,
        mins.parse::<i64>().ok().map(|m| m * 60)
    );
    assert_eq!(c.seed_policy.action, SeedAction::parse(&action));
}

#[then(
    regex = r"^the worker defaults to polling every (\d+)s, ticking every (\d+)s, ports (\d+)-(\d+), unlimited download, upload capped at (\d+) bytes per second and no metadata timeout$"
)]
fn cfg_defaults(w: &mut World, poll: u64, tick: u64, lo: u16, hi: u16, up: u32) {
    let c = w.config.as_ref().expect("config");
    assert_eq!(c.poll_interval, Duration::from_secs(poll));
    assert_eq!(c.tick_interval, Duration::from_secs(tick));
    assert_eq!(c.port_range, lo..hi);
    assert_eq!(c.down_limit_bps, None);
    // The registry ships a default upload cap (`worker.up_limit_bps` = 100000).
    assert_eq!(c.up_limit_bps, Some(up));
    assert_eq!(c.metadata_timeout_secs, None);
    assert!(c.seed_policy.is_unlimited());
}

/// SKADI-T-0489: bring a session restore that outran `max_active` back under the
/// cap. Exercises the store half — `release_active_beyond` — which is where the
/// FIFO choice and the re-queue live; the worker's `enforce_active_cap` adds only
/// the librqbit pause on top, which needs a live session.
#[when(regex = r"^the worker enforces a max_active of (\d+)$")]
async fn enforce_cap(w: &mut World, keep: usize) {
    w.store()
        .release_active_beyond(keep)
        .await
        .expect("release_active_beyond");
}

#[then(regex = r#"^"([^"]+)" is queued again$"#)]
async fn is_queued_again(w: &mut World, name: String) {
    let job = w.job(&name).await;
    assert_eq!(
        job.status,
        skadi_store::DownloadJobStatus::Queued,
        "{name} should be back in the queue"
    );
    assert!(
        job.worker_id.is_none(),
        "{name} should no longer be claimed by a worker"
    );
}
