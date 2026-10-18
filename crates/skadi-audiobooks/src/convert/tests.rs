//! Tests for the M4B conversion (SKADI-T-0139).
//!
//! The fixtures are silent MP3s written frame by frame here, so only the
//! conversion itself needs ffmpeg. Tests that run the real binary skip, with a
//! printed reason, when no working ffmpeg is found (`SKADI_TEST_FFMPEG`, else
//! `ffmpeg` on `PATH`). The failure paths use a stand-in script and always run.

use std::path::{Path, PathBuf};

use chrono::Utc;
use diesel::connection::Connection;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::MigrationHarness;
use tokio_util::sync::CancellationToken;

use skadi_core::{AcquisitionStatus, ExternalIds, FileRef, ProfileId, RootFolder};
use skadi_store::{ConfigRepo, ConfigSource, HistoryEntry, HistoryRepo, Store, TraceRepo};

use super::*;
use crate::SQLITE_MIGRATIONS;

async fn fresh_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skadi.db");
    let url = format!("sqlite://{}", path.display());
    let s = Store::connect(&url).unwrap();
    s.run_migrations().await.unwrap();
    drop(s);
    let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
    conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
    (dir, Store::connect(&url).unwrap())
}

/// A silent CBR MP3 of about `secs` seconds: MPEG-1 Layer III, 128 kbps,
/// 44.1 kHz, mono frames whose side info and main data are all zero.
fn silent_mp3(path: &Path, secs: u32) {
    const FRAME_LEN: usize = 417; // 144 * 128000 / 44100, no padding
    let frames = (secs as usize * 44_100).div_ceil(1152);
    let mut frame = vec![0u8; FRAME_LEN];
    frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC0]);
    let mut bytes = Vec::with_capacity(frames * FRAME_LEN);
    for _ in 0..frames {
        bytes.extend_from_slice(&frame);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

/// A book with one edition imported as `files` (in its book folder under
/// `root`), recorded in the acquisition history when `acquired`.
async fn imported_book(
    store: &Store,
    root: &Path,
    files: &[(&str, u32)],
    acquired: bool,
) -> (Book, BookFile, PathBuf) {
    let dir = root.join("robin-sample").join("the-long-road");
    for (name, secs) in files {
        silent_mp3(&dir.join(name), *secs);
    }
    let mut book = Book::new(
        ExternalIds::default(),
        "The Long Road",
        ProfileId::new(),
        RootFolder::new(root.to_string_lossy().into_owned()),
    );
    book.authors = vec!["Robin Sample".into()];
    store.upsert_book(&book).await.unwrap();
    let primary = dir.join(files.first().map_or("part 1.m4a", |f| f.0));
    let mut file = BookFile::missing(book.id);
    let mp3 = skadi_quality::audiobook::default_audiobook_definitions()
        .into_iter()
        .find(|d| d.name == "MP3-128")
        .unwrap()
        .id;
    file.status = AcquisitionStatus::Imported {
        file: FileRef {
            path: primary.clone(),
        },
        quality: mp3,
        score: 0,
        at: Utc::now() - chrono::Duration::minutes(5),
    };
    file.file = Some(FileRef {
        path: primary.clone(),
    });
    file.quality = Some(mp3);
    store.upsert_book_file(&file).await.unwrap();
    // As stored: the database keeps microseconds, and a Linux clock gives
    // nanoseconds, so the in-memory row would not compare equal to a read.
    let file = store.get_book_file(file.id).await.unwrap().unwrap();
    if acquired {
        store
            .record_history(&HistoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                at: Utc::now(),
                kind: "audiobook".into(),
                acquirable_ref: file.acquirable_ref().0,
                label: book.title.clone(),
                event: "imported".into(),
                detail: None,
                reason_code: None,
            })
            .await
            .unwrap();
    }
    book.files = vec![file.clone()];
    (book, file, dir)
}

async fn enable(store: &Store) {
    store
        .set_config(CONVERT_KEY, "true", ConfigSource::Runtime)
        .await
        .unwrap();
}

/// A stand-in ffmpeg: answers `-version`, then runs `body` for a conversion.
fn fake_ffmpeg(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("fake-ffmpeg");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo \"ffmpeg version fake\"; exit 0; fi\n{body}\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

/// A working ffmpeg, or `None` with the reason printed.
async fn working_ffmpeg(test: &str) -> Option<PathBuf> {
    let bin = std::env::var_os("SKADI_TEST_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffmpeg"));
    if ffmpeg::available(&bin).await {
        eprintln!("{test}: using {}", bin.display());
        Some(bin)
    } else {
        eprintln!(
            "SKIPPED {test}: no working ffmpeg ({} -version failed); set SKADI_TEST_FFMPEG to run it",
            bin.display()
        );
        None
    }
}

