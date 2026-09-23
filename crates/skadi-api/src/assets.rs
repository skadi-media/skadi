//! Static web-UI serving + SPA fallback (SKADI-T-0065).
//!
//! The Leptos front-end (`crates/skadi-web`) is built by `trunk` into `dist/`
//! and baked into this binary via `rust-embed` when the `embed-ui` feature is
//! on. [`attach`] mounts a fallback layer on the **outer** router (outside the
//! `/api/v1` nest) so:
//!
//!   * real API routes always win — they are matched before the fallback runs;
//!   * any other GET serves the matching embedded asset, or `index.html` for
//!     unknown client-side routes (the SPA fallback);
//!   * paths under `/api` that fall through are honest 404s, never the SPA.
//!
//! With `embed-ui` **off** (the default for the host workspace / tests), a tiny
//! built-in placeholder is served at `/` instead, and the binary needs no
//! `dist/` to compile.

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

/// The placeholder the UI's `index.html` ships with; the daemon swaps it for the
/// configured bearer token (or empty in open mode) when serving the file, so the
/// Mount the static/SPA fallback on the outer app router. Call this LAST, after
/// the `/api/v1` nest is in place, so API routes take precedence.
///
/// **The page carries no credential** (SKADI-T-0621). It used to: the daemon
/// substituted the operator's bearer token into `index.html` for anyone who
/// loaded it (SKADI-T-0068), which was defensible while skadi was one operator
/// on a LAN and indefensible once the household had accounts — a kid's browser
/// could read the admin token out of the page source. The UI now authenticates
/// by logging in like every other client.
pub fn attach(app: Router) -> Router {
    app.fallback(|uri: Uri| async move { static_handler(uri) })
}

/// True for paths the API owns; these must 404 rather than fall back to the SPA.
fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn static_handler(uri: Uri) -> Response {
    let path = uri.path();
    if is_api_path(path) {
        // The JSON envelope, not plain text (SKADI-T-0458): an unknown API path
        // is still an API response, and a client parsing `{error, message}`
        // should not get a bare string here of all places. This sits outside the
        // auth layer deliberately — a fallback inside it would 401 instead.
        return crate::error::ApiError(skadi_core::AppError::NotFound("no such route".into()))
            .into_response();
    }
    serve_asset(path.trim_start_matches('/'))
}

/// Serve `index.html` verbatim — no templating, and nothing secret in it.
#[cfg_attr(not(feature = "embed-ui"), allow(dead_code))]
fn index_html_response(bytes: &[u8]) -> Response {
    let html = String::from_utf8_lossy(bytes).into_owned();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html")
        .body(axum::body::Body::from(html))
        .expect("valid response")
}

#[cfg(feature = "embed-ui")]
mod embedded {
    use rust_embed::RustEmbed;

    /// The `trunk build` output, baked in at compile time.
    #[derive(RustEmbed)]
    #[folder = "../skadi-web/dist"]
    pub struct Assets;
}

/// Serve `path` from the embedded bundle, falling back to `index.html` for
/// unknown (client-side) routes.
#[cfg(feature = "embed-ui")]
fn serve_asset(path: &str) -> Response {
    let lookup = if path.is_empty() { "index.html" } else { path };

    if lookup == "index.html" {
        return match embedded::Assets::get("index.html") {
            Some(index) => index_html_response(&index.data),
            None => (StatusCode::NOT_FOUND, "ui not built").into_response(),
        };
    }

    if let Some(file) = embedded::Assets::get(lookup) {
        let mime = mime_guess::from_path(lookup).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(axum::body::Body::from(file.data.into_owned()))
            .expect("valid response");
    }

    // Unknown path → hand the SPA its shell so client-side routing can resolve.
    match embedded::Assets::get("index.html") {
        Some(index) => index_html_response(&index.data),
        None => (StatusCode::NOT_FOUND, "ui not built").into_response(),
    }
}

/// Placeholder served when the binary was built without `embed-ui`: the daemon
/// and its API work fully; only the bundled UI is absent.
#[cfg(not(feature = "embed-ui"))]
fn serve_asset(_path: &str) -> Response {
    const PLACEHOLDER: &str = concat!(
        "<!doctype html><meta charset=utf-8><title>Skadi</title>",
        "<body style=\"font-family:system-ui;background:#14161a;color:#e6e8eb;",
        "padding:2rem\"><h1>Skadi</h1><p>The API is running. This build was ",
        "compiled without the bundled web UI (the <code>embed-ui</code> ",
        "feature). The JSON API is available under <code>/api/v1</code>.</p>"
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html")],
        PLACEHOLDER,
    )
        .into_response()
}
