//! Shared HTTP client wrapper for Skadi's external integrations (indexers,
//! downloaders, metadata providers).
//!
//! Wraps a single `reqwest::Client` with a configurable timeout and bounded
//! retry-with-backoff for idempotent GETs, and maps all transport/status
//! failures to [`skadi_core::AppError::Network`] so callers only deal with
//! `skadi_core::Result`.

use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;

use skadi_core::{AppError, Result};

/// Retry policy for idempotent requests.
#[derive(Clone, Copy, Debug)]
pub struct RetryConfig {
    /// Number of retries *after* the initial attempt.
    pub max_retries: u32,
    /// Initial backoff, doubled after each retry.
    pub base_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff: Duration::from_millis(200),
        }
    }
}

/// A cheaply-cloneable HTTP client with timeout + retry/backoff.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    retry: RetryConfig,
}

impl HttpClient {
    /// Build a client with the given request timeout and default retry policy.
    pub fn new(timeout: Duration) -> Result<Self> {
        Self::with_config(timeout, RetryConfig::default())
    }

    /// Build a client with an explicit timeout and retry policy.
    pub fn with_config(timeout: Duration, retry: RetryConfig) -> Result<Self> {
        Self::build(timeout, retry, None, &[])
    }

    /// Build a client that egresses through `proxy`, bypassing it for hosts
    /// matching `no_proxy` (SKADI-T-0522).
    ///
    /// The proxy was previously only reachable through the ambient `HTTP_PROXY`
    /// environment, which the deploy does not set for the daemon — so only
    /// cardigann searches rode gluetun (it builds its own client), while Torznab,
    /// metadata, notifiers and definition sync all egressed direct. An operator
    /// running a VPN specifically to hide their indexer traffic was leaking most
    /// of it.
    ///
    /// `no_proxy` entries are matched as host suffixes, so `.local` covers
    /// `nas.local` and a bare host matches itself. Loopback is always bypassed:
    /// sending a request for our own worker through a VPN would fail, and no
    /// operator means that by "proxy everything".
    pub fn with_proxy(
        timeout: Duration,
        retry: RetryConfig,
        proxy: &str,
        no_proxy: &[String],
    ) -> Result<Self> {
        Self::build(timeout, retry, Some(proxy), no_proxy)
    }

    fn build(
        timeout: Duration,
        retry: RetryConfig,
        proxy: Option<&str>,
        no_proxy: &[String],
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            // Identify ourselves (SKADI-T-0522). reqwest sends no User-Agent by
            // default; several trackers and metadata APIs reject or throttle an
            // anonymous client, and an operator reading an indexer's logs could
            // not tell our traffic from anyone else's.
            .user_agent(concat!("skadi/", env!("CARGO_PKG_VERSION")));
        if let Some(url) = proxy.map(str::trim).filter(|p| !p.is_empty()) {
            let bypass: Vec<String> = no_proxy.to_vec();
            let p = reqwest::Proxy::all(url)
                .map_err(|e| AppError::Config(format!("invalid proxy {url:?}: {e}")))?
                .no_proxy(reqwest::NoProxy::from_string(&bypass_list(&bypass)));
            builder = builder.proxy(p);
        }
        let inner = builder
            .build()
            .map_err(|e| AppError::Network(format!("building HTTP client: {e}")))?;
        Ok(Self { inner, retry })
    }

    /// Borrow the underlying `reqwest::Client` (for one-off / non-idempotent
    /// requests the caller wants to drive directly, e.g. POST login).
    #[must_use]
    pub fn raw(&self) -> &reqwest::Client {
        &self.inner
    }