fn hidden_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.starts_with('.'))
        .collect()
}

#[tokio::test]
async fn feature_off_changes_nothing() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("01.mp3", 2), ("02.mp3", 2)], true).await;
    // An ffmpeg that leaves a mark if it is ever run.
    let mark = tmp.path().join("ran");
    let bin = fake_ffmpeg(tmp.path(), &format!("touch '{}'", mark.display()));
    let job = ConvertJob::with_ffmpeg(store.clone(), bin);

    let report = job.run_pass(&CancellationToken::new()).await;
    assert!(!report.enabled);
    // Explicitly off reads the same as unset.
    store
        .set_config(CONVERT_KEY, "false", ConfigSource::Runtime)
        .await
        .unwrap();
    let report = job.run_pass(&CancellationToken::new()).await;
    assert_eq!(report, PassReport::default());

    assert!(!mark.exists(), "ffmpeg was not run");
    assert_eq!(store.get_book_file(file.id).await.unwrap().unwrap(), file);
    assert!(dir.join("01.mp3").exists() && dir.join("02.mp3").exists());
    assert!(
        store
            .traces_for(&file.acquirable_ref().0)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_library_import_is_not_converted() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    imported_book(&store, &root, &[("01.mp3", 2)], false).await;
    enable(&store).await;
    let bin = fake_ffmpeg(tmp.path(), "exit 1");
    let report = ConvertJob::with_ffmpeg(store, bin)
        .run_pass(&CancellationToken::new())
        .await;
    assert!(report.enabled);
    assert_eq!(
        report.candidates, 0,
        "no acquire-path import, no conversion"
    );
}

#[tokio::test]
async fn missing_ffmpeg_marks_nothing() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, _) = imported_book(&store, &root, &[("01.mp3", 2)], true).await;
    enable(&store).await;
    let report = ConvertJob::with_ffmpeg(store.clone(), "/nonexistent/ffmpeg".into())
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.candidates, 1);
    assert!(report.ffmpeg_missing);
    assert!(
        store
            .traces_for(&file.acquirable_ref().0)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_failed_conversion_keeps_the_original_import() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("01.mp3", 2), ("02.mp3", 2)], true).await;
    enable(&store).await;
    let bin = fake_ffmpeg(tmp.path(), "echo 'Invalid data found' >&2\nexit 1");
    let job = ConvertJob::with_ffmpeg(store.clone(), bin);

    let report = job.run_pass(&CancellationToken::new()).await;
    assert_eq!(report.failed, 1);
    assert_eq!(store.get_book_file(file.id).await.unwrap().unwrap(), file);
    assert!(dir.join("01.mp3").exists() && dir.join("02.mp3").exists());
    assert!(
        hidden_files(&dir).is_empty(),
        "no partial output left behind"
    );

    let traces = store.traces_for(&file.acquirable_ref().0).await.unwrap();
    let failed = traces
        .iter()
        .find(|t| t.event == "convert_failed")
        .expect("the failure is on the trace stream");
    assert!(
        failed
            .detail
            .as_deref()
            .unwrap()
            .contains("Invalid data found")
    );
    assert!(!skadi_hunter::tracker().is_active(&file.acquirable_ref().0));

    // Not tried again straight away.
    let again = job.run_pass(&CancellationToken::new()).await;
    assert_eq!(again.candidates, 0);
}

#[tokio::test]
async fn output_that_is_not_audio_is_refused() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("01.mp3", 2)], true).await;
    enable(&store).await;
    // "Succeeds" but writes junk to its last argument, the output.
    let bin = fake_ffmpeg(
        tmp.path(),
        "for last; do :; done\necho junk > \"$last\"\nexit 0",
    );
    let report = ConvertJob::with_ffmpeg(store.clone(), bin)
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.failed, 1);
    assert_eq!(store.get_book_file(file.id).await.unwrap().unwrap(), file);
    assert!(dir.join("01.mp3").exists());
    assert!(hidden_files(&dir).is_empty());
}

