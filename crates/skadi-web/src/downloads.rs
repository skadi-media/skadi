//! Row state, filter and sort model of the Downloads list (`/downloaders`,
//! SKADI-T-0686). Pure: the component in `app.rs` holds a [`StateFilter`] and
//! two [`SortState`]s in signals and calls [`visible`] on each poll. Because the
//! filter and sort live outside the fetched rows, a poll refresh changes only
//! the values, never the selection.
//!
//! Every row has exactly one [`RowState`] ([`row_state`]), so the chip counts of
//! [`state_counts`] add up to the "All" count. The row badges ([`row_badge`],
//! SKADI-T-0687) come from the same state, so a badge always matches its chip;
//! bulk select (SKADI-T-0688, [`Selection`], [`bulk_targets`]) keys off the
//! row `id`.

use std::cmp::Ordering;
use std::collections::HashSet;

use crate::api::Download;

/// The one state a row is in, for the chips and (later) the row badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowState {
    /// `downloading`, `queued`, or any status this build does not know.
    Downloading,
    /// Completed and still managed (the API reports `seeding`).
    Seeding,
    Paused,
    /// The worker saw no peers and no progress for its stall timeout.
    Stalled,
    /// Status `error`, or a row of any status that carries an error message.
    Errored,
}

impl RowState {
    /// Wire/class key (`downloading`, `seeding`, …).
    pub fn key(self) -> &'static str {
        match self {
            RowState::Downloading => "downloading",
            RowState::Seeding => "seeding",
            RowState::Paused => "paused",
            RowState::Stalled => "stalled",
            RowState::Errored => "errored",
        }
    }
}

/// The state of one row. An error wins over the status, so a stalled or
/// seeding row that failed counts once, under Errored.
pub fn row_state(d: &Download) -> RowState {
    let has_error = d.error.as_deref().is_some_and(|e| !e.trim().is_empty());
    if has_error || d.status == "error" {
        return RowState::Errored;
    }
    match d.status.as_str() {
        "seeding" => RowState::Seeding,
        "paused" => RowState::Paused,
        "stalled" => RowState::Stalled,
        _ => RowState::Downloading,
    }
}

impl RowState {
    /// The status level of the state, in the class names that `.badge`,
    /// `.status-dot` and `.health-dot` share (`ok` / `pending` / `warn` / `bad`
    /// / `muted`).
    pub fn level(self) -> &'static str {
        match self {
            RowState::Downloading => "pending",
            RowState::Seeding => "ok",
            RowState::Paused => "muted",
            RowState::Stalled => "warn",
            RowState::Errored => "bad",
        }
    }
}

/// The badge a row shows before its name (SKADI-T-0687). Only the states that
/// need attention have one: a healthy row stays quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowBadge {
    pub label: &'static str,
    /// `.badge` level class (`warn` / `bad`), the same as [`RowState::level`].
    pub level: &'static str,
    /// The tooltip.
    pub title: String,
}

/// Tooltip of the "Stalled" badge. The worker records no time when a transfer
/// stalls (the row's `updated_at` moves on each progress tick), so the
/// tooltip gives the rule, not a duration.
pub const STALLED_TITLE: &str =
    "No peers and no progress for the stall timeout (start time not recorded)";

/// The message of an errored row: its error text, else a fixed line, so an
/// errored row always shows why.
pub fn error_message(d: &Download) -> Option<String> {
    if row_state(d) != RowState::Errored {
        return None;
    }
    Some(
        d.error
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .unwrap_or("The transfer failed. The worker gave no reason.")
            .to_string(),
    )
}

/// The badge of one row: "Stalled" (warn) or "Error" (bad, the message as
/// tooltip); `None` for a downloading, seeding or paused row.
pub fn row_badge(d: &Download) -> Option<RowBadge> {
    let state = row_state(d);
    match state {
        RowState::Stalled => Some(RowBadge {
            label: "Stalled",
            level: state.level(),
            title: STALLED_TITLE.to_string(),
        }),
        RowState::Errored => Some(RowBadge {
            label: "Error",
            level: state.level(),
            title: error_message(d).unwrap_or_default(),
        }),
        _ => None,
    }
}

