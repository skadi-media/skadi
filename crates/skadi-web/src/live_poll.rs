//! The live poll shared by the Downloads and Activity pages (SKADI-T-0693).
//!
//! Both pages refresh on a timer. They used to poll at 500 ms (Downloads) and
//! 2.5 s (Activity) whether or not anyone could see the tab, so a phone left on
//! the page, or a few background tabs, kept the daemon busy for nothing. This
//! loop polls every [`LIVE_POLL_MS`] while the tab is visible and makes **no**
//! request while it is hidden: it parks on the document's `visibilitychange`
//! event and refreshes at once when the tab is shown again.
//!
//! The decision of each turn is the pure [`next_step`], so the host tests pin
//! it; [`poll_while_visible`] is the thin browser loop around it.

use std::future::Future;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

/// Poll cadence while the tab is visible (ms). The data changes about once a
/// second, so 2 s keeps the pages current at a quarter of the old Downloads
/// request rate.
pub const LIVE_POLL_MS: u32 = 2000;

/// What the poll loop does next. Pure, so the host tests can pin it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollStep {
    /// The page is torn down: leave the loop.
    Stop,
    /// The tab is hidden: wait for `visibilitychange`, fetch nothing.
    WaitVisible,
    /// The tab is visible: fetch, then sleep [`LIVE_POLL_MS`].
    Fetch,
}

/// The next step for a loop whose page is `alive` in a tab that is `hidden`.
#[must_use]
pub fn next_step(alive: bool, hidden: bool) -> PollStep {
    match (alive, hidden) {
        (false, _) => PollStep::Stop,
        (true, true) => PollStep::WaitVisible,
        (true, false) => PollStep::Fetch,
    }
}

/// Whether the document is hidden (a background tab, a locked phone). No
/// document (not a browser) reads as visible, so the loop still runs.
fn document_hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.hidden())
}

/// Resolves on the next `visibilitychange` of the document. The listener is
/// registered `once`, so it removes itself.
async fn next_visibility_change() {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_once(true);
        let cb = Closure::once_into_js(move || {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ = doc.add_event_listener_with_callback_and_add_event_listener_options(
            "visibilitychange",
            cb.unchecked_ref(),
            &opts,
        );
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Run `tick` every [`LIVE_POLL_MS`] while the tab is visible, and not at all
/// while it is hidden, until `alive` returns false. The first tick runs at
/// once; a tab that is shown again ticks at once too.
pub async fn poll_while_visible<A, T, F>(alive: A, mut tick: T)
where
    A: Fn() -> bool,
    T: FnMut() -> F,
    F: Future<Output = ()>,
{
    loop {
        match next_step(alive(), document_hidden()) {
            PollStep::Stop => break,
            PollStep::WaitVisible => next_visibility_change().await,
            PollStep::Fetch => {
                tick().await;
                gloo_timers::future::TimeoutFuture::new(LIVE_POLL_MS).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_tab_waits_and_fetches_nothing() {
        assert_eq!(next_step(true, true), PollStep::WaitVisible);
    }

    #[test]
    fn a_visible_tab_fetches() {
        assert_eq!(next_step(true, false), PollStep::Fetch);
    }

    #[test]
    fn a_torn_down_page_stops_whether_or_not_hidden() {
        assert_eq!(next_step(false, false), PollStep::Stop);
        assert_eq!(next_step(false, true), PollStep::Stop);
    }

    #[test]
    fn the_cadence_is_about_two_seconds() {
        assert!((1500..=2500).contains(&LIVE_POLL_MS));
    }
}
