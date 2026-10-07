//! Multi-select on the library walls and the bulk bar (SKADI-T-0696).
//!
//! The Movies, TV and Audiobooks walls share one select mode: a "Select" button
//! in the toolbar's `bulk` slot turns it on, a tile click then toggles that
//! tile instead of opening it, and the bar acts on the selection with ONE
//! `POST /{movies|series|books}/bulk` request (monitor, unmonitor, search,
//! delete with or without files). Delete asks once, through the ConfirmDialog,
//! and names the count.
//!
//! The selection is keyed on the record `id`. An action always takes the
//! selected ids that are on the wall now, in wall order
//! ([`ordered_selection`]), so a filter change cannot act on items the operator
//! can no longer see, and the count on the bar is the count acted on.
//!
//! Admin only: the pages mount the bar for a known admin, and the server
//! refuses the endpoints to every other role.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, BulkAction, BulkReport, BulkRequest};
use crate::confirm::{ConfirmSpec, confirm};

/// The selection state of one wall. `Copy`, so tiles and the bar share it.
#[derive(Clone, Copy)]
pub struct Selection {
    /// Select mode is on: a tile click toggles instead of opening.
    pub mode: RwSignal<bool>,
    pub ids: RwSignal<HashSet<String>>,
}

impl Default for Selection {
    fn default() -> Self {
        Self::new()
    }
}

impl Selection {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: RwSignal::new(false),
            ids: RwSignal::new(HashSet::new()),
        }
    }

    /// Whether `id` is selected (reactive).
    #[must_use]
    pub fn has(&self, id: &str) -> bool {
        self.ids.with(|s| s.contains(id))
    }

    pub fn toggle(&self, id: &str) {
        self.ids.update(|s| toggle_id(s, id));
    }

    /// Leave select mode and drop the selection.
    pub fn exit(&self) {
        self.mode.set(false);
        self.ids.set(HashSet::new());
    }
}

/// Add `id` when absent, remove it when present.
pub fn toggle_id(set: &mut HashSet<String>, id: &str) {
    if !set.remove(id) {
        set.insert(id.to_string());
    }
}

/// The selected ids that are on the wall, in wall order (`visible` is the
/// sorted, filtered list the wall shows).
#[must_use]
pub fn ordered_selection(visible: &[String], selected: &HashSet<String>) -> Vec<String> {
    visible
        .iter()
        .filter(|id| selected.contains(*id))
        .cloned()
        .collect()
}

/// "Select all" target: every id on the wall, or none when all are selected
/// already (the same button then clears).
#[must_use]
pub fn select_all_or_none(visible: &[String], selected: &HashSet<String>) -> HashSet<String> {
    if !visible.is_empty() && visible.iter().all(|id| selected.contains(id)) {
        HashSet::new()
    } else {
        visible.iter().cloned().collect()
    }
}

/// What a wall calls its items: "1 movie", "12 movies".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Noun {
    pub one: &'static str,
    pub many: &'static str,
}

pub const MOVIES: Noun = Noun {
    one: "movie",
    many: "movies",
};
pub const SERIES: Noun = Noun {
    one: "series",
    many: "series",
};
pub const BOOKS: Noun = Noun {
    one: "audiobook",
    many: "audiobooks",
};

impl Noun {
    #[must_use]
    pub fn count(self, n: usize) -> String {
        format!("{n} {}", if n == 1 { self.one } else { self.many })
    }
}

/// The one question a bulk delete asks: it names the count, and says whether
/// the files go too.
#[must_use]
pub fn delete_confirm(noun: Noun, n: usize, files: bool) -> ConfirmSpec {
    let body = if files {
        "Their files are deleted from disk too. This can't be undone."
    } else {
        "They leave the library. Their files stay on disk."
    };
    ConfirmSpec::destructive(format!("Delete {}?", noun.count(n)), body)
        .confirm_label(format!("Delete {n}"))
}