/// A filter chip: every row, or the rows in one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateFilter {
    All,
    Only(RowState),
}

/// The chips, in display order.
pub const FILTERS: [StateFilter; 6] = [
    StateFilter::All,
    StateFilter::Only(RowState::Downloading),
    StateFilter::Only(RowState::Seeding),
    StateFilter::Only(RowState::Paused),
    StateFilter::Only(RowState::Stalled),
    StateFilter::Only(RowState::Errored),
];

/// localStorage key of the selected chip.
pub const FILTER_STORAGE_KEY: &str = "downloads_filter";

impl StateFilter {
    /// Storage key (`all`, or the [`RowState::key`]).
    pub fn key(self) -> &'static str {
        match self {
            StateFilter::All => "all",
            StateFilter::Only(s) => s.key(),
        }
    }

    /// The chip label.
    pub fn label(self) -> &'static str {
        match self {
            StateFilter::All => "All",
            StateFilter::Only(RowState::Downloading) => "Downloading",
            StateFilter::Only(RowState::Seeding) => "Seeding",
            StateFilter::Only(RowState::Paused) => "Paused",
            StateFilter::Only(RowState::Stalled) => "Stalled",
            StateFilter::Only(RowState::Errored) => "Errored",
        }
    }

    /// Back from a stored key; anything unknown (or nothing) is All.
    pub fn from_key(key: Option<&str>) -> StateFilter {
        FILTERS
            .into_iter()
            .find(|f| Some(f.key()) == key)
            .unwrap_or(StateFilter::All)
    }

    pub fn matches(self, d: &Download) -> bool {
        match self {
            StateFilter::All => true,
            StateFilter::Only(s) => row_state(d) == s,
        }
    }
}

/// Rows per chip, in [`FILTERS`] order.
pub fn state_counts(rows: &[Download]) -> Vec<(StateFilter, usize)> {
    FILTERS
        .into_iter()
        .map(|f| (f, rows.iter().filter(|d| f.matches(d)).count()))
        .collect()
}

/// A sortable column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    Name,
    Size,
    Progress,
    Down,
    Up,
    Peers,
    Eta,
    Ratio,
    Added,
}

const SORT_KEYS: [SortKey; 9] = [
    SortKey::Name,
    SortKey::Size,
    SortKey::Progress,
    SortKey::Down,
    SortKey::Up,
    SortKey::Peers,
    SortKey::Eta,
    SortKey::Ratio,
    SortKey::Added,
];

impl SortKey {
    pub fn key(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Progress => "progress",
            SortKey::Down => "down",
            SortKey::Up => "up",
            SortKey::Peers => "peers",
            SortKey::Eta => "eta",
            SortKey::Ratio => "ratio",
            SortKey::Added => "added",
        }
    }

    fn from_key(key: &str) -> Option<SortKey> {
        SORT_KEYS.into_iter().find(|k| k.key() == key)
    }
}

/// The header of the active table: column and label, in grid order.
pub const ACTIVE_COLUMNS: [(SortKey, &str); 8] = [
    (SortKey::Name, "Name"),
    (SortKey::Size, "Size"),
    (SortKey::Progress, "Progress"),
    (SortKey::Down, "↓ Spd"),
    (SortKey::Up, "↑ Spd"),
    (SortKey::Peers, "Peers"),
    (SortKey::Eta, "ETA"),
    (SortKey::Added, "Added"),
];

/// The header of the Seeding list.
pub const SEEDING_COLUMNS: [(SortKey, &str); 5] = [
    (SortKey::Name, "Name"),
    (SortKey::Size, "Size"),
    (SortKey::Up, "↑ Spd"),
    (SortKey::Ratio, "Ratio"),
    (SortKey::Added, "Added"),
];

/// localStorage keys of the two sort states.
pub const ACTIVE_SORT_STORAGE_KEY: &str = "downloads_sort";
pub const SEEDING_SORT_STORAGE_KEY: &str = "seeding_sort";

