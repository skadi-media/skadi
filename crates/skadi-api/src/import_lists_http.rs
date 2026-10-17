//! HTTP surface for import lists (SKADI-T-0511).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::Deserialize;

use skadi_core::AppError;
use skadi_store::{ImportList, ImportListExclusion, ImportListRepo, Store};

use crate::error::ApiError;
use crate::state::AppState;

fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

/// Routes for import lists and their exclusions.
pub fn import_lists_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/importlists", get(list).post(create))
        .route("/importlists/{id}", get(get_one).put(update).delete(remove))
        .route("/importlists/{id}/sync", post(sync_one))
        .route("/importlists/sync", post(sync_all))
        .route(
            "/importlists/exclusions",
            get(list_exclusions).post(add_exclusion),
        )
        .route("/importlists/exclusions/{id}", delete(remove_exclusion))
}

async fn list(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(store(&state)?.list_import_lists().await?))
}

async fn get_one(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    store(&state)?
        .get_import_list(&id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError(AppError::NotFound(format!("import list {id} not found"))))
}

/// A list needs a name an operator can find it by (SKADI-T-0733).
fn validate(list: &ImportList) -> Result<(), ApiError> {
    if list.name.trim().is_empty() {
        return Err(ApiError(AppError::field(
            "name",
            "an import list needs a name",
        )));
    }
    if list.kind.trim().is_empty() {
        return Err(ApiError(AppError::field(
            "kind",
            "an import list needs a kind",
        )));
    }
    if list.interval_minutes < 1 {
        return Err(ApiError(AppError::field(
            "interval_minutes",
            "the sync interval must be at least one minute",
        )));
    }
    Ok(())
}

async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    let says_monitored = body.get("add_monitored").is_some();
    let mut list: ImportList = serde_json::from_value(body)
        .map_err(|e| ApiError(AppError::Validation(format!("import list: {e}"))))?;
    // A body that leaves the switch out gets the domain's default: on for an
    // audiobook list, off for films and series (SKADI-T-0733).
    if !says_monitored {
        list.add_monitored = skadi_store::import_lists::monitored_by_default(&list.target_domain);
    }
    validate(&list)?;
    // A new list starts with no sync history and a server id, whatever the
    // body says: the UI sends the form, not a row.
    if list.id.trim().is_empty() {
        list.id = uuid::Uuid::new_v4().to_string();
    }
    list.last_synced_at = None;
    list.last_error = None;
    list.created_at = chrono::Utc::now();
    store(&state)?.upsert_import_list(&list).await?;
    Ok((StatusCode::CREATED, Json(list)))
}

async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut list): Json<ImportList>,
) -> Result<impl IntoResponse, ApiError> {
    let store = store(&state)?;
    validate(&list)?;
    // 404 before writing, so a PUT to a typo'd id does not silently create a
    // second list under an id the caller did not choose.
    let Some(stored) = store.get_import_list(&id).await? else {
        return Err(ApiError(AppError::NotFound(format!(
            "import list {id} not found"
        ))));
    };
    // The path wins over the body: a body carrying a different id would
    // otherwise move the list, which no caller means by PUT.
    list.id = id;
    // The sync history and the creation time belong to the server: an edit
    // form that leaves them out must not wipe them (SKADI-T-0733).
    list.created_at = stored.created_at;
    list.last_synced_at = stored.last_synced_at;
    list.last_error = stored.last_error;
    store.upsert_import_list(&list).await?;
    Ok(Json(list))
}

async fn remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    store(&state)?.delete_import_list(&id).await?;
    Ok(Json(serde_json::json!({ "deleted": id })))
}

/// Sync one list now, whether or not it is due.
///
/// The manual trigger deliberately ignores the interval: an operator pressing
/// "sync now" after fixing a list's settings should not be told to wait twelve
/// hours.
async fn sync_one(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let s = store(&state)?;
    let list = s
        .get_import_list(&id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("import list {id} not found"))))?;
    let report =
        crate::import_lists::sync_list(&list, &state.import_list_providers, &state.library, s)
            .await;
    let now = chrono::Utc::now();
    match report {
        Ok(r) => {
            let err = (!r.failed.is_empty()).then(|| format!("{} item(s) failed", r.failed.len()));
            let _ = s.record_sync(&id, now, err.as_deref()).await;
            Ok(Json(serde_json::to_value(&r).unwrap_or_default()))
        }
        Err(e) => {
            // Record the failure on the row before returning it, so the list page
            // shows the same reason the caller just saw, worded the same way.
            let e = ApiError(e);
            let _ = s.record_sync(&id, now, Some(&e.message())).await;
            Err(e)
        }
    }
}

async fn sync_all(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let reports = crate::import_lists::sync_due_lists(
        &state.import_list_providers,
        &state.library,
        store(&state)?,
    )
    .await?;
    Ok(Json(reports))
}

#[derive(Deserialize)]
struct ExclusionQuery {
    domain: Option<String>,
}

async fn list_exclusions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ExclusionQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        store(&state)?.list_exclusions(q.domain.as_deref()).await?,
    ))
}

async fn add_exclusion(
    State(state): State<Arc<AppState>>,
    Json(mut e): Json<ImportListExclusion>,
) -> Result<impl IntoResponse, ApiError> {
    if e.external_id.trim().is_empty() {
        return Err(ApiError(AppError::field(
            "external_id",
            "an exclusion needs the id of the item",
        )));
    }
    if e.id.trim().is_empty() {
        e.id = uuid::Uuid::new_v4().to_string();
    }
    e.external_id = e.external_id.trim().to_string();
    store(&state)?.add_exclusion(&e).await?;
    Ok((StatusCode::CREATED, Json(e)))
}

async fn remove_exclusion(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    store(&state)?.delete_exclusion(&id).await?;
    Ok(Json(serde_json::json!({ "deleted": id })))
}
