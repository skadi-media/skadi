//! Mock providers for the E2E harness — a deterministic, offline acquire path.
//! Lifted from `crates/skadi-movies/tests/e2e_http.rs` so the Playwright E2E
//! drives the same pipeline a unit e2e does, but through a real browser.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use skadi_audiobooks::{AudiobookMatcher, AudiobooksRepo};
use skadi_core::{
    DownloaderId, ExternalIds, ImdbId, IndexerId, MediaKind, NotifierId, Protocol,
    Result as SkadiResult, TmdbId,
};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_importer::{
    AcquirableMatch, AcquirableMatcher, AcquirableRef, CompletedDownload, DefaultImporter, Importer,
};
use skadi_indexers::{Category, IndexerCaps, Release, SearchQuery, TextSearch};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_movies::{MovieMatcher, MoviesRepo};
use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
use skadi_quality::ParsedRelease;

/// An indexer that returns one fixed release for any query, for one media kind.
pub struct OneShotIndexer {
    pub id: IndexerId,
    pub kind: MediaKind,
    pub release: Release,
}

#[async_trait]
impl skadi_indexers::Indexer for OneShotIndexer {
    fn id(&self) -> IndexerId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == self.kind
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn capabilities(&self) -> SkadiResult<IndexerCaps> {
        Ok(IndexerCaps {
            supports_rss: true,
            supports_search: true,
            id_params: std::collections::BTreeSet::new(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: vec![],
        })
    }
    async fn search(&self, _query: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
        Ok(vec![self.release.clone()])
    }
}

/// A downloader that reports the transfer already complete with fixed files.
pub struct ImmediateDownloader {
    pub id: DownloaderId,
    pub completed: Vec<PathBuf>,
}

#[async_trait]
impl Downloader for ImmediateDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn add(&self, _r: &Release, c: &Category) -> SkadiResult<DownloadHandle> {
        Ok(DownloadHandle {
            native_id: "test-handle".into(),
            category: format!("{}", c.0),
        })
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        Ok(DownloadStatus::Completed {
            files: self.completed.clone(),
        })
    }
    async fn remove(&self, _: &DownloadHandle, _: bool) -> SkadiResult<()> {
        Ok(())
    }
}

/// A notifier that swallows events (the E2E asserts via the UI, not notifications).
pub struct NoopNotifier {
    pub id: NotifierId,
}

#[async_trait]
impl Notifier for NoopNotifier {
    fn id(&self) -> NotifierId {
        self.id
    }
    fn channels(&self) -> &[NotificationKind] {
        &[NotificationKind::Imported]
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
    async fn notify(&self, _event: &NotificationEvent) -> SkadiResult<()> {
        Ok(())
    }
}

/// Builds a per-acquirable [`MovieMatcher`]-backed importer (the movies domain's
/// real matching path), so a grabbed release imports into the library.
pub struct MovieImporterFactoryForTest {
    pub repo: Arc<dyn MoviesRepo>,
}

#[async_trait]
impl skadi_hunter::ImporterFactory for MovieImporterFactoryForTest {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> SkadiResult<Arc<dyn Importer>> {
        let edition_id = uuid::Uuid::parse_str(&acquirable.0)
            .map(skadi_core::MovieEditionId::from)
            .map_err(|e| skadi_core::AppError::Validation(format!("bad ref: {e}")))?;
        let edition = self.repo.get_edition(edition_id).await?.unwrap();
        let movie = self.repo.get_movie(edition.movie_id).await?.unwrap();
        let kinds = self.repo.list_edition_kinds().await?;
        let matcher = MovieMatcher::new(movie.clone(), movie.editions.clone(), kinds);
        Ok(Arc::new(DefaultImporter::new(matcher)))
    }
}

/// Fallback matcher (unused once the factory above is installed).
pub struct NoopMatcher;
impl AcquirableMatcher for NoopMatcher {
    fn match_file(
        &self,
        _p: &ParsedRelease,
        _s: &std::path::Path,
        _c: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        Vec::new()
    }
}

/// A TMDB provider stub returning a canned "The Matrix" record for any lookup.
pub struct FakeTmdb;

#[async_trait]
impl MetadataProvider for FakeTmdb {
    fn name(&self) -> &str {
        "fake-tmdb"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    /// Any search finds The Matrix, so the Add page can be driven end to end
    /// (the a11y keyboard pass, SKADI-T-0700).
    async fn search(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![MetadataMatch {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                imdb: Some(ImdbId("tt0133093".into())),
                ..Default::default()
            },
            title: "The Matrix".into(),
            year: Some(1999),
            score: 1.0,
            poster_url: None,
            overview: Some("A hacker learns the truth about his reality.".into()),
        }])
    }
    async fn lookup(&self, _id: &ExternalId) -> SkadiResult<MetadataRecord> {
        Ok(MetadataRecord {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                imdb: Some(ImdbId("tt0133093".into())),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: Some("The Matrix".into()),
            overview: Some("A hacker learns the truth about his reality.".into()),
            runtime_minutes: Some(136),
            release_date: chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![],
            ..Default::default()
        })
    }
}

