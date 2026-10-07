//! Activity + History view (SKADI-T-0082): live in-flight acquire runs (polled
//! from `/activity`, which names each item and carries its transfer progress,
//! SKADI-T-0690) over the hunter trace stream, whose rows are labelled with
//! titles cross-referenced from the library.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

/// Poll cadence for the live activity + history (ms).
const POLL_MS: u32 = 2500;

/// Longest inline detail before we truncate and tuck the full text behind a
/// disclosure. Keeps the history table to one readable line per row.
const MAX_INLINE: usize = 80;

/// Trace-log color class for a trace `event` word (SKADI-T-0323): imported→ok,
/// decision/snatched→ice, no_release→violet, *_failed→bad, else (candidates_found
/// …)→muted.
fn trace_color(event: &str) -> &'static str {
    match event {
        "imported" => "ev-ok",
        "decision" | "snatched" => "ev-ice",
        "no_release" => "ev-violet",
        "download_failed" | "import_failed" => "ev-bad",
        _ => "ev-muted",
    }
}

/// Trim an RFC-3339 timestamp to `YYYY-MM-DD HH:MM` for compact display.
fn short_time(rfc3339: &str) -> String {
    let date = rfc3339.get(0..10).unwrap_or("");
    let time = rfc3339.get(11..16).unwrap_or("");
    format!("{date} {time}")
}

/// Strip the surrounding quotes (and unescape) from a Rust `Debug` string, so
/// `"magnet:?xt=…"` becomes `magnet:?xt=…`. Non-quoted input passes through.
fn unquote_debug(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        s.to_string()
    }
}