/// The line the bar shows after an action; `true` when nothing went wrong.
#[must_use]
pub fn report_message(action: BulkAction, noun: Noun, r: &BulkReport) -> (bool, String) {
    let done = noun.count(r.done.len());
    let mut text = match action {
        BulkAction::Monitor => format!("Monitored {done}."),
        BulkAction::Unmonitor => format!("Unmonitored {done}."),
        BulkAction::Search => format!(
            "Searching {done} ({} {} started).",
            r.searches_started,
            if r.searches_started == 1 {
                "search"
            } else {
                "searches"
            }
        ),
        BulkAction::Delete { files: true } => format!("Deleted {done} and their files."),
        BulkAction::Delete { files: false } => format!("Deleted {done}."),
    };
    if !r.skipped.is_empty() {
        text.push_str(&format!(
            " {} had nothing to search.",
            noun.count(r.skipped.len())
        ));
    }
    if !r.not_found.is_empty() {
        text.push_str(&format!(" {} no longer exist.", r.not_found.len()));
    }
    if let Some(first) = r.failed.first() {
        text.push_str(&format!(" {} failed: {}", r.failed.len(), first.reason));
    }
    (r.failed.is_empty(), text)
}

/// A record a bulk action can update in place, so the wall shows the result
/// without a second request.
pub trait BulkItem {
    fn bulk_id(&self) -> &str;
    fn set_monitored(&mut self, monitored: bool);
}

impl BulkItem for api::Movie {
    fn bulk_id(&self) -> &str {
        &self.id
    }
    fn set_monitored(&mut self, monitored: bool) {
        self.monitored = monitored;
    }
}

impl BulkItem for api::Series {
    fn bulk_id(&self) -> &str {
        &self.id
    }
    fn set_monitored(&mut self, monitored: bool) {
        self.monitored = monitored;
    }
}

impl BulkItem for api::Book {
    fn bulk_id(&self) -> &str {
        &self.id
    }
    fn set_monitored(&mut self, monitored: bool) {
        self.monitored = monitored;
    }
}

/// Apply a finished action to the loaded list: monitor flags flip and deleted
/// items (and ones the server no longer has) leave. Search changes nothing
/// here; its effect shows as the runs report status.
pub fn apply_report<T: BulkItem>(items: &mut Vec<T>, action: BulkAction, r: &BulkReport) {
    let done: HashSet<&str> = r.done.iter().map(String::as_str).collect();
    let gone: HashSet<&str> = r.not_found.iter().map(String::as_str).collect();
    match action {
        BulkAction::Monitor | BulkAction::Unmonitor => {
            let m = action == BulkAction::Monitor;
            for item in items.iter_mut() {
                if done.contains(item.bulk_id()) {
                    item.set_monitored(m);
                }
            }
        }
        BulkAction::Delete { .. } => {
            items.retain(|i| !done.contains(i.bulk_id()));
        }
        BulkAction::Search => {}
    }
    items.retain(|i| !gone.contains(i.bulk_id()));
}

