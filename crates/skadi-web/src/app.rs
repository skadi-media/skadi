//! The app shell: persistent left sidebar (brand + health, then Home / Domains
//! / System groups) over a routed main area (SKADI-T-0071). Split out of
//! `main.rs` into the lib so the bin stays a thin mount point.

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
use crate::dashboard::Dashboard;
use crate::import::LibraryImportPage;
use crate::movies::{MovieDetailPage, MoviesConfigPage, MoviesPage};
use crate::settings::{ProviderSection, indexer_spec, window_confirm};
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

    view! {
        <Show when=move || needs_login.get() fallback=Shell>
            <LoginPage/>
        </Show>
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
            <div class="app">
                <Sidebar/>
                <main class="main">
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
                </main>
            </div>
        </Router>
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

    // Which areas this account can actually reach (SKADI-T-0627). The API has
    // gated these since SKADI-T-0625, but the sidebar offered them to everyone
    // and let the 403 do the talking. Four links is little enough that a dead
    // one is conspicuous, so ask first. The role comes from the shell, which
    // sits above the router and so can hand the same answer to the section
    // strips on the pages.

    view! {
        <nav class="nav">
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
fn IndexersPage() -> impl IntoView {
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
fn DownloadersPage() -> impl IntoView {
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

/// One value from localStorage (`None` when storage is unavailable).
fn local_get(key: &str) -> Option<String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
}

/// Best-effort localStorage write (private mode / quota: ignored).
fn local_set(key: &str, value: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
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
    use crate::downloads::{self as dlm, RowState, SortKey, SortState, StateFilter};
    use crate::movies::size_human;
    use std::collections::HashMap;

    let jobs = RwSignal::new(Vec::<api::Download>::new());
    // acquirable-ref -> (title, media tag), so a job shows a name + FILM/BOOK chip.
    let titles = RwSignal::new(HashMap::<String, (String, &'static str)>::new());
    let loaded = RwSignal::new(false);
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

    // Re-fetch the list now (after a control action), so the UI reflects it without
    // waiting for the next poll tick.
    let refetch = move || {
        spawn_local(async move {
            if let Ok(d) = api::list_downloads().await {
                jobs.set(d);
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
        spawn_local(async move {
            loop {
                if let Ok(d) = api::list_downloads().await {
                    jobs.set(d);
                }
                loaded.set(true);
                gloo_timers::future::TimeoutFuture::new(DOWNLOADS_POLL_MS).await;
                if !alive.try_get_untracked().unwrap_or(false) {
                    break;
                }
            }
        });
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
            <span
                class=move || if sort.get().key == key { "sort-active" } else { "" }
                on:click=on_click
            >
                {label}{move || sort.get().arrow(key)}
            </span>
        }
    };

    // State filter chips with counts over the whole list (SKADI-T-0686).
    let chips = move || {
        let js = jobs.get();
        if !loaded.get() || js.is_empty() {
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
            if !loaded.get() {
                return view! { <div class="dl-empty">"Loading…"</div> }.into_any();
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
                    if !window_confirm(&format!(
                        "Remove \"{label_rm}\" from the download client? (keeps any downloaded files)"
                    )) {
                        return;
                    }
                    let id = id_rm.clone();
                    spawn_local(async move {
                        let _ = api::remove_download(&id, false).await;
                        refetch();
                    });
                };
                let label_del = label.clone();
                let id_del = j.id.clone();
                let on_delete = move |_| {
                    if !window_confirm(&format!(
                        "Remove \"{label_del}\" AND delete its downloaded files? This can't be undone."
                    )) {
                        return;
                    }
                    let id = id_del.clone();
                    spawn_local(async move {
                        let _ = api::remove_download(&id, true).await;
                        refetch();
                    });
                };

                let error_row = j.error.clone().map(|e| view! { <div class="dl-error">{e}</div> });

                view! {
                    <div class=row_cls>
                        <span class="dl-name" title=full_name>{label}</span>
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
                            <button type="button" title={if paused { "Resume" } else { "Pause" }} on:click=on_pause_resume>
                                {if paused { "▶" } else { "⏸" }}
                            </button>
                            <button type="button" title="Remove (keep files)" on:click=on_remove>"✕"</button>
                            <button type="button" class="danger" title="Delete files" on:click=on_delete>"🗑"</button>
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
        view! {
            <div class="dl-table">
                <div class="dl-header">{header}</div>
                {rows}
            </div>
        }
        .into_any()
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
            if loaded.get()
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
                let row_cls = format!("seed-row st-{}", dlm::row_state(&j).key());
                view! {
                    <div class=row_cls>
                        <span class="health-dot ok"></span>
                        <span class="tile-tag mono">{tag}</span>
                        <span class="seed-title">{label}</span>
                        <span class="seed-size mono">{size}</span>
                        <span class="seed-up mono gold">{up}</span>
                        <span class=ratio_cls>{ratio}</span>
                        <span class="seed-added mono faint" title=added_title>{added_str}</span>
                    </div>
                }
            })
            .collect_view();
        // Toggle handler persists to localStorage
        let on_toggle = move |_| {
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
                <div class="seed-header" on:click=on_toggle>
                    <span class="seed-chevron">{chevron}</span>
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
        if !loaded.get() {
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
            {active_rows}
            {seeding_rows}
            {move || (!jobs.get().is_empty()).then_some(()).map(|_| view! {
                <p class="muted dl-footnote">"Failed transfers are in "<A href="/activity">"Activity"</A>"."</p>
            })}
        </section>
    }
}

/// Poll cadence for the torrent list (ms) — near-real-time so active transfers
/// tick smoothly (progress/speed/peers). Cheap query; fine for a home deploy.
const DOWNLOADS_POLL_MS: u32 = 500;

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
