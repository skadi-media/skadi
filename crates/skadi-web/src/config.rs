//! Configuration UI (SKADI-T-0067): quality profile, root folders, edition
//! kinds, and the domains enable/disable toggle.
//!
//! Distinct from the provider Settings page (T-0066): these are the policy /
//! library knobs that take the daemon from "providers wired" to "ready to
//! acquire". The **quality profile** is the one that actually changes acquire
//! behavior — the daemon resolves the active profile from the `profiles`
//! settings and the supervisor reconciles it into scoring, so a 1080p cap here
//! really does stop a UHD grab.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;
use crate::app::DomainsVersion;
use crate::loading::{ListLoad, SkeletonKind};

/// System-wide configuration (SKADI-T-0071): domains enable/disable, shared
/// quality profiles, root folders, and notifiers. Domain-specific config (e.g.
/// movies edition kinds) lives under that domain's own area, not here.
#[component]
pub fn ConfigPage() -> impl IntoView {
    view! {
        <crate::subnav::SubNav/>
        // The strip names the page, so this head carries only the one action
        // on it. Keep the link: `/setup` has no other entry point in the UI.
        <div class="page-head">
            <A href="/setup" attr:class="btn-link">"↻ Run setup wizard"</A>
        </div>
        <DomainsSection/>
        <LibraryRootSection/>
        <crate::settings::ProviderSection spec=crate::settings::notifier_spec()/>
        <AdvancedSection/>
    }
}

// --- Domains ---------------------------------------------------------------

/// Accent + display title + one-line description for a domain `kind`, matching the
/// design's Config → Domains rows.
fn domain_meta(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "movie" => ("ice", "Movies", "TMDB metadata · Torznab / Newznab"),
        "audiobook" => ("teal", "Audiobooks", "Audnexus metadata · private trackers"),
        // The TV domain's kind is "series" (skadi-tv's internal naming); accept
        // the aliases too. GOLD to match the sidebar/library TV accent — this
        // kind was falling through to the grey empty default, so the card read
        // a lowercase name + a disabled-looking toggle (SKADI-T-0347).
        "tv" | "television" | "series" => ("gold", "TV", "TVDB metadata · episode tracking"),
        "music" => ("gold", "Music", "MusicBrainz metadata · album tracking"),
        "book" => ("muted", "Books", "OpenLibrary metadata · e-book tracking"),
        _ => ("muted", "", ""),
    }
}

#[component]
fn DomainsSection() -> impl IntoView {
    let domains = RwSignal::new(Vec::<api::Domain>::new());
    // First load, failure and Retry (SKADI-T-0698).
    let load = ListLoad::new();
    let busy = RwSignal::new(false);

    // Shared trigger so the persistent sidebar (and Health/dashboard on next
    // mount) reflect a toggle immediately.
    let version = use_context::<DomainsVersion>();

    let refresh = move || {
        spawn_local(async move {
            if let Some(d) = load.settle(api::list_domains().await) {
                let _ = domains.try_set(d);
            }
        });
    };
    Effect::new(move |_| refresh());

    let rows = move || {
        domains
            .get()
            .into_iter()
            .map(|d| {
                let name = d.name.clone();
                let toggle = move |_| {
                    let name = name.clone();
                    let next = !d.enabled;
                    busy.set(true);
                    spawn_local(async move {
                        let _ = api::set_domain_enabled(&name, next).await;
                        busy.set(false);
                        refresh();
                        // Greedily refresh the rest of the UI (sidebar nav).
                        if let Some(DomainsVersion(v)) = version {
                            v.update(|n| *n += 1);
                        }
                    });
                };
                let state = if d.enabled { "enabled" } else { "disabled" };
                let (accent, title, desc) = domain_meta(&d.kind);
                let title = if title.is_empty() {
                    d.name.clone()
                } else {
                    title.to_string()
                };
                let desc = if desc.is_empty() {
                    d.kind.clone()
                } else {
                    desc.to_string()
                };
                let switch_cls = if d.enabled {
                    format!("switch on {accent}")
                } else {
                    "switch".to_string()
                };
                let switch_label = format!("{title} domain");
                let state_cls = if d.enabled {
                    "domain-state ok"
                } else {
                    "domain-state muted"
                };
                view! {
                    <div class="domain-row">
                        <div class="domain-row-main">
                            <span class=format!("lib-accent {accent}")></span>
                            <div class="domain-row-text">
                                <strong>{title}</strong>
                                <span class="domain-row-desc mono">{desc}</span>
                            </div>
                        </div>
                        <div class="domain-row-right">
                            <span class=state_cls>{state}</span>
                            <button
                                class=switch_cls
                                role="switch"
                                aria-checked=if d.enabled { "true" } else { "false" }
                                aria-label=switch_label
                                on:click=toggle
                                disabled=move || busy.get()
                            ></button>
                        </div>
                    </div>
                }
            })
            .collect_view()
    };

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Domains"</h3>
                    <p class="muted">"Enable or disable compiled-in media domains. Changes take effect on the next supervisor tick (~5s)."</p>
                </div>
            </div>
            {load.status_n(SkeletonKind::Rows, Some(3), Callback::new(move |()| refresh()))}
            <div class="cards">{rows}</div>
        </section>
    }
}

