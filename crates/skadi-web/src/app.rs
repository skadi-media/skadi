//! The app shell: persistent left sidebar (brand + health, then Home / Domains
//! / System groups) over a routed main area (SKADI-T-0071). Under 720px the
//! sidebar is an off-canvas drawer behind a menu button ([`AppFrame`],
//! SKADI-T-0697). Split out of `main.rs` into the lib so the bin stays a thin
//! mount point.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::hooks::use_navigate;
use leptos_router::path;

use crate::activity::ActivityPage;
use crate::add::AddPage;
use crate::api;
use crate::audiobook_import::AudiobookImportPage;
use crate::audiobooks::{
    AudiobookDiscoverPage, AudiobooksConfigPage, AudiobooksPage, AuthorDetailPage, BookDetailPage,
};
use crate::config::ConfigPage;
use crate::confirm::{ConfirmDialogHost, ConfirmSpec, confirm};
use crate::dashboard::Dashboard;
use crate::import::LibraryImportPage;
use crate::library_toolbar::{local_get, local_set};
use crate::loading::{ListLoad, SkeletonKind};
use crate::movies::{MovieDetailPage, MoviesConfigPage, MoviesPage};
use crate::settings::{ProviderSection, indexer_spec};
use crate::tv::{SeriesDetailPage, TvPage};
use crate::tv_import::TvImportPage;

/// Result of the startup health probe, rendered in the shell.
#[derive(Clone)]
enum HealthState {
    Pending,
    Ok(String),
    Err(String),
}

/// Shared "domains changed" trigger. The Config domains toggle bumps it; the
/// persistent [`Sidebar`] (which never remounts) reads it so an enabled domain
/// appears in the nav immediately, without a page reload.
#[derive(Clone, Copy)]
pub struct DomainsVersion(pub RwSignal<u32>);

/// The sign-in page (SKADI-T-0621).
///
/// Shown when a request has come back `401` — which covers both "this browser
/// has never signed in" and "its token was revoked". An **open-mode** daemon
/// never answers `401`, so it never appears there, which is the whole reason
/// the gate keys off the flag rather than off whether a token is stored.
#[component]
fn LoginPage() -> impl IntoView {
    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let submit = move || {
        let (u, p) = (username.get_untracked(), password.get_untracked());
        if u.trim().is_empty() || p.is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match crate::api::login(u.trim(), &p).await {
                // A full reload is the simplest correct thing: every page's
                // data was fetched (or refused) under the old credential.
                Ok(_) => {
                    let _ = window().location().reload();
                }
                Err(e) => {
                    busy.set(false);
                    password.set(String::new());
                    error.set(Some(e.to_string()));
                }
            }
        });
    };

    view! {
        <div class="login-shell">
            <form class="login-card" on:submit=move |ev| { ev.prevent_default(); submit() }>
                <h1 class="login-title">"Skadi"</h1>
                <p class="muted">"Sign in to your household account."</p>
                <div class="field">
                    <label for="login-user">"Name"</label>
                    <input id="login-user" r#type="text" autocomplete="username" autofocus
                        prop:value=move || username.get()
                        on:input=move |ev| username.set(event_target_value(&ev))/>
                </div>
                <div class="field">
                    <label for="login-pass">"Password"</label>
                    <input id="login-pass" r#type="password" autocomplete="current-password"
                        prop:value=move || password.get()
                        on:input=move |ev| password.set(event_target_value(&ev))/>
                </div>
                {move || error.get().map(|e| view! { <p class="bad" role="alert">{e}</p> })}
                <button class="login-submit" r#type="submit" disabled=move || busy.get()>
                    {move || if busy.get() { "Signing in…" } else { "Sign in" }}
                </button>
            </form>
        </div>
    }
}

#[component]
pub fn App() -> impl IntoView {
    // Provided once at the shell so any view can request a sidebar refresh.
    provide_context(DomainsVersion(RwSignal::new(0)));

    // Polled rather than pushed: the flag is set deep inside `api`, where no
    // reactive scope is in hand (SKADI-T-0474 set this shape up).
    let needs_login = RwSignal::new(crate::api::auth_expired());
    Effect::new(move |_| {
        let handle = gloo_timers::callback::Interval::new(500, move || {
            needs_login.set(crate::api::auth_expired());
        });
        handle.forget();
    });

    // One dialog host for every `confirm::confirm` in the app (SKADI-T-0694).
    view! {
        <Show when=move || needs_login.get() fallback=Shell>
            <LoginPage/>
        </Show>
        <ConfirmDialogHost/>
    }
}

/// The signed-in console.
#[component]
fn Shell() -> impl IntoView {
    // Fetched once here, above the router, so the sidebar and every section
    // strip agree on who is signed in without a `/me` call apiece.
    let role = RwSignal::new(None::<String>);
    provide_context(crate::subnav::RoleCtx(role));
    Effect::new(move |_| {
        leptos::task::spawn_local(async move {
            if let Ok(m) = api::me().await {
                role.set(Some(m.role));
            }
        });
    });
    view! {
        <Router>
            <AppFrame>
                <Routes fallback=|| view! { <p>"Not found"</p> }>
                    <Route path=path!("/") view=Dashboard/>
                    <Route path=path!("/add") view=AddPage/>
                    <Route path=path!("/setup") view=crate::setup::SetupPage/>
                    <Route path=path!("/activity") view=ActivityPage/>
                    <Route path=path!("/wanted") view=crate::wanted::WantedPage/>
                    <Route path=path!("/players") view=crate::offline::PlayersPage/>
                    <Route path=path!("/upload") view=crate::upload::UploadPage/>
                    <Route path=path!("/movies") view=MoviesPage/>
                    <Route path=path!("/movies/import") view=LibraryImportPage/>
                    <Route path=path!("/movies/config") view=MoviesConfigPage/>
                    // Dynamic last; leptos_router ranks static segments higher
                    // so /movies/import and /movies/config still win.
                    <Route path=path!("/movies/:id") view=MovieDetailPage/>
                    <Route path=path!("/tv") view=TvPage/>
                    <Route path=path!("/tv/import") view=TvImportPage/>
                    // Dynamic last so any future static /tv/* still wins.
                    <Route path=path!("/tv/:id") view=SeriesDetailPage/>
                    <Route path=path!("/audiobooks") view=AudiobooksPage/>
                    <Route path=path!("/audiobooks/import") view=AudiobookImportPage/>
                    <Route path=path!("/audiobooks/config") view=AudiobooksConfigPage/>
                    <Route path=path!("/audiobooks/discover") view=AudiobookDiscoverPage/>
                    <Route path=path!("/audiobooks/authors/:id") view=AuthorDetailPage/>
                    // Dynamic last; static /audiobooks/config &
                    // /audiobooks/authors/:id rank higher so they still win.
                    <Route path=path!("/audiobooks/:id") view=BookDetailPage/>
                    <Route path=path!("/listen") view=crate::offline::ListenPage/>
                    <Route path=path!("/listen/:id/:fid") view=crate::player::PlayerPage/>
                    <Route path=path!("/watch/movie/:id/:eid") view=crate::watch::WatchMoviePage/>
                    <Route path=path!("/watch/tv/:id/:eid") view=crate::watch::WatchEpisodePage/>
                    <Route path=path!("/indexers") view=IndexersPage/>
                    <Route path=path!("/downloaders") view=DownloadersPage/>
                    <Route path=path!("/config") view=ConfigPage/>
                    <Route path=path!("/household") view=crate::household::HouseholdPage/>
                    <Route path=path!("/naming") view=crate::naming::NamingPage/>
                    <Route path=path!("/system") view=crate::system::SystemPage/>
                </Routes>
            </AppFrame>
        </Router>
    }
}

/// The `id` of the sidebar `<nav>`; the menu button's `aria-controls`.
pub const NAV_ID: &str = "app-nav";

/// The `id` of the routed `<main>`; the skip link's target (SKADI-T-0700).
pub const MAIN_ID: &str = "main";

/// The class on `<html>` that stops the page scrolling behind the open drawer.
pub const SCROLL_LOCK_CLASS: &str = "nav-lock";

fn set_scroll_lock(on: bool) {
    if let Some(root) = document().document_element() {
        let _ = root.class_list().toggle_with_force(SCROLL_LOCK_CLASS, on);
    }
}

/// After a route change: focus `<main>`, unless the new page already put the
/// focus inside it.
fn focus_main_after_navigation() {
    use wasm_bindgen::JsCast;
    let Some(main) = document()
        .get_element_by_id(MAIN_ID)
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let inside = document()
        .active_element()
        .is_some_and(|a| a.is_connected() && main.contains(Some(&a)));
    if !inside {
        let _ = main.focus();
    }
}

