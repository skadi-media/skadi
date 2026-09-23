//! Library-import commit integration test (SKADI-T-0074, restructure semantics;
//! reorg-via-link adoption SKADI-T-0303).
//!
//! Proves the adoption guarantee: `import::commit_item` **hardlinks** the source
//! file to the canonical library path under the root folder
//! (`<root>/<Title (Year)>/<Title (Year)>.<ext>`), registers the movie/edition
//! pointing at the *canonical* path, then **drops the source** so the import is a
//! net move (same inode, no byte copy). A source already at its canonical path is
//! registered in place with no filesystem operation (and is NOT removed).

use async_trait::async_trait;
use chrono::NaiveDate;

use skadi_core::{
    AcquisitionStatus, ExternalIds, MediaKind, ProfileId, Result as SkadiResult, RootFolder,
    RootFolderId, TmdbId,
};
use skadi_metadata::{ExternalId, MetadataMatch, MetadataProvider, MetadataQuery, MetadataRecord};
use skadi_movies::import::Placement;
use skadi_movies::{MoviesRepo, POSTGRES_MIGRATIONS, SQLITE_MIGRATIONS, import};
use skadi_testsupport::TestDb;

/// A scripted metadata provider whose `lookup` returns a canned record.
struct FakeProvider {
    record: MetadataRecord,
}

#[async_trait]
impl MetadataProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn supports(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Movie
    }
    async fn search(&self, _q: &MetadataQuery) -> SkadiResult<Vec<MetadataMatch>> {
        Ok(vec![])
    }
    async fn lookup(&self, _id: &ExternalId) -> SkadiResult<MetadataRecord> {
        Ok(self.record.clone())
    }
}

fn provider_for(tmdb: u64, title: &str, year: i32) -> FakeProvider {
    FakeProvider {
        record: MetadataRecord {
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(tmdb)),
                ..Default::default()
            },
            title: title.into(),
            original_title: Some(title.into()),
            overview: Some("A movie.".into()),
            runtime_minutes: Some(136),
            release_date: NaiveDate::from_ymd_opt(year, 3, 31),
            images: vec![],
            ..Default::default()
        },
    }
}

#[cfg(unix)]
fn inode_of(p: &std::path::Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap();
    (m.dev(), m.ino())
}

#[tokio::test]
async fn commit_restructures_into_canonical_layout_and_preserves_source() {
    // Postgres-default isolated DB (SQLite fallback). Source dump and library
    // root live in one tempdir — same filesystem, so the hardlink path runs.
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    let dir = tempfile::tempdir().unwrap();

    // A messy, non-canonical source folder: video + companions.
    let dump = dir.path().join("dump/The.Matrix.1999.1080p.BluRay.x264");
    std::fs::create_dir_all(&dump).unwrap();
    let src = dump.join("The.Matrix.1999.1080p.BluRay.x264.mkv");
    let contents = vec![7u8; 64 * 1024 * 1024];
    std::fs::write(&src, &contents).unwrap();
    std::fs::write(
        dump.join("The.Matrix.1999.1080p.BluRay.x264.en.srt"),
        b"subs",
    )
    .unwrap();
    std::fs::write(dump.join("movie.nfo"), b"<movie/>").unwrap();
    std::fs::write(dump.join("poster.jpg"), b"img").unwrap();
    // Capture the source inode so we can prove the canonical path is the SAME
    // inode after the move (a hardlink, never a copy).
    #[cfg(unix)]
    let src_inode = inode_of(&src);

    let library = dir.path().join("movies");
    std::fs::create_dir_all(&library).unwrap();
    let root_folder = RootFolder {
        id: RootFolderId::new(),
        path: library.clone(),
    };

    let provider = provider_for(603, "The Matrix", 1999);
    let (movie, placement) = import::commit_item(
        &store,
        &provider,
        src.clone(),
        TmdbId(603),
        ProfileId::new(),
        root_folder.clone(),
        None,
    )
    .await
    .unwrap()
    .expect("a new movie is created");
    assert_eq!(placement, Placement::Linked);

    // The edition points at the CANONICAL path, which exists as a hardlink of
    // the source (same inode — no copy was made).
    let canonical = library.join("the-matrix_(1999)_{tmdb-603}/theatrical/the-matrix_(1999).mkv");
    let loaded = store.get_movie(movie.id).await.unwrap().unwrap();
    assert_eq!(loaded.title, "The Matrix");
    assert_eq!(loaded.year, Some(1999));
    assert_eq!(loaded.editions.len(), 1);
    let edition = &loaded.editions[0];
    let imported_path = match &edition.status {
        AcquisitionStatus::Imported { file, .. } => file.path.clone(),
        other => panic!("expected Imported, got {other:?}"),
    };
    assert_eq!(imported_path, canonical, "registered at the canonical path");
    assert_eq!(
        edition.file.as_ref().map(|f| f.path.clone()),
        Some(canonical.clone())
    );
    assert!(canonical.exists(), "canonical file placed");
    #[cfg(unix)]
    assert_eq!(
        src_inode,
        inode_of(&canonical),
        "hardlink, not copy (inode preserved across the move)"
    );

    // Companions came along: subs + nfo next to the video (renamed to the
    // canonical stem), generic artwork at the movie-folder level.
    let edition_dir = canonical.parent().unwrap();
    let movie_dir = edition_dir.parent().unwrap();
    assert!(edition_dir.join("the-matrix_(1999).en.srt").exists());
    assert!(edition_dir.join("the-matrix_(1999).nfo").exists());
    assert!(movie_dir.join("poster.jpg").exists());

    // Reorg-via-link is a MOVE (SKADI-T-0303): the dedicated source folder is
    // gone (and its now-empty parent pruned), but the data lives on at the
    // canonical path — byte-for-byte, just relocated.
    assert!(!src.exists(), "source file removed (moved)");
    assert!(!dump.exists(), "dedicated source folder removed");
    assert!(
        !dir.path().join("dump").exists(),
        "now-empty source parent pruned"
    );
    assert_eq!(std::fs::read(&canonical).unwrap(), contents);

    // Committing the same TMDB id again is a no-op skip (Ok(None)).
    let again = import::commit_item(
        &store,
        &provider,
        src.clone(),
        TmdbId(603),
        ProfileId::new(),
        root_folder,
        None,
    )
    .await
    .unwrap();
    assert!(again.is_none(), "duplicate TMDB id is skipped");
}

#[tokio::test]
async fn commit_registers_already_canonical_source_in_place() {
    let db = TestDb::new(SQLITE_MIGRATIONS, POSTGRES_MIGRATIONS).await;
    let store = db.store.clone();
    let dir = tempfile::tempdir().unwrap();

    // The source already sits at its canonical path under the root folder.
    let library = dir.path().join("movies");
    let file = library.join("heat_(1995)_{tmdb-949}/theatrical/heat_(1995).mkv");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"already organized").unwrap();

    let provider = provider_for(949, "Heat", 1995);
    let (_, placement) = import::commit_item(
        &store,
        &provider,
        file.clone(),
        TmdbId(949),
        ProfileId::new(),
        RootFolder {
            id: RootFolderId::new(),
            path: library,
        },
        None,
    )
    .await
    .unwrap()
    .expect("a new movie is created");

    assert_eq!(placement, Placement::InPlace);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().nlink(),
            1,
            "no extra link created for an already-canonical file"
        );
    }
}