// Quality profile moved to the movies domain area (movies::ProfileSection) in
// SKADI-T-0071 follow-up — profiles are domain-owned for now.

// --- Library root ----------------------------------------------------------

/// Read-only view of the single library root (SKADI-T-0302). skadi owns one
/// mounted filesystem (`library.root`, set via `SKADI_LIBRARY_ROOT` / deploy) and
/// derives every domain's folder under it (`movie`, `television`, `audiobook`) —
/// there is no operator root picker. This just surfaces the mount's path, health,
/// and free space so the operator can confirm it's reachable.
#[component]
fn LibraryRootSection() -> impl IntoView {
    let report = RwSignal::new(None::<api::RootFolder>);
    let error = RwSignal::new(None::<String>);

    Effect::new(move |_| {
        spawn_local(async move {
            match api::root_folders().await {
                Ok(list) => {
                    report.set(list.into_iter().next());
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    });

    let body = move || {
        report.get().map(|r| {
            let gib = |b: u64| format!("{:.1} GiB", b as f64 / 1024.0 / 1024.0 / 1024.0);
            let space = match (r.free_bytes, r.total_bytes) {
                (Some(free), Some(total)) => format!("{} free of {}", gib(free), gib(total)),
                _ => "free space unknown".to_string(),
            };
            let (badge, badge_class) = if r.writable && r.exists {
                ("usable", "ok")
            } else if !r.exists {
                ("missing", "bad")
            } else {
                ("not writable", "bad")
            };
            view! {
                <div class="card">
                    <div class="card-main">
                        <strong>{r.path.clone()}</strong>
                        <span class=format!("pill {badge_class}")>{badge}</span>
                        <p class="muted">{space}</p>
                    </div>
                </div>
            }
        })
    };

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Library root"</h3>
                    <p class="muted">"The single mounted filesystem skadi writes to. Set it via the deploy env (SKADI_LIBRARY_ROOT); domains land under movie/ television/ audiobook/ and downloads under downloads/."</p>
                </div>
            </div>
            {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
            <div class="cards">{body}</div>
        </section>
    }
}

// Edition kinds moved to the movies domain area (movies::MoviesConfigPage) in
// SKADI-T-0071 — it's movies-specific config, not system-wide.

// --- Advanced settings: the config registry (SKADI-T-0540) -----------------

/// Order the groups so the ones an operator actually tunes come first, rather
/// than alphabetically — `library` and `import` are what people open this page
/// for; `http` and `blocklist` are rarely touched.
const GROUP_ORDER: &[&str] = &[
    "library",
    "import",
    "worker",
    "sweep",
    "monitor",
    "history",
    "blocklist",
    "tv",
    "http",
    "general",
];

fn group_rank(name: &str) -> usize {
    GROUP_ORDER
        .iter()
        .position(|g| *g == name)
        .unwrap_or(GROUP_ORDER.len())
}

/// Group the registry by key prefix and order both the groups and the keys
/// within them.
///
/// Pure so it can be tested without a DOM (`tests/logic.rs`); the prefix is the
/// natural grouping and needs no extra metadata on the key.
#[must_use]
pub fn group_config(keys: Vec<api::ConfigKey>) -> Vec<(String, Vec<api::ConfigKey>)> {
    let mut groups: Vec<(String, Vec<api::ConfigKey>)> = Vec::new();
    for key in keys {
        let name = key.group().to_string();
        match groups.iter_mut().find(|(g, _)| *g == name) {
            Some((_, list)) => list.push(key),
            None => groups.push((name, vec![key])),
        }
    }
    for (_, list) in &mut groups {
        list.sort_by(|a, b| a.key.cmp(&b.key));
    }
    groups.sort_by(|a, b| group_rank(&a.0).cmp(&group_rank(&b.0)).then(a.0.cmp(&b.0)));
    groups
}

/// The input type to render for a registry `kind`.
#[must_use]
pub fn input_kind(kind: &str) -> &'static str {
    match kind {
        "bool" => "checkbox",
        "u16" | "u64" => "number",
        _ => "text",
    }
}

#[component]
pub fn AdvancedSection() -> impl IntoView {
    let keys = RwSignal::new(Vec::<api::ConfigKey>::new());
    // The key list's first load, failure and Retry (SKADI-T-0698); `error`
    // is a refused save or reset.
    let load = ListLoad::new();
    let error = RwSignal::new(None::<String>);
    let note = RwSignal::new(None::<String>);

    let reload = move || {
        spawn_local(async move {
            if let Some(k) = load.settle(api::list_config().await.map_err(|e| e.0)) {
                let _ = keys.try_set(k);
            }
        });
    };
    reload();

    let save = move |key: String, value: String| {
        spawn_local(async move {
            match api::set_config(&key, &value).await {
                // The daemon explains *why* a value is refused (SKADI-T-0523's
                // validate_write / validate_cross_key). Showing its message
                // verbatim rather than a generic "invalid" is the whole benefit
                // of that validation.
                Err(e) => error.set(Some(e.0)),
                Ok(()) => {
                    error.set(None);
                    note.set(Some(format!("saved {key}")));
                }
            }
        });
    };

    let reset = move |key: String| {
        spawn_local(async move {
            match api::clear_config(&key).await {
                Err(e) => error.set(Some(e.0)),
                Ok(()) => {
                    error.set(None);
                    note.set(Some(format!("{key} reset to its default")));
                }
            }
        });
    };

    view! {
        <section class="card">
            <h3 class="card-title">"Advanced"</h3>
            <p class="muted">
                "Every daemon setting. Blank means the default shown in the placeholder."
            </p>
            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
            {move || note.get().map(|n| view! { <p class="muted">{n}</p> })}
            {load.status(SkeletonKind::Rows, Callback::new(move |()| reload()))}
            <For
                each=move || group_config(keys.get())
                key=|(name, _)| name.clone()
                let:group
            >
                <div class="config-group">
                    <h4>{group.0.clone()}</h4>
                    <For each=move || group.1.clone() key=|k| k.key.clone() let:k>
                        <ConfigRow k=k save=save reset=reset/>
                    </For>
                </div>
            </For>
        </section>
    }
}

#[component]
fn ConfigRow(
    k: api::ConfigKey,
    save: impl Fn(String, String) + Copy + 'static,
    reset: impl Fn(String) + Copy + 'static,
) -> impl IntoView {
    let key = k.key.clone();
    let draft = RwSignal::new(k.value.clone().unwrap_or_default());

    // Tier0 (`database_url`) is read before the config table exists, so it can
    // never be written here. Rendered read-only with the reason rather than
    // hidden — an operator looking for it should find it and see why.
    let locked = !k.editable;
    // A redacted key's value is never echoed, so the field shows set/not-set and
    // offers replacement. Never a masked string pretending to be the real
    // length, which would invite someone to "fix" a value they cannot see.
    let redacted = k.redacted;
    let env_backed = k.overwritten_by_env();

    let save_key = key.clone();
    let reset_key = key.clone();
    let stored = k.value.clone().unwrap_or_default();
    let is_set = k.is_set;
    // The label names the input, and the help text describes it (SKADI-T-0700).
    let input_id = format!("cfg-{key}");
    let help = k.help.clone().filter(|h| !h.trim().is_empty());
    let help_id = help.as_ref().map(|_| format!("cfg-help-{key}"));
    view! {
        <div class="config-row">
            <label class="config-key" for=input_id.clone()>
                {k.leaf().to_string()}
                {locked.then(|| view! { <span class="pill">"read-only"</span> })}
                {env_backed.then(|| view! { <span class="pill warn">"set by env"</span> })}
            </label>
            {if redacted {
                view! {
                    <span class="muted" id=input_id.clone()>
                        {if k.is_set { "set" } else { "not set" }}
                    </span>
                }.into_any()
            } else {
                view! {
                    <input
                        type=input_kind(&k.kind)
                        id=input_id.clone()
                        aria-describedby=help_id.clone()
                        prop:value=move || draft.get()
                        placeholder=k.default.clone()
                        disabled=locked
                        on:change=move |ev| draft.set(event_target_value(&ev))
                    />
                }.into_any()
            }}
            {(!locked).then(move || {
                let sk = save_key.clone();
                let rk = reset_key.clone();
                // Save exists only once there is something to save; Reset only
                // once there is something to reset (SKADI-T-0580).
                view! {
                    <span class="row-actions">
                        <button
                            class:hidden=move || draft.get() == stored
                            on:click=move |_| save(sk.clone(), draft.get())
                        >"Save"</button>
                        <button
                            class="link-btn"
                            class:hidden=!is_set
                            on:click=move |_| reset(rk.clone())
                        >"Reset"</button>
                    </span>
                }
            })}
            {help.map(|h| view! {
                <p class="muted config-help" id=help_id.clone()>{h}</p>
            })}
            {env_backed.then(|| view! {
                <p class="muted">
                    "This value comes from an environment variable and is rewritten \
                     on the next restart — a change saved here will not survive one."
                </p>
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These mirror the `tests/logic.rs` wasm cases (SKADI-T-0540). They are
    /// duplicated on the host target on purpose: the functions are pure, and the
    /// browser harness needs a chromedriver that is not always available — a
    /// test that cannot run is not a test. The wasm copies stay so the same
    /// behaviour is covered where the code actually ships.
    fn cfg_key(k: &str, kind: &str, source: Option<&str>) -> api::ConfigKey {
        serde_json::from_value(serde_json::json!({
            "key": k,
            "kind": kind,
            "default": "",
            "isSet": source.is_some(),
            "editable": true,
            "source": source,
        }))
        .expect("ConfigKey fixture")
    }

    #[test]
    fn groups_by_prefix_with_the_tuned_groups_first() {
        let groups = group_config(vec![
            cfg_key("http.proxy_url", "string", None),
            cfg_key("import.min_free_mb", "u64", None),
            cfg_key("library.root", "path", None),
            cfg_key("import.placement", "string", None),
            // No dot at all — must not be dropped, and must not invent a group.
            cfg_key("sweep_max_concurrent", "u16", None),
        ]);
        let names: Vec<&str> = groups.iter().map(|(g, _)| g.as_str()).collect();
        assert_eq!(names, vec!["library", "import", "http", "general"]);

        // Keys inside a group are ordered, so the form does not reshuffle
        // between loads just because the API returned a different order.
        let import = &groups[1].1;
        assert_eq!(import[0].key, "import.min_free_mb");
        assert_eq!(import[1].key, "import.placement");
    }

    #[test]
    fn a_row_label_drops_the_group_prefix() {
        assert_eq!(
            cfg_key("import.placement", "string", None).leaf(),
            "placement"
        );
        // A key with no prefix keeps its whole name rather than losing it.
        assert_eq!(
            cfg_key("sweep_max_concurrent", "u16", None).leaf(),
            "sweep_max_concurrent"
        );
    }

    #[test]
    fn controls_are_typed_from_the_registry_kind() {
        assert_eq!(input_kind("bool"), "checkbox");
        assert_eq!(input_kind("u16"), "number");
        assert_eq!(input_kind("u64"), "number");
        assert_eq!(input_kind("path"), "text");
        // An unrecognised kind falls back to text rather than vanishing.
        assert_eq!(input_kind("something-new"), "text");
    }

    /// The criterion most likely to be skipped, and the one with a real operator
    /// consequence: an env-sourced value is rewritten by the seeder on the next
    /// boot, so a runtime write to it does not survive a restart. A form that
    /// lets someone edit it without saying so is worse than no form — it looks
    /// like it worked.
    #[test]
    fn an_env_sourced_key_is_flagged_as_not_surviving_a_restart() {
        assert!(cfg_key("http.proxy_url", "string", Some("env")).overwritten_by_env());
        assert!(!cfg_key("http.proxy_url", "string", Some("runtime")).overwritten_by_env());
        // Unset is not env-backed — nothing overwrites a value that isn't there.
        assert!(!cfg_key("http.proxy_url", "string", None).overwritten_by_env());
    }
}
