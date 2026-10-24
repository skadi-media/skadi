//! New imports on the web (SKADI-T-0745, initiative COLLIERY-I-0653): what
//! media is new, as a widget on the Overview and a full page at `/new`.
//!
//! Both read `GET /new-imports` (SKADI-T-0744), which filters each row by the
//! member's policy on the server, so a kid's widget and a parent's differ with
//! no client logic. The count is the server's too (`/new-imports/count`, the
//! last 7 days for this member).
//!
//! **Live.** Both views refresh every [`NEW_IMPORTS_POLL_MS`] while the tab is
//! visible and not at all while it is hidden (the shared live poll). Imports
//! land minutes apart at most, so half a minute reads as live without the 2 s
//! cadence of Downloads.

use aurora_leptos::components::{Button, Empty, FeedList, FeedRow, PageHeader, Pill, Select};
use aurora_leptos::tokens::token;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

use crate::api;

/// Rows on the Overview widget.
pub const WIDGET_ROWS: u32 = 15;
/// Rows per page on `/new`.
pub const PAGE_ROWS: u32 = 30;
/// Refresh cadence while the tab is visible (ms).
pub const NEW_IMPORTS_POLL_MS: u32 = 30_000;

/// The season of an `S01E05` label. Pure.
#[must_use]
pub fn season_of(episode: &str) -> Option<u16> {
    let rest = episode.strip_prefix('S')?;
    let end = rest.find('E')?;
    rest[..end].parse().ok()
}

/// The item page a row opens. An episode opens its series at its season.
/// Pure.
#[must_use]
pub fn item_href(n: &api::NewImport) -> String {
    match n.kind.as_str() {
        "series" => match n.episode.as_deref().and_then(season_of) {
            Some(s) => format!("/tv/{}?season={s}", n.item_id),
            None => format!("/tv/{}", n.item_id),
        },
        "audiobook" => format!("/audiobooks/{}", n.item_id),
        _ => format!("/movies/{}", n.item_id),
    }
}

/// The line under the title: `S01E05 · Episode title` for an episode, else
/// the year. Pure.
#[must_use]
pub fn subtitle(n: &api::NewImport) -> String {
    match (&n.episode, &n.episode_title) {
        (Some(e), Some(t)) if !t.is_empty() => format!("{e} · {t}"),
        (Some(e), _) => e.clone(),
        _ => n.year.map(|y| y.to_string()).unwrap_or_default(),
    }
}

/// The media tag and its colour, as the Overview's other rows use them.
#[must_use]
pub fn kind_tag(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "series" => ("TV", token::GOLD),
        "audiobook" => ("BOOK", token::TEAL),
        _ => ("FILM", token::ICE),
    }
}

/// `"12 new this week"`, or `"Nothing new this week"`. Pure.
#[must_use]
pub fn count_label(count: usize) -> String {
    match count {
        0 => "Nothing new this week".into(),
        n => format!("{n} new this week"),
    }
}

/// Put the rows of a refreshed first page in front of what is on screen,
/// newest first, without a row twice. A refresh never drops the rows already
/// scrolled in. Pure.
#[must_use]
pub fn merge_newer(shown: &[api::NewImport], fresh: Vec<api::NewImport>) -> Vec<api::NewImport> {
    let mut out: Vec<api::NewImport> = fresh
        .into_iter()
        .filter(|f| !shown.iter().any(|s| s.id == f.id))
        .collect();
    out.extend(shown.iter().cloned());
    out
}

/// The `kind` query value of the filter, `None` for all. Pure.
#[must_use]
pub fn kind_param(filter: &str) -> Option<&str> {
    match filter {
        "movie" | "series" | "audiobook" => Some(filter),
        _ => None,
    }
}

/// One row: poster, title and subtitle, the media tag, an Upgrade pill, and
/// when it landed. The whole row links to the item page.
fn import_row(n: api::NewImport) -> impl IntoView {
    let href = item_href(&n);
    let sub = subtitle(&n);
    let (tag, tag_color) = kind_tag(&n.kind);
    let at = js_sys::Date::parse(&n.at);
    let at = (!at.is_nan()).then_some(at);
    let poster = n
        .poster_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(|u| crate::movies::tmdb_img(u, "w92"));
    let quality = n.quality.clone().unwrap_or_default();
    let upgrade = n.upgrade;
    view! {
        <FeedRow href=href at=at.unwrap_or_default() dot=token::OK>
            <span class="ni-row">
                {match poster {
                    Some(src) => view! { <img class="ni-poster" src=src alt="" loading="lazy"/> }.into_any(),
                    None => view! { <span class="ni-poster ni-poster-empty" aria-hidden="true"></span> }.into_any(),
                }}
                <span class="ni-text">
                    <span class="ni-title">{n.title}</span>
                    <span class="ni-sub muted">{sub}</span>
                </span>
                <span class="ni-tags">
                    <Pill color=tag_color>{tag}</Pill>
                    {upgrade.then(|| view! { <Pill color=token::VIOLET>"Upgrade"</Pill> })}
                    <span class="ni-quality mono">{quality}</span>
                </span>
            </span>
        </FeedRow>
    }
}

