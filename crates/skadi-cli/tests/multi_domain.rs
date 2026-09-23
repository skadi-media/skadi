//! Two-domain coexistence (SKADI-T-0136).
//!
//! Proves the per-kind `HunterServices` registry + shared Cloacina runner let
//! movies and audiobooks live in one process: both modules are built against a
//! single shared runner, each registers its services under its own `MediaKind`,
//! and the registry resolves each domain independently (neither clobbers the
//! other the way the old single-global `HunterServices` did).

use std::sync::Arc;

use skadi_api::{ProviderReloader, ProviderSet};
use skadi_audiobooks::{AudiobooksModule, SharedHunterDeps as AbDeps, default_audiobook_profile};
use skadi_core::MediaKind;
use skadi_hunter::services::{AudiobookScoring, ScoringConfig, reset_services};
use skadi_hunter::{services_for, try_services_for};
use skadi_movies::{MoviesModule, SharedHunterDeps as MvDeps};
use skadi_quality::audiobook::default_audiobook_definitions;
use skadi_quality::{default_definitions, standard_profile};
use skadi_store::Store;

fn temp_url(tag: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        dir.path().join(format!("{tag}.db")).display()
    );
    (dir, url)
}

#[tokio::test]
async fn movies_and_audiobooks_coexist_on_one_shared_runner() {
    reset_services();

    // One runner, derived once from the skadi URL (sibling hunter.db).
    let (_dir, url) = temp_url("skadi");
    let shared_runner = Arc::new(skadi_hunter::build_runner(&url).await.unwrap());

    // --- movies module against the shared runner ---
    let mv_store = Store::connect(&url).unwrap();
    mv_store.run_migrations().await.unwrap();
    let defs = default_definitions();
    let mv_profile = standard_profile(&defs);
    let movies = MoviesModule::with_runner(
        mv_store,
        shared_runner.clone(),
        MvDeps {
            indexers: vec![],
            downloaders: vec![],
            notifiers: vec![],
            scoring: ScoringConfig {
                definitions: defs,
                profile: mv_profile,
                formats: vec![],
                min_seeders: 0,
                audiobook: None,
            },
        },
    )
    .await
    .unwrap();

    // --- audiobooks module against the SAME shared runner ---
    let ab_store = Store::connect(&url).unwrap();
    let audiobooks = AudiobooksModule::with_runner(
        ab_store,
        shared_runner.clone(),
        AbDeps {
            indexers: vec![],
            downloaders: vec![],
            notifiers: vec![],
            scoring: ScoringConfig {
                definitions: vec![],
                profile: default_audiobook_profile(),
                formats: vec![],
                min_seeders: 0,
                audiobook: Some(AudiobookScoring {
                    definitions: default_audiobook_definitions(),
                    allow_abridged: false,
                }),
            },
        },
    )
    .await
    .unwrap();

    // Both modules share the one runner Arc.
    assert!(Arc::ptr_eq(&movies.runner(), &shared_runner));
    assert!(Arc::ptr_eq(&audiobooks.runner(), &shared_runner));

    // The supervisor's provider reconcile (`apply`) is what publishes each
    // domain's services into the registry — keyed by kind.
    let empty = || ProviderSet {
        indexers: vec![],
        downloaders: vec![],
        notifiers: vec![],
    };
    movies.apply(empty()).await.unwrap();
    audiobooks.apply(empty()).await.unwrap();

    let mv_svc = services_for(MediaKind::Movie);
    let ab_svc = services_for(MediaKind::Audiobook);
    assert_eq!(mv_svc.kind, MediaKind::Movie);
    assert_eq!(ab_svc.kind, MediaKind::Audiobook);
    // The two domains are distinct registry entries — neither clobbered the other.
    assert!(
        mv_svc.scoring.audiobook.is_none(),
        "movies is not an audiobook domain"
    );
    assert!(
        ab_svc.scoring.audiobook.is_some(),
        "audiobooks carries the audiobook axis"
    );

    // Sanity: a never-registered kind stays unset.
    assert!(try_services_for(MediaKind::Book).is_none());

    shared_runner.shutdown().await.unwrap();
    reset_services();
}
