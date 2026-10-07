//! Audiobooks domain view (SKADI-T-0133): browse the book library with per-file
//! acquisition status, add books by Audible ASIN, manage authors, trigger a
//! manual acquire, and toggle monitoring / delete. A deliberate parallel to
//! [`crate::movies`], adapted to the Author → Series → Book shape.
//!
//! Lists come from `GET /books` and `GET /authors` (they work even when the
//! audiobooks domain is disabled, so you can add + configure before enabling).
//! Profile/root pickers come from the shared `profiles`/`root_folders` settings.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::api;
use crate::confirm::{ConfirmSpec, confirm};
use crate::library_select::{BulkBar, Selection, apply_report, tile_check, tile_class};
use crate::library_toolbar::{
    AUDIOBOOKS_SORT_STORAGE, BOOK_SORT_KEYS, LibCounts, LibraryToolbar, chip_matches, sort_items,
    stored_sort,
};
use crate::movies::{DiagnosticsPanel, HistoryPanel, ReleasesPanel, status_class};

/// Decode the handful of HTML entities Audnexus/Audible synopses actually use.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&rsquo;", "\u{2019}")
        .replace("&lsquo;", "\u{2018}")
        .replace("&rdquo;", "\u{201d}")
        .replace("&ldquo;", "\u{201c}")
        .replace("&mdash;", "\u{2014}")
        .replace("&ndash;", "\u{2013}")
        .replace("&hellip;", "\u{2026}")
        .replace("&nbsp;", " ")
}

/// Turn an HTML-ish description into clean plain-text paragraphs. Audnexus /
/// Audible synopses carry `<p>`, `<b>`, `<br>` markup that we used to print
/// verbatim (SKADI-T-0141). Block boundaries (`<p>`, `</p>`, `<br>`, `<div>`,
/// `<li>`) become paragraph breaks; every other tag is dropped; common HTML
/// entities are decoded; internal whitespace is collapsed. Returns one string
/// per non-empty paragraph. Pure, so it's unit-testable.
pub fn clean_overview(raw: &str) -> Vec<String> {
    let mut text = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '<' {
            text.push(c);
            continue;
        }
        // Consume the tag up to the closing '>'.
        let mut tag = String::new();
        for n in chars.by_ref() {
            if n == '>' {
                break;
            }
            tag.push(n);
        }
        // Block-level tags become paragraph breaks; inline tags just vanish.
        let name: String = tag
            .trim()
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "p" | "br" | "div" | "li"
        ) {
            text.push('\n');
        }
    }
    let text = decode_entities(&text);
    text.split('\n')
        .map(|para| para.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|para| !para.is_empty())
        .collect()
}

/// A book's coarse library status for its cover tile: green when its file is
/// imported, amber while mid-acquisition, grey when missing. Pure, so it's
/// unit-testable.
pub fn book_status(b: &api::Book) -> (&'static str, &'static str) {
    let labels: Vec<String> = b
        .files
        .iter()
        .map(|f| api::status_label(&f.status))
        .collect();
    if labels.iter().any(|l| l == "imported" || l == "cutoff") {
        ("ok", "imported")
    } else if labels
        .iter()
        .any(|l| matches!(l.as_str(), "searching" | "snatched" | "downloading"))
    {
        ("pending", "in progress")
    } else {
        ("muted", "missing")
    }
}

/// A one-line "authors · narrators" byline for a book, omitting empties. Pure.
pub fn book_byline(b: &api::Book) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !b.authors.is_empty() {
        parts.push(b.authors.join(", "));
    }
    if !b.narrators.is_empty() {
        parts.push(format!("read by {}", b.narrators.join(", ")));
    }
    parts.join(" · ")
}

/// A series and its books, for the collapsible library grouping (SKADI-T-0153) —
/// the audiobook parallel to a Sonarr show + its seasons.
pub struct SeriesGroup {
    pub series_id: String,
    pub name: String,
    /// Books in the series, ordered by series position (then title).
    pub books: Vec<api::Book>,
}

/// Numeric sort key for a series position string (`"1"`, `"1.5"`, `"0.5"`);
/// unparseable / missing positions sort last.
fn series_pos_key(book: &api::Book) -> f64 {
    book.series
        .as_ref()
        .and_then(|s| s.position.as_ref())
        .and_then(|p| p.trim().parse::<f64>().ok())
        .unwrap_or(f64::INFINITY)
}