/// Map a `FailureReason` variant name to a human-readable label. Unknown inputs
/// (e.g. a quality name like `Bluray-1080p`) pass through unchanged.
fn humanize_variant(variant: &str) -> String {
    match variant {
        "DownloadFailed" => "Download failed".into(),
        "ImportFailed" => "Import failed".into(),
        "NoSuitableRelease" => "No suitable release".into(),
        "Other" => "Error".into(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// Title-relevance coverage below which a chosen release is flagged as a weak
/// match. Mirrors `WEAK_MATCH` in `skadi-hunter`'s worker — kept in sync by the
/// shared trace payload, not by import (the crates don't depend on each other).
const WEAK_MATCH: f32 = 0.8;

/// One line of a hunter trace's structured `detail` payload (SKADI-T-0380),
/// ready to render: a label and its value.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailFact {
    pub label: String,
    pub value: String,
    /// Render with warning emphasis — currently only a weak title match, the
    /// signature of a wrong-title grab.
    pub warn: bool,
}

/// Parse a hunter trace `detail` into displayable facts (SKADI-T-0380).
///
/// `search_detail` / `tally_detail` write JSON; older events (and history
/// failures) carry a Rust-`Debug` payload that [`detail_summary`] handles. Only
/// a leading `{` is treated as structured, so the two never collide and old rows
/// keep rendering as they did.
///
/// Returns an empty vec for anything unparseable — a trace panel is a
/// diagnostic, and failing to draw one must never be louder than the thing being
/// diagnosed.
#[must_use]
pub fn detail_facts(detail: &str) -> Vec<DetailFact> {
    let detail = detail.trim();
    if !detail.starts_with('{') {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |label: &str, value: String, warn: bool| {
        if !value.is_empty() {
            out.push(DetailFact {
                label: label.to_string(),
                value,
                warn,
            });
        }
    };

    // --- search payload ---
    if let Some(titles) = v.get("searched").and_then(|t| t.as_array()) {
        let joined = titles
            .iter()
            .filter_map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        push("searched", joined, false);
    }
    if let Some(y) = v.get("year").and_then(serde_json::Value::as_u64) {
        push("year", y.to_string(), false);
    }
    if let Some(ids) = v.get("ids").and_then(|i| i.as_object()) {
        let joined = ids
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| format!("{k}:{s}")))
            .collect::<Vec<_>>()
            .join(" ");
        push("ids", joined, false);
    }
    if let Some(s) = v.get("season").and_then(serde_json::Value::as_u64) {
        let ep = v.get("episode").and_then(serde_json::Value::as_u64);
        let scope = match ep {
            Some(e) => format!("S{s:02}E{e:02}"),
            None => format!("S{s:02} (season pack)"),
        };
        push("scope", scope, false);
    }

    // --- decision tally payload ---
    if let Some(n) = v.get("considered").and_then(serde_json::Value::as_u64) {
        push("considered", n.to_string(), false);
    }
    if let Some(rej) = v.get("rejected").and_then(|r| r.as_object())
        && !rej.is_empty()
    {
        // Busiest gate first — that is the one worth acting on.
        let mut pairs: Vec<(&String, u64)> = rej
            .iter()
            .filter_map(|(k, val)| val.as_u64().map(|n| (k, n)))
            .collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let total: u64 = pairs.iter().map(|(_, n)| n).sum();
        let joined = pairs
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ");
        push("rejected", format!("{total} — {joined}"), false);
    }
    if let Some(r) = v
        .get("chosen_relevance")
        .and_then(serde_json::Value::as_f64)
    {
        let weak = (r as f32) < WEAK_MATCH;
        let note = if weak { " — weak match" } else { "" };
        push("title match", format!("{r:.2}{note}"), weak);
    }
    out
}

/// What an operator can do about a stall dominated by one rejection gate
/// (SKADI-T-0381).
///
/// Keyed by the gate labels `skadi-hunter`'s `RejectReason::label()` writes into
/// the trace payload. The copy lives here rather than in the hunter because it
/// is UI guidance, not pipeline logic — the hunter should not own sentences
/// about which knob to turn. An unrecognised label yields `None`, so a future
/// gate degrades to "no suggestion" instead of wrong advice.
#[must_use]
pub fn remedy_for(gate: &str) -> Option<&'static str> {
    Some(match gate {
        "quality" => {
            "Candidates were found but none is allowed by the profile. Widen the \
             allowed qualities or lower the cutoff."
        }
        "relevance" => {
            "Results share some words with the title but aren't this item. The \
             search terms may be too generic — try a manual search."
        }
        "identity" => {
            "Titles matched loosely but the author/title gate rejected them. Try \
             a manual search to confirm the right edition exists."
        }
        "language" => {
            "Results were dubs or machine readings in another language. An English \
             release may not exist yet — try a manual search."
        }
        "seeders" => {
            "Candidates exist but are under-seeded. Lower the minimum-seeders \
             setting or wait for a healthier release."
        }
        "blocklisted" => {
            "Every candidate has already failed and been blocklisted. Clear the \
             blocklist for this item to retry them."
        }
        "size" => {
            "Candidates are far off the expected size for this media kind — \
             usually a mis-tagged or fake release."
        }
        "category" => {
            "Indexers are returning wrong-media results — check the indexer's \
             category mapping."
        }
        "episode" => {
            "Results are for this show but not this episode (or aren't episodes \
             at all). The episode may not be released yet — wait, or try a \
             manual search for a season pack."
        }
        "year" => {
            "Results share the title but name a different year — a remake, \
             sequel, or original. Check the item's year, or try a manual search."
        }
        _ => return None,
    })
}

/// A per-item acquisition diagnosis built from that item's trace rows
/// (SKADI-T-0381) — the "why isn't this grabbed?" answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagnosis {
    /// When the most recent search ran (RFC-3339, as stored).
    pub last_attempt: Option<String>,
    /// Candidates the most recent decision weighed.
    pub considered: usize,
    /// Per-gate rejection counts from the most recent decision, busiest first.
    pub rejected: Vec<(String, usize)>,
    /// Title of the release chosen on the most recent decision, if any.
    pub chosen: Option<String>,
    /// Relevance of that chosen release.
    pub chosen_relevance: Option<f32>,
    /// How many of the traced attempts ended with nothing grabbed.
    pub failed_attempts: usize,
    /// Guidance for the busiest gate, when there is one.
    pub remedy: Option<&'static str>,
}

impl Diagnosis {
    /// Total rejected on the most recent decision.
    #[must_use]
    pub fn rejected_total(&self) -> usize {
        self.rejected.iter().map(|(_, n)| n).sum()
    }

    /// The busiest gate and its share of rejections (`0.0..=1.0`).
    #[must_use]
    pub fn top_gate(&self) -> Option<(&str, usize, f32)> {
        let total = self.rejected_total();
        self.rejected.first().map(|(k, n)| {
            let share = if total == 0 {
                0.0
            } else {
                *n as f32 / total as f32
            };
            (k.as_str(), *n, share)
        })
    }

