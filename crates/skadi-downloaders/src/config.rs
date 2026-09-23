//! Typed downloader configuration (SKADI-T-0058).
//!
//! [`DownloaderConfig`] is the exact non-secret shape stored in the daemon's
//! settings `body` for `kind = "downloaders"` rows. The password is **not**
//! part of this config — it lives in the credential store and is passed to
//! [`DownloaderConfig::build`].

use serde::{Deserialize, Serialize};

use skadi_core::{AppError, DownloaderId, Result};

use crate::Downloader;

/// Non-secret configuration for one downloader, as stored in settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloaderConfig {
    /// The built-in DB-queue downloader (SKADI-I-0013): downloads run in the
    /// VPN-isolated `skadi-downloader-worker`, driven through the `downloads`
    /// table. No endpoint, no secret — just a name.
    Skadi(SkadiConfig),
}

/// Configuration for the built-in [`DbDownloader`](crate::DbDownloader).
///
/// The two directories are absolute paths under the shared storage mount
/// (`/mnt/storage`), namespaced so Skadi never collides with another download
/// client. The worker downloads + seeds in `incomplete_dir` and hardlinks
/// finished files into `complete_dir`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkadiConfig {
    /// Display name.
    pub name: String,
    /// Where the worker downloads + seeds (librqbit output folder).
    #[serde(default = "default_incomplete_dir")]
    pub incomplete_dir: String,
    /// Where finished files are hardlinked on completion (the clean view).
    #[serde(default = "default_complete_dir")]
    pub complete_dir: String,
}

// Generic defaults under the single library root convention (SKADI-I-0045,
// `library.root` defaults to `/data`). Bootstrap overrides these with paths
// derived from the live `library.root` when it seeds the built-in downloader, so
// the deploy's real mount is used.
fn default_incomplete_dir() -> String {
    "/data/downloads/incomplete".to_string()
}

fn default_complete_dir() -> String {
    "/data/downloads/complete".to_string()
}

impl DownloaderConfig {
    /// Whether this is the built-in DB-queue downloader. The provider factory
    /// constructs it directly (it needs the store handle, not an endpoint or
    /// secret), so [`build`](Self::build) is not used for it.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(self, DownloaderConfig::Skadi(_))
    }

    /// The display name, regardless of variant.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            DownloaderConfig::Skadi(c) => &c.name,
        }
    }

    /// For the built-in skadi downloader, its `(incomplete_dir, complete_dir)`;
    /// `None` for other kinds. The provider factory uses this to construct the
    /// [`DbDownloader`](crate::DbDownloader).
    #[must_use]
    pub fn skadi_dirs(&self) -> Option<(String, String)> {
        match self {
            DownloaderConfig::Skadi(c) => Some((c.incomplete_dir.clone(), c.complete_dir.clone())),
        }
    }

    /// Validate and build a live downloader from an endpoint and secret.
    ///
    /// Every supported downloader is now the built-in DB-queue one, which the
    /// provider factory constructs from the store handle rather than from a
    /// config + password (SKADI-T-0517 removed qBittorrent, the only kind that
    /// took an endpoint). The method is kept so the settings layer has one place
    /// to reject a row that claims to need building, and so re-introducing an
    /// endpoint-based client later does not change the call sites.
    pub fn build(self, _id: DownloaderId, _password: String) -> Result<Box<dyn Downloader>> {
        match self {
            DownloaderConfig::Skadi(cfg) => Err(AppError::Validation(format!(
                "skadi downloader {:?} is built-in: construct it via the provider factory, not build()",
                cfg.name
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_qbittorrent_row_is_rejected_not_panicked_on() {
        // SKADI-T-0517: qBittorrent is gone, but a database written before the
        // removal can still hold a `kind: "qbittorrent"` row. Deserialising it
        // must fail cleanly — the settings loader turns that into a warn-and-skip
        // rather than taking the daemon down.
        let json = serde_json::json!({
            "kind": "qbittorrent",
            "name": "main",
            "base_url": "http://localhost:8080",
            "username": "admin"
        });
        let parsed: std::result::Result<DownloaderConfig, _> = serde_json::from_value(json);
        assert!(parsed.is_err(), "unknown kind must not deserialize");
    }

    #[test]
    fn skadi_kind_defaults_paths_and_is_builtin() {
        // Minimal body: the download dirs default.
        let json = serde_json::json!({ "kind": "skadi", "name": "built-in" });
        let cfg: DownloaderConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.is_builtin());
        assert_eq!(cfg.name(), "built-in");
        assert_eq!(
            cfg.skadi_dirs(),
            Some((
                "/data/downloads/incomplete".to_string(),
                "/data/downloads/complete".to_string(),
            ))
        );
        // build() is not the construction path for the built-in downloader.
        assert!(matches!(
            cfg.build(DownloaderId::new(), String::new()),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn skadi_kind_round_trips_explicit_paths() {
        let cfg = DownloaderConfig::Skadi(SkadiConfig {
            name: "built-in".into(),
            incomplete_dir: "/mnt/storage/skadi-torrent/incomplete".into(),
            complete_dir: "/mnt/storage/skadi-torrent/complete".into(),
        });
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["kind"], "skadi");
        let back: DownloaderConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }
}
