//! Configuration backup and restore (SKADI-T-0463, component C35).
//!
//! Sonarr's System → Backups zips the database and config; the operator's only
//! option here used to be a Postgres dump by hand. This backs up the **control
//! plane** — the `config` table, every `settings` row, each domain's enabled
//! state, and the credentials in their sealed form — as a single JSON document.
//!
//! What it deliberately does not include is the library itself (movies, series,
//! books, downloads, history). Those are large, and they are reconstructible:
//! the files are on disk and the metadata comes back from the providers. The
//! irreplaceable part is the configuration — which indexers, which profiles,
//! which naming, which credentials — and that is what a restore needs to make a
//! fresh daemon behave like the old one. The vision already frames config-on-disk
//! as "a backup/seed/DR serialization format"; this is that format.
//!
//! **Credentials are never decrypted on the way out.** They are copied in the
//! sealed form the database holds, so a backup file is useless without
//! `SKADI_SECRET_KEY`. When the daemon runs with no key configured those bytes
//! are plaintext — the manifest says so per secret, and the create response warns.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use skadi_store::{
    ConfigRepo, ConfigSource, DomainState, DomainStateRepo, SealedSecret, SettingsRepo, Store,
};

use crate::error::ApiError;
use crate::{AppState, settings::SETTINGS_KINDS};

/// The backup format version. Bumped when the document's shape changes in a way
/// a previous restore could not read.
pub const FORMAT_VERSION: u32 = 1;

/// How many backups to keep when pruning. Old enough to cover "I broke it last
/// week", small enough that nobody has to think about disk.
pub const RETENTION: usize = 10;

/// One `config` row, as backed up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigRow {
    pub key: String,
    pub value: String,
    /// `env` rows are restored as `runtime`: an env-sourced value belongs to the
    /// deployment that set it, and writing it back as `env` would let a stale
    /// backup outrank the environment the operator is restoring into.
    pub source: String,
}

/// One `settings` row, as backed up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingRow {
    pub kind: String,
    pub id: String,
    pub body: serde_json::Value,
}

/// A domain's enabled state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainRow {
    pub name: String,
    pub enabled: bool,
    pub settings: serde_json::Value,
}

/// The backup document itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub format_version: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub skadi_version: String,
    pub config: Vec<ConfigRow>,
    pub settings: Vec<SettingRow>,
    pub domains: Vec<DomainRow>,
    pub secrets: Vec<SealedSecret>,
}

impl Backup {
    /// Whether any secret is stored unencrypted — true only when the daemon was
    /// running without `SKADI_SECRET_KEY`, and worth telling the operator.
    #[must_use]
    pub fn has_plaintext_secrets(&self) -> bool {
        self.secrets.iter().any(|s| !s.encrypted())
    }
}

/// What `GET /system/backup` lists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    /// The id used to restore it — the file's stem, or "latest".
    pub id: String,
    pub created_at: String,
    pub size_bytes: u64,
}

/// Read every backed-up table.
pub async fn capture(store: &Store) -> Result<Backup, ApiError> {
    let config = store
        .list_config()
        .await?
        .into_iter()
        .map(|c| ConfigRow {
            key: c.key,
            value: c.value,
            source: format!("{:?}", c.source).to_lowercase(),
        })
        .collect();

    let mut settings = Vec::new();
    for kind in SETTINGS_KINDS {
        for r in store.list_settings(kind).await? {
            settings.push(SettingRow {
                kind: r.kind,
                id: r.id,
                body: r.body,
            });
        }
    }

    let domains = DomainStateRepo::list(store)
        .await?
        .into_iter()
        .map(|d| DomainRow {
            name: d.name,
            enabled: d.enabled,
            settings: d.settings,
        })
        .collect();

    Ok(Backup {
        format_version: FORMAT_VERSION,
        created_at: chrono::Utc::now(),
        skadi_version: env!("CARGO_PKG_VERSION").to_string(),
        config,
        settings,
        domains,
        secrets: store.export_sealed_secrets().await?,
    })
}

/// Apply a backup over the live configuration.
///
/// Additive: rows in the backup are written, rows only in the database are left
/// alone. Deleting what is not in the backup would make a restore destructive in
/// a way the operator did not ask for — and a half-matching restore that removes
/// a working indexer is worse than one that leaves it.
pub async fn apply(store: &Store, backup: &Backup) -> Result<(), ApiError> {
    if backup.format_version > FORMAT_VERSION {
        return Err(ApiError(skadi_core::AppError::Validation(format!(
            "backup format version {} is newer than this daemon understands ({FORMAT_VERSION})",
            backup.format_version
        ))));
    }
    for c in &backup.config {
        // See `ConfigRow::source`: env-sourced values come back as runtime.
        store
            .set_config(&c.key, &c.value, ConfigSource::Runtime)
            .await?;
    }
    for s in &backup.settings {
        store.put_setting(&s.kind, &s.id, &s.body).await?;
    }
    for d in &backup.domains {
        store
            .upsert(&DomainState {
                name: d.name.clone(),
                enabled: d.enabled,
                enabled_at: None,
                settings: d.settings.clone(),
            })
            .await?;
    }
    for secret in &backup.secrets {
        store.import_sealed_secret(secret).await?;
    }
    Ok(())
}

