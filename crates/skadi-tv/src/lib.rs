//! `skadi-tv` — the television domain (SKADI-I-0037).
//!
//! The Sonarr-parity domain on top of the hunter: owns the [`Series`] library
//! entity, its [`Season`]/[`Episode`] model (Episode is the [`Acquirable`]), the
//! schema they live in, and the [`TvRepo`] over `skadi-store`. Metadata sync,
//! parsing/naming, the wanted-query + matcher, the API/UI, and the
//! `TelevisionModule` land in the follow-on tasks (T-0266..0276).
//!
//! [`Acquirable`]: skadi_core::Acquirable

use diesel_migrations::{EmbeddedMigrations, embed_migrations};

pub mod acquirable;
pub mod episode;
pub mod http;
pub mod import;
pub mod maintenance;
pub mod matcher;
pub mod metadata;
pub mod module;
pub mod monitor;
pub mod naming;
pub mod refresh;
pub mod repo;
pub mod scan;
pub mod schema;
pub mod series;
pub mod status_sink;
pub mod wanted;

pub use episode::{Episode, Season};
pub use http::{TelevisionHttp, TelevisionLibrary};
pub use matcher::EpisodeMatcher;
pub use metadata::{add_series, refresh_series};
pub use module::{DEFAULT_SWEEP_INTERVAL, SharedHunterDeps, TelevisionModule};
pub use monitor::MonitorMode;
pub use naming::{EpisodeNaming, SeriesNaming, canonical_episode_path};
pub use refresh::{SeriesRefreshWorker, reset_refresh_services, set_refresh_services};
pub use repo::{SeriesFilter, TvRepo};
pub use series::{Series, SeriesType};
pub use status_sink::EpisodeStatusSink;
pub use wanted::{SeriesWantedQuery, WantedScoring};

/// Embedded SQLite migrations for the TV schema (delivered via the
/// `DomainModule` in T-0276).
pub const SQLITE_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-sqlite");

/// Embedded Postgres migrations for the TV schema.
pub const POSTGRES_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-postgres");
