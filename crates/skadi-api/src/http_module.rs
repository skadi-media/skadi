//! The `HttpModule` trait — how domains contribute HTTP routes (SKADI-T-0054).
//!
//! `skadi-api` sits *below* the domain crates and must not depend on them, so it
//! can't host domain-specific endpoints (movies, edition kinds, …) directly.
//! Instead each domain crate depends on `skadi-api`, implements [`HttpModule`],
//! and returns a **self-contained** `axum::Router` (its own state already
//! applied via `.with_state(...)`). The daemon (`skadi run`, SKADI-T-0056)
//! collects these and hands them to [`build_app`](crate::serve::build_app),
//! which merges them under `/api/v1` behind the same bearer-auth layer.

use axum::Router;

/// A domain's HTTP surface. Routes are merged into the daemon's `/api/v1`
/// router; the auth layer is applied by the daemon, so implementors should not
/// add their own auth.
pub trait HttpModule: Send + Sync {
    /// The domain's routes, with any required state already applied so the
    /// returned router is `Router<()>`.
    fn routes(&self) -> Router;

    /// JSON schemas for the domain's response types, merged into the OpenAPI
    /// document's `components/schemas` (SKADI-T-0546).
    ///
    /// Same reason as `routes`: `skadi-api` sits below the domain crates and
    /// cannot name their types, so the domain hands them up. Defaults to none,
    /// so a module that has not described its payloads still compiles and still
    /// appears in the document with a bare `200`.
    fn schemas(&self) -> crate::openapi::Schemas {
        Vec::new()
    }
}