/// Where backups live: the `backup.dir` config key, else `/data/backups`
/// (matching the `/data/definitions` convention).
pub async fn backup_dir(store: &Store) -> PathBuf {
    match store.get_config("backup.dir").await {
        Ok(Some(entry)) if !entry.value.trim().is_empty() => PathBuf::from(entry.value),
        _ => PathBuf::from("/data/backups"),
    }
}

/// Write a backup into `dir`, prune to [`RETENTION`], and return its path.
pub async fn write_backup(store: &Store, dir: &Path) -> Result<PathBuf, ApiError> {
    let backup = capture(store).await?;
    let name = format!("skadi-{}.json", backup.created_at.format("%Y%m%dT%H%M%SZ"));
    let path = dir.join(name);
    let body = serde_json::to_vec_pretty(&backup)
        .map_err(|e| ApiError(skadi_core::AppError::Internal(format!("serialize: {e}"))))?;
    let dir_owned = dir.to_path_buf();
    let write_path = path.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir_owned)?;
        std::fs::write(&write_path, body)
    })
    .await
    .map_err(|e| ApiError(skadi_core::AppError::Internal(format!("task: {e}"))))?
    .map_err(|e| {
        ApiError(skadi_core::AppError::Internal(format!(
            "writing backup: {e}"
        )))
    })?;
    prune(dir, RETENTION).await;
    Ok(path)
}

/// Backups in `dir`, newest first.
pub async fn list(dir: &Path) -> Vec<BackupInfo> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut out: Vec<BackupInfo> = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // No directory yet simply means no backups — not an error.
            return out;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let created = meta
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            out.push(BackupInfo {
                id: path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                created_at: created,
                size_bytes: meta.len(),
            });
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out
    })
    .await
    .unwrap_or_default()
}

/// Delete all but the newest `keep` backups.
pub async fn prune(dir: &Path, keep: usize) {
    let existing = list(dir).await;
    let dir = dir.to_path_buf();
    let doomed: Vec<String> = existing.into_iter().skip(keep).map(|b| b.id).collect();
    if doomed.is_empty() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || {
        for id in doomed {
            let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
        }
    })
    .await;
}

/// Read one backup by id, or the newest when `id` is `latest`.
pub async fn read(dir: &Path, id: &str) -> Result<Backup, ApiError> {
    let id = if id == "latest" {
        list(dir)
            .await
            .first()
            .map(|b| b.id.clone())
            .ok_or_else(|| ApiError(skadi_core::AppError::NotFound("no backups exist".into())))?
    } else {
        // The id is a file stem; refuse anything that could escape the directory.
        if id.contains('/') || id.contains("..") {
            return Err(ApiError(skadi_core::AppError::Validation(
                "backup id must be a plain file name".into(),
            )));
        }
        id.to_string()
    };
    let path = dir.join(format!("{id}.json"));
    let body = tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .map_err(|e| ApiError(skadi_core::AppError::Internal(format!("task: {e}"))))?
        .map_err(|_| ApiError(skadi_core::AppError::NotFound(format!("backup {id:?}"))))?;
    serde_json::from_slice(&body).map_err(|e| {
        ApiError(skadi_core::AppError::Validation(format!(
            "backup is not readable: {e}"
        )))
    })
}

/// Smallest scheduled interval, so a misconfigured value cannot spin the disk.
pub const MIN_BACKUP_INTERVAL_SECS: u64 = 3_600;

/// Take a backup on a schedule (SKADI-T-0463), the scheduled half of what Sonarr
/// offers alongside the manual button.
///
/// Off unless `backup.interval_secs` is set, because writing files on a timer is
/// not something to start doing to an operator's disk without being asked. The
/// interval and the destination are re-read each tick, so a settings change
/// applies without a restart. Shares the daemon's shutdown token.
pub async fn backup_loop(store: Store, cancel: tokio_util::sync::CancellationToken) {
    let mut ticker =
        tokio::time::interval(std::time::Duration::from_secs(MIN_BACKUP_INTERVAL_SECS));
    // The immediate first tick would back up at every daemon start; skip it.
    ticker.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let Some(interval) = configured_interval(&store).await else {
                    continue;
                };
                // A coarse schedule on a fixed ticker: back up when the newest
                // backup is older than the configured interval. Simpler than
                // rebuilding the ticker, and it survives a restart because the
                // decision is made from what is on disk, not from uptime.
                let dir = backup_dir(&store).await;
                if !due(&dir, interval).await {
                    continue;
                }
                match write_backup(&store, &dir).await {
                    Ok(path) => tracing::info!(path = %path.display(), "scheduled backup written"),
                    Err(e) => tracing::warn!(error = ?e, "scheduled backup failed"),
                }
            }
        }
    }
}

