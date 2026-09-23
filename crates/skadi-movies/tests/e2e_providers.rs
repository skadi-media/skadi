//! Capstone e2e for provider wiring (SKADI-T-0062).
//!
//! Unlike the T-0057 e2e (mock hunter services injected via `set_services`),
//! this test configures **everything through the API**: a real `Torznab`
//! indexer and the real built-in downloader are built by the provider factory
//! from settings rows + sealed credentials, published by the supervisor's
//! provider reconcile, and the acquire pipeline drives them. No provider mocks —
//! the search stage hits a real Torznab endpoint over HTTP, and the snatch stage
//! enqueues a real row in the `downloads` table.
//!
//! The transfer itself is normally performed by the out-of-process
//! `skadi-downloader-worker`. Here the test plays that role directly against the
//! queue — claim the job, then mark it complete with the staged file — which is
//! exactly the daemon↔worker contract the worker implements (SKADI-T-0517
//! replaced the previous qBittorrent WebUI fake).
//!
//! Also proves **live reload**: the daemon starts with zero providers; the
//! indexer + downloader are POSTed while it runs, and the supervisor tick picks
//! them up without a restart.
//!
//! Single test binary (process-global services + tracker), run single-threaded.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wiremock::matchers::{method, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use skadi_api::{AppState, Config, DomainDescriptor, HttpModule, ProviderReloader, Supervisor};
use skadi_client::Client;
use skadi_core::{DomainModule, MediaKind, Result as SkadiResult};
use skadi_hunter::services::ScoringConfig;
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_quality::default_definitions;
use skadi_store::{ConfigRepo, ConfigSource, DownloadJobRepo, Store};

use skadi_movies::{MoviesHttp, MoviesLibrary, MoviesModule, SharedHunterDeps};

/// TMDB stub: canned Matrix record (metadata is out of scope here).
struct FakeTmdb;

