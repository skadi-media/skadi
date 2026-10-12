//! Add / acquire — pick a media type, then search *that* domain
//! (SKADI-I-0035 / SKADI-T-0248). The operator knows what they're looking for, so
//! a domain dropdown (Movie / TV / Audiobook) scopes the search to one provider —
//! TMDB (movies), TheTVDB (TV), or Audible/Audnexus (audiobooks) — instead of
//! fanning out across all three. Each result row adds the item as a monitored
//! "wanted" entry through the existing `add_movie` / `add_series` / `add_book` API.

use std::collections::HashSet;

use aurora_leptos::components::{Button, Loading, PageHeader, Pill, Select, TextInput, Toaster};
use aurora_leptos::tokens::token;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

/// `/add` — choose a domain, then search it (movies / TV / audiobooks).
#[component]
pub fn AddPage() -> impl IntoView {
    let query = RwSignal::new(String::new());
    // The domain the search is scoped to: "movie" (default), "tv", or "audiobook".
    let domain = RwSignal::new("movie".to_string());
    let movie_hits = RwSignal::new(Vec::<api::MovieSearchResult>::new());
    let series_hits = RwSignal::new(Vec::<api::SeriesSearchResult>::new());
    let book_hits = RwSignal::new(Vec::<api::BookSearchResult>::new());
    let searching = RwSignal::new(false);
    let searched = RwSignal::new(false);
    let toast = RwSignal::new(None::<String>);

    // Quality profile the add needs (defaults to the first). The library root is
    // derived server-side now (SKADI-T-0302) — no root picker.
    let profiles = RwSignal::new(Vec::<api::Setting>::new());
    // Library membership: movies by (title, year) (no tmdb on the list payload),
    // books by ASIN — so a result already owned reads "In library".
    let lib_movies = RwSignal::new(HashSet::<(String, Option<u16>)>::new());
    let lib_series = RwSignal::new(HashSet::<u64>::new());
    let lib_asins = RwSignal::new(HashSet::<String>::new());

    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(p) = api::list_settings("profiles").await {
                profiles.set(p);
            }
        });
        spawn_local(async move {
            if let Ok(ms) = api::list_movies().await {
                lib_movies.set(
                    ms.into_iter()
                        .map(|m| (m.title.to_lowercase(), m.year))
                        .collect(),
                );
            }
            if let Ok(ss) = api::list_series().await {
                lib_series.set(ss.into_iter().filter_map(|s| s.external_ids.tvdb).collect());
            }
            if let Ok(bs) = api::list_books(None, None).await {
                lib_asins.set(bs.into_iter().filter_map(|b| b.external_ids.asin).collect());
            }
        });
    });

    let effective_profile = move || profiles.with(|p| p.first().map(|x| x.id.clone()));

    // Search only the selected domain — the operator already knows what they want.
    let do_search = move || {
        let q = query.get().trim().to_string();
        if q.is_empty() {
            return;
        }
        let d = domain.get();
        searching.set(true);
        spawn_local(async move {
            match d.as_str() {
                "tv" => series_hits.set(api::search_series(&q).await.unwrap_or_default()),
                "audiobook" => book_hits.set(api::search_books(&q).await.unwrap_or_default()),
                _ => movie_hits.set(api::search_movies(&q, None).await.unwrap_or_default()),
            }
            searched.set(true);
            searching.set(false);
        });
    };

    // Switching domain clears the stale results (and the "searched" flag).
    let on_domain_change = move |_: String| {
        movie_hits.set(Vec::new());
        series_hits.set(Vec::new());
        book_hits.set(Vec::new());
        searched.set(false);
    };

    let placeholder = move || match domain.get().as_str() {
        "tv" => "Search a TV show to track…",
        "audiobook" => "Search an audiobook to track…",
        _ => "Search a movie to track…",
    };

    // Add a movie as monitored/wanted, then mark the row owned + toast.
    let add_movie = move |tmdb: u64, title: String, year: Option<u16>| {
        let Some(profile) = effective_profile() else {
            toast.set(Some("Create a quality profile in Config first".into()));
            return;
        };
        spawn_local(async move {
            match api::add_movie(tmdb, &profile).await {
                Ok(()) => {
                    lib_movies.update(|s| {
                        s.insert((title.to_lowercase(), year));
                    });
                    toast.set(Some(format!("Added “{title}”")));
                }
                Err(e) => toast.set(Some(e.to_string())),
            }
        });
    };
    let add_series = move |tvdb: u64, title: String| {
        let Some(profile) = effective_profile() else {
            toast.set(Some("Create a quality profile in Config first".into()));
            return;
        };
        spawn_local(async move {
            match api::add_series(tvdb, &profile, "all").await {
                Ok(()) => {
                    lib_series.update(|s| {
                        s.insert(tvdb);
                    });
                    toast.set(Some(format!("Added “{title}”")));
                }
                Err(e) => toast.set(Some(e.to_string())),
            }
        });
    };
    let add_book = move |asin: String, title: String| {
        let profile = effective_profile();
        spawn_local(async move {
            match api::add_book(asin.as_str(), profile.as_deref(), true).await {
                Ok(()) => {
                    lib_asins.update(|s| {
                        s.insert(asin);
                    });
                    toast.set(Some(format!("Added “{title}”")));
                }
                Err(e) => toast.set(Some(e.to_string())),
            }
        });
    };

    let results = move || {
        if searching.get() {
            return view! { <Loading label="Searching…"/> }.into_any();
        }
        if !searched.get() {
            return ().into_any();
        }
        // Render only the selected domain's hits.
        let rows = match domain.get().as_str() {
            "tv" => {
                let owned = lib_series.get();
                if series_hits.get().is_empty() {
                    return view! { <p class="muted add-status">"No matches — try another title."</p> }
                        .into_any();
                }
                series_hits
                    .get()
                    .into_iter()
                    .map(|s| {
                        let in_lib = owned.contains(&s.tvdb_id);
                        let sub = s.year.map(|y| y.to_string()).unwrap_or_default();
                        let (title, tvdb) = (s.title.clone(), s.tvdb_id);
                        let action = if in_lib {
                            view! { <span class="add-owned"><Pill color=token::OK>"In library"</Pill></span> }.into_any()
                        } else {
                            view! {
                                <Button class:add-btn=true aria_label=format!("Add {}", s.title)
                                    on_click=Callback::new(move |()| add_series(tvdb, title.clone()))>
                                    "+ Add"
                                </Button>
                            }
                            .into_any()
                        };
                        result_row(
                            "TV",
                            &s.title,
                            &sub,
                            s.poster_url.clone(),
                            s.overview.clone(),
                            action,
                        )
                    })
                    .collect_view()
            }
            "audiobook" => {
                let owned = lib_asins.get();
                if book_hits.get().is_empty() {
                    return view! { <p class="muted add-status">"No matches — try another title."</p> }
                        .into_any();
                }
                book_hits
                    .get()
                    .into_iter()
                    .map(|b| {
                        let in_lib = owned.contains(&b.asin);
                        let sub = b.authors.join(", ");
                        let (asin, title) = (b.asin.clone(), b.title.clone());
                        let action = if in_lib {
                            view! { <span class="add-owned"><Pill color=token::OK>"In library"</Pill></span> }.into_any()
                        } else {
                            view! {
                                <Button class:add-btn=true aria_label=format!("Add {}", b.title)
                                    on_click=Callback::new(move |()| add_book(asin.clone(), title.clone()))>
                                    "+ Add"
                                </Button>
                            }
                            .into_any()
                        };
                        result_row("BOOK", &b.title, &sub, b.cover_url.clone(), None, action)
                    })
                    .collect_view()
            }
            _ => {
                let owned = lib_movies.get();
                if movie_hits.get().is_empty() {
                    return view! { <p class="muted add-status">"No matches — try another title."</p> }
                        .into_any();
                }
                movie_hits
                    .get()
                    .into_iter()
                    .map(|m| {
                        let in_lib = owned.contains(&(m.title.to_lowercase(), m.year));
                        let sub = m.year.map(|y| y.to_string()).unwrap_or_default();
                        let (title, year, tmdb) = (m.title.clone(), m.year, m.tmdb_id);
                        let action = if in_lib {
                            view! { <span class="add-owned"><Pill color=token::OK>"In library"</Pill></span> }.into_any()
                        } else {
                            view! {
                                <Button class:add-btn=true aria_label=format!("Add {}", m.title)
                                    on_click=Callback::new(move |()| add_movie(tmdb, title.clone(), year))>
                                    "+ Add"
                                </Button>
                            }
                            .into_any()
                        };
                        result_row(
                            "FILM",
                            &m.title,
                            &sub,
                            m.poster_url.clone(),
                            m.overview.clone(),
                            action,
                        )
                    })
                    .collect_view()
            }
        };
        view! { <div class="add-results">{rows}</div> }.into_any()
    };

    // Aurora's toasts show the result (SKADI-T-0719). Each Aurora toast is a
    // live region added with its text, which a screen reader can miss, so a
    // region that is always there says it too (SKADI-T-0700, AURORA-T-0013).
    let toaster = use_context::<Toaster>();
    Effect::new(move |_| {
        if let (Some(m), Some(t)) = (toast.get(), toaster) {
            if m.starts_with("Added") {
                t.success(m);
            } else {
                t.error(m);
            }
        }
    });

    // Aurora's PageHeader, Select, TextInput and Button (SKADI-T-0719). A form,
    // so Enter in the field searches.
    view! {
        <div class="add-view">
            <PageHeader title="Add media" class:page-header=true
                sub="Pick a media type, then search it — movies via TMDB, TV via TheTVDB, audiobooks via Audnexus."/>
            <form class="add-search" on:submit=move |ev| { ev.prevent_default(); do_search(); }>
                <div class="add-domain">
                    <Select aria_label="Media type" value=domain on_change=Callback::new(on_domain_change)
                        option_pairs=vec![
                            ("movie".to_string(), "Movie".to_string()),
                            ("tv".to_string(), "TV".to_string()),
                            ("audiobook".to_string(), "Audiobook".to_string()),
                        ]/>
                </div>
                <div class="add-search-field">
                    {move || {
                        let placeholder = placeholder();
                        view! { <TextInput value=query aria_label="Search" placeholder=placeholder/> }
                    }}
                </div>
                <Button button_type="submit" loading=searching>"Search"</Button>
            </form>
            {results}
            <div class="toast-region sr-only" role="status" aria-live="polite">
                {move || toast.get()}
            </div>
        </div>
    }
}

/// One Add result row: poster + media tag + title/sub/overview + a contextual action.
fn result_row(
    tag: &str,
    title: &str,
    sub: &str,
    poster: Option<String>,
    overview: Option<String>,
    action: AnyView,
) -> AnyView {
    let title = title.to_string();
    let sub = sub.to_string();
    let tag_color = crate::household::kind_tone(&tag.to_lowercase());
    let tag = tag.to_string();
    // Real artwork when the search carried a poster/cover; otherwise the placeholder.
    let poster_view = match poster {
        Some(url) => view! { <img class="add-poster" src=url alt="" loading="lazy"/> }.into_any(),
        None => view! { <div class="add-poster" aria-hidden="true"></div> }.into_any(),
    };
    let overview_view = overview
        .filter(|o| !o.trim().is_empty())
        .map(|o| view! { <span class="add-overview">{o}</span> });
    view! {
        <div class="add-row">
            {poster_view}
            <div class="add-info">
                <div class="add-info-head">
                    <Pill color=tag_color>{tag}</Pill>
                    <span class="add-title">{title}</span>
                </div>
                <span class="add-sub mono">{sub}</span>
                {overview_view}
            </div>
            {action}
        </div>
    }
    .into_any()
}