/// The bulk row in the library toolbar: "Select" when off; the count, select
/// all, the four actions and "Done" when on. `visible` is the wall's sorted,
/// filtered id list; `collection` is `movies`, `series` or `books`;
/// `on_done` receives each finished action with the server's report.
#[component]
pub fn BulkBar(
    sel: Selection,
    #[prop(into)] visible: Signal<Vec<String>>,
    collection: &'static str,
    noun: Noun,
    on_done: Callback<(BulkAction, BulkReport)>,
) -> impl IntoView {
    let busy = RwSignal::new(false);
    let msg = RwSignal::new(None::<(bool, String)>);
    let targets = move || sel.ids.with(|s| visible.with(|v| ordered_selection(v, s)));
    let count = move || targets().len();

    let run = move |action: BulkAction| {
        let ids = targets();
        if ids.is_empty() || busy.get_untracked() {
            return;
        }
        spawn_local(async move {
            if let BulkAction::Delete { files } = action
                && !confirm(delete_confirm(noun, ids.len(), files)).await
            {
                return;
            }
            if busy.get_untracked() {
                return;
            }
            busy.set(true);
            msg.set(None);
            match api::bulk(collection, &BulkRequest::new(ids, action)).await {
                Ok(report) => {
                    msg.set(Some(report_message(action, noun, &report)));
                    if matches!(action, BulkAction::Delete { .. }) {
                        sel.ids.update(|s| {
                            for id in &report.done {
                                s.remove(id);
                            }
                        });
                    }
                    on_done.run((action, report));
                }
                Err(e) => msg.set(Some((false, e.to_string()))),
            }
            busy.set(false);
        });
    };
    let action_btn = move |label: &'static str, action: BulkAction, danger: bool| {
        view! {
            <button
                class=if danger { "bulk-action danger" } else { "bulk-action" }
                disabled=move || busy.get() || count() == 0
                on:click=move |_| run(action)
            >
                {label}
            </button>
        }
    };

    view! {
        {move || if sel.mode.get() {
            view! {
                <span class="bulk-count mono" aria-live="polite">{move || format!("{} selected", count())}</span>
                <button
                    class="bulk-all"
                    disabled=move || busy.get() || visible.with(Vec::is_empty)
                    on:click=move |_| {
                        let next = sel.ids.with_untracked(|s| visible.with_untracked(|v| select_all_or_none(v, s)));
                        sel.ids.set(next);
                    }
                >
                    {move || {
                        let all = sel.ids.with(|s| visible.with(|v| !v.is_empty() && v.iter().all(|id| s.contains(id))));
                        if all { "Clear".to_string() } else { format!("Select all ({})", visible.with(Vec::len)) }
                    }}
                </button>
                {action_btn("Monitor", BulkAction::Monitor, false)}
                {action_btn("Unmonitor", BulkAction::Unmonitor, false)}
                {action_btn("Search", BulkAction::Search, false)}
                {action_btn("Delete", BulkAction::Delete { files: false }, true)}
                {action_btn("Delete + files", BulkAction::Delete { files: true }, true)}
                <button class="bulk-done" on:click=move |_| { sel.exit(); msg.set(None); }>"Done"</button>
            }.into_any()
        } else {
            view! {
                <button class="bulk-toggle" on:click=move |_| sel.mode.set(true)>"Select"</button>
            }.into_any()
        }}
        {move || msg.get().map(|(ok, text)| view! {
            <span class=if ok { "bulk-msg ok" } else { "bulk-msg bad" }>{text}</span>
        })}
    }
}

/// Wire a tile into select mode: the class string for the tile (adds
/// `selecting` / `selected`), reactive.
pub fn tile_class(base: &'static str, sel: Option<Selection>, id: String) -> impl Fn() -> String {
    move || match sel {
        Some(s) if s.mode.get() => {
            if s.has(&id) {
                format!("{base} selecting selected")
            } else {
                format!("{base} selecting")
            }
        }
        _ => base.to_string(),
    }
}

/// A tile's role (SKADI-T-0700): a link to the detail page, or a checkbox in
/// select mode. The tile itself carries the role and the state, so its check
/// mark is decoration only.
pub fn tile_role(sel: Option<Selection>) -> impl Fn() -> &'static str {
    move || match sel {
        Some(s) if s.mode.get() => "checkbox",
        _ => "link",
    }
}

/// The tile's `aria-checked`: set in select mode only.
pub fn tile_checked(sel: Option<Selection>, id: String) -> impl Fn() -> Option<&'static str> {
    move || match sel {
        Some(s) if s.mode.get() => Some(if s.has(&id) { "true" } else { "false" }),
        _ => None,
    }
}

/// The check mark a tile shows in select mode.
pub fn tile_check(sel: Option<Selection>, id: String) -> impl IntoView {
    move || {
        let s = sel?;
        if !s.mode.get() {
            return None;
        }
        let on = s.has(&id);
        Some(view! {
            <span class=if on { "tile-check on" } else { "tile-check" } aria-hidden="true">
                {if on { "✓" } else { "" }}
            </span>
        })
    }
}
