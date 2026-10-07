//! Wanted (SKADI-T-0594): one list of everything the hunter is still searching
//! for — every monitored series episode, movie edition and audiobook file
//! without a good file — so the operator can prune it, run a manual search,
//! grab a specific release, or paste a magnet / `.torrent` link straight onto
//! the item. The per-domain detail pages already had all four actions; this is
//! the cross-domain view of the backlog that they lacked.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api::{self, AcquirablePath, WantedEdition, WantedItem};
use crate::confirm::{ConfirmSpec, confirm};
use crate::movies::{ReleasesPanel, status_class};

/// Edition chips shown per item before the "+N more" toggle.
const CHIP_CAP: usize = 12;

/// What one edition chip says: the episode code for TV, the edition kind for
/// a film, the file kind for a book.
#[must_use]
pub fn edition_label(item_kind: &str, e: &WantedEdition) -> String {
    match item_kind {
        "series" => e.kind.clone(),
        "movie" => e.kind_name.clone().unwrap_or_else(|| "Edition".into()),
        _ => e.kind_name.clone().unwrap_or_else(|| "Audiobook".into()),
    }
}

/// The item's own detail page.
#[must_use]
pub fn detail_href(kind: &str, id: &str) -> String {
    match kind {
        "series" => format!("/tv/{id}"),
        "movie" => format!("/movies/{id}"),
        _ => format!("/audiobooks/{id}"),
    }
}

/// The acquirable the search / grab / link actions address.
#[must_use]
pub fn acquirable_path(kind: &str, item_id: &str, edition_id: &str) -> AcquirablePath {
    match kind {
        "series" => AcquirablePath::tv_episode(item_id, edition_id),
        "movie" => AcquirablePath::movie_edition(item_id, edition_id),
        _ => AcquirablePath::book_file(item_id, edition_id),
    }
}

/// Human label for a kind chip.
#[must_use]
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "series" => "TV",
        "movie" => "Movies",
        "audiobook" => "Audiobooks",
        _ => "All",
    }
}

