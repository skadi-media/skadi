//! Television domain view (SKADI-T-0275): the series library wall + a per-series
//! detail page with season/episode rows and monitor toggles.
//!
//! Lists from `GET /series`; the detail page reads `GET /series/{id}` (with
//! seasons + episodes) and toggles monitoring per series / season / episode.
//! Adding a series lives in the unified `/add` flow.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::api;
use crate::confirm::{ConfirmSpec, confirm};
use crate::library_select::{BulkBar, Selection, apply_report, tile_check, tile_class};
use crate::library_toolbar::{
    LibCounts, LibraryToolbar, TV_SORT_STORAGE, VIDEO_SORT_KEYS, chip_matches, sort_items,
    stored_sort,
};
use crate::movies::{DiagnosticsPanel, HistoryPanel, ReleasesPanel, status_class, tmdb_img};

/// The series wall's status chips; a series with every episode is "Complete".
pub const TV_CHIPS: &[(&str, &str)] = &[
    ("all", "All"),
    ("owned", "Complete"),
    ("wanted", "Wanted"),
    ("downloading", "Downloading"),
];

/// Library-wall status for a series: `downloading` (any episode mid-acquisition),
/// `owned` (episodes present + all imported/cutoff), else `wanted`.
pub fn series_lib_status(s: &api::Series) -> &'static str {
    // Activity anywhere (specials included) still reads "downloading".
    if s.episodes.iter().any(|e| {
        matches!(
            api::status_label(&e.status).as_str(),
            "searching" | "snatched" | "downloading"
        )
    }) {
        return "downloading";
    }
    // Specials (season 0) don't count against completeness: a show with every
    // regular episode collected is owned, even with bonus specials missing —
    // same philosophy as import extras (SKADI-T-0329). A specials-only show is
    // judged by its specials.
    let regular: Vec<&api::Episode> = s.episodes.iter().filter(|e| e.season != 0).collect();
    let pool: Vec<&api::Episode> = if regular.is_empty() {
        s.episodes.iter().collect()
    } else {
        regular
    };
    let all_owned = !pool.is_empty()
        && pool.iter().all(|e| {
            let l = api::status_label(&e.status);
            l == "imported" || l == "cutoff"
        });
    if all_owned { "owned" } else { "wanted" }
}

