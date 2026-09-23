//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use std::time::Duration;

use skadi_http::HttpClient;
use wiremock::MockServer;

/// `MockServer` has no `Debug`; the world needs one.
pub struct Server(pub MockServer);
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MockServer({})", self.0.uri())
    }
}

/// `HttpClient` has no `Debug` either.
pub struct Client(pub HttpClient);
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HttpClient")
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub server: Option<Server>,
    pub client: Option<Client>,
    /// Outcome of the last request: `Ok(body)` or the error text.
    pub outcome: Option<Result<String, String>>,
    /// Wall-clock time the last request took.
    pub elapsed: Option<Duration>,
    /// Env vars this scenario set, restored afterwards (`@serial` only).
    pub env_touched: Vec<(String, Option<String>)>,
}

impl World {
    pub async fn server(&mut self) -> &MockServer {
        if self.server.is_none() {
            self.server = Some(Server(MockServer::start().await));
        }
        &self.server.as_ref().unwrap().0
    }

    pub fn client(&self) -> &HttpClient {
        &self
            .client
            .as_ref()
            .expect("a client (Given an egress client …)")
            .0
    }

    pub fn restore_env(&mut self) {
        for (key, prior) in self.env_touched.drain(..) {
            // SAFETY: `@serial` scenario; nothing else reads env concurrently.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }
}