/// Column plus direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub asc: bool,
}

impl Default for SortState {
    /// Newest first: `created_at` does not change between polls, so the rows
    /// hold still (a live column such as progress reorders them as they tick).
    fn default() -> Self {
        SortState {
            key: SortKey::Added,
            asc: false,
        }
    }
}

impl SortState {
    /// A header click: the same column flips direction, a new column starts
    /// ascending.
    pub fn clicked(self, key: SortKey) -> SortState {
        if self.key == key {
            SortState {
                key,
                asc: !self.asc,
            }
        } else {
            SortState { key, asc: true }
        }
    }

    /// `added:desc` and the like, for localStorage.
    pub fn encode(self) -> String {
        format!(
            "{}:{}",
            self.key.key(),
            if self.asc { "asc" } else { "desc" }
        )
    }

    /// Back from [`encode`](Self::encode); anything malformed is the default.
    pub fn decode(s: Option<&str>) -> SortState {
        s.and_then(|s| {
            let (k, dir) = s.split_once(':')?;
            let asc = match dir {
                "asc" => true,
                "desc" => false,
                _ => return None,
            };
            Some(SortState {
                key: SortKey::from_key(k)?,
                asc,
            })
        })
        .unwrap_or_default()
    }

    /// The header arrow for `key` (empty when it is not the sort column).
    pub fn arrow(self, key: SortKey) -> &'static str {
        match (self.key == key, self.asc) {
            (false, _) => "",
            (true, true) => " ▲",
            (true, false) => " ▼",
        }
    }
}

fn cmp_by(
    key: SortKey,
    a: &Download,
    b: &Download,
    name: &dyn Fn(&Download) -> String,
) -> Ordering {
    match key {
        SortKey::Name => name(a).to_lowercase().cmp(&name(b).to_lowercase()),
        SortKey::Size => a.total_bytes.cmp(&b.total_bytes),
        SortKey::Progress => a.percent.total_cmp(&b.percent),
        SortKey::Down => a
            .down_speed_bps
            .unwrap_or(0)
            .cmp(&b.down_speed_bps.unwrap_or(0)),
        SortKey::Up => a
            .up_speed_bps
            .unwrap_or(0)
            .cmp(&b.up_speed_bps.unwrap_or(0)),
        SortKey::Peers => a.peers.unwrap_or(0).cmp(&b.peers.unwrap_or(0)),
        // No ETA = never: after every known one.
        SortKey::Eta => a
            .eta_seconds
            .unwrap_or(i64::MAX)
            .cmp(&b.eta_seconds.unwrap_or(i64::MAX)),
        SortKey::Ratio => a.ratio.unwrap_or(0.0).total_cmp(&b.ratio.unwrap_or(0.0)),
        // Fixed-width UTC strings sort in time order; a row without one is oldest.
        SortKey::Added => a
            .created_at
            .as_deref()
            .unwrap_or("")
            .cmp(b.created_at.as_deref().unwrap_or("")),
    }
}

/// Sort in place. Ties fall back to the row `id` (always ascending), so rows
/// with equal values keep one order across polls whatever order the API sends.
pub fn sort_rows(rows: &mut [Download], sort: SortState, name: &dyn Fn(&Download) -> String) {
    rows.sort_by(|a, b| {
        let primary = cmp_by(sort.key, a, b, name);
        let primary = if sort.asc { primary } else { primary.reverse() };
        primary.then_with(|| a.id.cmp(&b.id))
    });
}

/// What the list shows for one filter: the active table and the Seeding list,
/// each filtered and sorted with its own sort state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Visible {
    pub active: Vec<Download>,
    pub seeding: Vec<Download>,
}

/// Split by status (`seeding` goes to the Seeding list, the rest to the active
/// table), keep the rows the filter matches, and sort each part.
pub fn visible(
    rows: &[Download],
    filter: StateFilter,
    active_sort: SortState,
    seeding_sort: SortState,
    name: &dyn Fn(&Download) -> String,
) -> Visible {
    let (mut seeding, mut active): (Vec<Download>, Vec<Download>) = rows
        .iter()
        .filter(|d| filter.matches(d))
        .cloned()
        .partition(|d| d.status == "seeding");
    sort_rows(&mut active, active_sort, name);
    sort_rows(&mut seeding, seeding_sort, name);
    Visible { active, seeding }
}

