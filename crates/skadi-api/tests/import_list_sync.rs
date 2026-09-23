//! The import-list sync engine (SKADI-T-0511): exclusions, dedup, and what
//! happens when one item of many fails.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{Duration, Utc};

use skadi_api::import_lists::{sync_due_lists, sync_list};
use skadi_api::library::{ImportListAddOptions, LibraryProvider};
use skadi_core::MediaKind;
use skadi_metadata::import_list::{ImportListProvider, ListItem};
use skadi_store::{ImportList, ImportListExclusion, ImportListRepo, Store};
use skadi_testsupport::TestDb;

/// A provider that offers a fixed set.
struct StubProvider(Vec<ListItem>);

#[async_trait]
impl ImportListProvider for StubProvider {
    fn kind(&self) -> &'static str {
        "stub"
    }
    async fn fetch(&self, _s: &serde_json::Value) -> skadi_core::Result<Vec<ListItem>> {
        Ok(self.0.clone())
    }
}

/// Records what it was asked to add. `existing` reports already-present;
/// `broken` fails.
#[derive(Default)]
struct StubDomain {
    added: Mutex<Vec<(String, bool)>>,
    existing: Vec<String>,
    broken: Vec<String>,
}

#[async_trait]
impl LibraryProvider for StubDomain {
    fn domain(&self) -> &str {
        "movies"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    async fn items(
        &self,
        _m: Option<bool>,
    ) -> skadi_core::Result<Vec<skadi_api::library::LibraryItemDto>> {
        Ok(Vec::new())
    }
    async fn add_from_list(
        &self,
        _id_kind: &str,
        external_id: &str,
        opts: &ImportListAddOptions,
    ) -> skadi_core::Result<bool> {
        if self.broken.iter().any(|b| b == external_id) {
            return Err(skadi_core::AppError::Validation("nope".into()));
        }
        if self.existing.iter().any(|e| e == external_id) {
            return Ok(false);
        }
        self.added
            .lock()
            .unwrap()
            .push((external_id.to_string(), opts.monitored));
        Ok(true)
    }
}

fn item(id: &str) -> ListItem {
    ListItem {
        id_kind: "tmdb".into(),
        external_id: id.into(),
        title: Some(format!("Film {id}")),
        year: Some(2000),
    }
}

async fn db() -> (TestDb, Store) {
    let db = TestDb::new_store_only().await;
    let store = db.store.clone();
    (db, store)
}

fn a_list() -> ImportList {
    let mut l = ImportList::new("Bond", "stub", "movies");
    l.settings = serde_json::json!({});
    l
}

#[tokio::test]
async fn excluded_items_are_never_offered_to_the_domain() {
    let (_db, store) = db().await;
    let list = a_list();
    store.upsert_import_list(&list).await.unwrap();

    // The operator deleted "2" and excluded it. Without the exclusion the list
    // re-adds it on every sync — an infinite argument between operator and daemon.
    store
        .add_exclusion(&ImportListExclusion {
            id: uuid::Uuid::new_v4().to_string(),
            target_domain: "movies".into(),
            id_kind: "tmdb".into(),
            external_id: "2".into(),
            title: Some("Film 2".into()),
            created_at: Utc::now(),
        })
        .await
        .unwrap();

    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubProvider(vec![
        item("1"),
        item("2"),
        item("3"),
    ]))];
    let domain = Arc::new(StubDomain::default());
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![domain.clone()];

    let report = sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.offered, 3);
    assert_eq!(report.added, 2);
    assert_eq!(report.excluded, 1);
    let added: Vec<String> = domain
        .added
        .lock()
        .unwrap()
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(
        added,
        vec!["1", "3"],
        "the excluded id never reached the add"
    );
}

#[tokio::test]
async fn already_present_items_are_counted_not_failed() {
    let (_db, store) = db().await;
    let list = a_list();
    let providers: Vec<Arc<dyn ImportListProvider>> =
        vec![Arc::new(StubProvider(vec![item("1"), item("2")]))];
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(StubDomain {
        existing: vec!["1".into()],
        ..StubDomain::default()
    })];

    // The ordinary case on every sync after the first. If this counted as a
    // failure the list would look permanently broken.
    let report = sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 1);
    assert_eq!(report.existing, 1);
    assert!(report.failed.is_empty());
}

#[tokio::test]
async fn one_bad_item_does_not_abandon_the_rest() {
    let (_db, store) = db().await;
    let list = a_list();
    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubProvider(vec![
        item("1"),
        item("bad"),
        item("3"),
    ]))];
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(StubDomain {
        broken: vec!["bad".into()],
        ..StubDomain::default()
    })];

    let report = sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert_eq!(report.added, 2, "the other two still went in");
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, "bad");
}

