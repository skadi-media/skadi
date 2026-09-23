//! In-flight acquire-run tracker (SKADI-T-0055).
//!
//! A process-global, in-memory registry of acquire runs currently executing, so
//! the API's `/activity` endpoint can show what the daemon is doing right now.
//! Deliberately a global (like the services registry) rather than a field on
//! [`HunterServices`](crate::services::HunterServices): it avoids threading a
//! handle through every construction site, and a single daemon process has
//! exactly one in-flight set.
//!
//! In-memory only: a daemon restart clears it, and Cloacina's own recovery
//! resumes the durable workflows. Those replayed runs have no
//! `start_acquire` owner, so the pipeline steps **adopt** them back into the
//! tracker as they execute ([`InFlightTracker::adopt`], SKADI-T-0388) — otherwise
//! the sweep saw no run in flight and launched a duplicate acquire over a
//! transfer Cloacina was still driving. Adopted entries are kept alive by
//! [`touch`](InFlightTracker::touch) and swept by
//! [`expire_adopted`](InFlightTracker::expire_adopted) once a replayed workflow
//! dies without reaching a step that could finish it. The `/activity` view is
//! therefore best-effort observability, not a source of truth (that's the
//! per-edition status in the store).
//!
//! ## Wiring granularity
//!
//! A run is recorded at the [`start_acquire`](crate::worker::start_acquire)
//! boundary and removed on completion. As it advances, the trigger surface and
//! the pipeline steps emit per-stage updates (`searching` → `deciding` →
//! `grabbing` → `snatching` → `downloading` → `importing`) plus acquisition
//! transparency — the chosen release, how many candidates were weighed, and the
//! classified decision (SKADI-T-0190) — via [`InFlightTracker::set_stage`],
//! [`set_chosen`](InFlightTracker::set_chosen) and
//! [`set_decision`](InFlightTracker::set_decision), so `/activity` shows what the
//! daemon is doing *and why* in real time.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use chrono::{DateTime, Utc};
use serde::Serialize;

use skadi_core::MediaKind;

/// A snapshot of one in-flight acquire run.
#[derive(Clone, Debug, Serialize)]
pub struct RunMeta {
    /// A correlation id for this run (not Cloacina's internal id).
    pub run_id: String,
    /// The media kind being acquired.
    pub kind: MediaKind,
    /// The opaque acquirable reference (the key the run is tracked under).
    pub acquirable_ref: String,
    /// When the run entered the tracker.
    pub started_at: DateTime<Utc>,
    /// The stage the run is in: `searching` → `deciding` → `grabbing` →
    /// `snatching` → `downloading` → `importing` → `notifying` (SKADI-T-0190).
    pub current_stage: String,
    /// The release `decide` chose, once chosen (acquisition transparency).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_title: Option<String>,
    /// How many candidate releases `decide` weighed before choosing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates_considered: Option<usize>,
    /// The profile decision for the chosen release (`Accept`/`Upgrade`/…), once
    /// classified at the snatch boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// `true` when a pipeline step registered this run after finding it
    /// untracked — a workflow Cloacina recovery replayed after a restart
    /// (SKADI-T-0388). No `start_acquire` future owns it, so the steps finish
    /// it and the sweep expires it if it goes quiet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adopted: bool,
    /// Last time the run reported life (stage change, progress heartbeat).
    pub last_seen: DateTime<Utc>,
    /// Transfer watch (SKADI-T-0388): the best progress `monitor` has observed
    /// and when it last improved. Lives here, not in the Cloacina context,
    /// because Cloacina rebuilds a task's context from its dependencies on
    /// every retry — nothing a failed `monitor` attempt stores survives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<TransferWatch>,
    /// The last `Downloading{progress}` flushed to the status sink, so the
    /// per-poll heartbeat can be throttled across `monitor` executions.
    #[serde(skip)]
    last_flushed: Option<(f32, DateTime<Utc>)>,
}

/// What `monitor` has seen of a transfer so far (SKADI-T-0388).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TransferWatch {
    /// When the transfer came under watch (first progress observation).
    pub since: DateTime<Utc>,
    /// Highest progress observed (never lowered by a client-side re-check).
    pub best_progress: f32,
    /// When `best_progress` last moved up.
    pub progress_at: DateTime<Utc>,
    /// When the client first reported the transfer **live** (SKADI-T-0394), i.e.
    /// actually connected and downloading rather than queued behind the client's
    /// hash/init work or fetching magnet metadata. `None` while it has only ever
    /// been seen queued. The stall clock runs from this, not from `since`: a
    /// transfer that has not started cannot be stalled, and 46 releases were
    /// permanently blocklisted on 2026-09-06 for exactly that mistake.
    pub live_since: Option<DateTime<Utc>>,
}

