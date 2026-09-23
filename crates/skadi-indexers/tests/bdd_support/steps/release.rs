//! The `Release` contract helpers: blocklist identity (`release_key`), manual
//! links (`release_from_link`) and title normalisation.
use chrono::Utc;
use cucumber::{given, then, when};
use skadi_core::IndexerId;
use skadi_indexers::{Release, ReleaseFetch, normalize_title, release_from_link, release_key};
use skadi_quality::ParsedRelease;

use crate::bdd_support::World;

fn rel(fetch: ReleaseFetch, title: &str, size: u64) -> Release {
    Release {
        indexer: IndexerId::new(),
        title: title.into(),
        fetch,
        size,
        published: Utc::now(),
        seeders: None,
        categories: Vec::new(),
        parsed: ParsedRelease::default(),
    }
}

#[given(regex = r#"^a release "([^"]*)" of size (\d+) fetched by magnet "([^"]*)"$"#)]
fn magnet_release(w: &mut World, title: String, size: u64, magnet: String) {
    w.release = Some(rel(ReleaseFetch::Magnet(magnet), &title, size));
}

#[given(regex = r#"^a release "([^"]*)" of size (\d+) fetched by torrent URL "([^"]*)"$"#)]
fn url_release(w: &mut World, title: String, size: u64, url: String) {
    w.release = Some(rel(ReleaseFetch::TorrentUrl(url), &title, size));
}

#[when("its blocklist key is computed")]
fn key(w: &mut World) {
    w.key = Some(release_key(w.release.as_ref().expect("release")));
}

#[then(regex = r#"^the blocklist key is "([^"]*)"$"#)]
fn key_is(w: &mut World, want: String) {
    assert_eq!(w.key.as_deref(), Some(want.as_str()));
}

#[then(regex = r#"^the blocklist key equals that of a re-search yielding torrent URL "([^"]*)"$"#)]
fn key_stable(w: &mut World, url: String) {
    let mut again = w.release.clone().expect("release");
    again.fetch = ReleaseFetch::TorrentUrl(url);
    assert_eq!(w.key.as_deref(), Some(release_key(&again).as_str()));
}

#[then(regex = r"^the blocklist key differs from that of the same title at size (\d+)$")]
fn key_differs(w: &mut World, size: u64) {
    let mut other = w.release.clone().expect("release");
    other.size = size;
    assert_ne!(w.key.as_deref(), Some(release_key(&other).as_str()));
}

#[when(regex = r#"^the operator pastes the link "([^"]*)" with title "([^"]*)"$"#)]
fn paste_titled(w: &mut World, link: String, title: String) {
    w.release = release_from_link(&link, Some(&title), skadi_quality::parse);
}

#[when(regex = r#"^the operator pastes the link "([^"]*)" with no title$"#)]
fn paste(w: &mut World, link: String) {
    w.release = release_from_link(&link, None, skadi_quality::parse);
}

#[then(regex = r#"^a manual release titled "([^"]*)" is built$"#)]
fn manual_built(w: &mut World, title: String) {
    let r = w.release.as_ref().expect("a release should be built");
    assert_eq!(r.title, title);
    assert_eq!(r.size, 0, "size unknown for a pasted link");
    assert_eq!(r.seeders, None);
}

#[then("the manual release fetch is a magnet")]
fn manual_magnet(w: &mut World) {
    assert!(matches!(
        w.release.as_ref().map(|r| &r.fetch),
        Some(ReleaseFetch::Magnet(_))
    ));
}

#[then("the manual release fetch is a torrent URL")]
fn manual_url(w: &mut World) {
    assert!(matches!(
        w.release.as_ref().map(|r| &r.fetch),
        Some(ReleaseFetch::TorrentUrl(_))
    ));
}

#[then("no manual release is built")]
fn manual_none(w: &mut World) {
    assert!(w.release.is_none(), "built {:?}", w.release);
}

#[then(regex = r#"^the title "([^"]*)" normalises to "([^"]*)"$"#)]
fn normalises(_w: &mut World, title: String, want: String) {
    assert_eq!(normalize_title(&title), want);
}