    /// True when there is nothing useful to show — the panel stays quiet rather
    /// than rendering an empty shell.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.last_attempt.is_none() && self.rejected.is_empty() && self.chosen.is_none()
    }
}

/// Diagnose one item from its trace rows, **newest first** as the API returns
/// them (SKADI-T-0381).
///
/// Reads the most recent `decision` / `no_release` event for the breakdown, and
/// counts how many of the traced attempts came up empty. Older events are only
/// counted, not merged: mixing tallies from different profile settings would
/// describe a system state that never existed.
#[must_use]
pub fn diagnose(traces: &[api::TraceRow]) -> Diagnosis {
    let mut d = Diagnosis {
        failed_attempts: traces.iter().filter(|t| t.event == "no_release").count(),
        last_attempt: traces
            .iter()
            .find(|t| {
                matches!(
                    t.event.as_str(),
                    "candidates_found" | "decision" | "no_release"
                )
            })
            .map(|t| t.at.clone()),
        ..Default::default()
    };

    let Some(latest) = traces
        .iter()
        .find(|t| matches!(t.event.as_str(), "decision" | "no_release"))
    else {
        return d;
    };
    // The chosen title lives in the message ("chose \"…\" of N candidates");
    // the structured counts live in `detail`.
    if latest.event == "decision" {
        d.chosen = latest
            .message
            .split_once('"')
            .and_then(|(_, rest)| rest.rsplit_once('"').map(|(t, _)| t.to_string()));
    }
    let Some(detail) = latest.detail.as_deref() else {
        return d;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) else {
        return d;
    };
    d.considered = v
        .get("considered")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    d.chosen_relevance = v
        .get("chosen_relevance")
        .and_then(serde_json::Value::as_f64)
        .map(|f| f as f32);
    if let Some(rej) = v.get("rejected").and_then(|r| r.as_object()) {
        let mut pairs: Vec<(String, usize)> = rej
            .iter()
            .filter_map(|(k, val)| val.as_u64().map(|n| (k.clone(), n as usize)))
            .collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        d.rejected = pairs;
    }
    d.remedy = d.top_gate().and_then(|(gate, _, _)| remedy_for(gate));
    d
}

/// Split a raw history `detail` into a short human summary and an optional
/// expandable payload (the long technical bit — a magnet URI, infohash, or
/// error string). `detail` for failures is the `Debug` of `FailureReason`,
/// e.g. `DownloadFailed("magnet:?xt=…")`; for imports it's a quality name.
///
/// Returns `(summary, expandable)` where `expandable` is `Some` only when there
/// is meaningful extra text worth hiding behind a disclosure.
pub fn detail_summary(detail: &str) -> (String, Option<String>) {
    let detail = detail.trim();
    if detail.is_empty() {
        return (String::new(), None);
    }
    let Some(open) = detail.find('(') else {
        // Bare variant (`NoSuitableRelease`) or a plain label (quality name).
        return (humanize_variant(detail), None);
    };
    let variant = detail[..open].trim();
    let inner = unquote_debug(detail[open + 1..].trim_end_matches(')').trim());
    let inner = inner.trim().to_string();
    let non_empty = |s: String| (!s.is_empty()).then_some(s);
    match variant {
        // The inner message is the signal — keep it inline if short, expand if long.
        "Other" if !inner.is_empty() => {
            if inner.chars().count() > MAX_INLINE {
                (truncate(&inner, MAX_INLINE), Some(inner))
            } else {
                (inner, None)
            }
        }
        // Known failure: friendly label + the raw payload tucked away.
        _ => (humanize_variant(variant), non_empty(inner)),
    }
}

/// The live-work stage board (SKADI-T-0321): each backend `current_stage` maps
/// to one operator-facing group, in pipeline order. The hunter advances a run
/// `running → searching → deciding → grabbing → snatching → downloading →
/// importing → notifying`; we fold those into the five things an operator cares
/// about — *what is each item doing right now*.
pub const STAGE_GROUPS: &[(&str, &[&str])] = &[
    ("Searching", &["running", "searching"]),
    ("Deciding", &["deciding"]),
    ("Grabbing", &["grabbing", "snatching"]),
    ("Downloading", &["downloading"]),
    ("Importing", &["importing", "notifying"]),
];

