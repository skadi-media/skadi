//! The toolbar over each library wall — Movies, TV and Audiobooks
//! (SKADI-T-0695): status chips with live counts, an optional facet row
//! (genres, "Organize by"), a sort control and the free-text filter. A
//! `bulk` slot at the end of the toolbar is for the selection bar of
//! SKADI-T-0696.
//!
//! The order is applied in the page, not in the API. Each wall loads its
//! whole library in one request (`GET /movies`, `GET /series?view=summary`,
//! `GET /books` with no `limit`), so the sort sees every row; there is no
//! server page to get wrong. If a wall ever starts to page (the SKADI-T-0494
//! `limit`/`offset` params), its sort must move to the API.
//!
//! **One comparison.** Every wall sorts through [`compare_items`]. The title
//! and author keys it uses are [`title_sort_key`] and
//! [`crate::audiobooks::name_sort_key`]; SKADI-T-0648 (one ordering rule per
//! concept) changes those two and nothing else.

use std::cmp::Ordering;

use leptos::prelude::*;

use crate::api;

// --- Sort model (pure) ---

/// A column a library wall can be ordered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortKey {
    Title,
    Added,
    Year,
    Author,
    Status,
}

/// The sort keys of the movie and TV walls, in menu order.
pub const VIDEO_SORT_KEYS: &[SortKey] = &[
    SortKey::Title,
    SortKey::Added,
    SortKey::Year,
    SortKey::Status,
];
/// The sort keys of the audiobook wall, in menu order: author in place of year.
pub const BOOK_SORT_KEYS: &[SortKey] = &[
    SortKey::Title,
    SortKey::Added,
    SortKey::Author,
    SortKey::Status,
];

/// The order the Status sort puts the wall statuses in, ascending — the same
/// order as the status chips (Owned, Wanted, Downloading).
pub const STATUS_ORDER: &[&str] = &["owned", "wanted", "downloading"];

impl SortKey {
    /// The stable id stored in localStorage and used as the `<option>` value.
    pub fn key(self) -> &'static str {
        match self {
            SortKey::Title => "title",
            SortKey::Added => "added",
            SortKey::Year => "year",
            SortKey::Author => "author",
            SortKey::Status => "status",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Title => "Title",
            SortKey::Added => "Date added",
            SortKey::Year => "Year",
            SortKey::Author => "Author",
            SortKey::Status => "Status",
        }
    }

    pub fn from_key(s: &str) -> Option<SortKey> {
        [
            SortKey::Title,
            SortKey::Added,
            SortKey::Year,
            SortKey::Author,
            SortKey::Status,
        ]
        .into_iter()
        .find(|k| k.key() == s)
    }

    /// The direction a key starts in when it is picked: A–Z for names and
    /// status, newest first for dates and years.
    pub fn default_asc(self) -> bool {
        !matches!(self, SortKey::Added | SortKey::Year)
    }
}

/// The sort of one wall: a key and a direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub asc: bool,
}

impl Default for SortState {
    /// Title, A–Z — the order a library is expected to open in.
    fn default() -> Self {
        SortState {
            key: SortKey::Title,
            asc: true,
        }
    }
}

impl SortState {
    /// The state after `key` is picked from the menu: the same state if it is
    /// already the key, else that key in its default direction.
    #[must_use]
    pub fn picked(self, key: SortKey) -> SortState {
        if key == self.key {
            self
        } else {
            SortState {
                key,
                asc: key.default_asc(),
            }
        }
    }

    /// The state after the direction button is pressed.
    #[must_use]
    pub fn flipped(self) -> SortState {
        SortState {
            asc: !self.asc,
            ..self
        }
    }

    /// `"title:asc"` — the localStorage form.
    pub fn encode(self) -> String {
        format!(
            "{}:{}",
            self.key.key(),
            if self.asc { "asc" } else { "desc" }
        )
    }