// ---- Multi-select and bulk actions (SKADI-T-0688) ----
//
// The selection is a set of `Download.id`s held in a signal of its own, apart
// from the polled rows, so a poll refresh keeps it. Only the active table has
// per-row actions (the Seeding list has none), so only its rows can be
// selected, and the bulk actions are the per-row actions: Pause / Resume where
// a row offers it, Remove, Remove with files.

/// The ids of the selected rows.
pub type Selection = HashSet<String>;

/// Keep only the ids that are still selectable: the row is still in the list
/// and still in the active table (a row that went to Seeding or left the
/// client drops out). Called on each poll.
pub fn prune_selection(sel: &Selection, rows: &[Download]) -> Selection {
    rows.iter()
        .filter(|d| d.status != "seeding" && sel.contains(&d.id))
        .map(|d| d.id.clone())
        .collect()
}

/// Select `id`, or unselect it when it is selected.
pub fn toggle_selected(sel: &mut Selection, id: &str) {
    if !sel.remove(id) {
        sel.insert(id.to_string());
    }
}

/// The state of the select-all box over the rows on show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectAll {
    None,
    Some,
    All,
}

/// How many of the `shown` rows are selected, as the select-all box shows it.
/// No rows on show is `None`.
pub fn select_all_state(sel: &Selection, shown: &[Download]) -> SelectAll {
    let n = shown.iter().filter(|d| sel.contains(&d.id)).count();
    match n {
        0 => SelectAll::None,
        n if n == shown.len() => SelectAll::All,
        _ => SelectAll::Some,
    }
}

/// The select-all box was clicked. When every row on show is selected, they
/// are all unselected; else every row on show is selected. Rows the filter
/// hides are not touched: select-all is scoped to the current filter.
pub fn toggle_all(sel: &mut Selection, shown: &[Download]) {
    if select_all_state(sel, shown) == SelectAll::All {
        for d in shown {
            sel.remove(&d.id);
        }
    } else {
        sel.extend(shown.iter().map(|d| d.id.clone()));
    }
}

/// The selected rows among those on show, in on-screen order. A selected row
/// that the filter hides now is left out: a bulk action works only on what the
/// operator can see.
pub fn selected_rows<'a>(sel: &Selection, shown: &'a [Download]) -> Vec<&'a Download> {
    shown.iter().filter(|d| sel.contains(&d.id)).collect()
}

/// An action of the bulk bar. Each one is a per-row action, fanned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAction {
    Pause,
    Resume,
    /// Remove from the client, keep the files.
    Remove,
    /// Remove and delete the downloaded files.
    RemoveWithFiles,
}

/// The bar's order.
pub const BULK_ACTIONS: [BulkAction; 4] = [
    BulkAction::Pause,
    BulkAction::Resume,
    BulkAction::Remove,
    BulkAction::RemoveWithFiles,
];

impl BulkAction {
    pub fn label(self) -> &'static str {
        match self {
            BulkAction::Pause => "Pause",
            BulkAction::Resume => "Resume",
            BulkAction::Remove => "Remove",
            BulkAction::RemoveWithFiles => "Remove with files",
        }
    }

    /// Whether `d` offers this action as a row action. A failed row (status
    /// `error`) has nothing to pause or resume; Pause is offered on a row that
    /// is not paused, Resume on a paused one; every row can be removed.
    pub fn applies_to(self, d: &Download) -> bool {
        match self {
            BulkAction::Pause => d.status != "error" && d.status != "paused",
            BulkAction::Resume => d.status == "paused",
            BulkAction::Remove | BulkAction::RemoveWithFiles => true,
        }
    }

    /// Both removes ask before they run, as the row buttons do.
    pub fn is_destructive(self) -> bool {
        matches!(self, BulkAction::Remove | BulkAction::RemoveWithFiles)
    }
}

