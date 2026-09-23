//! CRUD round-trips for `MoviesRepo` (SKADI-T-0044), backend-agnostic.
//!
//! Runs against whichever backend [`TestDb`] selects: the compose Postgres when
//! `SKADI_TEST_DATABASE_URL` is set (a unique throwaway database per test), or a
//! SQLite tempfile otherwise. Both skadi-store's and skadi-movies's migrations
//! are applied before the test sees the store.

use chrono::Utc;

use skadi_core::{
    AcquisitionStatus, EditionKindId, ExternalIds, MovieId, ProfileId, RootFolder, TmdbId,
};
use skadi_movies::{
    Movie, MovieEdition, MovieFilter, MoviesRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS,
    THEATRICAL_KIND_ID,
};
use skadi_testsupport::TestDb;

async fn fresh_store() -> TestDb {
    TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await
}

fn sample_movie() -> Movie {
    Movie {
        content_rating: None,
        genres: Vec::new(),
        id: MovieId::new(),
        external_ids: ExternalIds {
            tmdb: Some(TmdbId(603)),
            ..Default::default()
        },
        title: "The Matrix".into(),
        original_title: Some("The Matrix".into()),
        year: Some(1999),
        overview: Some("A hacker discovers the truth.".into()),
        runtime_minutes: Some(136),
        poster_url: Some("https://image.tmdb.org/t/p/w500/poster.jpg".into()),
        backdrop_url: Some("https://image.tmdb.org/t/p/w780/backdrop.jpg".into()),
        collection: Some(skadi_movies::MovieCollection {
            tmdb_id: 2344,
            name: "The Matrix Collection".into(),
        }),
        monitored: true,
        profile: ProfileId::new(),
        root_folder: RootFolder::new("/movies"),
        added_at: Utc::now(),
        last_metadata_refresh: None,
        editions: Vec::new(),
    }
}

#[tokio::test]
async fn built_in_edition_kinds_are_seeded() {
    let db = fresh_store().await;
    let store = db.store.clone();
    let kinds = store.list_edition_kinds().await.unwrap();
    assert!(
        kinds
            .iter()
            .any(|k| k.normalized_tag == "Theatrical" && k.builtin),
        "Theatrical seeded; got {:?}",
        kinds.iter().map(|k| &k.name).collect::<Vec<_>>()
    );
    assert!(
        kinds.len() >= 6,
        "expected at least six builtins, got {}",
        kinds.len()
    );

    let theatrical = store
        .get_edition_kind(EditionKindId::from(THEATRICAL_KIND_ID))
        .await
        .unwrap()
        .expect("Theatrical row found by deterministic id");
    assert_eq!(theatrical.normalized_tag, "Theatrical");
}