    /// Parse [`SortState::encode`]'s form. `None` for anything else, and for a
    /// key the wall does not offer (`allowed`).
    pub fn decode(s: &str, allowed: &[SortKey]) -> Option<SortState> {
        let (k, d) = s.split_once(':')?;
        let key = SortKey::from_key(k).filter(|k| allowed.contains(k))?;
        let asc = match d {
            "asc" => true,
            "desc" => false,
            _ => return None,
        };
        Some(SortState { key, asc })
    }

    /// The stored state, or the default when nothing usable is stored.
    pub fn restore(stored: Option<&str>, allowed: &[SortKey]) -> SortState {
        stored
            .and_then(|s| SortState::decode(s, allowed))
            .unwrap_or_default()
    }

    /// `"↑"` A–Z / oldest first, `"↓"` the reverse.
    pub fn arrow(self) -> &'static str {
        if self.asc { "↑" } else { "↓" }
    }

    /// The direction in words, for the button's title and aria-label.
    pub fn direction_label(self) -> &'static str {
        match (self.key, self.asc) {
            (SortKey::Added | SortKey::Year, true) => "Oldest first",
            (SortKey::Added | SortKey::Year, false) => "Newest first",
            (SortKey::Status, true) => "Owned first",
            (SortKey::Status, false) => "Downloading first",
            (_, true) => "A to Z",
            (_, false) => "Z to A",
        }
    }
}

/// What a wall row gives the sort. Implemented for the three library records.
pub trait LibraryItem {
    fn item_id(&self) -> &str;
    fn item_title(&self) -> &str;
    /// The server `added_at` (RFC 3339), when the record carries one.
    fn item_added_at(&self) -> Option<&str>;
    fn item_year(&self) -> Option<u16> {
        None
    }
    /// The name the Author sort reads (the first credited author).
    fn item_author(&self) -> Option<&str> {
        None
    }
    /// The wall status: `owned`, `wanted` or `downloading`.
    fn item_status(&self) -> &'static str;
}

impl LibraryItem for api::Movie {
    fn item_id(&self) -> &str {
        &self.id
    }
    fn item_title(&self) -> &str {
        &self.title
    }
    fn item_added_at(&self) -> Option<&str> {
        self.added_at.as_deref()
    }
    fn item_year(&self) -> Option<u16> {
        self.year
    }
    fn item_status(&self) -> &'static str {
        crate::movies::movie_lib_status(self)
    }
}

impl LibraryItem for api::Series {
    fn item_id(&self) -> &str {
        &self.id
    }
    fn item_title(&self) -> &str {
        &self.title
    }
    fn item_added_at(&self) -> Option<&str> {
        self.added_at.as_deref()
    }
    fn item_year(&self) -> Option<u16> {
        self.year
    }
    fn item_status(&self) -> &'static str {
        crate::tv::series_lib_status(self)
    }
}

impl LibraryItem for api::Book {
    fn item_id(&self) -> &str {
        &self.id
    }
    fn item_title(&self) -> &str {
        &self.title
    }
    fn item_added_at(&self) -> Option<&str> {
        self.added_at.as_deref()
    }
    fn item_year(&self) -> Option<u16> {
        self.year
    }
    /// The first non-blank author: the same pick as the author clusters
    /// ([`crate::audiobooks::group_books_by_author`]).
    fn item_author(&self) -> Option<&str> {
        self.authors
            .iter()
            .map(String::as_str)
            .find(|a| !a.trim().is_empty())
    }
    fn item_status(&self) -> &'static str {
        crate::audiobooks::audiobook_lib_status(self)
    }
}

const TITLE_ARTICLES: &[&str] = &["the ", "a ", "an "];

/// Sort key for a title: trimmed, lowercase, one leading article ("the ",
/// "a ", "an ") dropped, so *The Hobbit* files under H. This is the rule the
/// Android walls use (`sortKey`, `clients/skadi-android/app/.../Browse.kt`);
/// the web had none before SKADI-T-0695. Articles are a title rule only: a
/// person's name keeps them ([`crate::audiobooks::name_sort_key`]).
pub fn title_sort_key(title: &str) -> String {
    let t = title.trim().to_lowercase();
    TITLE_ARTICLES
        .iter()
        .find_map(|a| t.strip_prefix(a))
        .map_or_else(|| t.clone(), str::to_string)
}

