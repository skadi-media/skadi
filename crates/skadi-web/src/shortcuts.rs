//! Page-wide keyboard shortcuts (SKADI-T-0700): `/` puts the focus in the
//! page's search or filter field, `?` opens the list of shortcuts.
//!
//! [`ShortcutsHost`] is mounted once, in [`crate::AppFrame`]. A shortcut never
//! fires with Ctrl / Cmd / Alt held, while the focus is in a text field, or
//! while a confirm dialog is open.

use aurora_leptos::components::Modal;
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
pub const SEARCH_FIELDS: &str = "main input[type=search], main .add-search input, \
     main .lib-filter input, main input.lib-filter-input, main .catalog-filter input, \
     main input[type=text]";

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

/// The shortcut handler and the `?` dialog, an Aurora `Modal`
/// (SKADI-T-0719): it traps Tab, closes on Esc, the scrim or its × button
/// ("Close"), and gives focus back to the opener. Mount once.
#[component]
pub fn ShortcutsHost() -> impl IntoView {
    let open = RwSignal::new(false);

    let keys = window_event_listener(leptos::ev::keydown, move |ev| {
        // The open list handles its own keys (Aurora's Modal).
        if open.get_untracked() {
            return;
        }
        let dialog_open = document()
            .query_selector(".cl-modal-overlay")
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
                open.set(true);
            }
            None => {}
        }
    });
    on_cleanup(move || keys.remove());

    let rows = move || {
        SHORTCUTS
            .iter()
            .map(|(k, what)| {
                view! {
                    <div class="shortcut-row">
                        <dt><kbd>{*k}</kbd></dt>
                        <dd>{*what}</dd>
                    </div>
                }
            })
            .collect_view()
    };
    view! {
        <Modal open=open title="Keyboard shortcuts" size="sm">
            <dl class="shortcut-list" id=HELP_ID>{rows()}</dl>
        </Modal>
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
