//! Server push for Downloads and Activity (SKADI-T-0711).
//!
//! One background poller, alive only while at least one client listens, reads
//! the same snapshots `GET /downloads` and `GET /activity` answer with, about
//! once a second, and publishes a topic's snapshot when it changes. Every
//! listener is fanned out from that one poller, so two open tabs cost the
//! database one set of queries, not two.
//!
//! `GET /events?topics=downloads,activity` streams them as
//! `text/event-stream`. It is authenticated like every route, and a topic is
//! sent only to a role that may `GET` its route: the role gate of the
//! underlying route applies. The web client reads it with `fetch()` (which can
//! send the bearer header; `EventSource` cannot) and falls back to polling.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Extension, Query, State};
use axum::http::Method;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::Stream;
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::household::{Member, member_or_admin, path_allowed};
use crate::state::AppState;

/// How often the poller reads the snapshots.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The topics, and the route whose role gate each one follows.
pub const TOPICS: &[(&str, &str)] = &[("downloads", "/downloads"), ("activity", "/activity")];

/// One published snapshot.
#[derive(Clone, Debug)]
pub struct TopicEvent {
    pub topic: &'static str,
    pub data: Arc<String>,
}

/// The one poller and its listeners.
pub struct Hub {
    tx: broadcast::Sender<TopicEvent>,
    listeners: AtomicUsize,
    running: AtomicBool,
    last: Mutex<HashMap<&'static str, Arc<String>>>,
    /// Ticks the poller has run, across its lifetimes (for tests and logs).
    ticks: AtomicU64,
    /// How many times a poller was started.
    starts: AtomicU64,
}

impl Hub {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            tx,
            listeners: AtomicUsize::new(0),
            running: AtomicBool::new(false),
            last: Mutex::new(HashMap::new()),
            ticks: AtomicU64::new(0),
            starts: AtomicU64::new(0),
        }
    }

    /// Poller ticks so far.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    /// Pollers started so far.
    #[must_use]
    pub fn starts(&self) -> u64 {
        self.starts.load(Ordering::Relaxed)
    }

    /// Listeners now.
    #[must_use]
    pub fn listeners(&self) -> usize {
        self.listeners.load(Ordering::Relaxed)
    }

    fn cached(&self, topic: &str) -> Option<Arc<String>> {
        self.last.lock().ok()?.get(topic).cloned()
    }

    /// Publish `data` for `topic` when it differs from the last one.
    fn publish(&self, topic: &'static str, data: String) {
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        if last.get(topic).is_some_and(|d| **d == data) {
            return;
        }
        let data = Arc::new(data);
        last.insert(topic, data.clone());
        drop(last);
        let _ = self.tx.send(TopicEvent { topic, data });
    }
}

/// The process's hub.
pub fn hub() -> &'static Arc<Hub> {
    static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
    HUB.get_or_init(|| Arc::new(Hub::new()))
}

/// A listener's registration: counted while alive, uncounted on drop, so a
/// closed stream lets the poller stop.
pub struct Listening {
    hub: Arc<Hub>,
    pub rx: broadcast::Receiver<TopicEvent>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.hub.listeners.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Register a listener, starting the poller if none runs. `snapshot` reads a
/// topic's current body; the daemon passes the real handlers, tests a stub.
pub fn listen<F, Fut>(hub: &Arc<Hub>, snapshot: F) -> Listening
where
    F: Fn(&'static str) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let rx = hub.tx.subscribe();
    hub.listeners.fetch_add(1, Ordering::Relaxed);
    if !hub.running.swap(true, Ordering::AcqRel) {
        hub.starts.fetch_add(1, Ordering::Relaxed);
        let hub = hub.clone();
        tokio::spawn(async move {
            loop {
                if hub.listeners.load(Ordering::Acquire) == 0 {
                    hub.running.store(false, Ordering::Release);
                    // A listener that arrived between the check and the store
                    // finds `running` false and starts its own poller; one that
                    // arrived before the check is served by this one, so look
                    // again rather than leave it unserved.
                    if hub.listeners.load(Ordering::Acquire) == 0
                        || hub.running.swap(true, Ordering::AcqRel)
                    {
                        break;
                    }
                }
                hub.ticks.fetch_add(1, Ordering::Relaxed);
                for (topic, _) in TOPICS {
                    if let Some(data) = snapshot(topic).await {
                        hub.publish(topic, data);
                    }
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        });
    }
    Listening {
        hub: hub.clone(),
        rx,
    }
}

/// The topics of `wanted` that `role` may read: those whose route it may
/// `GET`. An unknown topic is dropped.
#[must_use]
pub fn readable_topics(role: crate::household::Role, wanted: &str) -> Vec<&'static str> {
    wanted
        .split(',')
        .map(str::trim)
        .filter_map(|w| TOPICS.iter().find(|(t, _)| *t == w))
        .filter(|(_, route)| path_allowed(role, &Method::GET, route))
        .map(|(t, _)| *t)
        .collect()
}

#[derive(Deserialize)]
struct EventsParams {
    topics: Option<String>,
}

pub fn events_router() -> Router<Arc<AppState>> {
    Router::new().route("/events", get(events))
}

/// `GET /events?topics=downloads,activity` (SKADI-T-0711).
async fn events(
    State(state): State<Arc<AppState>>,
    member: Option<Extension<Member>>,
    Query(params): Query<EventsParams>,
) -> Response {
    let member = member_or_admin(member);
    let wanted = params.topics.unwrap_or_else(|| "downloads,activity".into());
    let topics = readable_topics(member.role, &wanted);
    if topics.is_empty() {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "forbidden",
                "message": "none of the requested topics is readable by this account"
            })),
        )
            .into_response();
    }
    let hub = hub().clone();
    let listening = listen(&hub, move |topic| {
        let state = state.clone();
        async move {
            match topic {
                "downloads" => crate::library::downloads_snapshot(state).await,
                "activity" => crate::library::activity_snapshot(state).await,
                _ => None,
            }
        }
    });
    Sse::new(stream(hub, listening, topics))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

