//! HTTP access logging (SKADI-T-0590).
//!
//! The daemon served requests for months with **no request log at all**.
//! `tower-http`'s `trace` feature was in `Cargo.toml` and never wired to
//! anything, so when a phone reported a playback failure there was no way to
//! tell whether it had reached the daemon, what it asked for, or what it got
//! back. Diagnosis came down to guessing — and at least once, to concluding from
//! an empty `grep` that a request had never arrived when in fact nothing was
//! ever written down.
//!
//! ## Redaction is the reason this is hand-rolled
//!
//! A stock `TraceLayer` logs `request.uri()`, and this API's video URLs carry
//! the credential in the query string:
//!
//! ```text
//! /api/v1/movies/{id}/editions/{eid}/video?apikey=SECRET
//! ```
//!
//! A media player builds its own requests and will not send our `Authorization`
//! header, so `?apikey=` is the only option there (see `auth.rs`). Logging the
//! raw URI would therefore write the API token into the log file, into any log
//! shipper, and into every screenshot of a terminal. [`redact_query`] strips it
//! before anything is emitted, with the rules of [`crate::redact`].
//!
//! ## What gets logged
//!
//! One line per request at INFO: method, redacted path, status, duration, and —
//! for ranged responses — the `Range` asked for and the bytes actually returned.
//! Range detail matters because the failure mode it exists to catch (a player
//! that asks for bytes it never receives) is invisible in a plain status line:
//! a truncated 206 and a complete one are both "206".

use std::time::Instant;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Rewrite a query string so secret values become `***`.
///
/// Preserves the *shape* — which keys were present, in what order — because
/// "the client sent an apikey" and "the client sent nothing" are different
/// diagnoses and dropping the query entirely would erase that distinction.
/// The rules are [`crate::redact`]'s, so the access log and the rest of the
/// API mask the same keys the same way.
#[must_use]
pub fn redact_query(query: &str) -> String {
    crate::redact::redact(query).into_owned()
}

/// Path plus redacted query, as it should appear in a log.
#[must_use]
pub fn safe_target(uri: &axum::http::Uri) -> String {
    match uri.query() {
        Some(q) if !q.is_empty() => format!("{}?{}", uri.path(), redact_query(q)),
        _ => uri.path().to_string(),
    }
}

/// Log every request: method, redacted target, status, duration.
///
/// Middleware rather than `TraceLayer` so the URI can be redacted before it is
/// recorded — `TraceLayer` captures the raw one.
pub async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let target = safe_target(req.uri());
    // Captured before the request is consumed; a ranged response is only
    // interesting next to what was asked for.
    let range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let started = Instant::now();
    let response = next.run(req).await;
    let ms = started.elapsed().as_millis();
    let status = response.status();

    let len = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let content_range = response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    // A failed request is the one someone will come looking for, so it gets a
    // level that survives a default filter.
    if status.is_server_error() {
        tracing::error!(%method, target, status = status.as_u16(), ms, bytes = %len,
            range = range.as_deref().unwrap_or("-"), "request failed");
    } else if status == StatusCode::RANGE_NOT_SATISFIABLE || status.is_client_error() {
        tracing::warn!(%method, target, status = status.as_u16(), ms, bytes = %len,
            range = range.as_deref().unwrap_or("-"), "request rejected");
    } else if range.is_some() || content_range.is_some() {
        // Ranged: log what was asked and what came back. A player that stalls
        // mid-film shows up here as a gap between the two, which a bare status
        // line cannot express.
        tracing::info!(%method, target, status = status.as_u16(), ms,
            range = range.as_deref().unwrap_or("-"),
            content_range = content_range.as_deref().unwrap_or("-"),
            bytes = %len, "ranged request");
    } else {
        tracing::info!(%method, target, status = status.as_u16(), ms, bytes = %len, "request");
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_key_never_reaches_the_log() {
        // The case this module exists for: the video URL a media player uses.
        let q = "apikey=super-secret-token";
        let out = redact_query(q);
        assert!(
            !out.contains("super-secret-token"),
            "the credential must not survive redaction: {out}"
        );
        assert_eq!(out, "apikey=***");
    }

    #[test]
    fn redaction_is_case_insensitive_and_keeps_other_params() {
        let out = redact_query("view=summary&ApiKey=abc123&limit=50");
        assert_eq!(out, "view=summary&ApiKey=***&limit=50");
        assert!(!out.contains("abc123"));
    }

    /// The shape survives: knowing a key was *present* is itself diagnostic, so
    /// redaction must not drop the parameter altogether.
    #[test]
    fn a_redacted_key_is_still_visible_as_present() {
        assert!(redact_query("apikey=x").starts_with("apikey="));
    }

    #[test]
    fn every_known_secret_key_is_covered() {
        for k in crate::redact::SECRET_KEYS {
            let out = redact_query(&format!("{k}=leak-me"));
            assert!(!out.contains("leak-me"), "{k} was not redacted: {out}");
        }
    }

    #[test]
    fn a_query_with_no_secrets_is_untouched() {
        let q = "view=summary&limit=200&offset=0";
        assert_eq!(redact_query(q), q);
    }

    #[test]
    fn safe_target_handles_a_missing_or_empty_query() {
        let bare: axum::http::Uri = "/api/v1/movies".parse().unwrap();
        assert_eq!(safe_target(&bare), "/api/v1/movies");
        let empty: axum::http::Uri = "/api/v1/movies?".parse().unwrap();
        assert_eq!(safe_target(&empty), "/api/v1/movies");
    }

    #[test]
    fn safe_target_redacts_the_video_url_a_player_would_use() {
        let uri: axum::http::Uri = "/api/v1/movies/abc/editions/def/video?apikey=hunter2"
            .parse()
            .unwrap();
        let out = safe_target(&uri);
        assert_eq!(out, "/api/v1/movies/abc/editions/def/video?apikey=***");
        assert!(!out.contains("hunter2"));
    }

    /// A valueless parameter must not panic or be mistaken for a secret.
    #[test]
    fn a_flag_parameter_without_a_value_is_left_alone() {
        assert_eq!(redact_query("debug&apikey=s"), "debug&apikey=***");
    }
}
