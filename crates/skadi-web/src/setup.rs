//! First-run Setup wizard (SKADI-I-0035 / SKADI-T-0254): a full-screen 6-step flow
//! — Welcome · Domains · Library · Indexer · Downloader · Done — that POSTs each
//! choice (enable domains, register a root folder + an optional indexer) and routes
//! to the Overview when finished. Reachable any time via Config → "Run setup wizard".

use std::collections::HashSet;

use aurora_leptos::components::{Alert, Button, Card, PasswordInput, TextInput};
use aurora_leptos::tokens::token;
use leptos::portal::Portal;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use serde_json::json;

use crate::api;

const STEPS: [&str; 6] = [
    "Welcome",
    "Domains",
    "Library",
    "Indexer",
    "Downloader",
    "Done",
];

#[component]
pub fn SetupPage() -> impl IntoView {
    let step = RwSignal::new(0usize);
    // Choices, defaulting to the two shipped domains enabled.
    let domains = RwSignal::new(HashSet::from([
        "movies".to_string(),
        "audiobooks".to_string(),
    ]));
    let ix_name = RwSignal::new(String::new());
    let ix_url = RwSignal::new(String::new());
    let ix_key = RwSignal::new(String::new());
    let applying = RwSignal::new(false);

    let navigate = use_navigate();

    // Persist every choice, then route to the Overview.
    let finish = move || {
        applying.set(true);
        let nav = navigate.clone();
        spawn_local(async move {
            for d in ["movies", "audiobooks"] {
                let _ = api::set_domain_enabled(d, domains.get_untracked().contains(d)).await;
            }
            let url = ix_url.get_untracked();
            if !url.trim().is_empty() {
                let _ = api::create_setting(
                    "indexers",
                    &json!({
                        "kind": "torznab",
                        "name": ix_name.get_untracked(),
                        "base_url": url,
                        "api_key": ix_key.get_untracked(),
                        "categories": [],
                    }),
                )
                .await;
            }
            nav("/", Default::default());
        });
    };

    let stepper = move || {
        let cur = step.get();
        STEPS
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let cls = if i < cur {
                    "step done"
                } else if i == cur {
                    "step current"
                } else {
                    "step"
                };
                let mark = if i < cur {
                    "✓".to_string()
                } else {
                    (i + 1).to_string()
                };
                view! {
                    <div class=cls>
                        <span class="step-dot">{mark}</span>
                        <span class="step-label">{*label}</span>
                    </div>
                }
            })
            .collect_view()
    };

    // A selectable domain card for step 1.
    let domain_card =
        move |key: &'static str, title: &'static str, desc: &'static str, accent: &'static str| {
            let on = move || domains.get().contains(key);
            let toggle = move |()| {
                domains.update(|d| {
                    if !d.remove(key) {
                        d.insert(key.to_string());
                    }
                });
            };
            view! {
                // Aurora's Card as a toggle (SKADI-T-0719).
                <Card on_click=Callback::new(toggle) selected=Signal::derive(on)
                    attr:aria-pressed=move || if on() { "true" } else { "false" }>
                    <div class="setup-domain">
                        <span class=format!("lib-accent {accent}")></span>
                        <div class="setup-domain-text">
                            <strong>{title}</strong>
                            <span class="mono">{desc}</span>
                        </div>
                        <span class="setup-check">{move || if on() { "✓" } else { "" }}</span>
                    </div>
                </Card>
            }
        };

    let content = move || {
        match step.get() {
        0 => view! {
            <div class="setup-welcome">
                <svg class="setup-mark" width="56" height="56" viewBox="0 0 24 24" fill="none"
                    stroke="var(--brand-stroke)" stroke-width="1.4" stroke-linecap="round">
                    <line x1="12" y1="3" x2="12" y2="21"></line>
                    <line x1="4.2" y1="7.5" x2="19.8" y2="16.5"></line>
                    <line x1="19.8" y1="7.5" x2="4.2" y2="16.5"></line>
                </svg>
                <h1>"Welcome to Skadi"</h1>
                <p class="setup-lede">
                    "One daemon for every kind of media — no more juggling six apps. This takes about
                     a minute: pick your media, point at your library, connect an indexer. The
                     downloader is already built in."
                </p>
                <ol class="setup-bullets">
                    <li>"Enable the domains you want"</li>
                    <li>"Confirm your library mount"</li>
                    <li>"Connect a Torznab / Newznab indexer"</li>
                </ol>
            </div>
        }
        .into_any(),
        1 => view! {
            <div class="setup-step">
                <h2>"What should Skadi manage?"</h2>
                <div class="setup-domains">
                    {domain_card("movies", "Movies", "TMDB metadata · Torznab / Newznab", "ice")}
                    {domain_card("audiobooks", "Audiobooks", "Audnexus metadata · private trackers", "teal")}
                </div>
            </div>
        }
        .into_any(),
        2 => view! {
            <div class="setup-step">
                <h2>"Where does your library live?"</h2>
                <p class="setup-help mono">
                    "Skadi owns one mounted filesystem and lays out the library itself — movies
                     under movie/, TV under television/, audiobooks under audiobook/, and active
                     downloads under downloads/. Imports hardlink into place (same filesystem),
                     so they're instant and space-free."
                </p>
                <p class="setup-help mono">
                    "Set the mount via the deploy environment (SKADI_LIBRARY_ROOT, default /data).
                     There's nothing to pick here."
                </p>
            </div>
        }
        .into_any(),
        3 => view! {
            <div class="setup-step">
                <h2>"Connect an indexer"</h2>
                <p class="setup-help mono">"Torznab / Newznab — works with Prowlarr, Jackett, or a native endpoint. You can skip and add one later."</p>
                <div class="setup-fields">
                    <TextInput label="Name" placeholder="Prowlarr" value=ix_name/>
                    <TextInput label="URL" placeholder="http://prowlarr:9696/1/api" value=ix_url
                        mono=true spellcheck=false/>
                    <PasswordInput label="API key" value=ix_key autocomplete="off"/>
                </div>
            </div>
        }
        .into_any(),
        4 => view! {
            <div class="setup-step">
                <h2>"Downloads are built in"</h2>
                <Alert title="Built-in torrent worker" color=token::TEAL>
                    "VPN-isolated, drives torrents through the database queue. Selected by default — nothing to configure."
                </Alert>
            </div>
        }
        .into_any(),
        _ => view! {
            <div class="setup-step setup-done">
                <div class="setup-done-badge">"✓"</div>
                <h2>"You're all set"</h2>
                <div class="setup-summary mono">
                    <div>"Domains · " {move || {
                        let d = domains.get();
                        let mut v: Vec<&str> = Vec::new();
                        if d.contains("movies") { v.push("Movies"); }
                        if d.contains("audiobooks") { v.push("Audiobooks"); }
                        if v.is_empty() { "none".to_string() } else { v.join(", ") }
                    }}</div>
                    <div>"Library root · derived (SKADI_LIBRARY_ROOT)"</div>
                    <div>"Indexer · " {move || {
                        let u = ix_url.get();
                        if u.is_empty() { "skipped".to_string() } else { u }
                    }}</div>
                    <div>"Downloader · built-in torrent worker"</div>
                </div>
            </div>
        }
        .into_any(),
    }
    };

    // Callbacks (Copy) so the Portal's children closure stays `Fn`.
    let back = Callback::new(move |()| step.update(|s| *s = s.saturating_sub(1)));
    let next = Callback::new(move |()| {
        if step.get_untracked() + 1 >= STEPS.len() {
            finish();
        } else {
            step.update(|s| *s += 1);
        }
    });

    view! {
        // Portal to <body> so the full-screen first-run overlay escapes the app
        // shell's `main` (which is a containing block for fixed descendants).
        <Portal>
        <div class="setup-overlay">
            <div class="setup-header">
                <div class="brand">
                    <svg class="brand-mark" width="20" height="20" viewBox="0 0 24 24" fill="none"
                        stroke="var(--brand-stroke)" stroke-width="1.7" stroke-linecap="round">
                        <line x1="12" y1="3" x2="12" y2="21"></line>
                        <line x1="4.2" y1="7.5" x2="19.8" y2="16.5"></line>
                        <line x1="19.8" y1="7.5" x2="4.2" y2="16.5"></line>
                    </svg>
                    <span class="brand-name">"Skadi"</span>
                    <span class="setup-tag mono">"· first-run setup"</span>
                </div>
                <div class="setup-stepper">{stepper}</div>
            </div>
            <div class="setup-content">{content}</div>
            <div class="setup-footer">
                {move || (step.get() > 0).then(|| view! {
                    <Button variant="default" on_click=back>"Back"</Button>
                })}
                <Button class:setup-next=true loading=applying on_click=next>
                    {move || {
                        if step.get() + 1 >= STEPS.len() { "Enter Skadi" } else { "Continue" }
                    }}
                </Button>
            </div>
        </div>
        </Portal>
    }
}