/// The cached snapshot of each topic first, so a new listener renders at
/// once, then each change.
fn stream(
    hub: Arc<Hub>,
    listening: Listening,
    topics: Vec<&'static str>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let first: Vec<TopicEvent> = topics
        .iter()
        .filter_map(|t| hub.cached(t).map(|data| TopicEvent { topic: t, data }))
        .collect();
    futures::stream::unfold(
        (first.into_iter(), listening, topics),
        |(mut first, mut listening, topics)| async move {
            let next = if let Some(e) = first.next() {
                e
            } else {
                loop {
                    match listening.rx.recv().await {
                        Ok(e) if topics.contains(&e.topic) => break e,
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            };
            let event = Event::default().event(next.topic).data(next.data.as_str());
            Some((Ok(event), (first, listening, topics)))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::household::Role;

    #[test]
    fn a_role_gets_only_the_topics_whose_route_it_may_read() {
        assert_eq!(
            readable_topics(Role::Admin, "downloads,activity"),
            vec!["downloads", "activity"]
        );
        assert_eq!(
            readable_topics(Role::Member, "downloads,activity"),
            Vec::<&str>::new()
        );
        assert_eq!(readable_topics(Role::Kid, "downloads"), Vec::<&str>::new());
        assert_eq!(
            readable_topics(Role::Admin, "downloads, bogus"),
            vec!["downloads"]
        );
    }

    /// Two listeners share one poller, and it stops when both are gone.
    #[tokio::test(start_paused = true)]
    async fn two_listeners_share_one_poller_that_stops_after_them() {
        let hub = Arc::new(Hub::new());
        let reads = Arc::new(AtomicU64::new(0));
        let snapshot = {
            let reads = reads.clone();
            move |_topic: &'static str| {
                let reads = reads.clone();
                async move {
                    reads.fetch_add(1, Ordering::Relaxed);
                    Some("[]".to_string())
                }
            }
        };
        let a = listen(&hub, snapshot.clone());
        let b = listen(&hub, snapshot.clone());
        assert_eq!(
            hub.starts(),
            1,
            "the second listener joins the running poller"
        );
        tokio::time::sleep(POLL_INTERVAL * 5 + Duration::from_millis(10)).await;
        let ticks = hub.ticks();
        assert!(
            (5..=7).contains(&ticks),
            "about one tick a second, not two: {ticks}"
        );
        assert_eq!(reads.load(Ordering::Relaxed), ticks * TOPICS.len() as u64);
        drop(a);
        drop(b);
        tokio::time::sleep(POLL_INTERVAL * 3).await;
        let stopped = hub.ticks();
        tokio::time::sleep(POLL_INTERVAL * 3).await;
        assert_eq!(hub.ticks(), stopped, "no listener, no polling");
        let _c = listen(&hub, snapshot);
        assert_eq!(hub.starts(), 2, "a new listener starts it again");
    }

    /// Only a change is published, and a new listener gets the cached copy.
    #[tokio::test]
    async fn only_changes_are_published() {
        let hub = Arc::new(Hub::new());
        let mut rx = hub.tx.subscribe();
        hub.publish("downloads", "[1]".into());
        hub.publish("downloads", "[1]".into());
        hub.publish("downloads", "[2]".into());
        assert_eq!(*rx.recv().await.unwrap().data, "[1]");
        assert_eq!(*rx.recv().await.unwrap().data, "[2]");
        assert!(rx.try_recv().is_err(), "the repeat was not sent");
        assert_eq!(
            hub.cached("downloads").as_deref().map(String::as_str),
            Some("[2]")
        );
    }
}