/// Who the tracked run for an acquirable belongs to, as seen from a workflow
/// step entering a stage — see [`InFlightTracker::adopt`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// The entry carries this workflow's run id (or predates run ids).
    Mine,
    /// Nothing was tracked; this workflow was registered as an adopted run.
    Adopted,
    /// Another workflow for the same acquirable is already in flight.
    Foreign,
}

/// One progress observation's outcome — see [`InFlightTracker::observe_progress`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation {
    /// The watch after folding this observation in.
    pub watch: TransferWatch,
    /// Whether this progress should be flushed to the status sink now (first
    /// report, ≥1% movement, or the heartbeat interval elapsed).
    pub flush: bool,
}

impl RunMeta {
    fn new(run_id: String, kind: MediaKind, acquirable_ref: String) -> Self {
        let now = Utc::now();
        RunMeta {
            run_id,
            kind,
            acquirable_ref,
            started_at: now,
            current_stage: "running".into(),
            chosen_title: None,
            candidates_considered: None,
            decision: None,
            adopted: false,
            last_seen: now,
            transfer: None,
            last_flushed: None,
        }
    }
}

/// A concurrent set of in-flight runs, keyed by acquirable reference.
#[derive(Default)]
pub struct InFlightTracker {
    runs: RwLock<HashMap<String, RunMeta>>,
    /// Releases currently being fetched, `release_key` → owning acquirable ref
    /// (SKADI-T-0402). Separate from `runs` because a claim is taken at the
    /// snatch boundary, not when `decide` records a chosen title.
    claims: RwLock<HashMap<String, String>>,
}

impl InFlightTracker {
    /// Record a run as started, in stage `"running"`.
    pub fn start(
        &self,
        run_id: impl Into<String>,
        kind: MediaKind,
        acquirable_ref: impl Into<String>,
    ) {
        let acquirable_ref = acquirable_ref.into();
        let meta = RunMeta::new(run_id.into(), kind, acquirable_ref.clone());
        if let Ok(mut runs) = self.runs.write() {
            runs.insert(acquirable_ref, meta);
        }
    }

    /// Atomically record a run **only if** none is already tracked for this
    /// `acquirable_ref` (SKADI-T-0039 dedup). Returns `true` if recorded — the
    /// caller owns the entry and must [`finish`](Self::finish) it; `false` if a
    /// run is already in flight, in which case the caller must **not** start one.
    pub fn try_start(
        &self,
        run_id: impl Into<String>,
        kind: MediaKind,
        acquirable_ref: impl Into<String>,
    ) -> bool {
        let acquirable_ref = acquirable_ref.into();
        let Ok(mut runs) = self.runs.write() else {
            return false;
        };
        if runs.contains_key(&acquirable_ref) {
            return false;
        }
        runs.insert(
            acquirable_ref.clone(),
            RunMeta::new(run_id.into(), kind, acquirable_ref),
        );
        true
    }

    /// The `run_id` of the run currently tracked for `acquirable_ref`, if any.
    /// Lets off-pipeline emitters (the trace stream, SKADI-T-0323) correlate an
    /// event to its run without threading the id through every call site.
    #[must_use]
    pub fn run_id_for(&self, acquirable_ref: &str) -> Option<String> {
        self.runs
            .read()
            .ok()
            .and_then(|runs| runs.get(acquirable_ref).map(|m| m.run_id.clone()))
    }

    /// Whether a run is currently tracked for `acquirable_ref`.
    #[must_use]
    pub fn is_active(&self, acquirable_ref: &str) -> bool {
        self.runs
            .read()
            .map(|runs| runs.contains_key(acquirable_ref))
            .unwrap_or(false)
    }

    /// Update the current stage of an in-flight run, if present.
    pub fn set_stage(&self, acquirable_ref: &str, stage: impl Into<String>) {
        if let Ok(mut runs) = self.runs.write()
            && let Some(meta) = runs.get_mut(acquirable_ref)
        {
            meta.current_stage = stage.into();
            meta.last_seen = Utc::now();
        }
    }

