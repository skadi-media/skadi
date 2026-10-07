//! The in-app confirm dialog (SKADI-T-0694), in place of the browser's native
//! `confirm()`.
//!
//! [`ConfirmDialogHost`] is mounted once, in [`crate::App`]. Any handler then
//! asks with one line:
//!
//! ```ignore
//! spawn_local(async move {
//!     if !confirm(ConfirmSpec::destructive("Delete movie", body).confirm_label("Delete")).await {
//!         return;
//!     }
//!     // … do it
//! });
//! ```
//!
//! [`confirm`] resolves `true` on the confirm button and `false` on Cancel,
//! Esc, a click on the backdrop, or when no host is mounted (a destructive
//! action never runs unasked). The dialog traps Tab inside itself, puts the
//! first focus on Cancel for a destructive question (on the confirm button
//! otherwise), and gives focus back to the element that had it when it closes.
//! Enter only confirms when the confirm button has focus: it is a plain
//! `<button>`, so Enter presses whichever button is focused.

use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::HtmlElement;

/// What the dialog asks. Build it with [`ConfirmSpec::new`] or
/// [`ConfirmSpec::destructive`], then adjust the labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmSpec {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    pub cancel_label: String,
    /// Red confirm button, and the first focus goes to Cancel.
    pub destructive: bool,
}

impl ConfirmSpec {
    /// A neutral question: the confirm button reads "Continue" and has the
    /// first focus.
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            confirm_label: "Continue".into(),
            cancel_label: "Cancel".into(),
            destructive: false,
        }
    }

    /// A destructive question: the confirm button reads "Delete" (change it
    /// with [`Self::confirm_label`]), is styled as danger, and Cancel has the
    /// first focus.
    pub fn destructive(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            confirm_label: "Delete".into(),
            destructive: true,
            ..Self::new(title, body)
        }
    }

    #[must_use]
    pub fn confirm_label(mut self, label: impl Into<String>) -> Self {
        self.confirm_label = label.into();
        self
    }

    #[must_use]
    pub fn cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = label.into();
        self
    }
}

/// One open question: how to answer it, and where focus goes back to.
struct Pending {
    resolve: js_sys::Function,
    restore: Option<HtmlElement>,
}