/// The Overview widget: the latest [`WIDGET_ROWS`] new imports, this week's
/// count, and a link to `/new`.
#[component]
pub fn NewImportsWidget() -> impl IntoView {
    let rows = RwSignal::new(Vec::<api::NewImport>::new());
    let count = RwSignal::new(None::<usize>);
    let loaded = RwSignal::new(false);
    let alive = RwSignal::new(true);
    on_cleanup(move || alive.set(false));

    spawn_local(crate::live_poll::poll_every_while_visible(
        NEW_IMPORTS_POLL_MS,
        move || alive.try_get_untracked().unwrap_or(false),
        move || async move {
            if let Ok(page) = api::new_imports(None, None, WIDGET_ROWS).await {
                rows.try_set(page.items);
            }
            if let Ok(n) = api::new_imports_count().await {
                count.try_set(Some(n));
            }
            loaded.try_set(true);
        },
    ));

    let body = move || {
        let rs = rows.get();
        if rs.is_empty() {
            let message = if loaded.get() {
                "Nothing new yet."
            } else {
                "Loading…"
            };
            return view! { <Empty message=message/> }.into_any();
        }
        let items = rs.into_iter().map(import_row).collect_view();
        view! { <FeedList label="New imports">{items}</FeedList> }.into_any()
    };

    view! {
        <div class="ov-sec-head">
            <h3>"New imports"</h3>
            <span class="ni-count mono">{move || count.get().map(count_label).unwrap_or_default()}</span>
            <A href="/new" attr:class="ov-sec-link mono">"See all"</A>
        </div>
        {body}
    }
}

/// `/new`: every new import the member may see, newest first, filtered by
/// media type, loading the next page as the end of the list scrolls into view.
#[component]
pub fn NewImportsPage() -> impl IntoView {
    let filter = RwSignal::new(String::from("all"));
    let rows = RwSignal::new(Vec::<api::NewImport>::new());
    let cursor = RwSignal::new(None::<String>);
    // `true` once the server says there is no next page.
    let done = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let count = RwSignal::new(None::<usize>);
    let alive = RwSignal::new(true);
    on_cleanup(move || alive.set(false));

    // The next page after `cursor`. A filter change resets the list first.
    let load_more = move || {
        if busy.get_untracked() || done.get_untracked() {
            return;
        }
        busy.set(true);
        let kind = filter.get_untracked();
        let after = cursor.get_untracked();
        spawn_local(async move {
            match api::new_imports(kind_param(&kind), after.as_deref(), PAGE_ROWS).await {
                Ok(page) => {
                    // A filter changed while this was in flight: drop it.
                    if filter.try_get_untracked().as_deref() == Some(kind.as_str()) {
                        rows.try_update(|r| r.extend(page.items));
                        done.try_set(page.next_cursor.is_none());
                        cursor.try_set(page.next_cursor);
                        error.try_set(None);
                    }
                }
                Err(e) => {
                    error.try_set(Some(e.to_string()));
                }
            }
            busy.try_set(false);
        });
    };

    // First page, and again on each filter change.
    Effect::new(move |_| {
        let _ = filter.get();
        rows.set(Vec::new());
        cursor.set(None);
        done.set(false);
        busy.set(false);
        load_more();
    });

    // Live: new rows go on top; what is scrolled in stays.
    spawn_local(crate::live_poll::poll_every_while_visible(
        NEW_IMPORTS_POLL_MS,
        move || alive.try_get_untracked().unwrap_or(false),
        move || async move {
            if let Ok(n) = api::new_imports_count().await {
                count.try_set(Some(n));
            }
            let Some(kind) = filter.try_get_untracked() else {
                return;
            };
            if rows.try_with_untracked(Vec::is_empty).unwrap_or(true) {
                return;
            }
            if let Ok(page) = api::new_imports(kind_param(&kind), None, PAGE_ROWS).await
                && filter.try_get_untracked().as_deref() == Some(kind.as_str())
            {
                rows.try_update(|r| *r = merge_newer(r, page.items));
            }
        },
    ));

    // Infinite scroll: a sentinel after the list loads the next page when it
    // comes into view. The button under it does the same for a keyboard.
    let sentinel = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        let Some(el) = sentinel.get() else { return };
        let cb = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
            let seen = entries.iter().any(|e| {
                e.dyn_into::<web_sys::IntersectionObserverEntry>()
                    .is_ok_and(|e| e.is_intersecting())
            });
            if seen {
                load_more();
            }
        });
        if let Ok(obs) = web_sys::IntersectionObserver::new(cb.as_ref().unchecked_ref()) {
            obs.observe(&el);
            on_cleanup(move || obs.disconnect());
        }
        cb.forget();
    });

    let kinds = vec![
        ("all".to_string(), "All media".to_string()),
        ("movie".to_string(), "Movies".to_string()),
        ("series".to_string(), "TV".to_string()),
        ("audiobook".to_string(), "Audiobooks".to_string()),
    ];

    let list = move || {
        let rs = rows.get();
        if rs.is_empty() {
            if done.get() {
                return view! { <Empty message="Nothing new here."/> }.into_any();
            }
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        }
        let items = rs.into_iter().map(import_row).collect_view();
        view! { <FeedList label="New imports">{items}</FeedList> }.into_any()
    };

    view! {
        <PageHeader title="New imports" sub="newest first"
            meta=Box::new(move || view! {
                <span class="ni-count mono">{move || count.get().map(count_label).unwrap_or_default()}</span>
            }.into_any())/>
        <div class="ni-controls">
            <Select label="Media" value=filter option_pairs=kinds/>
        </div>
        {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
        {list}
        <div node_ref=sentinel class="ni-sentinel" aria-hidden="true"></div>
        {move || (!done.get() && !rows.get().is_empty()).then(|| view! {
            <Button variant="default" class:ni-more=true
                on_click=Callback::new(move |()| load_more())>
                {move || if busy.get() { "Loading…" } else { "Load more" }}
            </Button>
        })}
    }
}
