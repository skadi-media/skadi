//! The hunter's Cloacina workflows.
//!
//! Two workflows share one set of runner-free step bodies ([`crate::steps`]):
//!
//! - [`acquire`] — the original six-task DAG (search → decide → snatch → monitor
//!   → import → notify). Kept registered as the **drain/back-compat** path for
//!   runs persisted by the pre-SKADI-T-0042 grain (see ADR SKADI-A-0002); the
//!   `failure_injection` integration test also drives it directly.
//! - [`acquire_release`] — the new **per-release** grain (snatch → monitor →
//!   import → notify). Seeded with a state whose `chosen` is already set;
//!   search/decide run in-process in [`crate::worker::start_acquire`] before the
//!   run is launched. This is what the sweep and manual grab launch now.
//!
//! Each `#[task]` wrapper is **thin**: it calls the matching `steps::*` function
//! and maps any [`skadi_core::AppError`] to a Cloacina `TaskError` tagged with
//! the task id. All logic (status writes, blocklist, live progress) lives in
//! [`crate::steps`]; the pipeline functions it calls are unit-tested without a
//! runner.
//!
//! Downstream tasks gate on `trigger_rules = task_success("<dep>")` so a failed
//! upstream task does **not** run its successors (the default `Always` rule
//! would, with an empty context — the multi-hour-hang bug fixed in SKADI-T-0041).

use cloacina::workflow;

/// Re-exported so callers can reference the monitor budget without reaching into
/// [`crate::steps`].
///
/// The `monitor*` tasks' retry attributes below are the literal forms of
/// [`MONITOR_RETRY_ATTEMPTS`] / [`MONITOR_RETRY_DELAY_SECS`] (the `#[task]`
/// macro only takes literals): a **fixed, jitter-free 30 s** retry is what paces
/// the one-poll-per-execution transfer watch (SKADI-T-0388) — Cloacina 0.6
/// dispatches inline in its scheduler loop, so `monitor` must never sleep.
pub use crate::steps::{
    MONITOR_MAX_POLLS, MONITOR_POLL_INTERVAL_SECS, MONITOR_RETRY_ATTEMPTS, MONITOR_RETRY_DELAY_SECS,
};

/// Map any `AppError` to a cloacina `TaskError`, tagged with the task id.
fn te(task_id: &str, e: skadi_core::AppError) -> cloacina::TaskError {
    cloacina::TaskError::ExecutionFailed {
        message: format!("{e}"),
        task_id: task_id.to_string(),
        timestamp: chrono::Utc::now(),
    }
}

#[workflow(
    name = "acquire",
    description = "Drive one acquirable through search → decide → snatch → monitor → import → notify."
)]
pub mod acquire {
    use cloacina::{Context, TaskError, task};

    use super::te;
    use crate::steps;

    #[task(id = "search")]
    pub async fn search_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::search(context).await.map_err(|e| te("search", e))
    }

    #[task(id = "decide", dependencies = ["search"], trigger_rules = task_success("search"))]
    pub async fn decide_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::decide(context).await.map_err(|e| te("decide", e))
    }

    #[task(id = "snatch", dependencies = ["decide"], trigger_rules = task_success("decide"))]
    pub async fn snatch_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::snatch(context).await.map_err(|e| te("snatch", e))
    }

    #[task(id = "monitor", dependencies = ["snatch"], retry_attempts = 2400, retry_delay_ms = 30000, retry_backoff = "fixed", retry_jitter = false, trigger_rules = task_success("snatch"))]
    pub async fn monitor_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::monitor(context).await.map_err(|e| te("monitor", e))
    }

    #[task(id = "import", dependencies = ["monitor"], trigger_rules = task_success("monitor"))]
    pub async fn import_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::import(context).await.map_err(|e| te("import", e))
    }

    #[task(id = "notify", dependencies = ["import"], trigger_rules = task_success("import"))]
    pub async fn notify_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::notify(context).await.map_err(|e| te("notify", e))
    }
}

#[workflow(
    name = "acquire_release",
    description = "Drive one chosen release through snatch → monitor → import → notify (SKADI-T-0042 grain)."
)]
pub mod acquire_release {
    use cloacina::{Context, TaskError, task};

    use super::te;
    use crate::steps;

    // Cloacina task ids are **global** across the runner's registry, so these
    // can't reuse the `acquire` workflow's ids — they carry a `_release` suffix.
    #[task(id = "snatch_release")]
    pub async fn snatch_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::snatch(context)
            .await
            .map_err(|e| te("snatch_release", e))
    }

    #[task(id = "monitor_release", dependencies = ["snatch_release"], retry_attempts = 2400, retry_delay_ms = 30000, retry_backoff = "fixed", retry_jitter = false, trigger_rules = task_success("snatch_release"))]
    pub async fn monitor_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::monitor(context)
            .await
            .map_err(|e| te("monitor_release", e))
    }

    #[task(id = "import_release", dependencies = ["monitor_release"], trigger_rules = task_success("monitor_release"))]
    pub async fn import_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::import(context)
            .await
            .map_err(|e| te("import_release", e))
    }

    #[task(id = "notify_release", dependencies = ["import_release"], trigger_rules = task_success("import_release"))]
    pub async fn notify_task(context: &mut Context<serde_json::Value>) -> Result<(), TaskError> {
        steps::notify(context)
            .await
            .map_err(|e| te("notify_release", e))
    }
}
