//! The egress IP this worker observes, reported on its heartbeat (SKADI-T-0683).
//!
//! The daemon's `vpn` health check compares it with the exit IP gluetun reports
//! to the daemon. The point is to catch a misconfigured deploy: a worker that is
//! not in gluetun's network namespace downloads outside the tunnel.
//!
//! **How it is observed.** The worker asks gluetun's control server on its *own
//! loopback* (`http://127.0.0.1:8000/v1/publicip/ip` by default) for the public
//! IP. In the deploy the worker shares gluetun's network namespace
//! (`network_mode: service:gluetun`), so gluetun's control server is on the
//! worker's loopback, and its answer is the egress of every socket the worker
//! opens. A worker outside that namespace has nothing on its loopback port 8000
//! and reports nothing (`None`); a worker in another tunnel's namespace reports
//! that tunnel's IP. Either way the daemon sees the difference.
//!
//! Why not ask a public "what is my IP" service: that is a third-party request
//! from every worker every few minutes, and from a worker outside the tunnel it
//! would itself be leaked traffic. The loopback request never leaves the host,
//! and gluetun answers from the IP it already looked up when the tunnel came up,
//! so this adds no outside traffic at all. It is asked at most once per
//! [`EGRESS_REFRESH`], not every tick.

use std::time::{Duration, Instant};

use serde::Deserialize;

/// gluetun's control server as seen from inside its network namespace.
pub const DEFAULT_GLUETUN_URL: &str = "http://127.0.0.1:8000";

/// How often the worker asks gluetun again. gluetun gets a new exit IP only
/// when the tunnel reconnects, so a minute keeps the daemon's comparison close
/// to the truth without polling gluetun on every tick.
pub const EGRESS_REFRESH: Duration = Duration::from_secs(60);

/// Per-request budget: it is a loopback request, so anything slower is "no".
const EGRESS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct PublicIp {
    public_ip: Option<String>,
}

/// The `public_ip` of a gluetun `/v1/publicip/ip` body. An empty IP (gluetun has
/// not looked it up yet) and junk both mean `None`. Pure for unit testing.
#[must_use]
pub fn parse_public_ip(body: &str) -> Option<String> {
    serde_json::from_str::<PublicIp>(body)
        .ok()?
        .public_ip
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
}

/// Asks gluetun for the egress IP, at most once per refresh interval, and
/// remembers the answer between asks.
pub struct EgressProbe {
    /// gluetun's control-server base URL; `None` turns the probe off.
    url: Option<String>,
    api_key: Option<String>,
    every: Duration,
    http: reqwest::Client,
    last_asked: Option<Instant>,
    ip: Option<String>,
}

impl EgressProbe {
    /// From the environment: `SKADI_WORKER_GLUETUN_URL` (default
    /// [`DEFAULT_GLUETUN_URL`]; empty turns the probe off) and the optional
    /// `SKADI_GLUETUN_CONTROL_API_KEY` the daemon uses too.
    #[must_use]
    pub fn from_env() -> Self {
        let url = match std::env::var("SKADI_WORKER_GLUETUN_URL") {
            Ok(v) if v.trim().is_empty() => None,
            Ok(v) => Some(v),
            Err(_) => Some(DEFAULT_GLUETUN_URL.to_string()),
        };
        let api_key = std::env::var("SKADI_GLUETUN_CONTROL_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        Self::new(url, api_key, EGRESS_REFRESH)
    }

    #[must_use]
    pub fn new(url: Option<String>, api_key: Option<String>, every: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(EGRESS_TIMEOUT)
            // Never through a proxy: the question is what *this* namespace sees.
            .no_proxy()
            .build()
            .unwrap_or_default();
        EgressProbe {
            url: url.map(|u| u.trim_end_matches('/').to_string()),
            api_key,
            every,
            http,
            last_asked: None,
            ip: None,
        }
    }

    /// The egress IP to put on this heartbeat: asks gluetun when the last answer
    /// is older than the refresh interval, else the remembered one. A failed ask
    /// is `None` until the next one: the heartbeat reports what the worker sees.
    pub async fn current(&mut self) -> Option<String> {
        let url = self.url.as_deref()?;
        let due = self.last_asked.is_none_or(|at| at.elapsed() >= self.every);
        if due {
            self.last_asked = Some(Instant::now());
            let observed = ask(&self.http, url, self.api_key.as_deref()).await;
            if observed != self.ip {
                match &observed {
                    Some(ip) => tracing::info!(egress_ip = %ip, "observed VPN egress IP"),
                    None => tracing::warn!(
                        url,
                        "could not read the egress IP from gluetun on loopback; \
                         is this worker in gluetun's network namespace?"
                    ),
                }
            }
            self.ip = observed;
        }
        self.ip.clone()
    }
}

async fn ask(http: &reqwest::Client, base: &str, api_key: Option<&str>) -> Option<String> {
    let mut req = http.get(format!("{base}/v1/publicip/ip"));
    if let Some(k) = api_key {
        req = req.header("X-API-Key", k);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), "gluetun public-IP request refused");
        return None;
    }
    parse_public_ip(&resp.text().await.ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-route HTTP server on loopback that answers every request with
    /// `body` and counts the requests.
    async fn fake_gluetun(body: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[test]
    fn parses_gluetuns_public_ip_body() {
        assert_eq!(
            parse_public_ip(r#"{"public_ip":"203.0.113.7","country":"Testland"}"#).as_deref(),
            Some("203.0.113.7")
        );
        assert_eq!(
            parse_public_ip(r#"{"public_ip":""}"#),
            None,
            "not looked up yet"
        );
        assert_eq!(parse_public_ip(r#"{}"#), None);
        assert_eq!(parse_public_ip("<html>"), None);
    }

    #[tokio::test]
    async fn asks_gluetun_once_per_interval() {
        let (url, hits) = fake_gluetun(r#"{"public_ip":"203.0.113.7"}"#).await;
        let mut probe = EgressProbe::new(Some(url), None, Duration::from_secs(3600));
        assert_eq!(probe.current().await.as_deref(), Some("203.0.113.7"));
        assert_eq!(probe.current().await.as_deref(), Some("203.0.113.7"));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the second beat reuses the answer"
        );
    }

    #[tokio::test]
    async fn asks_again_when_the_interval_has_passed() {
        let (url, hits) = fake_gluetun(r#"{"public_ip":"203.0.113.7"}"#).await;
        let mut probe = EgressProbe::new(Some(url), None, Duration::ZERO);
        probe.current().await;
        probe.current().await;
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn nothing_on_loopback_is_no_egress_ip() {
        // Bind then drop: a loopback port with nothing listening, which is what
        // a worker outside gluetun's namespace sees.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let mut probe = EgressProbe::new(Some(format!("http://{addr}")), None, Duration::ZERO);
        assert_eq!(probe.current().await, None);
    }

    #[tokio::test]
    async fn no_url_is_off() {
        let mut probe = EgressProbe::new(None, None, Duration::ZERO);
        assert_eq!(probe.current().await, None);
    }
}
