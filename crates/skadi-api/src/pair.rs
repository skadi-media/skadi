//! Phone pairing, Tier 1 (SKADI-T-0333): a QR code that is simply the server's
//! URL — scan → the phone's browser opens skadi → the injected token
//! authenticates it → the PWA install prompt does the rest.
//!
//! No code exchange, no per-device tokens (squire's Keep model, deliberately
//! deferred to a future auth-hardening initiative): the offline player is
//! read-only against skadi and access is home/VPN-only.
//!
//! The encoded address comes from the request's `Host` header — whatever
//! address the desktop browser reached us at is almost certainly reachable by
//! a phone on the same network, which sidesteps guessing LAN IPs from inside a
//! container. `?url=` overrides it for exotic setups.

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use qrcode::QrCode;
use qrcode::render::svg;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct PairParams {
    /// Full URL / host override (e.g. `http://skadi.lan:8090/listen`).
    url: Option<String>,
    /// Pair as this household member (SKADI-T-0611); the QR carries their
    /// token. Absent ⇒ the operator's token, as before.
    member: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PairQr {
    /// The exact URL the QR encodes.
    url: String,
    /// Self-contained SVG markup, ready to inline into the page.
    qr_svg: String,
}

/// Render `data` as a self-contained QR SVG (dark-on-transparent, page-ready).
fn qr_svg(data: &str) -> Result<String, crate::error::ApiError> {
    let code = QrCode::new(data.as_bytes()).map_err(|e| {
        crate::error::ApiError(skadi_core::AppError::Internal(format!(
            "QR encode failed: {e}"
        )))
    })?;
    // Black on white with the quiet zone painted (SKADI-T-0616): the old
    // light-on-transparent rendering was an *inverted* code on the dark UI,
    // which some phone scanners never decode; a member's payload (host +
    // 64-hex token) is also denser than the operator's, so draw it larger.
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(300, 300)
        .quiet_zone(true)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

/// The address a phone should use, in precedence order: explicit `?url=` /
/// host, then the `SKADI_ADVERTISE_HOST` deploy override (the reliable answer
/// when the server runs in a container and can't detect the host LAN IP), then
/// the request's `Host` header.
fn resolve_host(explicit: Option<&str>, headers: &HeaderMap) -> String {
    if let Some(u) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return u.to_string();
    }
    if let Ok(adv) = std::env::var("SKADI_ADVERTISE_HOST") {
        let adv = adv.trim();
        if !adv.is_empty() {
            return adv.to_string();
        }
    }
    headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:8080")
        .to_string()
}

/// `GET /pair/qr` — the WEB-PLAYER pairing QR (open the URL in the phone
/// browser → PWA). `?url=` is a full URL override; otherwise host resolution
/// applies (see [`resolve_host`]).
async fn pair_qr(
    Query(params): Query<PairParams>,
    headers: HeaderMap,
) -> Result<Json<PairQr>, crate::error::ApiError> {
    let url = match params
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(u) if u.contains("://") => u.to_string(), // full URL override
        other => format!("http://{}/listen", resolve_host(other, &headers)),
    };
    let svg = qr_svg(&url)?;
    Ok(Json(PairQr { url, qr_svg: svg }))
}

/// `GET /pair/app` — the NATIVE-APP pairing QR. Encodes a `skadi://pair` URI
/// carrying host + token so the Android app scans ONE code and is fully paired
/// (no typed IP, no separate token step). Home-LAN trust model: the token is
/// the same one the web UI already hands any visitor.
async fn pair_app(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PairParams>,
    headers: HeaderMap,
) -> Result<Json<PairQr>, crate::error::ApiError> {
    let host = resolve_host(params.url.as_deref(), &headers);
    let token = match params.member.as_deref().filter(|m| !m.is_empty()) {
        Some(id) => {
            let dir = state.members.read().await;
            dir.members
                .iter()
                .find(|m| m.id == id)
                // A member now holds one token per device; the QR carries the
                // first, which for a not-yet-logged-in member is the one minted
                // at creation (SKADI-T-0619).
                .map(|m| {
                    m.tokens
                        .first()
                        .map(|d| d.token.clone())
                        .unwrap_or_else(|| m.token.clone())
                })
                .ok_or_else(|| {
                    crate::error::ApiError(skadi_core::AppError::NotFound(format!(
                        "member {id} not found"
                    )))
                })?
        }
        None => state.live_token.read().await.clone().unwrap_or_default(),
    };
    // skadi://pair?host=<host>&token=<token> — parsed by the app's QR scanner.
    let url = format!(
        "skadi://pair?host={}&token={}",
        urlencoding_encode(&host),
        urlencoding_encode(&token),
    );
    let svg = qr_svg(&url)?;
    Ok(Json(PairQr { url, qr_svg: svg }))
}

/// Minimal percent-encoding for the QR query values (host + token). Encodes
/// everything outside the unreserved set so `:`/`/` in a host and any token
/// byte survive the URI round-trip.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Routes: mounted under `/api/v1` with the standard bearer auth (the desktop
/// UI fetching this is already authenticated).
pub fn pair_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pair/qr", get(pair_qr))
        .route("/pair/app", get(pair_app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn qr_encodes_the_request_host() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::HOST,
            "203.0.113.50:8090".parse().unwrap(),
        );
        let Json(qr) = pair_qr(
            Query(PairParams {
                url: None,
                member: None,
            }),
            headers,
        )
        .await
        .unwrap();
        assert_eq!(qr.url, "http://203.0.113.50:8090/listen");
        assert!(qr.qr_svg.starts_with("<?xml") || qr.qr_svg.starts_with("<svg"));
    }

    #[tokio::test]
    async fn qr_honors_explicit_url_override() {
        let Json(qr) = pair_qr(
            Query(PairParams {
                url: Some("https://skadi.example/listen".into()),

                member: None,
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(qr.url, "https://skadi.example/listen");
    }

    #[test]
    fn app_uri_encoding_survives_host_and_token() {
        // Host colon + a token byte outside the unreserved set are encoded.
        assert_eq!(
            urlencoding_encode("203.0.113.27:8090"),
            "203.0.113.27%3A8090"
        );
        assert_eq!(urlencoding_encode("ab/cd+ef"), "ab%2Fcd%2Bef");
    }
}