/// Sort key for a server timestamp. The server writes RFC 3339 in UTC with
/// `Z` and only as many fraction digits as it needs ("…:01Z", "…:01.5Z",
/// "…:01.123456Z"), so the raw strings do not sort in time order inside one
/// second. This pads the fraction to nine digits. A string in another form
/// is used as is. `None` for a blank value.
pub fn added_sort_key(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let Some(body) = raw.strip_suffix('Z') else {
        return Some(raw.to_string());
    };
    let (secs, frac) = body.split_once('.').unwrap_or((body, ""));
    if !frac.chars().all(|c| c.is_ascii_digit()) || frac.len() > 9 {
        return Some(raw.to_string());
    }
    Some(format!("{secs}.{frac:0<9}Z"))
}

fn status_rank(status: &str) -> usize {
    STATUS_ORDER
        .iter()
        .position(|s| *s == status)
        .unwrap_or(STATUS_ORDER.len())
}

/// Order two values; a missing value goes last in both directions.
fn present_first<T: Ord>(a: Option<T>, b: Option<T>, asc: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            if asc {
                a.cmp(&b)
            } else {
                b.cmp(&a)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// **The** wall order (SKADI-T-0695). The chosen key first, in the chosen
/// direction, with a missing value (no year, no author, no date) last. Ties
/// go to the title key A–Z, then the id, so equal rows keep one order between
/// renders. Pure.
pub fn compare_items<T: LibraryItem>(a: &T, b: &T, sort: SortState) -> Ordering {
    let primary = match sort.key {
        SortKey::Title => present_first(
            Some(title_sort_key(a.item_title())),
            Some(title_sort_key(b.item_title())),
            sort.asc,
        ),
        SortKey::Added => present_first(
            a.item_added_at().and_then(added_sort_key),
            b.item_added_at().and_then(added_sort_key),
            sort.asc,
        ),
        SortKey::Year => present_first(a.item_year(), b.item_year(), sort.asc),
        SortKey::Author => present_first(
            a.item_author().map(crate::audiobooks::name_sort_key),
            b.item_author().map(crate::audiobooks::name_sort_key),
            sort.asc,
        ),
        SortKey::Status => present_first(
            Some(status_rank(a.item_status())),
            Some(status_rank(b.item_status())),
            sort.asc,
        ),
    };
    primary
        .then_with(|| title_sort_key(a.item_title()).cmp(&title_sort_key(b.item_title())))
        .then_with(|| a.item_id().cmp(b.item_id()))
}

/// Sort `items` in place by [`compare_items`].
pub fn sort_items<T: LibraryItem>(items: &mut [T], sort: SortState) {
    items.sort_by(|a, b| compare_items(a, b, sort));
}

/// Live chip counts of a wall.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibCounts {
    pub total: usize,
    pub owned: usize,
    pub wanted: usize,
    pub downloading: usize,
}

impl LibCounts {
    /// Count wall statuses. A status other than `owned`/`downloading` is
    /// `wanted`, as the chips filter it.
    pub fn of<'a>(statuses: impl IntoIterator<Item = &'a str>) -> LibCounts {
        let mut c = LibCounts::default();
        for s in statuses {
            c.total += 1;
            match s {
                "owned" => c.owned += 1,
                "downloading" => c.downloading += 1,
                _ => c.wanted += 1,
            }
        }
        c
    }

    /// The count a chip shows: `all` (or any other key) is the total.
    pub fn get(self, chip: &str) -> usize {
        match chip {
            "owned" => self.owned,
            "wanted" => self.wanted,
            "downloading" => self.downloading,
            _ => self.total,
        }
    }
}

/// Whether a row passes the status chip (`all` passes everything).
pub fn chip_matches(chip: &str, status: &str) -> bool {
    chip == "all" || chip == status
}

// --- Persistence ---

/// Read a localStorage value. `None` when storage is not available.
pub fn local_get(key: &str) -> Option<String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
}