pub fn default_importer_noop() -> Arc<dyn Importer> {
    Arc::new(DefaultImporter::new(NoopMatcher))
}

/// Builds a per-acquirable [`AudiobookMatcher`]-backed importer (the audiobooks
/// domain's real matching path), mirroring the production `AudiobookImporterFactory`
/// in `skadi-audiobooks/src/module.rs`. So a grabbed audiobook release imports.
pub struct AudiobookImporterFactoryForTest {
    pub repo: Arc<dyn AudiobooksRepo>,
}

#[async_trait]
impl skadi_hunter::ImporterFactory for AudiobookImporterFactoryForTest {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> SkadiResult<Arc<dyn Importer>> {
        let file_id = uuid::Uuid::parse_str(&acquirable.0)
            .map(skadi_core::BookFileId::from)
            .map_err(|e| skadi_core::AppError::Validation(format!("bad ref: {e}")))?;
        let file = self.repo.get_book_file(file_id).await?.unwrap();
        let book = self.repo.get_book(file.book_id).await?.unwrap();
        let matcher = AudiobookMatcher::new(book, file);
        Ok(Arc::new(DefaultImporter::new(matcher)))
    }
}

/// A tiny in-process HTTP mock of the Audnexus API: any `GET /books/{asin}`
/// returns a canned "Project Hail Mary" record (camelCase wire shape). The
/// audiobooks add-by-ASIN flow (`add_book` → `refresh_book` → `provider.lookup`)
/// needs `AudiobooksHttp` to hold a **concrete** `Arc<AudnexusProvider>`, so the
/// harness points a real provider at this stub via `with_base_url` — entirely
/// offline. Returns the bound base URL the provider is constructed against.
pub async fn spawn_fake_audnexus() -> String {
    use axum::extract::Path as AxPath;
    use axum::routing::get;
    use axum::{Json, Router};

    async fn book(AxPath(asin): AxPath<String>) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "asin": asin,
            "title": "Project Hail Mary",
            "subtitle": "A Novel",
            "authors": [{ "asin": "A1", "name": "Andy Weir" }],
            "narrators": [{ "name": "Ray Porter" }],
            "runtimeLengthMin": 970,
            "image": "https://example.invalid/phm.jpg",
            "releaseDate": "2021-05-04T00:00:00.000Z",
            "formatType": "unabridged",
            "summary": "Ryland Grace wakes up alone, and the fate of humanity rests on him.",
            "language": "english"
        }))
    }
    async fn author(AxPath(asin): AxPath<String>) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "asin": asin,
            "name": "Andy Weir",
            "description": "Author of The Martian.",
            "image": "https://example.invalid/aw.jpg"
        }))
    }

    let app = Router::new()
        .route("/books/{asin}", get(book))
        .route("/authors/{asin}", get(author));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake audnexus");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake audnexus");
    });
    format!("http://{addr}")
}