    /// [`set_stage`](Self::set_stage), but if no run is tracked for
    /// `acquirable_ref` — a workflow Cloacina recovery replayed after a restart,
    /// which no `start_acquire` call registered — register it now as an
    /// *adopted* run (SKADI-T-0388). This is what lets the sweep's dedup see a
    /// replayed transfer and not start a second acquire over it.
    ///
    /// `run_id` is the id the workflow's own state carries; it decides who the
    /// existing entry belongs to. Two workflows for the same acquirable can both
    /// be replayed after a restart (both were queued behind a blocked scheduler
    /// when it died) — the second one to arrive sees a *foreign* entry and must
    /// step aside instead of grabbing the same release again.
    pub fn adopt(
        &self,
        kind: MediaKind,
        acquirable_ref: &str,
        stage: impl Into<String>,
        run_id: Option<&str>,
    ) -> Ownership {
        let Ok(mut runs) = self.runs.write() else {
            return Ownership::Foreign;
        };
        let stage = stage.into();
        if let Some(meta) = runs.get_mut(acquirable_ref) {
            // Legacy states (persisted before run ids existed) can't prove
            // ownership; treat them as the owner so a drain never self-cancels.
            if run_id.is_none_or(|id| id == meta.run_id) {
                meta.current_stage = stage;
                meta.last_seen = Utc::now();
                return Ownership::Mine;
            }
            return Ownership::Foreign;
        }
        let mut meta = RunMeta::new(
            run_id.map_or_else(
                || format!("recovered-{}", uuid::Uuid::new_v4()),
                str::to_string,
            ),
            kind,
            acquirable_ref.to_string(),
        );
        meta.current_stage = stage;
        meta.adopted = true;
        runs.insert(acquirable_ref.to_string(), meta);
        Ownership::Adopted
    }

    /// Record that the run for `acquirable_ref` is still alive (a progress
    /// heartbeat), so [`expire_adopted`](Self::expire_adopted) leaves it be.
    pub fn touch(&self, acquirable_ref: &str) {
        if let Ok(mut runs) = self.runs.write()
            && let Some(meta) = runs.get_mut(acquirable_ref)
        {
            meta.last_seen = Utc::now();
        }
    }

    /// Fold one `monitor` poll into the run's transfer watch (SKADI-T-0388):
    /// counts as life, raises `best_progress` (never lowers it) and stamps
    /// `progress_at` when it moves. `heartbeat` is the longest the status sink
    /// may go without a flush even when progress is flat; the returned
    /// [`Observation::flush`] says whether to write this poll through. `None`
    /// when the run isn't tracked (nothing to watch against).
    pub fn observe_progress(
        &self,
        acquirable_ref: &str,
        progress: f32,
        heartbeat: chrono::Duration,
    ) -> Option<Observation> {
        self.observe_progress_live(acquirable_ref, progress, heartbeat, true)
    }

    /// [`observe_progress`](Self::observe_progress) with the client's liveness:
    /// `live == false` means the download client has the transfer queued (hash
    /// check, metadata fetch) rather than running, so the stall clock must not
    /// start yet (SKADI-T-0394).
    pub fn observe_progress_live(
        &self,
        acquirable_ref: &str,
        progress: f32,
        heartbeat: chrono::Duration,
        live: bool,
    ) -> Option<Observation> {
        let mut runs = self.runs.write().ok()?;
        let meta = runs.get_mut(acquirable_ref)?;
        let now = Utc::now();
        meta.last_seen = now;
        let watch = match meta.transfer {
            Some(mut w) => {
                if progress > w.best_progress {
                    w.best_progress = progress;
                    w.progress_at = now;
                }
                if live && w.live_since.is_none() {
                    // First time the client reports it running: this is when the
                    // stall clock starts, and progress made before now doesn't
                    // count against it.
                    w.live_since = Some(now);
                    w.progress_at = now;
                }
                w
            }
            None => TransferWatch {
                since: now,
                best_progress: progress,
                progress_at: now,
                live_since: live.then_some(now),
            },
        };
        meta.transfer = Some(watch);
        let flush = !meta
            .last_flushed
            .is_some_and(|(p, at)| (progress - p).abs() < 0.01 && now - at < heartbeat);
        if flush {
            meta.last_flushed = Some((progress, now));
        }
        Some(Observation { watch, flush })
    }

    /// The transfer watch for `acquirable_ref`, if it's tracked and `monitor`
    /// has polled it at least once.
    pub fn transfer_watch(&self, acquirable_ref: &str) -> Option<TransferWatch> {
        self.runs
            .read()
            .ok()?
            .get(acquirable_ref)
            .and_then(|m| m.transfer)
    }