/// Write a localStorage value; a failure (private mode, quota) is ignored.
pub fn local_set(key: &str, value: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
}

/// The localStorage keys of the three walls' sorts.
pub const MOVIES_SORT_STORAGE: &str = "movies_sort";
pub const TV_SORT_STORAGE: &str = "tv_sort";
pub const AUDIOBOOKS_SORT_STORAGE: &str = "audiobooks_sort";

/// A wall's sort signal, started from what `storage_key` holds. The
/// [`LibraryToolbar`] writes it back on each change.
pub fn stored_sort(storage_key: &str, allowed: &[SortKey]) -> RwSignal<SortState> {
    RwSignal::new(SortState::restore(
        local_get(storage_key).as_deref(),
        allowed,
    ))
}

// --- The component ---

/// The toolbar over a library wall.
///
/// The page owns the state (`filter`, `text`, `sort`) and does the filtering
/// and sorting itself (with [`chip_matches`] and [`sort_items`]); the
/// toolbar only shows the controls and writes the sort to `storage_key`.
///
/// Slots:
/// - `facets`: a second row of chips (genres, "Organize by"), between the
///   status chips and the sort control.
/// - `bulk`: the selection bar of SKADI-T-0696. It renders as the last row
///   of the toolbar (`.lib-toolbar-bulk`, full width, under the controls),
///   so a bulk bar sits with the filter it acts under.
/// - `sort_hidden`: hide the sort control (the audiobook wall when it is
///   organized into clusters, which have their own order).
#[component]
pub fn LibraryToolbar(
    /// `(key, label)` per status chip, in order; `key` is `all` or a wall status.
    chips: &'static [(&'static str, &'static str)],
    filter: RwSignal<&'static str>,
    #[prop(into)] counts: Signal<LibCounts>,
    text: RwSignal<String>,
    placeholder: &'static str,
    sort: RwSignal<SortState>,
    sort_keys: &'static [SortKey],
    storage_key: &'static str,
    #[prop(optional, into)] facets: Option<ViewFn>,
    #[prop(optional, into)] bulk: Option<ViewFn>,
    #[prop(optional, into)] sort_hidden: MaybeProp<bool>,
) -> impl IntoView {
    let set_sort = move |next: SortState| {
        sort.set(next);
        local_set(storage_key, &next.encode());
    };
    let chip_views = chips
        .iter()
        .map(|&(key, label)| {
            view! {
                <button
                    class=move || {
                        if filter.get() == key { "filter-chip active" } else { "filter-chip" }
                    }
                    on:click=move |_| filter.set(key)
                >
                    {label}
                    <span class="chip-count mono">{move || counts.get().get(key)}</span>
                </button>
            }
        })
        .collect_view();
    let options = sort_keys
        .iter()
        .map(|&k| {
            view! {
                <option value=k.key() selected=move || sort.get().key == k>
                    {k.label()}
                </option>
            }
        })
        .collect_view();
    view! {
        <div class="filter-bar lib-toolbar">
            <div class="lib-toolbar-row">
                <div class="filter-chips">{chip_views}</div>
                {facets.map(|f| f.run())}
                <div
                    class="lib-sort"
                    style:display=move || if sort_hidden.get().unwrap_or(false) { "none" } else { "" }
                >
                    <span class="org-label mono">"Sort"</span>
                    <select
                        class="lib-sort-key"
                        aria-label="Sort by"
                        prop:value=move || sort.get().key.key()
                        on:change=move |ev| {
                            if let Some(k) = SortKey::from_key(&event_target_value(&ev)) {
                                set_sort(sort.get_untracked().picked(k));
                            }
                        }
                    >
                        {options}
                    </select>
                    <button
                        class="lib-sort-dir"
                        title=move || sort.get().direction_label()
                        aria-label=move || sort.get().direction_label()
                        on:click=move |_| set_sort(sort.get_untracked().flipped())
                    >
                        {move || sort.get().arrow()}
                    </button>
                </div>
                <input
                    class="lib-filter-input"
                    r#type="text"
                    placeholder=placeholder
                    prop:value=move || text.get()
                    on:input=move |ev| text.set(event_target_value(&ev))
                />
            </div>
            {bulk.map(|b| view! { <div class="lib-toolbar-bulk">{b.run()}</div> })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Row {
        id: &'static str,
        title: &'static str,
        added: Option<&'static str>,
        year: Option<u16>,
        author: Option<&'static str>,
        status: &'static str,
    }

    impl LibraryItem for Row {
        fn item_id(&self) -> &str {
            self.id
        }
        fn item_title(&self) -> &str {
            self.title
        }
        fn item_added_at(&self) -> Option<&str> {
            self.added
        }
        fn item_year(&self) -> Option<u16> {
            self.year
        }
        fn item_author(&self) -> Option<&str> {
            self.author
        }
        fn item_status(&self) -> &'static str {
            self.status
        }
    }

    fn row(id: &'static str, title: &'static str) -> Row {
        Row {
            id,
            title,
            added: None,
            year: None,
            author: None,
            status: "wanted",
        }
    }

    fn ids(rows: &[Row]) -> Vec<&'static str> {
        rows.iter().map(|r| r.id).collect()
    }

    fn sorted(mut rows: Vec<Row>, key: SortKey, asc: bool) -> Vec<&'static str> {
        sort_items(&mut rows, SortState { key, asc });
        ids(&rows)
    }

    #[test]
    fn titles_ignore_a_leading_article() {
        assert_eq!(title_sort_key("The Hobbit"), "hobbit");
        assert_eq!(title_sort_key("  A Quiet Place "), "quiet place");
        assert_eq!(title_sort_key("An American Werewolf"), "american werewolf");
        // Only a whole leading word, and only once.
        assert_eq!(title_sort_key("Theodore"), "theodore");
        assert_eq!(title_sort_key("Anora"), "anora");
        assert_eq!(title_sort_key("The The"), "the");
        let rows = vec![
            row("z", "Zodiac"),
            row("h", "The Hobbit"),
            row("a", "Alien"),
        ];
        assert_eq!(sorted(rows.clone(), SortKey::Title, true), ["a", "h", "z"]);
        assert_eq!(sorted(rows, SortKey::Title, false), ["z", "h", "a"]);
    }

    #[test]
    fn added_sorts_in_time_order_whatever_the_fraction() {
        assert_eq!(
            added_sort_key("2026-10-07T10:12:01Z").as_deref(),
            Some("2026-10-07T10:12:01.000000000Z")
        );
        assert_eq!(
            added_sort_key("2026-10-07T10:12:01.5Z").as_deref(),
            Some("2026-10-07T10:12:01.500000000Z")
        );
        assert_eq!(added_sort_key(" "), None);
        assert_eq!(added_sort_key("yesterday").as_deref(), Some("yesterday"));
        // Raw, ".5Z" < "Z" would put the later one first.
        let mut a = row("whole", "x");
        a.added = Some("2026-10-07T10:12:01Z");
        let mut b = row("half", "x");
        b.added = Some("2026-10-07T10:12:01.5Z");
        let mut c = row("next-day", "x");
        c.added = Some("2026-10-08T00:00:00.000001Z");
        let none = row("none", "x");
        let rows = vec![none, c, b, a];
        assert_eq!(
            sorted(rows.clone(), SortKey::Added, true),
            ["whole", "half", "next-day", "none"]
        );
        // Newest first; the row with no date stays last.
        assert_eq!(
            sorted(rows, SortKey::Added, false),
            ["next-day", "half", "whole", "none"]
        );
    }

    #[test]
    fn year_sorts_with_missing_years_last() {
        let mut a = row("a", "A");
        a.year = Some(1999);
        let mut b = row("b", "B");
        b.year = Some(2024);
        let c = row("c", "C");
        let rows = vec![c, b, a];
        assert_eq!(sorted(rows.clone(), SortKey::Year, true), ["a", "b", "c"]);
        assert_eq!(sorted(rows, SortKey::Year, false), ["b", "a", "c"]);
    }

    #[test]
    fn author_sorts_by_surname() {
        let mut king = row("king", "It");
        king.author = Some("Stephen King");
        let mut le_guin = row("leguin", "Earthsea");
        le_guin.author = Some("Ursula K. Le Guin");
        let mut asimov = row("asimov", "Foundation");
        asimov.author = Some("Isaac Asimov");
        let anon = row("anon", "Beowulf");
        let rows = vec![anon, le_guin, king, asimov];
        assert_eq!(
            sorted(rows.clone(), SortKey::Author, true),
            ["asimov", "king", "leguin", "anon"]
        );
        assert_eq!(
            sorted(rows, SortKey::Author, false),
            ["leguin", "king", "asimov", "anon"]
        );
    }

    #[test]
    fn status_sorts_in_chip_order_then_by_title() {
        let mut o = row("o", "Zed");
        o.status = "owned";
        let mut d = row("d", "Alpha");
        d.status = "downloading";
        let w1 = row("w1", "The Middle");
        let w2 = row("w2", "Beta");
        let rows = vec![d, w1, o, w2];
        assert_eq!(
            sorted(rows.clone(), SortKey::Status, true),
            ["o", "w2", "w1", "d"]
        );
        // Reversed status; ties still A–Z by title.
        assert_eq!(sorted(rows, SortKey::Status, false), ["d", "w2", "w1", "o"]);
    }

    #[test]
    fn equal_rows_keep_one_order() {
        let rows = vec![row("b", "Same"), row("a", "Same"), row("c", "The Same")];
        assert_eq!(sorted(rows.clone(), SortKey::Year, true), ["a", "b", "c"]);
        assert_eq!(sorted(rows, SortKey::Year, false), ["a", "b", "c"]);
    }

    #[test]
    fn sort_state_round_trips_and_rejects_keys_the_wall_lacks() {
        for &k in BOOK_SORT_KEYS.iter().chain(VIDEO_SORT_KEYS) {
            for asc in [true, false] {
                let s = SortState { key: k, asc };
                assert_eq!(SortState::decode(&s.encode(), &[k]), Some(s));
            }
        }
        assert_eq!(SortState::decode("year:desc", BOOK_SORT_KEYS), None);
        assert_eq!(SortState::decode("author:asc", VIDEO_SORT_KEYS), None);
        assert_eq!(SortState::decode("title", VIDEO_SORT_KEYS), None);
        assert_eq!(SortState::decode("title:up", VIDEO_SORT_KEYS), None);
        assert_eq!(
            SortState::restore(Some("garbage"), VIDEO_SORT_KEYS),
            SortState::default()
        );
        assert_eq!(
            SortState::restore(None, VIDEO_SORT_KEYS),
            SortState::default()
        );
        assert_eq!(
            SortState::restore(Some("added:desc"), VIDEO_SORT_KEYS),
            SortState {
                key: SortKey::Added,
                asc: false
            }
        );
    }

    #[test]
    fn picking_a_key_starts_in_its_default_direction() {
        let s = SortState::default();
        assert_eq!(
            s.picked(SortKey::Added),
            SortState {
                key: SortKey::Added,
                asc: false
            }
        );
        assert!(s.picked(SortKey::Status).asc);
        let flipped = s.flipped();
        assert!(!flipped.asc);
        // Picking the current key keeps its direction.
        assert_eq!(flipped.picked(SortKey::Title), flipped);
        assert_eq!(s.arrow(), "↑");
        assert_eq!(flipped.direction_label(), "Z to A");
    }

    #[test]
    fn counts_add_up_to_the_total() {
        let c = LibCounts::of(["owned", "wanted", "downloading", "owned", "other"]);
        assert_eq!(
            c,
            LibCounts {
                total: 5,
                owned: 2,
                wanted: 2,
                downloading: 1
            }
        );
        assert_eq!(c.get("all"), 5);
        assert_eq!(c.get("owned"), 2);
        assert!(chip_matches("all", "owned"));
        assert!(chip_matches("wanted", "wanted"));
        assert!(!chip_matches("owned", "wanted"));
    }
}
