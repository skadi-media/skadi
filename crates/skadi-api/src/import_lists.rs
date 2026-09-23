//! Import-list sync (SKADI-T-0511, component C19).
//!
//! The engine owns everything a provider should not have to: which items are
//! already in the library, which the operator has excluded, and how a given
//! domain adds one. A provider is a fetch-and-parse
//! ([`ImportListProvider`](skadi_metadata::import_list::ImportListProvider)) and
//! nothing else, so adding Trakt or IMDb is one file.

use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;

use skadi_core::Result;
use skadi_metadata::import_list::ImportListProvider;
use skadi_store::{ImportList, ImportListRepo};

use crate::library::{ImportListAddOptions, LibraryProvider};

/// What one list's sync did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub list_id: String,
    pub list_name: String,
    /// Items the provider offered.
    pub offered: usize,
    /// Items added to the library.
    pub added: usize,
    /// Items already present.
    pub existing: usize,
    /// Items skipped because the operator excluded them.
    pub excluded: usize,
    /// Items that failed to add, with why. Kept per-item rather than aborting
    /// the run: one unavailable id should not stop the other ninety-nine.
    pub failed: Vec<(String, String)>,
}

/// Sync one list.
///
/// Errors only for a failure that makes the whole run meaningless — no provider
/// for the list's kind, no domain that can add for it, or a provider fetch that
/// failed. Per-item failures land in [`SyncReport::failed`], because a list is
/// mostly-working far more often than it is broken, and failing the run would
/// throw away the items that did work.
pub async fn sync_list(
    list: &ImportList,
    providers: &[Arc<dyn ImportListProvider>],
    domains: &[Arc<dyn LibraryProvider>],
    store: &dyn ImportListRepo,
) -> Result<SyncReport> {
    let mut report = SyncReport {
        list_id: list.id.clone(),
        list_name: list.name.clone(),
        ..SyncReport::default()
    };

    let provider = providers
        .iter()
        .find(|p| p.kind() == list.kind)
        .ok_or_else(|| {
            skadi_core::AppError::Validation(format!(
                "no import-list provider for kind `{}`",
                list.kind
            ))
        })?;

    // Resolved before the fetch: a list pointed at a disabled or unknown domain
    // is misconfigured, and finding that out after a round trip to TMDB wastes
    // the call and buries the real error under a network one.
    let domain = domains
        .iter()
        .find(|d| {
            // Accept either spelling: `target_domain` is written by hand in a
            // settings blob, and "movie" (the MediaKind subfolder, as
            // `item_tags` uses) and "movies" (the registered domain name) are
            // both the obvious thing to type. Rejecting one of them would be a
            // configuration trap with a silent-looking symptom.
            let want = list.target_domain.to_ascii_lowercase();
            d.kind().library_subfolder().eq_ignore_ascii_case(&want)
                || d.domain().eq_ignore_ascii_case(&want)
        })
        .ok_or_else(|| {
            skadi_core::AppError::Validation(format!(
                "import list `{}` targets domain `{}`, which is not registered",
                list.name, list.target_domain
            ))
        })?;

    let items = provider.fetch(&list.settings).await?;
    report.offered = items.len();

    let opts = ImportListAddOptions {
        monitored: list.add_monitored,
        profile_id: list.profile_id.clone(),
        root_folder: list.root_folder.clone(),
    };

    for item in items {
        // Exclusions are checked before the add, not after: the add is the
        // expensive, side-effecting half, and an excluded item must not reach it.
        match store
            .is_excluded(&list.target_domain, &item.id_kind, &item.external_id)
            .await
        {
            Ok(true) => {
                report.excluded += 1;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                // A failed exclusion check is not a licence to add. Treat it as
                // "might be excluded" and skip — re-adding something the operator
                // deleted is the failure this whole table exists to prevent.
                report
                    .failed
                    .push((item.external_id.clone(), format!("exclusion check: {e}")));
                continue;
            }
        }

        match domain
            .add_from_list(&item.id_kind, &item.external_id, &opts)
            .await
        {
            Ok(true) => report.added += 1,
            Ok(false) => report.existing += 1,
            Err(e) => report
                .failed
                .push((item.external_id.clone(), e.to_string())),
        }
    }

    Ok(report)
}

/// Sync every list that is due, recording each outcome on its row.
///
/// One list's failure does not stop the others: lists are independent, and a
/// broken Trakt token should not stop a TMDB collection syncing for days.
pub async fn sync_due_lists(
    providers: &[Arc<dyn ImportListProvider>],
    domains: &[Arc<dyn LibraryProvider>],
    store: &(dyn ImportListRepo + Send + Sync),
) -> Result<Vec<SyncReport>> {
    let now = Utc::now();
    let lists = store.list_import_lists().await?;
    let mut out = Vec::new();
    for list in lists.into_iter().filter(|l| l.is_due(now)) {
        match sync_list(&list, providers, domains, store).await {
            Ok(report) => {
                // A run with per-item failures still counts as a run: the
                // interval must advance, or a list with one bad id would re-sync
                // on every tick forever.
                let err = (!report.failed.is_empty())
                    .then(|| format!("{} item(s) failed", report.failed.len()));
                let _ = store
                    .record_sync(&list.id, Utc::now(), err.as_deref())
                    .await;
                tracing::info!(
                    list = %list.name,
                    offered = report.offered,
                    added = report.added,
                    existing = report.existing,
                    excluded = report.excluded,
                    failed = report.failed.len(),
                    "import list synced"
                );
                out.push(report);
            }
            Err(e) => {
                tracing::warn!(list = %list.name, error = %e, "import list sync failed");
                let _ = store
                    .record_sync(&list.id, Utc::now(), Some(&e.to_string()))
                    .await;
            }
        }
    }
    Ok(out)
}

/// How often the worker checks whether any list is due.
///
/// The tick is not the sync interval — each list carries its own
/// `interval_minutes`, and [`ImportList::is_due`] decides. This only bounds how
/// late a due list can be, so it is short enough not to matter and long enough
/// not to be a busy loop.
const TICK: std::time::Duration = std::time::Duration::from_secs(300);

/// Background worker: sync due lists on a tick until cancelled.
pub async fn sync_worker(
    state: Arc<crate::state::AppState>,
    cancel: tokio_util::sync::CancellationToken,
) {
    if state.import_list_providers.is_empty() {
        // Nothing can sync, so do not run a loop that wakes every five minutes to
        // discover that again.
        tracing::info!("import list worker: no providers registered; not started");
        return;
    }
    let mut ticker = tokio::time::interval(TICK);
    tracing::info!("import list sync worker started");
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::debug!("import list worker cancelled");
                break;
            }
            _ = ticker.tick() => {
                let Some(store) = state.store.as_ref() else {
                    continue;
                };
                if let Err(e) =
                    sync_due_lists(&state.import_list_providers, &state.library, store).await
                {
                    // Only a store-level failure reaches here; per-list failures
                    // are recorded on their rows and do not stop the sweep.
                    tracing::warn!(error = %e, "import list sweep failed");
                }
            }
        }
    }
}