    /// GET `url`, retrying transient failures, returning the body as text.
    pub async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self.get(url).await?;
        resp.text()
            .await
            .map_err(|e| AppError::Network(format!("reading body from {url}: {e}")))
    }

    /// GET `url`, retrying transient failures, returning the raw body bytes.
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.get(url).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| AppError::Network(format!("reading body from {url}: {e}")))
    }

    /// GET `url`, retrying transient failures, deserializing JSON into `T`.
    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let resp = self.get(url).await?;
        resp.json::<T>()
            .await
            .map_err(|e| AppError::Network(format!("decoding JSON from {url}: {e}")))
    }

    /// GET `url` with retry, returning the successful [`Response`] for custom handling.
    pub async fn get(&self, url: &str) -> Result<Response> {
        let url = url.to_string();
        self.send_idempotent(move |c| c.get(&url)).await
    }

    /// Send an idempotent request built by `build`, retrying transient failures.
    /// `build` is called fresh per attempt (a `RequestBuilder` is single-use).
    pub async fn send_idempotent<F>(&self, build: F) -> Result<Response>
    where
        F: Fn(&reqwest::Client) -> RequestBuilder,
    {
        let mut attempt = 0u32;
        let mut backoff = self.retry.base_backoff;
        // Set when the upstream told us how long to wait (SKADI-T-0510); overrides
        // this attempt's backoff.
        let mut retry_after: Option<Duration> = None;
        loop {
            match build(&self.inner).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp)
                    if is_retryable_status(resp.status()) && attempt < self.retry.max_retries =>
                {
                    // Honour `Retry-After` (SKADI-T-0510). We already retried a
                    // 429, but on our own exponential backoff — which for a base
                    // of a few hundred ms is far shorter than the window an API
                    // actually wants, so we hammered a rate-limited upstream and
                    // burned our retries before it would have answered. The
                    // upstream is the authority on when to come back.
                    retry_after = parse_retry_after(&resp);
                    tracing::debug!(
                        status = %resp.status(),
                        attempt,
                        retry_after_secs = retry_after.map(|d| d.as_secs()),
                        "retrying after retryable status"
                    );
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                    // A 404 is an answer, not a failure (SKADI-T-0502): the
                    // upstream is up and telling us the item is gone. Mapping it
                    // to `Network` made "TMDB removed this movie" and "TMDB is
                    // unreachable" indistinguishable, so a metadata sync could
                    // not tell a deleted item from an outage — and treating the
                    // former as the latter means retrying forever.
                    return Err(AppError::NotFound(format!(
                        "HTTP 404 from {}",
                        resp.url().path()
                    )));
                }
                Ok(resp) => {
                    return Err(AppError::Network(format!("HTTP {}", resp.status())));
                }
                Err(e) if is_retryable_err(&e) && attempt < self.retry.max_retries => {
                    tracing::debug!(error = %e, attempt, "retrying after transport error");
                }
                Err(e) => return Err(AppError::Network(e.to_string())),
            }
            // The upstream's own figure wins when it gave one; otherwise our
            // exponential backoff. Capped so a hostile or mistaken header cannot
            // park a request for hours.
            let wait = retry_after
                .take()
                .map(|d| d.min(MAX_RETRY_AFTER))
                // Jitter the backoff (SKADI-T-0522). Without it, every request
                // that failed in the same burst — a provider reconcile fanning
                // out over a dozen indexers, or a sweep hitting one that just
                // went down — retries at exactly the same instant, re-creating
                // the thundering herd that caused the failure. Not applied to a
                // `Retry-After`: the upstream named a time, and spreading around
                // it would mean some attempts land early.
                .unwrap_or_else(|| jittered(backoff));
            tokio::time::sleep(wait).await;
            attempt += 1;
            backoff = backoff.saturating_mul(2);
        }
    }
}

