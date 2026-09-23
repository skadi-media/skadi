//! Hunter trace stream emitter (SKADI-T-0323).
//!
//! Thin best-effort helper the pipeline calls at its step boundaries to persist
//! a structured per-step event (candidates found, the decision and why, snatch →
//! downloader, download failure, import outcome) via the store's
//! [`TraceRepo`](skadi_store::TraceRepo). The richer companion to the coarse
//! `history` outcomes — built for *diagnosing the hunter* against ephemeral
//! torrent sources.
//!
//! Recording is off the acquire hot path: a write failure is logged and
//! swallowed, never failing the workflow. The `run_id` is resolved from the
//! in-flight [`tracker`](crate::tracker) so callers needn't thread it through.

use chrono::Utc;
use skadi_core::MediaKind;
use skadi_store::{TraceEvent, TraceRepo};

use crate::services::try_services_for;

/// The denormalised `kind` string stored on a trace or decision row.
///
/// One mapping for both (SKADI-T-0433): `record_decision_history` had its own,
/// which sent everything that was not an audiobook to `"movie"` — so every TV
/// decision was filed under the movies domain and the TV rows an operator went
/// looking for did not exist.
pub(crate) fn kind_str(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Series => "tv",
        MediaKind::Audiobook => "audiobook",
        MediaKind::Music => "music",
        MediaKind::Book => "book",
        MediaKind::Subtitle => "subtitle",
    }
}

/// Append one trace event for `acquirable_ref` in `kind`'s domain. Best-effort:
/// resolves the domain's store via the service registry and the `run_id` via the
/// tracker, writes the row, and logs (never propagates) any failure. A no-op if
/// the domain's services aren't registered (e.g. in a unit test).
pub async fn emit(
    kind: MediaKind,
    acquirable_ref: &str,
    stage: &str,
    event: &str,
    message: impl Into<String>,
    detail: Option<String>,
) {
    let Some(svc) = try_services_for(kind) else {
        return;
    };
    let entry = TraceEvent {
        id: uuid::Uuid::new_v4().to_string(),
        at: Utc::now(),
        run_id: crate::tracker::tracker().run_id_for(acquirable_ref),
        kind: kind_str(kind).to_string(),
        acquirable_ref: acquirable_ref.to_string(),
        stage: stage.to_string(),
        event: event.to_string(),
        message: message.into(),
        detail,
    };
    if let Err(e) = svc.store.record_trace(&entry).await {
        tracing::warn!(error = %e, event, "recording trace event failed (non-fatal)");
    }
}

/// What a search actually asked for, as the `detail` payload of a
/// `candidates_found` event (SKADI-T-0380).
///
/// "Found 181 candidates" is only half an answer — the other half is *for what*.
/// A wrong-title grab (searching "101 Dalmatians", grabbing "101 Dalmatians II")
/// is invisible until the query and the ids behind it are on the record.
#[must_use]
pub fn search_detail(spec: &crate::state::SearchSpec) -> Option<String> {
    let ids = &spec.external_ids;
    let mut id_map = serde_json::Map::new();
    if let Some(i) = &ids.imdb {
        id_map.insert("imdb".into(), i.0.clone().into());
    }
    if let Some(i) = &ids.tmdb {
        id_map.insert("tmdb".into(), i.0.to_string().into());
    }
    if let Some(i) = &ids.tvdb {
        id_map.insert("tvdb".into(), i.0.to_string().into());
    }
    let payload = serde_json::json!({
        "searched": spec.titles,
        "year": spec.year,
        "ids": id_map,
        "season": spec.tv.map(|t| t.season),
        "episode": spec.tv.and_then(|t| t.episode),
    });
    serde_json::to_string(&payload).ok()
}

/// The `decide` tally as a `decision` / `no_release` `detail` payload
/// (SKADI-T-0380) — candidate count, per-gate rejections, chosen relevance.
#[must_use]
pub fn tally_detail(tally: &crate::pipeline::DecisionTally) -> Option<String> {
    serde_json::to_string(tally).ok()
}
