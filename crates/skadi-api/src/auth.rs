//! Bearer-token auth middleware.
//!
//! Every route except the health check requires `Authorization: Bearer <token>`
//! matching [`Config::bearer_token`](crate::config::Config::bearer_token). When
//! no token is configured the API runs in **open mode**: the middleware is a
//! no-op (a startup warning is logged in [`serve`](crate::serve)).
//!
//! The comparison is constant-time ([`subtle`]) so a timing side channel can't
//! be used to recover the token byte-by-byte.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ErrorBody;
use crate::state::AppState;

/// Env var holding the API bearer token.
///
/// Deliberately distinct from `SKADI_SECRET_KEY` (which `skadi-store` uses for
/// credential encryption) — the two secrets are unrelated.
pub const BEARER_ENV: &str = "SKADI_API_TOKEN";

/// The path that is always reachable without auth.
///
/// Note this is the *nested* path, not the externally-visible `/api/v1/health`:
/// the auth layer is attached to the inner router that [`serve`](crate::serve)
/// nests under `/api/v1`, and `Router::nest` strips that prefix from the request
/// URI before the layer runs — so the middleware sees `/health`.
const HEALTH_PATH: &str = "/health";
/// The readiness probe is unauthenticated for the same reason as liveness: a
/// deploy gate or a compose healthcheck has no token (SKADI-T-0475). Note this
/// is an exact match — `/health/checks` is the *diagnostics* endpoint and stays
/// behind auth, because it reports on the library and providers.
const READY_PATH: &str = "/health/ready";
/// Logging in is how a member *gets* a token, so it cannot require one
/// (SKADI-T-0620). Its own abuse defence is the widening delay in
/// [`household::login`](crate::household), not this layer.
const LOGIN_PATH: &str = "/auth/login";

/// Axum middleware enforcing bearer-token auth.
///
/// - Requests to [`HEALTH_PATH`] and [`READY_PATH`] always pass.
/// - Open mode (`bearer_token == None`) always passes.
/// - Otherwise the `Authorization: Bearer <token>` header must be present and
///   match in constant time, else `401`.
pub async fn bearer_auth(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == HEALTH_PATH || path == READY_PATH || path == LOGIN_PATH {
        return next.run(request).await;
    }

    // The *live* token (SKADI-T-0466), not the one resolved at startup, so
    // rotating `api_token` takes effect without a restart — during which the old
    // token kept working and the new one did not, which is the wrong way round
    // for a credential being rotated because it leaked.
    let live = state.live_token.read().await.clone();
    let mut request = request;
    if live.as_deref().is_none_or(|t| t.trim().is_empty()) {
        // Open mode: no token configured — the caller is the operator.
        request
            .extensions_mut()
            .insert(crate::household::Member::open_mode_admin());
        return next.run(request).await;
    }

    // The token names a member (SKADI-T-0611): the operator's `api_token`, or
    // one issued to a household member. Handlers read it as `Extension<Member>`.
    let member = match presented_token(&request) {
        Some(t) => state.resolve_member(&t).await,
        None => None,
    };
    match member {
        Some(m) => {
            // The raw token travels with the request so `POST /auth/logout`
            // knows which device to sign out (SKADI-T-0620).
            if let Some(t) = presented_token(&request) {
                request.extensions_mut().insert(PresentedToken(t));
            }
            request.extensions_mut().insert(m);
            next.run(request).await
        }
        None => unauthorized(),
    }
}

/// The token a request presents, by any of the three accepted routes
/// (SKADI-T-0466).
///
/// Checked in order of how deliberate they are:
/// 1. `Authorization: Bearer <token>` — the scheme is matched
///    **case-insensitively**, which RFC 6750 requires and which we got wrong: a
///    client sending `bearer` (lowercase) was rejected with no hint why.
/// 2. `X-Api-Key: <token>` — Sonarr/Radarr parity, so existing *arr tooling and
///    scripts work unchanged.
/// 3. `?apikey=<token>` — also Sonarr parity, and the only option for a consumer
///    that cannot set headers at all: an RSS/ICS reader given a feed URL
///    (SKADI-T-0465 needs this).
///
/// A query token is inherently more exposed — it lands in logs and referrers — so
/// it is last, and only reached when neither header is present.
/// The bearer token a request arrived with, carried as an extension so a
/// handler can act on *this device* rather than the member as a whole.
#[derive(Clone, Debug)]
pub struct PresentedToken(pub String);

fn presented_token(request: &Request<axum::body::Body>) -> Option<String> {
    let headers = request.headers();
    if let Some(v) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        // Split on the first space rather than `strip_prefix("Bearer ")`, so the
        // scheme can be compared without regard to case.
        if let Some((scheme, token)) = v.split_once(' ')
            && scheme.eq_ignore_ascii_case("bearer")
        {
            return Some(token.trim().to_string());
        }
    }
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return Some(v.trim().to_string());
    }
    request.uri().query().and_then(|q| {
        q.split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == "apikey")
            .map(|(_, v)| v.to_string())
    })
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        // RFC 7235: a 401 must say how to authenticate. Without this a client
        // could not tell "you need a token" from "your token is wrong" without
        // reading our docs.
        [(
            axum::http::header::WWW_AUTHENTICATE,
            r#"Bearer realm="skadi""#,
        )],
        Json(ErrorBody {
            error: "unauthorized",
            message: "missing or invalid bearer token".into(),
            field: None,
        }),
    )
        .into_response()
}
