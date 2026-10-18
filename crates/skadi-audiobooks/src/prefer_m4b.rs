//! Import a download as its M4B when it also holds the book as MP3s
//! (SKADI-T-0740).
//!
//! Some releases ship one book twice: the chapter MP3s and one M4B of the same
//! length. The generic importer placed every audio file and recorded the first
//! one, so the edition pointed at an MP3. [`PreferM4b`] checks the download
//! first: when it holds exactly one M4B and that M4B is as long as the rest of
//! the audio, only the M4B goes to the library. The other audio stays in the
//! download, which keeps seeding, and is reported as rejected.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use skadi_core::{AppError, Result};
use skadi_importer::{CompletedDownload, ImportOutcome, ImportPlan, Importer};

use crate::convert::{is_m4b, same_length};
use crate::import::{is_audio, is_excerpt};

/// Why the other copy of the book was left in the download.
const SKIPPED: &str =
    "not imported: the download also holds this book as one M4B of the same length";

/// An [`Importer`] that leaves out the MP3 copy of a book the download also
/// holds as one M4B.
pub struct PreferM4b<I> {
    inner: I,
}

impl<I: Importer> PreferM4b<I> {
    #[must_use]
    pub fn new(inner: I) -> Self {
        Self { inner }
    }
}

/// `completed` without the other copy, and the files left out.
async fn split(mut completed: CompletedDownload) -> Result<(CompletedDownload, Vec<PathBuf>)> {
    let files = completed.files.clone();
    let left_out = tokio::task::spawn_blocking(move || other_copy(&files))
        .await
        .map_err(|e| AppError::Internal(format!("M4B check task panicked: {e}")))?;
    completed.files.retain(|p| !left_out.contains(p));
    Ok((completed, left_out))
}

#[async_trait]
impl<I: Importer> Importer for PreferM4b<I> {
    async fn import(&self, completed: CompletedDownload) -> Result<ImportOutcome> {
        let (completed, left_out) = split(completed).await?;
        let mut outcome = self.inner.import(completed).await?;
        for p in left_out {
            tracing::info!(src = ?p, reason = SKIPPED, "import rejected");
            outcome.rejected.push((p, SKIPPED.to_string()));
        }
        Ok(outcome)
    }

    async fn preview(&self, completed: CompletedDownload) -> Result<ImportPlan> {
        let (completed, left_out) = split(completed).await?;
        let mut plan = self.inner.preview(completed).await?;
        plan.would_reject
            .extend(left_out.into_iter().map(|p| (p, SKIPPED.to_string())));
        Ok(plan)
    }
}