/// The skip link moves focus to `<main>` itself. It does not change the URL:
/// the router would read `#main` as a navigation.
fn skip_to_main(ev: leptos::ev::MouseEvent) {
    use wasm_bindgen::JsCast;
    ev.prevent_default();
    if let Some(main) = document()
        .get_element_by_id(MAIN_ID)
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = main.focus();
    }
}

/// Whether the narrow-screen drawer is open (SKADI-T-0697). Wide screens never
/// set it: the menu button that does is hidden there.
#[derive(Clone, Copy)]
struct DrawerOpen(RwSignal<bool>);

/// The first thing a keyboard user can reach inside `root`.
fn first_focusable(root: &web_sys::Element) -> Option<web_sys::HtmlElement> {
    use wasm_bindgen::JsCast;
    root.query_selector("a[href], button:not([disabled]), input, select")
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
}

/// The shell's frame: a top bar with the menu button (narrow screens only),
/// the sidebar, and the routed `<main>` (SKADI-T-0697).
///
/// Under 720px the sidebar is an off-canvas drawer; `style.css` does all the
/// layout from one signal, the `nav-open` class on `.app`. The drawer:
/// - opens from a real `<button>` with `aria-expanded` / `aria-controls`, and
///   focus moves to its first link;
/// - closes on Esc (focus back on the menu button), on a click on the scrim,
///   on any navigation, on a click on one of its links, and when the window
///   grows past the breakpoint;
/// - makes `<main>` `inert` while it is open, so Tab stays in the drawer.
///
/// Must sit inside a `<Router>` (it watches the location).
#[component]
pub fn AppFrame(children: Children) -> impl IntoView {
    let open = RwSignal::new(false);
    provide_context(DrawerOpen(open));
    let menu_ref = NodeRef::<leptos::html::Button>::new();

    // Close on navigation (the first run only records the path).
    let location = leptos_router::hooks::use_location();
    // A new page also takes the keyboard focus to <main> (SKADI-T-0700): the
    // link that was used stays in the sidebar, or is gone with the old page,
    // and Tab would start again from the top.
    Effect::new(move |prev: Option<String>| {
        let path = location.pathname.get();
        if prev.is_some_and(|p| p != path) {
            open.set(false);
            request_animation_frame(focus_main_after_navigation);
        }
        path
    });

    // Focus into the drawer once it is open. After a frame: the drawer is
    // `visibility: hidden` until the class lands, and a hidden link takes no
    // focus.
    Effect::new(move |was: Option<bool>| {
        let now = open.get();
        if now && was == Some(false) {
            request_animation_frame(move || {
                let nav = document().get_element_by_id(NAV_ID);
                if let Some(target) = nav.as_ref().and_then(first_focusable) {
                    let _ = target.focus();
                }
            });
        }
        now
    });

    let close_to_menu = move || {
        open.set(false);
        if let Some(b) = menu_ref.get_untracked() {
            let _ = b.focus();
        }
    };
    let keys = window_event_listener(leptos::ev::keydown, move |ev| {
        // A confirm dialog above the drawer takes its own Esc.
        let dialog_open = document()
            .query_selector(".confirm-dialog")
            .ok()
            .flatten()
            .is_some();
        if ev.key() == "Escape" && open.get_untracked() && !dialog_open {
            ev.prevent_default();
            close_to_menu();
        }
    });
    on_cleanup(move || keys.remove());
    let resize = window_event_listener(leptos::ev::resize, move |_| {
        let wide = window()
            .inner_width()
            .ok()
            .and_then(|w| w.as_f64())
            .is_some_and(|w| w > 720.0);
        if wide && open.get_untracked() {
            open.set(false);
        }
    });
    on_cleanup(move || resize.remove());

    // Lock the page behind the open drawer, so a swipe scrolls the drawer and
    // not the page under the scrim (SKADI-T-0700).
    Effect::new(move |_| set_scroll_lock(open.get()));
    on_cleanup(|| set_scroll_lock(false));

    view! {
        <div class="app" class:nav-open=move || open.get()>
            <a class="skip-link" href=format!("#{MAIN_ID}") on:click=skip_to_main>"Skip to content"</a>
            <header class="topbar">
                <button
                    type="button"
                    class="menu-btn"
                    node_ref=menu_ref
                    aria-label="Menu"
                    aria-controls=NAV_ID
                    aria-expanded=move || if open.get() { "true" } else { "false" }
                    on:click=move |_| open.update(|o| *o = !*o)
                >
                    <span class="menu-icon" aria-hidden="true"></span>
                </button>
                <span class="topbar-brand">
                    <Snowflake/>
                    <span class="brand-name">"Skadi"</span>
                </span>
            </header>
            <div class="nav-scrim" aria-hidden="true" on:click=move |_| close_to_menu()></div>
            <Sidebar/>
            <crate::shortcuts::ShortcutsHost/>
            <main class="main" id=MAIN_ID tabindex="-1" inert=move || open.get()>
                {children()}
            </main>
        </div>
    }
}

/// Who is signed in, and the way out (SKADI-T-0621).
///
/// Absent in open mode, where `/me` answers as the synthetic operator and there
/// is no credential to drop.
#[component]
fn AccountStrip() -> impl IntoView {
    let me = RwSignal::new(None::<crate::api::Member>);
    Effect::new(move |_| {
        leptos::task::spawn_local(async move {
            if let Ok(m) = crate::api::me().await {
                me.set(Some(m));
            }
        });
    });
    let changing = RwSignal::new(false);
    move || {
        let signed_in = crate::api::is_signed_in();
        me.get().map(|m| {
            view! {
                <div class="nav-account">
                    <span title=m.role.clone()>{m.name.clone()}</span>
                    {signed_in.then(|| view! {
                        <button on:click=move |_| changing.update(|c| *c = !*c)>"Password"</button>
                        <button on:click=move |_| {
                            leptos::task::spawn_local(async move {
                                crate::api::logout().await;
                                let _ = window().location().reload();
                            });
                        }>"Sign out"</button>
                    })}
                </div>
                {move || changing.get().then(|| view! { <ChangePassword on_done=move || changing.set(false)/> })}
            }
        })
    }
}

/// Change your own password (SKADI-T-0626). Everyone gets this, whatever their
/// role — a password the operator minted and handed over is one the person
/// should be able to replace with something only they know.
#[component]
fn ChangePassword<F>(on_done: F) -> impl IntoView
where
    F: Fn() + Copy + Send + Sync + 'static,
{
    let current = RwSignal::new(String::new());
    let next = RwSignal::new(String::new());
    let confirm = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let done = RwSignal::new(false);
    let busy = RwSignal::new(false);

    let submit = move || {
        let (c, n, k) = (
            current.get_untracked(),
            next.get_untracked(),
            confirm.get_untracked(),
        );
        if n != k {
            error.set(Some("Those two do not match.".into()));
            return;
        }
        if n.trim().len() < 6 {
            error.set(Some("Use at least 6 characters.".into()));
            return;
        }
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match crate::api::change_password(&c, n.trim()).await {
                Ok(()) => {
                    busy.set(false);
                    done.set(true);
                    current.set(String::new());
                    next.set(String::new());
                    confirm.set(String::new());
                }
                Err(e) => {
                    busy.set(false);
                    error.set(Some(e.to_string()));
                }
            }
        });
    };

    view! {
        <form class="pw-form" on:submit=move |ev| { ev.prevent_default(); submit() }>
            {move || if done.get() {
                view! {
                    <p class="ok">"Password changed. Your other devices have been signed out."</p>
                    <button r#type="button" on:click=move |_| on_done()>"Close"</button>
                }.into_any()
            } else {
                view! {
                    <div class="field">
                        <label>"Current password"</label>
                        <input r#type="password" autocomplete="current-password"
                            prop:value=move || current.get()
                            on:input=move |ev| current.set(event_target_value(&ev))/>
                    </div>
                    <div class="field">
                        <label>"New password"</label>
                        <input r#type="password" autocomplete="new-password"
                            prop:value=move || next.get()
                            on:input=move |ev| next.set(event_target_value(&ev))/>
                    </div>
                    <div class="field">
                        <label>"New password again"</label>
                        <input r#type="password" autocomplete="new-password"
                            prop:value=move || confirm.get()
                            on:input=move |ev| confirm.set(event_target_value(&ev))/>
                    </div>
                    {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
                    <div class="form-actions">
                        <button r#type="submit" disabled=move || busy.get()>"Change it"</button>
                        <button r#type="button" on:click=move |_| on_done()>"Cancel"</button>
                    </div>
                }.into_any()
            }}
        </form>
    }
}

