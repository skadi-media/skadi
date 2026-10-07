//! The built-in DB-queue downloader (SKADI-I-0013 / T-0087).
//!
//! [`DbDownloader`] implements the [`Downloader`](crate::Downloader) trait over
//! the `downloads` queue ([`DownloadJobRepo`](skadi_store::DownloadJobRepo)): it
//! `add`s by **enqueuing** a row and reports `status`/`remove` by reading and
//! flagging that row. The actual torrenting is done by the VPN-isolated
//! `skadi-downloader-worker`, which claims the row and writes progress back.
//!
//! It speaks to no network endpoint and holds no secret — it is the daemon's
//! only downloader, constructed by the provider factory with the store handle.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use skadi_core::{AppError, DownloaderId, Protocol, Result};
use skadi_indexers::{Category, Release, ReleaseFetch};
use skadi_store::{DownloadJobRepo, DownloadJobStatus, NewDownloadJob};

use crate::{DownloadHandle, DownloadStatus, Downloader};

/// A downloader backed by the `downloads` queue table.
pub struct DbDownloader {
    id: DownloaderId,
    repo: Arc<dyn DownloadJobRepo>,
    /// Where the worker should download + seeds (written into each job row).
    incomplete_dir: String,
    /// Where the worker hardlinks finished files (written into each job row).
    complete_dir: String,
}

impl DbDownloader {
    /// Build over the queue repo (the daemon passes its `Store`) with the
    /// configured download paths from the skadi downloader settings.
    #[must_use]
    pub fn new(
        id: DownloaderId,
        repo: Arc<dyn DownloadJobRepo>,
        incomplete_dir: String,
        complete_dir: String,
    ) -> Self {
        Self {
            id,
            repo,
            incomplete_dir,
            complete_dir,
        }
    }
}

/// The torrent source string for a release, or an error for unsupported kinds.
fn release_source(release: &Release) -> Result<String> {
    match &release.fetch {
        ReleaseFetch::Magnet(m) => Ok(m.clone()),
        ReleaseFetch::TorrentUrl(u) => Ok(u.clone()),
        ReleaseFetch::NzbUrl(_) => Err(AppError::Validation(
            "skadi downloader cannot handle an NZB release (torrents only)".into(),
        )),
    }
}