/// The configured backup interval, or `None` when scheduled backups are off.
async fn configured_interval(store: &Store) -> Option<u64> {
    let raw = store.get_config("backup.interval_secs").await.ok()??;
    let secs: u64 = raw.value.trim().parse().ok()?;
    (secs > 0).then(|| secs.max(MIN_BACKUP_INTERVAL_SECS))
}

/// Whether the newest backup in `dir` is older than `interval`.
async fn due(dir: &Path, interval: u64) -> bool {
    let Some(newest) = list(dir).await.into_iter().next() else {
        return true; // none yet
    };
    match chrono::DateTime::parse_from_rfc3339(&newest.created_at) {
        Ok(t) => {
            let age = chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc));
            age.num_seconds() >= interval as i64
        }
        Err(_) => true,
    }
}

pub fn backup_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/system/backup", get(list_backups).post(create_backup))
        .route("/system/backup/restore/{id}", post(restore_backup))
}

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state.store.as_ref().ok_or_else(|| {
        ApiError(skadi_core::AppError::Internal(
            "store not configured".into(),
        ))
    })
}

async fn list_backups(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let dir = backup_dir(store(&state)?).await;
    Ok(Json(list(&dir).await))
}

async fn create_backup(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let dir = backup_dir(store).await;
    let path = write_backup(store, &dir).await?;
    let backup = capture(store).await?;
    let mut body = serde_json::json!({
        "id": path.file_stem().and_then(|s| s.to_str()).unwrap_or_default(),
        "path": path.display().to_string(),
        "settings": backup.settings.len(),
        "secrets": backup.secrets.len(),
    });
    if backup.has_plaintext_secrets() {
        body["warning"] = serde_json::json!(
            "SKADI_SECRET_KEY is not set, so credentials are stored unencrypted and \
             this backup file contains them in the clear — protect it accordingly"
        );
    }
    Ok((StatusCode::ACCEPTED, Json(body)))
}

async fn restore_backup(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let dir = backup_dir(store).await;
    let backup = read(&dir, &id).await?;
    apply(store, &backup).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "restored": id,
            "created_at": backup.created_at,
            "settings": backup.settings.len(),
            "secrets": backup.secrets.len(),
            // The supervisor's next tick rebuilds providers from the restored
            // settings, so there is nothing for the operator to restart.
            "note": "providers reload on the next supervisor tick",
        })),
    ))
}

/// Expire recycle-bin entries on a schedule (SKADI-T-0418).
///
/// Off unless both `import.recycle_bin` and a non-zero
/// `import.recycle_retention_days` are set — the bin's whole purpose is to hold
/// an operator's only remaining copy of a superseded file, so nothing here
/// deletes anything on a default they never chose. Both are re-read each tick, so
/// a settings change applies without a restart. Shares the daemon's shutdown
/// token.
pub async fn recycle_sweep_loop(store: Store, cancel: tokio_util::sync::CancellationToken) {
    // Hourly: the retention floor is a day, so anything finer is wasted wake-ups.
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
    ticker.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let Some((bin, days)) = recycle_settings(&store).await else {
                    continue;
                };
                // `read_dir` + `remove_file` over a library mount: blocking, and
                // must never tie up an async worker (SKADI-T-0081).
                let removed = tokio::task::spawn_blocking(move || {
                    skadi_importer::sweep_recycle_bin(&bin, days)
                })
                .await
                .unwrap_or_default();
                if !removed.is_empty() {
                    tracing::info!(count = removed.len(), "recycle bin swept");
                }
            }
        }
    }
}

/// The configured bin and retention, or `None` when the sweep is off.
async fn recycle_settings(store: &Store) -> Option<(std::path::PathBuf, u64)> {
    let bin = store.get_config("import.recycle_bin").await.ok()??.value;
    let bin = bin.trim();
    if bin.is_empty() {
        return None;
    }
    let days: u64 = store
        .get_config("import.recycle_retention_days")
        .await
        .ok()??
        .value
        .trim()
        .parse()
        .ok()?;
    (days > 0).then(|| (std::path::PathBuf::from(bin), days))
}
