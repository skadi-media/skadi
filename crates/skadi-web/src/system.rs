//! Settings → System (SKADI-T-0684): what the daemon is, whether it is healthy,
//! what it runs on a timer, and what it has been saying.
//!
//! Four read surfaces the API has served since SKADI-T-0467 and SKADI-T-0680
//! (`/system/status`, `/health/checks`, `/system/task`, `/log`), plus the
//! "Run now" of `POST /health/checks/run`. Every one of them is admin-only on
//! the API side (`household::path_allowed`), so the page is too.
//!
//! **The gate is a known-good role.** The page draws its panels only when the
//! role is `Some("admin")`, never when it is "not a known bad one": an
//! unresolved role must fetch nothing (SKADI-T-0639). The role is read in the
//! reactive closure, so the panels mount (and fetch) once `/me` answers; the
//! fetches themselves are in a child component that only an admin mounts.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::dashboard::severity_class;

/// How many log lines to ask for: the daemon's whole ring (`logbuf::CAPACITY`).
pub const LOG_PAGE: usize = 500;

/// The log-level filter: `(minimum level, label)`. A line passes when its
/// level is at least as severe as the minimum.
pub const LOG_FILTERS: &[(&str, &str)] = &[
    ("TRACE", "All levels"),
    ("DEBUG", "Debug and above"),
    ("INFO", "Info and above"),
    ("WARN", "Warnings and errors"),
    ("ERROR", "Errors only"),
];

/// What the empty last/next-run cells show. The daemon keeps no task run
/// record yet (SKADI-T-0440), so the columns stay, with a dash.
pub const NO_VALUE: &str = "—";

/// Whether this role may open the System page: the admin only.
pub fn can_open(role: Option<&str>) -> bool {
    role == Some("admin")
}

/// The commit for the status header: the daemon's value, at most 12
/// characters of a sha, or `unknown` when the daemon has none (an image built
/// without the build arg, or a daemon older than the field).
pub fn commit_label(commit: Option<&str>) -> String {
    match commit.map(str::trim) {
        None | Some("") => "unknown".into(),
        Some(c) if c.len() > 12 && c.chars().all(|ch| ch.is_ascii_hexdigit()) => c[..12].into(),
        Some(c) => c.into(),
    }
}

/// Uptime in the two largest units: `3d 4h`, `2h 5m`, `7m 12s`, `40s`.
pub fn uptime_label(seconds: i64) -> String {
    let s = seconds.max(0);
    let (d, h, m, sec) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60, s % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    }
}

/// A task interval: `every 5 min`, `every 2 h`, `every 90 s`.
pub fn interval_label(seconds: u64) -> String {
    if seconds >= 3_600 && seconds.is_multiple_of(3_600) {
        format!("every {} h", seconds / 3_600)
    } else if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("every {} min", seconds / 60)
    } else {
        format!("every {seconds} s")
    }
}

/// An RFC 3339 time as `YYYY-MM-DD HH:MM:SS` (UTC, as the daemon sends it), or
/// [`NO_VALUE`] when there is none.
pub fn time_label(t: Option<&str>) -> String {
    match t {
        None | Some("") => NO_VALUE.into(),
        Some(t) => match t.get(..19) {
            Some(head) if t.as_bytes().get(10) == Some(&b'T') => head.replacen('T', " ", 1),
            _ => t.into(),
        },
    }
}

/// The banner over the check table: `error` when any check is an error,
/// `warn` when any is a warning, else none. A level this client does not know
/// counts as an error, as [`severity_class`] draws it.
pub fn banner_level(checks: &[api::HealthCheck]) -> Option<&'static str> {
    let mut worst = None;
    for c in checks {
        match c.level() {
            "ok" | "pending" => {}
            "warn" => worst = worst.or(Some("warn")),
            _ => return Some("error"),
        }
    }
    worst
}

/// Severity rank of a log level: 0 = `ERROR` … 4 = `TRACE`. A level this
/// client does not know ranks as `ERROR`, so no filter hides it.
fn log_rank(level: &str) -> usize {
    LOG_FILTERS
        .iter()
        .rev()
        .position(|(l, _)| l.eq_ignore_ascii_case(level.trim()))
        .unwrap_or(0)
}

/// The lines at least as severe as `min` (one of [`LOG_FILTERS`]), in order.
pub fn filter_log(lines: &[api::LogLine], min: &str) -> Vec<api::LogLine> {
    let floor = log_rank(min);
    lines
        .iter()
        .filter(|l| log_rank(&l.level) <= floor)
        .cloned()
        .collect()
}

/// CSS tone of a log level, from the shared semantic colours.
pub fn log_level_class(level: &str) -> &'static str {
    match log_rank(level) {
        0 => "bad",
        1 => "warn",
        2 => "",
        _ => "muted",
    }
}

