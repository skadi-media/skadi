//! Home system dashboard (SKADI-T-0072).
//!
//! An at-a-glance view grouped into **System / Health / Operational / Domains**
//! sections. Wires the metrics that are cheap today — daemon version
//! (`/health`), aggregated health checks (`/health/checks`, T-0116), root-folder
//! free space (`/root-folders`, T-0116), in-flight count (`/activity`), and one
//! card per enabled domain (`/domains`) with counts derived client-side from
//! that domain's library — `/movies` for movies, `/books` for audiobooks
//! (T-0143). Sensors without a backend (download throughput, VPN egress) render
//! as labelled placeholders — never faked numbers.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;

use crate::api;
use crate::movies::size_human;

/// Human transfer rate from bytes/s, e.g. `14.2 MB/s`; empty when zero/unknown. Pure.
fn rate(bps: i64) -> String {
    if bps > 0 {
        format!("{}/s", size_human(bps as u64))
    } else {
        String::new()
    }
}

/// CSS dot class for a health-check status (`ok`/`warn`/`fail`).
pub fn check_class(status: &str) -> &'static str {
    match status {
        "ok" => "ok",
        "warn" => "pending",
        _ => "bad",
    }
}

/// Presentational health-badge grid (SKADI-T-0119): pure render of `checks`,
/// each with an ok/warn/fail status dot. Extracted so it's unit-mountable.
#[component]
pub fn HealthChecks(checks: Vec<api::HealthCheck>) -> impl IntoView {
    if checks.is_empty() {
        return view! { <p class="muted">"No checks reported."</p> }.into_any();
    }
    checks
        .into_iter()
        .map(|c| {
            let cls = check_class(&c.status);
            let metric_cls = if cls == "bad" {
                "metric check bad"
            } else {
                "metric check"
            };
            view! {
                <div class=metric_cls>
                    <span class=format!("status-dot {cls}")></span>
                    <div class="metric-body">
                        <span class="metric-name">{c.name}</span>
                        <span class="muted metric-detail wrap">{c.detail}</span>
                    </div>
                </div>
            }
        })
        .collect_view()
        .into_any()
}

/// Per-domain library tallies for a Domains card. Extracted from the render so
/// the derivation is unit-testable without a DOM (SKADI-T-0118).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DomainCounts {
    pub total: usize,
    pub monitored: usize,
    pub imported: usize,
    pub in_progress: usize,
    pub missing: usize,
}

/// Tally a movie list into [`DomainCounts`]. A movie is **imported** if any
/// edition is imported/cutoff, else **in progress** if any is mid-acquisition,
/// else **missing** — matching the poster-tile rollup.
pub fn movie_counts(movies: &[api::Movie]) -> DomainCounts {
    let mut c = DomainCounts {
        total: movies.len(),
        ..Default::default()
    };
    for m in movies {
        if m.monitored {
            c.monitored += 1;
        }
        let labels: Vec<String> = m
            .editions
            .iter()
            .map(|e| api::status_label(&e.status))
            .collect();
        if labels.iter().any(|l| l == "imported" || l == "cutoff") {
            c.imported += 1;
        } else if labels
            .iter()
            .any(|l| matches!(l.as_str(), "searching" | "snatched" | "downloading"))
        {
            c.in_progress += 1;
        } else {
            c.missing += 1;
        }
    }
    c
}

/// Tally a book list into [`DomainCounts`], reusing the audiobook cover-tile
/// rollup ([`crate::audiobooks::book_status`]) so the dashboard agrees with the
/// library view. (SKADI-T-0143.)
pub fn book_counts(books: &[api::Book]) -> DomainCounts {
    let mut c = DomainCounts {
        total: books.len(),
        ..Default::default()
    };
    for b in books {
        if b.monitored {
            c.monitored += 1;
        }
        match crate::audiobooks::book_status(b).1 {
            "imported" => c.imported += 1,
            "in progress" => c.in_progress += 1,
            _ => c.missing += 1,
        }
    }
    c
}