#[component]
pub fn TvPage() -> impl IntoView {
    let series = RwSignal::new(Vec::<api::Series>::new());
    let error = RwSignal::new(None::<String>);
    let lib_filter = RwSignal::new("all");
    let lib_text = RwSignal::new(String::new());
    // Sort (SKADI-T-0695), remembered across reloads.
    let sort = stored_sort(TV_SORT_STORAGE, VIDEO_SORT_KEYS);
    // Genre facet (SKADI-T-0605): one chip per genre the loaded items carry,
    // most common first; empty until the server has refreshed metadata.
    let genre_filter = RwSignal::new(None::<String>);
    // Import and the settings gear are operator surfaces, and were offered to
    // every role — the per-role sweep walks the nav strips and never saw these
    // in-page links (SKADI-T-0639). Gate on a *known* admin so an unresolved
    // role offers nothing.
    let role_signal = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    let is_admin = move || role_signal.and_then(|r| r.get()).as_deref() == Some("admin");

    Effect::new(move |_| {
        spawn_local(async move {
            match api::list_series().await {
                Ok(s) => {
                    series.set(s);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    });

    let counts =
        Signal::derive(move || series.with(|ss| LibCounts::of(ss.iter().map(series_lib_status))));
    // Multi-select + bulk bar (SKADI-T-0696), admin only.
    let sel = Selection::new();
    // The wall as shown: filtered, then sorted; tiles and "select all" share it.
    let shown_series = move || {
        let f = lib_filter.get();
        let q = lib_text.get().to_lowercase();
        let mut shown: Vec<api::Series> = series
            .get()
            .into_iter()
            .filter(|s| chip_matches(f, series_lib_status(s)))
            .filter(|s| q.is_empty() || s.title.to_lowercase().contains(&q))
            .filter(|s| {
                genre_filter
                    .get()
                    .is_none_or(|g| s.genres.iter().any(|x| x == &g))
            })
            .collect();
        sort_items(&mut shown, sort.get());
        shown
    };
    let tiles = move || {
        shown_series()
            .into_iter()
            .map(|s| series_tile(s, Some(sel)))
            .collect_view()
    };
    let visible_ids =
        Signal::derive(move || shown_series().into_iter().map(|s| s.id).collect::<Vec<_>>());
    let on_bulk_done = Callback::new(
        move |(action, report): (api::BulkAction, api::BulkReport)| {
            series.update(|ss| apply_report(ss, action, &report));
        },
    );
    let genre_chips = move || {
        let counts = crate::genre_counts(series.get().iter().map(|s| s.genres.as_slice()));
        let active = genre_filter.get();
        counts
            .into_iter()
            .map(|(name, n)| {
                let is_active = active.as_deref() == Some(name.as_str());
                let pick = name.clone();
                view! {
                    <button
                        class=if is_active { "filter-chip active" } else { "filter-chip" }
                        on:click=move |_| genre_filter.update(|g| {
                            if g.as_deref() == Some(pick.as_str()) { *g = None } else { *g = Some(pick.clone()) }
                        })
                    >
                        {name.clone()}
                        <span class="chip-count mono">{n}</span>
                    </button>
                }
            })
            .collect_view()
    };

    view! {
        <section class="library">
            <div class="lib-head">
                <div class="lib-title">
                    <span class="lib-accent gold"></span>
                    <h2>"TV"</h2>
                </div>
                <div class="lib-head-right">
                    {move || is_admin().then(|| view! {
                        <A href="/tv/import" attr:class="btn-link" attr:title="Import an existing TV library">"Import"</A>
                    })}
                </div>
            </div>
            <LibraryToolbar
                chips=TV_CHIPS
                filter=lib_filter
                counts=counts
                text=lib_text
                placeholder="⌕ Filter series…"
                sort=sort
                sort_keys=VIDEO_SORT_KEYS
                storage_key=TV_SORT_STORAGE
                facets=move || view! { <div class="filter-chips genre-chips">{genre_chips}</div> }
                // Reactive inside: the slot renders once, and the role resolves later.
                bulk=move || view! { {move || is_admin().then(|| view! {
                    <BulkBar
                        sel=sel
                        visible=visible_ids
                        collection="series"
                        noun=crate::library_select::SERIES
                        on_done=on_bulk_done
                    />
                })} }
            />
            {move || error.get().map(|e| view! { <p class="bad">"Load failed: " {e}</p> })}
            {move || series.get().is_empty().then(|| view! { <p class="muted">"No series yet — add one with + Add media."</p> })}
            <div class="poster-grid">{tiles}</div>
        </section>
    }
}

/// One poster tile in the series wall; click navigates to the detail page, or
/// toggles the tile in select mode (SKADI-T-0696).
fn series_tile(s: api::Series, sel: Option<Selection>) -> AnyView {
    let id = s.id.clone();
    let nav = use_navigate();
    let on_open = move |_| match sel {
        Some(x) if x.mode.get_untracked() => x.toggle(&id),
        _ => nav(&format!("/tv/{id}"), Default::default()),
    };
    let base = if s.monitored {
        "poster-tile"
    } else {
        "poster-tile unmonitored"
    };
    let class = tile_class(base, sel, s.id.clone());
    let check = tile_check(sel, s.id.clone());
    let year = s.year.map(|y| y.to_string()).unwrap_or_default();
    let status = series_lib_status(&s);
    let dot = match status {
        "owned" => "owned",
        "downloading" => "dl",
        _ => "wanted",
    };
    let wanted = status == "wanted";
    let poster = s
        .poster_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(|u| tmdb_img(u, "w342"));
    let title = s.title.clone();

    view! {
        <div class=class on:click=on_open>
            <div class="poster-img">
                {match poster {
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
                    <span class="tile-sub mono">{year}</span>
            </div>
        </div>
    }
    .into_any()
}

/// Series detail page: hero + overview, the per-season episode list with monitor
/// toggles, and series-level monitor / delete actions.
#[component]
pub fn SeriesDetailPage() -> impl IntoView {
    let params = use_params_map();
    let series = RwSignal::new(None::<api::Series>);
    // Which seasons and episode histories are open. Held HERE, not inside the
    // season/episode blocks: those are rebuilt whenever the series reloads
    // after an action, and a signal created inside them was reset with them —
    // every open season snapped shut on the next acquire or monitor toggle.
    let open_seasons = RwSignal::new(std::collections::HashSet::<u16>::new());
    let open_episodes = RwSignal::new(std::collections::HashSet::<String>::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let alive = RwSignal::new(true);

    let load = move || {
        let id = params.read().get("id").unwrap_or_default();
        if id.is_empty() {
            return;
        }
        spawn_local(async move {
            match api::get_series(&id).await {
                Ok(s) => {
                    series.set(Some(s));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };

    Effect::new(move |_| {
        spawn_local(async move {
            // Only re-render on a changed payload (SKADI-T-0596): an unchanged tick
            // must not rebuild the episode rows and wipe an open release search.
            let mut last: Option<String> = None;
            loop {
                let id = params.get_untracked().get("id").unwrap_or_default();
                // try_set + break: survives navigating away mid-fetch
                // (disposed-signal panic, SKADI-T-0331 verification).
                if !id.is_empty()
                    && let Ok(Some(s)) = api::get_series_if_changed(&id, &mut last).await
                    && series.try_set(Some(s)).is_some()
                {
                    break;
                }
                gloo_timers::future::TimeoutFuture::new(4000).await;
                if !alive.try_get_untracked().unwrap_or(false) {
                    break;
                }
            }
        });
    });
    on_cleanup(move || alive.set(false));

    let nav = use_navigate();

    let body = move || {
        let Some(s) = series.get() else {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        };
        let sid = s.id.clone();
        let year = s.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let monitored = s.monitored;
        let overview = s.overview.clone().filter(|o| !o.is_empty());
        let genres = Some(s.genres.join(", ")).filter(|g| !g.is_empty());
        let rating = s.content_rating.clone().filter(|r| !r.is_empty());
        let network = s.network.clone().filter(|n| !n.is_empty());
        let status = s.status.clone().filter(|n| !n.is_empty());
        let poster = s
            .poster_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| tmdb_img(u, "w500"));
        // Faded banner behind the hero: a wide backdrop when we have one, else the
        // poster as a fallback (same pattern as the movie detail page).
        let backdrop = s
            .backdrop_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| tmdb_img(u, "w780"))
            .or_else(|| poster.clone());

        let mon_id = sid.clone();
        let toggle_monitor = move |_| {
            let id = mon_id.clone();
            busy.set(true);
            spawn_local(async move {
                let _ = api::set_series_monitored(&id, !monitored).await;
                busy.set(false);
                load();
            });
        };
        let del_id = sid.clone();
        let del_title = s.title.clone();
        let nav_del = nav.clone();
        let on_delete = move |_| {
            let body =
                format!("Delete \"{del_title}\" and its files from disk? This can't be undone.");
            let id = del_id.clone();
            let nav = nav_del.clone();
            spawn_local(async move {
                if !confirm(ConfirmSpec::destructive("Delete the series?", body)).await {
                    return;
                }
                busy.set(true);
                let _ = api::delete_series(&id, true).await;
                busy.set(false);
                nav("/tv", Default::default());
            });
        };

        // Season blocks, ascending; specials (0) last.
        let mut seasons = s.seasons.clone();
        seasons.sort_by_key(|sea| {
            if sea.number == 0 {
                u16::MAX
            } else {
                sea.number
            }
        });
        let season_blocks = seasons
            .into_iter()
            .map(|sea| {
                season_block(
                    &s,
                    sea,
                    Callback::new(move |()| load()),
                    open_seasons,
                    open_episodes,
                )
            })
            .collect_view();

        // Series-wide progress (fast overview): REGULAR episodes collected /
        // total — specials don't count against completeness (SKADI-T-0329);
        // the Specials season block below still shows its own numbers.
        let total_eps = s.episodes.iter().filter(|e| e.season != 0).count();
        let collected_eps = s
            .episodes
            .iter()
            .filter(|e| e.season != 0 && is_collected(&e.status))
            .count();
        let series_complete = total_eps > 0 && collected_eps == total_eps;
        let progress_cls = if series_complete {
            "series-progress mono ok"
        } else {
            "series-progress mono"
        };

        let no_hero = backdrop.is_none();
        view! {
            <>
                <div class="page-head">
                    // A real router link (movies/audiobooks parity) — the old
                    // programmatic-nav <button> didn't navigate (SKADI-T-0329).
                    <A href="/tv" attr:class="btn-link">"← Library"</A>
                </div>
                <div class="detail" class:no-hero=no_hero>
                    {backdrop.map(|src| view! {
                        <div class="detail-hero" style=format!("background-image:url('{src}')")></div>
                    })}
                    <div class="detail-body">
                        {match poster {
                            Some(src) => view! { <img class="detail-poster" src=src alt=""/> }.into_any(),
                            None => view! { <div class="detail-poster poster-ph">{s.title.clone()}</div> }.into_any(),
                        }}
                        <div class="detail-info">
                            <h2>{s.title.clone()}<span class="muted">{year}</span></h2>
                            <p class="mono muted">
                                {network.map(|n| format!("{n} · ")).unwrap_or_default()}
                                {status.unwrap_or_default()}
                                <span class=progress_cls>
                                    {format!(" · {collected_eps}/{total_eps} collected")}
                                </span>
                            </p>
                            <div class="card-actions">
                                <button class="btn" prop:disabled=move || busy.get() on:click=toggle_monitor>
                                    {if monitored { "Unmonitor series" } else { "Monitor series" }}
                                </button>
                                <button class="btn bad" prop:disabled=move || busy.get() on:click=on_delete>"Delete"</button>
                            </div>
{rating.map(|r| view! { <span class="rating-chip">{r}</span> })}
                        {genres.map(|g| view! { <p class="genres muted">{g}</p> })}
                            {overview.map(|o| view! { <p class="overview">{o}</p> })}
                            <div class="card-editions">{season_blocks}</div>
                        </div>
                    </div>
                </div>
            </>
        }
        .into_any()
    };

    view! {
        <>
            {move || error.get().map(|e| view! { <p class="bad">"Error: " {e}</p> })}
            {body}
        </>
    }
}

/// Whether an episode is **collected** (its file is in the library) — i.e. its
/// acquisition status is `Imported`. Used for the per-season / series progress.
pub fn is_collected(status: &serde_json::Value) -> bool {
    api::status_label(status) == "imported"
}

/// One season's block: a **collapsible** header showing `collected/total` + a
/// monitor toggle, over its episode rows. Long shows stay scannable — a fully
/// collected season starts collapsed; an incomplete one starts open so the gaps
/// are visible at a glance.
fn season_block(
    s: &api::Series,
    sea: api::Season,
    reload: Callback<()>,
    open_seasons: RwSignal<std::collections::HashSet<u16>>,
    open_episodes: RwSignal<std::collections::HashSet<String>>,
) -> AnyView {
    let sid = s.id.clone();
    let series_title = s.title.clone();
    let num = sea.number;
    let label = if num == 0 {
        "Specials".to_string()
    } else {
        format!("Season {num}")
    };
    let monitored = sea.monitored;
    let toggle_sid = sid.clone();
    let toggle = move |_| {
        let id = toggle_sid.clone();
        spawn_local(async move {
            let _ = api::monitor_season(&id, num, !monitored).await;
            reload.run(());
        });
    };

    let mut eps: Vec<api::Episode> = s
        .episodes
        .iter()
        .filter(|e| e.season == num)
        .cloned()
        .collect();
    eps.sort_by_key(|e| e.number);
    let total = eps.len();
    let collected = eps.iter().filter(|e| is_collected(&e.status)).count();
    let complete = total > 0 && collected == total;
    let rows = eps
        .into_iter()
        .map(|e| episode_row(&sid, &series_title, e, reload, open_episodes))
        .collect_view();

    // Collapsed by default — long shows stay scannable; open the seasons you want.
    // The episode list is kept in the DOM and hidden via CSS so per-episode
    // History stays expanded across toggles. Open state lives on the page.
    let collapsed = move || !open_seasons.get().contains(&num);
    let toggle_open = move |_| {
        open_seasons.update(|o| {
            if !o.remove(&num) {
                o.insert(num);
            }
        });
    };
    let count_cls = if complete {
        "season-count mono ok"
    } else {
        "season-count mono muted"
    };

    view! {
        <div class="edition-block season-block">
            <div class="edition-row season-head">
                <button class="btn-link season-toggle" on:click=toggle_open>
                    <span class="chevron">{move || if collapsed() { "▸" } else { "▾" }}</span>
                    <strong>{label}</strong>
                </button>
                <span class=count_cls>{format!("{collected}/{total} collected")}</span>
                <button class="btn-link season-mon" on:click=toggle>
                    {if monitored { "Monitored" } else { "Unmonitored" }}
                </button>
            </div>
            <div class="season-eps" class:collapsed=collapsed>
                {rows}
            </div>
        </div>
    }
    .into_any()
}

/// One episode row: SxxEyy + title + air date + status, with a monitor toggle.
fn episode_row(
    series_id: &str,
    // Only for naming a saved file (SKADI-T-0635). "S01E02 - Pilot.mkv"
    // landing in someone's Downloads beside three other shows is not a name.
    series_title: &str,
    e: api::Episode,
    reload: Callback<()>,
    open_episodes: RwSignal<std::collections::HashSet<String>>,
) -> AnyView {
    let sid = series_id.to_string();
    let code = format!("S{:02}E{:02}", e.season, e.number);
    let title = e.title.clone().unwrap_or_default();
    let air = e.air_date.clone().unwrap_or_default();
    let label = api::status_label(&e.status);
    let cls = status_class(&label);
    let show_diag = !matches!(label.as_str(), "imported" | "cutoff");
    let watch_href = (!show_diag).then(|| format!("/watch/tv/{series_id}/{}", e.id));
    // Save the file itself (SKADI-T-0635) — a plain anchor against the same
    // route the player uses, which already takes `apikey` and answers Range.
    let save = (!show_diag).then(|| {
        let stem = if title.is_empty() {
            format!("{series_title} - {code}")
        } else {
            format!("{series_title} - {code} - {title}")
        };
        let container = e.media_info.as_ref().and_then(|mi| mi.container.clone());
        (
            api::episode_video_url(series_id, &e.id),
            api::download_filename(&stem, container.as_deref()),
        )
    });
    let monitored = e.monitored;
    let eid = e.id.clone();
    let eid_hist = e.id.clone();
    let eid_diag = e.id.clone();
    // Manual acquire (SKADI-T-0559), reachable from the same expander as the
    // history: TV has no per-episode detail page, so everything about an episode
    // lives inline under its row.
    let at = api::AcquirablePath::tv_episode(series_id, &e.id);
    let eid_panel = e.id.clone();
    let sid_act = series_id.to_string();
    let eid_act = e.id.clone();
    let sid_reset = series_id.to_string();
    let eid_reset = e.id.clone();
    let note = RwSignal::new(None::<Result<String, String>>);

    let on_acquire = move |_| {
        let (sid, eid) = (sid_act.clone(), eid_act.clone());
        note.set(Some(Ok("searching…".into())));
        spawn_local(async move {
            let r = api::acquire_edition(&api::AcquirablePath::tv_episode(&sid, &eid)).await;
            note.set(Some(
                r.map(|()| "acquire started".into())
                    .map_err(|e| e.to_string()),
            ));
            reload.run(());
        });
    };
    let on_reset = move |_| {
        let (sid, eid) = (sid_reset.clone(), eid_reset.clone());
        note.set(Some(Ok("resetting…".into())));
        spawn_local(async move {
            let r = api::reset_edition(&api::AcquirablePath::tv_episode(&sid, &eid)).await;
            note.set(Some(r.map(|()| "reset".into()).map_err(|e| e.to_string())));
            reload.run(());
        });
    };
    // Expand to reveal this episode's acquisition History (SKADI-T-0315) — TV has no per-episode
    // detail page, so the timeline lives inline under the row.
    let ep_key = e.id.clone();
    let expanded = {
        let k = ep_key.clone();
        move || open_episodes.get().contains(&k)
    };
    let expanded_body = expanded.clone();
    let toggle_expanded = move |_| {
        let k = ep_key.clone();
        open_episodes.update(|o| {
            if !o.remove(&k) {
                o.insert(k);
            }
        });
    };
    let toggle = move |_| {
        let sid = sid.clone();
        let eid = eid.clone();
        spawn_local(async move {
            let _ = api::monitor_episode(&sid, &eid, !monitored).await;
            reload.run(());
        });
    };

    view! {
        <div class="episode-wrap">
            <div class="edition-row episode-row">
                <button
                    class="btn-link hist-toggle"
                    title="History"
                    on:click=toggle_expanded
                >
                    {move || if expanded() { "▾" } else { "▸" }}
                </button>
                <span class="mono">{code}</span>
                <span class="episode-title">{title}</span>
                <span class="mono muted">{air}</span>
                <span class=cls>{label}</span>
                {watch_href.map(|h| view! {
                    <A href=h attr:class="btn-link" attr:title="Play in this browser">"▶"</A>
                })}
                {save.map(|(href, name)| view! {
                    <a href=href download=name class="btn-link"
                       title="Save this episode to your computer">"⇩"</a>
                })}
                <button class="btn-link" on:click=toggle title="Toggle monitoring">
                    {if monitored { "●" } else { "○" }}
                </button>
                <button class="btn-link" on:click=on_acquire title="Search and acquire">
                    "⤓"
                </button>
                <button class="btn-link" on:click=on_reset title="Reset to Missing">
                    "↺"
                </button>
            </div>
            {move || note.get().map(|r| match r {
                Ok(m) => view! { <p class="muted ep-note">{m}</p> }.into_any(),
                Err(e) => view! { <p class="bad ep-note">{e}</p> }.into_any(),
            })}
            {move || {
                expanded_body()
                    .then(|| view! {
                        <ReleasesPanel
                            at=at.clone()
                            acquirable=eid_panel.clone()
                            reload=reload
                        />
                        <HistoryPanel acquirable=eid_hist.clone() open=true/>
                        {show_diag.then(|| view! { <DiagnosticsPanel acquirable=eid_diag.clone()/> })}
                    })
            }}
        </div>
    }
    .into_any()
}