/// The ids `action` runs on: the selected rows on show that offer it, in
/// on-screen order.
pub fn bulk_targets(action: BulkAction, sel: &Selection, shown: &[Download]) -> Vec<String> {
    selected_rows(sel, shown)
        .into_iter()
        .filter(|d| action.applies_to(d))
        .map(|d| d.id.clone())
        .collect()
}

fn transfers(n: usize) -> String {
    if n == 1 {
        "1 transfer".into()
    } else {
        format!("{n} transfers")
    }
}

/// The one question a destructive bulk action asks for the whole selection;
/// it names the count. `None` for an action that does not ask.
pub fn bulk_confirm_message(action: BulkAction, n: usize) -> Option<String> {
    match action {
        BulkAction::Remove => Some(format!(
            "Remove {} from the download client? (keeps any downloaded files)",
            transfers(n)
        )),
        BulkAction::RemoveWithFiles => Some(format!(
            "Remove {} AND delete their downloaded files? This can't be undone.",
            transfers(n)
        )),
        BulkAction::Pause | BulkAction::Resume => None,
    }
}

/// The line the bar shows after a fan-out, from the counts of calls that went
/// through and that failed. `None` when all went through.
pub fn bulk_result_message(action: BulkAction, done: usize, failed: usize) -> Option<String> {
    (failed > 0).then(|| format!("{}: {failed} of {} failed.", action.label(), done + failed))
}

