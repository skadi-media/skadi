//! Shared BDD world for `skadi-client` (SKADI-I-0057 pass P6).
//!
//! The client is exercised against a **recorded-response** daemon: a tiny
//! HTTP/1.1 server on `127.0.0.1:0` (tokio only — the crate has no test-server
//! dependency) that answers every request with one canned status + body and
//! records what the client sent (method, path, headers). No network beyond
//! loopback, no real daemon.
pub mod steps;

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One request as the fake daemon saw it.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The canned answer the fake daemon gives.
#[derive(Debug, Clone)]
pub struct Canned {
    pub status: u16,
    pub content_type: String,
    pub body: String,
    /// Never answer (hold the connection open) — for timeout scenarios.
    pub hang: bool,
}

impl Default for Canned {
    fn default() -> Self {
        Canned {
            status: 200,
            content_type: "application/json".into(),
            body: "{}".into(),
            hang: false,
        }
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// Base URL of the fake daemon (`http://127.0.0.1:<port>`), once started.
    pub base_url: Option<String>,
    /// What the fake daemon answers.
    pub canned: Arc<Mutex<Canned>>,
    /// Every request the fake daemon received.
    pub seen: Arc<Mutex<Vec<Seen>>>,
    /// The token the client is built with.
    pub token: Option<String>,
    /// The last client call's outcome.
    pub result: Option<Result<serde_json::Value, skadi_client::ApiError>>,
}

impl World {
    /// Start the fake daemon (idempotent) and return its base URL.
    pub async fn daemon(&mut self) -> String {
        if let Some(u) = &self.base_url {
            return u.clone();
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let canned = self.canned.clone();
        let seen = self.seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let canned = canned.lock().unwrap().clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let request = read_request(&mut sock).await;
                    seen.lock().unwrap().push(request);
                    if canned.hang {
                        // Hold the socket open forever (until the client gives up).
                        std::future::pending::<()>().await;
                    }
                    let reason = match canned.status {
                        200 => "OK",
                        201 => "Created",
                        204 => "No Content",
                        400 => "Bad Request",
                        401 => "Unauthorized",
                        404 => "Not Found",
                        500 => "Internal Server Error",
                        _ => "Status",
                    };
                    let response = format!(
                        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        canned.status,
                        reason,
                        canned.content_type,
                        canned.body.len(),
                        canned.body
                    );
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        let url = format!("http://{addr}");
        self.base_url = Some(url.clone());
        url
    }

    /// A client pointed at the fake daemon with the world's token.
    pub async fn client(&mut self) -> skadi_client::Client {
        let url = self.daemon().await;
        // A 1s request timeout, deliberately shorter than any scenario deadline
        // (SKADI-T-0470): the timeout scenario asserts the *client* gives up
        // first, which the 30s production default would not do inside a 3s test.
        skadi_client::Client::with_timeout(
            url,
            self.token.clone(),
            std::time::Duration::from_secs(1),
        )
    }

    pub fn last_seen(&self) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("the fake daemon saw no request")
    }

    pub fn result(&self) -> &Result<serde_json::Value, skadi_client::ApiError> {
        self.result.as_ref().expect("no client call was made")
    }
}

/// Parse one HTTP/1.1 request head (+ body per content-length) off the socket.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Seen {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end.min(buf.len())]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let want: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end.min(buf.len())..].to_vec();
    while body.len() < want {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Seen {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}