thread_local! {
    /// The mounted host's open question (`None` = closed). Set while a
    /// [`ConfirmDialogHost`] is mounted.
    static HOST: RefCell<Option<RwSignal<Option<ConfirmSpec>>>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

/// Ask the question in `spec`; `true` when the user confirms. A second call
/// while a question is open answers the first one `false`.
pub async fn confirm(spec: ConfirmSpec) -> bool {
    let Some(open) = HOST.with(|h| *h.borrow()) else {
        web_sys::console::warn_1(&"confirm(): no ConfirmDialogHost is mounted".into());
        return false;
    };
    // The previous question, if any, is answered "no" — without moving focus,
    // which the new one is about to take.
    if let Some(prev) = PENDING.with(|p| p.borrow_mut().take()) {
        let _ = prev.resolve.call1(&JsValue::NULL, &JsValue::FALSE);
    }
    let restore = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .and_then(|e| e.dyn_into::<HtmlElement>().ok());
    let mut resolve = None;
    let promise = js_sys::Promise::new(&mut |res, _rej| resolve = Some(res));
    let Some(resolve) = resolve else {
        return false;
    };
    PENDING.with(|p| *p.borrow_mut() = Some(Pending { resolve, restore }));
    open.set(Some(spec));
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Answer the open question, close the dialog and give focus back.
fn settle(answer: bool) {
    let Some(p) = PENDING.with(|p| p.borrow_mut().take()) else {
        return;
    };
    if let Some(open) = HOST.with(|h| *h.borrow()) {
        open.set(None);
    }
    if let Some(el) = p.restore.filter(|e| e.is_connected()) {
        let _ = el.focus();
    }
    let _ = p.resolve.call1(&JsValue::NULL, &JsValue::from_bool(answer));
}

/// Where Tab goes inside the dialog: `current` is the focused button's index
/// among `len` buttons (`None` when focus is outside the dialog). Wraps at both
/// ends, so focus never leaves. Pure, for `tests/logic.rs`.
#[must_use]
pub fn trap_next(current: Option<usize>, len: usize, back: bool) -> usize {
    if len == 0 {
        return 0;
    }
    match (current, back) {
        (None, false) => 0,
        (None, true) => len - 1,
        (Some(i), false) => (i + 1) % len,
        (Some(i), true) => (i + len - 1) % len,
    }
}

/// Esc and the Tab trap, while a question is open.
fn on_key(ev: &web_sys::KeyboardEvent, open: RwSignal<Option<ConfirmSpec>>) {
    if open.with_untracked(Option::is_none) {
        return;
    }
    match ev.key().as_str() {
        "Escape" => {
            ev.prevent_default();
            settle(false);
        }
        "Tab" => {
            let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                return;
            };
            let Ok(list) = doc.query_selector_all(".confirm-dialog button") else {
                return;
            };
            let buttons: Vec<HtmlElement> = (0..list.length())
                .filter_map(|i| list.item(i))
                .filter_map(|n| n.dyn_into::<HtmlElement>().ok())
                .collect();
            if buttons.is_empty() {
                return;
            }
            let active = doc.active_element();
            let current = buttons.iter().position(|b| {
                active
                    .as_ref()
                    .is_some_and(|a| a == b.as_ref() as &web_sys::Element)
            });
            ev.prevent_default();
            let _ = buttons[trap_next(current, buttons.len(), ev.shift_key())].focus();
        }
        _ => {}
    }
}

/// The dialog itself. Mount once, high in the tree ([`crate::App`] does).
#[component]
pub fn ConfirmDialogHost() -> impl IntoView {
    let open = RwSignal::new(None::<ConfirmSpec>);
    HOST.with(|h| *h.borrow_mut() = Some(open));
    on_cleanup(move || {
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            if *h == Some(open) {
                *h = None;
            }
        });
        if let Some(p) = PENDING.with(|p| p.borrow_mut().take()) {
            let _ = p.resolve.call1(&JsValue::NULL, &JsValue::FALSE);
        }
    });
    let keys = window_event_listener(leptos::ev::keydown, move |ev| on_key(&ev, open));
    on_cleanup(move || keys.remove());

    let cancel_ref = NodeRef::<leptos::html::Button>::new();
    let confirm_ref = NodeRef::<leptos::html::Button>::new();
    // First focus, once the buttons of a newly opened question are in the DOM.
    Effect::new(move |_| {
        let destructive = open.with(|o| o.as_ref().map(|s| s.destructive));
        let (Some(destructive), Some(cancel), Some(ok)) =
            (destructive, cancel_ref.get(), confirm_ref.get())
        else {
            return;
        };
        let _ = if destructive {
            cancel.focus()
        } else {
            ok.focus()
        };
    });

    move || {
        open.get().map(|spec| {
            let ok_class = if spec.destructive {
                "confirm-ok danger"
            } else {
                "confirm-ok primary"
            };
            view! {
                <div class="confirm-backdrop"
                    on:click=move |ev| {
                        // A click on the backdrop itself, not one that
                        // bubbled up from the dialog.
                        if ev.target() == ev.current_target() {
                            settle(false);
                        }
                    }>
                    <div class="confirm-dialog" role="alertdialog" aria-modal="true"
                        aria-labelledby="confirm-title" aria-describedby="confirm-body">
                        <h2 id="confirm-title" class="confirm-title">{spec.title}</h2>
                        <p id="confirm-body" class="confirm-body">{spec.body}</p>
                        <div class="confirm-actions">
                            <button type="button" class="confirm-cancel secondary" node_ref=cancel_ref
                                on:click=move |_| settle(false)>
                                {spec.cancel_label}
                            </button>
                            <button type="button" class=ok_class node_ref=confirm_ref
                                on:click=move |_| settle(true)>
                                {spec.confirm_label}
                            </button>
                        </div>
                    </div>
                </div>
            }
        })
    }
}