#[tokio::test]
async fn a_folder_with_an_m4b_of_another_length_is_skipped_for_good() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("01.mp3", 2)], true).await;
    // Not readable as audio: no length to compare, so not this book.
    std::fs::write(dir.join("the-long-road.m4b"), b"x").unwrap();
    enable(&store).await;
    let job = ConvertJob::with_ffmpeg(store.clone(), fake_ffmpeg(tmp.path(), "exit 1"));
    assert_eq!(job.run_pass(&CancellationToken::new()).await.skipped, 1);
    let traces = store.traces_for(&file.acquirable_ref().0).await.unwrap();
    assert!(
        traces
            .iter()
            .any(|t| t.event == "convert_skipped" && t.message.contains("not the same length"))
    );
    assert_eq!(job.run_pass(&CancellationToken::new()).await.candidates, 0);
}

#[tokio::test]
async fn a_folder_with_two_m4bs_is_skipped_for_good() {
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("01.mp3", 2)], true).await;
    std::fs::write(dir.join("one.m4b"), b"x").unwrap();
    std::fs::write(dir.join("two.m4b"), b"x").unwrap();
    enable(&store).await;
    let job = ConvertJob::with_ffmpeg(store.clone(), fake_ffmpeg(tmp.path(), "exit 1"));
    assert_eq!(job.run_pass(&CancellationToken::new()).await.skipped, 1);
    let traces = store.traces_for(&file.acquirable_ref().0).await.unwrap();
    assert!(
        traces
            .iter()
            .any(|t| t.message.contains("more than one M4B"))
    );
}

/// A release that ships the M4B beside the MP3s (SKADI-T-0139): the edition
/// moves onto that M4B, the MP3 goes and the download stops seeding, with no
/// encode.
#[tokio::test]
async fn an_m4b_of_the_same_length_is_adopted() {
    let Some(bin) = working_ffmpeg("an_m4b_of_the_same_length_is_adopted").await else {
        return;
    };
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("the-long-road.mp3", 3)], true).await;
    let m4b = dir.join("the-long-road.m4b");
    let made = tokio::process::Command::new(&bin)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg("anullsrc=r=44100:cl=mono")
        .args(["-t", "3", "-c:a", "aac", "-b:a", "64k", "-f", "ipod"])
        .arg(&m4b)
        .status()
        .await
        .unwrap();
    assert!(made.success());
    use skadi_store::DownloadJobRepo;
    let dl = store
        .enqueue(&skadi_store::NewDownloadJob {
            acquirable_ref: "The Long Road".into(),
            source: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            category: None,
            incomplete_dir: None,
            complete_dir: None,
        })
        .await
        .unwrap();
    store
        .set_download_target(&dl.id, "audiobook", &file.acquirable_ref().0)
        .await
        .unwrap();
    store.mark_complete(&dl.id, &[]).await.unwrap();
    enable(&store).await;

    // A fake ffmpeg that fails: adoption must not run it.
    let report = ConvertJob::with_ffmpeg(store.clone(), fake_ffmpeg(tmp.path(), "exit 1"))
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.adopted, 1, "{report:?}");
    assert_eq!(report.converted, 0);

    let now = store.get_book_file(file.id).await.unwrap().unwrap();
    assert_eq!(now.file.as_ref().unwrap().path, m4b);
    let AcquisitionStatus::Imported {
        file: f, quality, ..
    } = &now.status
    else {
        panic!("still imported");
    };
    assert_eq!(f.path, m4b);
    let def = skadi_quality::audiobook::default_audiobook_definitions()
        .into_iter()
        .find(|d| d.id == *quality)
        .unwrap();
    assert_eq!(def.format, AudioFormat::M4b);
    assert!(m4b.exists() && !dir.join("the-long-road.mp3").exists());
    let dl = store.get_download(&dl.id).await.unwrap().unwrap();
    assert_eq!(dl.status, skadi_store::DownloadJobStatus::RemoveRequested);
    assert!(dl.delete_data);
}

