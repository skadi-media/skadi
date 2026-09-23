//! Config-plane read/write over HTTP (SKADI-T-0535).
//!
//! Every Tier1 key was previously env-only: changing one meant editing
//! `deploy/.env` and restarting the stack, even though the values live in the
//! `config` table and several are re-read on a tick. This exposes the registry so
//! a client can list what exists, see what it is set to and where that came from,
//! and change it.
//!
//! Writes go through [`skadi_config::validate_write`] and
//! [`skadi_config::validate_cross_key`] (SKADI-T-0524) rather than re-deriving
//! validation here — one definition of what a valid value is.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use skadi_core::AppError;
use skadi_store::{ConfigRepo, ConfigSource, Store};

use crate::AppState;
use crate::error::{ApiError, ApiJson};

/// Keys whose value is never returned (SKADI-T-0535).
///
/// The API token and the credential-vault key are how the daemon is protected
/// and how stored credentials stay sealed; echoing either through the very
/// endpoint they guard would make reading one enough to take the other. Their
/// *presence* is still reported, because "is a secret key configured?" is a
/// question an operator needs answered.
const REDACTED: &[&str] = &["api_token", "secret_key"];

fn is_redacted(key: &str) -> bool {
    REDACTED.contains(&key)
}

/// One config key as the API reports it.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigKeyDto {
    key: &'static str,
    /// `string` | `bool` | `u16` | `u64` | `path`.
    kind: &'static str,
    /// The registry default, so a client can show what "unset" means.
    default: &'static str,
    /// The value in force. `None` for a redacted key.
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    /// Whether a value is stored at all — the only signal a client gets for a
    /// redacted key.
    is_set: bool,
    /// Whether the value is withheld rather than absent.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    redacted: bool,
    /// `env` or `runtime`, when a row exists. An **env-sourced** value is
    /// overwritten on the next boot by the env seeder, so a runtime write to it
    /// will not survive a restart — a client must be able to say so.
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// Tier0 keys (`database_url`) are read before the table exists and cannot be
    /// changed here at all.
    editable: bool,
}

fn kind_str(k: skadi_config::ValueKind) -> &'static str {
    match k {
        skadi_config::ValueKind::String => "string",
        skadi_config::ValueKind::Bool => "bool",
        skadi_config::ValueKind::U16 => "u16",
        skadi_config::ValueKind::U64 => "u64",
        skadi_config::ValueKind::Path => "path",
    }
}

pub fn config_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/config", get(list))
        .route("/config/{key}", get(get_one).put(set_one).delete(reset))
}

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

async fn dto(store: &Store, spec: &'static skadi_config::ConfigKeySpec) -> ConfigKeyDto {
    let row = store.get_config(spec.key).await.ok().flatten();
    let redacted = is_redacted(spec.key);
    ConfigKeyDto {
        key: spec.key,
        kind: kind_str(spec.kind),
        default: spec.default,
        value: if redacted {
            None
        } else {
            Some(
                row.as_ref()
                    .map_or_else(|| spec.default.to_string(), |e| e.value.clone()),
            )
        },
        is_set: row.is_some(),
        redacted,
        source: row.as_ref().map(|e| e.source.as_str().to_string()),
        editable: spec.tier == skadi_config::Tier::Tier1,
    }
}

/// `GET /config` — the whole registry with current values.
async fn list(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let mut out = Vec::with_capacity(skadi_config::REGISTRY.len());
    for spec in skadi_config::REGISTRY {
        out.push(dto(store, spec).await);
    }
    Ok(Json(out))
}

/// `GET /config/{key}` — one key.
async fn get_one(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let spec = skadi_config::spec(&key)
        .ok_or_else(|| ApiError(AppError::NotFound(format!("no such config key: {key}"))))?;
    Ok(Json(dto(store, spec).await))
}

#[derive(Deserialize)]
struct SetValue {
    value: String,
}

/// `PUT /config/{key}` — set a runtime value.
async fn set_one(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    ApiJson(body): ApiJson<SetValue>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let spec = skadi_config::spec(&key)
        .ok_or_else(|| ApiError(AppError::NotFound(format!("no such config key: {key}"))))?;
    if spec.tier != skadi_config::Tier::Tier1 {
        // Tier0 is read before the table exists — writing it here would store a
        // value nothing ever reads, which is worse than refusing.
        return Err(ApiError(AppError::field(
            &key,
            "this key is read before the config table exists and cannot be set at runtime",
        )));
    }
    // Declared type first (SKADI-T-0524), so the error names *this* key.
    skadi_config::validate_write(&key, &body.value)
        .map_err(|e| ApiError(AppError::field(&key, e.to_string())))?;

    // Then cross-key, against the config as it *would be* after the write —
    // checking the stored state instead would let an inverted port range through.
    let mut pairs: Vec<(String, String)> = store
        .list_config()
        .await?
        .into_iter()
        .map(|e| (e.key, e.value))
        .collect();
    pairs.retain(|(k, _)| k != &key);
    pairs.push((key.clone(), body.value.clone()));
    let view = skadi_config::ConfigView::from_pairs(pairs);
    skadi_config::validate_cross_key(&view)
        .map_err(|e| ApiError(AppError::field(&key, e.to_string())))?;

    store
        .set_config(&key, &body.value, ConfigSource::Runtime)
        .await?;
    // Never log the value: this endpoint carries the API token and the vault key.
    tracing::info!(key = %key, "config key set at runtime");
    Ok(Json(dto(store, spec).await))
}

/// `DELETE /config/{key}` — drop the stored row so the registry default applies.
///
/// Distinct from setting the default explicitly: a deleted row is re-seeded from
/// env on the next boot, whereas an explicit runtime value is not. "Reset" and
/// "set to the value that happens to be the default" are different intentions.
async fn reset(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    let spec = skadi_config::spec(&key)
        .ok_or_else(|| ApiError(AppError::NotFound(format!("no such config key: {key}"))))?;
    store.delete_config(&key).await?;
    tracing::info!(key = %key, "config key reset to its default");
    Ok(Json(dto(store, spec).await))
}
