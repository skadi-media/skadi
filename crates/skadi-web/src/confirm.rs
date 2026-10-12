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
//! action never runs unasked). The dialog is Aurora's `ConfirmDialog`
//! (SKADI-T-0719): it traps Tab inside itself, puts the first focus on
//! Cancel, so a stray Enter cancels, and focus goes back to the element that
//! had it when it closes.

use std::cell::RefCell;

use aurora_leptos::components::ConfirmDialog;
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
    /// Red confirm button.
    pub destructive: bool,
}

impl ConfirmSpec {
    /// A neutral question: the confirm button reads "Continue".
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
    /// with [`Self::confirm_label`]) and is styled as danger.
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

/// The dialog itself: Aurora's `ConfirmDialog` (SKADI-T-0719). Mount once,
/// high in the tree ([`crate::App`] does). Aurora traps Tab, closes on Esc and
/// on a click on the scrim, and puts the first focus on Cancel.
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

    // One Aurora dialog per question: its labels are fixed when it is built.
    move || {
        open.get().map(|spec| {
            let shown = RwSignal::new(true);
            let body = spec.body.clone();
            view! {
                <ConfirmDialog
                    open=shown
                    title=spec.title
                    confirm_label=spec.confirm_label
                    cancel_label=spec.cancel_label
                    danger=spec.destructive
                    on_confirm=Callback::new(move |()| settle(true))
                    on_cancel=Callback::new(move |()| settle(false))
                >
                    <p class="confirm-body">{body.clone()}</p>
                </ConfirmDialog>
            }
        })
    }
}
