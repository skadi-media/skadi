//! Router assembly and the [`serve`] runtime entry point.

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use skadi_core::{AppError, Result};

use crate::auth::bearer_auth;
use crate::health::{health, readiness};
use crate::state::AppState;

/// Build the full `/api/v1` router with auth applied, merging in each domain's
/// self-contained routes (see [`HttpModule`](crate::http_module::HttpModule)).
///
/// The base routes (health, settings, domains) use [`AppState`]; domain routers
/// arrive with their own state already applied. The auth middleware wraps every
/// route and internally lets the health check through
/// (see [`crate::auth::bearer_auth`]).
/// The ceiling on an ordinary request body.
///
/// Generous next to any JSON this API exchanges — the largest is a
/// library-import commit batch — and far below anything that would make a
/// route worth attacking for memory. Bulk media goes through the upload chunk
/// route, which sets its own.
pub const GLOBAL_BODY_LIMIT: usize = 16 * 1024 * 1024;

pub fn build_app(state: Arc<AppState>, domain_routers: Vec<Router>) -> Router {
    build_app_with_schemas(state, domain_routers, Vec::new())
}

/// As [`build_app`], plus the JSON schemas domains contribute to the OpenAPI
/// document (SKADI-T-0546).
///
/// A separate entry point rather than a changed signature: only the daemon and
/// the e2e harness have schemas to give, and every test that stands up a router
/// would otherwise have to pass an empty vec to say nothing.
pub fn build_app_with_schemas(
    state: Arc<AppState>,
    domain_routers: Vec<Router>,
    domain_schemas: crate::openapi::Schemas,
) -> Router {
    let base = Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(readiness))
        .merge(crate::calendar::calendar_router())
        .merge(crate::config_api::config_router())
        .merge(crate::settings::settings_router())
        .merge(crate::domains::domains_router())
        .merge(crate::blocklist::blocklist_router())
        .merge(crate::import_lists_http::import_lists_router())
        .merge(crate::diagnostics::diagnostics_router())
        .merge(crate::backup::backup_router())
        .merge(crate::library::library_router())
        .merge(crate::quality::quality_router())
        .merge(crate::search::search_router())
        .merge(crate::definitions::definitions_router())
        .merge(crate::naming::naming_router())
        .merge(crate::pair::pair_router())
        .merge(crate::household::household_router())
        .merge(crate::appdist::pair_apk_router())
        .merge(crate::fs::fs_router())
        .merge(crate::uploads::uploads_router())
        // The API describes itself (SKADI-T-0472). Behind auth like everything
        // else: it enumerates the whole surface, and every client that needs it
        // already holds a token.
        .merge(crate::openapi::openapi_router(domain_schemas))
        .with_state(state.clone());

    let mut api = base;
    for routes in domain_routers {
        api = api.merge(routes);
    }

    // A wrong method on a known route answers with the envelope (SKADI-T-0458).
    // The *unknown path* case is handled outside this router, in `assets.rs`:
    // putting a fallback here would place it behind the auth layer, so an
    // unknown path would 401 instead of 404 and leak nothing useful.
    let api = api.method_not_allowed_fallback(crate::error::method_not_allowed_fallback);

    // Role gate (SKADI-T-0612) sits *inside* bearer_auth, which names the member.
    let api = api.layer(axum::middleware::from_fn(crate::household::household_gate));
    let api = api.layer(axum::middleware::from_fn_with_state(state, bearer_auth));
    let app = Router::new().nest("/api/v1", api);

    // Static web UI + SPA fallback go on LAST so the API nest above wins every
    // route it owns (see [`crate::assets`]). The page carries no credential —
    // the UI logs in (SKADI-T-0621).
    // Unauthenticated APK distribution BEFORE the SPA fallback (SKADI-T-0340):
    // a fresh phone downloads the app before it has any token.
    let app = app.merge(crate::appdist::app_dist_router());
    let app = crate::assets::attach(app);

    // An explicit request-body ceiling (SKADI-T-0630). Until now the daemon set
    // none anywhere, so axum's **unconfigured 2 MB default** silently applied
    // to every route — including `library-import/commit`, whose batches the web
    // UI already chunks into 25s and which would have failed on a large enough
    // one. A limit nobody chose is still a limit; this one is chosen.
    //
    // The upload chunk route overrides this with its own, larger layer (see
    // `crate::uploads`); a route-level `DefaultBodyLimit` wins over an outer
    // one, which is why the generous case is the exception rather than the
    // rule. Everything else stays small, so no JSON route is a memory target.
    let app = app.layer(DefaultBodyLimit::max(GLOBAL_BODY_LIMIT));

    // Access logging goes on OUTERMOST so it sees every request: the API nest,
    // the unauthenticated APK route, the static UI, and anything that 404s
    // before reaching a handler (SKADI-T-0590). Inside the auth layer it would
    // miss exactly the failures worth diagnosing.
    app.layer(axum::middleware::from_fn(crate::access_log::log_requests))
}

/// Build the app with no domain routes (used by the skeleton + most tests).
pub fn router(state: Arc<AppState>) -> Router {
    build_app(state, Vec::new())
}

/// Bind to [`Config::bind_addr`](crate::config::Config::bind_addr) and serve
/// until the state's [`CancellationToken`](tokio_util::sync::CancellationToken)
/// fires.
///
/// Logs a warning when running in open mode (no bearer token configured).
pub async fn serve(state: Arc<AppState>, domain_routers: Vec<Router>) -> Result<()> {
    serve_with_schemas(state, domain_routers, Vec::new()).await
}

/// As [`serve`], plus the domain-contributed OpenAPI schemas (SKADI-T-0546).
pub async fn serve_with_schemas(
    state: Arc<AppState>,
    domain_routers: Vec<Router>,
    domain_schemas: crate::openapi::Schemas,
) -> Result<()> {
    if state.config.bearer_token.is_none() {
        tracing::warn!(
            "{} not set — the API is running in OPEN MODE with no authentication",
            crate::auth::BEARER_ENV
        );
    }

    let addr = state.config.bind_addr;
    let cancel = state.cancel.clone();
    let app = build_app_with_schemas(state, domain_routers, domain_schemas);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AppError::Config(format!("cannot bind {addr}: {e}")))?;
    tracing::info!("skadi-api listening on {addr}");
    crate::appdist::advertise_mdns(addr.port());

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .map_err(|e| AppError::Internal(format!("axum server error: {e}")));
    // Send mDNS goodbye packets so clients drop us immediately (SKADI-T-0346).
    tokio::task::spawn_blocking(crate::appdist::shutdown_mdns)
        .await
        .ok();
    result
}
