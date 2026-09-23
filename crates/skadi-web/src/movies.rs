//! Movies domain view (SKADI-T-0069): browse the library with per-edition
//! acquisition status, add movies (search by title or by TMDB id), trigger a
//! manual acquire, and toggle monitoring / delete.
//!
//! Lists from `GET /movies` (works even when the movies domain is disabled, so
//! you can add + configure before enabling). Profile/root pickers come from the
//! `profiles`/`root_folders` settings; edition-kind ids are resolved to names
//! via `/edition-kinds`.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{Value, json};

use crate::api;

/// CSS class for a coarse status label.
pub fn status_class(label: &str) -> &'static str {
    match label {
        "imported" | "cutoff" => "ok",
        "failed" => "bad",
        "missing" => "muted",
        _ => "pending", // searching / snatched / downloading
    }
}

#[component]
pub fn MoviesPage() -> impl IntoView {
    let movies = RwSignal::new(Vec::<api::Movie>::new());
    let error = RwSignal::new(None::<String>);
    // Library-wall filters (SKADI-T-0245): status chip + free-text filter.
    let lib_filter = RwSignal::new("all");
    let lib_text = RwSignal::new(String::new());
    // Genre facet (SKADI-T-0605): one chip per genre the loaded items carry,
    // most common first; empty until the server has refreshed metadata.
    let genre_filter = RwSignal::new(None::<String>);

    // Adding lives in the unified `/add` view (SKADI-T-0248); this page is purely
    // the library wall now.
    Effect::new(move |_| {
        spawn_local(async move {
            match api::list_movies().await {
                Ok(m) => {
                    movies.set(m);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    });

    // (total, owned, wanted, downloading) for the header + chip counts.
    let counts = move || {
        let (mut owned, mut wanted, mut dl) = (0usize, 0usize, 0usize);
        let ms = movies.get();
        for m in &ms {
            match movie_lib_status(m) {
                "owned" => owned += 1,
                "downloading" => dl += 1,
                _ => wanted += 1,
            }
        }
        (ms.len(), owned, wanted, dl)
    };
    let poster_tiles = move || {
        let f = lib_filter.get();
        let q = lib_text.get().to_lowercase();
        movies
            .get()
            .into_iter()
            .filter(|m| f == "all" || movie_lib_status(m) == f)
            .filter(|m| q.is_empty() || m.title.to_lowercase().contains(&q))
            .filter(|m| {
                genre_filter
                    .get()
                    .is_none_or(|g| m.genres.iter().any(|x| x == &g))
            })
            .map(poster_tile)
            .collect_view()
    };
    let genre_chips = move || {
        let counts = crate::genre_counts(movies.get().iter().map(|m| m.genres.as_slice()));
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
    // A filter chip: label + live count, active style when selected.
    let chip = move |key: &'static str, label: &'static str| {
        let count = move || {
            let (t, o, w, d) = counts();
            match key {
                "owned" => o,
                "wanted" => w,
                "downloading" => d,
                _ => t,
            }
        };
        view! {
            <button
                class=move || {
                    if lib_filter.get() == key { "filter-chip active" } else { "filter-chip" }
                }
                on:click=move |_| lib_filter.set(key)
            >
                {label}
                <span class="chip-count mono">{count}</span>
            </button>
        }
    };

    view! {
        <section class="library">
            <div class="lib-head">
                <div class="lib-title">
                    <span class="lib-accent ice"></span>
                    <h2>"Movies"</h2>
                </div>
                <div class="lib-head-right">
                    <A href="/movies/import" attr:class="btn-link" attr:title="Import existing media in place">"Import"</A>
                    <A href="/movies/config" attr:class="gear" attr:title="Movies settings">"⚙"</A>
                </div>
            </div>
            <div class="filter-bar">
                <div class="filter-chips">
                    {chip("all", "All")} {chip("owned", "Owned")} {chip("wanted", "Wanted")}
                    {chip("downloading", "Downloading")}
                </div>
                <div class="filter-chips genre-chips">{genre_chips}</div>
                <input
                    class="lib-filter-input"
                    r#type="text"
                    placeholder="⌕ Filter library…"
                    prop:value=move || lib_text.get()
                    on:input=move |ev| lib_text.set(event_target_value(&ev))
                />
            </div>
            {move || error.get().map(|e| view! { <p class="bad">"Load failed: " {e}</p> })}
            {move || movies.get().is_empty().then(|| view! { <p class="muted">"No movies yet — add one with + Add media."</p> })}
            <div class="poster-grid">{poster_tiles}</div>
        </section>
    }
}

/// Rewrite a TMDB image URL (`…/t/p/original/<path>`) to a smaller size segment
/// for lighter loads; non-TMDB URLs pass through unchanged.
pub fn tmdb_img(url: &str, size: &str) -> String {
    if let Some(idx) = url.find("/t/p/") {
        // After `/t/p/` comes `<size>/<path>`; replace the size segment.
        let after = &url[idx + 5..];
        if let Some(slash) = after.find('/') {
            return format!("{}/t/p/{size}{}", &url[..idx], &after[slash..]);
        }
    }
    url.to_string()
}

/// Library-wall status category for a movie (SKADI-T-0245): `owned` (any edition
/// imported/cutoff), `downloading` (any mid-acquisition), else `wanted`. Drives the
/// tile's status-dot color and the filter chips.
pub fn movie_lib_status(m: &api::Movie) -> &'static str {
    let labels: Vec<String> = m
        .editions
        .iter()
        .map(|e| api::status_label(&e.status))
        .collect();
    if labels.iter().any(|l| l == "imported" || l == "cutoff") {
        "owned"
    } else if labels
        .iter()
        .any(|l| matches!(l.as_str(), "searching" | "snatched" | "downloading"))
    {
        "downloading"
    } else {
        "wanted"
    }
}

/// One 2:3 poster tile in the library wall: media tag + status dot, title/sub
/// bottom-anchored over a scrim, a dashed "+" overlay when wanted (SKADI-T-0245).
fn poster_tile(m: api::Movie) -> AnyView {
    let id = m.id.clone();
    // Open the full-page detail (the drawer was dropped).
    let nav = use_navigate();
    let on_open = move |_| nav(&format!("/movies/{id}"), Default::default());
    let year = m.year.map(|y| y.to_string()).unwrap_or_default();
    let status = movie_lib_status(&m);
    let dot = match status {
        "owned" => "owned",
        "downloading" => "dl",
        _ => "wanted",
    };
    let wanted = status == "wanted";
    let poster = m
        .poster_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(|u| tmdb_img(u, "w342"));
    let title = m.title.clone();

    view! {
        <div class="poster-tile" on:click=on_open>
            <div class="poster-img">
                {match poster {
                    Some(src) => view! { <img src=src alt=title.clone() loading="lazy"/> }.into_any(),
                    None => view! { <div class="poster-ph">{title.clone()}</div> }.into_any(),
                }}
                // Dot only for non-owned states — a wall of identical green
                // dots on a fully-owned library is pure noise (SKADI-T-0348).
                // Type chip dropped: you're already in the Movies wall.
                {(status != "owned")
                    .then(|| view! { <span class=format!("status-dot {dot}")></span> })}
                {wanted
                    .then(|| view! { <div class="tile-wanted"><span>"+"</span></div> })}
            </div>
            <div class="tile-scrim">
                    <span class="tile-title">{title}</span>
                    <span class="tile-sub mono">{year}</span>
            </div>
        </div>
    }
    .into_any()
}

/// Movie detail page (SKADI-T-0076): backdrop hero + poster + overview, the
/// per-edition status, and the acquire / monitor / delete actions. Read-first —
/// it surfaces the artwork the library now stores and the actions that already
/// existed on the old card.
#[component]
pub fn MovieDetailPage() -> impl IntoView {
    let params = use_params_map();
    let movie = RwSignal::new(None::<api::Movie>);
    let kinds = RwSignal::new(HashMap::<String, String>::new());
    let acquire_status = RwSignal::new(HashMap::<String, Result<String, String>>::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let alive = RwSignal::new(true);
    let load = move || {
        let id = params.read().get("id").unwrap_or_default();
        if id.is_empty() {
            return;
        }
        spawn_local(async move {
            match api::get_movie(&id).await {
                Ok(m) => {
                    movie.set(Some(m));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };

    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(ks) = api::list_edition_kinds().await {
                kinds.set(ks.into_iter().map(|k| (k.id, k.name)).collect());
            }
        });
        // Poll the movie every 3s so status + Downloading{progress} stay live
        // (polling-based, no websockets — consistent with the CSR design).
        spawn_local(async move {
            // Only re-render on a changed payload (SKADI-T-0596): a tick that finds
            // nothing new must not wipe an open panel or a release search.
            let mut last: Option<String> = None;
            loop {
                let id = params.get_untracked().get("id").unwrap_or_default();
                if !id.is_empty() {
                    // try_set + break: survives navigating away mid-fetch
                    // (disposed-signal panic, SKADI-T-0331 verification).
                    match api::get_movie_if_changed(&id, &mut last).await {
                        Ok(Some(m)) => {
                            if movie.try_set(Some(m)).is_some() {
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

    // Re-fetch the movie after an action (grab/reset/etc.) for snappy feedback
    // between poll ticks.
    let reload = Callback::new(move |_: ()| load());

    let nav = use_navigate();

    let body = move || {
        let Some(m) = movie.get() else {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        };
        let year = m.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let monitored = m.monitored;
        let overview = m.overview.clone().filter(|o| !o.is_empty());
        let genres = Some(m.genres.join(", ")).filter(|g| !g.is_empty());
        let rating = m.content_rating.clone().filter(|r| !r.is_empty());
        let backdrop = m
            .backdrop_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| tmdb_img(u, "w1280"));
        let poster = m
            .poster_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| tmdb_img(u, "w500"));
        let kmap = kinds.get();

        // Actions reconstructed per render (capture the current movie id).
        let mon_id = m.id.clone();
        let toggle_monitor = move |_| {
            let id = mon_id.clone();
            busy.set(true);
            spawn_local(async move {
                let _ = api::set_movie_monitored(&id, !monitored).await;
                busy.set(false);
                load();
            });
        };
        let del_id = m.id.clone();
        let del_title = m.title.clone();
        let nav_del = nav.clone();
        let on_delete = move |_| {
            if !window_confirm(&format!(
                "Delete \"{del_title}\" and its files from disk? This can't be undone."
            )) {
                return;
            }
            let id = del_id.clone();
            let nav = nav_del.clone();
            busy.set(true);
            spawn_local(async move {
                let _ = api::delete_movie(&id, true).await;
                busy.set(false);
                nav("/movies", Default::default());
            });
        };

        let edition_rows = m
            .editions
            .iter()
            .map(|e| {
                let label = api::status_label(&e.status);
                let cls = status_class(&label);
                // Asking why an imported edition was not grabbed answers itself.
                let show_diag = !matches!(label.as_str(), "imported" | "cutoff");
                // Live download progress, when the edition is mid-transfer.
                let progress = api::download_progress(&e.status)
                    .map(|p| format!(" {:.0}%", (p * 100.0).clamp(0.0, 100.0)));
                let kind_name = kmap
                    .get(&e.kind)
                    .cloned()
                    .unwrap_or_else(|| "edition".into());
                let movie_id = m.id.clone();
                let edition_id = e.id.clone();
                let acq_mid = movie_id.clone();
                let acq_eid = edition_id.clone();
                let on_acquire = Callback::new(move |()| {
                    let movie_id = acq_mid.clone();
                    let edition_id = acq_eid.clone();
                    acquire_status.update(|x| {
                        x.insert(edition_id.clone(), Ok("…".into()));
                    });
                    spawn_local(async move {
                        let r = api::acquire_edition(&api::AcquirablePath::movie_edition(&movie_id, &edition_id)).await;
                        acquire_status.update(|x| {
                            x.insert(
                                edition_id.clone(),
                                r.map(|()| "acquire started".into())
                                    .map_err(|e| e.to_string()),
                            );
                        });
                    });
                });
                // Reset (unwedge) — only meaningful for an in-flight/failed edition.
                let rst_mid = movie_id.clone();
                let rst_eid = edition_id.clone();
                let resettable = edition_resettable(&label);
                let on_reset = Callback::new(move |()| {
                    let movie_id = rst_mid.clone();
                    let edition_id = rst_eid.clone();
                    acquire_status.update(|x| {
                        x.insert(edition_id.clone(), Ok("resetting…".into()));
                    });
                    spawn_local(async move {
                        let r = api::reset_edition(&api::AcquirablePath::movie_edition(&movie_id, &edition_id)).await;
                        acquire_status.update(|x| {
                            x.insert(
                                edition_id.clone(),
                                r.map(|()| "reset to Missing".into())
                                    .map_err(|e| e.to_string()),
                            );
                        });
                        reload.run(());
                    });
                });
                let eid_for_view = e.id.clone();
                let acq_view = move || {
                    acquire_status.with(|x| match x.get(&eid_for_view) {
                        Some(Ok(msg)) => view! { <span class="ok">{msg.clone()}</span> }.into_any(),
                        Some(Err(msg)) => {
                            view! { <span class="bad">{msg.clone()}</span> }.into_any()
                        }
                        None => ().into_any(),
                    })
                };
                let watch_href = (!show_diag).then(|| format!("/watch/movie/{movie_id}/{edition_id}"));
                view! {
                    <div class="edition-block">
                        <div class="edition-row">
                            <span class="muted">{kind_name}</span>
                            <span class=cls>{label}{progress}</span>
                            {watch_href.map(|h| view! {
                                <A href=h attr:class="btn-link listen-link" attr:title="Play in this browser">"▶ Watch"</A>
                            })}
                            <EditionActions resettable=resettable on_acquire=on_acquire on_reset=on_reset/>
                            {acq_view}
                        </div>
                        <ReleasesPanel
                            at=api::AcquirablePath::movie_edition(&movie_id, &edition_id)
                            acquirable=edition_id.clone()
                            reload=reload
                        />
                        <HistoryPanel acquirable=edition_id.clone()/>
                        {show_diag.then(|| view! { <DiagnosticsPanel acquirable=edition_id.clone()/> })}
                    </div>
                }
            })
            .collect_view();

        let no_hero = backdrop.is_none();
        view! {
            <div class="detail" class:no-hero=no_hero>
                {backdrop.map(|src| view! {
                    <div class="detail-hero" style=format!("background-image:url('{src}')")></div>
                })}
                <div class="detail-body">
                    {match poster {
                        Some(src) => view! { <img class="detail-poster" src=src alt=m.title.clone()/> }.into_any(),
                        None => view! { <div class="detail-poster poster-ph">{m.title.clone()}</div> }.into_any(),
                    }}
                    <div class="detail-info">
                        <h2>{m.title.clone()}<span class="muted">{year}</span></h2>
                        <div class="card-actions">
                            <button on:click=toggle_monitor disabled=move || busy.get()>
                                {if monitored { "Unmonitor" } else { "Monitor" }}
                            </button>
                            <button class="danger" on:click=on_delete disabled=move || busy.get()>"Delete"</button>
                        </div>
{rating.map(|r| view! { <span class="rating-chip">{r}</span> })}
                        {genres.map(|g| view! { <p class="genres muted">{g}</p> })}
                        {overview.map(|o| view! { <p class="overview">{o}</p> })}
                        <div class="card-editions">{edition_rows}</div>
                    </div>
                </div>
            </div>
        }
        .into_any()
    };

    view! {
        <div class="page-head">
            <A href="/movies" attr:class="btn-link">"← Library"</A>
        </div>
        {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
        {body}
    }
}

/// Whether an edition in `label` state can be **Reset** (unwedged) — only the
/// non-terminal/failed acquire states (SKADI-T-0112/T-0117). Pure, so it's
/// unit-testable.
pub fn edition_resettable(label: &str) -> bool {
    matches!(label, "searching" | "snatched" | "downloading" | "failed")
}

/// Presentational per-edition action bar (SKADI-T-0117/T-0119): **Acquire** +
/// **Reset** (disabled unless [`edition_resettable`]). Actions delegated via
/// callbacks so it's unit-mountable with no state.
#[component]
pub fn EditionActions(
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

/// Per-item acquisition **History** (SKADI-T-0315): the arr-style timeline of grabbed /
/// imported / failed events for one acquirable, newest first — with the failure reason code
/// and a **blocklist-and-search** action on a grab. Domain-agnostic: takes the opaque
/// `acquirable` ref (movie edition id / book file id / episode id), so movies, audiobooks, and
/// TV all reuse it. Mirrors Sonarr/Radarr's per-item History tab.
#[component]
pub fn HistoryPanel(acquirable: String, #[prop(default = false)] open: bool) -> impl IntoView {
    let rows = RwSignal::new(Vec::<api::HistoryRow>::new());
    let loaded = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    // Collapsed by default so it doesn't dominate the detail page; `open` (TV, where the row
    // expand already gates it) starts it expanded.
    // Open/closed survives the page re-rendering this panel (SKADI-T-0596).
    let collapsed = crate::persist::persisted(format!("history:{acquirable}:collapsed"), !open);
    let acq = StoredValue::new(acquirable);

    let reload = move || {
        spawn_local(async move {
            // try_set: this fetch can land after the page is gone (fast
            // navigation into the player) — a plain set on a disposed signal
            // panics the whole wasm app (SKADI-T-0331 verification).
            if let Ok(h) = api::item_history(&acq.get_value(), 50).await {
                let _ = rows.try_set(h);
            }
            let _ = loaded.try_set(true);
        });
    };
    Effect::new(move |_| reload());

    let on_blocklist = Callback::new(move |id: String| {
        note.set(Some(
            "blocklisting + searching for a replacement…".to_string(),
        ));
        spawn_local(async move {
            let r = api::blocklist_and_search(&id).await;
            let _ = note.try_set(Some(match r {
                Ok(()) => "blocklisted — re-searching".to_string(),
                Err(e) => format!("failed: {e}"),
            }));
            reload();
        });
    });

    let body = move || {
        let rs = rows.get();
        if rs.is_empty() {
            let msg = if loaded.get() {
                "No history yet for this item."
            } else {
                "Loading…"
            };
            return view! { <p class="muted">{msg}</p> }.into_any();
        }
        rs.into_iter()
            .map(|r| {
                let cls = match r.event.as_str() {
                    "imported" => "ok",
                    "grabbed" => "pending",
                    "failed" => "bad",
                    _ => "muted",
                };
                let detail = r
                    .detail
                    .clone()
                    .or_else(|| r.reason_code.clone())
                    .unwrap_or_default();
                let can_block = matches!(r.event.as_str(), "grabbed" | "imported");
                let id = r.id.clone();
                let when = r.at.get(..16).unwrap_or(&r.at).replace('T', " ");
                view! {
                    <div class="hist-row">
                        <span class=format!("hist-dot {cls}")></span>
                        <span class="hist-event">{r.event.clone()}</span>
                        <span class="hist-label">{r.label.clone()}</span>
                        <span class="hist-detail mono">{detail}</span>
                        <span class="hist-at mono">{when}</span>
                        {can_block
                            .then(move || {
                                view! {
                                    <button
                                        class="hist-bl"
                                        title="Blocklist this release and search for another"
                                        on:click=move |_| on_blocklist.run(id.clone())
                                    >
                                        "⊘ replace"
                                    </button>
                                }
                            })}
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        <div class="history-panel">
            <button class="hist-head" on:click=move |_| collapsed.update(|c| *c = !*c)>
                <span class="cluster-chevron">
                    {move || if collapsed.get() { "▸" } else { "▾" }}
                </span>
                <h4>"History"</h4>
                {move || {
                    let n = rows.get().len();
                    (n > 0).then(|| view! { <span class="chip-count mono">{n}</span> })
                }}
                {move || note.get().map(|n| view! { <span class="hist-note mono">{n}</span> })}
            </button>
            {move || {
                (!collapsed.get())
                    .then(move || view! { <div class="hist-rows">{body}</div> })
            }}
        </div>
    }
}

/// Per-item acquisition **diagnostics** (SKADI-T-0381) — the in-UI answer to
/// "this says Missing; what has skadi actually tried?".
///
/// Reads the item's own trace stream and reports the last attempt, how many
/// candidates it weighed, which gate rejected them, and what to do about it.
/// Before this, answering that meant finding the acquirable UUID and querying
/// `trace_events` by hand (2026-09-01).
///
/// Domain-agnostic like [`HistoryPanel`]: takes the opaque `acquirable` ref, so
/// movies, TV and audiobooks all reuse it. Renders nothing at all when the item
/// has no trace history — an empty diagnostic panel is just noise on the many
/// items that were imported without incident.
#[component]
pub fn DiagnosticsPanel(acquirable: String) -> impl IntoView {
    let diag = RwSignal::new(None::<crate::activity::Diagnosis>);
    let loaded = RwSignal::new(false);
    // Open/closed survives the page re-rendering this panel (SKADI-T-0596).
    let collapsed = crate::persist::persisted(format!("diag:{acquirable}:collapsed"), true);
    let acq = StoredValue::new(acquirable);

    Effect::new(move |_| {
        spawn_local(async move {
            // try_set throughout: this can land after a fast navigation away,
            // and a plain set on a disposed signal takes down the wasm app
            // (same hazard HistoryPanel documents).
            if let Ok(rows) = api::item_traces(&acq.get_value()).await {
                let _ = diag.try_set(Some(crate::activity::diagnose(&rows)));
            }
            let _ = loaded.try_set(true);
        });
    });

    // Rejection breakdown as proportional bars — which gate to loosen, at a
    // glance, without reading numbers.
    let bars = move || {
        let Some(d) = diag.get() else {
            return ().into_any();
        };
        let total = d.rejected_total();
        if total == 0 {
            return ().into_any();
        }
        d.rejected
            .iter()
            .map(|(gate, n)| {
                let pct = (*n as f32 / total as f32 * 100.0).round();
                view! {
                    <div class="diag-bar-row">
                        <span class="diag-bar-label mono">{gate.clone()}</span>
                        <span class="diag-bar-track">
                            <span class="diag-bar-fill" style=format!("width:{pct}%")></span>
                        </span>
                        <span class="diag-bar-n mono">{format!("{n} · {pct:.0}%")}</span>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    let body = move || {
        let Some(d) = diag.get() else {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        };
        let when = d
            .last_attempt
            .as_deref()
            .map(|a| a.get(..16).unwrap_or(a).replace('T', " "))
            .unwrap_or_else(|| "—".into());
        let chosen = d.chosen.clone();
        let relevance = d.chosen_relevance;
        let considered = d.considered;
        let failed = d.failed_attempts;
        let remedy = d.remedy;
        view! {
            <div class="diag-facts">
                <div class="diag-fact">
                    <span class="diag-k">"Last attempt"</span>
                    <span class="diag-v mono">{when}</span>
                </div>
                <div class="diag-fact">
                    <span class="diag-k">"Candidates weighed"</span>
                    <span class="diag-v mono">{considered.to_string()}</span>
                </div>
                {(failed > 0).then(|| view! {
                    <div class="diag-fact">
                        <span class="diag-k">"Attempts with no grab"</span>
                        <span class="diag-v mono">{failed.to_string()}</span>
                    </div>
                })}
                {chosen.map(|t| view! {
                    <div class="diag-fact">
                        <span class="diag-k">"Chose"</span>
                        <span class="diag-v mono">{t}</span>
                    </div>
                })}
                {relevance.map(|r| {
                    let weak = r < 0.8;
                    let cls = if weak { "diag-v mono warn" } else { "diag-v mono" };
                    let text = if weak {
                        format!("{r:.2} — weak, check this is the right title")
                    } else {
                        format!("{r:.2}")
                    };
                    view! {
                        <div class="diag-fact">
                            <span class="diag-k">"Title match"</span>
                            <span class=cls>{text}</span>
                        </div>
                    }
                })}
            </div>
            <div class="diag-bars">{bars}</div>
            {remedy.map(|r| view! { <p class="notice">{r}</p> })}
        }
        .into_any()
    };

    move || {
        // Nothing traced (or nothing useful) ⇒ render nothing.
        let show = loaded.get() && diag.get().is_some_and(|d| !d.is_empty());
        show.then(|| {
            view! {
                <div class="history-panel">
                    <button class="hist-head" on:click=move |_| collapsed.update(|c| *c = !*c)>
                        <span class="cluster-chevron">
                            {move || if collapsed.get() { "▸" } else { "▾" }}
                        </span>
                        <h4>"Why isn't this grabbed?"</h4>
                        {move || {
                            diag.get()
                                .and_then(|d| d.top_gate().map(|(g, n, _)| format!("{n} {g}")))
                                .map(|s| view! { <span class="chip-count mono">{s}</span> })
                        }}
                    </button>
                    {move || (!collapsed.get()).then(body)}
                </div>
            }
        })
    }
}

/// Interactive search + grab panel for one acquirable (SKADI-T-0117). Collapsed
/// until "Search releases" runs `GET …/releases`; then shows a table of scored
/// candidates with **Grab** and **Block/Unblock** per row. Reuses the shared
/// `evaluate` verdict (a blocklisted candidate shows as rejected, with Unblock).
///
/// Shared by all three domains since SKADI-T-0559. It was movie-specific and
/// audiobooks had already copied it wholesale (`BookReleasesPanel`, whose doc
/// comment said "mirrors the movies ReleasesPanel"); television would have made
/// three copies of the same 130 lines. Now that the manual-acquire routes are
/// identical across domains (SKADI-T-0558), one `AcquirablePath` is the only
/// thing that differs, so the copies collapse into this.
///
/// `acquirable` is the id the blocklist keys on, which is not derivable from the
/// path — it is the edition / episode / file id, not the parent's.
#[component]
pub fn ReleasesPanel(
    at: api::AcquirablePath,
    acquirable: String,
    reload: Callback<()>,
) -> impl IntoView {
    // Everything worth keeping survives the page re-rendering this panel
    // (SKADI-T-0596): the detail pages poll every few seconds and rebuilt the
    // whole item body each tick, which used to blank the search results, the
    // pasted link and the "grab manually" disclosure.
    let k = |what: &str| format!("releases:{acquirable}:{what}");
    let rows = crate::persist::persisted(k("rows"), Vec::<api::ReleaseCandidate>::new());
    let searched = crate::persist::persisted(k("searched"), false);
    let searching = RwSignal::new(false);
    let err = RwSignal::new(None::<String>);
    let note = crate::persist::persisted(k("note"), None::<Result<String, String>>);
    // Manual acquisition (SKADI-I-0043): paste a magnet or .torrent URL.
    let link = crate::persist::persisted(k("link"), String::new());
    let manual_open = crate::persist::persisted(k("manual_open"), false);

    // Owned, shared (Copy) into every action closure.
    let ids = StoredValue::new((at, acquirable));

    let run_search = move || {
        let (at, _) = ids.get_value();
        searching.set(true);
        err.set(None);
        spawn_local(async move {
            match api::list_releases(&at).await {
                Ok(r) => {
                    rows.set(r);
                    searched.set(true);
                }
                Err(e) => err.set(Some(e.to_string())),
            }
            searching.set(false);
        });
    };
    let on_search = move |_| run_search();

    // Actions, hoisted to Callbacks so the presentational `ReleasesTable` stays
    // pure (data + callbacks) and unit-mountable (SKADI-T-0119).
    let on_grab = Callback::new(move |release: Value| {
        let (at, _) = ids.get_value();
        note.set(Some(Ok("grabbing…".into())));
        spawn_local(async move {
            let r = api::grab_release(&at, &release).await;
            note.set(Some(
                r.map(|()| "grab started".into()).map_err(|e| e.to_string()),
            ));
            reload.run(());
        });
    });
    let on_block = Callback::new(move |(rk, title): (String, String)| {
        let (_, acquirable) = ids.get_value();
        spawn_local(async move {
            let r = api::block_release(&rk, &title, &acquirable).await;
            note.set(Some(
                r.map(|()| "blocked".into()).map_err(|e| e.to_string()),
            ));
            run_search();
        });
    });
    let on_unblock = Callback::new(move |rk: String| {
        spawn_local(async move {
            match api::list_blocklist(None).await {
                Ok(entries) => match entries.into_iter().find(|e| e.release_key == rk) {
                    Some(e) => note.set(Some(
                        api::unblock_release(&e.id)
                            .await
                            .map(|()| "unblocked".into())
                            .map_err(|e| e.to_string()),
                    )),
                    None => note.set(Some(Ok("already unblocked".into()))),
                },
                Err(e) => note.set(Some(Err(e.to_string()))),
            }
            run_search();
        });
    });

    // Manual acquisition: synthesize a release from the pasted magnet or .torrent
    // URL + grab it through the same pipeline (SKADI-I-0043).
    let on_grab_link = move |_| {
        let (at, _) = ids.get_value();
        let l = link.get_untracked().trim().to_string();
        if l.is_empty() {
            return;
        }
        note.set(Some(Ok("grabbing…".into())));
        spawn_local(async move {
            let r = api::grab_link(&at, &l, None).await;
            note.set(Some(
                r.map(|()| "grab started".into()).map_err(|e| e.to_string()),
            ));
            link.set(String::new());
            reload.run(());
        });
    };

    view! {
        <div class="releases">
            <div class="releases-head">
                <button class="secondary" on:click=on_search disabled=move || searching.get()>
                    {move || if searching.get() { "Searching…" } else { "Search releases" }}
                </button>
                {move || note.get().map(|s| match s {
                    Ok(m) => view! { <span class="ok">{m}</span> }.into_any(),
                    Err(m) => view! { <span class="bad">{m}</span> }.into_any(),
                })}
            </div>
            <details
                class="manual-grab"
                prop:open=move || manual_open.get()
                on:toggle=move |ev| {
                    let open = event_target::<web_sys::HtmlDetailsElement>(&ev).open();
                    if manual_open.get_untracked() != open {
                        manual_open.set(open);
                    }
                }
            >
                <summary>"Grab manually with a magnet or .torrent URL"</summary>
            <div class="releases-magnet">
                <input
                    type="text"
                    placeholder="magnet:?xt=… or https://…/release.torrent"
                    prop:value=move || link.get()
                    on:input=move |ev| link.set(event_target_value(&ev))
                />
                <button
                    class="secondary"
                    on:click=on_grab_link
                    disabled=move || link.get().trim().is_empty()
                >
                    "Grab"
                </button>
            </div>
            </details>
            {move || err.get().map(|e| view! { <p class="bad">{e}</p> })}
            {move || (!rows.get().is_empty()).then(|| view! {
                <ReleasesTable
                    candidates=rows.get()
                    on_grab=on_grab
                    on_block=on_block
                    on_unblock=on_unblock
                />
            })}
            {move || (rows.get().is_empty() && searched.get()).then(|| view! {
                <p class="muted">"No releases found."</p>
            })}
        </div>
    }
}

/// Presentational table of release candidates (SKADI-T-0117/T-0119): pure render
/// of `candidates` with **Grab** + **Block** per accepted row and **Unblock**
/// per blocklisted row. Actions are delegated via callbacks so the table has no
/// network/state of its own and can be unit-mounted in tests.
#[component]
pub fn ReleasesTable(
    candidates: Vec<api::ReleaseCandidate>,
    /// Grab the release (its raw object is echoed back).
    on_grab: Callback<Value>,
    /// Block a release by `(release_key, title)`.
    on_block: Callback<(String, String)>,
    /// Unblock a release by `release_key`.
    on_unblock: Callback<String>,
) -> impl IntoView {
    let rows = candidates
        .into_iter()
        .map(|c| {
            let title = c.title();
            let size = size_human(c.size_bytes());
            let seeders = c
                .seeders()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into());
            let age = format!("{}d", c.age_days.max(0));
            let quality = c.quality.clone();
            let blocked = c.blocklisted();
            let verdict_cls = if c.accepted {
                "ok"
            } else if blocked {
                "muted"
            } else {
                "bad"
            };
            let verdict = if c.accepted {
                quality.clone()
            } else {
                c.reason.clone()
            };

            let action = if blocked {
                let rk = c.release_key.clone();
                view! {
                    <button class="secondary" on:click=move |_| on_unblock.run(rk.clone())>
                        "Unblock"
                    </button>
                }
                .into_any()
            } else {
                let rel = c.release.clone();
                let rk = c.release_key.clone();
                let t = title.clone();
                // A season pack satisfies every episode in the season, not the
                // row it was clicked from (SKADI-T-0559). Said *before* the
                // click and confirmed on it: an operator who asks for one
                // episode and gets twelve reads that as a bug, even though it is
                // the correct and often the only available option.
                let is_pack = c.season_pack;
                let confirm_title = title.clone();
                view! {
                    <button on:click=move |_| {
                        if is_pack
                            && !crate::settings::window_confirm(&format!(
                                "{confirm_title} is a season pack — grabbing it will \
                                 satisfy every episode in the season, not just this \
                                 one. Continue?"
                            ))
                        {
                            return;
                        }
                        on_grab.run(rel.clone());
                    }>
                        {if is_pack { "Grab pack" } else { "Grab" }}
                    </button>
                    <button
                        class="secondary"
                        on:click=move |_| on_block.run((rk.clone(), t.clone()))
                    >
                        "Block"
                    </button>
                }
                .into_any()
            };

            view! {
                <tr>
                    <td>{title}</td>
                    <td>{quality}</td>
                    <td>{size}</td>
                    <td>{seeders}</td>
                    <td>{age}</td>
                    <td class=verdict_cls>{verdict}</td>
                    <td class="row-actions">{action}</td>
                </tr>
            }
        })
        .collect_view();

    view! {
        <table class="releases-table">
            <thead>
                <tr>
                    <th>"Title"</th><th>"Quality"</th><th>"Size"</th>
                    <th>"Seeders"</th><th>"Age"</th><th>"Verdict"</th><th></th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </table>
    }
}

/// Human-readable byte size (binary units). `0` renders as a dash.
pub fn size_human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "—".into();
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Movies-domain configuration (SKADI-T-0071): movies-specific settings that
/// don't belong in system Config — the quality profile and the edition-kinds
/// registry. Reached via the gear on the Movies library page.
#[component]
pub fn MoviesConfigPage() -> impl IntoView {
    view! {
        <crate::subnav::SubNav/>
        <div class="page-head">
            <h2>"Movies · Config"</h2>
            <A href="/movies">"← Library"</A>
        </div>
        <ProfileSection/>
        <EditionKindsSection/>
    }
}

/// Name of the profile the daemon will actually enforce, given `rows` in the
/// order `list_settings("profiles")` returns them (`id ASC`).
///
/// Mirrors `resolve_active_profile` in `skadi-movies`/`skadi-tv`: **the first
/// row wins**, so the enforced profile is whichever holds the lowest UUID —
/// never an operator choice. With no rows the daemon falls back to its built-in
/// default. Extracted pure so the surprising rule is pinned by a test
/// (SKADI-T-0379).
#[must_use]
pub fn active_profile_name(rows: &[api::Setting]) -> String {
    match rows.first() {
        None => "built-in default".into(),
        Some(r) => r
            .body
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("(unnamed)")
            .to_string(),
    }
}

/// Whether the profile `id` is the one the daemon enforces — i.e. it is the
/// first row. `None` (an unsaved, brand-new profile) is never active.
#[must_use]
pub fn profile_is_active(rows: &[api::Setting], id: Option<&str>) -> bool {
    match (id, rows.first()) {
        (Some(id), Some(first)) => first.id == id,
        _ => false,
    }
}

/// The quality profile that gates/ranks movie acquisitions (domain-owned for
/// now). Allowed qualities low→high; a release above the cutoff isn't upgraded;
/// anything not allowed is rejected. With none set, all qualities are accepted.
///
/// The `profiles` setting is **global, not per-domain**: `skadi-movies` and
/// `skadi-tv` both resolve the first row of `list_settings("profiles")`, so the
/// profile edited here gates TV acquisitions too. The UI says so (SKADI-T-0379)
/// rather than implying a movies-only scope.
#[component]
fn ProfileSection() -> impl IntoView {
    let defs = RwSignal::new(Vec::<api::QualityDef>::new());
    // Every stored profile (for the picker) + the one currently being edited.
    // `profile_id == None` means the form is composing a brand-new profile.
    let profiles = RwSignal::new(Vec::<api::Setting>::new());
    let profile_id = RwSignal::new(None::<String>);
    let name = RwSignal::new(String::new());
    let allowed = RwSignal::new(HashSet::<String>::new());
    let cutoff = RwSignal::new(String::new());
    let upgrade = RwSignal::new(false);
    let status = RwSignal::new(None::<Result<String, String>>);
    let busy = RwSignal::new(false);

    // Load a profile row into the form, or clear it for a new one (`None`).
    let apply = move |row: Option<api::Setting>| match row {
        Some(r) => {
            profile_id.set(Some(r.id.clone()));
            let b = &r.body;
            name.set(
                b.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            allowed.set(
                b.get("allowed")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default(),
            );
            cutoff.set(
                b.get("cutoff")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            upgrade.set(
                b.get("upgrade_allowed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            );
        }
        None => {
            profile_id.set(None);
            name.set(String::new());
            allowed.set(HashSet::new());
            cutoff.set(String::new());
            upgrade.set(false);
        }
    };

    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(d) = api::list_quality_definitions().await {
                defs.set(d);
            }
            match api::list_settings("profiles").await {
                Ok(rows) => {
                    profiles.set(rows.clone());
                    // Keep the current selection if it still exists, else first.
                    let keep = profile_id.get_untracked();
                    let row = keep
                        .as_deref()
                        .and_then(|id| rows.iter().find(|p| p.id == id).cloned())
                        .or_else(|| rows.into_iter().next());
                    apply(row);
                }
                Err(e) => status.set(Some(Err(e.to_string()))),
            }
        });
    });

    let allowed_in_order = move || {
        defs.get()
            .into_iter()
            .filter(|d| allowed.with(|a| a.contains(&d.id)))
            .collect::<Vec<_>>()
    };

    // Which profile the daemon *actually* enforces (SKADI-T-0379). It resolves
    // the first row of `list_settings("profiles")`, and the store returns those
    // ordered by `id ASC` — so the winner is whichever profile happens to hold
    // the lowest UUID, never an operator choice. A stale SD-only profile
    // sorting first silently rejected every HD release (2026-09-01); surfacing
    // the resolution is the whole point of this banner.
    let active_name = move || profiles.with(|rows| active_profile_name(rows));
    let editing_is_active =
        move || profiles.with(|rows| profile_is_active(rows, profile_id.get().as_deref()));
    let quality_name_of = move |id: &str| {
        defs.get()
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| d.name)
            .unwrap_or_else(|| "the cutoff".into())
    };
    let cutoff_name = move || quality_name_of(&cutoff.get());

    let def_rows = move || {
        defs.get()
            .into_iter()
            .map(|d| {
                let id1 = d.id.clone();
                let id2 = d.id.clone();
                let checked = move || allowed.with(|a| a.contains(&id1));
                let toggle = move |_| {
                    allowed.update(|a| {
                        if a.contains(&id2) {
                            a.remove(&id2);
                        } else {
                            a.insert(id2.clone());
                        }
                    });
                };
                view! {
                    <label class="check">
                        <input type="checkbox" prop:checked=checked on:change=toggle/>
                        {d.name.clone()}
                    </label>
                }
            })
            .collect_view()
    };

    let cutoff_options = move || {
        allowed_in_order()
            .into_iter()
            .map(|d| {
                let id = d.id.clone();
                let sel = move || cutoff.with(|c| *c == id);
                view! { <option value=d.id.clone() selected=sel>{d.name.clone()}</option> }
            })
            .collect_view()
    };

    let save = move |_| {
        let allowed_ids: Vec<String> = allowed_in_order().into_iter().map(|d| d.id).collect();
        if allowed_ids.is_empty() {
            status.set(Some(Err("select at least one quality".into())));
            return;
        }
        let cut = cutoff.get_untracked();
        let cut = if allowed_ids.contains(&cut) {
            cut
        } else {
            allowed_ids.last().cloned().unwrap()
        };
        let body = json!({
            "name": name.get_untracked(),
            "allowed": allowed_ids,
            "cutoff": cut,
            "upgrade_allowed": upgrade.get_untracked(),
            "min_format_score": 0,
        });
        let id = profile_id.get_untracked();
        busy.set(true);
        status.set(None);
        spawn_local(async move {
            let result = match &id {
                Some(id) => api::update_setting("profiles", id, &body).await,
                None => api::create_setting("profiles", &body).await,
            };
            busy.set(false);
            match result {
                Ok(s) => {
                    profile_id.set(Some(s.id));
                    // Refresh the picker so a new profile appears / a rename shows.
                    if let Ok(rows) = api::list_settings("profiles").await {
                        profiles.set(rows);
                    }
                    status.set(Some(Ok(
                        "saved — the daemon will enforce it within ~5s".into()
                    )));
                }
                Err(e) => status.set(Some(Err(e.to_string()))),
            }
        });
    };

    let delete = move |_| {
        let Some(id) = profile_id.get_untracked() else {
            return;
        };
        if !window_confirm("Delete this profile?") {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let _ = api::delete_setting("profiles", &id).await;
            busy.set(false);
            // Reload the list and select the first remaining profile (or a new one).
            match api::list_settings("profiles").await {
                Ok(rows) => {
                    profiles.set(rows.clone());
                    apply(rows.into_iter().next());
                }
                Err(_) => apply(None),
            }
            status.set(Some(Ok("deleted".into())));
        });
    };

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Quality profile"</h3>
                    <p class="muted">"Allowed qualities (low→high); a release above the cutoff isn't upgraded; anything not allowed is rejected. With none set, all qualities are accepted."</p>
                </div>
            </div>
            <div class="active-profile">
                <span class="tag">"active"</span>
                <strong>{active_name}</strong>
                <span class="muted">
                    "— enforced for movies "
                    <em>"and TV"</em>
                    ". The daemon resolves the first profile by internal ID, so other saved profiles are stored but never applied."
                </span>
            </div>
            <div class="form">
                <div class="field">
                    <label>"Profile"</label>
                    <select
                        prop:value=move || profile_id.get().unwrap_or_default()
                        on:change=move |ev| {
                            let v = event_target_value(&ev);
                            if v.is_empty() {
                                apply(None);
                            } else {
                                let row = profiles.with(|ps| ps.iter().find(|p| p.id == v).cloned());
                                apply(row);
                            }
                            status.set(None);
                        }
                    >
                        <option value="">"+ New profile"</option>
                        {move || profiles.get().into_iter().map(|p| {
                            let pname = p.body.get("name").and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)").to_string();
                            view! { <option value=p.id.clone()>{pname}</option> }
                        }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <label>"Name"</label>
                    <input
                        type="text"
                        placeholder="1080p"
                        prop:value=move || name.get()
                        on:input=move |ev| name.set(event_target_value(&ev))
                    />
                </div>
                <div class="field">
                    <label>"Allowed qualities"</label>
                    <div class="checks">{def_rows}</div>
                </div>
                <div class="field">
                    <label>"Upgrade cutoff"</label>
                    <select on:change=move |ev| cutoff.set(event_target_value(&ev))>
                        {cutoff_options}
                    </select>
                </div>
                <div class="field">
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=move || upgrade.get()
                            on:change=move |ev| upgrade.set(event_target_checked(&ev))
                        />
                        "Allow upgrades (re-grab a better release until the cutoff)"
                    </label>
                </div>
                // Upgrades re-acquire files you already hold — the behaviour that
                // reads as "it's re-grabbing stuff I already have" when it isn't
                // spelled out (SKADI-T-0379).
                {move || upgrade.get().then(|| view! {
                    <p class="notice warn">
                        "Upgrades enabled — imports below "
                        <strong>{cutoff_name()}</strong>
                        " will be re-downloaded when a better release shows up."
                    </p>
                })}
                // Editing a profile the daemon will never read is silent effort;
                // say so rather than letting a save look like it took effect.
                {move || (profile_id.get().is_some() && !editing_is_active()).then(|| view! {
                    <p class="notice">
                        "You're editing an inactive profile. "
                        <strong>{active_name()}</strong>
                        " is the one being enforced."
                    </p>
                })}
                {move || status.get().map(|s| match s {
                    Ok(m) => view! { <p class="ok">{m}</p> }.into_any(),
                    Err(m) => view! { <p class="bad">{m}</p> }.into_any(),
                })}
                <div class="form-actions">
                    <button on:click=save disabled=move || busy.get()>
                        {move || if busy.get() {
                            "Saving…"
                        } else if profile_id.get().is_none() {
                            "Create profile"
                        } else {
                            "Save profile"
                        }}
                    </button>
                    {move || profile_id.get().map(|_| view! {
                        <button class="danger" on:click=delete>"Delete"</button>
                    })}
                </div>
            </div>
        </section>
    }
}

/// The edition-kinds registry the MovieMatcher consults (moved here from the
/// old system Config page — it's movies-specific). Built-ins can be edited but
/// not deleted.
#[component]
fn EditionKindsSection() -> impl IntoView {
    let items = RwSignal::new(Vec::<api::EditionKind>::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // Editing target: None = closed, Some("") = adding, Some(id) = editing.
    let editing = RwSignal::new(None::<String>);
    let f_name = RwSignal::new(String::new());
    let f_tag = RwSignal::new(String::new());
    let f_patterns = RwSignal::new(String::new());

    let refresh = move || {
        spawn_local(async move {
            match api::list_edition_kinds().await {
                Ok(list) => {
                    items.set(list);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };
    Effect::new(move |_| refresh());

    let open_add = move |_| {
        f_name.set(String::new());
        f_tag.set(String::new());
        f_patterns.set(String::new());
        editing.set(Some(String::new()));
    };

    let submit = move |_| {
        let patterns: Vec<Value> = f_patterns
            .get_untracked()
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_string()))
            .collect();
        let body = json!({
            "name": f_name.get_untracked(),
            "normalized_tag": f_tag.get_untracked(),
            "match_patterns": patterns,
        });
        let target = editing.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let r = match target.as_deref() {
                Some("") | None => api::create_edition_kind(&body).await,
                Some(id) => api::update_edition_kind(id, &body).await,
            };
            busy.set(false);
            match r {
                Ok(()) => {
                    editing.set(None);
                    refresh();
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };

    let rows = move || {
        items
            .get()
            .into_iter()
            .map(|k| {
                let patterns = k.match_patterns.join(", ");
                let edit_id = k.id.clone();
                let e_name = k.name.clone();
                let e_tag = k.normalized_tag.clone();
                let e_pat = patterns.clone();
                let on_edit = move |_| {
                    f_name.set(e_name.clone());
                    f_tag.set(e_tag.clone());
                    f_patterns.set(e_pat.clone());
                    editing.set(Some(edit_id.clone()));
                };
                let del_id = k.id.clone();
                let del_name = k.name.clone();
                let on_del = move |_| {
                    let del_id = del_id.clone();
                    if !window_confirm(&format!("Delete edition kind \"{del_name}\"?")) {
                        return;
                    }
                    busy.set(true);
                    spawn_local(async move {
                        let _ = api::delete_edition_kind(&del_id).await;
                        busy.set(false);
                        refresh();
                    });
                };
                let builtin = k.builtin;
                view! {
                    <div class="card">
                        <div class="card-main">
                            <strong>{k.name.clone()}</strong>
                            <span class="muted summary">{patterns}</span>
                            {builtin.then(|| view! { <span class="badge">"built-in"</span> })}
                        </div>
                        <div class="card-actions">
                            <button on:click=on_edit>"Edit"</button>
                            <button class="danger" on:click=on_del disabled=builtin>"Delete"</button>
                        </div>
                    </div>
                }
            })
            .collect_view()
    };

    let form_panel = move || {
        if editing.get().is_none() {
            return ().into_any();
        }
        view! {
            <div class="form">
                <div class="field">
                    <label>"Name"</label>
                    <input type="text" placeholder="Director's Cut"
                        prop:value=move || f_name.get()
                        on:input=move |ev| f_name.set(event_target_value(&ev))/>
                </div>
                <div class="field">
                    <label>"Normalized tag (filesystem-safe)"</label>
                    <input type="text" placeholder="Directors Cut"
                        prop:value=move || f_tag.get()
                        on:input=move |ev| f_tag.set(event_target_value(&ev))/>
                </div>
                <div class="field">
                    <label>"Match patterns (comma-separated)"</label>
                    <input type="text" placeholder="director, dc"
                        prop:value=move || f_patterns.get()
                        on:input=move |ev| f_patterns.set(event_target_value(&ev))/>
                </div>
                <div class="form-actions">
                    <button on:click=submit disabled=move || busy.get()>
                        {move || if busy.get() { "Saving…" } else { "Save" }}
                    </button>
                    <button class="secondary" on:click=move |_| editing.set(None)>"Cancel"</button>
                </div>
            </div>
        }
        .into_any()
    };

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Edition kinds"</h3>
                    <p class="muted">"The registry the matcher uses to map a release's edition tag (Director's Cut, IMAX, …). Built-ins can be edited but not deleted."</p>
                </div>
                <button on:click=open_add>"+ Add"</button>
            </div>
            {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
            <div class="cards">{rows}</div>
            {form_panel}
        </section>
    }
}

/// `window.confirm` bridge for the delete guard.
fn window_confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.confirm_with_message(message).ok())
        .unwrap_or(false)
}
