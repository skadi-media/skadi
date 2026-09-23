//! Integration tests for SKADI-T-0044: prove `skadi-movies`'s tables coexist
//! with `skadi-store`'s in the user's database without colliding, on both
//! backends. Mirrors `crates/skadi-hunter/tests/integration.rs`'s storage
//! isolation pattern.

use diesel::connection::Connection;
use diesel::sql_query;
use diesel::sqlite::SqliteConnection;
use diesel::{QueryableByName, RunQueryDsl};
use diesel_migrations::MigrationHarness;

use skadi_movies::{POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS};
use skadi_store::Store;

#[derive(QueryableByName)]
struct TableName {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

fn sqlite_tables(path: &str) -> Vec<String> {
    let mut conn = SqliteConnection::establish(path).expect("open sqlite db");
    sql_query("PRAGMA busy_timeout = 10000")
        .execute(&mut conn)
        .expect("set busy_timeout");
    sql_query(
        "SELECT name FROM sqlite_master \
         WHERE type='table' AND name NOT LIKE 'sqlite_%' \
           AND name != '__diesel_schema_migrations' ORDER BY name",
    )
    .load::<TableName>(&mut conn)
    .expect("list tables")
    .into_iter()
    .map(|t| t.name)
    .collect()
}

#[tokio::test]
async fn sqlite_movies_schema_coexists_with_skadi_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skadi.db");
    let url = format!("sqlite://{}", path.display());

    // 1. skadi-store migrations.
    let store = Store::connect(&url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);

    // 2. skadi-movies migrations.
    {
        let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
    }

    // 3. Tables from both crates exist; nothing collides.
    let tables = sqlite_tables(&path.display().to_string());
    for required in [
        "domains",
        "credentials", // skadi-store
        "edition_kinds",
        "movies",
        "movie_editions", // skadi-movies
    ] {
        assert!(
            tables.iter().any(|t| t == required),
            "expected table {required:?} after both crates' migrations; got {tables:?}"
        );
    }

    // 4. Crate ownership boundary: nothing from cloacina or any other crate
    //    snuck in.
    for forbidden in ["task_executions", "workflow_executions", "schedules"] {
        assert!(
            !tables.iter().any(|t| t == forbidden),
            "unexpected cloacina table {forbidden:?} in skadi.db; got {tables:?}"
        );
    }
}

// --- Postgres-gated path ---

fn pg_url() -> Option<String> {
    std::env::var("SKADI_TEST_DATABASE_URL").ok()
}

#[tokio::test]
async fn postgres_movies_schema_coexists_when_pg_available() {
    let Some(url) = pg_url() else {
        eprintln!("SKADI_TEST_DATABASE_URL unset — skipping Postgres path");
        return;
    };

    // Migrate skadi-store first.
    let store = Store::connect(&url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);

    // Drop any leftover movies tables from a previous run, then re-migrate.
    use diesel::pg::PgConnection;
    let mut conn = PgConnection::establish(&url).expect("connect pg");
    for stmt in [
        "DROP TABLE IF EXISTS movie_editions CASCADE",
        "DROP TABLE IF EXISTS movies CASCADE",
        "DROP TABLE IF EXISTS edition_kinds CASCADE",
        // Drop only the skadi-movies migration row so re-running works; leave
        // skadi-store's rows alone.
        "DELETE FROM __diesel_schema_migrations WHERE version = '20260528000001'",
    ] {
        let _ = diesel::sql_query(stmt).execute(&mut conn);
    }
    conn.run_pending_migrations(POSTGRES_MIGRATIONS)
        .expect("apply skadi-movies migrations");

    // Verify both crates' tables present.
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        c: i64,
    }
    for table in [
        "domains",
        "credentials",
        "edition_kinds",
        "movies",
        "movie_editions",
    ] {
        let n: i64 = diesel::sql_query(format!(
            "SELECT COUNT(*)::BIGINT AS c FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = '{table}'"
        ))
        .get_result::<Count>(&mut conn)
        .expect("count table")
        .c;
        assert_eq!(n, 1, "expected table {table} to exist in public schema");
    }

    // Cleanup so reruns are idempotent.
    for stmt in [
        "DROP TABLE IF EXISTS movie_editions CASCADE",
        "DROP TABLE IF EXISTS movies CASCADE",
        "DROP TABLE IF EXISTS edition_kinds CASCADE",
        "DELETE FROM __diesel_schema_migrations WHERE version = '20260528000001'",
    ] {
        let _ = diesel::sql_query(stmt).execute(&mut conn);
    }
}
