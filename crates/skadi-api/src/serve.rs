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
        .merge(crate::new_imports::new_imports_router())
        .merge(crate::quality::quality_router())
        .merge(crate::search::search_router())
        .merge(crate::definitions::definitions_router())
        .merge(crate::naming::naming_router())
        .merge(crate::pair::pair_router())
        .merge(crate::household::household_router())
        .merge(crate::appdist::pair_apk_router())
        .merge(crate::fs::fs_router())
        .merge(crate::uploads::uploads_router())
        .merge(crate::events::events_router())
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

    let result = serve_listener(listener, app, cancel).await;
    // Send mDNS goodbye packets so clients drop us immediately (SKADI-T-0346).
    tokio::task::spawn_blocking(crate::appdist::shutdown_mdns)
        .await
        .ok();
    result
}

/// Serve `app` on `listener` until `cancel` fires, then wait for the open
/// connections to finish.
///
/// This is the loop `axum::serve` runs, written out for one reason
/// (SKADI-T-0756): HTTP/1.1 response header names go out in Title-Case
/// (`Content-Range`, not `content-range`). Header names are case-insensitive
/// and every ordinary client treats them so, but the HTTP client in Samsung's
/// TV player does not: given lowercase names it plays a file and silently
/// refuses every seek, because it never learns the server takes ranges.
/// `axum::serve` has no way to set the hyper option that fixes this.
pub async fn serve_listener(
    listener: tokio::net::TcpListener,
    app: Router,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::server::graceful::GracefulShutdown;
    use hyper_util::service::TowerToHyperService;

    let mut builder = Builder::new(TokioExecutor::new());
    builder.http1().title_case_headers(true);
    let graceful = GracefulShutdown::new();

    loop {
        let stream = tokio::select! {
            () = cancel.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                Err(e) => {
                    // A peer that went away between SYN and accept is its
                    // problem, not ours. Anything else (out of file
                    // descriptors, most likely) will not clear by retrying at
                    // once: wait, so the loop does not spin a core.
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    ) {
                        tracing::error!("accept error: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    continue;
                }
            },
        };
        let conn = builder
            .serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerToHyperService::new(app.clone()),
            )
            .into_owned();
        let conn = graceful.watch(conn);
        tokio::spawn(async move {
            // A connection error is a client that hung up or sent garbage;
            // the access log already has every request that got as far as one.
            let _ = conn.await;
        });
    }

    // Stop accepting, then let requests in flight finish, as `axum::serve`
    // with a graceful shutdown did.
    drop(listener);
    graceful.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod serve_listener_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn start() -> (
        std::net::SocketAddr,
        tokio_util::sync::CancellationToken,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        // A fallback, not a route: the OpenAPI drift test reads every route
        // in this crate's source and would ask for this one to be documented.
        let app = Router::new().fallback(|| async {
            (
                axum::http::StatusCode::PARTIAL_CONTENT,
                [
                    (axum::http::header::ACCEPT_RANGES, "bytes"),
                    (axum::http::header::CONTENT_RANGE, "bytes 0-3/4"),
                    (axum::http::header::CONTENT_TYPE, "video/mp4"),
                ],
                "data",
            )
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(serve_listener(listener, app, cancel.clone()));
        (addr, cancel, task)
    }

    /// The names as they are on the wire. A client library would hide the
    /// case, which is the whole subject, so this reads the socket.
    #[tokio::test]
    async fn response_header_names_are_title_case_on_the_wire() {
        let (addr, cancel, task) = start().await;
        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(
            b"GET /file HTTP/1.1\r\nHost: x\r\nRange: bytes=0-\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).await.unwrap();
        let raw = String::from_utf8_lossy(&raw);
        let head = raw.split("\r\n\r\n").next().unwrap();

        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        for name in [
            "Accept-Ranges",
            "Content-Range",
            "Content-Length",
            "Content-Type",
        ] {
            assert!(
                head.lines().any(|l| l.starts_with(&format!("{name}: "))),
                "no `{name}` header in:\n{head}"
            );
        }
        assert!(
            !head
                .lines()
                .skip(1)
                .any(|l| l.starts_with(char::is_lowercase)),
            "a lowercase header name in:\n{head}"
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelling_ends_the_server_and_frees_the_port() {
        let (addr, cancel, task) = start().await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("the server did not stop")
            .unwrap()
            .unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    }

    /// Graceful shutdown closes idle keep-alive connections; it does not wait
    /// for the client to hang up.
    #[tokio::test]
    async fn an_idle_keep_alive_connection_does_not_block_shutdown() {
        let (addr, cancel, task) = start().await;
        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(b"GET /file HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0u8; 512];
        let n = sock.read(&mut buf).await.unwrap();
        assert!(n > 0);
        // The connection is still open and idle. Shutdown must not wait on it.
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("an idle connection held the server open")
            .unwrap()
            .unwrap();
    }
}
