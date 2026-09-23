//! Naming settings page (SKADI-T-0299): per-domain rename templates with a live
//! preview of a real example path, rendered server-side via `/naming/preview` so the
//! operator can lock the library layout in visually before it touches disk.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

#[component]
pub fn NamingPage() -> impl IntoView {
    let movie_folder = RwSignal::new(String::new());
    let movie_file = RwSignal::new(String::new());
    let series_folder = RwSignal::new(String::new());
    let series_file = RwSignal::new(String::new());
    let ab_folder = RwSignal::new(String::new());
    let ab_file = RwSignal::new(String::new());
    let space = RwSignal::new(String::from("_"));

    let movie_preview = RwSignal::new(String::new());
    let tv_preview = RwSignal::new(String::new());
    let ab_preview = RwSignal::new(String::new());

    let saving = RwSignal::new(false);
    let msg = RwSignal::new(String::new());

    // Load current templates.
    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(s) = api::naming_settings().await {
                movie_folder.set(s.movie_folder);
                movie_file.set(s.movie_file);
                series_folder.set(s.series_folder);
                series_file.set(s.series_file);
                ab_folder.set(s.audiobook_folder);
                ab_file.set(s.audiobook_file);
                space.set(s.space);
            }
        });
    });

    // Live previews: re-render whenever a domain's templates or the space char change.
    Effect::new(move |_| {
        let (f, fi, sp) = (movie_folder.get(), movie_file.get(), space.get());
        spawn_local(async move {
            if let Ok(p) = api::naming_preview("movie", &f, &fi, &sp).await {
                movie_preview.set(p);
            }
        });
    });
    Effect::new(move |_| {
        let (f, fi, sp) = (series_folder.get(), series_file.get(), space.get());
        spawn_local(async move {
            if let Ok(p) = api::naming_preview("tv", &f, &fi, &sp).await {
                tv_preview.set(p);
            }
        });
    });
    Effect::new(move |_| {
        let (f, fi, sp) = (ab_folder.get(), ab_file.get(), space.get());
        spawn_local(async move {
            if let Ok(p) = api::naming_preview("audiobook", &f, &fi, &sp).await {
                ab_preview.set(p);
            }
        });
    });

    let on_save = move |_| {
        saving.set(true);
        msg.set(String::new());
        let s = api::NamingSettings {
            movie_folder: movie_folder.get_untracked(),
            movie_file: movie_file.get_untracked(),
            series_folder: series_folder.get_untracked(),
            series_file: series_file.get_untracked(),
            audiobook_folder: ab_folder.get_untracked(),
            audiobook_file: ab_file.get_untracked(),
            space: space.get_untracked(),
        };
        spawn_local(async move {
            match api::set_naming_settings(&s).await {
                Ok(()) => msg.set("Saved — applies to new imports.".into()),
                Err(e) => msg.set(format!("Save failed: {}", e.0)),
            }
            saving.set(false);
        });
    };

    let section = move |label: &'static str,
                        folder: RwSignal<String>,
                        file: RwSignal<String>,
                        preview: RwSignal<String>| {
        view! {
            <div class="naming-domain">
                <h3>{label}</h3>
                <label class="naming-field">
                    <span>"Folder template"</span>
                    <input
                        type="text"
                        prop:value=move || folder.get()
                        on:input=move |ev| folder.set(event_target_value(&ev))
                    />
                </label>
                <label class="naming-field">
                    <span>"File template"</span>
                    <input
                        type="text"
                        prop:value=move || file.get()
                        on:input=move |ev| file.set(event_target_value(&ev))
                    />
                </label>
                <div class="naming-preview mono">{move || preview.get()}</div>
            </div>
        }
    };

    view! {
        <crate::subnav::SubNav/>
        <div class="page-head"><h2>"Naming"</h2></div>
        <p class="muted naming-help">
            "Rename templates per domain. "<code>"{Tokens}"</code>" substitute (e.g. "
            <code>"{TitleKebab}"</code>", "<code>"{Year}"</code>", "<code>"{TmdbTag}"</code>
            "), the space character replaces whitespace. Changes apply to new imports."
        </p>
        <div class="naming-grid">
            {section("Movies", movie_folder, movie_file, movie_preview)}
            {section("TV", series_folder, series_file, tv_preview)}
            {section("Audiobooks", ab_folder, ab_file, ab_preview)}
        </div>
        <div class="naming-foot">
            <label class="naming-field naming-space">
                <span>"Space character"</span>
                <input
                    type="text"
                    maxlength="1"
                    prop:value=move || space.get()
                    on:input=move |ev| space.set(event_target_value(&ev))
                />
            </label>
            <button type="button" on:click=on_save disabled=move || saving.get()>
                {move || if saving.get() { "Saving…" } else { "Save" }}
            </button>
            <span class="muted naming-msg">{move || msg.get()}</span>
        </div>
    }
}