    /// Drop adopted runs that have shown no life for longer than `max_idle` —
    /// a replayed workflow that failed hard after its last step touched the
    /// tracker, which nothing else will ever finish. Owned (non-adopted) runs
    /// are never expired: their `start_acquire` future finishes them. Returns
    /// the acquirable refs dropped.
    pub fn expire_adopted(&self, max_idle: chrono::Duration) -> Vec<String> {
        let Ok(mut runs) = self.runs.write() else {
            return Vec::new();
        };
        let cutoff = Utc::now() - max_idle;
        let dead: Vec<String> = runs
            .values()
            .filter(|m| m.adopted && m.last_seen < cutoff)
            .map(|m| m.acquirable_ref.clone())
            .collect();
        for r in &dead {
            runs.remove(r);
        }
        drop(runs);
        // A replayed run that went quiet also releases its claim, so the release
        // is grabbable again (SKADI-T-0402).
        for r in &dead {
            self.release_claims(r);
        }
        dead
    }

    /// Remove a finished run **only if it was adopted** — the terminal steps of
    /// a workflow call this, and must not yank an owned entry out from under
    /// the `start_acquire` future that is still awaiting the run.
    ///
    /// With a `run_id`, only that run's entry is removed — a superseded replay
    /// ending at `snatch` must not evict the sibling that is actually
    /// downloading.
    pub fn finish_adopted(&self, acquirable_ref: &str, run_id: Option<&str>) {
        if let Ok(mut runs) = self.runs.write()
            && runs
                .get(acquirable_ref)
                .is_some_and(|m| m.adopted && run_id.is_none_or(|id| id == m.run_id))
        {
            runs.remove(acquirable_ref);
            drop(runs);
            self.release_claims(acquirable_ref);
        }
    }

    /// Record what `decide` chose for an in-flight run: the chosen release title
    /// and how many candidates were weighed (SKADI-T-0190 transparency). No-op if
    /// the run isn't tracked.
    pub fn set_chosen(
        &self,
        acquirable_ref: &str,
        chosen_title: Option<String>,
        candidates_considered: usize,
    ) {
        if let Ok(mut runs) = self.runs.write()
            && let Some(meta) = runs.get_mut(acquirable_ref)
        {
            meta.chosen_title = chosen_title;
            meta.candidates_considered = Some(candidates_considered);
        }
    }

    /// Claim a release for this acquirable, **atomically** (SKADI-T-0402).
    ///
    /// Returns `true` if no other run is already *fetching* the same release —
    /// the caller may snatch it — and records the claim. Returns `false` if
    /// another acquirable holds it, in which case the caller must NOT add a
    /// second transfer.
    ///
    /// The tracker's other dedupe is per acquirable, which is right for "don't
    /// run two acquires for one episode" but says nothing about two acquires
    /// *choosing the same release*: that is how House of the Dragon S03 was sent
    /// to the client twice within a minute on 2026-09-06 — separate runs, one
    /// pack. Claims live in their own map, written only at the snatch boundary:
    /// keying off `chosen_title` would be wrong, because `decide` records that
    /// for every run before any of them snatches, so sibling runs would all see
    /// each other and all stand down. Check and insert happen under one write
    /// lock, so two runs racing cannot both win and cannot both back off.
    ///
    /// Re-claiming a release this acquirable already holds succeeds (the step is
    /// retried by Cloacina); the claim is released by
    /// [`finish`](Self::finish)/[`expire_adopted`](Self::expire_adopted).
    pub fn claim_release(&self, acquirable_ref: &str, release_title: &str) -> bool {
        let key = release_key(release_title);
        let Ok(mut claims) = self.claims.write() else {
            // Lock poisoned: fail open rather than silently stop acquiring.
            return true;
        };
        match claims.get(&key) {
            Some(holder) => holder == acquirable_ref,
            None => {
                claims.insert(key, acquirable_ref.to_string());
                true
            }
        }
    }

    /// Drop every release claim held by `acquirable_ref` (SKADI-T-0402), so the
    /// release can be grabbed again once this run ends — successfully or not.
    fn release_claims(&self, acquirable_ref: &str) {
        if let Ok(mut claims) = self.claims.write() {
            claims.retain(|_, holder| holder != acquirable_ref);
        }
    }

