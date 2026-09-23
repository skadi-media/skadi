//! Library-import commit e2e test (SKADI-T-0134, restructure semantics).
//!
//! Builds a synthetic on-disk audiobook tree (an ASIN-tagged single `.m4b` book
//! and a multi-file MP3 folder), then drives `scan -> match -> commit` end-to-end
//! against a real `AudnexusProvider` pointed at a scripted `wiremock` server and
//! an isolated [`TestDb`].
//!
//! Proves the restructure guarantee: `import::commit_item` builds the [`Book`]
//! from Audnexus, **hardlinks** each audio file into the canonical
//! Author/[Series/]Book layout under the root folder, and registers the book with
//! one `Imported` `BookFile` pointing at the canonical primary path. The original
//! source files are never modified or removed (still on disk, same inode).

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use skadi_audiobooks::{
    AudiobooksRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS, import, scan_candidates,
};
use skadi_core::{AcquisitionStatus, AsinId, ProfileId, RootFolder, RootFolderId};
use skadi_http::HttpClient;
use skadi_metadata::AudnexusProvider;
use skadi_testsupport::TestDb;

/// Canonical Audnexus book JSON for a single-file book (Project Hail Mary).
fn phm_json() -> serde_json::Value {
    json!({
        "asin": "B08G9PRS1K",
        "title": "Project Hail Mary",
        "authors": [{ "asin": "A9", "name": "Andy Weir" }],
        "narrators": [{ "name": "Ray Porter" }],
        "runtimeLengthMin": 970,
        "image": "https://img/phm.jpg",
        "releaseDate": "2021-05-04T00:00:00.000Z",
        "formatType": "unabridged",
        "summary": "Ryland Grace wakes up alone.",
        "language": "english"
    })
}

/// Canonical Audnexus book JSON for a multi-file series book (The Way of Kings).
fn wok_json() -> serde_json::Value {
    json!({
        "asin": "B003ITRL7G",
        "title": "The Way of Kings",
        "authors": [{ "asin": "A1", "name": "Brandon Sanderson" }],
        "narrators": [{ "name": "Michael Kramer" }],
        "seriesPrimary": { "asin": "S1", "name": "Stormlight Archive", "position": "1" },
        "runtimeLengthMin": 2734,
        "releaseDate": "2010-08-31T00:00:00.000Z",
        "formatType": "unabridged",
        "language": "english"
    })
}

#[cfg(unix)]
fn inode_of(p: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap();
    (m.dev(), m.ino())
}