/// The System page. The admin gets the four panels; any other known role is
/// told the page is not theirs; an unresolved role sees a loading line and
/// fetches nothing.
#[component]
pub fn SystemPage() -> impl IntoView {
    let role = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    move || {
        // Read here, in the reactive closure, so the page re-renders when
        // `/me` answers. A read inside an async block would not be tracked.
        let role = role.and_then(|r| r.get());
        if can_open(role.as_deref()) {
            view! {
                <crate::subnav::SubNav/>
                <div class="page-head"><h2>"System"</h2></div>
                <SystemPanels/>
            }
            .into_any()
        } else if role.is_none() {
            view! { <p class="muted">"Loading…"</p> }.into_any()
        } else {
            view! { <p class="muted">"This page is for the household admin."</p> }.into_any()
        }
    }
}

/// The four panels and their fetches. Mounted for the admin only.
#[component]
fn SystemPanels() -> impl IntoView {
    let status = RwSignal::new(None::<api::SystemStatus>);
    let status_err = RwSignal::new(None::<String>);
    let checks = RwSignal::new(Vec::<api::HealthCheck>::new());
    let checks_err = RwSignal::new(None::<String>);
    let running = RwSignal::new(false);
    let tasks = RwSignal::new(Vec::<api::SystemTask>::new());
    let tasks_err = RwSignal::new(None::<String>);
    let logs = RwSignal::new(Vec::<api::LogLine>::new());
    let logs_err = RwSignal::new(None::<String>);
    let logs_loading = RwSignal::new(false);
    let log_min = RwSignal::new(String::from("TRACE"));

    let load_logs = move || {
        logs_loading.set(true);
        spawn_local(async move {
            match api::system_log(LOG_PAGE).await {
                Ok(l) => {
                    logs.set(l);
                    logs_err.set(None);
                }
                Err(e) => logs_err.set(Some(e.0)),
            }
            logs_loading.set(false);
        });
    };

    // Each panel loads on its own: a failed one never blanks the others.
    Effect::new(move |_| {
        spawn_local(async move {
            match api::system_status().await {
                Ok(s) => status.set(Some(s)),
                Err(e) => status_err.set(Some(e.0)),
            }
        });
        spawn_local(async move {
            match api::health_checks().await {
                Ok(c) => checks.set(c),
                Err(e) => checks_err.set(Some(e.0)),
            }
        });
        spawn_local(async move {
            match api::system_tasks().await {
                Ok(t) => tasks.set(t),
                Err(e) => tasks_err.set(Some(e.0)),
            }
        });
        load_logs();
    });

    let run_now = move |_| {
        if running.get_untracked() {
            return;
        }
        running.set(true);
        spawn_local(async move {
            match api::run_health_checks().await {
                Ok(c) => {
                    checks.set(c);
                    checks_err.set(None);
                }
                Err(e) => checks_err.set(Some(e.0)),
            }
            running.set(false);
        });
    };

    let error_line = |e: RwSignal<Option<String>>| {
        move || {
            e.get()
                .map(|e| view! { <p class="bad">"Load failed: " {e}</p> })
        }
    };

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Status"</h3>
                    <p class="muted">"The running daemon and what it is built from."</p>
                </div>
            </div>
            {error_line(status_err)}
            {move || status.get().map(|s| view! { <StatusFacts status=s/> })}
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Health"</h3>
                    <p class="muted">
                        "The results of the last check run. The daemon runs the checks on its own timer."
                    </p>
                </div>
                <button on:click=run_now disabled=move || running.get()>
                    {move || if running.get() { "Running…" } else { "Run now" }}
                </button>
            </div>
            {error_line(checks_err)}
            {move || view! { <CheckTable checks=checks.get()/> }}
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Tasks"</h3>
                    <p class="muted">"The jobs the daemon runs on a timer."</p>
                </div>
            </div>
            {error_line(tasks_err)}
            {move || view! { <TaskTable tasks=tasks.get()/> }}
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Logs"</h3>
                    <p class="muted">
                        "The newest lines the daemon logged since it started, newest first. "
                        "The full record is in the container log."
                    </p>
                </div>
                <div class="page-head-actions">
                    <select
                        aria-label="Log level"
                        prop:value=move || log_min.get()
                        on:change=move |ev| log_min.set(event_target_value(&ev))
                    >
                        {LOG_FILTERS
                            .iter()
                            .map(|(v, label)| view! { <option value=*v>{*label}</option> })
                            .collect_view()}
                    </select>
                    <button on:click=move |_| load_logs() disabled=move || logs_loading.get()>
                        {move || if logs_loading.get() { "Refreshing…" } else { "Refresh" }}
                    </button>
                </div>
            </div>
            {error_line(logs_err)}
            {move || view! { <LogTable lines=filter_log(&logs.get(), &log_min.get())/> }}
        </section>
    }
}