/// Sidebar label + route for a domain, by its `kind`. `None` for a kind with no
/// front-end view (so we don't render a dead link).
fn domain_nav(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "movie" => Some(("Movies", "/movies")),
        "series" => Some(("TV", "/tv")),
        "audiobook" => Some(("Audiobooks", "/audiobooks")),
        _ => None,
    }
}

/// The persistent left navigation: brand + health, then grouped sections. The
/// Domains group lists only **enabled** domains (from `/domains`), so it agrees
/// with the dashboard — a disabled domain is enabled under Config → Domains,
/// after which its link appears (SKADI-T-0143 follow-up).
#[component]
fn Sidebar() -> impl IntoView {
    let domains = RwSignal::new(Vec::<api::Domain>::new());
    let movie_count = RwSignal::new(None::<usize>);
    let series_count = RwSignal::new(None::<usize>);
    let book_count = RwSignal::new(None::<usize>);
    let roots = RwSignal::new(Vec::<api::RootFolder>::new());

    // Read before the fetches below, because it gates them. The API has
    // enforced these since SKADI-T-0625; the sidebar was still *asking*, so a
    // read-only member's console fired a 403 on every page load
    // (operator report, 2026-09-24).
    let role_ctx = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    let role = move || role_ctx.and_then(|r| r.get());
    let is_admin = move || role().as_deref() == Some("admin");
    let can_contribute = move || matches!(role().as_deref(), Some("admin") | Some("contributor"));
    // Re-fetch on mount and whenever the shared version bumps (a domain toggle in
    // Config), so the nav reflects enable/disable without a reload.
    let version = use_context::<DomainsVersion>();
    Effect::new(move |_| {
        if let Some(DomainsVersion(v)) = version {
            v.track();
        }
        spawn_local(async move {
            if let Ok(d) = api::list_domains().await {
                domains.set(d);
            }
        });
    });
    // Library counts + root-folder usage for the nav badges + footer bars.
    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(m) = api::list_movies().await {
                movie_count.set(Some(m.len()));
            }
            if let Ok(s) = api::list_series().await {
                series_count.set(Some(s.len()));
            }
            if let Ok(b) = api::list_books(None, None).await {
                book_count.set(Some(b.len()));
            }
            // `/root-folders` is admin-only. Asking as anyone else is a
            // guaranteed 403 in the console and an empty meter either way.
            if is_admin()
                && let Ok(r) = api::root_folders().await
            {
                roots.set(r);
            }
        });
    });

    let library_links = move || {
        let enabled: Vec<_> = domains.get().into_iter().filter(|d| d.enabled).collect();
        if enabled.is_empty() {
            return ().into_any();
        }
        let links = enabled
            .into_iter()
            .filter_map(|d| {
                let (label, href) = domain_nav(&d.kind)?;
                let (dot, count) = match d.kind.as_str() {
                    "movie" => ("ice", movie_count.get()),
                    "series" => ("gold", series_count.get()),
                    "audiobook" => ("teal", book_count.get()),
                    _ => ("muted", None),
                };
                Some(view! {
                    <A href=href>
                        <span class=format!("nav-dot {dot}")></span>
                        <span class="nav-label">{label}</span>
                        <span class="nav-count mono">
                            {count.map(|c| c.to_string()).unwrap_or_default()}
                        </span>
                    </A>
                })
            })
            .collect_view();
        view! {
            <div class="nav-group">
                <span class="nav-group-label">"Library"</span>
                {links}
            </div>
        }
        .into_any()
    };

    let storage = move || {
        roots
            .get()
            .into_iter()
            .map(|r| {
                let pct = match (r.total_bytes, r.free_bytes) {
                    (Some(total), Some(free)) if total > 0 => {
                        ((total.saturating_sub(free)) as f64 / total as f64 * 100.0).round() as u64
                    }
                    _ => 0,
                };
                // Severity by fill (SKADI-T-0347): a media box at 92% is a
                // near-failure, not a calm accent-blue bar. ok <75, warn 75-90,
                // bad >90.
                let sev = if pct > 90 {
                    "bad"
                } else if pct >= 75 {
                    "warn"
                } else {
                    "ok"
                };
                view! {
                    <div class="storage-row">
                        <div class="storage-row-head mono">
                            <span class="storage-path">{r.path.clone()}</span>
                            <span class=format!("storage-pct {sev}")>{pct}"%"</span>
                        </div>
                        <div class="storage-track">
                            <div
                                class=format!("storage-fill {sev}")
                                style=format!("width:{pct}%")
                            ></div>
                        </div>
                    </div>
                }
            })
            .collect_view()
    };

    // A `Callback` rather than a bare closure: the Add button now renders
    // inside a reactive closure, and a plain closure that moves `navigate`
    // would make the enclosing view `FnOnce`.
    let navigate = use_navigate();
    let go_add = Callback::new(move |()| navigate("/add", Default::default()));
    let drawer = use_context::<DrawerOpen>();

    // Which areas this account can actually reach (SKADI-T-0627). The API has
    // gated these since SKADI-T-0625, but the sidebar offered them to everyone
    // and let the 403 do the talking. Four links is little enough that a dead
    // one is conspicuous, so ask first. The role comes from the shell, which
    // sits above the router and so can hand the same answer to the section
    // strips on the pages.

    view! {
        <nav
            class="nav"
            id=NAV_ID
            aria-label="Main"
            // A link to the page already shown changes no path, so close on
            // the click itself too (SKADI-T-0697).
            on:click=move |ev| {
                use wasm_bindgen::JsCast;
                let on_link = ev
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .and_then(|e| e.closest("a[href]").ok().flatten())
                    .is_some();
                if on_link && let Some(DrawerOpen(open)) = drawer {
                    open.set(false);
                }
            }
        >
            <div class="brand">
                <Snowflake/>
                <span class="brand-name">"Skadi"</span>
            </div>
            <HealthBadge/>
            {move || can_contribute().then(|| view! {
                <button class="add-media" on:click=move |_| go_add.run(())>
                    "+ Add media"
                </button>
            })}

            <div class="nav-main">
                // One entry per *area*, not per page (SKADI-T-0627). Wanted and
                // the running hunts share an Activity strip; everything that
                // configures skadi sits behind Settings — each as a section
                // strip on the page itself, so deep links still work.
                <A href="/"><span class="nav-label">"Overview"</span></A>
                // A contributor can see what is still missing but not the
                // hunter's internals, so their Activity link is Wanted.
                {move || is_admin().then(|| view! {
                    <A href="/activity"><span class="nav-label">"Activity"</span></A>
                })}
                {move || (!is_admin() && can_contribute()).then(|| view! {
                    <A href="/wanted"><span class="nav-label">"Wanted"</span></A>
                })}
                {library_links}
                {move || is_admin().then(|| view! {
                    <A href="/indexers"><span class="nav-label">"Settings"</span></A>
                })}
            </div>

            <div class="nav-footer">
                <AccountStrip/>
                {storage}
            </div>
        </nav>
    }
}

/// The Skadi mark — a 6-point snowflake/asterisk (3 crossing strokes), the brand
/// accent. Inline SVG so there's no raster asset (SKADI-I-0035).
#[component]
fn Snowflake() -> impl IntoView {
    view! {
        <svg
            class="brand-mark"
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="var(--brand-stroke)"
            stroke-width="1.7"
            stroke-linecap="round"
        >
            <line x1="12" y1="3" x2="12" y2="21"></line>
            <line x1="4.2" y1="7.5" x2="19.8" y2="16.5"></line>
            <line x1="19.8" y1="7.5" x2="4.2" y2="16.5"></line>
        </svg>
    }
}