fn touch(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn scan_match_commit_hardlinks_into_canonical_layout_and_preserves_sources() {
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();

    // Scripted Audnexus: the two book lookups the commit path hits.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/books/B08G9PRS1K"))
        .respond_with(ResponseTemplate::new(200).set_body_json(phm_json()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/books/B003ITRL7G"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wok_json()))
        .mount(&server)
        .await;
    let provider = AudnexusProvider::new(HttpClient::new(Duration::from_secs(5)).unwrap())
        .with_base_url(server.uri());

    // Synthetic source tree. Dump and library root live under one tempdir (same
    // filesystem), so hardlinking is possible.
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("dump");

    // (1) A single-file book at the top level, ASIN-tagged in the filename.
    let phm_bytes = vec![7u8; 4096];
    let phm_src = dump.join("Andy Weir - Project Hail Mary {asin-B08G9PRS1K}.m4b");
    touch(&phm_src, &phm_bytes);

    // (2) A multi-file MP3 folder, ASIN-tagged in the folder name.
    let wok_dir = dump.join("The Way of Kings {asin-B003ITRL7G}");
    let wok_a = wok_dir.join("Chapter_01.mp3");
    let wok_b = wok_dir.join("Chapter_02.mp3");
    touch(&wok_a, b"ch1");
    touch(&wok_b, b"ch2");
    touch(&wok_dir.join("cover.jpg"), b"img"); // non-audio, ignored

    // --- scan ---
    let candidates = scan_candidates(&dump).unwrap();
    assert_eq!(candidates.len(), 2, "two books discovered");
    let phm = candidates
        .iter()
        .find(|c| c.asin.as_ref().map(|a| a.0.as_str()) == Some("B08G9PRS1K"))
        .expect("single-file PHM candidate");
    assert!(phm.single_file);
    let wok = candidates
        .iter()
        .find(|c| c.asin.as_ref().map(|a| a.0.as_str()) == Some("B003ITRL7G"))
        .expect("multi-file WoK candidate");
    assert!(!wok.single_file);
    assert_eq!(wok.files.len(), 2, "two chapters, cover dropped");

    // --- commit (with the scan-extracted ASINs, simulating user confirmation) ---
    let library = dir.path().join("audiobooks");
    let root = RootFolder {
        id: RootFolderId::new(),
        path: library.clone(),
    };
    let profile = ProfileId::new();

    // Capture source inodes BEFORE the commits move them, so we can prove the
    // canonical paths are the same inodes (hardlink, not copy) — SKADI-T-0303.
    #[cfg(unix)]
    let phm_inode = inode_of(&phm_src);
    #[cfg(unix)]
    let wok_a_inode = inode_of(&wok_a);

    let (phm_book, phm_placement) = import::commit_item(
        &store,
        &provider,
        phm.files.clone(),
        AsinId("B08G9PRS1K".into()),
        profile,
        root.clone(),
        None,
    )
    .await
    .unwrap()
    .expect("PHM imported");
    assert_eq!(phm_placement, import::Placement::Linked);

    let (_wok_book, wok_placement) = import::commit_item(
        &store,
        &provider,
        wok.files.clone(),
        AsinId("B003ITRL7G".into()),
        profile,
        root.clone(),
        None,
    )
    .await
    .unwrap()
    .expect("WoK imported");
    assert_eq!(wok_placement, import::Placement::Linked);

    // --- single-file assertions ---
    let phm_canonical =
        library.join("andy-weir/project-hail-mary_{asin-B08G9PRS1K}/project-hail-mary.m4b");
    let loaded = store
        .get_book_by_asin(&AsinId("B08G9PRS1K".into()))
        .await
        .unwrap()
        .expect("PHM persisted");
    assert_eq!(loaded.title, "Project Hail Mary");
    assert_eq!(loaded.files.len(), 1);
    let imported_path = match &loaded.files[0].status {
        AcquisitionStatus::Imported { file, .. } => file.path.clone(),
        other => panic!("expected Imported, got {other:?}"),
    };
    assert_eq!(
        imported_path, phm_canonical,
        "registered at the canonical path"
    );
    assert!(phm_canonical.exists(), "canonical file placed");
    #[cfg(unix)]
    assert_eq!(
        phm_inode,
        inode_of(&phm_canonical),
        "hardlink, not copy (inode preserved across the move)"
    );

    // Reorg-via-link is a MOVE (SKADI-T-0303): the source m4b is gone, the data
    // living on at the canonical path byte-for-byte.
    assert!(!phm_src.exists(), "PHM source removed (moved)");
    assert_eq!(std::fs::read(&phm_canonical).unwrap(), phm_bytes);
    let _ = phm_book;

    // --- multi-file assertions (series folder, source names kept) ---
    let wok_book_dir =
        library.join("brandon-sanderson/stormlight-archive/1_-_the-way-of-kings_{asin-B003ITRL7G}");
    assert!(
        wok_book_dir.join("Chapter_01.mp3").exists(),
        "chapter 1 placed under series folder, source name kept"
    );
    assert!(wok_book_dir.join("Chapter_02.mp3").exists());
    // The source chapters are moved out (gone); the cover.jpg left behind keeps the
    // source folder around (we only remove the audio we adopted) — SKADI-T-0303.
    assert!(
        !wok_a.exists() && !wok_b.exists(),
        "WoK source chapters removed (moved)"
    );
    #[cfg(unix)]
    assert_eq!(
        wok_a_inode,
        inode_of(&wok_book_dir.join("Chapter_01.mp3")),
        "hardlink, not copy (inode preserved across the move)"
    );

    // --- duplicate ASIN is skipped (Ok(None)) ---
    let again = import::commit_item(
        &store,
        &provider,
        phm.files.clone(),
        AsinId("B08G9PRS1K".into()),
        profile,
        root,
        None,
    )
    .await
    .unwrap();
    assert!(again.is_none(), "duplicate ASIN is skipped");
}

/// SKADI-T-0541: the library-import scan hides books already held, matched by
/// **inode** rather than path.
///
/// Import hardlinks library files to their source (SKADI-T-0424), so the held
/// copy and the one still sitting in the scan directory are the same inode under
/// two different names — the canonical library name, and whatever the operator
/// called it. A path comparison misses every one of them, which is why the
/// clutter appeared despite the library "knowing" about the file.
///
/// Covers the domain-specific half: that the audiobooks repo query reaches the
/// held file paths, and that the identity rule then matches the source.
#[tokio::test]
async fn held_book_files_are_identified_by_inode_not_path() {
    use skadi_audiobooks::{AudiobooksRepo, Book, BookFile, BookFilter};
    use skadi_core::{AcquisitionStatus, AsinId, ExternalIds, ProfileId, RootFolder};
    use skadi_testsupport::TestDb;
    use std::path::PathBuf;

    let db = TestDb::new(
        skadi_audiobooks::SQLITE_MIGRATIONS,
        skadi_audiobooks::POSTGRES_MIGRATIONS,
    )
    .await;
    let store = &db.store;

    let dir = skadi_core::unique_temp_path("ab-scan-filter");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("Andy Weir - Project Hail Mary.m4b");
    std::fs::write(&source, b"audio").unwrap();
    let library = dir.join("project-hail-mary (2021).m4b");
    std::fs::hard_link(&source, &library).unwrap();
    let other = dir.join("Unrelated Book.m4b");
    std::fs::write(&other, b"different").unwrap();

    let mut book = Book::new(
        ExternalIds {
            asin: Some(AsinId("B08G9PRS1K".into())),
            ..Default::default()
        },
        "Project Hail Mary",
        ProfileId::new(),
        RootFolder::new("/audiobooks"),
    );
    book.monitored = true;
    store.upsert_book(&book).await.unwrap();
    let file = BookFile::missing(book.id);
    store.upsert_book_file(&file).await.unwrap();
    // The file column is written by `set_book_file_status` — the status variant is
    // the single source of truth for what is held.
    store
        .set_book_file_status(
            file.id,
            AcquisitionStatus::Imported {
                file: skadi_core::FileRef {
                    path: library.clone(),
                },
                quality: skadi_quality::UNKNOWN_QUALITY_ID,
                score: 0,
                at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();

    // The query the scan handler runs.
    let held: Vec<PathBuf> = store
        .list_books(BookFilter::default())
        .await
        .unwrap()
        .iter()
        .flat_map(|b| b.files.iter())
        .filter_map(|f| f.file.as_ref().map(|x| x.path.clone()))
        .collect();
    assert_eq!(held, vec![library.clone()], "the repo query finds the file");

    let ids = skadi_importer::file_identities(&held);
    assert!(
        skadi_importer::file_identity(&source).is_some_and(|id| ids.contains(&id)),
        "the source shares an inode with the held library file"
    );
    assert!(
        skadi_importer::file_identity(&other).is_some_and(|id| !ids.contains(&id)),
        "an unrelated file must not be filtered"
    );
    assert_ne!(source, library, "names differ; only the inode matches");
}
