//! Page-wide keyboard shortcuts (SKADI-T-0700): `/` puts the focus in the
//! page's search or filter field, `?` opens the list of shortcuts.
//!
//! [`ShortcutsHost`] is mounted once, in [`crate::AppFrame`]. A shortcut never
//! fires with Ctrl / Cmd / Alt held, while the focus is in a text field, or
//! while a confirm dialog is open.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// What a key press asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// `/`: focus the search / filter field of the page.
    FocusSearch,
    /// `?`: show the list of shortcuts.
    Help,
}

/// The shortcut for `key`, when one-key shortcuts are allowed at all
/// ([`crate::a11y::shortcut_allowed`]).
pub fn shortcut_for(key: &str, allowed: bool) -> Option<Shortcut> {
    if !allowed {
        return None;
    }
    match key {
        "/" => Some(Shortcut::FocusSearch),
        "?" => Some(Shortcut::Help),
        _ => None,
    }
}

/// The fields that `/` can focus, in order of preference. Only fields in
/// `<main>` count: the drawer has none.
pub const SEARCH_FIELDS: &str = "main input[type=search], main .add-search-input, \
     main .lib-filter-input, main .catalog-filter, main input[type=text]";

/// The rows of the shortcut list: (keys, what they do).
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("/", "Go to the search or filter field"),
    ("?", "Show this list"),
    ("Tab / Shift+Tab", "Go to the next / previous control"),
    (
        "Enter / Space",
        "Use the control (open a tile, expand a row)",
    ),
    ("Esc", "Close a dialog or the menu"),
    ("n / p", "Next / previous episode (watch page)"),
];

/// The `id` of the shortcut list's dialog.
pub const HELP_ID: &str = "shortcuts-help";

fn focus_search() -> bool {
    let Some(el) = document()
        .query_selector(SEARCH_FIELDS)
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    let _ = el.focus();
    true
}

fn active_element() -> Option<web_sys::HtmlElement> {
    document()
        .active_element()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
}

/// The shortcut handler and the `?` dialog. Mount once.
#[component]
pub fn ShortcutsHost() -> impl IntoView {
    let open = RwSignal::new(false);
    // Focus goes back to where it was when the list closes.
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let close_ref = NodeRef::<leptos::html::Button>::new();

    let close = move || {
        open.set(false);
        if let Some(el) = opener.get_value() {
            let _ = el.focus();
        }
    };

    let keys = window_event_listener(leptos::ev::keydown, move |ev| {
        if open.get_untracked() {
            match ev.key().as_str() {
                "Escape" => {
                    ev.prevent_default();
                    close();
                }
                // The list has one control; Tab stays on it.
                "Tab" => {
                    ev.prevent_default();
                    if let Some(b) = close_ref.get_untracked() {
                        let _ = b.focus();
                    }
                }
                _ => {}
            }
            return;
        }
        let dialog_open = document()
            .query_selector(".confirm-dialog")
            .ok()
            .flatten()
            .is_some();
        let allowed = !dialog_open && crate::a11y::shortcut_event(&ev);
        match shortcut_for(&ev.key(), allowed) {
            Some(Shortcut::FocusSearch) => {
                if focus_search() {
                    ev.prevent_default();
                }
            }
            Some(Shortcut::Help) => {
                ev.prevent_default();
                opener.set_value(active_element());
                open.set(true);
            }
            None => {}
        }
    });
    on_cleanup(move || keys.remove());

    // First focus on the Close button once the list is in the DOM.
    Effect::new(move |_| {
        if open.get()
            && let Some(b) = close_ref.get()
        {
            let _ = b.focus();
        }
    });

    move || {
        open.get().then(|| {
            let rows = SHORTCUTS
                .iter()
                .map(|(k, what)| {
                    view! {
                        <div class="shortcut-row">
                            <dt><kbd>{*k}</kbd></dt>
                            <dd>{*what}</dd>
                        </div>
                    }
                })
                .collect_view();
            view! {
                <div class="confirm-backdrop"
                    on:click=move |ev| {
                        if ev.target() == ev.current_target() {
                            close();
                        }
                    }>
                    <div class="confirm-dialog shortcuts-dialog" id=HELP_ID role="dialog"
                        aria-modal="true" aria-labelledby="shortcuts-title">
                        <h2 id="shortcuts-title" class="confirm-title">"Keyboard shortcuts"</h2>
                        <dl class="shortcut-list">{rows}</dl>
                        <div class="confirm-actions">
                            <button type="button" class="confirm-ok primary" node_ref=close_ref
                                on:click=move |_| close()>
                                "Close"
                            </button>
                        </div>
                    </div>
                </div>
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_and_question_mark_are_the_shortcuts() {
        assert_eq!(shortcut_for("/", true), Some(Shortcut::FocusSearch));
        assert_eq!(shortcut_for("?", true), Some(Shortcut::Help));
        assert_eq!(shortcut_for("a", true), None);
    }

    #[test]
    fn no_shortcut_fires_when_not_allowed() {
        assert_eq!(shortcut_for("/", false), None);
        assert_eq!(shortcut_for("?", false), None);
    }
}
