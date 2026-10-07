//! A reusable server-side folder picker (SKADI-T-0149) for path inputs.
//!
//! Browser file pickers choose files on the *client*; every path here is a path
//! on the *server* (`/mnt/storage/...`). So this is a small directory browser
//! backed by `GET /fs/browse`: a text field (still hand-editable) plus a
//! **Browse** button that opens an inline list of subfolders, navigable up to the
//! configured media roots, with "Use this folder" to fill the field.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

/// A path text input with a server-side folder browser attached. `value` is the
/// bound path string the caller reads back.
#[component]
pub fn PathPicker(
    /// The bound path value (read + written by the picker).
    value: RwSignal<String>,
    /// Placeholder for the text field.
    #[prop(into)]
    placeholder: String,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let listing = RwSignal::new(None::<api::FsListing>);
    let error = RwSignal::new(None::<String>);

    // Fetch a directory listing into the panel.
    let browse = move |path: Option<String>| {
        spawn_local(async move {
            match api::fs_browse(path.as_deref()).await {
                Ok(l) => {
                    listing.set(Some(l));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };

    let toggle = move |_| {
        let now = !open.get_untracked();
        open.set(now);
        if now {
            // Open at the current value (if any), else the default root.
            let v = value.get_untracked();
            browse((!v.trim().is_empty()).then_some(v));
        }
    };

    let panel = move || {
        if !open.get() {
            return ().into_any();
        }
        let err = error.get().map(|e| view! { <p class="bad">{e}</p> });
        let body = listing.get().map(|l| {
            let here = l.path.clone();
            let use_here_path = here.clone();
            let use_here = move |_| {
                value.set(use_here_path.clone());
                open.set(false);
            };
            let up_btn = l.parent.clone().map(|p| {
                let go = move |_| browse(Some(p.clone()));
                view! { <button type="button" class="fs-entry fs-up" aria-label="Up one folder" on:click=go>".."</button> }
            });
            let rows = l
                .entries
                .into_iter()
                .map(|e| {
                    let target = e.path.clone();
                    let go = move |_| browse(Some(target.clone()));
                    view! {
                        <button type="button" class="fs-entry" on:click=go>
                            {format!("\u{1F4C1} {}", e.name)}
                        </button>
                    }
                })
                .collect_view();
            view! {
                <div class="fs-head">
                    <span class="muted fs-here">{here}</span>
                    <button type="button" class="copy-btn" on:click=use_here>"Use this folder"</button>
                </div>
                <div class="fs-list">{up_btn}{rows}</div>
            }
        });
        view! { <div class="fs-browser">{err}{body}</div> }.into_any()
    };

    view! {
        <div class="path-picker">
            <div class="path-row">
                <input
                    class="path-field"
                    placeholder=placeholder
                    prop:value=move || value.get()
                    on:input=move |ev| value.set(event_target_value(&ev))
                />
                <button type="button" class="copy-btn" on:click=toggle>"Browse"</button>
            </div>
            {panel}
        </div>
    }
}