#[tokio::test]
async fn converts_a_multi_file_mp3_book_to_one_chaptered_m4b() {
    let Some(bin) = working_ffmpeg("converts_a_multi_file_mp3_book_to_one_chaptered_m4b").await
    else {
        return;
    };
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let names = [
        ("01 - Departure.mp3", 3),
        ("02 - The Pass.mp3", 3),
        ("03 - Home.mp3", 3),
    ];
    let (_, file, dir) = imported_book(&store, &root, &names, true).await;
    // `seed` placement: each library file is a hardlink of a seeding download.
    let downloads = tmp.path().join("downloads/The Long Road");
    std::fs::create_dir_all(&downloads).unwrap();
    for (n, _) in &names {
        std::fs::hard_link(dir.join(n), downloads.join(n)).unwrap();
    }
    // The download that brought the book, still seeding.
    use skadi_store::DownloadJobRepo;
    let job = store
        .enqueue(&skadi_store::NewDownloadJob {
            acquirable_ref: "The Long Road".into(),
            source: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            category: None,
            incomplete_dir: None,
            complete_dir: None,
        })
        .await
        .unwrap();
    store
        .set_download_target(&job.id, "audiobook", &file.acquirable_ref().0)
        .await
        .unwrap();
    store.mark_complete(&job.id, &[]).await.unwrap();
    enable(&store).await;

    let report = ConvertJob::with_ffmpeg(store.clone(), bin)
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.converted, 1, "{report:?}");

    let now = store.get_book_file(file.id).await.unwrap().unwrap();
    let AcquisitionStatus::Imported {
        file: f, quality, ..
    } = &now.status
    else {
        panic!("still imported");
    };
    assert!(is_m4b(&f.path) && f.path.parent() == Some(dir.as_path()));
    assert_eq!(now.file.as_ref().unwrap().path, f.path);
    let def = skadi_quality::audiobook::default_audiobook_definitions()
        .into_iter()
        .find(|d| d.id == *quality)
        .unwrap();
    assert_eq!(def.format, AudioFormat::M4b);

    let marks = skadi_media_probe::chapters::chapters(&f.path).unwrap();
    assert_eq!(marks.len(), 3, "one chapter per file: {marks:?}");
    assert_eq!(marks[1].title, "02 - The Pass");

    // One audio file left in the library, and seeding stops: the download is
    // asked to go with its data (Dylan, 2026-10-17).
    let audio: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| crate::import::is_audio(p))
        .collect();
    assert_eq!(audio, vec![f.path.clone()]);
    let job = store.get_download(&job.id).await.unwrap().unwrap();
    assert_eq!(job.status, skadi_store::DownloadJobStatus::RemoveRequested);
    assert!(job.delete_data, "the source's data goes too");
    let traces = store.traces_for(&file.acquirable_ref().0).await.unwrap();
    assert!(traces.iter().any(|t| t.event == "converted"));
}

#[tokio::test]
async fn a_single_file_without_chapters_gets_one_and_the_original_goes() {
    let Some(bin) = working_ffmpeg("a_single_file_without_chapters_gets_one").await else {
        return;
    };
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("book.mp3", 3)], true).await;
    enable(&store).await;

    let report = ConvertJob::with_ffmpeg(store.clone(), bin)
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.converted, 1, "{report:?}");
    let now = store.get_book_file(file.id).await.unwrap().unwrap();
    let path = now.file.unwrap().path;
    assert_eq!(
        skadi_media_probe::chapters::chapters(&path).unwrap().len(),
        1
    );
    assert!(
        !dir.join("book.mp3").exists(),
        "the original goes once the M4B is recorded"
    );
}

#[tokio::test]
async fn a_cue_sheet_gives_the_chapters_of_a_single_file() {
    let Some(bin) = working_ffmpeg("a_cue_sheet_gives_the_chapters_of_a_single_file").await else {
        return;
    };
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let (_, file, dir) = imported_book(&store, &root, &[("book.mp3", 6)], true).await;
    std::fs::write(
        dir.join("book.cue"),
        "FILE \"book.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 01 00:02:00\nTRACK 03 AUDIO\nTITLE \"Third\"\nINDEX 01 00:04:00\n",
    )
    .unwrap();
    enable(&store).await;
    let report = ConvertJob::with_ffmpeg(store.clone(), bin)
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.converted, 1, "{report:?}");
    let path = store
        .get_book_file(file.id)
        .await
        .unwrap()
        .unwrap()
        .file
        .unwrap()
        .path;
    let marks = skadi_media_probe::chapters::chapters(&path).unwrap();
    let titles: Vec<_> = marks.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, ["First", "Second", "Third"]);
    assert!((marks[1].start_secs - 2.0).abs() < 0.1, "{marks:?}");
}

