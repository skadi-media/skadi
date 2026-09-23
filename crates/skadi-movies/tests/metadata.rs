//! `add_movie` integration test (SKADI-T-0045): exercises the orchestration
//! between `MoviesRepo` and a `MetadataProvider` on a real SQLite store with
//! the seeded `edition_kinds` registry. Refresh-only unit tests live in
//! `src/metadata.rs`.

use async_trait::async_trait;
use chrono::NaiveDate;
use diesel::connection::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::MigrationHarness;

use skadi_core::{AppError, MediaKind, ProfileId, Result, RootFolder, TmdbId};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_movies::{MoviesRepo, SQLITE_MIGRATIONS, add_movie};
use skadi_store::Store;

async fn fresh_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skadi.db");
    let url = format!("sqlite://{}", path.display());
    let store = Store::connect(&url).unwrap();
    store.run_migrations().await.unwrap();
    drop(store);
    {
        let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
    }
    (dir, Store::connect(&url).unwrap())
}

struct FakeProvider {
    record: MetadataRecord,
}

#[async_trait]
impl MetadataProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn search(&self, _q: &MetadataQuery) -> Result<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, _id: &ExternalId) -> Result<MetadataRecord> {
        Ok(self.record.clone())
    }
}

fn matrix_provider() -> FakeProvider {
    FakeProvider {
        record: MetadataRecord {
            external_ids: skadi_core::ExternalIds::default(),
            title: "The Matrix".into(),
            original_title: Some("The Matrix".into()),
            overview: Some("A hacker discovers the truth.".into()),
            runtime_minutes: Some(136),
            release_date: NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![],
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn add_movie_creates_movie_and_default_theatrical_edition() {
    let (_dir, store) = fresh_store().await;
    let provider = matrix_provider();

    let movie = add_movie(
        &store,
        &provider,
        TmdbId(603),
        ProfileId::new(),
        RootFolder::new("/movies"),
    )
    .await
    .expect("add_movie ok");

    assert_eq!(movie.title, "The Matrix");
    assert_eq!(movie.year, Some(1999));
    assert_eq!(
        movie.editions.len(),
        1,
        "default Theatrical edition created"
    );

    // Persisted: fetching via the repo returns the same data with the edition.
    let from_db = store
        .get_movie_by_tmdb(TmdbId(603))
        .await
        .unwrap()
        .expect("movie persisted");
    assert_eq!(from_db.editions.len(), 1);
    let kind = store
        .get_edition_kind(from_db.editions[0].kind)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kind.normalized_tag, "Theatrical");
}

#[tokio::test]
async fn add_movie_rejects_duplicate_tmdb_id() {
    let (_dir, store) = fresh_store().await;
    let provider = matrix_provider();
    add_movie(
        &store,
        &provider,
        TmdbId(603),
        ProfileId::new(),
        RootFolder::new("/movies"),
    )
    .await
    .unwrap();

    let err = add_movie(
        &store,
        &provider,
        TmdbId(603),
        ProfileId::new(),
        RootFolder::new("/movies"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Validation(_)));
}
