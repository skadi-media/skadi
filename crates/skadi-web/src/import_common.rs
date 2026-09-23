//! Shared pieces of the three library-import views (movies / TV / audiobooks) —
//! SKADI-T-0328. Only *accidental* divergence is unified here: the confidence
//! badge vocabulary and the commit-result rendering. Each page keeps its own
//! review layout (flat table vs Show→Season→Episode tree) on purpose.

use leptos::prelude::*;

use crate::api;

/// CSS class for a match confidence label. One shared map so `nfo`/`manual`
/// render green on every page (movies' old local copy would have shown them
/// grey the moment it grew NFO confidence).
pub fn confidence_class(c: &str) -> &'static str {
    match c {
        "high" | "nfo" | "manual" => "ok",
        "low" => "pending",
        _ => "muted",
    }
}

/// Tooltip explaining a confidence badge — the raw tokens are jargon.
pub fn confidence_title(c: &str) -> &'static str {
    match c {
        "nfo" => "Exact id read from the folder's NFO sidecar — authoritative",
        "high" => "Title + year agree with the metadata search — trustworthy",
        "manual" => "You picked this match (search or id) — trusted as entered",
        "low" => "Fuzzy match only (year missing or disagreeing) — please verify",
        _ => "No match found — pick the right entry manually",
    }
}

/// Render a commit outcome: the tally line, a **collapsed** benign
/// "left in place" list (`unmatched`), and an expanded error list only when
/// errors are genuinely present. Shared by all three import pages so benign
/// skips never read as a wall of red.
pub fn commit_result_view(res: &api::CommitResult) -> AnyView {
    let unmatched = res.unmatched.clone();
    let errs = res.errors.clone();
    view! {
        <p class="ok">
            {format!(
                "Imported {} ({} linked, {} already in place), skipped {}.",
                res.imported, res.linked, res.in_place, res.skipped
            )}
        </p>
        {(!unmatched.is_empty()).then(|| view! {
            <details class="import-unmatched">
                <summary class="muted">
                    {format!("{} file(s) left in place (no matching target / already imported / duplicate)", unmatched.len())}
                </summary>
                <ul class="muted">
                    {unmatched.into_iter().map(|e| view! { <li>{e}</li> }).collect_view()}
                </ul>
            </details>
        })}
        {(!errs.is_empty()).then(|| view! {
            <details class="import-errors" open>
                <summary class="bad">{format!("{} error(s)", errs.len())}</summary>
                <ul class="bad">
                    {errs.into_iter().map(|e| view! { <li>{e}</li> }).collect_view()}
                </ul>
            </details>
        })}
    }
    .into_any()
}
