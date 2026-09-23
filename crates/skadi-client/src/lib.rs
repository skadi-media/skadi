//! `skadi-client` — a typed HTTP client for the Skadi API (SKADI-T-0056).
//!
//! Per the vision principle, the CLI is just another API consumer: it talks to a
//! running daemon over HTTP through this client, exactly as a third-party tool
//! would. The client wraps `reqwest` with one typed method per endpoint, a
//! bearer token applied to every request, and a structured [`ApiError`].
//!
//! ## Response shape (v0 decision)
//!
//! Request *parameters* are typed; response *bodies* are returned as
//! [`serde_json::Value`]. The only consumer in v0 is the CLI, which prints JSON
//! verbatim, so typed response structs would add a shared-DTO crate and ongoing
//! maintenance for no v0 benefit. Typed responses can be layered on later
//! without changing call sites that already treat the body as JSON.

use serde::Serialize;
use serde_json::Value;

/// Errors surfaced by the client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Transport-level failure (connection refused, DNS, timeout, …).
    #[error("network error: {0}")]
    Network(String),
    /// 401 Unauthorized — missing or wrong bearer token.
    #[error("unauthorized")]
    Unauthorized,
    /// 404 Not Found.
    #[error("not found: {0}")]
    NotFound(String),
    /// A non-success status. `kind` and `message` are the daemon's
    /// `{error, message}` envelope when it sent one (SKADI-T-0470); `body` is the
    /// raw text either way, so nothing is lost when it did not.
    ///
    /// The envelope became reliable in SKADI-T-0458 — before that, framework
    /// rejections answered in a different shape, so decoding it would have
    /// produced a structured error for some failures and a raw body for others.
    #[error("{}", status_display(*code, kind.as_deref(), message.as_deref(), body))]
    Status {
        code: u16,
        body: String,
        /// The envelope's `error` field, e.g. `validation`, `not_found`.
        kind: Option<String>,
        /// The envelope's `message` field — the human-readable reason.
        message: Option<String>,
    },
    /// The response body was not valid JSON.
    #[error("invalid response body: {0}")]
    Decode(String),
}

type Result<T> = std::result::Result<T, ApiError>;

/// Render a failed response for a human (SKADI-T-0470).
///
/// When the daemon sent its envelope, lead with the kind and the message —
/// `validation: cutoff must be one of the allowed qualities` says what to fix.
/// Falling back to `HTTP <code>: <body>` only when there is no envelope keeps a
/// proxy's HTML error page from being mistaken for one.
fn status_display(code: u16, kind: Option<&str>, message: Option<&str>, body: &str) -> String {
    match (kind, message) {
        (Some(k), Some(m)) => format!("{k}: {m}"),
        (None, Some(m)) => format!("HTTP {code}: {m}"),
        _ => format!("HTTP {code}: {body}"),
    }
}

/// The daemon's error envelope (`skadi_api::error`), as the client decodes it.
#[derive(serde::Deserialize)]
struct ErrorEnvelope {
    error: String,
    message: String,
}

/// How long a request may take before the client gives up (SKADI-T-0470).
///
/// Without a timeout a wedged daemon hung the CLI forever — no output, no error,
/// nothing to Ctrl-C out of but the process itself. 30s is well past any healthy
/// response (the slowest real endpoint is an interactive search) while still
/// being a length a person will wait. `SKADI_CLIENT_TIMEOUT_SECS` overrides it;
/// `0` disables the timeout for the rare long-running call.
fn default_timeout() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 30;
    let secs = std::env::var("SKADI_CLIENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    // reqwest has no "no timeout" Duration, so a 0 means a value long enough to
    // be effectively none without special-casing the builder.
    std::time::Duration::from_secs(if secs == 0 { 86_400 } else { secs })
}

