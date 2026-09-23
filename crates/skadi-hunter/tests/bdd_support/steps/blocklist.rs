//! C14: the blocklist as the hunter consumes it — expiry semantics of the
//! store repo the `decide` gate reads, and *arr-parity gaps.

use cucumber::{given, then, when};

use skadi_store::{BlocklistRepo, NewBlocklistEntry};

use crate::bdd_support::World;
use crate::bdd_support::fixtures::{magnet_for, release};

fn entry(
    w: &World,
    title: &str,
    acquirable: &str,
    expires_in_hours: Option<i64>,
) -> NewBlocklistEntry {
    let r = release(w.kind(), title, Some(1), magnet_for(title));
    NewBlocklistEntry {
        release_key: skadi_indexers::release_key(&r),
        title: title.to_string(),
        acquirable_ref: Some(acquirable.to_string()),
        indexer: Some("bdd".into()),
        reason: Some("manual".into()),
        expires_at: expires_in_hours.map(|h| chrono::Utc::now() + chrono::Duration::hours(h)),
    }
}

#[given(expr = "{string} was blocklisted for {string} permanently")]
async fn block_permanent(w: &mut World, title: String, acquirable: String) {
    let e = entry(w, &title, &acquirable, None);
    w.store.as_ref().unwrap().block(&e).await.unwrap();
}

#[given(expr = "{string} was blocklisted for {string} expiring in {int} hours")]
async fn block_expiring(w: &mut World, title: String, acquirable: String, hours: i64) {
    let e = entry(w, &title, &acquirable, Some(hours));
    w.store.as_ref().unwrap().block(&e).await.unwrap();
}

#[when(expr = "{string} is blocklisted again for {string}")]
async fn reblock(w: &mut World, title: String, acquirable: String) {
    block_permanent(w, title, acquirable).await;
}

#[when("expired blocklist entries are purged")]
async fn purge(w: &mut World) {
    let n = w
        .store
        .as_ref()
        .unwrap()
        .purge_expired_blocklist()
        .await
        .unwrap();
    w.notes.push(format!("purged {n}"));
}

#[then(expr = "the blocklist vetoes {string}")]
async fn vetoes(w: &mut World, title: String) {
    let r = release(w.kind(), &title, Some(1), magnet_for(&title));
    let key = skadi_indexers::release_key(&r);
    let store = w.store.as_ref().unwrap();
    assert!(store.is_blocked(&key).await.unwrap());
    assert!(store.blocked_keys().await.unwrap().contains(&key));
}

#[then(expr = "the blocklist does not veto {string}")]
async fn no_veto(w: &mut World, title: String) {
    let r = release(w.kind(), &title, Some(1), magnet_for(&title));
    let key = skadi_indexers::release_key(&r);
    let store = w.store.as_ref().unwrap();
    assert!(!store.is_blocked(&key).await.unwrap());
    assert!(!store.blocked_keys().await.unwrap().contains(&key));
}

#[then(expr = "the blocklist holds {int} entry/entries")]
async fn count(w: &mut World, n: usize) {
    assert_eq!(
        w.store
            .as_ref()
            .unwrap()
            .list_blocklist()
            .await
            .unwrap()
            .len(),
        n
    );
}

#[then(expr = "the blocklist for {string} holds {int} entry/entries")]
async fn count_for(w: &mut World, acquirable: String, n: usize) {
    let rows = w
        .store
        .as_ref()
        .unwrap()
        .list_blocklist_for(&acquirable)
        .await
        .unwrap();
    assert_eq!(rows.len(), n);
}

#[then(expr = "the blocklist still lets {string} be grabbed for {string}")]
async fn scoped(w: &mut World, title: String, acquirable: String) {
    // REQ-BLOCKLIST.6 / Sonarr: a blocklist row is scoped to the series/movie it
    // was recorded for. Skadi's veto set is `blocked_keys()` — global by
    // release key — so the item scope is informational only.
    let r = release(w.kind(), &title, Some(1), magnet_for(&title));
    let key = skadi_indexers::release_key(&r);
    let store = w.store.as_ref().unwrap();
    let rows = store.list_blocklist_for(&acquirable).await.unwrap();
    let scoped_veto = rows.iter().any(|e| e.release_key == key);
    let global_veto = store.blocked_keys().await.unwrap().contains(&key);
    assert!(
        !global_veto || scoped_veto,
        "{title} is vetoed for {acquirable} although it was only blocklisted for another item"
    );
}