/// Split a book list into **series groups** (each a collapsible unit, sorted by
/// name; books within sorted by series position then title) and **standalone**
/// books (no series), preserving the input order of standalones. Pure, so it's
/// unit-testable (SKADI-T-0153).
pub fn group_books_by_series(books: Vec<api::Book>) -> (Vec<SeriesGroup>, Vec<api::Book>) {
    let mut groups: Vec<SeriesGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut standalone: Vec<api::Book> = Vec::new();
    for b in books {
        match b.series.clone() {
            Some(s) => {
                let i = *index.entry(s.series_id.clone()).or_insert_with(|| {
                    groups.push(SeriesGroup {
                        series_id: s.series_id.clone(),
                        name: s.name.clone(),
                        books: Vec::new(),
                    });
                    groups.len() - 1
                });
                groups[i].books.push(b);
            }
            None => standalone.push(b),
        }
    }
    for g in &mut groups {
        g.books.sort_by(|a, b| {
            series_pos_key(a)
                .partial_cmp(&series_pos_key(b))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
    }
    groups.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    (groups, standalone)
}

/// A normalized key for matching an author name across sources — lowercase,
/// ASCII-alphanumeric only (drops '.', spacing, punctuation). A book may store
/// "A.G. Riddle" while the registered author is "A. G. Riddle"; keying both by this
/// lets the cluster still find its ASIN (and thus render the Watch button). Mirrors
/// `skadi_audiobooks::discovery::name_key` used when registering authors (T-0363).
fn author_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

const NAME_PARTICLES: &[&str] = &[
    "le", "la", "de", "du", "da", "di", "del", "della", "des", "van", "von", "der", "den", "ter",
    "ten", "dos", "das", "st", "st.", "al", "el", "bin", "ibn",
];
const NAME_SUFFIXES: &[&str] = &[
    "jr", "jr.", "sr", "sr.", "ii", "iii", "iv", "phd", "ph.d.", "md", "m.d.",
];
const COLLECTIVE_CREDITS: &[&str] = &[
    "full cast",
    "various",
    "various authors",
    "anonymous",
    "unknown author",
];

/// Sort key for a person's name, **surname first** (SKADI-T-0648 F1): "Stephen
/// King" orders as "king stephen". Audnexus exposes no sort name, so the surname
/// is derived — the last word plus any particles before it ("Le Guin", "van
/// Vogt"), ignoring a suffix ("Jr.", "III"). "Surname, Given" is taken as
/// written; collective credits ("Full Cast") keep their order. Mirrors the
/// Android `NameSorting.key`; both test the same vector.
pub fn name_sort_key(name: &str) -> String {
    let n = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if n.is_empty() || COLLECTIVE_CREDITS.contains(&n.as_str()) {
        return n;
    }
    if let Some((head, tail)) = n.split_once(", ")
        && !NAME_SUFFIXES.contains(&tail.trim_end_matches(','))
    {
        return format!("{head} {tail}");
    }
    let mut words: Vec<&str> = n.split(' ').map(|w| w.trim_end_matches(',')).collect();
    while words.len() > 1 && NAME_SUFFIXES.contains(words.last().expect("non-empty")) {
        words.pop();
    }
    if words.len() == 1 {
        return words[0].to_string();
    }
    let mut start = words.len() - 1;
    while start > 1 && NAME_PARTICLES.contains(&words[start - 1]) {
        start -= 1;
    }
    let mut out = words[start..].to_vec();
    out.extend_from_slice(&words[..start]);
    out.join(" ")
}

/// Group books by **primary author** (`authors[0]`) for the "organize by author" pivot
/// (the author parallel to [`group_books_by_series`]). Authors sorted by surname
/// ([`name_sort_key`]); books within each by title. Books with no author go under "Unknown author". Pure.
pub fn group_books_by_author(books: Vec<api::Book>) -> Vec<(String, Vec<api::Book>)> {
    let mut map: HashMap<String, Vec<api::Book>> = HashMap::new();
    for b in books {
        let author = b
            .authors
            .iter()
            .find(|a| !a.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "Unknown author".to_string());
        map.entry(author).or_default().push(b);
    }
    let mut groups: Vec<(String, Vec<api::Book>)> = map.into_iter().collect();
    for (_, bs) in &mut groups {
        bs.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    }
    groups.sort_by_cached_key(|g| name_sort_key(&g.0));
    groups
}

/// Roll up a series' books into a status dot class + an `"N/M imported"` summary
/// for its collapsed header.
#[allow(dead_code)]
fn series_status(books: &[api::Book]) -> (&'static str, String) {
    let total = books.len();
    let imported = books
        .iter()
        .filter(|b| book_status(b).1 == "imported")
        .count();
    let in_progress = books.iter().any(|b| book_status(b).1 == "in progress");
    let dot = if total > 0 && imported == total {
        "ok"
    } else if imported > 0 || in_progress {
        "pending"
    } else {
        "muted"
    };
    (dot, format!("{imported}/{total} imported"))
}

/// `"1 book"` / `"N books"` — correct singular/plural for a series count. Pure.
pub fn books_label(n: usize) -> String {
    if n == 1 {
        "1 book".to_string()
    } else {
        format!("{n} books")
    }
}

/// Series label with position, e.g. `"Stormlight Archive #1"`. Pure.
pub fn series_label(link: &api::SeriesLink) -> String {
    match &link.position {
        Some(p) if !p.is_empty() => format!("{} #{p}", link.name),
        _ => link.name.clone(),
    }
}

/// Whether an ASIN string looks plausibly like an Audible ASIN (10 alphanumeric
/// chars — all-digit ASINs are valid, SKADI-T-0150). Used to validate the
/// add-by-ASIN forms before hitting the API. Pure.
pub fn looks_like_asin(s: &str) -> bool {
    let s = s.trim();
    s.len() == 10 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Pull a 10-character ASIN out of whatever the user pasted — a bare ASIN, or a
/// `Title [ASIN]` / `{asin-ASIN}` string copied from a folder or a store page.
/// Returns the upper-cased ASIN, or `None` if there's no 10-char alphanumeric
/// token. The **last** 10-char run wins (the ASIN is conventionally at the end,
/// often bracketed). All-digit ASINs are accepted (SKADI-T-0150). Pure.
pub fn extract_asin(raw: &str) -> Option<String> {
    let s = raw.trim();
    if looks_like_asin(s) {
        return Some(s.to_uppercase());
    }
    let mut best: Option<String> = None;
    let mut run = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            run.push(ch);
        } else {
            if run.len() == 10 {
                best = Some(run.to_uppercase());
            }
            run.clear();
        }
    }
    if run.len() == 10 {
        best = Some(run.to_uppercase());
    }
    best
}

/// A collapsible library cluster (SKADI-T-0254): a clickable heading (chevron + name +
/// count) that reveals its tiles only when expanded — collapsed by default so an
/// organized wall scrolls fast. `sub_series` sub-groups an author's books by series
/// under smaller headings. Expansion is tracked by `key` in the shared `expanded` set,
/// so it persists across filter/search re-renders.
#[component]
fn BookCluster(
    name: String,
    key: String,
    books: Vec<api::Book>,
    sub_series: bool,
    expanded: RwSignal<HashSet<String>>,
    /// `(scope, asin)` this cluster can Watch — `"author"`/`"series"` + its ASIN.
    /// `None` for clusters with no catalog identity (Standalone, Unknown author):
    /// no Watch button and no missing-works surfacing (SKADI-T-0353).
    watch: Option<(&'static str, String)>,
    /// Total known works (from the series rollup) for an `N/M` completeness count.
    total: Option<usize>,
    /// A real series cluster: list members with no series position after the
    /// numbered ones under "Related", and leave them out of the count
    /// (SKADI-T-0654). False for Standalone and author clusters, whose books
    /// have no series position by definition.
    split_related: bool,
    /// Shared `(scope, key)` set of active watchers, for reactive toggle state.
    watchers: RwSignal<HashSet<(String, String)>>,
    /// Bump to re-fetch the library after an add/grab/want.
    reload: RwSignal<u32>,
) -> impl IntoView {
    // In a series cluster only *positioned* books count, so the label agrees
    // with the rollup's `total`, which also excludes related members. Counting
    // every book showed "2/1" for A Song of Ice and Fire once its anthology
    // stopped counting.
    let n = if split_related {
        books.iter().filter(|b| book_is_positioned(b)).count()
    } else {
        books.len()
    };
    let count_label = match total {
        Some(t) if t > n => format!("{n}/{t}"),
        _ => n.to_string(),
    };
    let key_chev = key.clone();
    let key_tog = key.clone();
    let key_body = key.clone();
    let key_fx = key;

    // Missing/unowned known works for this cluster, lazily fetched on first expand
    // (so an organized wall doesn't fan out N catalog calls up front).
    let works = RwSignal::new(None::<Vec<api::Work>>);
    if let Some((scope, asin)) = watch.clone() {
        let asin = asin.clone();
        Effect::new(move |_| {
            if expanded.get().contains(&key_fx) && works.get_untracked().is_none() {
                let asin = asin.clone();
                spawn_local(async move {
                    let res = if scope == "series" {
                        api::list_works_by_series(&asin).await
                    } else {
                        api::list_works_by_author(&asin).await
                    };
                    if let Ok(ws) = res {
                        works.set(Some(ws));
                    }
                });
            }
        });
    }

    // The Watch toggle (author/series scope). Watching backfills current gaps as
    // Wanted immediately AND covers future releases (operator decision, SKADI-T-0353).
    let watch_btn = watch.clone().map(|(scope, asin)| {
        let target_cls = (scope.to_string(), asin.clone());
        let target_lbl = target_cls.clone();
        let is_on = move || watchers.get().contains(&target_cls);
        let is_on_lbl = move || watchers.get().contains(&target_lbl);
        let asin_h = asin.clone();
        let on_toggle = move |_| {
            let asin = asin_h.clone();
            let key = (scope.to_string(), asin.clone());
            let on_now = watchers.get_untracked().contains(&key);
            spawn_local(async move {
                if on_now {
                    let _ = api::clear_watcher(scope, &asin).await;
                    watchers.update(|s| {
                        s.remove(&(scope.to_string(), asin.clone()));
                    });
                } else {
                    let _ = api::set_watcher(scope, &asin).await;
                    watchers.update(|s| {
                        s.insert((scope.to_string(), asin.clone()));
                    });
                    // Rate-limited backfill (SKADI-I-0051 #8): mark at most
                    // BACKFILL_BURST existing gaps Wanted right now so a prolific
                    // author/series doesn't flood the indexers with hundreds of
                    // simultaneous searches. The watcher we just set covers the
                    // rest — the server-side discovery worker backfills the tail
                    // gradually (bounded per pass). Immediate for small gaps,
                    // paced for large ones.
                    const BACKFILL_BURST: usize = 25;
                    let res = if scope == "series" {
                        api::list_works_by_series(&asin).await
                    } else {
                        api::list_works_by_author(&asin).await
                    };
                    if let Ok(ws) = res {
                        for w in ws.into_iter().filter(|w| !w.owned).take(BACKFILL_BURST) {
                            let _ = api::add_book(&w.asin, None, false).await;
                        }
                    }
                    reload.update(|n| *n += 1);
                }
            });
        };
        view! {
            <button
                type="button"
                class="watch-btn"
                class:on=is_on
                on:click=on_toggle
                title="Watch — auto-acquire missing & future releases"
            >
                {move || if is_on_lbl() { "★ Watching" } else { "☆ Watch" }}
            </button>
        }
    });

    let reload_fn = move || reload.update(|n| *n += 1);

    view! {
        <div class="lib-cluster">
            <div class="lib-cluster-head-row">
                <button
                    class="lib-cluster-head"
                    on:click=move |_| {
                        expanded
                            .update(|s| {
                                if !s.remove(&key_tog) {
                                    s.insert(key_tog.clone());
                                }
                            })
                    }
                >
                    <span class="cluster-chevron">
                        {move || if expanded.get().contains(&key_chev) { "▾" } else { "▸" }}
                    </span>
                    <span class="lib-cluster-name">{name}</span>
                    <span class="chip-count mono">{count_label}</span>
                </button>
                {watch_btn}
            </div>
            {move || {
                if !expanded.get().contains(&key_body) {
                    return None;
                }
                let bs = books.clone();
                let ws = works.get().unwrap_or_default();
                Some(
                    if sub_series {
                        author_body(bs, &ws, reload_fn)
                    } else {
                        let tiles = merge_owned_missing(bs, &ws);
                        let (numbered, related) = if split_related {
                            split_related_tiles(tiles)
                        } else {
                            (tiles, Vec::new())
                        };
                        view! {
                            <div class="poster-grid">
                                {numbered.into_iter().map(|t| render_tile(t, reload_fn)).collect_view()}
                            </div>
                            {(!related.is_empty()).then(|| view! {
                                <div class="lib-subcluster">
                                    <h4 class="lib-subcluster-head muted">"Related"</h4>
                                    <div class="poster-grid">
                                        {related.into_iter().map(|t| render_tile(t, reload_fn)).collect_view()}
                                    </div>
                                </div>
                            })}
                        }
                            .into_any()
                    },
                )
            }}
        </div>
    }
}

/// Body of an author cluster: books grouped into per-series subclusters, each merged
/// with that series' missing works (position-ordered), plus a trailing "Standalone &
/// more" subcluster for standalone books and any missing works whose series the user
/// owns nothing of (SKADI-T-0353).
fn author_body<F: Fn() + Copy + 'static>(
    books: Vec<api::Book>,
    works: &[api::Work],
    reload: F,
) -> AnyView {
    let (groups, standalone) = group_books_by_series(books);
    let mut consumed: HashSet<String> = HashSet::new();
    let mut subs: Vec<AnyView> = groups
        .into_iter()
        .map(|g| {
            let series_works: Vec<api::Work> = works
                .iter()
                .filter(|w| {
                    w.series_name
                        .as_deref()
                        .map(|s| s.eq_ignore_ascii_case(&g.name))
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            for w in &series_works {
                consumed.insert(w.asin.clone());
            }
            let tiles = merge_owned_missing(g.books, &series_works);
            view! {
                <div class="lib-subcluster">
                    <h4 class="lib-subcluster-head">{g.name}</h4>
                    <div class="poster-grid">
                        {tiles.into_iter().map(|t| render_tile(t, reload)).collect_view()}
                    </div>
                </div>
            }
            .into_any()
        })
        .collect();

    // Standalone owned books + any missing works not already placed under a series.
    let mut tail: Vec<SeriesTile> = standalone.into_iter().map(SeriesTile::Owned).collect();
    let have: HashSet<String> = tail
        .iter()
        .filter_map(|t| match t {
            SeriesTile::Owned(b) => b.external_ids.asin.clone(),
            _ => None,
        })
        .collect();
    for w in works {
        if !w.owned && !consumed.contains(&w.asin) && !have.contains(&w.asin) {
            tail.push(SeriesTile::Missing(w.clone()));
        }
    }
    if !tail.is_empty() {
        tail.sort_by_key(|a| a.title_key());
        subs.push(
            view! {
                <div class="lib-subcluster">
                    <h4 class="lib-subcluster-head muted">"Standalone & more"</h4>
                    <div class="poster-grid">
                        {tail.into_iter().map(|t| render_tile(t, reload)).collect_view()}
                    </div>
                </div>
            }
            .into_any(),
        );
    }
    view! { <div class="lib-subclusters">{subs}</div> }.into_any()
}

#[component]
pub fn AudiobooksPage() -> impl IntoView {
    let books = RwSignal::new(Vec::<api::Book>::new());
    let error = RwSignal::new(None::<String>);
    // Library-wall filters (SKADI-T-0253): status chip + free-text filter.
    let lib_filter = RwSignal::new("all");
    let lib_text = RwSignal::new(String::new());
    // "Organize by" pivot (SKADI-T-0254): "none" (flat wall), "author", or "series".
    let organize = RwSignal::new("none");
    // Sort of the flat wall (SKADI-T-0695), remembered across reloads. The
    // author and series clusters keep their own order, so the control hides
    // while the wall is organized.
    let sort = stored_sort(AUDIOBOOKS_SORT_STORAGE, BOOK_SORT_KEYS);
    // Cluster keys (`<mode>:<name>`) currently expanded — clusters are collapsed by
    // default so an organized wall scrolls fast. Keyed by mode so expansion persists
    // per pivot and survives filter/search re-renders.
    let expanded = RwSignal::new(HashSet::<String>::new());
    // Catalog identity + watch state for one-click Watch + missing-book surfacing
    // (SKADI-T-0353). Author name (lc) → ASIN; series name (lc) → rollup; the active
    // `(scope, key)` watcher set. A reload token re-fetches the library after an action.
    let authors_asin = RwSignal::new(HashMap::<String, String>::new());
    let series_roll = RwSignal::new(HashMap::<String, api::SeriesRollup>::new());
    let watchers = RwSignal::new(HashSet::<(String, String)>::new());
    let reload = RwSignal::new(0u32);

    // Adding lives in the unified `/add` view; this page is the library wall.
    Effect::new(move |_| {
        reload.track();
        spawn_local(async move {
            match api::list_books(None, None).await {
                Ok(b) => {
                    books.set(b);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    });
    // Supporting catalog data for Watch + completeness (re-loaded on reload too, so a
    // watch toggle or add refreshes owned/total counts and watch badges).
    let role_signal = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    // Reactive accessors for the view. Same rule as the effect below: gate on a
    // role we actually know, so an unresolved role offers nothing rather than
    // everything.
    let is_admin_view = move || role_signal.and_then(|r| r.get()).as_deref() == Some("admin");
    let not_kid =
        move || matches!(role_signal.and_then(|r| r.get()).as_deref(), Some(r) if r != "kid");
    Effect::new(move |_| {
        reload.track();
        // Read in the effect body, not the async block below: an async block
        // is outside the reactive scope, so the effect would not re-run when
        // `/me` answers and an operator would lose this data entirely.
        //
        // These three are gated server-side and were being fetched for
        // everyone: authors and series rollups are refused to a kid, and the
        // watch list is admin-only, so an audiobooks page fired up to three
        // 403s per visit for anyone else (SKADI-T-0639).
        let role = role_signal.and_then(|r| r.get());
        let is_admin = role.as_deref() == Some("admin");
        // Wait for the role before fetching, rather than assuming. Negating
        // the kid check meant this was true while `/me` was still in flight,
        // so the first pass fetched anyway and a kid ate two 403s before the
        // effect re-ran. Default to *not* asking; a known role opts in.
        let may_read_rollups = matches!(role.as_deref(), Some(r) if r != "kid");
        spawn_local(async move {
            if may_read_rollups && let Ok(list) = api::list_authors(None).await {
                authors_asin.set(
                    list.into_iter()
                        .filter_map(|a| a.asin.map(|x| (author_key(&a.name), x)))
                        .collect(),
                );
            }
            if may_read_rollups && let Ok(rolls) = api::list_book_series().await {
                series_roll.set(
                    rolls
                        .into_iter()
                        .map(|r| (r.name.to_lowercase(), r))
                        .collect(),
                );
            }
            if is_admin && let Ok(ws) = api::list_watchers().await {
                watchers.set(ws.into_iter().map(|w| (w.scope, w.key)).collect());
            }
        });
    });

    let counts =
        Signal::derive(move || books.with(|bs| LibCounts::of(bs.iter().map(audiobook_lib_status))));
    // Filtered books, shared by every organize mode.
    let filtered = move || {
        let f = lib_filter.get();
        let q = lib_text.get().to_lowercase();
        books
            .get()
            .into_iter()
            .filter(|b| chip_matches(f, audiobook_lib_status(b)))
            .filter(|b| {
                q.is_empty()
                    || b.title.to_lowercase().contains(&q)
                    || b.authors.join(" ").to_lowercase().contains(&q)
            })
            .collect::<Vec<_>>()
    };
    // Multi-select + bulk bar (SKADI-T-0696), admin only, on the flat wall
    // only: the clusters have their own order and collapse, so "select all"
    // would act on tiles the operator cannot see.
    let sel = Selection::new();
    let flat_shown = move || {
        let mut bs = filtered();
        sort_items(&mut bs, sort.get());
        bs
    };
    let visible_ids =
        Signal::derive(move || flat_shown().into_iter().map(|b| b.id).collect::<Vec<_>>());
    let on_bulk_done = Callback::new(
        move |(action, report): (api::BulkAction, api::BulkReport)| {
            books.update(|bs| apply_report(bs, action, &report));
        },
    );
    Effect::new(move |_| {
        if organize.get() != "none" {
            sel.exit();
        }
    });
    // The wall: a flat poster-grid, or collapsible author/series clusters.
    let grid = move || {
        let bs = filtered();
        match organize.get() {
            "series" => {
                let rolls = series_roll.get();
                let (groups, standalone) = group_books_by_series(bs);
                let mut clusters: Vec<AnyView> = groups
                    .into_iter()
                    .map(|g| {
                        let key = format!("series:{}", g.series_id);
                        // Series ASIN + known-work total come from the completeness
                        // rollup (matched by name — SeriesLink carries no ASIN).
                        let roll = rolls.get(&g.name.to_lowercase());
                        let watch = roll
                            .and_then(|r| r.series_asin.clone())
                            .map(|a| ("series", a));
                        let total = roll.map(|r| r.total);
                        view! {
                            <BookCluster
                                name=g.name
                                key=key
                                books=g.books
                                sub_series=false
                                expanded=expanded
                                watch=watch
                                total=total
                                split_related=true
                                watchers=watchers
                                reload=reload
                            />
                        }
                        .into_any()
                    })
                    .collect();
                if !standalone.is_empty() {
                    clusters.push(
                        view! {
                            <BookCluster
                                name="Standalone".to_string()
                                key="series:__standalone".to_string()
                                books=standalone
                                sub_series=false
                                expanded=expanded
                                watch=None
                                total=None
                                split_related=false
                                watchers=watchers
                                reload=reload
                            />
                        }
                        .into_any(),
                    );
                }
                view! { <div class="lib-clusters">{clusters}</div> }.into_any()
            }
            "author" => {
                let amap = authors_asin.get();
                let clusters: Vec<AnyView> = group_books_by_author(bs)
                    .into_iter()
                    .map(|(author, abooks)| {
                        let key = format!("author:{author}");
                        let watch = amap
                            .get(&author_key(&author))
                            .cloned()
                            .map(|a| ("author", a));
                        view! {
                            <BookCluster
                                name=author
                                key=key
                                books=abooks
                                sub_series=true
                                expanded=expanded
                                watch=watch
                                total=None
                                split_related=false
                                watchers=watchers
                                reload=reload
                            />
                        }
                        .into_any()
                    })
                    .collect();
                view! { <div class="lib-clusters">{clusters}</div> }.into_any()
            }
            _ => {
                let mut bs = bs;
                sort_items(&mut bs, sort.get());
                view! {
                    <div class="poster-grid">
                        {bs.into_iter().map(|b| cover_tile_in(b, Some(sel))).collect_view()}
                    </div>
                }
                .into_any()
            }
        }
    };
    // "Organize by" pivot chip — switches the wall between flat / author / series.
    let org_chip = move |key: &'static str, label: &'static str| {
        view! {
            <button
                class=move || {
                    if organize.get() == key { "filter-chip active" } else { "filter-chip" }
                }
                on:click=move |_| organize.set(key)
            >
                {label}
            </button>
        }
    };

    view! {
        <section class="library">
            <div class="lib-head">
                <div class="lib-title">
                    <span class="lib-accent teal"></span>
                    <h2>"Audiobooks"</h2>
                </div>
                <div class="lib-head-right">
                    // The "Listen on your phone" signpost that used to sit here
                    // (SKADI-T-0349) is gone: it existed because the sidebar's
                    // "Listen" entry did not read as "phone setup". Players is
                    // its own named Settings section now (SKADI-T-0627), so the
                    // signpost was pointing at a door that already has a sign.
                    // These three were offered to everyone. Discover reads the
                    // works store, which a kid may not; Import and the gear are
                    // operator surfaces outright. The per-role sweep walks the
                    // nav strips, so it never saw them — they are in-page links
                    // (SKADI-T-0639).
                    {move || not_kid().then(|| view! {
                        <A href="/audiobooks/discover" attr:class="btn-link" attr:title="Discover unowned titles from your authors & series">"Discover"</A>
                    })}
                    {move || is_admin_view().then(|| view! {
                        <A href="/audiobooks/import" attr:class="btn-link" attr:title="Import an existing audiobook library">"Import"</A>
                        <A href="/audiobooks/config" attr:class="gear" attr:title="Audiobooks settings">"⚙"</A>
                    })}
                </div>
            </div>
            <LibraryToolbar
                chips=BOOK_CHIPS
                filter=lib_filter
                counts=counts
                text=lib_text
                placeholder="⌕ Filter library…"
                sort=sort
                sort_keys=BOOK_SORT_KEYS
                storage_key=AUDIOBOOKS_SORT_STORAGE
                sort_hidden=Signal::derive(move || organize.get() != "none")
                facets=move || view! {
                    <div class="filter-chips org-chips">
                        <span class="org-label mono">"Organize"</span>
                        {org_chip("none", "Default")} {org_chip("author", "Author")}
                        {org_chip("series", "Series")}
                    </div>
                }
                // Reactive inside: the slot renders once, and the role (and the
                // organize mode) resolve later.
                bulk=move || view! { {move || (is_admin_view() && organize.get() == "none").then(|| view! {
                    <BulkBar
                        sel=sel
                        visible=visible_ids
                        collection="books"
                        noun=crate::library_select::BOOKS
                        on_done=on_bulk_done
                    />
                })} }
            />
            {move || error.get().map(|e| view! { <p class="bad">"Load failed: " {e}</p> })}
            {move || books.get().is_empty().then(|| view! { <p class="muted">"No audiobooks yet — add one with + Add media."</p> })}
            {grid}
        </section>
    }
}

/// The audiobook wall's status chips (SKADI-T-0253).
pub const BOOK_CHIPS: &[(&str, &str)] = &[
    ("all", "All"),
    ("owned", "Owned"),
    ("wanted", "Wanted"),
    ("downloading", "Downloading"),
];

/// Library-wall status category for a book (SKADI-T-0253): owned / downloading / wanted.
pub fn audiobook_lib_status(b: &api::Book) -> &'static str {
    match book_status(b).1 {
        "imported" => "owned",
        "in progress" => "downloading",
        _ => "wanted",
    }
}

/// One 2:3 cover tile in the audiobook library wall: BOOK tag + status dot, title/
/// byline over a scrim, dashed "+" when wanted; opens the full-page detail.
fn cover_tile(b: api::Book) -> AnyView {
    cover_tile_in(b, None)
}

/// [`cover_tile`] on the flat wall, where select mode (SKADI-T-0696) makes a
/// click toggle the tile instead of opening it.
fn cover_tile_in(b: api::Book, sel: Option<Selection>) -> AnyView {
    let id = b.id.clone();
    // Open the full-page detail (the drawer was dropped).
    let nav = use_navigate();
    let on_open = move |_| match sel {
        Some(s) if s.mode.get_untracked() => s.toggle(&id),
        _ => nav(&format!("/audiobooks/{id}"), Default::default()),
    };
    let base = if b.monitored {
        "poster-tile book"
    } else {
        "poster-tile book unmonitored"
    };
    let class = tile_class(base, sel, b.id.clone());
    let check = tile_check(sel, b.id.clone());
    let status = audiobook_lib_status(&b);
    let dot = match status {
        "owned" => "owned",
        "downloading" => "dl",
        _ => "wanted",
    };
    let wanted = status == "wanted";
    let cover = b.cover_url.clone().filter(|u| !u.is_empty());
    let title = b.title.clone();
    let byline = b.authors.join(", ");

    view! {
        <div class=class on:click=on_open>
            <div class="poster-img book-cover">
                {match cover {
                    Some(src) => view! { <img src=src alt=title.clone() loading="lazy"/> }.into_any(),
                    None => view! { <div class="poster-ph">{title.clone()}</div> }.into_any(),
                }}
                {check}
                {(status != "owned")
                    .then(|| view! { <span class=format!("status-dot {dot}")></span> })}
                {wanted.then(|| view! { <div class="tile-wanted"><span>"+"</span></div> })}
            </div>
            <div class="tile-scrim">
                    <span class="tile-title">{title}</span>
                    <span class="tile-sub mono">{byline}</span>
            </div>
        </div>
    }
    .into_any()
}

/// One entry in an expanded series/author grid: an owned library book, or a catalog
/// work the user doesn't own (rendered muted). Merged and sorted by series position
/// so a series reads with its gaps in order, Sonarr-style (SKADI-T-0165/0353).
pub enum SeriesTile {
    Owned(api::Book),
    Missing(api::Work),
}

impl SeriesTile {
    /// Numeric series-position sort key; missing/unparseable sort last.
    pub fn pos_key(&self) -> f64 {
        match self {
            SeriesTile::Owned(b) => series_pos_key(b),
            SeriesTile::Missing(w) => work_pos_key(w),
        }
    }
    fn title_key(&self) -> String {
        match self {
            SeriesTile::Owned(b) => b.title.to_lowercase(),
            SeriesTile::Missing(w) => w.title.to_lowercase(),
        }
    }
}

/// Numeric sort key for a work's series position (mirror of [`series_pos_key`]).
fn work_pos_key(w: &api::Work) -> f64 {
    w.series_position
        .as_ref()
        .and_then(|p| p.trim().parse::<f64>().ok())
        .unwrap_or(f64::INFINITY)
}

/// Merge owned books and unowned catalog works into one position-ordered tile list,
/// dropping works already represented by an owned book (dedup by ASIN). This is what
/// makes an expanded series show `#1 (missing) · #2 (owned) · …` in reading order.
pub fn merge_owned_missing(owned: Vec<api::Book>, works: &[api::Work]) -> Vec<SeriesTile> {
    let have: HashSet<String> = owned
        .iter()
        .filter_map(|b| b.external_ids.asin.clone())
        .collect();
    let mut tiles: Vec<SeriesTile> = owned.into_iter().map(SeriesTile::Owned).collect();
    for w in works {
        if !w.owned && !have.contains(&w.asin) {
            tiles.push(SeriesTile::Missing(w.clone()));
        }
    }
    tiles.sort_by(|a, b| {
        a.pos_key()
            .partial_cmp(&b.pos_key())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title_key().cmp(&b.title_key()))
    });
    tiles
}

/// Whether a series position is a real one (SKADI-T-0654): non-blank after
/// trimming. `"0"` (a prequel) and `"1.5"` count; `None` and blank do not. Mirrors
/// the server's `has_position`.
#[must_use]
pub fn has_series_position(position: Option<&str>) -> bool {
    position.is_some_and(|p| !p.trim().is_empty())
}

/// Whether a library book has a position in its series.
#[must_use]
pub fn book_is_positioned(b: &api::Book) -> bool {
    has_series_position(b.series.as_ref().and_then(|s| s.position.as_deref()))
}

impl SeriesTile {
    /// Whether this member has a position in its series (SKADI-T-0654).
    #[must_use]
    pub fn is_positioned(&self) -> bool {
        match self {
            SeriesTile::Owned(b) => book_is_positioned(b),
            SeriesTile::Missing(w) => has_series_position(w.series_position.as_deref()),
        }
    }
}

/// Split a series' tiles into the numbered members, in order, and the
/// unpositioned ones listed under "Related" (SKADI-T-0654). Order within each
/// part is preserved.
#[must_use]
pub fn split_related_tiles(tiles: Vec<SeriesTile>) -> (Vec<SeriesTile>, Vec<SeriesTile>) {
    tiles.into_iter().partition(SeriesTile::is_positioned)
}

fn render_tile<F: Fn() + Copy + 'static>(t: SeriesTile, reload: F) -> AnyView {
    match t {
        SeriesTile::Owned(b) => cover_tile(b),
        SeriesTile::Missing(w) => missing_cover_tile(w, reload),
    }
}

/// A muted scrim tile for a **missing** (known-but-unowned) work, sized to sit inline
/// with owned covers in the same `.poster-grid` (SKADI-T-0353). Two one-click actions:
/// **Grab** (add + search/snatch now, `search=true`) and **+ Want** (add as Missing so
/// the hunter sweep fetches it later, `search=false`). `reload` refreshes the library
/// so the tile flips to an owned cover once acquired.
fn missing_cover_tile<F: Fn() + Copy + 'static>(w: api::Work, reload: F) -> AnyView {
    let cover = w.cover_url.clone().filter(|u| !u.is_empty());
    let title = w.title.clone();
    let byline = w.authors.join(", ");
    let pos = w
        .series_position
        .clone()
        .filter(|p| !p.is_empty())
        .map(|p| format!("#{p} "))
        .unwrap_or_default();
    let asin_g = w.asin.clone();
    let asin_w = w.asin.clone();
    let on_grab = move |_| {
        let asin = asin_g.clone();
        spawn_local(async move {
            let _ = api::add_book(&asin, None, true).await;
            reload();
        });
    };
    let on_want = move |_| {
        let asin = asin_w.clone();
        spawn_local(async move {
            let _ = api::add_book(&asin, None, false).await;
            reload();
        });
    };

    view! {
        <div class="poster-tile book missing" title="Not in your library">
            <div class="poster-img book-cover">
                {match cover {
                    Some(src) => view! { <img src=src alt=title.clone() loading="lazy"/> }.into_any(),
                    None => view! { <div class="poster-ph">{title.clone()}</div> }.into_any(),
                }}
                <span class="status-dot wanted"></span>
            </div>
            <div class="tile-scrim">
                    <span class="tile-title">{format!("{pos}{title}")}</span>
                    <span class="tile-sub mono">{byline}</span>
                    <div class="missing-actions">
                        <button type="button" class="mini-btn" on:click=on_grab title="Search and download now">"Grab"</button>
                        <button type="button" class="mini-btn ghost" on:click=on_want title="Add to wanted — the hunter fetches it on the next sweep">"+ Want"</button>
                    </div>
            </div>
        </div>
    }
    .into_any()
}

/// One row in an author's body-of-work browse (SKADI-T-0159). Owned works link to
/// their book; unowned works are "missing" with a per-title **Watch** (book scope)
/// and an immediate **Add**; works already covered by a watcher show "Watching".
/// `reload` re-fetches the page after an action so state reflects immediately.
fn work_row<F: Fn() + Copy + 'static>(w: api::Work, reload: F) -> AnyView {
    let pos = w.series_position.clone().filter(|p| !p.is_empty());
    let cover = w.cover_url.clone().filter(|u| !u.is_empty());
    let title = w.title.clone();
    let year = w
        .release_date
        .as_deref()
        .and_then(|d| d.get(0..4))
        .map(|y| y.to_string())
        .unwrap_or_default();
    let series = w.series_name.clone().filter(|s| !s.is_empty());

    let thumb = match cover {
        Some(src) => {
            view! { <img class="work-cover" src=src alt=title.clone() loading="lazy"/> }.into_any()
        }
        None => view! { <div class="work-cover poster-ph">{title.clone()}</div> }.into_any(),
    };

    let action = if let Some(bid) = w.book_id.clone() {
        view! { <A href=format!("/audiobooks/{bid}") attr:class="badge ok">"Owned ✓"</A> }
            .into_any()
    } else if w.watched {
        view! { <span class="badge">"Watching"</span> }.into_any()
    } else {
        let asin_w = w.asin.clone();
        let on_watch = move |_| {
            let asin = asin_w.clone();
            spawn_local(async move {
                let _ = api::set_watcher("book", &asin).await;
                reload();
            });
        };
        let asin_a = w.asin.clone();
        let on_add = move |_| {
            let asin = asin_a.clone();
            spawn_local(async move {
                let _ = api::add_book(&asin, None, true).await;
                reload();
            });
        };
        view! {
            <div class="work-actions">
                <button type="button" class="watch-btn" on:click=on_watch>"Watch"</button>
                <button type="button" on:click=on_add>"Add"</button>
            </div>
        }
        .into_any()
    };

    let pos_label = pos.map(|p| view! { <span class="badge muted">{format!("#{p}")}</span> });
    let sub = series
        .map(|s| {
            if year.is_empty() {
                s
            } else {
                format!("{s} · {year}")
            }
        })
        .unwrap_or(year);

    view! {
        <div class="work-row">
            {thumb}
            <div class="work-meta">
                <span class="work-title">{pos_label}{title}</span>
                <span class="muted">{sub}</span>
            </div>
            {action}
        </div>
    }
    .into_any()
}

/// Discover page (SKADI-T-0317): library-driven recommendations — unowned catalog works by the
/// authors/series already in your library, ranked upcoming → recently-released. Reuses
/// [`work_row`] tiles (Watch / Add), the "what to listen to next" surface.
#[component]
pub fn AudiobookDiscoverPage() -> impl IntoView {
    let works = RwSignal::new(Vec::<api::Work>::new());
    let loaded = RwSignal::new(false);
    let reload = move || {
        spawn_local(async move {
            if let Ok(w) = api::discover_audiobooks(120).await {
                works.set(w);
            }
            loaded.set(true);
        });
    };
    Effect::new(move |_| reload());

    view! {
        <div class="page">
            <div class="lib-head">
                <h1>"Discover"</h1>
                <A href="/audiobooks" attr:class="btn-link">"← Library"</A>
            </div>
            <p class="muted">
                "New and unowned titles from the authors and series already in your library — "
                "soonest releases first. Add one to grab it, or Watch its author/series to keep up."
            </p>
            {move || {
                if !loaded.get() {
                    return view! { <p class="muted">"Loading…"</p> }.into_any();
                }
                let all = works.get();
                if all.is_empty() {
                    return view! {
                        <p class="muted">
                            "Nothing to discover yet. Add a book or follow an author/series and "
                            "skadi will surface their other titles here as it catalogues them."
                        </p>
                    }
                    .into_any();
                }
                view! {
                    <div class="work-list">
                        {all.into_iter().map(|w| work_row(w, reload)).collect_view()}
                    </div>
                }
                .into_any()
            }}
        </div>
    }
}

/// Book detail page (SKADI-T-0133): cover + title/subtitle/byline/series +
/// overview, the per-file status badge, and the acquire / monitor / delete
/// actions. Mirrors [`crate::movies::MovieDetailPage`]; polls every 3s so status
/// + download progress stay live.
#[component]
pub fn BookDetailPage() -> impl IntoView {
    let params = use_params_map();
    let book = RwSignal::new(None::<api::Book>);
    let acquire_status = RwSignal::new(HashMap::<String, Result<String, String>>::new());
    // Per-file on-disk location (folder + size/count), fetched lazily for imported
    // files and cached by file id (SKADI-T-0147).
    let locations = RwSignal::new(HashMap::<String, api::BookFileLocation>::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let alive = RwSignal::new(true);
    let load = move || {
        let id = params.read().get("id").unwrap_or_default();
        if id.is_empty() {
            return;
        }
        spawn_local(async move {
            match api::get_book(&id).await {
                Ok(b) => {
                    book.set(Some(b));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };

    Effect::new(move |_| {
        spawn_local(async move {
            // Only re-render on a changed payload (SKADI-T-0596).
            let mut last: Option<String> = None;
            loop {
                let id = params.get_untracked().get("id").unwrap_or_default();
                if !id.is_empty() {
                    // try_set + break: the fetch straddles navigation — a plain
                    // set on the disposed page panics the wasm app and killed
                    // the player mid-download (SKADI-T-0331 verification).
                    match api::get_book_if_changed(&id, &mut last).await {
                        Ok(Some(b)) => {
                            if book.try_set(Some(b)).is_some() {
                                break;
                            }
                            let _ = error.try_set(None);
                        }
                        Ok(None) => {
                            let _ = error.try_set(None);
                        }
                        Err(e) => {
                            if error.try_set(Some(e.to_string())).is_some() {
                                break;
                            }
                        }
                    }
                }
                gloo_timers::future::TimeoutFuture::new(3000).await;
                if !alive.try_get_untracked().unwrap_or(false) {
                    break;
                }
            }
        });
    });
    on_cleanup(move || alive.set(false));

    // When the book (re)loads, fetch the on-disk location for each imported file
    // that we haven't cached yet.
    Effect::new(move |_| {
        let Some(b) = book.get() else {
            return;
        };
        let book_id = b.id.clone();
        for f in &b.files {
            let label = api::status_label(&f.status);
            if label != "imported" && label != "cutoff" {
                continue;
            }
            let fid = f.id.clone();
            if locations.with_untracked(|m| m.contains_key(&fid)) {
                continue;
            }
            let book_id = book_id.clone();
            spawn_local(async move {
                if let Ok(Some(loc)) = api::book_file_location(&book_id, &fid).await {
                    // try_update: this NFS-backed stat can land AFTER the user
                    // navigated away (e.g. straight into the player) — writing
                    // a disposed signal panics the whole wasm app (found by the
                    // SKADI-T-0331 player verification).
                    let _ = locations.try_update(|m| {
                        m.insert(fid, loc);
                    });
                }
            });
        }
    });

    let reload = Callback::new(move |_: ()| load());
    let nav = use_navigate();

    let body = move || {
        let Some(b) = book.get() else {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        };
        let year = b.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let monitored = b.monitored;
        let overview = b.overview.clone().filter(|o| !o.is_empty());
        let cover = b.cover_url.clone().filter(|u| !u.is_empty());
        let subtitle = b.subtitle.clone().filter(|s| !s.is_empty());
        let byline = book_byline(&b);
        let series = b.series.as_ref().map(series_label);

        let mon_id = b.id.clone();
        let toggle_monitor = move |_| {
            let id = mon_id.clone();
            busy.set(true);
            spawn_local(async move {
                let _ = api::set_book_monitored(&id, !monitored).await;
                busy.set(false);
                load();
            });
        };
        let del_id = b.id.clone();
        let del_title = b.title.clone();
        let nav_del = nav.clone();
        let on_delete = move |_| {
            let body =
                format!("Delete \"{del_title}\" and its files from disk? This can't be undone.");
            let id = del_id.clone();
            let nav = nav_del.clone();
            spawn_local(async move {
                if !confirm(ConfirmSpec::destructive("Delete the book?", body)).await {
                    return;
                }
                busy.set(true);
                let _ = api::delete_book(&id, true).await;
                busy.set(false);
                nav("/audiobooks", Default::default());
            });
        };

        let file_rows = b
            .files
            .iter()
            .map(|f| {
                let label = api::status_label(&f.status);
                let cls = status_class(&label);
                let progress = api::download_progress(&f.status)
                    .map(|p| format!(" {:.0}%", (p * 100.0).clamp(0.0, 100.0)));
                let book_id = b.id.clone();
                let file_id = f.id.clone();

                let acq_bid = book_id.clone();
                let acq_fid = file_id.clone();
                let on_acquire = Callback::new(move |()| {
                    let book_id = acq_bid.clone();
                    let file_id = acq_fid.clone();
                    acquire_status.update(|x| {
                        x.insert(file_id.clone(), Ok("…".into()));
                    });
                    spawn_local(async move {
                        let r = api::acquire_book_file(&book_id, &file_id).await;
                        acquire_status.update(|x| {
                            x.insert(
                                file_id.clone(),
                                r.map(|()| "acquire started".into())
                                    .map_err(|e| e.to_string()),
                            );
                        });
                    });
                });

                let rst_bid = book_id.clone();
                let rst_fid = file_id.clone();
                let resettable = file_resettable(&label);
                let on_reset = Callback::new(move |()| {
                    let book_id = rst_bid.clone();
                    let file_id = rst_fid.clone();
                    acquire_status.update(|x| {
                        x.insert(file_id.clone(), Ok("resetting…".into()));
                    });
                    spawn_local(async move {
                        let r = api::reset_book_file(&book_id, &file_id).await;
                        acquire_status.update(|x| {
                            x.insert(
                                file_id.clone(),
                                r.map(|()| "reset to Missing".into())
                                    .map_err(|e| e.to_string()),
                            );
                        });
                        reload.run(());
                    });
                });

                let fid_for_view = f.id.clone();
                let acq_view = move || {
                    acquire_status.with(|x| match x.get(&fid_for_view) {
                        Some(Ok(msg)) => view! { <span class="ok">{msg.clone()}</span> }.into_any(),
                        Some(Err(msg)) => {
                            view! { <span class="bad">{msg.clone()}</span> }.into_any()
                        }
                        None => ().into_any(),
                    })
                };

                // Where this imported file lives on disk (folder + size/count),
                // so people can actually get to the book (SKADI-T-0147).
                let fid_loc = f.id.clone();
                let label_loc = label.clone();
                // Save the file itself (SKADI-T-0635). Reactive on `locations`
                // because that is the only place the container comes from:
                // audiobooks are not probed, so `media_info` is null and the
                // library scan never fills it in. Until the location loads the
                // link still works, just without an extension — which is the
                // honest fallback rather than guessing `.m4b` onto an `.mp3`.
                let save_bid = book_id.clone();
                let save_fid = file_id.clone();
                let save_label = label.clone();
                let save_title = b.title.clone();
                let save_author = b.authors.first().cloned().unwrap_or_default();
                let save_view = move || {
                    if save_label != "imported" && save_label != "cutoff" {
                        return ().into_any();
                    }
                    let ext = locations.with(|m| m.get(&save_fid).map(|l| l.format.clone()));
                    let stem = if save_author.is_empty() {
                        save_title.clone()
                    } else {
                        format!("{save_author} - {save_title}")
                    };
                    let name = api::download_filename(&stem, ext.as_deref());
                    let href = api::book_audio_download_url(&save_bid, &save_fid);
                    view! {
                        <a href=href download=name class="btn-link"
                           title="Save this audiobook to your computer">"⇩ Save"</a>
                    }
                    .into_any()
                };

                let location_view = move || {
                    if label_loc != "imported" && label_loc != "cutoff" {
                        return ().into_any();
                    }
                    match locations.with(|m| m.get(&fid_loc).cloned()) {
                        Some(loc) => {
                            let meta = format!(
                                "{} · {} file{} · {}",
                                loc.format,
                                loc.file_count,
                                if loc.file_count == 1 { "" } else { "s" },
                                crate::movies::size_human(loc.total_bytes),
                            );
                            let folder = loc.folder.clone();
                            let folder_copy = folder.clone();
                            let imported = loc
                                .imported_at
                                .as_deref()
                                .and_then(|s| s.get(0..10))
                                .unwrap_or("")
                                .to_string();
                            view! {
                                <div class="file-location">
                                    <div class="muted file-meta">{meta}</div>
                                    <div class="path-row">
                                        <input class="path-field" readonly prop:value=folder.clone()/>
                                        <button
                                            class="copy-btn"
                                            on:click=move |_| copy_to_clipboard(&folder_copy)
                                        >"Copy"</button>
                                    </div>
                                    {(!imported.is_empty())
                                        .then(|| view! { <div class="muted">"imported "{imported}</div> })}
                                </div>
                            }
                            .into_any()
                        }
                        None => ().into_any(),
                    }
                };

                view! {
                    <div class="edition-block">
                        <div class="edition-row">
                            <span class="muted">"audiobook"</span>
                            <span class=cls>{label.clone()}{progress}</span>
                            {(label == "imported" || label == "cutoff").then(|| {
                                // Offline player entry point (SKADI-T-0331).
                                let href = format!("/listen/{book_id}/{file_id}");
                                view! { <A href=href attr:class="btn-link listen-link">"▶ Listen"</A> }
                            })}
                            {save_view}
                            <BookFileActions resettable=resettable on_acquire=on_acquire on_reset=on_reset/>
                            {acq_view}
                        </div>
                        {location_view}
                        <ReleasesPanel
                            at=api::AcquirablePath::book_file(&book_id, &file_id)
                            acquirable=file_id.clone()
                            reload=reload
                        />
                        <HistoryPanel acquirable=file_id.clone()/>
                        {(!(label == "imported" || label == "cutoff"))
                            .then(|| view! { <DiagnosticsPanel acquirable=file_id.clone()/> })}
                    </div>
                }
            })
            .collect_view();

        view! {
            <div class="detail no-hero book">
                <div class="detail-body">
                    {match cover {
                        Some(src) => view! { <img class="detail-poster" src=src alt=b.title.clone()/> }.into_any(),
                        None => view! { <div class="detail-poster poster-ph">{b.title.clone()}</div> }.into_any(),
                    }}
                    <div class="detail-info">
                        <h2>{b.title.clone()}<span class="muted">{year}</span></h2>
                        {subtitle.map(|s| view! { <p class="muted">{s}</p> })}
                        {(!byline.is_empty()).then(|| view! { <p class="muted">{byline}</p> })}
                        {series.map(|s| view! { <p><span class="badge">{s}</span></p> })}
                        <div class="card-actions">
                            <button on:click=toggle_monitor disabled=move || busy.get()>
                                {if monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button class="danger" on:click=on_delete disabled=move || busy.get()>"Delete"</button>
                        </div>
                        {overview.map(|o| clean_overview(&o)
                            .into_iter()
                            .map(|p| view! { <p class="overview">{p}</p> })
                            .collect_view())}
                        <div class="card-editions">{file_rows}</div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    view! {
        <div class="page-head">
            <A href="/audiobooks" attr:class="btn-link">"← Library"</A>
        </div>
        {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
        {body}
    }
}

/// Whether a book file in `label` state can be **Reset** (unwedged) — the
/// non-terminal/failed acquire states. Pure, so it's unit-testable. Mirrors
/// [`crate::movies::edition_resettable`].
pub fn file_resettable(label: &str) -> bool {
    matches!(label, "searching" | "snatched" | "downloading" | "failed")
}

/// Presentational per-file action bar: **Acquire** + **Reset** (disabled unless
/// [`file_resettable`]). Actions delegated via callbacks so it's unit-mountable
/// with no state. Mirrors [`crate::movies::EditionActions`].
#[component]
pub fn BookFileActions(
    resettable: bool,
    on_acquire: Callback<()>,
    on_reset: Callback<()>,
) -> impl IntoView {
    view! {
        <button on:click=move |_| on_acquire.run(())>"Acquire"</button>
        <button class="secondary" on:click=move |_| on_reset.run(()) disabled=!resettable>
            "Reset"
        </button>
    }
}

/// Author detail page (SKADI-T-0133): author info + their books + a monitor
/// toggle. Books come from `GET /books?author=<id>`.
#[component]
pub fn AuthorDetailPage() -> impl IntoView {
    let params = use_params_map();
    let author = RwSignal::new(None::<api::Author>);
    let books = RwSignal::new(Vec::<api::Book>::new());
    // The author's full body of work (owned + unowned) from the known-works store.
    let works = RwSignal::new(Vec::<api::Work>::new());
    // Whether an author-scope watcher is active (acquire the whole catalog).
    let author_watched = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let role_signal = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    let is_admin_view = move || role_signal.and_then(|r| r.get()).as_deref() == Some("admin");

    let load = move || {
        let id = params.read().get("id").unwrap_or_default();
        if id.is_empty() {
            return;
        }
        // Read in the effect body, not the async block — see the library page
        // above. `/watchers` is admin-only, so gate on a *known* admin: an
        // unresolved role must fetch nothing (SKADI-T-0639).
        let is_admin = role_signal.and_then(|r| r.get()).as_deref() == Some("admin");
        spawn_local(async move {
            let mut author_asin = None;
            match api::get_author(&id).await {
                Ok(a) => {
                    author_asin = a.asin.clone();
                    author.set(Some(a));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
            if let Ok(b) = api::list_books(None, Some(&id)).await {
                books.set(b);
            }
            // Body of work + author watch state both key off the author's ASIN.
            if let Some(asin) = author_asin {
                if let Ok(w) = api::list_works_by_author(&asin).await {
                    works.set(w);
                }
                let watched = if is_admin {
                    api::list_watchers()
                        .await
                        .map(|ws| ws.iter().any(|w| w.scope == "author" && w.key == asin))
                        .unwrap_or(false)
                } else {
                    false
                };
                author_watched.set(watched);
            } else {
                works.set(Vec::new());
                author_watched.set(false);
            }
        });
    };
    Effect::new(move |_| load());

    let nav = use_navigate();

    let body = move || {
        let Some(a) = author.get() else {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        };
        let monitored = a.monitored;
        let description = a.description.clone().filter(|d| !d.is_empty());
        let image = a.image_url.clone().filter(|u| !u.is_empty());
        // Read here rather than in a closure further down: this whole body is a
        // reactive closure, so reading the role at this level re-renders it when
        // `/me` answers, and the handlers below (created fresh on each run) can
        // then be consumed by a plain `if`.
        let show_actions = is_admin_view();

        // Watch-author toggle (author-scope watcher): acquire the whole catalog's
        // unowned works. Only when we know the author's ASIN.
        let watch_btn = a.asin.clone().map(|asin| {
            let watched = author_watched.get();
            let cls = if watched { "watch-btn on" } else { "watch-btn" };
            let txt = if watched {
                "Watching author"
            } else {
                "Watch author"
            };
            let toggle_watch = move |_| {
                let asin = asin.clone();
                busy.set(true);
                spawn_local(async move {
                    let _ = if watched {
                        api::clear_watcher("author", &asin).await
                    } else {
                        api::set_watcher("author", &asin).await
                    };
                    busy.set(false);
                    load();
                });
            };
            view! {
                <button type="button" class=cls on:click=toggle_watch disabled=move || busy.get()>
                    {txt}
                </button>
            }
        });

        let mon_id = a.id.clone();
        let toggle_monitor = move |_| {
            let id = mon_id.clone();
            busy.set(true);
            spawn_local(async move {
                let _ = api::set_author_monitored(&id, !monitored).await;
                busy.set(false);
                load();
            });
        };
        let del_id = a.id.clone();
        let del_name = a.name.clone();
        let nav_del = nav.clone();
        let on_delete = move |_| {
            let body = format!("Delete \"{del_name}\"? (library entry only — no files removed)");
            let id = del_id.clone();
            let nav = nav_del.clone();
            spawn_local(async move {
                if !confirm(ConfirmSpec::destructive("Delete the author?", body)).await {
                    return;
                }
                busy.set(true);
                let _ = api::delete_author(&id).await;
                busy.set(false);
                nav("/audiobooks", Default::default());
            });
        };

        view! {
            <div class="detail">
                <div class="detail-body">
                    {match image {
                        Some(src) => view! { <img class="detail-poster" src=src alt=a.name.clone()/> }.into_any(),
                        None => view! { <div class="detail-poster poster-ph">{a.name.clone()}</div> }.into_any(),
                    }}
                    <div class="detail-info">
                        <h2>{a.name.clone()}</h2>
                        // Watch, Monitor and Delete are all writes to
                        // `/authors/{id}` or `/watchers`, which no non-admin may
                        // make. They were rendered for every role, so a
                        // read-only member was shown a **Delete** button that
                        // silently 403'd on click (SKADI-T-0639). The sweep
                        // records 403s from page loads, not clicks, so it could
                        // not have caught these.
                        {show_actions.then(|| view! {
                            <div class="card-actions">
                                {watch_btn}
                                <button on:click=toggle_monitor disabled=move || busy.get()>
                                    {if monitored { "Unmonitor" } else { "Monitor" }}
                                </button>
                                <button class="danger" on:click=on_delete disabled=move || busy.get()>"Delete"</button>
                            </div>
                        })}
                        {description.map(|d| clean_overview(&d)
                            .into_iter()
                            .map(|p| view! { <p class="overview">{p}</p> })
                            .collect_view())}
                    </div>
                </div>
                <section class="provider-section">
                    <div class="section-head">
                        <div>
                            <h3>"Body of work"</h3>
                            <p class="muted">
                                "Every known title from this author's catalog — owned + missing.
                                 Watch the author above to acquire them all, or watch/add titles one
                                 at a time below."
                            </p>
                        </div>
                    </div>
                    {move || (works.get().is_empty() && !books.get().is_empty()).then(|| view! {
                        <p class="muted">"Cataloguing this author's works… check back shortly."</p>
                    })}
                    {move || (works.get().is_empty() && books.get().is_empty()).then(|| view! {
                        <p class="muted">"No works catalogued for this author yet."</p>
                    })}
                    <div class="work-list">
                        {move || works.get().into_iter().map(|w| work_row(w, load)).collect_view()}
                    </div>
                </section>
            </div>
        }
        .into_any()
    };

    view! {
        <div class="page-head">
            <h2>"Author"</h2>
            <A href="/audiobooks" attr:class="gear" attr:title="Back to Audiobooks">"←"</A>
        </div>
        {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
        {body}
    }
}

/// The built-in audiobook quality ladder, best first (split out of
/// [`AudiobooksConfigPage`] so a DOM test can render it with rows,
/// SKADI-T-0697). Empty = still loading. Each cell carries a `data-label`, which
/// the narrow-screen card layout prints beside the value.
#[component]
pub fn QualityLadderTable(tiers: Vec<api::AudiobookQuality>) -> impl IntoView {
    let rows = if tiers.is_empty() {
        view! { <tr><td colspan="4" class="muted">"Loading…"</td></tr> }.into_any()
    } else {
        let last = tiers.len().saturating_sub(1);
        tiers
            .into_iter()
            .enumerate()
            .map(|(i, row)| {
                let tag = if i == 0 {
                    Some(view! { <span class="badge ok">"best"</span> })
                } else if i == last {
                    Some(view! { <span class="badge muted">"fallback"</span> })
                } else {
                    None
                };
                let bitrate = if row.kbps == 0 {
                    "VBR".to_string()
                } else {
                    format!("{} kbps", row.kbps)
                };
                view! {
                    <tr>
                        <td data-label="Tier"><strong>{row.name}</strong></td>
                        <td class="muted" data-label="Format">{row.format}</td>
                        <td class="muted" data-label="Bitrate">{bitrate}</td>
                        <td data-label="Rank">{tag}</td>
                    </tr>
                }
            })
            .collect_view()
            .into_any()
    };
    view! { <table class="history-table"><tbody>{rows}</tbody></table> }
}

/// Audiobooks-domain configuration (SKADI-T-0142): audiobooks rank by a built-in,
/// format-first quality ladder (every M4B beats every MP3) rather than a tunable
/// movie-shaped profile, so this page *displays* that ladder read-only plus the
/// registered root folders, and only nags about enabling the domain when it's
/// actually off.
#[component]
pub fn AudiobooksConfigPage() -> impl IntoView {
    let ladder = RwSignal::new(Vec::<api::AudiobookQuality>::new());
    // `None` until `/domains` loads — so we don't flash the "disabled" banner.
    let enabled = RwSignal::new(None::<bool>);

    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(q) = api::audiobook_quality().await {
                ladder.set(q);
            }
        });
        spawn_local(async move {
            if let Ok(ds) = api::list_domains().await {
                enabled.set(Some(ds.iter().any(|d| d.kind == "audiobook" && d.enabled)));
            }
        });
    });

    view! {
        <crate::subnav::SubNav/>
        <div class="page-head">
            <h2>"Audiobooks · Config"</h2>
            <A href="/audiobooks">"← Library"</A>
        </div>

        {move || (enabled.get() == Some(false)).then(|| view! {
            <p class="bad">
                "The audiobooks domain is disabled. Enable it under "
                <A href="/config">"Config → Domains"</A>" before acquiring."
            </p>
        })}

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Quality ladder"</h3>
                    <p class="muted">
                        "Audiobooks rank by a built-in, format-first ladder — every M4B beats every
                         MP3. It's fixed (no per-profile tuning): the highest available tier wins,
                         down to the fallback."
                    </p>
                </div>
            </div>
            {move || view! { <QualityLadderTable tiers=ladder.get()/> }}
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Library"</h3>
                    <p class="muted">
                        "Books are hardlinked into the audiobook/ subfolder of the single library
                         root. See "<A href="/config">"Config → Library root"</A>"."
                    </p>
                </div>
            </div>
        </section>
    }
}

/// Best-effort "copy this text to the clipboard" via the async Clipboard API.
/// Fire-and-forget — the returned promise is ignored (no UI dependency on it).
fn copy_to_clipboard(text: &str) {
    if let Some(win) = web_sys::window() {
        let _ = win.navigator().clipboard().write_text(text);
    }
}