/// The audio in `files` to leave out because one M4B among them holds the same
/// book. Empty unless there is exactly one M4B, other audio beside it, and the
/// M4B is the same length as that audio. Blocking.
#[must_use]
pub fn other_copy(files: &[PathBuf]) -> Vec<PathBuf> {
    let audio = files.iter().filter(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        is_audio(p) && !is_excerpt(name)
    });
    let (m4bs, others): (Vec<&PathBuf>, Vec<&PathBuf>) = audio.partition(|p| is_m4b(p));
    let [m4b] = m4bs.as_slice() else {
        return Vec::new();
    };
    if others.is_empty() {
        return Vec::new();
    }
    let len = skadi_media_probe::audio_facts(m4b).map(|f| f.duration);
    let total = others
        .iter()
        .map(|p| {
            skadi_media_probe::audio_facts(p)
                .map(|f| f.duration)
                .filter(|d| !d.is_zero())
        })
        .sum::<Option<Duration>>();
    match (len, total) {
        (Some(len), Some(total)) if same_length(len, total) => {
            others.into_iter().cloned().collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use skadi_core::{ExternalIds, ProfileId, RootFolder};
    use skadi_downloaders::DownloadHandle;
    use skadi_importer::DefaultImporter;

    use super::*;
    use crate::book::Book;
    use crate::book_file::BookFile;
    use crate::convert::tests::{silent_mp3, working_ffmpeg};
    use crate::matcher::AudiobookMatcher;

    /// A silent AAC M4B of `secs` seconds made with ffmpeg.
    async fn silent_m4b(bin: &Path, path: &Path, secs: u32) {
        let made = tokio::process::Command::new(bin)
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
            .args([
                "-t",
                &secs.to_string(),
                "-c:a",
                "aac",
                "-b:a",
                "64k",
                "-f",
                "ipod",
            ])
            .arg(path)
            .status()
            .await
            .unwrap();
        assert!(made.success());
    }

    fn book(root: &Path) -> (Book, BookFile) {
        let mut book = Book::new(
            ExternalIds::default(),
            "The Long Road",
            ProfileId::new(),
            RootFolder::new(root.to_string_lossy().into_owned()),
        );
        book.authors = vec!["Robin Sample".into()];
        let file = BookFile::missing(book.id);
        (book, file)
    }

    fn completed(files: Vec<PathBuf>) -> CompletedDownload {
        CompletedDownload {
            handle: DownloadHandle {
                native_id: "dl-1".into(),
                category: "audiobooks".into(),
            },
            files,
            category: "audiobooks".into(),
        }
    }

    #[test]
    fn mp3s_alone_are_all_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("01.mp3");
        let b = tmp.path().join("02.mp3");
        silent_mp3(&a, 2);
        silent_mp3(&b, 2);
        assert!(other_copy(&[a, b]).is_empty());
    }

    #[tokio::test]
    async fn only_one_m4b_of_the_same_length_wins() {
        let Some(bin) = working_ffmpeg("only_one_m4b_of_the_same_length_wins").await else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("01.mp3");
        let b = tmp.path().join("02.mp3");
        silent_mp3(&a, 2);
        silent_mp3(&b, 2);
        let cover = tmp.path().join("cover.jpg");
        std::fs::write(&cover, b"jpg").unwrap();
        let same = tmp.path().join("book.m4b");
        silent_m4b(&bin, &same, 4).await;
        let files = vec![a.clone(), b.clone(), same.clone(), cover.clone()];
        assert_eq!(other_copy(&files), vec![a.clone(), b.clone()]);

        // Another length: not the same book, so nothing is left out.
        let long = tmp.path().join("long.m4b");
        silent_m4b(&bin, &long, 9).await;
        assert!(other_copy(&[a.clone(), b.clone(), long.clone()]).is_empty());

        // Two M4Bs: no single copy to prefer.
        assert!(other_copy(&[a, b, same, long]).is_empty());
    }

    /// The download that put an MP3 on the live editions: chapter MP3s and an
    /// M4B of the same length. Only the M4B reaches the library, under the
    /// single-file name, and the MP3s stay in the download.
    #[tokio::test]
    async fn a_download_with_both_imports_the_m4b() {
        let Some(bin) = working_ffmpeg("a_download_with_both_imports_the_m4b").await else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("audiobooks");
        std::fs::create_dir_all(&root).unwrap();
        let dl = tmp
            .path()
            .join("downloads")
            .join("Robin Sample - The Long Road");
        let a = dl.join("01 - The Long Road.mp3");
        let b = dl.join("02 - The Long Road.mp3");
        silent_mp3(&a, 2);
        silent_mp3(&b, 2);
        let m4b = dl.join("The Long Road.m4b");
        silent_m4b(&bin, &m4b, 4).await;

        let (book, file) = book(&root);
        let importer = PreferM4b::new(DefaultImporter::new(AudiobookMatcher::new(book, file)));
        let files = vec![a.clone(), b.clone(), m4b.clone()];

        let plan = importer.preview(completed(files.clone())).await.unwrap();
        assert_eq!(plan.would_import.len(), 1);
        assert_eq!(plan.would_reject.len(), 2);

        let outcome = importer.import(completed(files)).await.unwrap();
        assert_eq!(outcome.imported.len(), 1);
        let placed = &outcome.imported[0].file.path;
        assert_eq!(placed.extension().and_then(|e| e.to_str()), Some("m4b"));
        assert!(placed.starts_with(&root));
        assert!(placed.exists());
        let rejected: Vec<&PathBuf> = outcome.rejected.iter().map(|(p, _)| p).collect();
        assert_eq!(rejected, vec![&a, &b]);
        assert!(outcome.rejected.iter().all(|(_, why)| why == SKIPPED));
        // The download keeps every file, so it can keep seeding.
        assert!(a.exists() && b.exists() && m4b.exists());
    }
}
