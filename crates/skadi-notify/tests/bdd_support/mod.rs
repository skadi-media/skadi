//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use skadi_notify::{NotificationEvent, Notifier};
use wiremock::MockServer;

/// One scenario: an in-process webhook receiver, the notifier under test and
/// the outcome of the last delivery.
#[derive(Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub server: Option<MockServer>,
    pub notifier: Option<Box<dyn Notifier>>,
    /// The secret the notifier was built with (to re-derive the signature).
    pub secret: Option<Vec<u8>>,
    pub last_event: Option<NotificationEvent>,
    /// `Some(Ok)` delivered, `Some(Err)` failed, `None` filtered out by `wants`.
    pub delivery: Option<Result<(), String>>,
    pub test_result: Option<Result<(), String>>,
    pub config_json: Option<serde_json::Value>,
    pub build_error: Option<String>,
    pub event_json: Option<String>,
}

impl std::fmt::Debug for World {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("World")
            .field("notes", &self.notes)
            .field("last_event", &self.last_event)
            .field("delivery", &self.delivery)
            .finish_non_exhaustive()
    }
}

impl World {
    pub fn server(&self) -> &MockServer {
        self.server.as_ref().expect("a webhook endpoint must exist")
    }

    pub fn notifier(&self) -> &dyn Notifier {
        self.notifier
            .as_deref()
            .expect("a notifier must be configured first")
    }

    /// Every POST the endpoint received, as (headers, body).
    pub async fn posts(&self) -> Vec<wiremock::Request> {
        self.server()
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.method == wiremock::http::Method::POST)
            .collect()
    }
}

/// `MockServer::drop` verifies expectations with `futures::executor::block_on`;
/// doing that on a tokio worker thread while scenarios run concurrently can park
/// every worker (deadlock). Hand the server to a plain OS thread instead.
impl Drop for World {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = std::thread::spawn(move || drop(server)).join();
        }
    }
}