/// `backoff` spread by up to +25% (SKADI-T-0522), so concurrent retries do not
/// re-converge on the same instant.
///
/// **Additive only.** Spreading downward as well would let a retry fire *sooner*
/// than the configured backoff, which is not what a backoff is for — the policy's
/// promise is "at least this long", and a caller that set a floor meant it. Only
/// adding keeps that promise while still de-correlating callers.
///
/// Deliberately not a cryptographic source: this only needs to be *uncorrelated*
/// between callers, and pulling in an RNG dependency for a sleep length would be
/// a poor trade. The nanosecond of the current instant differs between tasks that
/// reach this line, which is all the spread required.
fn jittered(backoff: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // 100%..=125% of the configured backoff.
    let pct = 100 + u64::from(nanos % 26);
    Duration::from_nanos(
        u64::try_from(backoff.as_nanos())
            .unwrap_or(u64::MAX)
            .saturating_mul(pct)
            / 100,
    )
}

/// The `no_proxy` list reqwest is given: the operator's entries plus loopback,
/// which is always bypassed (SKADI-T-0522).
fn bypass_list(no_proxy: &[String]) -> String {
    let mut parts: Vec<&str> = vec!["localhost", "127.0.0.1", "::1"];
    parts.extend(
        no_proxy
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty()),
    );
    parts.join(",")
}

/// The longest we will honour a `Retry-After` for (SKADI-T-0510). A mistaken or
/// hostile header should not park a request for hours; past this we fall back to
/// our own backoff, which is bounded by `max_retries`.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// `Retry-After` as a duration: either delta-seconds or an HTTP-date (RFC 9110).
///
/// Both forms are legal and real APIs use both — TMDB sends seconds, some CDNs
/// send a date — so parsing only one would silently ignore half of them.
fn parse_retry_after(resp: &Response) -> Option<Duration> {
    let raw = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date: wait until then, or not at all if it is already past.
    let when = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let delta = when.with_timezone(&chrono::Utc) - chrono::Utc::now();
    delta.to_std().ok()
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn is_retryable_err(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fast_client() -> HttpClient {
        // Tiny backoff so retry tests stay fast.
        HttpClient::with_config(
            Duration::from_secs(5),
            RetryConfig {
                max_retries: 3,
                base_backoff: Duration::from_millis(1),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn get_text_returns_body_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&server)
            .await;

        let client = fast_client();
        let body = client
            .get_text(&format!("{}/ok", server.uri()))
            .await
            .unwrap();
        assert_eq!(body, "hello");
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let server = MockServer::start().await;
        // First two calls 503, then a 200.
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(200).set_body_string("recovered"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client();
        let body = client
            .get_text(&format!("{}/flaky", server.uri()))
            .await
            .unwrap();
        assert_eq!(body, "recovered");
    }

    #[tokio::test]
    async fn gives_up_after_max_retries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/down"))
            .respond_with(ResponseTemplate::new(500))
            .expect(4) // initial + 3 retries
            .mount(&server)
            .await;

        let client = fast_client();
        let err = client
            .get_text(&format!("{}/down", server.uri()))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
    }

    #[tokio::test]
    async fn does_not_retry_client_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1) // no retries on 4xx
            .mount(&server)
            .await;

        let client = fast_client();
        let err = client
            .get_text(&format!("{}/missing", server.uri()))
            .await
            .unwrap_err();
        // The subject of this test is the `.expect(1)` above: a 4xx is not
        // retried. The error *kind* changed with SKADI-T-0502 — a 404 is the
        // upstream answering "gone", not a network failure — so it now surfaces
        // as `NotFound`, which is what lets a metadata sync tell a deleted item
        // from an outage instead of retrying forever.
        assert!(matches!(err, AppError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_non_404_client_error_is_still_a_network_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/forbidden"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client();
        let err = client
            .get_text(&format!("{}/forbidden", server.uri()))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Network(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn get_json_deserializes() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Caps {
            limit: u32,
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/caps"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"limit": 100})),
            )
            .mount(&server)
            .await;

        let client = fast_client();
        let caps: Caps = client
            .get_json(&format!("{}/caps", server.uri()))
            .await
            .unwrap();
        assert_eq!(caps, Caps { limit: 100 });
    }
}