/// The items the filters allow, each trimmed to the editions the status filter
/// allows; sorted by title. `kind`/`status` of `all` mean no filter; `q` is a
/// case-insensitive title substring.
#[must_use]
pub fn filter_items(items: &[WantedItem], kind: &str, status: &str, q: &str) -> Vec<WantedItem> {
    let q = q.trim().to_lowercase();
    let mut out: Vec<WantedItem> = items
        .iter()
        .filter(|i| kind == "all" || i.kind == kind)
        .filter(|i| q.is_empty() || i.title.to_lowercase().contains(&q))
        .filter_map(|i| {
            let editions: Vec<WantedEdition> = i
                .editions
                .iter()
                .filter(|e| status == "all" || e.status_kind == status)
                .cloned()
                .collect();
            (!editions.is_empty()).then(|| WantedItem {
                editions,
                ..i.clone()
            })
        })
        .collect();
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Whether a TV edition is a special (`S00Exx`).
#[must_use]
pub fn is_special(e: &WantedEdition) -> bool {
    e.kind.starts_with("S00")
}

/// Editions in display order: regular seasons first, specials last, so a
/// series wanting a pile of `S00` extras does not bury its real episodes
/// behind the "+N more" toggle.
#[must_use]
pub fn ordered_editions(item: &WantedItem) -> Vec<WantedEdition> {
    let mut eds = item.editions.clone();
    if item.kind == "series" {
        eds.sort_by_key(is_special);
    }
    eds
}

/// Ids of the series whose wanted list includes specials — the bulk-prune set.
#[must_use]
pub fn series_with_specials(items: &[WantedItem]) -> Vec<String> {
    items
        .iter()
        .filter(|i| i.kind == "series" && i.editions.iter().any(is_special))
        .map(|i| i.id.clone())
        .collect()
}

/// Item counts per kind `(series, movie, audiobook)` for the filter chips.
#[must_use]
pub fn kind_counts(items: &[WantedItem]) -> (usize, usize, usize) {
    items
        .iter()
        .fold((0, 0, 0), |(s, m, a), i| match i.kind.as_str() {
            "series" => (s + 1, m, a),
            "movie" => (s, m + 1, a),
            _ => (s, m, a + 1),
        })
}

#[component]
pub fn WantedPage() -> impl IntoView {
    let all = RwSignal::new(Vec::<WantedItem>::new());
    let summary = RwSignal::new(api::WantedSummary::default());
    let kind = RwSignal::new("all".to_string());
    let status = RwSignal::new("all".to_string());
    let q = RwSignal::new(String::new());
    let expanded = RwSignal::new(None::<(String, String)>);
    let show_all = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    let loading = RwSignal::new(true);
    let note = RwSignal::new(None::<Result<String, String>>);

    let load = move || {
        loading.set(true);
        spawn_local(async move {
            match api::wanted().await {
                Ok(r) => {
                    summary.set(r.summary);
                    all.set(r.items);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
            loading.set(false);
        });
    };
    Effect::new(move |_| load());
    let reload = Callback::new(move |()| load());

    let filtered = move || filter_items(&all.get(), &kind.get(), &status.get(), &q.get());

    let kind_chip = move |value: &'static str, count: Signal<usize>| {
        view! {
            <button
                class=move || if kind.get() == value { "filter-chip active" } else { "filter-chip" }
                on:click=move |_| kind.set(value.to_string())
            >
                {kind_label(value)}
                <span class="chip-count mono">{move || count.get().to_string()}</span>
            </button>
        }
    };
    let status_chip = move |value: &'static str, label: &'static str| {
        let count = Signal::derive(move || {
            if value == "all" {
                summary.get().editions
            } else {
                summary.get().by_status.get(value).copied().unwrap_or(0)
            }
        });
        view! {
            <button
                class=move || if status.get() == value { "filter-chip active" } else { "filter-chip" }
                on:click=move |_| status.set(value.to_string())
            >
                {label}
                <span class="chip-count mono">{move || count.get().to_string()}</span>
            </button>
        }
    };
    let counts = Signal::derive(move || kind_counts(&all.get()));
    let n_all = Signal::derive(move || all.get().len());
    let n_series = Signal::derive(move || counts.get().0);
    let n_movie = Signal::derive(move || counts.get().1);
    let n_book = Signal::derive(move || counts.get().2);

    // Prune: unmonitor the whole item (it leaves the backlog on reload).
    let unmonitor_item = move |item_kind: String, id: String| {
        note.set(Some(Ok("updating…".into())));
        spawn_local(async move {
            let r = match item_kind.as_str() {
                "series" => api::set_series_monitored(&id, false).await,
                "movie" => api::set_movie_monitored(&id, false).await,
                _ => api::set_book_monitored(&id, false).await,
            };
            note.set(Some(
                r.map(|()| "unmonitored".into()).map_err(|e| e.to_string()),
            ));
            load();
        });
    };
    // Prune a series' specials in one go: they are most of the TV backlog and
    // rarely wanted (12 Monkeys alone wanted seventeen S00 episodes).
    let unmonitor_specials = move |series_id: String| {
        note.set(Some(Ok("updating…".into())));
        spawn_local(async move {
            let r = api::monitor_season(&series_id, 0, false).await;
            note.set(Some(
                r.map(|()| "specials unmonitored".into())
                    .map_err(|e| e.to_string()),
            ));
            load();
        });
    };
    // Bulk prune (SKADI-T-0594): specials were 82% of the TV backlog on prod
    // (4,480 of 5,408 files; 213 of 267 series wanted nothing else). Asked
    // first (the ConfirmDialog, SKADI-T-0694) so a stray click cannot
    // unmonitor two hundred seasons.
    let bulk_busy = RwSignal::new(false);
    // Progress + result shown inline next to the button: the first cut put the
    // note up in the page header, and the run read as "nothing happened".
    let bulk_note = RwSignal::new(None::<Result<String, String>>);
    let unmonitor_all_specials = move || {
        let ids = series_with_specials(&all.get_untracked());
        let n = ids.len();
        spawn_local(async move {
            let spec = ConfirmSpec::destructive(
                "Unmonitor all specials?",
                format!("This unmonitors season 0 on {n} series."),
            )
            .confirm_label("Unmonitor all specials");
            if !confirm(spec).await || bulk_busy.get_untracked() {
                return;
            }
            bulk_busy.set(true);
            bulk_note.set(Some(Ok(format!("0 of {n} done…"))));
            let mut failed = 0usize;
            for (i, id) in ids.into_iter().enumerate() {
                if api::monitor_season(&id, 0, false).await.is_err() {
                    failed += 1;
                }
                bulk_note.set(Some(Ok(format!("{} of {n} done…", i + 1))));
            }
            bulk_note.set(Some(if failed == 0 {
                Ok(format!("done — specials unmonitored on {n} series"))
            } else {
                Err(format!("{failed} of {n} series failed to update"))
            }));
            bulk_busy.set(false);
            load();
        });
    };
    // Prune one episode (TV only has per-edition monitoring).
    let unmonitor_episode = move |series_id: String, eid: String| {
        note.set(Some(Ok("updating…".into())));
        spawn_local(async move {
            let r = api::monitor_episode(&series_id, &eid, false).await;
            note.set(Some(
                r.map(|()| "episode unmonitored".into())
                    .map_err(|e| e.to_string()),
            ));
            expanded.set(None);
            load();
        });
    };
    // Manual search: run the automatic pipeline now for this one edition.
    let search_now = move |at: AcquirablePath| {
        note.set(Some(Ok("search queued…".into())));
        spawn_local(async move {
            let r = api::acquire_edition(&at).await;
            note.set(Some(
                r.map(|()| "search started".into())
                    .map_err(|e| e.to_string()),
            ));
        });
    };

    view! {
        <crate::subnav::SubNav/>
        <div class="wanted-view">
            <div class="page-head">
                <div>
                    <h2 class="page-title">"Wanted"</h2>
                    <p class="page-sub">
                        {move || {
                            let s = summary.get();
                            let failed = s.by_status.get("failed").copied().unwrap_or(0);
                            let missing = s.by_status.get("missing").copied().unwrap_or(0);
                            format!("{} items · {} files wanted · {} missing · {} failed and retrying", s.items, s.editions, missing, failed)
                        }}
                    </p>
                </div>
                <div class="page-head-actions">
                    {move || note.get().map(|s| match s {
                        Ok(m) => view! { <span class="ok">{m}</span> }.into_any(),
                        Err(m) => view! { <span class="bad">{m}</span> }.into_any(),
                    })}
                    <button class="secondary" on:click=move |_| load() disabled=move || loading.get()>
                        {move || if loading.get() { "Loading…" } else { "Refresh" }}
                    </button>
                </div>
            </div>

            <div class="filter-bar">
                <div class="filter-chips">
                    {kind_chip("all", n_all)}
                    {kind_chip("series", n_series)}
                    {kind_chip("movie", n_movie)}
                    {kind_chip("audiobook", n_book)}
                </div>
                <div class="filter-chips">
                    {status_chip("all", "Any status")}
                    {status_chip("missing", "Missing")}
                    {status_chip("failed", "Failed")}
                </div>
                <input
                    class="wanted-filter"
                    type="search"
                    placeholder="Filter titles…"
                    prop:value=move || q.get()
                    on:input=move |ev| q.set(event_target_value(&ev))
                />
            </div>
            {move || {
                let n = series_with_specials(&all.get()).len();
                let has_note = bulk_note.get().is_some();
                (n > 0 || has_note).then(|| view! {
                    <div class="wanted-bulk">
                        <span class="muted">
                            {if n > 0 {
                                format!("{n} series are waiting on specials (S00) — usually extras nobody asked for.")
                            } else {
                                "No series is waiting on specials.".to_string()
                            }}
                        </span>
                        {(n > 0).then(|| view! {
                            <button class="secondary" on:click=move |_| unmonitor_all_specials() disabled=move || bulk_busy.get()>
                                "Unmonitor all specials…"
                            </button>
                        })}
                        {move || bulk_note.get().map(|s| match s {
                            Ok(m) => view! { <span class="ok mono">{m}</span> }.into_any(),
                            Err(m) => view! { <span class="bad mono">{m}</span> }.into_any(),
                        })}
                    </div>
                })
            }}

            {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}

            <div class="activity-list wanted-list">
                {move || {
                    let items = filtered();
                    if items.is_empty() && !loading.get() {
                        return view! { <p class="muted">"Nothing wanted matches these filters."</p> }.into_any();
                    }
                    items.into_iter().map(|item| {
                        let item_kind = item.kind.clone();
                        let item_id = item.id.clone();
                        let title = item.title.clone();
                        let href = detail_href(&item.kind, &item.id);
                        let ordered = ordered_editions(&item);
                        let total = ordered.len();
                        let has_specials = item.kind == "series" && ordered.iter().any(is_special);
                        let expanded_all = show_all.get().as_deref() == Some(item.id.as_str());
                        let shown: Vec<WantedEdition> = if expanded_all {
                            ordered.clone()
                        } else {
                            ordered.iter().take(CHIP_CAP).cloned().collect()
                        };
                        let sp_id = item.id.clone();
                        let hidden = total.saturating_sub(shown.len());
                        let more_id = item.id.clone();
                        let un_kind = item_kind.clone();
                        let un_id = item_id.clone();
                        let chips = shown.into_iter().map(|e| {
                            let label = edition_label(&item_kind, &e);
                            let cls = format!("wanted-ed {}", status_class(&e.status_kind));
                            let key = (item_id.clone(), e.id.clone());
                            let key_cmp = key.clone();
                            view! {
                                <button
                                    class=move || if expanded.get().as_ref() == Some(&key_cmp) { format!("{cls} open") } else { cls.clone() }
                                    title=e.status_kind.clone()
                                    on:click=move |_| {
                                        let k = key.clone();
                                        expanded.update(|x| *x = if x.as_ref() == Some(&k) { None } else { Some(k) });
                                    }
                                >
                                    {label}
                                </button>
                            }
                        }).collect_view();
                        // The expanded edition's panel, if it belongs to this item.
                        let panel_kind = item_kind.clone();
                        let panel_item = item_id.clone();
                        let panel_eds = item.editions.clone();
                        let panel = move || {
                            let (iid, eid) = expanded.get()?;
                            if iid != panel_item {
                                return None;
                            }
                            let e = panel_eds.iter().find(|e| e.id == eid)?.clone();
                            let at = acquirable_path(&panel_kind, &panel_item, &eid);
                            let at_search = at.clone();
                            let label = edition_label(&panel_kind, &e);
                            let is_tv = panel_kind == "series";
                            let sid = panel_item.clone();
                            let eid_un = eid.clone();
                            Some(view! {
                                <div class="wanted-panel">
                                    <div class="wanted-panel-head">
                                        <strong>{label}</strong>
                                        <span class=format!("badge {}", status_class(&e.status_kind))>{e.status_kind.clone()}</span>
                                        <span class="spacer"></span>
                                        <button class="secondary" on:click=move |_| search_now(at_search.clone())>"Search now"</button>
                                        {is_tv.then(|| {
                                            let sid = sid.clone();
                                            let eid_un = eid_un.clone();
                                            view! {
                                                <button class="secondary" on:click=move |_| unmonitor_episode(sid.clone(), eid_un.clone())>"Unmonitor episode"</button>
                                            }
                                        })}
                                    </div>
                                    <ReleasesPanel at=at acquirable=eid.clone() reload=reload/>
                                </div>
                            })
                        };
                        view! {
                            <div class="wanted-item">
                                <div class="activity-row wanted-row">
                                    <span class="badge">{kind_label(&item.kind)}</span>
                                    <A href=href><strong>{title}</strong></A>
                                    {item.year.map(|y| view! { <span class="muted mono">{y.to_string()}</span> })}
                                    <span class="wanted-eds">
                                        {chips}
                                        {(hidden > 0).then(|| view! {
                                            <button class="wanted-more" on:click=move |_| show_all.set(Some(more_id.clone()))>
                                                {format!("+{hidden} more")}
                                            </button>
                                        })}
                                    </span>
                                    <span class="spacer"></span>
                                    {has_specials.then(|| view! {
                                        <button class="secondary wanted-unmonitor" on:click=move |_| unmonitor_specials(sp_id.clone())>"Unmonitor specials"</button>
                                    })}
                                    <button class="secondary wanted-unmonitor" on:click=move |_| unmonitor_item(un_kind.clone(), un_id.clone())>"Unmonitor"</button>
                                </div>
                                {panel}
                            </div>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </div>
    }
}