/// Tally a series list into [`DomainCounts`], reusing the TV library-wall rollup
/// ([`crate::tv::series_lib_status`]) so the dashboard agrees with the TV view: a
/// series is **imported** (owned) when every episode is present, **in progress**
/// while any is mid-acquisition, else **missing/wanted** (SKADI-T-0118 parity).
pub fn series_counts(series: &[api::Series]) -> DomainCounts {
    let mut c = DomainCounts {
        total: series.len(),
        ..Default::default()
    };
    for s in series {
        if s.monitored {
            c.monitored += 1;
        }
        match crate::tv::series_lib_status(s) {
            "owned" => c.imported += 1,
            "downloading" => c.in_progress += 1,
            _ => c.missing += 1,
        }
    }
    c
}

/// Friendly title for a domain card, from the domain's `kind`.
fn domain_title(kind: &str, name: &str) -> String {
    match kind {
        "movie" => "Movies".into(),
        "audiobook" => "Audiobooks".into(),
        "series" => "Television".into(),
        _ => name.to_string(),
    }
}

#[component]
pub fn Dashboard() -> impl IntoView {
    let version = RwSignal::new(None::<String>);
    let checks = RwSignal::new(Vec::<api::HealthCheck>::new());
    // `false` until the (async) /health/checks request returns, so the strip can show
    // a "checking…" placeholder instead of a misleading "No checks reported." gap.
    let checks_loaded = RwSignal::new(false);
    let movies = RwSignal::new(Vec::<api::Movie>::new());
    let books = RwSignal::new(Vec::<api::Book>::new());
    let series = RwSignal::new(Vec::<api::Series>::new());
    let domains = RwSignal::new(Vec::<api::Domain>::new());
    let downloads = RwSignal::new(Vec::<api::Download>::new());
    let history = RwSignal::new(Vec::<api::HistoryRow>::new());
    // acquirable-ref → (title, kind) so Active-hunt rows show a name + media tag.
    let titles = RwSignal::new(HashMap::<String, (String, &'static str)>::new());

    // Each metric loads independently — a slow/failed one never blanks the rest.
    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(h) = api::health().await {
                version.set(Some(h.version));
            }
        });
        spawn_local(async move {
            if let Ok(c) = api::health_checks().await {
                checks.set(c);
            }
            checks_loaded.set(true);
        });
        spawn_local(async move {
            let mut map = HashMap::<String, (String, &'static str)>::new();
            if let Ok(m) = api::list_movies().await {
                for mv in &m {
                    for e in &mv.editions {
                        map.insert(e.id.clone(), (mv.title.clone(), "FILM"));
                    }
                }
                movies.set(m);
            }
            if let Ok(b) = api::list_books(None, None).await {
                for bk in &b {
                    for f in &bk.files {
                        map.insert(f.id.clone(), (bk.title.clone(), "BOOK"));
                    }
                }
                books.set(b);
            }
            if let Ok(s) = api::list_series().await {
                for sr in &s {
                    for ep in &sr.episodes {
                        map.insert(ep.id.clone(), (sr.title.clone(), "TV"));
                    }
                }
                series.set(s);
            }
            titles.set(map);
        });
        spawn_local(async move {
            if let Ok(d) = api::list_domains().await {
                domains.set(d);
            }
        });
        spawn_local(async move {
            if let Ok(d) = api::list_downloads().await {
                downloads.set(d);
            }
        });
        spawn_local(async move {
            if let Ok(h) = api::history(25).await {
                history.set(h);
            }
        });
    });

    // Derived library tallies (client-side, cheap) for the metric row + rollups.
    let mc = move || movie_counts(&movies.get());
    let bc = move || book_counts(&books.get());
    let sc = move || series_counts(&series.get());
    let total = move || mc().total + bc().total + sc().total;
    let monitored = move || mc().monitored + bc().monitored + sc().monitored;
    let wanted = move || mc().missing + bc().missing + sc().missing;
    let active = move || {
        downloads
            .get()
            .into_iter()
            .filter(|d| d.status == "downloading")
            .collect::<Vec<_>>()
    };
    let downloading = move || active().len();
    let throughput = move || {
        active()
            .iter()
            .filter_map(|d| d.down_speed_bps)
            .filter(|b| *b > 0)
            .sum::<i64>()
    };

    // Active-hunt rows: each downloading transfer with a name + % + bar + speed.
    let hunt_rows = move || {
        let names = titles.get();
        let ds = active();
        if ds.is_empty() {
            return view! {
                <div class="hunt-empty">"No active transfers — everything imported."</div>
            }
            .into_any();
        }
        ds.into_iter()
            .map(|d| {
                // The titles map is keyed by library file/edition id; a download's
                // `acquirable_ref` is already a human-readable release title, so fall
                // back to it directly rather than showing "(unknown)".
                let (title, tag) = names
                    .get(&d.acquirable_ref)
                    .cloned()
                    .unwrap_or_else(|| (d.acquirable_ref.clone(), "·"));
                let pct = d.percent.round().clamp(0.0, 100.0);
                let speed = rate(d.down_speed_bps.unwrap_or(0));
                view! {
                    <div class="hunt-row">
                        <div class="hunt-row-head">
                            <span class="tile-tag mono">{tag}</span>
                            <span class="hunt-title">{title}</span>
                            <span class="hunt-pct tnum">{format!("{pct:.0}%")}</span>
                        </div>
                        <div class="hunt-bar">
                            <div class="hunt-bar-fill" style=format!("width:{pct:.0}%")></div>
                        </div>
                        <div class="hunt-meta mono">{speed}</div>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    // Recently imported: the import feed (history), newest first, quality in gold.
    let imported_rows = move || {
        let imported: Vec<_> = history
            .get()
            .into_iter()
            .filter(|h| h.event == "imported")
            .take(8)
            .collect();
        if imported.is_empty() {
            return view! { <p class="muted">"Nothing imported yet."</p> }.into_any();
        }
        imported
            .into_iter()
            .map(|h| {
                view! {
                    <div class="imp-row">
                        <span class="health-dot ok"></span>
                        <span class="imp-title">{h.label}</span>
                        <span class="imp-meta mono">{h.detail.unwrap_or_default()}</span>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    // Rollup cards (one per enabled domain) → that library.
    let rollups = move || {
        domains
            .get()
            .into_iter()
            .filter(|d| d.enabled)
            .filter_map(|d| {
                let (c, accent, href) = match d.kind.as_str() {
                    "movie" => (mc(), "ice", "/movies"),
                    "audiobook" => (bc(), "teal", "/audiobooks"),
                    "series" => (sc(), "gold", "/tv"),
                    _ => return None,
                };
                let title = domain_title(&d.kind, &d.name);
                Some(view! {
                    <A href=href attr:class="rollup">
                        <div class="rollup-head">
                            <span class=format!("lib-accent {accent}")></span>
                            <strong>{title}</strong>
                        </div>
                        <span class="rollup-stats mono">
                            {format!(
                                "{} items · {} wanted · {} downloading",
                                c.total,
                                c.missing,
                                c.in_progress,
                            )}
                        </span>
                    </A>
                })
            })
            .collect_view()
    };

    // Health strip chips from the aggregated checks.
    let health_strip = move || {
        // Greedy: render placeholder chips immediately while the async /health/checks
        // request is in flight, so the strip appears instantly and fills in — rather
        // than sitting blank/"No checks reported." until the (cold) request returns.
        if !checks_loaded.get() {
            return (0..5)
                .map(|_| {
                    view! {
                        <div class="health-chip">
                            <span class="health-dot"></span>
                            <div class="health-chip-body">
                                <span class="health-chip-name muted">"checking…"</span>
                            </div>
                        </div>
                    }
                })
                .collect_view()
                .into_any();
        }
        let cs = checks.get();
        if cs.is_empty() {
            return view! { <p class="muted">"No checks reported."</p> }.into_any();
        }
        // Collapse the (many) indexer checks into ONE rollup chip instead of a
        // wall of identical red/green tiles — "all indexers down/up" reads as a
        // summary and the individual reasons live on the Indexers page
        // (SKADI-T-0348). Infra checks stay as their own chips.
        let (indexers, infra): (Vec<_>, Vec<_>) =
            cs.into_iter().partition(|c| c.name.starts_with("indexer:"));
        let infra_chips = infra
            .into_iter()
            // A domain that is enabled and fine is the normal state of three
            // chips out of nine; it earns a chip only when something is wrong
            // (SKADI-T-0580).
            .filter(|c| !(c.name.starts_with("domain:") && c.status == "ok"))
            .map(|c| {
                let cls = check_class(&c.status);
                let name = c.name.strip_prefix("domain:").map(str::to_string).unwrap_or(c.name.clone());
                view! {
                    <div class="health-chip">
                        <span class=format!("health-dot {cls}")></span>
                        <div class="health-chip-body">
                            <span class="health-chip-name">{name}</span>
                            <span class="health-chip-detail mono">{c.detail}</span>
                        </div>
                    </div>
                }
            })
            .collect_view();
        let rollup = (!indexers.is_empty()).then(|| {
            let total = indexers.len();
            let healthy = indexers.iter().filter(|c| c.status == "ok").count();
            let cls = if healthy == total {
                "ok"
            } else if healthy == 0 {
                "bad"
            } else {
                "warn"
            };
            let detail = if healthy == total {
                format!("{total} reachable")
            } else {
                format!("{healthy}/{total} reachable — check Indexers")
            };
            view! {
                <A href="/indexers" attr:class="health-chip health-rollup">
                    <span class=format!("health-dot {cls}")></span>
                    <div class="health-chip-body">
                        <span class="health-chip-name">"Indexers"</span>
                        <span class="health-chip-detail mono">{detail}</span>
                    </div>
                </A>
            }
        });
        view! { <>{infra_chips}{rollup}</> }.into_any()
    };

    let navigate = use_navigate();
    let go_add = Callback::new(move |()| navigate("/add", Default::default()));

    // "Search or add anything" is the *catalog* search — the first step of
    // adding, which needs `can_contribute()`. A read-only member who clicked
    // it landed on a page that answered 403 to every query (operator report,
    // 2026-09-24). Searching your own library is a local filter on each
    // library page and is unaffected.
    let role_ctx = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    let can_contribute = move || {
        matches!(
            role_ctx.and_then(|r| r.get()).as_deref(),
            // `None` while `/me` is in flight: show it rather than flash it
            // away under an operator who can use it.
            None | Some("admin") | Some("contributor")
        )
    };

    view! {
        <div class="ov-head">
            <div>
                <h2 class="page-title">"Overview"</h2>
                <p class="page-sub mono">"all media"</p>
            </div>
            {move || can_contribute().then(|| view! {
                <button class="ov-search" on:click=move |_| go_add.run(())>
                    <span class="ov-search-icon">"⌕"</span>
                    "Search or add anything…"
                </button>
            })}
        </div>

        <div class="metrics-row">
            <div class="ov-metric">
                <span class="u-label">"Library"</span>
                <span class="ov-metric-num tnum">{move || total().to_string()}</span>
                <span class="ov-metric-sub mono">"items tracked"</span>
            </div>
            <div class="ov-metric">
                <span class="u-label">"Monitored"</span>
                <span class="ov-metric-num tnum">{move || monitored().to_string()}</span>
                <span class="ov-metric-sub mono">"auto-upgrading"</span>
            </div>
            <div class="ov-metric">
                <span class="u-label">"Wanted"</span>
                <span class="ov-metric-num tnum">{move || wanted().to_string()}</span>
                <span class="ov-metric-sub mono">"missing"</span>
            </div>
            <div class="ov-metric">
                <span class="u-label">"Downloading"</span>
                <span class="ov-metric-num tnum">{move || downloading().to_string()}</span>
                <span class="ov-metric-sub mono">
                    {move || {
                        let t = throughput();
                        if t > 0 { format!("↓ {}", rate(t)) } else { "idle".into() }
                    }}
                </span>
            </div>
        </div>

        <div class="health-strip">{health_strip}</div>

        <div class="ov-cols">
            <section class="ov-left">
                <div class="ov-sec-head">
                    <h3>"Active hunt"</h3>
                    <A href="/activity" attr:class="ov-sec-link mono">
                        // Count the transfers actually shown below (downloading), not
                        // the acquisition-in-flight count — they were inconsistent
                        // (e.g. "0 in flight" above two active download bars).
                        {move || format!("{} in flight", downloading())}
                    </A>
                </div>
                {hunt_rows}
                <div class="rollups">{rollups}</div>
            </section>

            <section class="ov-right">
                <div class="ov-sec-head">
                    <h3>"Recently imported"</h3>
                    <A href="/activity" attr:class="ov-sec-link mono">"View all"</A>
                </div>
                {imported_rows}
            </section>
        </div>
    }
}