/// A connected client for one Skadi daemon.
#[derive(Clone)]
pub struct Client {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    /// Build a client for `base_url` (e.g. `http://127.0.0.1:8080`), with an
    /// optional bearer token (omit for an open-mode daemon).
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self::with_timeout(base_url, token, default_timeout())
    }

    /// As [`new`](Self::new) with an explicit request timeout (SKADI-T-0470).
    pub fn with_timeout(
        base_url: impl Into<String>,
        token: Option<String>,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                // A builder failure here means the TLS backend could not be
                // initialised; falling back to a default client keeps the CLI
                // usable rather than panicking at construction.
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base_url, path)
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// Send a request and map the response into a JSON value or [`ApiError`].
    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Value> {
        let resp = self
            .auth(rb)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        if status.is_success() {
            if text.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|e| ApiError::Decode(e.to_string()));
        }
        match status.as_u16() {
            401 => Err(ApiError::Unauthorized),
            404 => Err(ApiError::NotFound(text)),
            code => {
                // Decode the daemon's `{error, message}` envelope (SKADI-T-0470)
                // so a caller can branch on the kind and show the reason, instead
                // of getting a JSON blob to print raw. Both fields stay optional:
                // a proxy or a non-skadi server on the same port answers with
                // whatever it likes, and that must not become a decode failure.
                let (kind, message) = serde_json::from_str::<ErrorEnvelope>(&text)
                    .map(|e| (Some(e.error), Some(e.message)))
                    .unwrap_or((None, None));
                Err(ApiError::Status {
                    code,
                    body: text,
                    kind,
                    message,
                })
            }
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.send(self.http.get(self.url(path))).await
    }

    async fn delete(&self, path: &str) -> Result<Value> {
        self.send(self.http.delete(self.url(path))).await
    }

    async fn post_json<B: Serialize>(&self, path: &str, body: &B) -> Result<Value> {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    async fn put_json<B: Serialize>(&self, path: &str, body: &B) -> Result<Value> {
        self.send(self.http.put(self.url(path)).json(body)).await
    }

    // --- health ---

    /// `GET /health`.
    pub async fn health(&self) -> Result<Value> {
        self.get("/health").await
    }

    /// `GET /health/ready` — the readiness probe (SKADI-T-0475).
    ///
    /// Errors on 503, which is what makes it usable as a container healthcheck
    /// and a deploy gate: liveness answers as soon as the process is serving,
    /// readiness only once the database answers and providers are published.
    pub async fn readiness(&self) -> Result<Value> {
        self.get("/health/ready").await
    }

    // --- domains ---

    /// `GET /domains`.
    pub async fn domains(&self) -> Result<Value> {
        self.get("/domains").await
    }

    /// `PUT /domains/{name}` `{enabled}`.
    pub async fn set_domain_enabled(&self, name: &str, enabled: bool) -> Result<Value> {
        self.put_json(
            &format!("/domains/{name}"),
            &serde_json::json!({ "enabled": enabled }),
        )
        .await
    }

    // --- settings (generic, one of SETTINGS_KINDS) ---

    /// `GET /settings/{kind}`.
    pub async fn list_settings(&self, kind: &str) -> Result<Value> {
        self.get(&format!("/settings/{kind}")).await
    }

    /// `POST /settings/{kind}`.
    pub async fn create_setting(&self, kind: &str, body: &Value) -> Result<Value> {
        self.post_json(&format!("/settings/{kind}"), body).await
    }

    /// `PUT /settings/{kind}/{id}` — replace a record (SKADI-T-0469).
    ///
    /// A replace rather than a merge: the CLI takes one JSON body, and a partial
    /// body silently merging into a record the operator cannot see is worse than
    /// one that plainly replaces it.
    pub async fn update_setting(&self, kind: &str, id: &str, body: &Value) -> Result<Value> {
        self.put_json(&format!("/settings/{kind}/{id}"), body).await
    }

    /// `DELETE /settings/{kind}/{id}`.
    pub async fn delete_setting(&self, kind: &str, id: &str) -> Result<Value> {
        self.delete(&format!("/settings/{kind}/{id}")).await
    }

    /// `POST /settings/{kind}/{id}/test` — run the provider's connectivity
    /// check; returns `{ok: bool, error?}`.
    pub async fn test_setting(&self, kind: &str, id: &str) -> Result<Value> {
        self.post_json(
            &format!("/settings/{kind}/{id}/test"),
            &serde_json::json!({}),
        )
        .await
    }

    // --- movies ---

    /// `POST /movies`. `profile`/`root_folder` are optional — the daemon
    /// defaults to the first registered profile / root folder. `search`
    /// starts the acquire run immediately when the domain is enabled
    /// (SKADI-I-0012).
    pub async fn add_movie(
        &self,
        tmdb_id: u64,
        profile: Option<&str>,
        root_folder: Option<&str>,
        search: bool,
    ) -> Result<Value> {
        let mut body = serde_json::json!({ "tmdb_id": tmdb_id, "search": search });
        if let Some(p) = profile {
            body["profile"] = serde_json::json!(p);
        }
        if let Some(r) = root_folder {
            body["root_folder"] = serde_json::json!(r);
        }
        self.post_json("/movies", &body).await
    }

    /// `GET /movies` (optionally `?monitored=`).
    pub async fn list_movies(&self, monitored: Option<bool>) -> Result<Value> {
        let path = match monitored {
            Some(m) => format!("/movies?monitored={m}"),
            None => "/movies".to_string(),
        };
        self.get(&path).await
    }

    /// `GET /movies/{id}`.
    pub async fn get_movie(&self, id: &str) -> Result<Value> {
        self.get(&format!("/movies/{id}")).await
    }

    /// `DELETE /movies/{id}`.
    pub async fn delete_movie(&self, id: &str) -> Result<Value> {
        self.delete(&format!("/movies/{id}")).await
    }

    /// `POST /movies/{id}/editions/{eid}/acquire`.
    pub async fn acquire_edition(&self, movie_id: &str, edition_id: &str) -> Result<Value> {
        self.post_json(
            &format!("/movies/{movie_id}/editions/{edition_id}/acquire"),
            &serde_json::json!({}),
        )
        .await
    }

    /// `GET /movies/{id}/editions/{eid}/releases` — interactive search: scored
    /// release candidates for the edition (SKADI-T-0114).
    pub async fn list_releases(&self, movie_id: &str, edition_id: &str) -> Result<Value> {
        self.get(&format!(
            "/movies/{movie_id}/editions/{edition_id}/releases"
        ))
        .await
    }

    /// `POST /movies/{id}/editions/{eid}/grab` — grab a specific release the
    /// operator picked (SKADI-T-0114). `release` is a candidate object echoed
    /// from [`list_releases`](Self::list_releases).
    pub async fn grab_release(
        &self,
        movie_id: &str,
        edition_id: &str,
        release: &Value,
    ) -> Result<Value> {
        self.post_json(
            &format!("/movies/{movie_id}/editions/{edition_id}/grab"),
            &serde_json::json!({ "release": release }),
        )
        .await
    }

    // --- audiobooks (SKADI-T-0131) ---

    /// `POST /books`. `profile`/`root` are optional — the daemon defaults to the
    /// first registered profile / root folder. `search` starts the acquire run
    /// immediately when the domain is enabled.
    pub async fn add_audiobook(
        &self,
        asin: &str,
        profile: Option<&str>,
        root: Option<&str>,
        search: bool,
    ) -> Result<Value> {
        let mut body = serde_json::json!({ "asin": asin, "search": search });
        if let Some(p) = profile {
            body["profile"] = serde_json::json!(p);
        }
        if let Some(r) = root {
            body["root"] = serde_json::json!(r);
        }
        self.post_json("/books", &body).await
    }

    /// `GET /books` (optionally `?monitored=`).
    pub async fn list_audiobooks(&self, monitored: Option<bool>) -> Result<Value> {
        let path = match monitored {
            Some(m) => format!("/books?monitored={m}"),
            None => "/books".to_string(),
        };
        self.get(&path).await
    }

    /// `GET /books/{id}`.
    pub async fn get_audiobook(&self, id: &str) -> Result<Value> {
        self.get(&format!("/books/{id}")).await
    }

    /// `DELETE /books/{id}`.
    pub async fn delete_audiobook(&self, id: &str) -> Result<Value> {
        self.delete(&format!("/books/{id}")).await
    }

    /// `POST /books/merge-editions` — fold duplicate books into editions.
    pub async fn merge_book_editions(&self, apply: bool) -> Result<Value> {
        self.post_json(
            &format!("/books/merge-editions?apply={apply}"),
            &serde_json::json!({}),
        )
        .await
    }

    /// `POST /books/{id}/files/{fid}/acquire`.
    pub async fn acquire_book_file(&self, book_id: &str, file_id: &str) -> Result<Value> {
        self.post_json(
            &format!("/books/{book_id}/files/{file_id}/acquire"),
            &serde_json::json!({}),
        )
        .await
    }

    /// `GET /books/{id}/files/{fid}/releases` — interactive search: scored
    /// release candidates for the book file.
    pub async fn list_book_releases(&self, book_id: &str, file_id: &str) -> Result<Value> {
        self.get(&format!("/books/{book_id}/files/{file_id}/releases"))
            .await
    }

    /// `POST /books/{id}/files/{fid}/grab` — grab a specific release the operator
    /// picked. `release` is a candidate object echoed from
    /// [`list_book_releases`](Self::list_book_releases).
    pub async fn grab_book_release(
        &self,
        book_id: &str,
        file_id: &str,
        release: &Value,
    ) -> Result<Value> {
        self.post_json(
            &format!("/books/{book_id}/files/{file_id}/grab"),
            &serde_json::json!({ "release": release }),
        )
        .await
    }

    /// `GET /authors` (optionally `?monitored=`).
    pub async fn list_authors(&self, monitored: Option<bool>) -> Result<Value> {
        let path = match monitored {
            Some(m) => format!("/authors?monitored={m}"),
            None => "/authors".to_string(),
        };
        self.get(&path).await
    }

    /// `GET /authors/{id}`.
    pub async fn get_author(&self, id: &str) -> Result<Value> {
        self.get(&format!("/authors/{id}")).await
    }

    /// `POST /authors` — add (or return the existing) author by Audnexus ASIN.
    pub async fn add_author(&self, asin: &str) -> Result<Value> {
        self.post_json("/authors", &serde_json::json!({ "asin": asin }))
            .await
    }

    /// `GET /books/lookup?asin=` — the Audnexus metadata record for an ASIN (the
    /// "add book" preview).
    pub async fn lookup_book(&self, asin: &str) -> Result<Value> {
        self.get(&format!("/books/lookup?asin={asin}")).await
    }

    /// `GET /authors/lookup?name=` — author search candidates by name. The
    /// `name` is sent as a query param via reqwest so spaces/specials are
    /// encoded.
    pub async fn lookup_authors(&self, name: &str) -> Result<Value> {
        self.send(
            self.http
                .get(self.url("/authors/lookup"))
                .query(&[("name", name)]),
        )
        .await
    }

    // --- diagnostics (SKADI-T-0116) ---

    /// `GET /health/checks` — daemon/db/domain/provider health badges.
    pub async fn health_checks(&self) -> Result<Value> {
        self.get("/health/checks").await
    }

    /// `GET /root-folders` — per root folder free/total bytes + writability.
    pub async fn root_folders(&self) -> Result<Value> {
        self.get("/root-folders").await
    }

    // --- blocklist (SKADI-T-0115) ---

    /// `GET /blocklist` (optionally scoped to one acquirable ref).
    pub async fn list_blocklist(&self, acquirable: Option<&str>) -> Result<Value> {
        let path = match acquirable {
            Some(a) => format!("/blocklist?acquirable={a}"),
            None => "/blocklist".to_string(),
        };
        self.get(&path).await
    }

    /// `POST /blocklist` — manually block a release.
    pub async fn block_release(&self, body: &Value) -> Result<Value> {
        self.post_json("/blocklist", body).await
    }

    /// `DELETE /blocklist/{id}` — clear one entry.
    pub async fn unblock_release(&self, id: &str) -> Result<Value> {
        self.delete(&format!("/blocklist/{id}")).await
    }

    /// `DELETE /blocklist?ids=…` — remove specific entries (SKADI-T-0473).
    pub async fn unblock_releases(&self, ids: &[String]) -> Result<Value> {
        self.delete(&format!("/blocklist?ids={}", ids.join(",")))
            .await
    }

    /// `DELETE /blocklist?all=true` — clear the **entire** blocklist.
    ///
    /// Separate from [`unblock_releases`](Self::unblock_releases) on purpose: the
    /// API refuses a bare `DELETE /blocklist` (SKADI-T-0473), so wiping every
    /// block an operator has accumulated has to be asked for by name rather than
    /// reached by passing an empty id list.
    pub async fn clear_blocklist(&self) -> Result<Value> {
        self.delete("/blocklist?all=true").await
    }

    // --- history + wanted (SKADI-T-0469) ---

    /// `GET /history` — grabs and imports, newest first.
    pub async fn history(&self, limit: Option<u32>) -> Result<Value> {
        match limit {
            Some(n) => self.get(&format!("/history?limit={n}")).await,
            None => self.get("/history").await,
        }
    }

    /// `GET /history/counts` — totals by event kind.
    pub async fn history_counts(&self) -> Result<Value> {
        self.get("/history/counts").await
    }

    /// `GET /wanted` — everything monitored and still missing, across domains.
    pub async fn wanted(&self) -> Result<Value> {
        self.get("/wanted").await
    }

    /// `POST /search-all` — fan a search across every enabled indexer.
    pub async fn search_all(&self, query: Option<&str>) -> Result<Value> {
        let body = match query {
            Some(q) => serde_json::json!({ "query": q }),
            None => serde_json::json!({}),
        };
        self.post_json("/search-all", &body).await
    }

    // --- television (SKADI-T-0469) ---

    /// `GET /series` with an optional monitored filter.
    pub async fn list_series(&self, monitored: Option<bool>) -> Result<Value> {
        match monitored {
            Some(m) => self.get(&format!("/series?monitored={m}")).await,
            None => self.get("/series").await,
        }
    }

    /// `GET /series/{id}` — one series with its seasons and episodes.
    pub async fn get_series(&self, id: &str) -> Result<Value> {
        self.get(&format!("/series/{id}")).await
    }

    /// `DELETE /series/{id}` — forget a series (no file deletion).
    pub async fn delete_series(&self, id: &str) -> Result<Value> {
        self.delete(&format!("/series/{id}")).await
    }

    // --- edition kinds ---

    /// `GET /edition-kinds`.
    pub async fn list_edition_kinds(&self) -> Result<Value> {
        self.get("/edition-kinds").await
    }

    // --- library + activity ---

    /// `GET /library` with optional filters.
    pub async fn library(&self, kind: Option<&str>, monitored: Option<bool>) -> Result<Value> {
        let mut q: Vec<String> = Vec::new();
        if let Some(k) = kind {
            q.push(format!("kind={k}"));
        }
        if let Some(m) = monitored {
            q.push(format!("monitored={m}"));
        }
        let path = if q.is_empty() {
            "/library".to_string()
        } else {
            format!("/library?{}", q.join("&"))
        };
        self.get(&path).await
    }

    /// `GET /activity`.
    pub async fn activity(&self) -> Result<Value> {
        self.get("/activity").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_joins_base_and_api_prefix() {
        let c = Client::new("http://localhost:8080/", Some("tok".into()));
        assert_eq!(c.url("/health"), "http://localhost:8080/api/v1/health");
    }

    #[test]
    fn base_url_trailing_slash_trimmed() {
        let c = Client::new("http://x:1///", None);
        assert_eq!(c.url("/domains"), "http://x:1/api/v1/domains");
    }
}
