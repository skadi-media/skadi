//! C26 metadata sync steps (`add_movie` / `refresh_movie`) over a scripted provider.
use async_trait::async_trait;
use chrono::NaiveDate;
use cucumber::{given, then, when};

use skadi_core::{AcquisitionStatus, ImdbId, MediaKind, Result, RootFolder, TmdbId};
use skadi_metadata::{
    ExternalId, ImageKind, ImageRef, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord,
};
use skadi_movies::{MovieDefaults, MoviesRepo, add_movie, refresh_movie};

use crate::bdd_support::World;

/// A scripted provider whose `lookup` returns one canned record.
#[derive(Debug, Clone)]
pub struct FakeProvider {
    pub record: MetadataRecord,
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

fn provider(w: &mut World) -> &mut FakeProvider {
    w.provider.get_or_insert_with(|| FakeProvider {
        record: MetadataRecord::default(),
    })
}

#[given(expr = "the metadata provider returns {string} released {int} running {int} minutes")]
async fn provider_returns(w: &mut World, title: String, year: i32, runtime: u32) {
    let p = provider(w);
    p.record.title = title.clone();
    p.record.original_title = Some(title);
    p.record.overview = Some("A film.".into());
    p.record.runtime_minutes = Some(runtime);
    p.record.release_date = NaiveDate::from_ymd_opt(year, 3, 31);
}

#[given(expr = "the provider record carries imdb {string}")]
async fn provider_imdb(w: &mut World, imdb: String) {
    provider(w).record.external_ids.imdb = Some(ImdbId(imdb));
}

#[given(expr = "the provider record carries a poster {string} and backdrop {string}")]
async fn provider_images(w: &mut World, poster: String, backdrop: String) {
    provider(w).record.images = vec![
        ImageRef {
            kind: ImageKind::Poster,
            path: poster,
        },
        ImageRef {
            kind: ImageKind::Backdrop,
            path: backdrop,
        },
    ];
}

#[given("the provider record has no release date")]
async fn provider_no_date(w: &mut World) {
    provider(w).record.release_date = None;
}

#[when(expr = "{string} is added by TMDB id {int}")]
async fn add(w: &mut World, title: String, tmdb: u64) {
    let p = provider(w).clone();
    let store = w.store();
    match add_movie(
        &store,
        &p,
        TmdbId(tmdb),
        skadi_core::ProfileId::new(),
        RootFolder::new("/movies"),
    )
    .await
    {
        Ok(m) => {
            w.editions.insert(title.clone(), m.editions[0].clone());
            w.movies.insert(title, m.clone());
            w.last_movie = Some(m);
            w.error = None;
        }
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[then(expr = "the library holds {string} from {int} with one Missing Theatrical edition")]
async fn holds(w: &mut World, title: String, year: u16) {
    let m = w.last_movie.as_ref().expect("added movie");
    assert_eq!(m.title, title);
    assert_eq!(m.year, Some(year));
    assert!(m.monitored, "a freshly added movie is monitored");
    assert!(m.last_metadata_refresh.is_some());
    let stored = w.store().get_movie(m.id).await.unwrap().expect("persisted");
    assert_eq!(stored.editions.len(), 1);
    assert_eq!(
        stored.editions[0].kind.into_uuid(),
        skadi_movies::THEATRICAL_KIND_ID
    );
    assert!(matches!(
        stored.editions[0].status,
        AcquisitionStatus::Missing
    ));
}

#[then(expr = "the add is rejected as a duplicate naming {string}")]
async fn duplicate(w: &mut World, title: String) {
    let e = w.error.as_deref().expect("an error");
    assert!(e.contains("already exists") && e.contains(&title), "{e}");
}

#[then(expr = "only one movie row exists for {string}")]
async fn single_row(w: &mut World, title: String) {
    let n = w
        .store()
        .list_movies(skadi_movies::MovieFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.title == title)
        .count();
    assert_eq!(n, 1);
}

#[given(expr = "the movie {string} was unmonitored by the user with imdb {string}")]
async fn user_edits(w: &mut World, title: String, imdb: String) {
    let mut m = w.movies.get(&title).expect("movie").clone();
    m.monitored = false;
    m.external_ids.imdb = Some(ImdbId(imdb));
    w.store().upsert_movie(&m).await.expect("upsert");
    w.movies.insert(title, m);
}

#[when(expr = "the movie {string} is refreshed from the provider")]
async fn refresh(w: &mut World, title: String) {
    let p = provider(w).clone();
    let existing = w.movies.get(&title).expect("movie").clone();
    let tmdb = existing.external_ids.tmdb.clone().unwrap();
    match refresh_movie(&p, tmdb, Some(existing), None).await {
        Ok(m) => {
            w.store().upsert_movie(&m).await.expect("persist refresh");
            w.last_movie = Some(m);
            w.error = None;
        }
        Err(e) => w.error = Some(e.to_string()),
    }
}

#[when("a fresh movie is refreshed without provider defaults")]
async fn refresh_no_defaults(w: &mut World) {
    let p = provider(w).clone();
    w.error = refresh_movie(&p, TmdbId(1), None, None)
        .await
        .err()
        .map(|e| e.to_string());
}

#[when("a fresh movie is refreshed with provider defaults")]
async fn refresh_defaults(w: &mut World) {
    let p = provider(w).clone();
    let m = refresh_movie(
        &p,
        TmdbId(1),
        None,
        Some(MovieDefaults {
            profile: skadi_core::ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
        }),
    )
    .await
    .expect("refresh with defaults");
    w.last_movie = Some(m);
}

#[then("the refresh is rejected as a validation error")]
async fn refresh_rejected(w: &mut World) {
    let e = w.error.as_deref().expect("an error");
    assert!(e.contains("provider_defaults required"), "{e}");
}

#[then(expr = "the refreshed movie is titled {string} from {int}")]
async fn refreshed_title(w: &mut World, title: String, year: u16) {
    let m = w.last_movie.as_ref().expect("movie");
    assert_eq!(m.title, title);
    assert_eq!(m.year, Some(year));
}

#[then(
    expr = "the refreshed movie keeps the user's monitored flag, profile, root folder and imdb {string}"
)]
async fn preserved(w: &mut World, imdb: String) {
    let m = w.last_movie.as_ref().expect("movie");
    let before = w.movies.values().find(|b| b.id == m.id).expect("before");
    assert!(!m.monitored);
    assert_eq!(m.id, before.id);
    assert_eq!(m.profile, before.profile);
    assert_eq!(m.root_folder, before.root_folder);
    assert_eq!(m.added_at, before.added_at);
    assert_eq!(
        m.external_ids.imdb.as_ref().map(|i| i.0.as_str()),
        Some(imdb.as_str())
    );
    let stored = w.store().get_movie(m.id).await.unwrap().unwrap();
    assert_eq!(stored.editions.len(), 1, "editions survive a refresh");
}

#[then("the refreshed movie has no year")]
async fn no_year(w: &mut World) {
    assert_eq!(w.last_movie.as_ref().unwrap().year, None);
}

#[then(expr = "the refreshed movie carries poster {string} and backdrop {string}")]
async fn images(w: &mut World, poster: String, backdrop: String) {
    let m = w.last_movie.as_ref().unwrap();
    assert_eq!(m.poster_url.as_deref(), Some(poster.as_str()));
    assert_eq!(m.backdrop_url.as_deref(), Some(backdrop.as_str()));
}

#[then("the refreshed movie is stamped with a metadata refresh time")]
async fn stamped(w: &mut World) {
    assert!(
        w.last_movie
            .as_ref()
            .unwrap()
            .last_metadata_refresh
            .is_some()
    );
}

/// Radarr exposes `RefreshMovie` per movie and in bulk (SKADI-T-0450).
///
/// Asserted against the real router: an unknown id must come back as our
/// `not_found` envelope, which distinguishes "the route exists and rejected this
/// id" from "there is no such route" — a missing route would fall through to the
/// API's own 404 instead.
#[then("the movies API exposes a per-movie metadata refresh route")]
async fn refresh_route(w: &mut World) {
    use axum::body::Body;
    use axum::http::Request;
    use skadi_api::HttpModule;
    use tower::ServiceExt;

    let fake = provider(w).clone();
    let url = w.db.as_ref().expect("library").0.url();
    let store = w.store();
    let runner = std::sync::Arc::new(skadi_hunter::build_runner(url).await.expect("runner"));
    let provider: std::sync::Arc<dyn MetadataProvider> = std::sync::Arc::new(fake);
    let router = skadi_movies::MoviesHttp::new(store, provider, runner).routes();

    let unknown = uuid::Uuid::new_v4();
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/movies/{unknown}/refresh"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("route responds");
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::NOT_FOUND,
        "the route exists and rejects an unknown movie id"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json envelope");
    assert_eq!(
        json["error"], "not_found",
        "a real handler answered, not the router fallback: {json}"
    );
}