#[tokio::test]
async fn movie_crud_round_trips() {
    let db = fresh_store().await;
    let store = db.store.clone();
    let movie = sample_movie();
    store.upsert_movie(&movie).await.unwrap();

    let by_id = store.get_movie(movie.id).await.unwrap().unwrap();
    assert_eq!(by_id.title, "The Matrix");
    assert_eq!(by_id.year, Some(1999));
    assert_eq!(
        by_id.poster_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w500/poster.jpg"),
        "poster_url round-trips through the repo"
    );
    assert_eq!(
        by_id.backdrop_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w780/backdrop.jpg")
    );
    // Both collection columns, together (SKADI-T-0581). Asserting only the name
    // would pass with the id dropped, and the id is what the client groups on —
    // a collection that round-trips its label but loses its key silently stops
    // collapsing anything.
    assert_eq!(
        by_id.collection,
        Some(skadi_movies::MovieCollection {
            tmdb_id: 2344,
            name: "The Matrix Collection".into(),
        }),
        "collection id + name round-trip through the repo"
    );
    assert!(by_id.editions.is_empty());

    let by_tmdb = store.get_movie_by_tmdb(TmdbId(603)).await.unwrap().unwrap();
    assert_eq!(by_tmdb.id, movie.id);

    let listed = store
        .list_movies(MovieFilter {
            monitored: Some(true),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    store.delete_movie(movie.id).await.unwrap();
    assert!(store.get_movie(movie.id).await.unwrap().is_none());
}

#[tokio::test]
async fn edition_crud_and_status_writes() {
    let db = fresh_store().await;
    let store = db.store.clone();
    let movie = sample_movie();
    store.upsert_movie(&movie).await.unwrap();

    let edition = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
    store.upsert_edition(&edition).await.unwrap();

    let editions = store.list_editions(movie.id).await.unwrap();
    assert_eq!(editions.len(), 1);
    assert!(matches!(editions[0].status, AcquisitionStatus::Missing));

    // Status set: Searching first, then a full Imported payload.
    store
        .set_edition_status(
            edition.id,
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 1,
            },
        )
        .await
        .unwrap();
    let again = store.get_edition(edition.id).await.unwrap().unwrap();
    assert!(matches!(again.status, AcquisitionStatus::Searching { .. }));

    // Idempotent overwrite with the same status.
    store
        .set_edition_status(
            edition.id,
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 2,
            },
        )
        .await
        .unwrap();

    // Imported also persists file/quality/format_score.
    let qid = skadi_core::QualityId::new();
    store
        .set_edition_status(
            edition.id,
            AcquisitionStatus::Imported {
                file: skadi_core::FileRef {
                    path: "/movies/The Matrix (1999)/The Matrix (1999).mkv".into(),
                },
                quality: qid,
                score: 42,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let imported = store.get_edition(edition.id).await.unwrap().unwrap();
    assert!(matches!(
        imported.status,
        AcquisitionStatus::Imported { .. }
    ));
    assert!(imported.file.is_some());
    assert_eq!(imported.quality, Some(qid));
    assert_eq!(imported.format_score, 42);

    // get_edition_by_ref decodes the AcquirableRef.
    let by_ref = store
        .get_edition_by_ref(&edition.acquirable_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_ref.id, edition.id);
}

#[tokio::test]
async fn movie_get_returns_editions_populated() {
    let db = fresh_store().await;
    let store = db.store.clone();
    let movie = sample_movie();
    store.upsert_movie(&movie).await.unwrap();
    let e = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
    store.upsert_edition(&e).await.unwrap();

    let loaded = store.get_movie(movie.id).await.unwrap().unwrap();
    assert_eq!(loaded.editions.len(), 1);
    assert_eq!(loaded.editions[0].id, e.id);
}

#[tokio::test]
async fn deleting_a_builtin_edition_kind_is_refused() {
    let db = fresh_store().await;
    let store = db.store.clone();
    let err = store
        .delete_edition_kind(EditionKindId::from(THEATRICAL_KIND_ID))
        .await
        .unwrap_err();
    assert!(
        matches!(err, skadi_core::AppError::Validation(_)),
        "expected Validation, got {err:?}"
    );
    // Still there.
    assert!(
        store
            .get_edition_kind(EditionKindId::from(THEATRICAL_KIND_ID))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn user_added_edition_kind_round_trips_and_can_be_deleted() {
    use skadi_movies::EditionKind;
    let db = fresh_store().await;
    let store = db.store.clone();
    let kind = EditionKind {
        id: EditionKindId::new(),
        name: "Star Wars 4K Digital Film Scan".into(),
        normalized_tag: "SW 4K Scan".into(),
        match_patterns: vec!["4k digital film scan".into(), r"4k\.dfs".into()],
        builtin: false,
    };
    store.upsert_edition_kind(&kind).await.unwrap();
    let back = store.get_edition_kind(kind.id).await.unwrap().unwrap();
    assert_eq!(back, kind);

    store.delete_edition_kind(kind.id).await.unwrap();
    assert!(store.get_edition_kind(kind.id).await.unwrap().is_none());
}