/// Calls `/api/v1/health` on mount and shows the daemon status + version.
#[component]
fn HealthBadge() -> impl IntoView {
    let (state, set_state) = signal(HealthState::Pending);

    spawn_local(async move {
        match api::health().await {
            Ok(h) => set_state.set(HealthState::Ok(h.version)),
            Err(e) => set_state.set(HealthState::Err(e.to_string())),
        }
    });

    view! {
        <div class="daemon-line mono">
            {move || match state.get() {
                HealthState::Pending => {
                    view! {
                        <span class="health-dot muted"></span>
                        <span class="muted">"checking daemon…"</span>
                    }
                        .into_any()
                }
                HealthState::Ok(version) => {
                    view! {
                        <span class="health-dot ok"></span>
                        <span class="ok">"daemon ok"</span>
                        <span class="faint">" · v" {version}</span>
                    }
                        .into_any()
                }
                HealthState::Err(msg) => {
                    view! {
                        <span class="health-dot bad"></span>
                        <span class="bad">"daemon unreachable: " {msg}</span>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

/// Indexers (system-wide) — Torznab providers.
#[component]
pub fn IndexersPage() -> impl IntoView {
    view! {
        <crate::subnav::SubNav/>
        <ProviderSection spec=indexer_spec()/>
    }
}

/// Downloaders (system-wide). Skadi ships a single **zero-config built-in**
/// downloader — the VPN-isolated DB-queue worker — so this page is a one-click
/// register rather than a kind selector + form (SKADI-T-0097). The built-in
/// needs no host or secret; its download paths default and are editable only
/// via the settings API.
#[component]
pub fn DownloadersPage() -> impl IntoView {
    // The skadi torrent worker is the *only* downloader — auto-registered on boot,
    // zero-config — so this page is its control surface: engine status + the live
    // torrent list (SKADI-T-0170 / I-0041). The engine status reads the worker's
    // own DB heartbeat (SKADI-T-0288), so it reflects the actual worker *process*,
    // not whether a downloader is registered — truthful even when idle or down.
    let engine = RwSignal::new(None::<api::WorkerStatus>);
    let vpn = RwSignal::new(None::<api::VpnStatus>);
    let loaded = RwSignal::new(false);
    let alive = RwSignal::new(true);
    Effect::new(move |_| {
        spawn_local(async move {
            loop {
                if let Ok(w) = api::worker_status().await {
                    engine.set(Some(w));
                }
                if let Ok(v) = api::vpn_status().await {
                    vpn.set(Some(v));
                }
                loaded.set(true);
                gloo_timers::future::TimeoutFuture::new(3000).await;
                if !alive.try_get_untracked().unwrap_or(false) {
                    break;
                }
            }
        });
    });
    on_cleanup(move || alive.set(false));

    // gluetun VPN tunnel state — the worker is VPN-isolated and the tunnel is
    // always-on (gluetun's firewall is the structural kill-switch), so this just
    // *confirms* it's up and shouts if it ever isn't. Read-only by design — no
    // control to disable it (SKADI-T-0292).
    let vpn_panel = move || match vpn.get() {
        None => ().into_any(),
        Some(v) if !v.reachable => view! {
            <div class="vpn-panel">
                <span class="health-dot muted"></span>
                <div class="vpn-text"><strong>"VPN status unavailable"</strong>
                    <span class="muted vpn-sub">"gluetun control API unreachable"</span>
                </div>
            </div>
        }
        .into_any(),
        Some(v) => {
            let connected = v.connected;
            let loc = match (v.city.clone(), v.country.clone()) {
                (Some(c), Some(co)) => format!("{c}, {co}"),
                (_, Some(co)) => co,
                _ => "location unknown".into(),
            };
            let ip = v.exit_ip.clone().unwrap_or_default();
            let sub = if connected {
                format!("exit {ip} · {loc} · kill-switch armed (egress is the tunnel)")
            } else {
                "tunnel down — the worker's egress is blocked, no leak (kill-switch holding)".into()
            };
            let title = if connected {
                "VPN connected"
            } else {
                "VPN disconnected"
            };
            let cls = if connected {
                "vpn-panel ok"
            } else {
                "vpn-panel bad"
            };
            let dot = if connected { "ok" } else { "bad" };
            view! {
                <div class=cls>
                    <span class=format!("health-dot {dot}")></span>
                    <div class="vpn-text">
                        <strong>{title}</strong>
                        <span class="muted mono vpn-sub">{sub}</span>
                    </div>
                </div>
            }
            .into_any()
        }
    };

    let engine_card = move || match engine.get() {
        None => {
            let msg = if loaded.get() {
                "Worker status unavailable"
            } else {
                "Checking the torrent worker…"
            };
            view! {
                <div class="dl-engine">
                    <span class="health-dot muted"></span>
                    <div class="dl-engine-text"><strong>{msg}</strong></div>
                </div>
            }
            .into_any()
        }
        Some(w) if w.running => {
            let seen = w
                .age_secs
                .map(|a| format!("last beat {a}s ago"))
                .unwrap_or_else(|| "alive".into());
            let ver = w
                .version
                .clone()
                .map(|v| format!(" · v{v}"))
                .unwrap_or_default();
            let disk = fmt_free_disk(w.free_bytes, w.total_bytes);
            view! {
                <div class="dl-engine ok">
                    <span class="health-dot ok"></span>
                    <div class="dl-engine-text">
                        <strong>"Torrent worker running"</strong>
                        <span class="muted mono dl-engine-sub">{format!("{seen}{ver} · VPN-isolated{disk}")}</span>
                    </div>
                </div>
            }
            .into_any()
        }
        Some(w) => {
            let detail = match w.age_secs {
                Some(a) => format!("no heartbeat in {a}s"),
                None => "never checked in".into(),
            };
            view! {
                <div class="dl-engine bad">
                    <span class="health-dot bad"></span>
                    <div class="dl-engine-text">
                        <strong>"Torrent worker not running"</strong>
                        <span class="muted dl-engine-sub">
                            {format!("{detail} — new downloads can't start until it's back. It's VPN-isolated, so a down tunnel keeps it from starting.")}
                        </span>
                    </div>
                </div>
            }
            .into_any()
        }
    };

    view! {
        <crate::subnav::SubNav/>
        <div class="page-head"><h2>"Downloads"</h2></div>
        <div class="dl-view">
            {vpn_panel}
            {engine_card}
            <DownloadsSection/>
            <DownloadSettingsForm/>
        </div>
    }
}

/// Human download/upload rate, e.g. `1.2 MB/s`; `—` when unknown / zero. Pure.
fn fmt_speed(bps: Option<i64>) -> String {
    match bps {
        Some(b) if b > 0 => format!("{}/s", crate::movies::size_human(b as u64)),
        _ => "—".to_string(),
    }
}

/// Human ETA, e.g. `45s`, `2m 30s`, `1h 05m`; `—` when unknown. Pure.
fn fmt_eta(secs: Option<i64>) -> String {
    match secs {
        Some(s) if s > 0 => {
            let s = s as u64;
            if s >= 3600 {
                format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
            } else if s >= 60 {
                format!("{}m {:02}s", s / 60, s % 60)
            } else {
                format!("{s}s")
            }
        }
        _ => "—".to_string(),
    }
}

/// The "Added" cell of a Downloads row: the age of `created_at` at `now_ms`.
fn added_label(created_at: Option<&str>, now_ms: f64) -> String {
    let secs = created_at
        .map(js_sys::Date::parse)
        .filter(|t| !t.is_nan())
        .map(|t| (now_ms - t) / 1000.0);
    crate::downloads::age_label(secs)
}

/// The "Stalled" / "Error" badge of a download row (SKADI-T-0687), from
/// [`crate::downloads::row_badge`]; nothing for a healthy row.
fn row_badge_view(d: &api::Download) -> Option<AnyView> {
    crate::downloads::row_badge(d).map(|b| {
        let cls = format!("badge {} dl-badge", b.level);
        view! { <span class=cls title=b.title>{b.label}</span> }.into_any()
    })
}

/// `" · 1.2 TB free of 4.0 TB"` free-space suffix for the engine card; empty when
/// the download path's space is unknown. Pure.
fn fmt_free_disk(free: Option<u64>, total: Option<u64>) -> String {
    match (free, total) {
        (Some(f), Some(t)) if t > 0 => format!(
            " · {} free of {}",
            crate::movies::size_human(f),
            crate::movies::size_human(t)
        ),
        (Some(f), _) => format!(" · {} free", crate::movies::size_human(f)),
        _ => String::new(),
    }
}

/// Live "Active downloads" list for the Downloaders page (SKADI-T-0166): polls
/// `/downloads` and renders per-torrent progress + speed / ETA / peers / ratio.
#[component]
fn DownloadsSection() -> impl IntoView {
    use crate::downloads::{
        self as dlm, BulkAction, RowState, SelectAll, SortKey, SortState, StateFilter,
    };
    use crate::movies::size_human;
    use std::collections::{HashMap, HashSet};

    let jobs = RwSignal::new(Vec::<api::Download>::new());
    // acquirable-ref -> (title, media tag), so a job shows a name + FILM/BOOK chip.
    let titles = RwSignal::new(HashMap::<String, (String, &'static str)>::new());
    // First load, failure and Retry (SKADI-T-0698).
    let load = ListLoad::new();
    let alive = RwSignal::new(true);

    // Filter chip and sort state (SKADI-T-0686; sorting SKADI-T-0370). Held in
    // their own signals, apart from the polled rows, so a poll refresh keeps the
    // selection; remembered in localStorage like the Seeding accordion.
    let filter = RwSignal::new(StateFilter::from_key(
        local_get(dlm::FILTER_STORAGE_KEY).as_deref(),
    ));
    let active_sort = RwSignal::new(SortState::decode(
        local_get(dlm::ACTIVE_SORT_STORAGE_KEY).as_deref(),
    ));
    let seeding_sort = RwSignal::new(SortState::decode(
        local_get(dlm::SEEDING_SORT_STORAGE_KEY).as_deref(),
    ));

    // Multi-select (SKADI-T-0688): the ids of the selected active rows, in a
    // signal apart from the polled rows so a poll keeps it. Pruned on each poll
    // to the rows still in the active table.
    let selected = RwSignal::new(dlm::Selection::new());
    let bulk_busy = RwSignal::new(false);
    let bulk_msg = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let pruned = jobs.with(|js| selected.with_untracked(|s| dlm::prune_selection(s, js)));
        if selected.with_untracked(|s| *s != pruned) {
            selected.set(pruned);
        }
    });

    // Re-fetch the list now (after a control action), so the UI reflects it without
    // waiting for the next poll tick.
    let refetch = move || {
        spawn_local(async move {
            if let Some(d) = load.settle(api::list_downloads().await) {
                let _ = jobs.try_set(d);
            }
        });
    };

    Effect::new(move |_| {
        spawn_local(async move {
            let mut map = HashMap::<String, (String, &'static str)>::new();
            if let Ok(ms) = api::list_movies().await {
                for m in ms {
                    let t = m.title.clone();
                    for e in m.editions {
                        map.insert(e.id, (t.clone(), "FILM"));
                    }
                }
            }
            if let Ok(bs) = api::list_books(None, None).await {
                for b in bs {
                    let t = b.title.clone();
                    for f in b.files {
                        map.insert(f.id, (t.clone(), "BOOK"));
                    }
                }
            }
            titles.set(map);
        });
        // ~2 s while the tab is visible, nothing while it is hidden (SKADI-T-0693).
        spawn_local(crate::live_poll::poll_while_visible(
            move || alive.try_get_untracked().unwrap_or(false),
            move || async move {
                if let Some(d) = load.settle(api::list_downloads().await) {
                    let _ = jobs.try_set(d);
                }
            },
        ));
    });
    on_cleanup(move || alive.set(false));

    // The rows on show: filtered by the chip, split into the active table and the
    // Seeding list, each sorted by its own header (pure: `downloads::visible`).
    let shown = Memo::new(move |_| {
        let names = titles.get();
        let name = |d: &api::Download| {
            names
                .get(&d.acquirable_ref)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| d.acquirable_ref.clone())
        };
        dlm::visible(
            &jobs.get(),
            filter.get(),
            active_sort.get(),
            seeding_sort.get(),
            &name,
        )
    });

    // A clickable header cell for `sort` (shared by both lists).
    let sort_header = move |sort: RwSignal<SortState>,
                            storage_key: &'static str,
                            key: SortKey,
                            label: &'static str| {
        let on_click = move |_| {
            let next = sort.get_untracked().clicked(key);
            sort.set(next);
            local_set(storage_key, &next.encode());
        };
        view! {
            // A real button, so Tab reaches it and Enter / Space sort (SKADI-T-0700).
            <button
                type="button"
                class=move || if sort.get().key == key { "sort-head sort-active" } else { "sort-head" }
                on:click=on_click
            >
                {label}<span aria-hidden="true">{move || sort.get().arrow(key)}</span>
            </button>
        }
    };

    // Run one bulk action over the selected rows on show that offer it: one call
    // of the per-row endpoint each, in turn, then one refetch (SKADI-T-0688).
    let run_bulk = move |action: BulkAction| {
        let ids = selected
            .with_untracked(|s| shown.with_untracked(|v| dlm::bulk_targets(action, s, &v.active)));
        if ids.is_empty() || bulk_busy.get_untracked() {
            return;
        }
        let question = dlm::bulk_confirm_message(action, ids.len());
        spawn_local(async move {
            if let Some(body) = question {
                let n = dlm::transfers(ids.len());
                let (title, label) = if action == BulkAction::RemoveWithFiles {
                    (format!("Delete {n} and their files?"), "Remove and delete")
                } else {
                    (format!("Remove {n}?"), "Remove")
                };
                let spec = ConfirmSpec::destructive(title, body).confirm_label(label);
                if !confirm(spec).await || bulk_busy.get_untracked() {
                    return;
                }
            }
            bulk_busy.set(true);
            bulk_msg.set(None);
            let (mut done, mut failed) = (0, 0);
            for id in &ids {
                let r = match action {
                    BulkAction::Pause => api::pause_download(id).await,
                    BulkAction::Resume => api::resume_download(id).await,
                    BulkAction::Remove => api::remove_download(id, false).await,
                    BulkAction::RemoveWithFiles => api::remove_download(id, true).await,
                };
                if r.is_ok() {
                    done += 1;
                } else {
                    failed += 1;
                }
            }
            if action.is_destructive() {
                selected.update(|s| {
                    for id in &ids {
                        s.remove(id);
                    }
                });
            }
            bulk_msg.set(dlm::bulk_result_message(action, done, failed));
            bulk_busy.set(false);
            refetch();
        });
    };

    // The bulk bar, while any row on show is selected (SKADI-T-0688). Each
    // button names how many of the selection it acts on.
    let bulk_bar = move || {
        if filter.get() == StateFilter::Only(RowState::Seeding) {
            return ().into_any();
        }
        let v = shown.get();
        let sel = selected.get();
        let picked = dlm::selected_rows(&sel, &v.active).len();
        if picked == 0 {
            return bulk_msg
                .get()
                .map(|m| view! { <div class="dl-bulk"><span class="bad">{m}</span></div> })
                .into_any();
        }
        let busy = bulk_busy.get();
        let buttons = dlm::BULK_ACTIONS
            .into_iter()
            .map(|a| {
                let n = dlm::bulk_targets(a, &sel, &v.active).len();
                let cls = if a == BulkAction::RemoveWithFiles {
                    "dl-toolbar-btn danger"
                } else {
                    "dl-toolbar-btn"
                };
                view! {
                    <button type="button" class=cls disabled=busy || n == 0
                        on:click=move |_| run_bulk(a)>
                        {format!("{} ({n})", a.label())}
                    </button>
                }
            })
            .collect_view();
        let on_clear = move |_| {
            selected.set(dlm::Selection::new());
            bulk_msg.set(None);
        };
        view! {
            <div class="dl-bulk" role="toolbar" aria-label="Selected transfers">
                <span class="dl-bulk-count">{format!("{picked} selected")}</span>
                {buttons}
                <button type="button" class="dl-toolbar-btn" on:click=on_clear>"Clear"</button>
                {bulk_msg.get().map(|m| view! { <span class="bad">{m}</span> })}
            </div>
        }
        .into_any()
    };

    // State filter chips with counts over the whole list (SKADI-T-0686).
    let chips = move || {
        let js = jobs.get();
        if !load.is_loaded() || js.is_empty() {
            return ().into_any();
        }
        let current = filter.get();
        let chips = dlm::state_counts(&js)
            .into_iter()
            .map(|(f, n)| {
                let cls = if f == current {
                    "filter-chip active"
                } else {
                    "filter-chip"
                };
                let on_click = move |_| {
                    filter.set(f);
                    local_set(dlm::FILTER_STORAGE_KEY, f.key());
                };
                view! {
                    <button type="button" class=cls on:click=on_click>
                        {f.label()}
                        <span class="chip-count mono">{n.to_string()}</span>
                    </button>
                }
            })
            .collect_view();
        view! { <div class="filter-chips dl-filters">{chips}</div> }.into_any()
    };

    // Queue order controls (SKADI-T-0692): top / up / down on a queued row, for
    // the admin only (the priority route is admin-only). The outcome of a
    // refused move shows above the table until the next move.
    let queue_role = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    let queue_msg = RwSignal::new(None::<String>);
    let queue_busy = RwSignal::new(false);
    let run_move = move |id: String, how: dlm::QueueMove| {
        if queue_busy.get_untracked() {
            return;
        }
        queue_busy.set(true);
        queue_msg.set(None);
        spawn_local(async move {
            if let Err(e) = api::move_download(&id, how.as_str()).await {
                queue_msg.set(Some(format!("Could not move the download: {}", e.0)));
            }
            queue_busy.set(false);
            refetch();
        });
    };
    let queue_msg_view = move || {
        queue_msg
            .get()
            .map(|m| view! { <div class="run-action-msg bad" role="status">{m}</div> })
    };

    // Active transfers (downloading / paused / stalled): compact table-style rows
    // (SKADI-I-0053). One line per torrent:
    // Name | Size | Progress | ↓Speed | ↑Speed | Peers | ETA | Added
    let active_rows = move || {
        let names = titles.get();
        let f = filter.get();
        if f == StateFilter::Only(RowState::Seeding) {
            return ().into_any();
        }
        let visible = shown.get();
        let js = visible.active;
        if js.is_empty() {
            if !load.is_loaded() {
                return ().into_any();
            }
            if f == StateFilter::All || jobs.with(|j| j.is_empty()) {
                return view! { <div class="dl-empty">"No active transfers."</div> }.into_any();
            }
            if !visible.seeding.is_empty() {
                return ().into_any();
            }
            return view! { <div class="dl-empty">"No transfers in this state."</div> }.into_any();
        }
        let now_ms = js_sys::Date::now();
        let q_len = jobs.with(|all| dlm::queue_len(all));
        let q_role = queue_role.and_then(|r| r.get());
        let rows = js
            .into_iter()
            .map(|j| {
                let (label, _tag) = names
                    .get(&j.acquirable_ref)
                    .cloned()
                    .unwrap_or_else(|| (j.acquirable_ref.clone(), "·"));
                let full_name = label.clone();
                let pct = j.percent;
                let state = dlm::row_state(&j);
                let paused = j.status == "paused";
                // A failed job (status `error`) has nothing to pause or resume;
                // Remove still clears it.
                let failed = j.status == "error";
                let row_cls = format!(
                    "dl-row-compact st-{}{}",
                    state.key(),
                    if paused { " paused" } else { "" }
                );

                // Format columns
                let size_str = size_human(j.total_bytes.max(0) as u64);
                let down_str = fmt_speed(j.down_speed_bps);
                let down_cls = if j.down_speed_bps.map(|b| b > 0).unwrap_or(false) {
                    "dl-down mono tnum active"
                } else {
                    "dl-down mono tnum"
                };
                let up_str = fmt_speed(j.up_speed_bps);
                let up_cls = if j.up_speed_bps.map(|b| b > 0).unwrap_or(false) {
                    "dl-up mono tnum active"
                } else {
                    "dl-up mono tnum"
                };
                let peers_str = j.peers.map(|p| p.to_string()).unwrap_or_else(|| "—".into());
                let eta_str = fmt_eta(j.eta_seconds);
                let added_str = added_label(j.created_at.as_deref(), now_ms);
                let added_title = j.created_at.clone().unwrap_or_default();
                // Icon buttons say what they do and to which row (SKADI-T-0700).
                let pr_label = format!("{} {full_name}", if paused { "Resume" } else { "Pause" });
                let rm_label = format!("Remove {full_name} (keep files)");
                let del_label = format!("Delete {full_name} and its files");

                // Row checkbox (SKADI-T-0688): its own reactive read, so a
                // toggle does not rebuild the table.
                let id_sel = j.id.clone();
                let id_chk = j.id.clone();
                let check = view! {
                    <label class="dl-check">
                        <input type="checkbox" aria-label=format!("Select {full_name}")
                            prop:checked=move || selected.with(|s| s.contains(&id_chk))
                            on:change=move |_| selected.update(|s| dlm::toggle_selected(s, &id_sel))/>
                    </label>
                };

                // Action handlers
                let id_pr = j.id.clone();
                let on_pause_resume = move |_| {
                    let id = id_pr.clone();
                    spawn_local(async move {
                        let _ = if paused {
                            api::resume_download(&id).await
                        } else {
                            api::pause_download(&id).await
                        };
                        refetch();
                    });
                };
                let label_rm = label.clone();
                let id_rm = j.id.clone();
                let on_remove = move |_| {
                    let body = format!(
                        "Remove \"{label_rm}\" from the download client? (keeps any downloaded files)"
                    );
                    let id = id_rm.clone();
                    spawn_local(async move {
                        let spec = ConfirmSpec::destructive("Remove the transfer?", body)
                            .confirm_label("Remove");
                        if !confirm(spec).await {
                            return;
                        }
                        let _ = api::remove_download(&id, false).await;
                        refetch();
                    });
                };
                let label_del = label.clone();
                let id_del = j.id.clone();
                let on_delete = move |_| {
                    let body = format!(
                        "Remove \"{label_del}\" AND delete its downloaded files? This can't be undone."
                    );
                    let id = id_del.clone();
                    spawn_local(async move {
                        let spec = ConfirmSpec::destructive("Delete the transfer and its files?", body)
                            .confirm_label("Remove and delete");
                        if !confirm(spec).await {
                            return;
                        }
                        let _ = api::remove_download(&id, true).await;
                        refetch();
                    });
                };

                // Stalled / errored: a badge before the name, and an errored
                // row shows its message in full on its own line (SKADI-T-0687).
                let badge = row_badge_view(&j);
                // Place in the claim order and the queue moves (SKADI-T-0692).
                let q_pos = dlm::queue_label(&j).map(|l| {
                    view! { <span class="dl-qpos mono tnum" title="Place in the queue (#1 is claimed next)">{l}</span> }
                });
                let q_buttons = dlm::queue_moves(&j, q_role.as_deref(), q_len)
                    .into_iter()
                    .map(|(m, enabled)| {
                        let id = j.id.clone();
                        view! {
                            <button type="button" class="dl-qmove" title=m.title() aria-label=m.title()
                                disabled=move || !enabled || queue_busy.get()
                                on:click=move |_| run_move(id.clone(), m)>
                                {m.glyph()}
                            </button>
                        }
                    })
                    .collect_view();
                let error_row = dlm::error_message(&j)
                    .map(|e| view! { <div class="dl-error" title=e.clone()>{e.clone()}</div> });

                view! {
                    <div class=row_cls>
                        {check}
                        <span class="dl-name" title=full_name>{q_pos}{badge}{label}</span>
                        <span class="dl-size mono tnum">{size_str}</span>
                        <span class="dl-progress">
                            <span class="dl-bar">
                                <span class="dl-bar-fill" style=format!("width:{pct:.0}%")></span>
                            </span>
                            <span class="dl-pct tnum">{format!("{pct:.0}%")}</span>
                        </span>
                        <span class=down_cls>{down_str}</span>
                        <span class=up_cls>{up_str}</span>
                        <span class="dl-peers mono tnum">{peers_str}</span>
                        <span class="dl-eta mono tnum">{eta_str}</span>
                        <span class="dl-added mono tnum" title=added_title>{added_str}</span>
                        <div class="dl-actions">
                            {q_buttons}
                            {(!failed).then(|| view! {
                                <button type="button" title={if paused { "Resume" } else { "Pause" }} aria-label=pr_label on:click=on_pause_resume>
                                    {if paused { "▶" } else { "⏸" }}
                                </button>
                            })}
                            <button type="button" title="Remove (keep files)" aria-label=rm_label on:click=on_remove>"✕"</button>
                            <button type="button" class="danger" title="Delete files" aria-label=del_label on:click=on_delete>"🗑"</button>
                        </div>
                        {error_row}
                    </div>
                }
            })
            .collect_view();
        let header = dlm::ACTIVE_COLUMNS
            .into_iter()
            .map(|(key, label)| sort_header(active_sort, dlm::ACTIVE_SORT_STORAGE_KEY, key, label))
            .collect_view();
        // Select-all over the rows on show only (the current filter).
        let all_state =
            move || shown.with(|v| selected.with(|s| dlm::select_all_state(s, &v.active)));
        let on_all = move |_| {
            shown.with_untracked(|v| selected.update(|s| dlm::toggle_all(s, &v.active)));
        };
        view! {
            <div class="dl-table">
                <div class="dl-header">
                    <label class="dl-check">
                        <input type="checkbox" aria-label="Select all shown"
                            prop:checked=move || all_state() == SelectAll::All
                            prop:indeterminate=move || all_state() == SelectAll::Some
                            on:change=on_all/>
                    </label>
                    {header}
                </div>
                {rows}
            </div>
        }
        .into_any()
    };

    // Manual import of a finished transfer (SKADI-T-0689). Offered to the admin
    // only (the import routes are admin-only on the API); the role is read in
    // the row closure, so an unresolved role offers nothing. One preview is open
    // at a time; its plan, the per-row result or error, and the rows imported
    // here this session live apart from the polled rows, so a poll keeps them.
    let role = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    let import_open = RwSignal::new(None::<String>);
    let import_plan = RwSignal::new(None::<Result<api::ImportPlan, String>>);
    let import_busy = RwSignal::new(false);
    let import_msg = RwSignal::new(HashMap::<String, (bool, String)>::new());
    let imported_here = RwSignal::new(HashSet::<String>::new());
    let open_import = move |id: String, facts: api::DownloadImport| {
        import_open.set(Some(id.clone()));
        import_plan.set(None);
        import_msg.update(|m| {
            m.remove(&id);
        });
        spawn_local(async move {
            let plan = api::preview_import(&api::ManualImportRequest::for_download(&facts))
                .await
                .map_err(|e| e.0);
            // A preview for a panel that was closed or moved on is dropped.
            if import_open.get_untracked().as_deref() == Some(id.as_str()) {
                import_plan.set(Some(plan));
            }
        });
    };
    let run_import = move |id: String, facts: api::DownloadImport| {
        if import_busy.get_untracked() {
            return;
        }
        let question = import_plan.with_untracked(|p| match p {
            Some(Ok(plan)) => dlm::import_confirm_message(plan),
            _ => None,
        });
        spawn_local(async move {
            if let Some(body) = question {
                let spec = ConfirmSpec::destructive("Replace library files?", body)
                    .confirm_label("Replace and import");
                if !confirm(spec).await || import_busy.get_untracked() {
                    return;
                }
            }
            import_busy.set(true);
            let result = api::run_import(&api::ManualImportRequest::for_download(&facts)).await;
            let (ok, text) = match result {
                Ok(outcome) => dlm::import_result(&outcome),
                Err(e) => (false, e.0),
            };
            if ok {
                imported_here.update(|s| {
                    s.insert(id.clone());
                });
                import_open.set(None);
                import_plan.set(None);
            }
            import_msg.update(|m| {
                m.insert(id, (ok, text));
            });
            import_busy.set(false);
            refetch();
        });
    };
    let close_import = move || {
        import_open.set(None);
        import_plan.set(None);
    };

    // Seeding: collapsible accordion below active downloads (SKADI-T-0373).
    // Collapsed by default; state persists via localStorage. The Seeding chip
    // opens it, since it is then the only list on show.
    let seeding_open = RwSignal::new(
        local_get("seeding_open")
            .map(|v| v == "true")
            .unwrap_or(false),
    );
    let seeding_rows = move || {
        let names = titles.get();
        let js = shown.get().seeding;
        if js.is_empty() {
            if load.is_loaded()
                && filter.get() == StateFilter::Only(RowState::Seeding)
                && !jobs.with(|j| j.is_empty())
            {
                return view! { <div class="dl-empty">"No transfers in this state."</div> }
                    .into_any();
            }
            return ().into_any();
        }
        let count = js.len();
        let now_ms = js_sys::Date::now();
        let rows = js
            .into_iter()
            .map(|j| {
                let (label, tag) = names
                    .get(&j.acquirable_ref)
                    .cloned()
                    .unwrap_or_else(|| (j.acquirable_ref.clone(), "·"));
                let size = size_human(j.total_bytes.max(0) as u64);
                let up = j
                    .up_speed_bps
                    .filter(|b| *b > 0)
                    .map(|b| format!("↑ {}", fmt_speed(Some(b))))
                    .unwrap_or_default();
                let ratio_val = j.ratio.unwrap_or(0.0);
                let ratio = j.ratio.map(|r| format!("ratio {r:.2}")).unwrap_or_default();
                let ratio_cls = if ratio_val <= 0.0 {
                    "seed-ratio mono faint"
                } else if ratio_val < 1.0 {
                    "seed-ratio mono"
                } else {
                    "seed-ratio mono ok"
                };
                let added_str = added_label(j.created_at.as_deref(), now_ms);
                let added_title = j.created_at.clone().unwrap_or_default();
                let state = dlm::row_state(&j);
                let row_cls = format!("seed-row st-{}", state.key());
                let dot_cls = format!("health-dot {}", state.level());
                let badge = row_badge_view(&j);
                let error_line = dlm::error_message(&j)
                    .map(|e| view! { <div class="dl-error seed-error" title=e.clone()>{e.clone()}</div> });

                // Manual import (SKADI-T-0689): the button, the importer's
                // reason, the preview under the row, and the result or error.
                let done_here = imported_here.with(|s| s.contains(&j.id));
                let action = imported_here.with(|s| {
                    dlm::import_action(&j, role.and_then(|r| r.get()).as_deref(), s)
                });
                let import_note = (!done_here)
                    .then(|| dlm::import_note(&j))
                    .flatten()
                    .map(|n| view! { <div class="dl-error seed-error" title=n.clone()>{n.clone()}</div> });
                let facts = j.import.clone();
                let import_btn = action.zip(facts.clone()).map(|(a, f)| {
                    let id = j.id.clone();
                    view! {
                        <button type="button" class="dl-toolbar-btn seed-import"
                            on:click=move |_| open_import(id.clone(), f.clone())>{a.label()}</button>
                    }
                });
                let panel = action
                    .zip(facts)
                    .filter(|_| import_open.with(|o| o.as_deref() == Some(j.id.as_str())))
                    .map(|(a, f)| {
                        let target = names
                            .get(&f.acquirable_ref)
                            .map(|(n, _)| n.clone())
                            .unwrap_or_else(|| f.acquirable_ref.clone());
                        import_panel_view(
                            a,
                            &target,
                            &f,
                            import_plan.get(),
                            import_busy.get(),
                            {
                                let (id, f) = (j.id.clone(), f.clone());
                                move || run_import(id.clone(), f.clone())
                            },
                            close_import,
                        )
                    });
                let msg_line = import_msg.with(|m| m.get(&j.id).cloned()).map(|(ok, t)| {
                    let cls = if ok { "seed-import-msg ok" } else { "seed-import-msg bad" };
                    view! { <div class=cls role="status">{t}</div> }
                });
                view! {
                    <div class=row_cls>
                        <span class=dot_cls></span>
                        <span class="tile-tag mono">{tag}</span>
                        <span class="seed-title">{badge}{label}</span>
                        <span class="seed-size mono">{size}</span>
                        <span class="seed-up mono gold">{up}</span>
                        <span class=ratio_cls>{ratio}</span>
                        <span class="seed-added mono faint" title=added_title>{added_str}</span>
                        {import_btn}
                        {error_line}
                        {import_note}
                        {msg_line}
                        {panel}
                    </div>
                }
            })
            .collect_view();
        // Toggle handler persists to localStorage
        let on_toggle = move || {
            let next = !seeding_open.get();
            seeding_open.set(next);
            local_set("seeding_open", if next { "true" } else { "false" });
        };
        let is_open =
            move || seeding_open.get() || filter.get() == StateFilter::Only(RowState::Seeding);
        let chevron = move || if is_open() { "▼" } else { "▶" };
        let header = dlm::SEEDING_COLUMNS
            .into_iter()
            .map(|(key, label)| {
                sort_header(seeding_sort, dlm::SEEDING_SORT_STORAGE_KEY, key, label)
            })
            .collect_view();
        view! {
            <div class="seed-accordion">
                <div
                    class="seed-header"
                    role="button"
                    tabindex="0"
                    aria-expanded=move || crate::a11y::expanded(is_open())
                    on:click=move |_| on_toggle()
                    on:keydown=move |ev| {
                        if crate::a11y::activates(&ev) {
                            on_toggle();
                        }
                    }
                >
                    <span class="seed-chevron" aria-hidden="true">{chevron}</span>
                    <span class="u-label">{format!("Seeding ({count})")}</span>
                </div>
                <div class="seed-body" class:collapsed=move || !is_open()>
                    <div class="seed-sort"><span class="u-label">"Sort"</span>{header}</div>
                    {rows}
                </div>
            </div>
        }
        .into_any()
    };

    // Sticky toolbar with global controls and aggregate stats (SKADI-T-0369).
    let toolbar = move || {
        let js = jobs.get();
        if !load.is_loaded() {
            return ().into_any();
        }
        let dl = js.iter().filter(|j| j.status == "downloading").count();
        let qd = js
            .iter()
            .filter(|j| j.status == "queued" || j.status == "stalled")
            .count();
        let sd = js.iter().filter(|j| j.status == "seeding").count();
        let paused = js.iter().filter(|j| j.status == "paused").count();
        let down: i64 = js.iter().filter_map(|j| j.down_speed_bps).sum();
        let up: i64 = js.iter().filter_map(|j| j.up_speed_bps).sum();

        let any_active = dl > 0 || qd > 0;
        let any_paused = paused > 0;

        // Counts: "X downloading · Y seeding"
        let mut counts = Vec::new();
        if dl + qd + paused > 0 {
            counts.push(format!("{} active", dl + qd + paused));
        }
        if sd > 0 {
            counts.push(format!("{sd} seeding"));
        }
        let counts_str = if counts.is_empty() {
            "Idle".into()
        } else {
            counts.join(" · ")
        };

        // Aggregate speeds
        let down_str = if down > 0 {
            format!("↓ {}", fmt_speed(Some(down)))
        } else {
            "↓ —".into()
        };
        let up_str = if up > 0 {
            format!("↑ {}", fmt_speed(Some(up)))
        } else {
            "↑ —".into()
        };

        let on_pause_all = move |_| {
            spawn_local(async move {
                let _ = api::pause_all().await;
                refetch();
            })
        };
        let on_resume_all = move |_| {
            spawn_local(async move {
                let _ = api::resume_all().await;
                refetch();
            })
        };

        view! {
            <div class="dl-toolbar">
                <div class="dl-toolbar-controls">
                    {any_active.then(|| view! {
                        <button type="button" class="dl-toolbar-btn" on:click=on_pause_all>"⏸ Pause All"</button>
                    })}
                    {any_paused.then(|| view! {
                        <button type="button" class="dl-toolbar-btn" on:click=on_resume_all>"▶ Resume All"</button>
                    })}
                </div>
                <div class="dl-toolbar-stats mono tnum">
                    <span class="dl-toolbar-speed ice">{down_str}</span>
                    <span class="dl-toolbar-speed gold">{up_str}</span>
                </div>
                <div class="dl-toolbar-counts muted">{counts_str}</div>
            </div>
        }
        .into_any()
    };

    view! {
        <section class="provider-section dl-section">
            {toolbar}
            {chips}
            {bulk_bar}
            {queue_msg_view}
            {load.status(SkeletonKind::Rows, Callback::new(move |()| refetch()))}
            {active_rows}
            {seeding_rows}
            {move || (!jobs.get().is_empty()).then_some(()).map(|_| view! {
                <p class="muted dl-footnote">"Failed transfers are in "<A href="/activity">"Activity"</A>"."</p>
            })}
        </section>
    }
}

/// The preview of a manual import, under its row (SKADI-T-0689): where the scan
/// starts, the item it is for, what each file would do, and the confirm button
/// (off while the preview loads, has nothing to import, or an import runs). An
/// error from the preview endpoint is shown here, in place of the plan.
fn import_panel_view(
    action: crate::downloads::ImportAction,
    target: &str,
    facts: &api::DownloadImport,
    plan: Option<Result<api::ImportPlan, String>>,
    busy: bool,
    on_confirm: impl Fn() + 'static,
    on_cancel: impl Fn() + 'static,
) -> AnyView {
    use crate::downloads::{ImportAction, plan_view};
    let body = match plan {
        None => view! { <p class="muted">"Loading the preview…"</p> }.into_any(),
        Some(Err(e)) => {
            view! { <div class="seed-import-msg bad" role="status">{e}</div> }.into_any()
        }
        Some(Ok(plan)) => {
            let v = plan_view(&plan, &facts.acquirable_ref);
            let match_note = match (action, v.can_import, v.same_target) {
                (_, false, _) => None,
                (ImportAction::Retry, true, true) => Some("The match is the same as the grab's."),
                (_, true, false) => {
                    Some("The files match other items than the grab's: check the list.")
                }
                (ImportAction::Import, true, true) => None,
            };
            let lines = v
                .lines
                .into_iter()
                .map(|l| {
                    let cls = format!("seed-plan-kind {}", l.kind.label());
                    view! {
                        <li>
                            <span class=cls>{l.kind.label()}</span>
                            <span class="mono" title=l.path.clone()>{l.path.clone()}</span>
                            <span class="faint">{l.detail}</span>
                        </li>
                    }
                })
                .collect_view();
            let can = v.can_import && !busy;
            view! {
                <p class="seed-plan-summary">{v.summary}</p>
                {match_note.map(|n| view! { <p class="muted">{n}</p> })}
                <ul class="seed-plan">{lines}</ul>
                <button type="button" class="dl-toolbar-btn" disabled=!can
                    on:click=move |_| on_confirm()>
                    {if busy { "Importing…" } else { action.confirm_label() }}
                </button>
            }
            .into_any()
        }
    };
    view! {
        <div class="seed-import-panel">
            <div class="u-label">"Import preview"</div>
            <p class="muted">"For " <strong>{target.to_string()}</strong></p>
            <p class="mono faint" title=facts.path.clone()>{facts.path.clone()}</p>
            {body}
            <button type="button" class="dl-toolbar-btn" on:click=move |_| on_cancel()>"Cancel"</button>
        </div>
    }
    .into_any()
}

/// Bytes/sec → "KB/s" string for the form (`0` ⇒ "0", i.e. unlimited). Pure.
fn bps_to_kbps(bps: u64) -> String {
    if bps == 0 {
        "0".into()
    } else {
        format!("{}", bps as f64 / 1000.0)
    }
}

/// "KB/s" string → bytes/sec, clamped non-negative (`""`/junk ⇒ 0). Pure.
fn kbps_to_bps(s: &str) -> u64 {
    (s.trim().parse::<f64>().unwrap_or(0.0).max(0.0) * 1000.0).round() as u64
}

/// Live worker-settings form (SKADI-T-0291): bandwidth caps, max-active, seed
/// policy. Writes to the config plane; the worker hot-applies within a tick — no
/// restart. Tucked in a disclosure so the control panel leads with the transfers.
#[component]
fn DownloadSettingsForm() -> impl IntoView {
    let max_active = RwSignal::new(String::new());
    let down_kbps = RwSignal::new(String::new());
    let up_kbps = RwSignal::new(String::new());
    let seed_ratio = RwSignal::new(String::new());
    let seed_time = RwSignal::new(String::new());
    let seed_action = RwSignal::new(String::from("stop"));
    let saving = RwSignal::new(false);
    let msg = RwSignal::new(String::new());

    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(s) = api::download_settings().await {
                max_active.set(s.max_active.to_string());
                down_kbps.set(bps_to_kbps(s.down_limit_bps));
                up_kbps.set(bps_to_kbps(s.up_limit_bps));
                seed_ratio.set(format!("{}", s.seed_ratio));
                seed_time.set(s.seed_time_mins.to_string());
                seed_action.set(s.seed_action);
            }
        });
    });

    let on_save = move |_| {
        saving.set(true);
        msg.set(String::new());
        let s = api::DownloadSettings {
            max_active: max_active.get_untracked().trim().parse().unwrap_or(0),
            down_limit_bps: kbps_to_bps(&down_kbps.get_untracked()),
            up_limit_bps: kbps_to_bps(&up_kbps.get_untracked()),
            seed_ratio: seed_ratio.get_untracked().trim().parse().unwrap_or(0.0),
            seed_time_mins: seed_time.get_untracked().trim().parse().unwrap_or(0),
            seed_action: seed_action.get_untracked(),
        };
        spawn_local(async move {
            match api::set_download_settings(&s).await {
                Ok(()) => msg.set("Saved — the worker applies this within a few seconds.".into()),
                Err(e) => msg.set(format!("Save failed: {}", e.0)),
            }
            saving.set(false);
        });
    };

    view! {
        <details class="dl-settings">
            <summary>"Settings — bandwidth · queue · seeding"</summary>
            <div class="dl-settings-body">
                <label class="dl-field">
                    <span>"Max active downloads"</span>
                    <input type="number" min="0" prop:value=move || max_active.get()
                        on:input=move |ev| max_active.set(event_target_value(&ev)) />
                    <span class="muted dl-hint">"0 = unlimited"</span>
                </label>
                <label class="dl-field">
                    <span>"Download cap (KB/s)"</span>
                    <input type="number" min="0" step="10" prop:value=move || down_kbps.get()
                        on:input=move |ev| down_kbps.set(event_target_value(&ev)) />
                    <span class="muted dl-hint">"0 = unlimited"</span>
                </label>
                <label class="dl-field">
                    <span>"Upload cap (KB/s)"</span>
                    <input type="number" min="0" step="10" prop:value=move || up_kbps.get()
                        on:input=move |ev| up_kbps.set(event_target_value(&ev)) />
                    <span class="muted dl-hint">"0 = unlimited · default 100"</span>
                </label>
                <label class="dl-field">
                    <span>"Seed ratio"</span>
                    <input type="number" min="0" step="0.1" prop:value=move || seed_ratio.get()
                        on:input=move |ev| seed_ratio.set(event_target_value(&ev)) />
                    <span class="muted dl-hint">"0 = no ratio limit"</span>
                </label>
                <label class="dl-field">
                    <span>"Seed time (minutes)"</span>
                    <input type="number" min="0" prop:value=move || seed_time.get()
                        on:input=move |ev| seed_time.set(event_target_value(&ev)) />
                    <span class="muted dl-hint">"0 = no time limit"</span>
                </label>
                <label class="dl-field">
                    <span>"On seed limit"</span>
                    <select prop:value=move || seed_action.get()
                        on:change=move |ev| seed_action.set(event_target_value(&ev))>
                        <option value="stop">"Stop seeding (keep data)"</option>
                        <option value="remove">"Stop + remove torrent (keep data)"</option>
                    </select>
                </label>
                <div class="dl-settings-actions">
                    <button type="button" on:click=on_save disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                    <span class="muted dl-save-msg">{move || msg.get()}</span>
                </div>
            </div>
        </details>
    }
}
