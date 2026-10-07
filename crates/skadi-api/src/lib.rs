//! `skadi-api` — the HTTP control surface for the Skadi daemon (SKADI-I-0008).
//!
//! Wires `axum` to the rest of the workspace: [`Config`] reads the daemon's
//! env, [`AppState`] is the shared handle handlers see via [`axum::extract::State`],
//! [`auth`] enforces bearer-token auth, [`error`] maps [`skadi_core::AppError`] to
//! HTTP responses, and [`serve`] is the runtime entry point. Subsequent tasks
//! in I-0008 attach more endpoints (bootstrap, supervisor, settings, movies,
//! library, activity) on top of this skeleton.

pub mod access_log;
pub mod appdist;
pub mod assets;
pub mod auth;
pub mod backup;
pub mod blocklist;
pub mod bootstrap;
pub mod calendar;
pub mod config;
pub mod config_api;
pub mod definitions;
pub mod diagnostics;
pub mod domains;
pub mod error;
pub mod fs;
pub mod health;
pub mod health_checks;
pub mod household;
pub mod http_module;
pub mod import_lists;
pub mod import_lists_http;
pub mod library;
pub mod logbuf;
pub mod naming;
pub mod openapi;
pub mod pair;
pub mod providers;
pub mod quality;
pub mod ranged;
pub mod search;
pub mod serve;
pub mod settings;
pub mod state;
pub mod supervisor;
pub mod uploads;
pub mod vpn;

pub use auth::{BEARER_ENV, bearer_auth};
pub use blocklist::blocklist_router;
pub use bootstrap::{
    bootstrap, ensure_cloacina_database, load_config_view, seed_config_from_env, seed_mode_presets,
};
pub use config::{BIND_ADDR_ENV, Config, DATABASE_URL_ENV};
pub use diagnostics::{RootFolderReport, diagnostics_router};
pub use domains::domains_router;
pub use error::{ApiError, ErrorBody};
pub use health_checks::{CheckResult, HealthCheck, HealthRegistry, Severity};
pub use http_module::HttpModule;
pub use import_lists_http::import_lists_router;
pub use library::{
    LibraryEditionDto, LibraryItemDto, LibraryProvider, library_router, quality_display_name,
};
pub use providers::{
    OneProvider, ProviderReloader, ProviderSet, build_one, build_providers, provider_fingerprint,
    service_fingerprint,
};
pub use search::search_router;
pub use serve::{build_app, router, serve};
pub use settings::{SETTINGS_KINDS, secret_field_for, settings_router};
pub use state::{AppState, DomainDescriptor};
pub use supervisor::{DEFAULT_TICK_INTERVAL, Supervisor};