    /// Record the classified profile decision for the chosen release (set at the
    /// snatch boundary, where the run's scoring is assembled). No-op if untracked.
    pub fn set_decision(&self, acquirable_ref: &str, decision: Option<String>) {
        if let Ok(mut runs) = self.runs.write()
            && let Some(meta) = runs.get_mut(acquirable_ref)
        {
            meta.decision = decision;
        }
    }

    /// Remove a finished run.
    pub fn finish(&self, acquirable_ref: &str) {
        if let Ok(mut runs) = self.runs.write() {
            runs.remove(acquirable_ref);
        }
        self.release_claims(acquirable_ref);
    }

    /// A snapshot of all in-flight runs (empty when idle).
    pub fn snapshot(&self) -> Vec<RunMeta> {
        self.runs
            .read()
            .map(|runs| runs.values().cloned().collect())
            .unwrap_or_default()
    }
}

static TRACKER: OnceLock<InFlightTracker> = OnceLock::new();

/// The process-global in-flight tracker.
pub fn tracker() -> &'static InFlightTracker {
    TRACKER.get_or_init(InFlightTracker::default)
}

/// Normalised identity of a release for the in-flight duplicate check
/// (SKADI-T-0402): lowercase, alphanumerics only, so the same pack advertised
/// with different punctuation or spacing by two indexers is still one release.
fn release_key(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_snapshot_finish() {
        let t = InFlightTracker::default();
        assert!(t.snapshot().is_empty());
        t.start("run-1", MediaKind::Movie, "ref-1");
        let snap = t.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].acquirable_ref, "ref-1");
        assert_eq!(snap[0].current_stage, "running");
        t.set_stage("ref-1", "search");
        assert_eq!(t.snapshot()[0].current_stage, "search");
        t.finish("ref-1");
        assert!(t.snapshot().is_empty());
    }

    #[test]
    fn records_chosen_candidates_and_decision() {
        let t = InFlightTracker::default();
        t.start("run-1", MediaKind::Movie, "ref-1");
        // Before decide, transparency fields are absent (None → omitted on wire).
        let snap = t.snapshot();
        assert!(snap[0].chosen_title.is_none());
        assert!(snap[0].candidates_considered.is_none());
        assert!(snap[0].decision.is_none());

        // decide chose a release out of 7 candidates.
        t.set_chosen("ref-1", Some("The Movie 1080p".into()), 7);
        // snatch classified it.
        t.set_decision("ref-1", Some("Upgrade".into()));
        t.set_stage("ref-1", "downloading");

        let m = &t.snapshot()[0];
        assert_eq!(m.chosen_title.as_deref(), Some("The Movie 1080p"));
        assert_eq!(m.candidates_considered, Some(7));
        assert_eq!(m.decision.as_deref(), Some("Upgrade"));
        assert_eq!(m.current_stage, "downloading");

        // Setters on an unknown ref are silent no-ops.
        t.set_chosen("nope", Some("x".into()), 1);
        t.set_decision("nope", Some("Accept".into()));
        assert_eq!(t.snapshot().len(), 1);
    }

    #[test]
    fn try_start_dedupes_until_finished() {
        let t = InFlightTracker::default();
        assert!(t.try_start("a", MediaKind::Movie, "ref-1"), "first wins");
        assert!(!t.is_active("ref-2"));
        assert!(t.is_active("ref-1"));
        // A second try for the same ref is refused while the first is active.
        assert!(!t.try_start("b", MediaKind::Movie, "ref-1"), "dup refused");
        assert_eq!(t.snapshot().len(), 1, "no duplicate entry");
        // After finishing, the ref is free again.
        t.finish("ref-1");
        assert!(
            t.try_start("c", MediaKind::Movie, "ref-1"),
            "free after finish"
        );
    }

    #[test]
    fn adopt_registers_untracked_runs_and_blocks_duplicates() {
        let t = InFlightTracker::default();
        // A replayed workflow's step finds nothing tracked: it adopts the run,
        // keeping the run id its state carries.
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-1", "downloading", Some("run-a")),
            Ownership::Adopted
        );
        let m = &t.snapshot()[0];
        assert!(m.adopted);
        assert_eq!(m.run_id, "run-a");
        assert_eq!(m.current_stage, "downloading");
        // The sweep now sees it in flight and won't start a second acquire.
        assert!(!t.try_start("dup", MediaKind::Series, "ref-1"));
        // A later step of the same run just updates the stage.
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-1", "importing", Some("run-a")),
            Ownership::Mine
        );
        assert_eq!(t.snapshot().len(), 1);
        assert_eq!(t.snapshot()[0].current_stage, "importing");
        // A *second* replayed workflow for the same acquirable is foreign: it
        // must not touch the entry (stage stays), and the owner stays tracked.
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-1", "snatching", Some("run-b")),
            Ownership::Foreign
        );
        assert_eq!(t.snapshot()[0].current_stage, "importing");
        // A legacy state without a run id can't be told apart: treated as mine.
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-1", "importing", None),
            Ownership::Mine
        );
        // An owned run whose own step arrives is left alone by adopt (no flag
        // flip); a stranger's step is foreign.
        assert!(t.try_start("own", MediaKind::Series, "ref-2"));
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-2", "snatching", Some("own")),
            Ownership::Mine
        );
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-2", "snatching", Some("other")),
            Ownership::Foreign
        );
        assert!(
            !t.snapshot()
                .iter()
                .any(|m| m.acquirable_ref == "ref-2" && m.adopted)
        );
        // Untracked + no run id still adopts under a generated id.
        assert_eq!(
            t.adopt(MediaKind::Series, "ref-3", "downloading", None),
            Ownership::Adopted
        );
        assert!(
            t.snapshot()
                .iter()
                .any(|m| m.acquirable_ref == "ref-3" && m.run_id.starts_with("recovered-"))
        );
    }

    #[test]
    fn finish_adopted_and_expire_leave_owned_runs_alone() {
        let t = InFlightTracker::default();
        assert!(t.try_start("own", MediaKind::Movie, "owned"));
        t.adopt(
            MediaKind::Movie,
            "adopted-live",
            "downloading",
            Some("live"),
        );
        t.adopt(MediaKind::Movie, "adopted-dead", "downloading", None);
        t.adopt(MediaKind::Movie, "adopted-done", "notifying", Some("done"));

        // finish_adopted: no-op on an owned entry, no-op for a stranger's run
        // id, removes the adopted one it names (or any adopted one when the
        // caller has no run id).
        t.finish_adopted("owned", None);
        t.finish_adopted("adopted-live", Some("superseded-sibling"));
        t.finish_adopted("adopted-done", Some("done"));
        assert!(t.is_active("owned"));
        assert!(t.is_active("adopted-live"));
        assert!(!t.is_active("adopted-done"));

        // Age everything, then heartbeat only the live one.
        if let Ok(mut runs) = t.runs.write() {
            for m in runs.values_mut() {
                m.last_seen = Utc::now() - chrono::Duration::hours(5);
            }
        }
        t.touch("adopted-live");
        let mut dropped = t.expire_adopted(chrono::Duration::hours(2));
        dropped.sort();
        assert_eq!(dropped, vec!["adopted-dead".to_string()]);
        assert!(t.is_active("owned"), "owned runs never expire");
        assert!(t.is_active("adopted-live"), "touched runs survive");
    }

    #[test]
    fn observe_progress_tracks_best_progress_and_throttles_flushes() {
        let t = InFlightTracker::default();
        let hb = chrono::Duration::minutes(5);
        assert!(t.observe_progress("untracked", 0.5, hb).is_none());
        assert!(t.transfer_watch("untracked").is_none());

        t.adopt(MediaKind::Series, "r", "downloading", None);
        let first = t.observe_progress("r", 0.10, hb).unwrap();
        assert!(first.flush, "first poll always flushes");
        assert_eq!(first.watch.best_progress, 0.10);
        let since = first.watch.since;

        // Flat progress inside the heartbeat: watch unchanged, no flush.
        let flat = t.observe_progress("r", 0.10, hb).unwrap();
        assert!(!flat.flush);
        assert_eq!(flat.watch.progress_at, first.watch.progress_at);

        // A client-side re-check that reports LESS never lowers the best.
        let lower = t.observe_progress("r", 0.05, hb).unwrap();
        assert_eq!(lower.watch.best_progress, 0.10);

        // Real movement flushes and re-stamps progress_at (since is stable).
        let moved = t.observe_progress("r", 0.25, hb).unwrap();
        assert!(moved.flush);
        assert_eq!(moved.watch.best_progress, 0.25);
        assert!(moved.watch.progress_at >= first.watch.progress_at);
        assert_eq!(moved.watch.since, since);
        assert_eq!(t.transfer_watch("r"), Some(moved.watch));

        // Flat but past the heartbeat: flushes again.
        let stale = t
            .observe_progress("r", 0.25, chrono::Duration::zero())
            .unwrap();
        assert!(stale.flush);
    }
}