/// Fraction complete in `0.0..=1.0` (0.0 when the total isn't known yet).
fn progress_fraction(done: i64, total: i64) -> f32 {
    if total <= 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

#[async_trait]
impl Downloader for DbDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }

    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    /// Reachability check: a cheap queue round-trip (a get on a sentinel id
    /// returns `None`, proving the database answers).
    async fn test(&self) -> Result<()> {
        self.repo
            .get_download("__skadi_connectivity_check__")
            .await
            .map(|_| ())
    }

    async fn add(&self, release: &Release, category: &Category) -> Result<DownloadHandle> {
        let source = release_source(release)?;
        let category_name = category.0.to_string();
        let job = self
            .repo
            .enqueue(&NewDownloadJob {
                acquirable_ref: release.title.clone(),
                source,
                category: Some(category_name.clone()),
                incomplete_dir: Some(self.incomplete_dir.clone()),
                complete_dir: Some(self.complete_dir.clone()),
            })
            .await?;
        // The queue row id is the handle the importer later resolves.
        Ok(DownloadHandle {
            native_id: job.id,
            category: category_name,
        })
    }

    async fn status(&self, handle: &DownloadHandle) -> Result<DownloadStatus> {
        let job = self
            .repo
            .get_download(&handle.native_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("no download job {}", handle.native_id)))?;
        Ok(match job.status {
            DownloadJobStatus::Queued => DownloadStatus::Queued,
            // A paused transfer is still in-flight from the hunter's view (it keeps
            // monitoring); the operator can resume it (SKADI-T-0168). A `stalled`
            // transfer is likewise still in-flight — just not making progress
            // (SKADI-T-0213).
            // A transfer the client has not made live yet — it is queued behind
            // the session's hash/init work, or still fetching magnet metadata — is
            // reported as `Queued`, not as `Downloading { progress: 0.0 }`
            // (SKADI-T-0394). The distinction is what stops the hunter's stall
            // watch from failing a release for sitting at 0% while it simply waits
            // its turn: only a live transfer can stall. `client_state` is `None`
            // for older rows and non-skadi clients, which keep the old behaviour.
            DownloadJobStatus::Downloading
            | DownloadJobStatus::Paused
            | DownloadJobStatus::Stalled
                if job
                    .client_state
                    .as_deref()
                    .is_some_and(|s| s == "initializing") =>
            {
                DownloadStatus::Queued
            }
            DownloadJobStatus::Downloading
            | DownloadJobStatus::Paused
            | DownloadJobStatus::Stalled => DownloadStatus::Downloading {
                progress: progress_fraction(job.progress_bytes, job.total_bytes),
            },
            // `Seeded` (seed limit reached, seeding stopped) is still a completed
            // download from the hunter's view — the files are on disk.
            DownloadJobStatus::Completed | DownloadJobStatus::Seeded => DownloadStatus::Completed {
                files: job.files.into_iter().map(PathBuf::from).collect(),
            },
            DownloadJobStatus::Error => DownloadStatus::Failed {
                reason: job.error.unwrap_or_else(|| "download error".into()),
            },
            // Teardown states: someone asked to remove the job. The hunter polls
            // only a transfer it still watches, so seeing one here means the
            // operator removed it under a live run (SKADI-T-0691). Terminal, but
            // not the release's fault, so it is told apart from `Failed`.
            DownloadJobStatus::RemoveRequested | DownloadJobStatus::Removed => {
                DownloadStatus::Removed
            }
        })
    }

    async fn remove(&self, handle: &DownloadHandle, delete_data: bool) -> Result<()> {
        self.repo
            .request_remove(&handle.native_id, delete_data)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use skadi_core::IndexerId;
    use skadi_quality::ParsedRelease;
    use skadi_store::{DownloadProgress, Store};

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("dbdl").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    fn magnet_release(title: &str) -> Release {
        Release {
            indexer: IndexerId::new(),
            title: title.into(),
            fetch: ReleaseFetch::Magnet("magnet:?xt=urn:btih:abc".into()),
            size: 1_000,
            published: Utc::now(),
            seeders: Some(10),
            categories: Vec::new(),
            parsed: ParsedRelease::default(),
        }
    }

    #[tokio::test]
    async fn add_enqueues_and_status_tracks_the_row() {
        let store = temp_store().await;
        let dl = DbDownloader::new(
            DownloaderId::new(),
            Arc::new(store.clone()),
            "/mnt/storage/skadi-torrent/incomplete".into(),
            "/mnt/storage/skadi-torrent/complete".into(),
        );

        // add → a queued row; the handle is the row id.
        let handle = dl
            .add(&magnet_release("The Matrix"), &Category(2000))
            .await
            .unwrap();
        assert!(!handle.native_id.is_empty());
        assert_eq!(handle.category, "2000");
        assert!(matches!(
            dl.status(&handle).await.unwrap(),
            DownloadStatus::Queued
        ));
        // The configured download dirs are written into the job row.
        let row = store
            .get_download(&handle.native_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.incomplete_dir.as_deref(),
            Some("/mnt/storage/skadi-torrent/incomplete")
        );
        assert_eq!(
            row.complete_dir.as_deref(),
            Some("/mnt/storage/skadi-torrent/complete")
        );

        // Simulate the worker: claim, report progress.
        store.claim_next("w1").await.unwrap().unwrap();
        store
            .update_progress(
                &handle.native_id,
                &DownloadProgress {
                    progress_bytes: 250,
                    total_bytes: 1000,
                    info_hash: Some("abc".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        match dl.status(&handle).await.unwrap() {
            DownloadStatus::Downloading { progress } => {
                assert!((progress - 0.25).abs() < 1e-6, "progress {progress}");
            }
            other => panic!("expected downloading, got {other:?}"),
        }

        // Worker completes with files.
        store
            .mark_complete(&handle.native_id, &["/data/downloads/m/movie.mkv".into()])
            .await
            .unwrap();
        match dl.status(&handle).await.unwrap() {
            DownloadStatus::Completed { files } => {
                assert_eq!(files, vec![PathBuf::from("/data/downloads/m/movie.mkv")]);
            }
            other => panic!("expected completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_flags_the_row_and_status_reflects_error() {
        let store = temp_store().await;
        let dl = DbDownloader::new(
            DownloaderId::new(),
            Arc::new(store.clone()),
            "/mnt/storage/skadi-torrent/incomplete".into(),
            "/mnt/storage/skadi-torrent/complete".into(),
        );
        let handle = dl
            .add(&magnet_release("Dune"), &Category(2000))
            .await
            .unwrap();

        dl.remove(&handle, true).await.unwrap();
        // The row is now remove_requested with the delete flag set.
        let pending = store.list_remove_requested().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, handle.native_id);
        assert!(pending[0].delete_data);
        // Status surfaces teardown as `Removed`, not as a failure of the
        // release (SKADI-T-0691).
        assert_eq!(dl.status(&handle).await.unwrap(), DownloadStatus::Removed);
    }

    #[tokio::test]
    async fn status_of_unknown_handle_is_not_found() {
        let store = temp_store().await;
        let dl = DbDownloader::new(
            DownloaderId::new(),
            Arc::new(store),
            "/mnt/storage/skadi-torrent/incomplete".into(),
            "/mnt/storage/skadi-torrent/complete".into(),
        );
        let handle = DownloadHandle {
            native_id: "nope".into(),
            category: "2000".into(),
        };
        assert!(matches!(
            dl.status(&handle).await,
            Err(AppError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn nzb_release_is_rejected() {
        let store = temp_store().await;
        let dl = DbDownloader::new(
            DownloaderId::new(),
            Arc::new(store),
            "/mnt/storage/skadi-torrent/incomplete".into(),
            "/mnt/storage/skadi-torrent/complete".into(),
        );
        let mut release = magnet_release("x");
        release.fetch = ReleaseFetch::NzbUrl("http://x/y.nzb".into());
        assert!(matches!(
            dl.add(&release, &Category(2000)).await,
            Err(AppError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn test_round_trips_the_queue() {
        let store = temp_store().await;
        let dl = DbDownloader::new(
            DownloaderId::new(),
            Arc::new(store),
            "/mnt/storage/skadi-torrent/incomplete".into(),
            "/mnt/storage/skadi-torrent/complete".into(),
        );
        dl.test().await.unwrap();
    }
}
