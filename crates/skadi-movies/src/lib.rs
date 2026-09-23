//! `skadi-movies` — the movies domain.
//!
//! The first concrete [`DomainModule`] on top of the hunter (SKADI-I-0007).
//! Owns the [`Movie`] library entity, its [`MovieEdition`] acquirables, the
//! runtime-configurable [`EditionKind`] registry, the schema they live in, the
//! TMDB-backed metadata sync, the file-naming/matching policy the importer
//! needs, the sweep query the hunter needs, and a `MoviesModule` that wires it
//! all together for the daemon to mount.

use diesel_migrations::{EmbeddedMigrations, embed_migrations};

pub mod edition;
pub mod http;
pub mod import;
pub mod maintenance;
pub mod matcher;
pub mod metadata;
pub mod module;
pub mod movie;
pub mod naming;
pub mod nfo;
pub mod refresh;
pub mod repo;
pub mod schema;
pub mod status_sink;
pub mod wanted;

pub use edition::{EditionKind, MovieEdition};
pub use http::{MoviesHttp, MoviesLibrary};
pub use matcher::MovieMatcher;
pub use metadata::{MovieDefaults, add_movie, refresh_movie};
pub use module::{DEFAULT_SWEEP_INTERVAL, MoviesModule, SharedHunterDeps};
pub mod scan;
pub use movie::{Movie, MovieCollection};
pub use naming::canonical_movie_path;
pub use refresh::{
    MovieRefreshWorker, RefreshServices, reset_refresh_services, set_refresh_services,
};
pub use repo::{MovieFilter, MoviesRepo};
pub use status_sink::MovieStatusSink;
pub use wanted::{MovieWantedQuery, WantedScoring};

/// Embedded SQLite migrations for the movies schema (delivered to the daemon
/// via `MoviesModule::migrations()` in T-0049).
pub const SQLITE_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-sqlite");

/// Embedded Postgres migrations for the movies schema.
pub const POSTGRES_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-postgres");

/// Deterministic UUID for the built-in **Theatrical** [`EditionKind`] row.
/// Seeded by migrations; the matcher (T-0046) uses this as its fallback when
/// no other kind matches a release.
pub const THEATRICAL_KIND_ID: uuid::Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000001");
