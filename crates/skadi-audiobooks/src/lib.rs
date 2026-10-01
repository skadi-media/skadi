//! `skadi-audiobooks` — the audiobooks domain (SKADI-I-0017), the second
//! concrete vertical on Skadi's `DomainModule` abstraction (after movies).
//!
//! The model is **Author → Series → Book**:
//! - [`Author`] and [`Series`] are organizational entities (an author is
//!   monitorable for new releases; a series groups + orders books).
//! - [`Book`] is the [`LibraryItem`](skadi_core::LibraryItem) the user tracks.
//! - [`BookFile`] is the [`Acquirable`](skadi_core::Acquirable) — one audiobook
//!   per book — the unit the hunter actually acquires. Abridged-vs-unabridged is
//!   a quality-axis concern (see `skadi_quality::audiobook`), not a separate
//!   acquirable, so the acquire grain stays one-audiobook-per-book.
//!
//! This module defines the value types; the repo + schema land in SKADI-T-0125,
//! metadata sync in SKADI-T-0126, and the module wiring in SKADI-T-0129. The
//! whole crate is a deliberate parallel to `skadi-movies`.

use diesel_migrations::{EmbeddedMigrations, embed_migrations};

pub mod author;
pub mod book;
pub mod book_file;
pub mod discovery;
pub mod http;
pub mod import;
pub mod maintenance;
pub mod matcher;
pub mod metadata;
pub mod module;
pub mod naming;
pub mod refresh;
pub mod repo;
pub mod roles;
mod schema;
pub mod status_sink;
pub mod wanted;
pub mod work;

pub use author::{Author, Series, SeriesLink};
pub use book::Book;
pub use book_file::BookFile;
pub use discovery::{
    AuthorDiscovery, AuthorDiscoveryWorker, DEFAULT_DISCOVERY_INTERVAL, IngestProgress,
    MAX_PAGES_PER_PASS, ingest_author_pages, ingest_author_works,
};
pub use http::{AudiobooksHttp, AudiobooksLibrary};
pub use import::{Placement, RawCandidate, commit_item, parse_asin, scan_candidates};
pub use matcher::AudiobookMatcher;
pub use metadata::{BookDefaults, add_author, add_book, refresh_book};
pub use module::{
    AudiobooksModule, DEFAULT_SWEEP_INTERVAL, SharedHunterDeps, audiobook_builtin_profile_id,
    default_audiobook_profile,
};
pub use naming::canonical_audiobook_path;
pub use refresh::{
    BookRefreshWorker, RefreshServices, reset_refresh_services, set_refresh_services,
};
pub use repo::{AudiobooksRepo, AuthorFilter, BookFilter, WatchersRepo, WorksRepo};
pub use roles::{
    AuthorMaintenanceWorker, LinkReport, NormalizeReport, normalize_contributor_roles,
    repair_author_links,
};
pub use status_sink::AudiobookStatusSink;
pub use wanted::{AudiobookWantedQuery, WantedScoring};
pub use work::{WatchScope, Watcher, Work};

/// SQLite migrations for the audiobooks domain (generated from the logical DDL).
pub const SQLITE_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-sqlite");

/// Postgres migrations for the audiobooks domain (generated from the logical DDL).
pub const POSTGRES_MIGRATIONS: EmbeddedMigrations =
    embed_migrations!("schema/generated/migrations-postgres");
