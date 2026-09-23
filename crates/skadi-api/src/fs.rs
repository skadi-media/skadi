//! Server-side folder browser (SKADI-T-0149): a read-only directory listing
//! under a fixed set of allowed roots, so the web UI can offer a **folder picker**
//! for path inputs (root folders, library import) instead of free-text — without
//! exposing the whole container filesystem.
//!
//! Roots come from `SKADI_BROWSE_ROOTS` (`:`-separated), defaulting to the deploy
//! media mounts `/mnt/storage:/library`. Only roots that actually exist are
//! offered, and a browse target must canonicalize to within one of them, so
//! `..`/symlink tricks can't escape the mounts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};

use skadi_core::AppError;

use crate::error::ApiError;
use crate::state::AppState;

/// Default browse roots when `SKADI_BROWSE_ROOTS` is unset — the deploy's media
/// mounts. Overridable so a non-deploy run can point elsewhere.
const DEFAULT_BROWSE_ROOTS: &str = "/mnt/storage:/library";

/// The configured browse roots that actually exist on disk (canonicalized).
fn browse_roots() -> Vec<PathBuf> {
    std::env::var("SKADI_BROWSE_ROOTS")
        .unwrap_or_else(|_| DEFAULT_BROWSE_ROOTS.to_string())
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::fs::canonicalize(s).ok())
        .collect()
}

/// Whether `path` is at or under one of `roots` (component-wise, so
/// `/mnt/storage2` is NOT under `/mnt/storage`).
fn within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| path == r || path.starts_with(r))
}

#[derive(Serialize)]
struct FsEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
struct FsListing {
    /// The directory listed (canonical).
    path: String,
    /// Parent dir, or `None` when `path` is itself a root (don't climb above the
    /// mounts).
    parent: Option<String>,
    /// Immediate subdirectories, sorted case-insensitively by name.
    entries: Vec<FsEntry>,
}

#[derive(Deserialize)]
struct BrowseParams {
    path: Option<String>,
}

pub fn fs_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/fs/roots", get(fs_roots))
        .route("/fs/browse", get(fs_browse))
}

/// `GET /fs/roots` — the existing browse roots (folder-picker starting points).
async fn fs_roots() -> impl IntoResponse {
    let roots: Vec<String> = browse_roots()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Json(roots)
}

/// `GET /fs/browse?path=` — list subdirectories of `path` (defaults to the first
/// root). 400 when `path` escapes the allowed roots or isn't a directory.
async fn fs_browse(Query(q): Query<BrowseParams>) -> Result<impl IntoResponse, ApiError> {
    let roots = browse_roots();
    if roots.is_empty() {
        // An empty listing, not a 500 (SKADI-T-0460). "Nothing to browse" is a
        // normal client-visible condition — the defaults (`/mnt/storage`,
        // `/library`) only exist inside the container, so every developer running
        // the daemon on a laptop hit an `internal` error on a perfectly healthy
        // system. `internal` should mean the daemon is broken, and it is not.
        return Ok(Json(FsListing {
            path: String::new(),
            parent: None,
            entries: Vec::new(),
        }));
    }
    let requested = q.path.filter(|s| !s.trim().is_empty());
    let listing = tokio::task::spawn_blocking(move || browse(requested, &roots))
        .await
        .map_err(|e| ApiError(AppError::Internal(format!("browse task: {e}"))))??;
    Ok(Json(listing))
}

/// List the subdirectories of `requested` (or the first root), refusing anything
/// outside the allowed roots. Blocking fs — call under `spawn_blocking`.
fn browse(requested: Option<String>, roots: &[PathBuf]) -> Result<FsListing, AppError> {
    let target = match requested {
        Some(p) => {
            std::fs::canonicalize(&p).map_err(|e| AppError::Validation(format!("path {p}: {e}")))?
        }
        None => roots[0].clone(),
    };
    if !within_roots(&target, roots) {
        return Err(AppError::Validation(
            "path is outside the allowed media roots".into(),
        ));
    }
    if !target.is_dir() {
        return Err(AppError::Validation("not a directory".into()));
    }
    // Offer a parent only when the target isn't itself a root.
    let parent = if roots.iter().any(|r| r == &target) {
        None
    } else {
        target
            .parent()
            .filter(|p| within_roots(p, roots))
            .map(|p| p.to_string_lossy().into_owned())
    };
    let mut entries: Vec<FsEntry> = std::fs::read_dir(&target)
        .map_err(|e| AppError::Internal(format!("read_dir {}: {e}", target.display())))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| FsEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path().to_string_lossy().into_owned(),
        })
        .filter(|e| !e.name.starts_with('.')) // hide dotdirs
        .collect();
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(FsListing {
        path: target.to_string_lossy().into_owned(),
        parent,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_roots_is_component_wise() {
        let roots = vec![PathBuf::from("/mnt/storage")];
        assert!(within_roots(Path::new("/mnt/storage"), &roots));
        assert!(within_roots(Path::new("/mnt/storage/movies"), &roots));
        assert!(!within_roots(Path::new("/mnt/storage2"), &roots));
        assert!(!within_roots(Path::new("/mnt"), &roots));
        assert!(!within_roots(Path::new("/etc/passwd"), &roots));
    }

    #[test]
    fn browse_lists_subdirs_and_refuses_escape() {
        // A throwaway tree: <tmp>/skadi-fs-test/{a,b,.hidden}, file c.txt.
        let base = skadi_core::unique_temp_path("fs-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::create_dir_all(base.join("b")).unwrap();
        std::fs::create_dir_all(base.join(".hidden")).unwrap();
        std::fs::write(base.join("c.txt"), b"x").unwrap();
        let roots = vec![std::fs::canonicalize(&base).unwrap()];

        let listing = browse(None, &roots).unwrap();
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"], // dirs only, dotdirs + files excluded, sorted
        );
        assert!(listing.parent.is_none(), "at a root → no parent");

        // Drilling into a subdir exposes a parent back to the root.
        let sub = browse(Some(base.join("a").to_string_lossy().into_owned()), &roots).unwrap();
        assert!(sub.parent.is_some());

        // Escaping the root is refused.
        assert!(browse(Some("/etc".into()), &roots).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}