/// Index of the [`STAGE_GROUPS`] group a raw `current_stage` belongs to. Unknown
/// stages fall into the first group (Searching) so a run is never dropped.
pub fn stage_group(stage: &str) -> usize {
    STAGE_GROUPS
        .iter()
        .position(|(_, stages)| stages.contains(&stage))
        .unwrap_or(0)
}

/// Run-length counts over adjacent equal items, in order. `[A, A, B, A]` →
/// `[2, 1, 1]`. Used to collapse consecutive identical history rows into a
/// single grouped row carrying a repeat count.
pub fn run_lengths<T: PartialEq>(items: &[T]) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let mut n = 1;
        while i + n < items.len() && items[i + n] == items[i] {
            n += 1;
        }
        runs.push(n);
        i += n;
    }
    runs
}

/// Human age of an RFC-3339 timestamp relative to `now_ms` (epoch millis).
/// Falls back to the raw date when the timestamp cannot be parsed or is in
/// the future. Pure so the boundaries are testable.
pub fn ago(rfc3339: &str, now_ms: f64) -> String {
    let t = js_sys::Date::parse(rfc3339);
    if t.is_nan() {
        return short_time(rfc3339);
    }
    let secs = ((now_ms - t) / 1000.0).floor();
    if secs < 0.0 {
        return short_time(rfc3339);
    }
    let secs = secs as u64;
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => short_time(rfc3339),
    }
}

/// `"… — found 576 candidates"` → `Some(576)`.
pub fn parse_found(message: &str) -> Option<usize> {
    let rest = message.split("found ").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

/// One hunter run, folded from its trace rows (SKADI-T-0582).
///
/// The raw stream is two to four rows per run — `candidates_found`, then a
/// `decision` or `no_release`, then maybe `snatched`, `download_failed`,
/// `imported` — each restating the item and the count. One run is one line.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSummary {
    /// The run id, or the row id for a stray row without one.
    pub key: String,
    /// Time of the run's latest event.
    pub at: String,
    pub acquirable_ref: String,
    /// Stage and event of the latest row: the outcome so far.
    pub stage: String,
    pub event: String,
    pub message: String,
    /// From the `candidates_found` row, when the run has one.
    pub candidates: Option<usize>,
    /// Structured detail of the outcome row (rejection tally, decision).
    pub detail: Option<String>,
    /// Structured detail of the search row (what was searched for).
    pub search_detail: Option<String>,
}

/// Fold newest-first trace rows into newest-first run summaries.
pub fn fold_runs(traces: &[api::TraceRow]) -> Vec<RunSummary> {
    let mut out: Vec<RunSummary> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for t in traces {
        let key = t.run_id.clone().unwrap_or_else(|| t.id.clone());
        match index.get(&key) {
            // Later in the list = earlier in time: only backfill the search facts.
            Some(&i) => {
                let r = &mut out[i];
                if t.event == "candidates_found" {
                    if r.candidates.is_none() {
                        r.candidates = parse_found(&t.message);
                    }
                    if r.search_detail.is_none() {
                        r.search_detail = t.detail.clone();
                    }
                }
            }
            None => {
                index.insert(key.clone(), out.len());
                let is_search = t.event == "candidates_found";
                out.push(RunSummary {
                    key,
                    at: t.at.clone(),
                    acquirable_ref: t.acquirable_ref.clone(),
                    stage: t.stage.clone(),
                    event: t.event.clone(),
                    message: t.message.clone(),
                    candidates: is_search.then(|| parse_found(&t.message)).flatten(),
                    detail: (!is_search).then(|| t.detail.clone()).flatten(),
                    search_detail: is_search.then(|| t.detail.clone()).flatten(),
                });
            }
        }
    }
    out
}

/// The part of an item label that names the *show*: `"Um, Actually S07E12"`
/// → `"Um, Actually"`, `"Foo Season 3"` → `"Foo"`. Anything else is itself.
pub fn series_key(label: &str) -> (String, Option<String>) {
    if let Some((head, tail)) = label.rsplit_once(' ') {
        let is_ep = tail.len() >= 6
            && tail.starts_with('S')
            && tail[1..].chars().take(2).all(|c| c.is_ascii_digit())
            && tail.contains('E')
            && tail
                .rsplit('E')
                .next()
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if is_ep {
            return (head.to_string(), Some(tail.to_string()));
        }
        if let Some((h2, word)) = head.rsplit_once(' ')
            && word == "Season"
            && tail.chars().all(|c| c.is_ascii_digit())
        {
            return (h2.to_string(), Some(format!("Season {tail}")));
        }
    }
    (label.to_string(), None)
}

