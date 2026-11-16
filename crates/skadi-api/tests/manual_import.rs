//! Manual import marks its target Imported (SKADI-T-0766).
//!
//! `POST /downloads/import` used to place the file and stop: the episode or
//! film stayed `Missing` with no file path, and the hunter looked for it again.
//! These tests drive the endpoint with hunter services registered for a kind and
//! read the status sink the acquire workflow writes to.
//!
//! A file of its own: the hunter's service registry is process-global, and the
//! tests in `library.rs` rely on no services being registered.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config, DomainDescriptor, LibraryItemDto, LibraryProvider};
use skadi_core::{AcquisitionStatus, FileRef, MediaKind, ProfileId, Result as SkadiResult};
use skadi_hunter::{
    HunterServices, ImporterFactory, InMemoryStatusSink, ScoringConfig, StatusSink, set_services,
};
use skadi_importer::{
    AcquirableRef, CompletedDownload, ImportOutcome, ImportPlan, ImportedFile, Importer,
};
use skadi_quality::{QualityProfile, UNKNOWN_QUALITY_ID, default_definitions};
use skadi_testsupport::TestDb;

/// A domain of one kind with nothing in it: the endpoint only asks its kind.
struct KindLib(&'static str, MediaKind);

#[async_trait]
impl LibraryProvider for KindLib {
    fn domain(&self) -> &str {
        self.0
    }
    fn kind(&self) -> MediaKind {
        self.1
    }
    async fn items(&self, _monitored: Option<bool>) -> SkadiResult<Vec<LibraryItemDto>> {
        Ok(Vec::new())
    }
}

/// What the fake importer does with the one file it is given.
#[derive(Clone, Copy)]
enum Places {
    /// Reports the file as placed at `dest`.
    Fresh,
    /// Reports `dest` as already holding this item's file.
    AlreadyThere,
}

/// "Imports" every completed download for one acquirable to a fixed path.
struct FixedImporter {
    acquirable: AcquirableRef,
    dest: PathBuf,
    places: Places,
}

#[async_trait]
impl Importer for FixedImporter {
    async fn import(&self, _completed: CompletedDownload) -> SkadiResult<ImportOutcome> {
        let file = ImportedFile {
            acquirable: self.acquirable.clone(),
            file: FileRef {
                path: self.dest.clone(),
            },
            replaced: false,
        };
        Ok(match self.places {
            Places::Fresh => ImportOutcome {
                imported: vec![file],
                ..ImportOutcome::default()
            },
            Places::AlreadyThere => ImportOutcome {
                already_present: vec![file],
                ..ImportOutcome::default()
            },
        })
    }
    async fn preview(&self, _completed: CompletedDownload) -> SkadiResult<ImportPlan> {
        Ok(ImportPlan::default())
    }
}

/// Hands out a [`FixedImporter`] for whichever acquirable the request names.
struct FixedFactory {
    dest: PathBuf,
    places: Places,
}

#[async_trait]
impl ImporterFactory for FixedFactory {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> SkadiResult<Arc<dyn Importer>> {
        Ok(Arc::new(FixedImporter {
            acquirable: acquirable.clone(),
            dest: self.dest.clone(),
            places: self.places,
        }))
    }
}

/// App state with one domain of `kind`, and hunter services for it whose
/// importer reports `dest`. Returns the status sink to read back.
async fn daemon(
    domain: &'static str,
    kind: MediaKind,
    dest: PathBuf,
    places: Places,
) -> (Arc<AppState>, Arc<InMemoryStatusSink>, TestDb) {
    let db = TestDb::new_store_only().await;
    let config = Config {
        database_url: db.url().to_string(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: None,
    };
    let state = AppState::new_full(
        config,
        Some(db.store.clone()),
        vec![DomainDescriptor {
            name: domain.into(),
            kind,
        }],
        vec![Arc::new(KindLib(domain, kind))],
    );
    let defs = default_definitions();
    let hi = defs
        .iter()
        .find(|q| q.name == "Bluray-1080p")
        .expect("Bluray-1080p")
        .id;
    let status = Arc::new(InMemoryStatusSink::new());
    let factory = Arc::new(FixedFactory { dest, places });
    set_services(Arc::new(HunterServices {
        kind,
        store: db.store.clone(),
        status: status.clone(),
        indexers: Vec::new(),
        downloaders: Vec::new(),
        importer: Arc::new(FixedImporter {
            acquirable: AcquirableRef("unused".into()),
            dest: PathBuf::new(),
            places,
        }),
        importer_factory: Some(factory),
        notifiers: Vec::new(),
        scoring: ScoringConfig {
            definitions: defs,
            profile: QualityProfile {
                id: ProfileId::new(),
                name: "t0766".into(),
                allowed: vec![hi],
                cutoff: hi,
                upgrade_allowed: false,
                formats: vec![],
                min_format_score: 0,
            },
            formats: Vec::new(),
            min_seeders: 0,
            audiobook: None,
        },
    }));
    (state, status, db)
}

async fn post(state: &Arc<AppState>, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let res = skadi_api::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/downloads/import")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// A release folder holding one (empty) video file, as the operator points at it.
fn release_dir(name: &str, file: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), b"not really video").unwrap();
    tmp
}

async fn imported(status: &InMemoryStatusSink, aref: &str) -> (PathBuf, skadi_core::QualityId) {
    match status
        .get_status(&AcquirableRef(aref.into()))
        .await
        .unwrap()
    {
        Some(AcquisitionStatus::Imported { file, quality, .. }) => (file.path, quality),
        other => panic!("{aref} should be Imported, got {other:?}"),
    }
}

#[tokio::test]
async fn a_manual_import_marks_the_film_imported_with_its_path_and_quality() {
    let dest = PathBuf::from("/library/movie/a-film_(2020)/theatrical/a-film_(2020).mkv");
    let (state, status, _db) =
        daemon("movies", MediaKind::Movie, dest.clone(), Places::Fresh).await;
    let src = release_dir(
        "A.Film.2020.1080p.BluRay.x264-GRP",
        "A.Film.2020.1080p.BluRay.x264-GRP.mkv",
    );
    let path = src.path().join("A.Film.2020.1080p.BluRay.x264-GRP");

    let (code, body) = post(
        &state,
        serde_json::json!({
            "path": path.to_string_lossy(),
            "kind": "movie",
            "acquirable_ref": "edition-0766",
        }),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["imported"][0], dest.to_string_lossy().as_ref());

    let (file, quality) = imported(&status, "edition-0766").await;
    assert_eq!(file, dest, "the status carries the placed file's path");
    let bluray = default_definitions()
        .into_iter()
        .find(|q| q.name == "Bluray-1080p")
        .unwrap()
        .id;
    assert_eq!(quality, bluray, "the quality comes from the release name");
}

#[tokio::test]
async fn a_manual_import_marks_the_episode_imported_from_a_single_file() {
    let dest = PathBuf::from("/library/television/a-show_(2024)/Season_01/a-show_-_S01E07.mkv");
    let (state, status, _db) = daemon("tv", MediaKind::Series, dest.clone(), Places::Fresh).await;
    let src = release_dir("downloads", "A.Show.S01E07.720p.WEB-DL.h264-GRP.mkv");
    // The operator points at the file itself, as for one episode of a pack.
    let path = src
        .path()
        .join("downloads")
        .join("A.Show.S01E07.720p.WEB-DL.h264-GRP.mkv");

    let (code, body) = post(
        &state,
        serde_json::json!({
            "path": path.to_string_lossy(),
            "kind": "series",
            "acquirable_ref": "episode-0766",
        }),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");

    let (file, quality) = imported(&status, "episode-0766").await;
    assert_eq!(file, dest);
    assert_ne!(
        quality, UNKNOWN_QUALITY_ID,
        "720p WEB-DL in the file name is a known quality"
    );
}

/// The file is already at the item's destination (an earlier manual import that
/// placed it and left the item Missing): the item becomes Imported.
#[tokio::test]
async fn a_manual_import_of_a_file_already_in_place_marks_the_item_imported() {
    let dest = PathBuf::from("/library/audiobook/an-author/a-book/a-book.m4b");
    let (state, status, _db) = daemon(
        "audiobooks",
        MediaKind::Audiobook,
        dest.clone(),
        Places::AlreadyThere,
    )
    .await;
    let src = release_dir("A Book - An Author", "a-book.m4b");
    let path = src.path().join("A Book - An Author");

    let (code, body) = post(
        &state,
        serde_json::json!({
            "path": path.to_string_lossy(),
            "kind": "audiobook",
            "acquirable_ref": "book-0766",
        }),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(imported(&status, "book-0766").await.0, dest);
}
