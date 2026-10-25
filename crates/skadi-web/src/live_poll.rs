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
pub async fn poll_while_visible<A, T, F>(alive: A, tick: T)
where
    A: Fn() -> bool,
    T: FnMut() -> F,
    F: Future<Output = ()>,
{
    poll_every_while_visible(LIVE_POLL_MS, alive, tick).await;
}

/// [`poll_while_visible`] at a cadence of `every_ms`, for a view whose data
/// changes more slowly than Downloads (the new imports, SKADI-T-0745).
pub async fn poll_every_while_visible<A, T, F>(every_ms: u32, alive: A, mut tick: T)
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
                gloo_timers::future::TimeoutFuture::new(every_ms).await;
            }
        }
    }
}

/// The events completed in `buf` (SKADI-T-0711): each `(event name, data)`,
/// with what is left of a half-received event kept in `buf`. Server-sent
/// events end with a blank line; `:` lines are keep-alives. Pure.
pub fn parse_sse(buf: &mut String) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let normalised = buf.replace("\r\n", "\n");
    let mut rest = normalised.as_str();
    while let Some(end) = rest.find("\n\n") {
        let block = &rest[..end];
        rest = &rest[end + 2..];
        let mut name = String::from("message");
        let mut data: Vec<&str> = Vec::new();
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                name = v.trim_start().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        if !data.is_empty() {
            out.push((name, data.join("\n")));
        }
    }
    *buf = rest.to_string();
    out
}

/// The first wait before the stream is tried again, and the longest.
pub const STREAM_RETRY_MIN_MS: u32 = 2_000;
pub const STREAM_RETRY_MAX_MS: u32 = 30_000;

/// The next retry wait: doubled, capped. Pure.
#[must_use]
pub fn next_backoff(ms: u32) -> u32 {
    ms.saturating_mul(2).min(STREAM_RETRY_MAX_MS)
}

/// Server push for a page (SKADI-T-0711): while the tab is visible, read the
/// `topics` stream and hand each event to `on_event`; when the stream cannot
/// open or drops, poll with `tick` until the retry wait passes, then try the
/// stream again (the wait doubles, up to 30 s, and resets once the stream
/// delivers). A hidden tab closes the stream and waits; a torn-down page stops.
pub async fn stream_while_visible<A, E, T, F>(
    alive: A,
    topics: &'static str,
    mut on_event: E,
    mut tick: T,
) where
    A: Fn() -> bool + Copy + 'static,
    E: FnMut(&str, &str),
    T: FnMut() -> F,
    F: Future<Output = ()>,
{
    let mut backoff = STREAM_RETRY_MIN_MS;
    loop {
        match next_step(alive(), document_hidden()) {
            PollStep::Stop => break,
            PollStep::WaitVisible => next_visibility_change().await,
            PollStep::Fetch => {
                if read_stream(alive, topics, &mut on_event).await {
                    backoff = STREAM_RETRY_MIN_MS;
                }
                // The stream is closed: poll until it is time to try again.
                let mut waited = 0;
                while waited < backoff && next_step(alive(), document_hidden()) == PollStep::Fetch {
                    tick().await;
                    gloo_timers::future::TimeoutFuture::new(LIVE_POLL_MS).await;
                    waited += LIVE_POLL_MS;
                }
                backoff = next_backoff(backoff);
            }
        }
    }
}

/// Read one connection of the stream until it ends, the tab hides or the page
/// goes. Whether at least one event arrived.
async fn read_stream<A, E>(alive: A, topics: &'static str, on_event: &mut E) -> bool
where
    A: Fn() -> bool + Copy + 'static,
    E: FnMut(&str, &str),
{
    let Ok(controller) = web_sys::AbortController::new() else {
        return false;
    };
    let Ok(body) = crate::api::open_events(topics, &controller.signal()).await else {
        return false;
    };
    // Close the stream when the tab hides or the page goes: a read in
    // progress only notices at its next chunk otherwise.
    let open = std::rc::Rc::new(std::cell::Cell::new(true));
    {
        let open = open.clone();
        let controller = controller.clone();
        wasm_bindgen_futures::spawn_local(async move {
            while open.get() {
                gloo_timers::future::TimeoutFuture::new(500).await;
                if next_step(alive(), document_hidden()) != PollStep::Fetch {
                    controller.abort();
                    break;
                }
            }
        });
    }
    let reader: web_sys::ReadableStreamDefaultReader = body.get_reader().unchecked_into();
    let Ok(decoder) = web_sys::TextDecoder::new() else {
        open.set(false);
        return false;
    };
    let opts = web_sys::TextDecodeOptions::new();
    opts.set_stream(true);
    let mut buf = String::new();
    let mut delivered = false;
    loop {
        let Ok(chunk) = wasm_bindgen_futures::JsFuture::from(reader.read()).await else {
            break;
        };
        let done = js_sys::Reflect::get(&chunk, &"done".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if done {
            break;
        }
        let Ok(value) = js_sys::Reflect::get(&chunk, &"value".into()) else {
            break;
        };
        let bytes = js_sys::Uint8Array::new(&value).to_vec();
        if let Ok(text) = decoder.decode_with_u8_array_and_options(&bytes, &opts) {
            buf.push_str(&text);
        }
        for (name, data) in parse_sse(&mut buf) {
            delivered = true;
            on_event(&name, &data);
        }
    }
    open.set(false);
    controller.abort();
    delivered
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
    fn server_sent_events_are_split_and_a_half_event_is_kept() {
        let mut buf = String::from(
            "event: downloads\ndata: [1]\n\n: keep-alive\n\nevent: activity\ndata: [\ndata: 2]\n\nevent: downl",
        );
        let got = parse_sse(&mut buf);
        assert_eq!(
            got,
            vec![
                ("downloads".to_string(), "[1]".to_string()),
                ("activity".to_string(), "[\n2]".to_string()),
            ]
        );
        assert_eq!(buf, "event: downl", "the half event waits for the rest");
        buf.push_str("oads\r\ndata: []\r\n\r\n");
        assert_eq!(
            parse_sse(&mut buf),
            vec![("downloads".to_string(), "[]".to_string())]
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn the_retry_wait_doubles_up_to_a_cap() {
        assert_eq!(next_backoff(STREAM_RETRY_MIN_MS), 4_000);
        assert_eq!(next_backoff(20_000), STREAM_RETRY_MAX_MS);
        assert_eq!(next_backoff(STREAM_RETRY_MAX_MS), STREAM_RETRY_MAX_MS);
    }

    #[test]
    fn the_cadence_is_about_two_seconds() {
        assert!((1500..=2500).contains(&LIVE_POLL_MS));
    }
}