#[async_trait::async_trait]
impl MetadataProvider for FakeTmdb {
    fn name(&self) -> &str {
        "fake-tmdb"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn search(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, _id: &ExternalId) -> SkadiResult<MetadataRecord> {
        Ok(MetadataRecord {
            external_ids: skadi_core::ExternalIds {
                tmdb: Some(skadi_core::TmdbId(603)),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: Some("The Matrix".into()),
            overview: None,
            runtime_minutes: Some(136),
            release_date: chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
            images: vec![],
            ..Default::default()
        })
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

const CAPS_XML: &str = r#"<?xml version="1.0"?>
<caps>
  <searching>
    <search available="yes" supportedParams="q" />
    <movie-search available="yes" supportedParams="q,tmdbid" />
  </searching>
  <categories>
    <category id="2000" name="Movies" />
  </categories>
</caps>"#;

/// The one release our fake Torznab returns, and the btih infohash the snatch
/// stage carries onto the queued download row.
const INFOHASH: &str = "00000000000000000000000000000000deadbeef";

fn search_xml() -> String {
    format!(
        r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>The.Matrix.1999.1080p.BluRay.x264-GRP</title>
      <link>magnet:?xt=urn:btih:{INFOHASH}</link>
      <pubDate>Tue, 10 Jan 2023 12:00:00 +0000</pubDate>
      <size>8000000000</size>
      <torznab:attr name="seeders" value="42" />
    </item>
  </channel>
</rss>"#
    )
}

#[tokio::test]
async fn providers_from_settings_drive_a_real_acquire_with_live_reload() {
    let dir = tempfile::tempdir().unwrap();
    // The "downloaded" file the worker reports: padded past the matcher's
    // 50 MB sample threshold.
    let save_path = dir.path().join("downloads");
    std::fs::create_dir_all(&save_path).unwrap();
    let src_name = "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv";
    std::fs::write(save_path.join(src_name), vec![0u8; 52 * 1024 * 1024]).unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let skadi_url = format!("sqlite://{}", dir.path().join("skadi.db").display());

    // --- fake provider servers ---

    // Torznab: caps + a one-release search.
    let torznab = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("t", "caps"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CAPS_XML))
        .mount(&torznab)
        .await;
    for t in ["movie", "search"] {
        Mock::given(method("GET"))
            .and(query_param("t", t))
            .respond_with(ResponseTemplate::new(200).set_body_string(search_xml()))
            .mount(&torznab)
            .await;
    }

    // --- daemon assembly (mirrors `skadi run`) ---

    let store = Store::connect(&skadi_url).unwrap();
    let defs = default_definitions();
    let hi = defs.iter().find(|q| q.name == "Bluray-1080p").unwrap().id;
    let lo = defs.iter().find(|q| q.name == "Bluray-720p").unwrap().id;
    let scoring_profile = skadi_quality::QualityProfile {
        id: skadi_core::ProfileId::new(),
        name: "e2e".into(),
        allowed: vec![lo, hi],
        cutoff: hi,
        upgrade_allowed: false,
        formats: vec![],
        min_format_score: 0,
    };

    // Providers start EMPTY — the supervisor reconcile will populate them from
    // settings (that's the point of this test).
    let module = Arc::new(
        MoviesModule::new(
            store.clone(),
            &skadi_url,
            SharedHunterDeps {
                indexers: vec![],
                downloaders: vec![],
                notifiers: vec![],
                scoring: ScoringConfig {
                    definitions: defs,
                    profile: scoring_profile,
                    formats: vec![],
                    min_seeders: 0,
                    audiobook: None,
                },
            },
        )
        .await
        .unwrap(),
    );
    let registry: Vec<Arc<dyn DomainModule>> = vec![module.clone()];
    let config = Config {
        database_url: skadi_url.clone(),
        bind_addr: format!("127.0.0.1:{}", free_port()).parse().unwrap(),
        bearer_token: None,
    };
    skadi_api::bootstrap(&config, &registry).await.unwrap();

    let provider: Arc<dyn MetadataProvider> = Arc::new(FakeTmdb);
    let movies_http = MoviesHttp::new(module.store(), provider, module.runner());
    let movies_library: Arc<dyn skadi_api::LibraryProvider> =
        Arc::new(MoviesLibrary::new(module.store()));

    let state = AppState::new_full(
        config.clone(),
        Some(store.clone()),
        vec![DomainDescriptor {
            name: "movies".into(),
            kind: MediaKind::Movie,
        }],
        vec![movies_library],
    );
    let cancel = state.cancel.clone();
    let routes = HttpModule::routes(&movies_http);
    let server = tokio::spawn(skadi_api::serve(state, vec![routes]));

    // Supervisor with the module as provider reloader, fast tick for the test.
    let reloaders: Vec<Arc<dyn ProviderReloader>> = vec![module.clone()];
    let supervisor = Arc::new(Supervisor::with_reloaders(
        store.clone(),
        registry,
        reloaders,
    ));
    let sup_cancel = cancel.clone();
    let sup_handle = tokio::spawn(
        supervisor
            .clone()
            .run(sup_cancel, Duration::from_millis(100)),
    );

    let base = format!("http://{}", config.bind_addr);
    let client = Client::new(base, None);

    // Wait for /health.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if client.health().await.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "server never came up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // --- configure THROUGH THE API, daemon already running (live reload) ---

    // Indexer: real torznab config + sealed api key.
    let ix = client
        .create_setting(
            "indexers",
            &serde_json::json!({
                "kind": "torznab",
                "name": "fake-torznab",
                "base_url": torznab.uri(),
                "categories": [2000],
                "api_key": "test-key"
            }),
        )
        .await
        .expect("create indexer");
    // Test-connection proves the stored config + credential reach the fake.
    let test = client
        .test_setting("indexers", ix["id"].as_str().unwrap())
        .await
        .expect("test indexer");
    assert_eq!(test["ok"], true, "indexer test-connection: {test}");

    // Downloader: the built-in DB-queue client. No endpoint and no secret — the
    // transfer happens in the worker, which this test stands in for below.
    client
        .create_setting(
            "downloaders",
            &serde_json::json!({
                "kind": "skadi",
                "name": "built-in",
                "incomplete_dir": save_path.display().to_string(),
                "complete_dir": save_path.display().to_string()
            }),
        )
        .await
        .expect("create downloader");

    // Profile + root folder + enable the domain.
    let profile = client
        .create_setting("profiles", &serde_json::json!({ "name": "e2e" }))
        .await
        .unwrap();
    let profile_id = profile["id"].as_str().unwrap().to_string();
    // Single library root (SKADI-T-0302): movies land under `<library.root>/movie`.
    store
        .set_config(
            "library.root",
            &library.display().to_string(),
            ConfigSource::Runtime,
        )
        .await
        .unwrap();
    client.set_domain_enabled("movies", true).await.unwrap();

    // Give the supervisor a beat to reconcile providers + spawn the worker.
    // (Workers publish their own services at start; provider reconcile then
    // re-publishes with the live set — poll until the acquire below succeeds.)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Add the movie + trigger the acquire.
    let movie = client
        .add_movie(
            603,
            Some(&profile_id),
            Some(&library.display().to_string()),
            false,
        )
        .await
        .expect("add movie");
    let movie_id = movie["id"].as_str().unwrap().to_string();
    let edition_id = movie["editions"][0]["id"].as_str().unwrap().to_string();
    client
        .acquire_edition(&movie_id, &edition_id)
        .await
        .expect("acquire");

    // Stand in for the worker: the snatch stage enqueues a `downloads` row, so
    // claim it and report the staged file complete, exactly as the worker would.
    //
    // The acquire is re-issued while we wait. The providers are published by a
    // supervisor tick, so an acquire that lands between the POST and the tick
    // legitimately finds no indexer and parks the edition on a retry backoff far
    // longer than this test can wait. Re-asking is what an operator would do, and
    // it makes the test depend on the observable outcome rather than on internal
    // reconcile timing.
    // Generous: this test boots a real daemon, a supervisor and a Cloacina
    // runner, and under a full `cargo test --workspace` the machine is saturated
    // by every other crate's suite. A 30 s budget passed in isolation and timed
    // out in the workspace run.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut claimed = false;
    let mut last_retry = Instant::now();
    while Instant::now() < deadline {
        if last_retry.elapsed() > Duration::from_secs(2) {
            let _ = client.acquire_edition(&movie_id, &edition_id).await;
            last_retry = Instant::now();
        }
        if let Some(job) = store.claim_next("e2e-worker").await.unwrap() {
            store
                .mark_complete(&job.id, &[save_path.join(src_name).display().to_string()])
                .await
                .unwrap();
            claimed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(claimed, "snatch never enqueued a download job");

    // On its own clock: the claim loop above may have spent most of the first
    // deadline waiting for the snatch to enqueue.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut imported = false;
    while Instant::now() < deadline {
        let got = client.get_movie(&movie_id).await.unwrap();
        if got["editions"][0]["status"].get("Imported").is_some() {
            imported = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(imported, "edition never reached Imported");

    // The file landed in the library at the sanitized destination.
    assert!(
        library
            .join("movie/the-matrix_(1999)_{tmdb-603}/theatrical/the-matrix_(1999).mkv")
            .exists(),
        "imported file present in the library"
    );

    // The fake Torznab actually served the search (not a mock indexer).
    let torznab_hits = torznab.received_requests().await.unwrap();
    assert!(
        torznab_hits
            .iter()
            .any(|r| { r.url.query().is_some_and(|q| !q.contains("t=caps")) }),
        "fake torznab never received a search request"
    );
    // Clean shutdown.
    cancel.cancel();
    let _ = server.await;
    let _ = sup_handle.await;
    module.runner().shutdown().await.unwrap();
}