#[tokio::test]
async fn add_monitored_reaches_the_domain_and_defaults_off() {
    let (_db, store) = db().await;
    let mut list = a_list();
    assert!(
        !list.add_monitored,
        "a new list must not acquire by default"
    );

    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubProvider(vec![item("1")]))];
    let domain = Arc::new(StubDomain::default());
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![domain.clone()];
    sync_list(&list, &providers, &domains, &store)
        .await
        .unwrap();
    assert!(
        !domain.added.lock().unwrap()[0].1,
        "a new list must not monitor what it adds"
    );

    let domain2 = Arc::new(StubDomain::default());
    let domains2: Vec<Arc<dyn LibraryProvider>> = vec![domain2.clone()];
    list.add_monitored = true;
    sync_list(&list, &providers, &domains2, &store)
        .await
        .unwrap();
    assert!(
        domain2.added.lock().unwrap()[0].1,
        "opting in reaches the domain"
    );
}

#[tokio::test]
async fn an_unknown_provider_or_domain_is_an_error_not_a_silent_no_op() {
    let (_db, store) = db().await;
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(StubDomain::default())];

    // No provider for the kind: a list that quietly synced nothing forever would
    // be indistinguishable from an empty list.
    let list = a_list();
    let err = sync_list(&list, &[], &domains, &store).await.unwrap_err();
    assert!(err.to_string().contains("no import-list provider"), "{err}");

    // Targets a domain nobody registered.
    let mut bad = a_list();
    bad.target_domain = "podcasts".into();
    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubProvider(vec![]))];
    let err = sync_list(&bad, &providers, &domains, &store)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not registered"), "{err}");
}

#[tokio::test]
async fn only_due_lists_sync_and_the_run_is_recorded() {
    let (_db, store) = db().await;

    // Never synced ⇒ due. Otherwise adding a list appears to do nothing until
    // its first interval elapses and the operator concludes it is broken.
    let fresh = a_list();
    assert!(fresh.is_due(Utc::now()));
    store.upsert_import_list(&fresh).await.unwrap();

    let mut recent = ImportList::new("Recent", "stub", "movies");
    recent.last_synced_at = Some(Utc::now() - Duration::minutes(5));
    recent.interval_minutes = 720;
    assert!(!recent.is_due(Utc::now()));
    store.upsert_import_list(&recent).await.unwrap();

    let mut disabled = ImportList::new("Off", "stub", "movies");
    disabled.enabled = false;
    assert!(!disabled.is_due(Utc::now()), "a disabled list is never due");
    store.upsert_import_list(&disabled).await.unwrap();

    let providers: Vec<Arc<dyn ImportListProvider>> = vec![Arc::new(StubProvider(vec![item("1")]))];
    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(StubDomain::default())];

    let reports = sync_due_lists(&providers, &domains, &store).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].list_name, "Bond");

    // The run is recorded, so the next tick does not repeat it.
    let after = store.get_import_list(&fresh.id).await.unwrap().unwrap();
    assert!(after.last_synced_at.is_some());
    assert!(after.last_error.is_none());
    assert!(!after.is_due(Utc::now()));
}

#[tokio::test]
async fn a_failing_list_records_its_error_and_still_advances() {
    let (_db, store) = db().await;
    let list = a_list();
    store.upsert_import_list(&list).await.unwrap();

    let domains: Vec<Arc<dyn LibraryProvider>> = vec![Arc::new(StubDomain::default())];
    // No provider registered ⇒ the whole run fails.
    let reports = sync_due_lists(&[], &domains, &store).await.unwrap();
    assert!(reports.is_empty());

    let after = store.get_import_list(&list.id).await.unwrap().unwrap();
    assert!(after.last_error.is_some(), "the reason is on the row");
    // The interval still advances: a broken list must not re-run on every tick.
    assert!(after.last_synced_at.is_some());
}

#[tokio::test]
async fn excluding_the_same_item_twice_is_not_an_error() {
    let (_db, store) = db().await;
    let e = ImportListExclusion {
        id: uuid::Uuid::new_v4().to_string(),
        target_domain: "movies".into(),
        id_kind: "tmdb".into(),
        external_id: "7".into(),
        title: None,
        created_at: Utc::now(),
    };
    store.add_exclusion(&e).await.unwrap();
    // The UI cannot know what every list offers, so "exclude this" must be
    // repeatable rather than a request that sometimes 500s.
    let mut again = e.clone();
    again.id = uuid::Uuid::new_v4().to_string();
    store.add_exclusion(&again).await.unwrap();

    assert_eq!(store.list_exclusions(None).await.unwrap().len(), 1);
    assert!(store.is_excluded("movies", "tmdb", "7").await.unwrap());
    // Scoped by domain and by id namespace — a TMDB id and an IMDb id that
    // happen to read the same are different exclusions.
    assert!(!store.is_excluded("series", "tmdb", "7").await.unwrap());
    assert!(!store.is_excluded("movies", "imdb", "7").await.unwrap());
}
