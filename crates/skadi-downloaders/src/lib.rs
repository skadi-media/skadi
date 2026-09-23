//! `skadi-downloaders` — the downloader abstraction.
//!
//! Defines the [`Downloader`] trait and its common [`DownloadHandle`] /
//! [`DownloadStatus`] types and the built-in [`db::DbDownloader`] that drives
//! the `downloads` queue (SKADI-I-0013), which is the only client: the
//! VPN-isolated `skadi-downloader-worker` performs every transfer.
//! Torrent-only for v0; a SABnzbd (Usenet) client is the deferred sibling
//! behind the same trait.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use skadi_core::{DownloaderId, Protocol, Result};
use skadi_indexers::{Category, Release};

pub mod config;
pub mod db;
pub use config::{DownloaderConfig, SkadiConfig};
pub use db::DbDownloader;

/// A handle to a transfer in progress, as known to a specific downloader.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct DownloadHandle {
    /// The downloader-native id (e.g. a torrent infohash).
    pub native_id: String,
    /// The category the transfer was filed under (lets the importer find it).
    pub category: String,
}

/// Backend-agnostic transfer state.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum DownloadStatus {
    Queued,
    Downloading { progress: f32 },
    Completed { files: Vec<PathBuf> },
    Failed { reason: String },
}

/// A configured download client.
#[async_trait]
pub trait Downloader: Send + Sync {
    fn id(&self) -> DownloaderId;
    fn protocol(&self) -> Protocol;
    /// Hand a located release to the client, filed under `category`.
    async fn add(&self, release: &Release, category: &Category) -> Result<DownloadHandle>;
    /// Current state of a transfer.
    async fn status(&self, handle: &DownloadHandle) -> Result<DownloadStatus>;
    /// Remove a transfer, optionally deleting its data.
    async fn remove(&self, handle: &DownloadHandle, delete_data: bool) -> Result<()>;
    /// Verify reachability + credentials (SKADI-T-0061).
    ///
    /// **Required, no default** — every implementor (mocks included) declares
    /// its own. Real clients make a cheap authenticated call (e.g. login).
    async fn test(&self) -> Result<()>;
}