/// The "Added" cell: a compact age (`5m`, `3h`, `2d`) from the seconds since
/// the job was enqueued. Unknown (no `created_at`, unparsable) is `—`; a clock
/// a little ahead of the daemon's is `now`.
pub fn age_label(secs: Option<f64>) -> String {
    let Some(secs) = secs.filter(|s| s.is_finite()) else {
        return "—".into();
    };
    let secs = secs.max(0.0) as u64;
    match secs {
        0..=59 => "now".into(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_age_label_is_compact_and_unknown_is_a_dash() {
        assert_eq!(age_label(None), "—");
        assert_eq!(age_label(Some(f64::NAN)), "—");
        assert_eq!(age_label(Some(-5.0)), "now");
        assert_eq!(age_label(Some(59.0)), "now");
        assert_eq!(age_label(Some(60.0)), "1m");
        assert_eq!(age_label(Some(3_599.0)), "59m");
        assert_eq!(age_label(Some(3_600.0)), "1h");
        assert_eq!(age_label(Some(86_400.0 * 3.5)), "3d");
    }

    fn row(id: &str, status: &str) -> Download {
        Download {
            id: id.into(),
            acquirable_ref: format!("ref-{id}"),
            status: status.into(),
            ..Default::default()
        }
    }

    fn sel(ids: &[&str]) -> Selection {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pausing_ten_selected_transfers_targets_exactly_those_ten() {
        let rows: Vec<Download> = (0..12)
            .map(|i| row(&format!("r{i}"), "downloading"))
            .collect();
        let chosen: Vec<String> = (0..10).map(|i| format!("r{i}")).collect();
        let s: Selection = chosen.iter().cloned().collect();
        assert_eq!(bulk_targets(BulkAction::Pause, &s, &rows), chosen);
    }

    #[test]
    fn pause_skips_failed_and_paused_rows_and_resume_takes_only_paused() {
        let rows = vec![
            row("a", "downloading"),
            row("b", "error"),
            row("c", "paused"),
            row("d", "stalled"),
            row("e", "queued"),
        ];
        let s = sel(&["a", "b", "c", "d", "e"]);
        assert_eq!(bulk_targets(BulkAction::Pause, &s, &rows), ["a", "d", "e"]);
        assert_eq!(bulk_targets(BulkAction::Resume, &s, &rows), ["c"]);
        assert_eq!(
            bulk_targets(BulkAction::RemoveWithFiles, &s, &rows),
            ["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn select_all_is_scoped_to_the_rows_on_show() {
        let shown = vec![row("a", "paused"), row("b", "paused")];
        // "z" is selected but the filter hides it.
        let mut s = sel(&["z"]);
        assert_eq!(select_all_state(&s, &shown), SelectAll::None);
        toggle_all(&mut s, &shown);
        assert_eq!(s, sel(&["a", "b", "z"]));
        assert_eq!(select_all_state(&s, &shown), SelectAll::All);
        // A bulk action does not reach the hidden row.
        assert_eq!(bulk_targets(BulkAction::Remove, &s, &shown), ["a", "b"]);
        toggle_all(&mut s, &shown);
        assert_eq!(s, sel(&["z"]));
        toggle_selected(&mut s, "a");
        assert_eq!(select_all_state(&s, &shown), SelectAll::Some);
        toggle_selected(&mut s, "a");
        assert_eq!(select_all_state(&s, &shown), SelectAll::None);
        assert_eq!(select_all_state(&s, &[]), SelectAll::None);
    }

    #[test]
    fn the_selection_survives_a_poll_but_drops_gone_and_seeding_rows() {
        let s = sel(&["a", "b", "c"]);
        let polled = vec![
            row("a", "downloading"),
            row("b", "seeding"),
            row("d", "queued"),
        ];
        assert_eq!(prune_selection(&s, &polled), sel(&["a"]));
    }

    #[test]
    fn the_destructive_actions_ask_once_and_name_the_count() {
        assert_eq!(
            bulk_confirm_message(BulkAction::RemoveWithFiles, 7).as_deref(),
            Some("Remove 7 transfers AND delete their downloaded files? This can't be undone.")
        );
        assert!(
            bulk_confirm_message(BulkAction::Remove, 1)
                .unwrap()
                .contains("Remove 1 transfer from")
        );
        assert_eq!(bulk_confirm_message(BulkAction::Pause, 3), None);
        assert_eq!(bulk_confirm_message(BulkAction::Resume, 3), None);
        assert!(
            BULK_ACTIONS
                .iter()
                .all(|a| a.is_destructive() == bulk_confirm_message(*a, 2).is_some())
        );
    }

    #[test]
    fn a_partial_fan_out_failure_is_reported() {
        assert_eq!(bulk_result_message(BulkAction::Pause, 10, 0), None);
        assert_eq!(
            bulk_result_message(BulkAction::Pause, 8, 2).as_deref(),
            Some("Pause: 2 of 10 failed.")
        );
    }

    #[test]
    fn stalled_and_errored_rows_get_a_badge_that_matches_their_chip() {
        let stalled = row("s", "stalled");
        let badge = row_badge(&stalled).expect("a stalled row has a badge");
        assert_eq!((badge.label, badge.level), ("Stalled", "warn"));
        assert_eq!(badge.title, STALLED_TITLE);
        assert!(StateFilter::Only(RowState::Stalled).matches(&stalled));

        let failed = Download {
            error: Some(" tracker said no ".into()),
            ..row("e", "error")
        };
        let badge = row_badge(&failed).expect("an errored row has a badge");
        assert_eq!((badge.label, badge.level), ("Error", "bad"));
        assert_eq!(badge.title, "tracker said no");
        assert_eq!(error_message(&failed).as_deref(), Some("tracker said no"));
        assert!(StateFilter::Only(RowState::Errored).matches(&failed));

        // An error on a stalled or seeding row wins: one badge, the Error one.
        let both = Download {
            error: Some("disk full".into()),
            ..row("b", "stalled")
        };
        assert_eq!(row_badge(&both).unwrap().label, "Error");

        // Status `error` without a message still says why it is red.
        let bare = row("x", "error");
        assert_eq!(row_badge(&bare).unwrap().label, "Error");
        assert!(!error_message(&bare).unwrap().is_empty());

        for quiet in ["downloading", "queued", "seeding", "paused"] {
            assert_eq!(row_badge(&row("q", quiet)), None, "{quiet}");
            assert_eq!(error_message(&row("q", quiet)), None, "{quiet}");
        }
    }

    #[test]
    fn each_state_has_a_status_level() {
        assert_eq!(RowState::Seeding.level(), "ok");
        assert_eq!(RowState::Stalled.level(), "warn");
        assert_eq!(RowState::Errored.level(), "bad");
        assert_eq!(RowState::Paused.level(), "muted");
        assert_eq!(RowState::Downloading.level(), "pending");
    }

    fn ref_name(d: &Download) -> String {
        d.acquirable_ref.clone()
    }

    fn ids(rows: &[Download]) -> Vec<&str> {
        rows.iter().map(|d| d.id.as_str()).collect()
    }

    #[test]
    fn each_row_has_one_state_and_an_error_wins() {
        assert_eq!(row_state(&row("a", "downloading")), RowState::Downloading);
        assert_eq!(row_state(&row("a", "queued")), RowState::Downloading);
        assert_eq!(row_state(&row("a", "seeding")), RowState::Seeding);
        assert_eq!(row_state(&row("a", "paused")), RowState::Paused);
        assert_eq!(row_state(&row("a", "stalled")), RowState::Stalled);
        assert_eq!(row_state(&row("a", "error")), RowState::Errored);
        let mut failed = row("a", "stalled");
        failed.error = Some("stats: gone".into());
        assert_eq!(row_state(&failed), RowState::Errored);
        failed.error = Some("  ".into());
        assert_eq!(row_state(&failed), RowState::Stalled);
    }

    #[test]
    fn chip_counts_match_the_rows_each_chip_shows_and_add_up_to_all() {
        let mut rows = vec![
            row("1", "downloading"),
            row("2", "queued"),
            row("3", "seeding"),
            row("4", "paused"),
            row("5", "stalled"),
            row("6", "seeding"),
        ];
        rows[5].error = Some("boom".into());
        let counts = state_counts(&rows);
        let n = |f| counts.iter().find(|(g, _)| *g == f).unwrap().1;
        assert_eq!(n(StateFilter::All), 6);
        assert_eq!(n(StateFilter::Only(RowState::Downloading)), 2);
        assert_eq!(n(StateFilter::Only(RowState::Seeding)), 1);
        assert_eq!(n(StateFilter::Only(RowState::Paused)), 1);
        assert_eq!(n(StateFilter::Only(RowState::Stalled)), 1);
        assert_eq!(n(StateFilter::Only(RowState::Errored)), 1);
        let per_state: usize = counts.iter().skip(1).map(|(_, c)| c).sum();
        assert_eq!(per_state, 6);
        for (f, c) in counts {
            let v = visible(
                &rows,
                f,
                SortState::default(),
                SortState::default(),
                &ref_name,
            );
            assert_eq!(v.active.len() + v.seeding.len(), c, "{f:?}");
            assert!(v.active.iter().chain(&v.seeding).all(|d| f.matches(d)));
        }
    }

    #[test]
    fn the_seeding_chip_leaves_the_active_table_empty() {
        let rows = vec![row("1", "downloading"), row("2", "seeding")];
        let v = visible(
            &rows,
            StateFilter::Only(RowState::Seeding),
            SortState::default(),
            SortState::default(),
            &ref_name,
        );
        assert!(v.active.is_empty());
        assert_eq!(ids(&v.seeding), ["2"]);
    }

    #[test]
    fn the_filter_key_round_trips_and_junk_is_all() {
        for f in FILTERS {
            assert_eq!(StateFilter::from_key(Some(f.key())), f);
        }
        assert_eq!(StateFilter::from_key(Some("nope")), StateFilter::All);
        assert_eq!(StateFilter::from_key(None), StateFilter::All);
    }

    #[test]
    fn the_sort_state_round_trips_and_junk_is_newest_first() {
        for key in SORT_KEYS {
            for asc in [true, false] {
                let s = SortState { key, asc };
                assert_eq!(SortState::decode(Some(&s.encode())), s);
            }
        }
        assert_eq!(
            SortState::decode(Some("added:sideways")),
            SortState::default()
        );
        assert_eq!(SortState::decode(Some("colour:asc")), SortState::default());
        assert_eq!(SortState::decode(None), SortState::default());
        assert_eq!(SortState::default().encode(), "added:desc");
    }

    #[test]
    fn a_header_click_flips_the_same_column_and_starts_a_new_one_ascending() {
        let s = SortState::default();
        assert_eq!(
            s.clicked(SortKey::Added),
            SortState {
                key: SortKey::Added,
                asc: true
            }
        );
        assert_eq!(
            s.clicked(SortKey::Size),
            SortState {
                key: SortKey::Size,
                asc: true
            }
        );
        assert_eq!(s.arrow(SortKey::Added), " ▼");
        assert_eq!(s.arrow(SortKey::Size), "");
    }

    #[test]
    fn added_sorts_by_created_at_and_a_missing_one_is_oldest() {
        let mut a = row("a", "downloading");
        a.created_at = Some("2026-10-07T09:00:00.000Z".into());
        let mut b = row("b", "downloading");
        b.created_at = Some("2026-10-06T23:59:59.999Z".into());
        let c = row("c", "downloading");
        let mut rows = vec![b.clone(), c.clone(), a.clone()];
        sort_rows(&mut rows, SortState::default(), &ref_name);
        assert_eq!(ids(&rows), ["a", "b", "c"]);
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Added,
                asc: true,
            },
            &ref_name,
        );
        assert_eq!(ids(&rows), ["c", "b", "a"]);
    }

    #[test]
    fn ties_keep_the_id_order_in_either_direction_whatever_order_arrives() {
        let rows = vec![
            row("b", "downloading"),
            row("c", "paused"),
            row("a", "queued"),
        ];
        for asc in [true, false] {
            let sort = SortState {
                key: SortKey::Size,
                asc,
            };
            let mut one = rows.clone();
            let mut two: Vec<_> = rows.iter().rev().cloned().collect();
            sort_rows(&mut one, sort, &ref_name);
            sort_rows(&mut two, sort, &ref_name);
            assert_eq!(ids(&one), ["a", "b", "c"]);
            assert_eq!(one, two);
        }
    }

    #[test]
    fn a_poll_with_new_live_values_keeps_the_rows_still_under_the_default_sort() {
        let mut a = row("a", "downloading");
        a.created_at = Some("2026-10-07T08:00:00.000Z".into());
        a.percent = 10.0;
        let mut b = row("b", "downloading");
        b.created_at = Some("2026-10-07T07:00:00.000Z".into());
        b.percent = 90.0;
        let name = &ref_name;
        let before = visible(
            &[b.clone(), a.clone()],
            StateFilter::All,
            SortState::default(),
            SortState::default(),
            name,
        );
        a.percent = 95.0;
        a.down_speed_bps = Some(1_000_000);
        let after = visible(
            &[a, b],
            StateFilter::All,
            SortState::default(),
            SortState::default(),
            name,
        );
        assert_eq!(ids(&before.active), ids(&after.active));
    }

    #[test]
    fn the_seeding_list_sorts_by_its_own_columns() {
        let mut x = row("x", "seeding");
        x.ratio = Some(0.4);
        let mut y = row("y", "seeding");
        y.ratio = Some(2.0);
        let z = row("z", "seeding");
        let v = visible(
            &[x, y, z],
            StateFilter::All,
            SortState::default(),
            SortState {
                key: SortKey::Ratio,
                asc: false,
            },
            &ref_name,
        );
        assert_eq!(ids(&v.seeding), ["y", "x", "z"]);
        assert!(SEEDING_COLUMNS.iter().any(|(k, _)| *k == SortKey::Ratio));
    }

    #[test]
    fn name_sort_uses_the_resolved_title_case_blind() {
        let rows = vec![row("1", "downloading"), row("2", "downloading")];
        let title = |d: &Download| {
            if d.id == "1" {
                "zulu".to_string()
            } else {
                "Alpha".to_string()
            }
        };
        let mut sorted = rows.clone();
        sort_rows(
            &mut sorted,
            SortState {
                key: SortKey::Name,
                asc: true,
            },
            &title,
        );
        assert_eq!(ids(&sorted), ["2", "1"]);
    }
}
