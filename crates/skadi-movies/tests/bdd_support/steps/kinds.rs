//! C28 edition-kind registry steps (`MoviesRepo` registry methods).
use cucumber::{then, when};

use skadi_core::{AppError, EditionKindId};
use skadi_movies::{EditionKind, MoviesRepo};

use crate::bdd_support::World;

#[when("the edition kinds are listed")]
async fn list(w: &mut World) {
    w.kinds = w.store().list_edition_kinds().await.expect("list");
}

#[then(expr = "there are {int} builtin kinds")]
async fn n_builtin(w: &mut World, n: usize) {
    assert_eq!(
        w.kinds.iter().filter(|k| k.builtin).count(),
        n,
        "{:?}",
        w.kinds
    );
}

#[then("the kinds are ordered by name")]
async fn ordered(w: &mut World) {
    let names: Vec<&str> = w.kinds.iter().map(|k| k.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}

#[then(expr = "the kinds include {string}")]
async fn includes(w: &mut World, list: String) {
    for name in list.split(',').map(str::trim) {
        assert!(
            w.kinds.iter().any(|k| k.name == name),
            "{name} missing from {:?}",
            w.kinds.iter().map(|k| &k.name).collect::<Vec<_>>()
        );
    }
}

#[when(expr = "the builtin kind {string} is deleted")]
async fn delete_builtin(w: &mut World, name: String) {
    let k = w
        .store()
        .list_edition_kinds()
        .await
        .unwrap()
        .into_iter()
        .find(|k| k.name == name)
        .expect("kind");
    w.error = w
        .store()
        .delete_edition_kind(k.id)
        .await
        .err()
        .map(|e| format!("{e:?}"));
}

#[then("the delete is rejected as a validation error")]
async fn delete_rejected(w: &mut World) {
    let e = w.error.as_deref().expect("an error");
    assert!(e.starts_with("Validation"), "{e}");
}

#[then(expr = "the kind {string} still exists")]
async fn still_exists(w: &mut World, name: String) {
    let kinds = w.store().list_edition_kinds().await.unwrap();
    assert!(kinds.iter().any(|k| k.name == name));
}

#[when(expr = "a custom kind {string} tagged {string} matching {string} is created")]
async fn create(w: &mut World, name: String, tag: String, pattern: String) {
    let k = EditionKind {
        id: EditionKindId::new(),
        name,
        normalized_tag: tag,
        match_patterns: vec![pattern],
        builtin: false,
    };
    w.error = w
        .store()
        .upsert_edition_kind(&k)
        .await
        .err()
        .map(|e| format!("{e:?}"));
    w.kinds.push(k);
}

#[then(expr = "the kind {string} can be found by tag {string} and is not builtin")]
async fn by_tag(w: &mut World, name: String, tag: String) {
    let k = w
        .store()
        .get_edition_kind_by_tag(&tag)
        .await
        .unwrap()
        .expect("found by tag");
    assert_eq!(k.name, name);
    assert!(!k.builtin);
    let by_id = w.store().get_edition_kind(k.id).await.unwrap().unwrap();
    assert_eq!(by_id, k);
}

#[when(expr = "the custom kind {string} is deleted")]
async fn delete_custom(w: &mut World, name: String) {
    let k = w
        .kinds
        .iter()
        .find(|k| k.name == name)
        .expect("created kind")
        .clone();
    w.store().delete_edition_kind(k.id).await.expect("delete");
}

#[then(expr = "the kind {string} is gone")]
async fn gone(w: &mut World, name: String) {
    let kinds = w.store().list_edition_kinds().await.unwrap();
    assert!(!kinds.iter().any(|k| k.name == name));
}

#[then("the second kind write is rejected as a validation error")]
async fn dup_rejected(w: &mut World) {
    let e = w.error.as_deref().unwrap_or("Ok(())");
    let want = AppError::Validation("normalized tag collides".into());
    assert!(
        e.starts_with("Validation"),
        "expected {want:?} for a colliding normalized_tag, got {e}"
    );
}

#[when(expr = "a kind with an empty name tagged {string} is created")]
async fn empty_name(w: &mut World, tag: String) {
    create(w, String::new(), tag, "x".into()).await;
}

#[then("the kind write is rejected as a validation error")]
async fn empty_rejected(w: &mut World) {
    let e = w.error.as_deref().unwrap_or("Ok(())");
    assert!(e.starts_with("Validation"), "empty-name kind accepted: {e}");
}

#[then(expr = "the kind {string} round-trips through JSON")]
async fn serde(w: &mut World, name: String) {
    let k = w.kinds.iter().find(|k| k.name == name).expect("kind");
    let json = serde_json::to_string(k).unwrap();
    let back: EditionKind = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, k);
}
