//! Applied schema version versus the migrations embedded in this binary
//! (SKADI-T-0682).
//!
//! Startup applies every pending migration, so in normal operation the two
//! sets are equal. They differ after a half-applied upgrade (a migration
//! failed, the daemon kept running on the old schema) or when an older image is
//! started against a database that a newer one already migrated. Both used to
//! surface only as odd query errors; [`Store::schema_status`] lets the health
//! check name them.
//!
//! The domain crates (movies, television, audiobooks) apply their own migration
//! sets into the **same** database and the same [`MIGRATIONS_TABLE`]
//! (`skadi_api::bootstrap`). So "what this binary embeds" is the store's set
//! plus each registered domain's set: the caller passes the domain versions
//! ([`Store::migration_versions`]), else every domain migration reads as
//! unknown.

use diesel::migration::MigrationSource;
use diesel::prelude::*;
use diesel_migrations::EmbeddedMigrations;

use skadi_core::{AppError, Result};

use crate::{POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS, Store};

/// The table in which diesel records each applied migration.
pub const MIGRATIONS_TABLE: &str = "__diesel_schema_migrations";

/// What the database has applied and what this binary embeds (the store's
/// migrations plus the domain versions the caller passed), as diesel migration
/// versions (`20260611000000` for `2026-06-11-000000_init`), sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStatus {
    pub applied: Vec<String>,
    pub embedded: Vec<String>,
}

impl SchemaStatus {
    /// Embedded migrations that the database has not applied: the database is
    /// **behind** this binary.
    #[must_use]
    pub fn pending(&self) -> Vec<&str> {
        self.embedded
            .iter()
            .filter(|v| !self.applied.contains(v))
            .map(String::as_str)
            .collect()
    }

    /// Applied migrations that this binary does not embed: the database is
    /// **ahead** of this binary (a newer image migrated it).
    #[must_use]
    pub fn unknown(&self) -> Vec<&str> {
        self.applied
            .iter()
            .filter(|v| !self.embedded.contains(v))
            .map(String::as_str)
            .collect()
    }

    /// The newest applied version, if any.
    #[must_use]
    pub fn latest_applied(&self) -> Option<&str> {
        self.applied.last().map(String::as_str)
    }

    /// The newest embedded version.
    #[must_use]
    pub fn latest_embedded(&self) -> Option<&str> {
        self.embedded.last().map(String::as_str)
    }
}

#[derive(QueryableByName)]
struct Version {
    #[diesel(sql_type = diesel::sql_types::Text)]
    version: String,
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

fn embedded_versions<DB: diesel::backend::Backend>(
    source: &impl MigrationSource<DB>,
) -> Result<Vec<String>> {
    let mut versions: Vec<String> = source
        .migrations()
        .map_err(|e| AppError::Internal(format!("read embedded migrations: {e}")))?
        .iter()
        .map(|m| m.name().version().to_string())
        .collect();
    versions.sort();
    Ok(versions)
}

impl Store {
    /// The versions of one migration set (a domain's) for this store's
    /// backend, sorted.
    pub fn migration_versions(
        &self,
        sqlite: EmbeddedMigrations,
        postgres: EmbeddedMigrations,
    ) -> Result<Vec<String>> {
        if self.backend_name() == "postgres" {
            embedded_versions::<diesel::pg::Pg>(&postgres)
        } else {
            embedded_versions::<diesel::sqlite::Sqlite>(&sqlite)
        }
    }

    /// Compare the migrations the database has applied with the ones this
    /// binary embeds: the store's own set plus `domain_versions` (from
    /// [`Store::migration_versions`] for each registered domain).
    ///
    /// Read only: a database that was never migrated (no [`MIGRATIONS_TABLE`])
    /// reports no applied migrations, and the table is not created.
    pub async fn schema_status(&self, domain_versions: &[String]) -> Result<SchemaStatus> {
        let domain_versions = domain_versions.to_vec();
        self.with_conn(move |conn| {
            // The same two queries on each arm: `sql_query` needs the concrete
            // backend connection.
            macro_rules! applied {
                ($c:expr, $exists_sql:expr) => {{
                    let exists: Vec<Count> = diesel::sql_query($exists_sql).load($c)?;
                    if exists.first().is_none_or(|c| c.n == 0) {
                        Ok(Vec::new())
                    } else {
                        diesel::sql_query(format!(
                            "SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"
                        ))
                        .load::<Version>($c)
                        .map(|rows| rows.into_iter().map(|r| r.version).collect())
                    }
                }};
            }
            let (applied, embedded) = conn.dispatch(
                |pg| {
                    let applied: QueryResult<Vec<String>> = (|| {
                        applied!(
                            pg,
                            "SELECT COUNT(*) AS n FROM information_schema.tables \
                             WHERE table_schema = current_schema() \
                             AND table_name = '__diesel_schema_migrations'"
                        )
                    })();
                    (
                        applied,
                        embedded_versions::<diesel::pg::Pg>(&POSTGRES_MIGRATIONS),
                    )
                },
                |sq| {
                    let applied: QueryResult<Vec<String>> = (|| {
                        applied!(
                            sq,
                            "SELECT COUNT(*) AS n FROM sqlite_master \
                             WHERE type = 'table' AND name = '__diesel_schema_migrations'"
                        )
                    })();
                    (
                        applied,
                        embedded_versions::<diesel::sqlite::Sqlite>(&SQLITE_MIGRATIONS),
                    )
                },
            );
            let mut embedded = embedded?;
            embedded.extend(domain_versions);
            embedded.sort();
            embedded.dedup();
            Ok(SchemaStatus {
                applied: applied.map_err(crate::db_err)?,
                embedded,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(applied: &[&str], embedded: &[&str]) -> SchemaStatus {
        SchemaStatus {
            applied: applied.iter().map(|s| (*s).to_string()).collect(),
            embedded: embedded.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn equal_sets_have_nothing_pending_or_unknown() {
        let s = status(&["1", "2"], &["1", "2"]);
        assert!(s.pending().is_empty());
        assert!(s.unknown().is_empty());
    }

    #[test]
    fn a_missing_middle_migration_is_pending() {
        let s = status(&["1", "3"], &["1", "2", "3"]);
        assert_eq!(s.pending(), vec!["2"]);
        assert!(s.unknown().is_empty());
    }

    #[test]
    fn an_applied_version_the_binary_lacks_is_unknown() {
        let s = status(&["1", "2", "9"], &["1", "2"]);
        assert!(s.pending().is_empty());
        assert_eq!(s.unknown(), vec!["9"]);
        assert_eq!(s.latest_applied(), Some("9"));
        assert_eq!(s.latest_embedded(), Some("2"));
    }

    #[test]
    fn embedded_versions_are_the_generated_directories() {
        let sqlite = embedded_versions::<diesel::sqlite::Sqlite>(&SQLITE_MIGRATIONS).unwrap();
        let pg = embedded_versions::<diesel::pg::Pg>(&POSTGRES_MIGRATIONS).unwrap();
        assert!(!sqlite.is_empty());
        assert_eq!(sqlite, pg, "both backends embed the same versions");
        assert_eq!(sqlite.first().map(String::as_str), Some("20260611000000"));
    }
}