/// Consecutive runs for the same show with the same outcome, folded together.
#[derive(Clone, Debug, PartialEq)]
pub struct RunGroup {
    pub head: RunSummary,
    /// Display label of the group: the show, or the item itself.
    pub label: String,
    /// Episode codes (or item labels) in the group, newest first.
    pub items: Vec<String>,
}

/// Collapse `runs` (newest first, already filtered) where adjacent entries
/// name the same show and ended the same way. A sweep walking a season
/// episode by episode with the same "no suitable release" is one fact, not
/// six lines (SKADI-T-0582).
pub fn collapse_runs(runs: Vec<RunSummary>, name: impl Fn(&str) -> String) -> Vec<RunGroup> {
    let mut out: Vec<RunGroup> = Vec::new();
    for r in runs {
        let label = name(&r.acquirable_ref);
        let (show, part) = series_key(&label);
        let item = part.unwrap_or_else(|| label.clone());
        if let Some(last) = out.last_mut()
            && last.label == show
            && last.head.event == r.event
        {
            last.items.push(item);
            continue;
        }
        out.push(RunGroup {
            head: r,
            label: show,
            items: vec![item],
        });
    }
    out
}

/// Whether the log filter keeps an event.
pub fn filter_allows(filter: &str, event: &str) -> bool {
    match filter {
        "decisions" => matches!(event, "decision" | "no_release"),
        "failures" => event.ends_with("_failed"),
        "imports" => matches!(event, "imported" | "snatched"),
        _ => true,
    }
}

/// Operator wording for a trace event.
pub fn event_label(event: &str) -> &str {
    match event {
        "candidates_found" => "searched",
        "decision" => "chose",
        "no_release" => "no release",
        "snatched" => "grabbed",
        "download_failed" => "download failed",
        "import_failed" => "import failed",
        "imported" => "imported",
        other => other,
    }
}