#[tokio::test]
async fn aac_sources_are_copied_not_re_encoded() {
    let Some(bin) = working_ffmpeg("aac_sources_are_copied_not_re_encoded").await else {
        return;
    };
    let (tmp, store) = fresh_store().await;
    let root = tmp.path().join("audiobooks");
    let dir = root.join("robin-sample").join("the-long-road");
    // Two AAC parts, made from the silent MP3 fixture.
    let mp3 = tmp.path().join("seed.mp3");
    silent_mp3(&mp3, 3);
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["part 1.m4a", "part 2.m4a"] {
        let ok = tokio::process::Command::new(&bin)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&mp3)
            .args(["-c:a", "aac", "-b:a", "64k"])
            .arg(dir.join(n))
            .status()
            .await
            .unwrap()
            .success();
        assert!(ok, "making the AAC fixture");
    }
    let (_, file, _) = imported_book(&store, &root, &[], true).await;
    // Point the edition at the first part (the helper wrote no files).
    let mut f = file.clone();
    let first = dir.join("part 1.m4a");
    if let AcquisitionStatus::Imported { file: r, .. } = &mut f.status {
        r.path = first.clone();
    }
    f.file = Some(FileRef { path: first });
    store.upsert_book_file(&f).await.unwrap();
    enable(&store).await;

    let report = ConvertJob::with_ffmpeg(store.clone(), bin)
        .run_pass(&CancellationToken::new())
        .await;
    assert_eq!(report.converted, 1, "{report:?}");
    let path = store
        .get_book_file(f.id)
        .await
        .unwrap()
        .unwrap()
        .file
        .unwrap()
        .path;
    assert_eq!(
        skadi_media_probe::chapters::chapters(&path).unwrap().len(),
        2
    );
}

#[test]
fn every_original_goes_and_an_only_copy_to_the_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let linked = lib.join("a.mp3");
    let only = lib.join("b.mp3");
    std::fs::write(&linked, b"a").unwrap();
    std::fs::write(&only, b"b").unwrap();
    let seeding = tmp.path().join("seeding-a.mp3");
    std::fs::hard_link(&linked, &seeding).unwrap();

    // With a recycle bin: a second name is removed (its download is stopped
    // with its data separately), the only copy goes to the bin.
    let bin = tmp.path().join("bin");
    assert_eq!(
        retire_originals(&[linked.clone(), only.clone()], Some(&bin)),
        (2, 0)
    );
    assert!(!linked.exists() && !only.exists());
    assert!(bin.join("b.mp3").exists() && !bin.join("a.mp3").exists());

    // Without one: deleted.
    std::fs::write(&only, b"b").unwrap();
    assert_eq!(retire_originals(std::slice::from_ref(&only), None), (1, 0));
    assert!(!only.exists());
}

#[test]
fn retry_rule() {
    let imported = Utc::now() - chrono::Duration::days(10);
    let now = Utc::now();
    let ev = |event: &str, days_ago: i64| TraceEvent {
        id: uuid::Uuid::new_v4().to_string(),
        at: now - chrono::Duration::days(days_ago),
        run_id: None,
        kind: "audiobook".into(),
        acquirable_ref: "x".into(),
        stage: STAGE.into(),
        event: event.into(),
        message: String::new(),
        detail: None,
    };
    assert!(attempt_allowed(&[], imported, now));
    assert!(!attempt_allowed(&[ev("convert_failed", 0)], imported, now));
    assert!(attempt_allowed(&[ev("convert_failed", 2)], imported, now));
    let three = [
        ev("convert_failed", 5),
        ev("convert_failed", 4),
        ev("convert_failed", 3),
    ];
    assert!(!attempt_allowed(&three, imported, now));
    assert!(!attempt_allowed(&[ev("convert_skipped", 5)], imported, now));
    // The old "already holds an M4B" skip is tried again: such a folder is now
    // adopted when its M4B matches.
    let legacy = TraceEvent {
        message: LEGACY_M4B_SKIP.into(),
        ..ev("convert_skipped", 5)
    };
    assert!(attempt_allowed(&[legacy], imported, now));
    // Events from before the current import do not count.
    assert!(attempt_allowed(&[ev("convert_skipped", 20)], imported, now));
}

#[test]
fn quality_is_an_m4b_tier() {
    let name = |kbps| {
        let id = m4b_quality(kbps);
        skadi_quality::audiobook::default_audiobook_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap()
            .name
    };
    assert_eq!(name(64), "M4B-64");
    assert_eq!(name(128), "M4B-128");
    assert_eq!(
        name(32),
        "M4B-64",
        "below the lowest tier is the lowest tier"
    );
}

#[test]
fn the_silent_fixture_reads_as_mp3() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("s.mp3");
    silent_mp3(&p, 3);
    let facts = skadi_media_probe::audio_facts(&p).expect("lofty reads it");
    assert!(
        (2900..=3100).contains(&facts.duration.as_millis()),
        "{facts:?}"
    );
}

#[test]
fn concurrency_is_three_by_default_and_1_to_8() {
    assert_eq!(concurrency(None), 3);
    assert_eq!(concurrency(Some(0)), 1);
    assert_eq!(concurrency(Some(4)), 4);
    assert_eq!(concurrency(Some(64)), MAX_CONCURRENCY);
}