/// The status header: version, build commit, uptime, database, library root.
#[component]
pub fn StatusFacts(status: api::SystemStatus) -> impl IntoView {
    let commit_full = status.commit.clone().unwrap_or_default();
    let commit = commit_label(status.commit.as_deref());
    let root = if status.library_root.is_empty() {
        "not set".to_string()
    } else {
        status.library_root.clone()
    };
    let started = time_label(Some(&status.start_time));
    view! {
        <div class="diag-facts system-facts">
            <div class="diag-fact"><span class="diag-k">"Version"</span>
                <span class="diag-v mono system-version">{status.version.clone()}</span></div>
            <div class="diag-fact"><span class="diag-k">"Build commit"</span>
                <span class="diag-v mono system-commit" title=commit_full>{commit}</span></div>
            <div class="diag-fact"><span class="diag-k">"Uptime"</span>
                <span class="diag-v mono" title=format!("since {started} UTC")>
                    {uptime_label(status.uptime_seconds)}
                </span></div>
            <div class="diag-fact"><span class="diag-k">"Database"</span>
                <span class="diag-v mono">{status.database.clone()}</span></div>
            <div class="diag-fact"><span class="diag-k">"Library root"</span>
                <span class="diag-v mono">{root}</span></div>
        </div>
    }
}

/// Every check with its severity, message, remediation and last run, under a
/// banner when any check is a warning or an error.
#[component]
pub fn CheckTable(checks: Vec<api::HealthCheck>) -> impl IntoView {
    if checks.is_empty() {
        return view! { <p class="muted">"No checks reported."</p> }.into_any();
    }
    let banner = banner_level(&checks).map(|level| {
        let (class, text) = if level == "error" {
            (
                "notice bad system-banner",
                "A health check failed. The fix is in the table.",
            )
        } else {
            (
                "notice warn system-banner",
                "A health check has a warning. The fix is in the table.",
            )
        };
        view! { <p class=class role="status">{text}</p> }
    });
    let rows = checks
        .into_iter()
        .map(|c| {
            let level = c.level().to_string();
            let sev = severity_class(&level);
            let dot = format!("status-dot {sev}");
            let label = c.label.clone().unwrap_or_else(|| c.name.clone());
            let remedy = c.remediation.clone().unwrap_or_default();
            let when = time_label(c.checked_at.as_deref());
            view! {
                <tr class=format!("check-row sev-{sev}")>
                    <td class="nowrap">
                        <span class=dot></span>
                        <span class="check-level mono">{level}</span>
                    </td>
                    <td>
                        <div class="check-label">{label}</div>
                        <div class="check-id mono muted">{c.name.clone()}</div>
                    </td>
                    <td class="check-message">{c.detail.clone()}</td>
                    <td class="check-remedy">{remedy}</td>
                    <td class="nowrap mono muted">{when}</td>
                </tr>
            }
        })
        .collect_view();
    view! {
        {banner}
        <table class="system-table checks-table">
            <thead>
                <tr>
                    <th>"Severity"</th>
                    <th>"Check"</th>
                    <th>"Message"</th>
                    <th>"How to fix"</th>
                    <th>"Checked (UTC)"</th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </table>
    }
    .into_any()
}

/// The recurring jobs. Last and next run show [`NO_VALUE`] until the daemon
/// records task runs (SKADI-T-0440).
#[component]
pub fn TaskTable(tasks: Vec<api::SystemTask>) -> impl IntoView {
    if tasks.is_empty() {
        return view! { <p class="muted">"No tasks reported."</p> }.into_any();
    }
    let rows = tasks
        .into_iter()
        .map(|t| {
            view! {
                <tr class="task-row">
                    <td>
                        <div class="mono">{t.name.clone()}</div>
                        <div class="muted task-what">{t.what.clone()}</div>
                    </td>
                    <td class="nowrap">{interval_label(t.interval_seconds)}</td>
                    <td class="nowrap mono muted">{time_label(t.last_run.as_deref())}</td>
                    <td class="nowrap mono muted">{time_label(t.next_run.as_deref())}</td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <table class="system-table tasks-table">
            <thead>
                <tr>
                    <th>"Task"</th>
                    <th>"Interval"</th>
                    <th>"Last run (UTC)"</th>
                    <th>"Next run (UTC)"</th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </table>
    }
    .into_any()
}

/// Log lines, newest first, as the daemon returned them.
#[component]
pub fn LogTable(lines: Vec<api::LogLine>) -> impl IntoView {
    if lines.is_empty() {
        return view! { <p class="muted">"No log lines at this level."</p> }.into_any();
    }
    let rows = lines
        .into_iter()
        .map(|l| {
            let tone = format!("log-level mono {}", log_level_class(&l.level));
            view! {
                <tr class="log-row">
                    <td class="nowrap mono muted">{time_label(Some(&l.time))}</td>
                    <td class="nowrap"><span class=tone>{l.level.clone()}</span></td>
                    <td class="nowrap mono muted log-target">{l.target.clone()}</td>
                    <td class="mono log-message">{l.message.clone()}</td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <table class="system-table log-table">
            <thead>
                <tr>
                    <th>"Time (UTC)"</th>
                    <th>"Level"</th>
                    <th>"Source"</th>
                    <th>"Message"</th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </table>
    }
    .into_any()
}