/// `1.2 MB/s` style rate for a downloading row.
fn fmt_rate(bps: i64) -> String {
    let b = bps.max(0) as f64;
    if b >= 1_048_576.0 {
        format!("{:.1} MB/s", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB/s", b / 1024.0)
    } else {
        format!("{b:.0} B/s")
    }
}

/// The progress line of a downloading run (SKADI-T-0690): percent done and
/// `"40% · 1.2 MB/s · 5m"`. From the transfer fields the server merges into
/// the run; `None` when the run has no download row.
#[must_use]
pub fn run_progress(r: &api::ActivityRun) -> Option<(f64, String)> {
    r.download_id.as_ref()?;
    let pct = match (r.size_bytes, r.downloaded_bytes) {
        (Some(size), Some(done)) if size > 0 => (done as f64 / size as f64 * 100.0).round(),
        _ => 0.0,
    }
    .clamp(0.0, 100.0);
    let eta = r
        .eta_seconds
        .filter(|e| *e > 0)
        .map(|e| {
            if e >= 3600 {
                format!(" · {}h {}m", e / 3600, (e % 3600) / 60)
            } else {
                format!(" · {}m", e / 60)
            }
        })
        .unwrap_or_default();
    Some((
        pct,
        format!(
            "{pct:.0}% · {}{eta}",
            fmt_rate(r.down_speed_bps.unwrap_or(0))
        ),
    ))
}

#[component]
pub fn ActivityPage() -> impl IntoView {
    let runs = RwSignal::new(Vec::<api::ActivityRun>::new());
    let traces = RwSignal::new(Vec::<api::TraceRow>::new());
    let log_filter = RwSignal::new("all".to_string());
    let expanded = RwSignal::new(None::<String>);
    // acquirable ref -> item title, for the trace log's rows (live runs carry
    // their own `title` from the server, SKADI-T-0690).
    let titles = RwSignal::new(HashMap::<String, String>::new());
    let error = RwSignal::new(None::<String>);
    let alive = RwSignal::new(true);

    Effect::new(move |_| {
        // Resolve titles once (the library rarely changes mid-watch). Both domains:
        // a live run is keyed by its acquirable ref — a movie edition id or an
        // audiobook book-file id — so map both to their item title.
        spawn_local(async move {
            let mut map = HashMap::<String, String>::new();
            if let Ok(ms) = api::list_movies().await {
                for m in ms {
                    let title = m.title.clone();
                    for e in m.editions {
                        map.insert(e.id, title.clone());
                    }
                }
            }
            if let Ok(ss) = api::list_series().await {
                for s in ss {
                    for e in &s.episodes {
                        map.insert(
                            e.id.clone(),
                            format!("{} S{:02}E{:02}", s.title, e.season, e.number),
                        );
                    }
                    // Season-pack runs are keyed `season-<series id>-<n>`.
                    for se in &s.seasons {
                        map.insert(
                            format!("season-{}-{}", s.id, se.number),
                            format!("{} Season {}", s.title, se.number),
                        );
                    }
                }
            }
            if let Ok(bs) = api::list_books(None, None).await {
                for b in bs {
                    let title = b.title.clone();
                    for f in b.files {
                        map.insert(f.id, title.clone());
                    }
                }
            }
            titles.set(map);
        });
        // Poll activity + history until the page is torn down.
        spawn_local(async move {
            loop {
                match api::activity().await {
                    Ok(a) => {
                        runs.set(a);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
                if let Ok(t) = api::traces(160).await {
                    traces.set(t);
                }
                gloo_timers::future::TimeoutFuture::new(POLL_MS).await;
                if !alive.try_get_untracked().unwrap_or(false) {
                    break;
                }
            }
        });
    });
    on_cleanup(move || alive.set(false));

    // The live-work board (SKADI-T-0321): runs grouped by what they're *doing*
    // — Searching · Deciding · Grabbing · Downloading · Importing — each group
    // always shown with its count, an idle placeholder when empty, and per-item
    // transparency (candidates weighed, chosen release, profile decision) that
    // the backend already reports but the old flat list discarded.
    // A compact stage bar (Searching 8 · Deciding 0 · …) always shown, so empty
    // lanes collapse to a chip instead of a full-height empty panel and the
    // trace log gets the vertical space (SKADI-T-0348).
    let stage_bar = move || {
        let rs = runs.get();
        STAGE_GROUPS
            .iter()
            .enumerate()
            .map(|(gi, (group_name, _))| {
                let count = rs
                    .iter()
                    .filter(|r| stage_group(&r.current_stage) == gi)
                    .count();
                let cls = if count > 0 {
                    "stage-pill active"
                } else {
                    "stage-pill"
                };
                view! {
                    <span class=cls>
                        <span class="u-label">{*group_name}</span>
                        <span class="mono">{count}</span>
                    </span>
                }
            })
            .collect_view()
    };

    let stage_board = move || {
        let names = titles.get();
        let rs = runs.get();
        STAGE_GROUPS
            .iter()
            .enumerate()
            // Only NON-empty lanes get a detail section now — empties live in the
            // stage bar above.
            .filter_map(|(gi, (group_name, _))| {
                let items: Vec<_> = rs
                    .iter()
                    .filter(|r| stage_group(&r.current_stage) == gi)
                    .cloned()
                    .collect();
                if items.is_empty() {
                    return None;
                }
                let count = items.len();
                let body = {
                    items
                        .into_iter()
                        .map(|r| {
                            // Unresolved acquirable refs are raw UUIDs — show a
                            // human placeholder with the id demoted, not a bare
                            // GUID masquerading as a title (SKADI-T-0348).
                            // The server names the item (SKADI-T-0690); the
                            // library map is only a fallback.
                            let resolved = r
                                .title
                                .clone()
                                .or_else(|| names.get(&r.acquirable_ref).cloned());
                            let has_name = resolved.is_some();
                            // Past `decide` there is a release title; when the
                            // item itself did not resolve, that title is the
                            // best name there is, so it leads (SKADI-T-0580).
                            let chosen_title = (gi >= 2).then(|| r.chosen_title.clone()).flatten();
                            let label_view = match resolved.or_else(|| chosen_title.clone()) {
                                Some(t) => view! { <strong>{t}</strong> }.into_any(),
                                None => {
                                    let short = r
                                        .acquirable_ref
                                        .get(0..8)
                                        .unwrap_or(&r.acquirable_ref)
                                        .to_string();
                                    view! {
                                        <strong class="muted">
                                            "Unknown item "
                                            <span class="faint mono">{short}</span>
                                        </strong>
                                    }
                                    .into_any()
                                }
                            };
                            let started = r
                                .started_at
                                .as_deref()
                                .map(|t| ago(t, js_sys::Date::now()))
                                .unwrap_or_default();
                            // Downloading: size, progress, rate and time left
                            // as the server merged them from the download row.
                            let progress = (gi == 3).then(|| run_progress(&r)).flatten().map(|(pct, text)| {
                                view! {
                                    <span class="run-progress">
                                        <span class="run-bar"><span class="run-bar-fill" style=format!("width:{pct:.0}%")></span></span>
                                        <span class="mono tnum">{text}</span>
                                    </span>
                                }
                            });
                            // What is this item doing? Deciding shows how many
                            // candidates are being weighed; once a release is
                            // chosen (grab onward) show its title + decision.
                            let candidates = (gi == 1)
                                .then_some(r.candidates_considered)
                                .flatten()
                                .map(|n| {
                                    let plural = if n == 1 { "" } else { "s" };
                                    view! {
                                        <span class="stage-doing muted">
                                            {format!("weighing {n} candidate{plural}")}
                                        </span>
                                    }
                                });
                            let chosen = has_name
                                .then(|| chosen_title.clone())
                                .flatten()
                                .map(|t| view! { <span class="stage-doing mono">{t}</span> });
                            let decision =
                                (gi >= 2).then(|| r.decision.clone()).flatten().map(|d| {
                                    let cls = if d.eq_ignore_ascii_case("upgrade") {
                                        "badge upgrade"
                                    } else {
                                        "badge ok"
                                    };
                                    view! { <span class=cls>{d}</span> }
                                });
                            view! {
                                <div class="activity-row">
                                    <span class="spinner"></span>
                                    {label_view}
                                    {candidates}
                                    {chosen}
                                    {decision}
                                    {progress}
                                    <span class="muted stage-when">{started}</span>
                                </div>
                            }
                        })
                        .collect_view()
                        .into_any()
                };
                Some(view! {
                    <section class="stage-group">
                        <div class="stage-head">
                            <span class="stage-name u-label">{*group_name}</span>
                            <span class="stage-count mono">{count}</span>
                        </div>
                        <div class="stage-items">{body}</div>
                    </section>
                })
            })
            .collect_view()
            .into_any()
    };

    // The hunter trace stream, one line per RUN (SKADI-T-0582). The raw stream
    // is two to four rows per run that restate the item and the count, newest
    // first — so a decision read above the search it came from. Rows fold into
    // a run; consecutive runs for the same show with the same outcome fold
    // again, because a sweep walking a season is one fact. A filter row keeps
    // the failures findable, and a click opens the full text and facts.
    let trace_lines = move || {
        let names = titles.get();
        let ts = traces.get();
        let filter = log_filter.get();
        let open = expanded.get();
        if ts.is_empty() {
            return view! {
                <p class="muted">
                    "No traces yet — they appear as the hunter searches, decides, grabs and imports."
                </p>
            }
            .into_any();
        }
        let now = js_sys::Date::now();
        let runs: Vec<RunSummary> = fold_runs(&ts)
            .into_iter()
            .filter(|r| filter_allows(&filter, &r.event))
            .collect();
        if runs.is_empty() {
            return view! { <p class="muted">"Nothing matches this filter in the last 160 events."</p> }.into_any();
        }
        let groups = collapse_runs(runs, |r| {
            names.get(r).cloned().unwrap_or_else(|| r.to_string())
        });
        let lines = groups
            .into_iter()
            .take(80)
            .map(|g| {
                let color = trace_color(&g.head.event);
                let is_bad = g.head.event.ends_with("_failed");
                let key = g.head.key.clone();
                let is_open = open.as_deref() == Some(key.as_str());
                let n = g.items.len();
                // "Show — 6 episodes (S07E12, S07E08, …) — <outcome>" for a
                // fold; "Show S07E12 — <outcome>" for one run.
                let subject = if n > 1 {
                    let shown: Vec<&str> = g.items.iter().take(5).map(String::as_str).collect();
                    let more = if n > 5 { ", …" } else { "" };
                    format!("{} — {n} runs ({}{more})", g.label, shown.join(", "))
                } else if g.items[0] == g.label {
                    g.label.clone()
                } else {
                    format!("{} {}", g.label, g.items[0])
                };
                let text = format!("{subject} — {}", g.head.message);
                let cands = g.head.candidates.map(|c| format!("{c} candidates"));
                let facts = |d: Option<&str>| {
                    d.map(detail_facts)
                        .unwrap_or_default()
                        .into_iter()
                        // The count already sits at the end of the line.
                        .filter(|f| f.label != "considered")
                        .map(|f| {
                            let cls = if f.warn { "log-fact mono warn" } else { "log-fact mono" };
                            view! {
                                <span class=cls>
                                    <span class="log-fact-label">{f.label}</span>
                                    {f.value}
                                </span>
                            }
                        })
                        .collect_view()
                };
                let outcome_facts = facts(g.head.detail.as_deref());
                let search_facts = is_open.then(|| facts(g.head.search_detail.as_deref()));
                let toggle_key = key.clone();
                let on_click = move |_| {
                    expanded.update(|e| {
                        *e = if e.as_deref() == Some(toggle_key.as_str()) { None } else { Some(toggle_key.clone()) }
                    });
                };
                view! {
                    <div class="run-row" class:bad=is_bad class:open=is_open on:click=on_click>
                        <div class="run-line">
                            <span class="run-time mono" title=g.head.at.clone()>{ago(&g.head.at, now)}</span>
                            <span class=format!("run-ev mono {color}")>{event_label(&g.head.event).to_string()}</span>
                            <span class="run-text">{text}</span>
                            {cands.map(|c| view! { <span class="run-cands mono">{c}</span> })}
                        </div>
                        <div class="log-facts">{outcome_facts}{search_facts}</div>
                    </div>
                }
            })
            .collect_view();
        view! { <div class="event-log-lines">{lines}</div> }.into_any()
    };

    let filter_chip = move |key: &'static str, label: &'static str| {
        view! {
            <button
                class="filter-chip"
                class:active=move || log_filter.get() == key
                on:click=move |_| log_filter.set(key.to_string())
            >
                {label}
            </button>
        }
    };

    view! {
        <crate::subnav::SubNav/>
        <div class="page-head">
            <div>
                <h2 class="page-title">"Activity"</h2>
                <p class="page-sub mono">
                    {move || format!("{} in flight", runs.get().len())}
                </p>
            </div>
        </div>

        {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
        <div class="stage-bar">{stage_bar}</div>
        <div class="stage-board">{stage_board}</div>

        <div class="event-log">
            <div class="event-log-head">
                <span class="event-log-label u-label">"Trace log · skadi-hunter"</span>
                <div class="filter-chips log-filters">
                    {filter_chip("all", "All")}
                    {filter_chip("decisions", "Decisions")}
                    {filter_chip("failures", "Failures")}
                    {filter_chip("imports", "Grabs & imports")}
                </div>
            </div>
            {trace_lines}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(v: serde_json::Value) -> api::ActivityRun {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn run_progress_formats_the_merged_transfer() {
        let r = run(serde_json::json!({
            "run_id": "r", "acquirable_ref": "x", "current_stage": "downloading",
            "download_id": "d", "size_bytes": 200, "downloaded_bytes": 50,
            "eta_seconds": 7260, "down_speed_bps": 2048
        }));
        assert_eq!(
            run_progress(&r),
            Some((25.0, "25% · 2 KB/s · 2h 1m".into()))
        );
    }

    #[test]
    fn run_progress_without_a_size_is_zero_and_without_a_row_is_none() {
        let r = run(serde_json::json!({
            "run_id": "r", "acquirable_ref": "x", "current_stage": "downloading",
            "download_id": "d", "downloaded_bytes": 0, "eta_seconds": 0
        }));
        assert_eq!(run_progress(&r), Some((0.0, "0% · 0 B/s".into())));
        let none = run(serde_json::json!({
            "run_id": "r", "acquirable_ref": "x", "current_stage": "downloading"
        }));
        assert_eq!(run_progress(&none), None);
    }
}
