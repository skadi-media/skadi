//! Settings CRUD endpoints (SKADI-T-0053, secret-aware since SKADI-T-0059).
//!
//! `GET/POST /settings/{kind}` and `GET/PUT/DELETE /settings/{kind}/{id}` over
//! the generic [`SettingsRepo`](skadi_store::SettingsRepo), for the daemon's
//! configuration entities. `kind` is validated against [`SETTINGS_KINDS`]; the
//! body is an opaque JSON document (typed per-kind validation arrives with the
//! provider factory consuming these rows).
//!
//! ## Secrets (SKADI-T-0059)
//!
//! Provider kinds carry one secret field (`indexers → api_key`,
//! `downloaders → password`, `notifiers → secret`). That field never lands in
//! the settings `body`: create/update strip it from the request and seal it in
//! the encrypted [`CredentialRepo`](skadi_store::CredentialRepo) under
//! `(owner_kind = kind, owner_id = settings id)`. Reads never return it —
//! responses carry a `has_secret` marker instead. Deletes cascade the
//! credential. Update semantics: secret field **absent** ⇒ keep the existing
//! credential; **present and empty** ⇒ `Validation` error (no silent wipes);
//! present and non-empty ⇒ replace.
//!
//! `edition_kinds` is **not** served here: it lives in `skadi-movies` and is
//! exposed alongside the movies routes (the binary wires those), since
//! `skadi-api` must not depend on a domain crate.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};

use skadi_core::AppError;
use skadi_store::{CredentialRepo, SettingRecord, SettingsRepo, Store};

use crate::error::ApiError;
use crate::state::AppState;

/// The settings entity kinds this endpoint accepts.
pub const SETTINGS_KINDS: &[&str] = &[
    "indexers",
    "downloaders",
    "profiles",
    "notifiers",
    // Custom-format definitions (SKADI-T-0183), domain-keyed (movie|audiobook).
    "custom_formats",
    // Tag registry (SKADI-T-0464). A tag is just a label; membership lives on
    // the items, not here.
    "tags",
];

/// The secret field name for kinds that carry one (SKADI-T-0059).
pub fn secret_field_for(kind: &str) -> Option<&'static str> {
    match kind {
        "indexers" => Some("api_key"),
        "downloaders" => Some("password"),
        "notifiers" => Some("secret"),
        _ => None,
    }
}

/// The wire shape of a stored settings document. `has_secret` is `Some` only
/// for kinds that carry a secret.
#[derive(Serialize)]
struct SettingDto {
    id: String,
    body: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_secret: Option<bool>,
    created_at: String,
    updated_at: String,
}

impl SettingDto {
    fn from_record(r: SettingRecord, has_secret: Option<bool>) -> Self {
        SettingDto {
            id: r.id,
            body: r.body,
            has_secret,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}

/// Routes for the settings entities, to be merged into the authed API router.
pub fn settings_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/settings/{kind}", get(list).post(create))
        .route(
            "/settings/{kind}/{id}",
            get(fetch).put(update).patch(patch).delete(remove),
        )
        .route("/settings/{kind}/{id}/test", axum::routing::post(test))
        // Sonarr/Radarr call this surface `/tag`, and their clients and scripts
        // expect it there (SKADI-T-0464). It is an alias onto the same store,
        // not a second implementation — normalisation and uniqueness are the
        // settings handlers', so the two paths cannot disagree.
        .route("/tag", get(list_tags).post(create_tag))
        .route(
            "/tag/{id}",
            get(fetch_tag).put(update_tag).delete(remove_tag),
        )
}

/// `GET /tag` — Sonarr-compatible alias for `GET /settings/tags`.
async fn list_tags(state: State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    // Tags have no media_type facet, so the filter is always empty here.
    list(
        state,
        Path("tags".to_string()),
        axum::extract::Query(ListQuery { media_type: None }),
    )
    .await
}

/// `POST /tag` — Sonarr-compatible alias for `POST /settings/tags`.
async fn create_tag(
    state: State<Arc<AppState>>,
    body: Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    create(state, Path("tags".to_string()), body).await
}

/// `GET /tag/{id}`.
async fn fetch_tag(
    state: State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    fetch(state, Path(("tags".to_string(), id))).await
}

/// `PUT /tag/{id}`.
async fn update_tag(
    state: State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    update(state, Path(("tags".to_string(), id)), body).await
}

/// `DELETE /tag/{id}`.
async fn remove_tag(
    state: State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    remove(state, Path(("tags".to_string(), id))).await
}

/// Reject unknown kinds with 404 so the surface is exactly the five entities.
fn check_kind(kind: &str) -> Result<(), ApiError> {
    if SETTINGS_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(ApiError(AppError::NotFound(format!(
            "unknown settings kind: {kind}"
        ))))
    }
}

/// The configured store, or a 500 if the daemon wasn't bootstrapped with one.
fn store(state: &AppState) -> Result<&Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

/// Normalize + validate a `profiles` body at write time (SKADI-I-0012).
///
/// Sparse bodies become fully-specified rows: missing/empty `allowed` and
/// missing `cutoff` are filled from the [Standard profile]
/// (`skadi_quality::standard_profile`); `upgrade_allowed` defaults `true`,
/// `min_format_score` to `0`. Entries in `allowed`/`cutoff` may be quality
/// definition **ids or names** (`"Bluray-1080p"`) — names are resolved and the
/// row is stored with canonical id strings. Unknown qualities or a cutoff
/// outside `allowed` are rejected, so an invalid profile can never be stored
/// and silently ignored at resolve time.
/// Enforce exactly-one default profile per media type (SKADI-T-0532).
///
/// When `body` claims `default: true`, clear the flag on every other `profiles`
/// row of the same media type. Done as part of the write rather than validated
/// and refused, because "make this the default" is the operator's whole intent —
/// refusing until they first un-set the old one would be busywork with a window
/// where nothing is default at all.
async fn clear_other_defaults(
    store: &Store,
    body: &serde_json::Value,
    keep_id: Option<&str>,
) -> Result<(), ApiError> {
    if !body
        .get("default")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let media_type = body.get("media_type").and_then(|v| v.as_str());
    for row in store.list_settings("profiles").await? {
        if Some(row.id.as_str()) == keep_id {
            continue;
        }
        if row.body.get("media_type").and_then(|v| v.as_str()) != media_type {
            continue;
        }
        if !row
            .body
            .get("default")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let mut other = row.body.clone();
        if let Some(obj) = other.as_object_mut() {
            obj.insert("default".into(), serde_json::Value::Bool(false));
        }
        store.put_setting("profiles", &row.id, &other).await?;
    }
    Ok(())
}

fn normalize_profile_body(body: &mut serde_json::Value) -> Result<(), ApiError> {
    use serde_json::{Value, json};

    let defs = skadi_quality::default_definitions();
    let resolve = |v: &Value| -> Option<uuid::Uuid> {
        let s = v.as_str()?;
        if let Ok(u) = uuid::Uuid::parse_str(s) {
            return defs.iter().any(|d| d.id.into_uuid() == u).then_some(u);
        }
        defs.iter()
            .find(|d| d.name.eq_ignore_ascii_case(s))
            .map(|d| d.id.into_uuid())
    };

    let obj = body.as_object_mut().ok_or_else(|| {
        ApiError(AppError::Validation(
            "profile body must be a JSON object".into(),
        ))
    })?;

    let standard = skadi_quality::standard_profile(&defs);

    // allowed: fill from Standard when missing/empty, else resolve every entry.
    let allowed: Vec<uuid::Uuid> = match obj.get("allowed") {
        None | Some(Value::Null) => standard.allowed.iter().map(|q| q.into_uuid()).collect(),
        Some(Value::Array(a)) if a.is_empty() => {
            standard.allowed.iter().map(|q| q.into_uuid()).collect()
        }
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                resolve(v).ok_or_else(|| {
                    ApiError(AppError::field(
                        "allowed",
                        format!("unknown quality in allowed: {v}"),
                    ))
                })
            })
            .collect::<Result<_, _>>()?,
        Some(other) => {
            return Err(ApiError(AppError::field(
                "allowed",
                format!("must be an array of quality ids/names, got {other}"),
            )));
        }
    };

    // cutoff: fill from Standard when missing, else resolve; must be allowed.
    let cutoff = match obj.get("cutoff") {
        None | Some(Value::Null) => standard.cutoff.into_uuid(),
        Some(v) => resolve(v)
            .ok_or_else(|| ApiError(AppError::field("cutoff", format!("unknown quality: {v}"))))?,
    };
    if !allowed.contains(&cutoff) {
        return Err(ApiError(AppError::field(
            "cutoff",
            "must be one of the allowed qualities",
        )));
    }

    let allowed_json: Vec<Value> = allowed.iter().map(|u| json!(u.to_string())).collect();
    obj.insert("allowed".into(), Value::Array(allowed_json));
    obj.insert("cutoff".into(), json!(cutoff.to_string()));
    // SKADI-T-0532: the operator's chosen fallback for this media type. Absent
    // means "not the default" — never "unchanged", so a save that omits the field
    // cannot silently keep a stale claim to it.
    match obj.get("default") {
        None | Some(Value::Null) => {
            obj.insert("default".into(), json!(false));
        }
        Some(Value::Bool(_)) => {}
        Some(other) => {
            return Err(ApiError(AppError::field(
                "default",
                format!("must be true or false, got {other}"),
            )));
        }
    }
    obj.entry("upgrade_allowed").or_insert(json!(true));
    obj.entry("min_format_score").or_insert(json!(0));
    obj.entry("upgrade_until_format_score").or_insert(json!(0));

    // media_type (SKADI-T-0301): quality profiles are owned by a media *type*.
    // Movies + TV share the `video` axis; audio/print domains don't use a tunable
    // profile (audio has its fixed ladder), so every stored `profiles` row is
    // `video` — stamp it so the type filter (and any future print profiles) work.
    // An explicit value must be one of the known types.
    match obj.get("media_type") {
        None | Some(Value::Null) => {
            obj.insert("media_type".into(), json!("video"));
        }
        Some(Value::String(s)) if matches!(s.as_str(), "video" | "audio" | "print") => {}
        Some(other) => {
            return Err(ApiError(AppError::Validation(format!(
                "profile: media_type must be one of video|audio|print, got {other}"
            ))));
        }
    }

    // `upgrade_until_format_score` (SKADI-T-0187) must be an integer if present —
    // a wrong type would fail `ProfileSpec` deserialization at resolve time and
    // silently drop the whole profile, so reject it at write time.
    if !obj
        .get("upgrade_until_format_score")
        .is_some_and(serde_json::Value::is_i64)
    {
        return Err(ApiError(AppError::Validation(
            "profile: upgrade_until_format_score must be an integer".into(),
        )));
    }

    // Validate the per-format score assignments (SKADI-T-0183/0185), incl. the
    // optional `mode` (Preferred|Required|Ignored). Deserializing into the typed
    // `CustomFormatScore` rejects a malformed entry or an unknown mode at write
    // time, so a bad profile can never be stored and then silently fall back to
    // the default at resolve time.
    if let Some(formats) = obj.get("formats")
        && !formats.is_null()
    {
        serde_json::from_value::<Vec<skadi_quality::CustomFormatScore>>(formats.clone()).map_err(
            |e| {
                ApiError(AppError::Validation(format!(
                    "profile: invalid formats: {e}"
                )))
            },
        )?;
    }
    Ok(())
}

/// Validate + normalize a `custom_formats` body at write time (SKADI-T-0183).
///
/// A custom format is `{ domain, name, rules }`. `domain` is required and keyed to
/// a media domain (movie|audiobook) — skadi is one all-in-one app, so each format
/// belongs to a domain and is only loaded into that domain's scoring registry.
/// `name` must be non-empty; `rules` must be a non-empty array of valid
/// [`FormatRule`](skadi_quality::FormatRule)s (an uncompilable `TitleRegex` is a
/// 422, never a row that silently never matches). The `domain` string is
/// normalized to the canonical `MediaKind` form (`"Movie"` / `"Audiobook"`).
fn normalize_custom_format_body(body: &mut serde_json::Value) -> Result<(), ApiError> {
    use serde_json::Value;

    let obj = body.as_object_mut().ok_or_else(|| {
        ApiError(AppError::Validation(
            "custom_format body must be a JSON object".into(),
        ))
    })?;

    // domain: required; normalize to the canonical MediaKind variant string.
    let domain = obj.get("domain").and_then(|v| v.as_str()).ok_or_else(|| {
        ApiError(AppError::Validation(
            "custom_format requires a 'domain' (movie|audiobook)".into(),
        ))
    })?;
    let canonical = match domain.to_ascii_lowercase().as_str() {
        "movie" => "Movie",
        "audiobook" => "Audiobook",
        other => {
            return Err(ApiError(AppError::Validation(format!(
                "custom_format: unsupported domain {other:?} (expected movie|audiobook)"
            ))));
        }
    };
    obj.insert("domain".into(), Value::String(canonical.into()));

    // name: required, non-empty.
    let name_ok = obj
        .get("name")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    if !name_ok {
        return Err(ApiError(AppError::Validation(
            "custom_format requires a non-empty 'name'".into(),
        )));
    }

    // rules: required, non-empty array of well-formed FormatRules.
    let rules_val = obj.get("rules").cloned().unwrap_or(Value::Null);
    let rules: Vec<skadi_quality::FormatRule> = serde_json::from_value(rules_val)
        .map_err(|e| ApiError(AppError::Validation(format!("custom_format rules: {e}"))))?;
    if rules.is_empty() {
        return Err(ApiError(AppError::Validation(
            "custom_format requires at least one rule".into(),
        )));
    }
    for r in &rules {
        r.validate()
            .map_err(|e| ApiError(AppError::Validation(format!("custom_format rule: {e}"))))?;
    }
    Ok(())
}

/// Pull the secret out of a request body for kinds that carry one.
///
/// Returns the secret value when present and valid. Errors on a present-but-
/// empty value so a sloppy update can't silently wipe a stored credential.
fn extract_secret(kind: &str, body: &mut serde_json::Value) -> Result<Option<String>, ApiError> {
    let Some(field) = secret_field_for(kind) else {
        return Ok(None);
    };
    let Some(obj) = body.as_object_mut() else {
        return Ok(None);
    };
    let Some(value) = obj.remove(field) else {
        return Ok(None);
    };
    match value.as_str() {
        Some(s) if !s.is_empty() => Ok(Some(s.to_string())),
        _ => Err(ApiError(AppError::Validation(format!(
            "{kind}: {field} must be a non-empty string when provided"
        )))),
    }
}

/// `Some(bool)` for secret-carrying kinds, `None` otherwise.
async fn has_secret(store: &Store, kind: &str, id: &str) -> Result<Option<bool>, ApiError> {
    if secret_field_for(kind).is_none() {
        return Ok(None);
    }
    Ok(Some(store.get_secret(kind, id).await?.is_some()))
}

/// Query params for the settings list. `media_type` (SKADI-T-0301) filters
/// `profiles` to a single media type so a domain only ever sees its own type's
/// profiles (a video domain → `video` rows; an audio domain → none, since audio
/// uses its fixed ladder). Ignored for non-`profiles` kinds.
#[derive(Deserialize, Default)]
struct ListQuery {
    media_type: Option<String>,
}

async fn list(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    let records = store.list_settings(&kind).await?;
    let mut dtos = Vec::with_capacity(records.len());
    for r in records {
        // Type filter for profiles: a row's media_type defaults to `video` when
        // unstamped (legacy rows), so a `?media_type=audio` request returns none.
        if kind == "profiles"
            && let Some(want) = q.media_type.as_deref()
        {
            let mt = r
                .body
                .get("media_type")
                .and_then(|v| v.as_str())
                .unwrap_or("video");
            if !mt.eq_ignore_ascii_case(want) {
                continue;
            }
        }
        let marker = has_secret(store, &kind, &r.id).await?;
        dtos.push(SettingDto::from_record(r, marker));
    }
    Ok(Json(dtos))
}

/// Refuse a tag whose label already exists (SKADI-T-0464).
///
/// Checked here rather than by a unique index because the settings store holds
/// every kind in one table keyed by an opaque id — the label lives inside the
/// JSON body, so uniqueness is a property of this kind, not of the schema. Two
/// tags with the same label would be indistinguishable in every UI that shows
/// them, and `?tags=anime` could not say which was meant.
async fn reject_duplicate_tag(
    store: &Store,
    body: &serde_json::Value,
    keep_id: Option<&str>,
) -> Result<(), ApiError> {
    let label = body
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    for row in store.list_settings("tags").await? {
        if Some(row.id.as_str()) == keep_id {
            continue;
        }
        if row.body.get("label").and_then(|v| v.as_str()) == Some(label) {
            return Err(ApiError(AppError::Validation(format!(
                "tags: a tag labelled {label:?} already exists"
            ))));
        }
    }
    Ok(())
}

/// Normalise and validate a `tags` body (SKADI-T-0464).
///
/// A tag is one field, `label`, and the rules exist so two tags cannot differ in
/// ways a human would not notice:
///
/// * trimmed and lowercased, so `Anime`, `anime ` and `ANIME` are the same tag —
///   Sonarr does the same, and without it an operator ends up with three tags
///   that look identical in a list;
/// * non-empty after trimming;
/// * no commas, because the natural query form for filtering by several tags is
///   `?tags=a,b` and a comma inside a label would make that unparseable. Better
///   refused at write time than discovered when a filter silently splits.
fn normalize_tag_body(body: &mut serde_json::Value) -> Result<(), ApiError> {
    let err = |m: &str| ApiError(AppError::Validation(format!("tags: {m}")));
    let label = body
        .get("label")
        .and_then(|v| v.as_str())
        .ok_or_else(|| err("a tag needs a string `label`"))?;
    let normalized = label.trim().to_lowercase();
    if normalized.is_empty() {
        return Err(err("`label` cannot be empty"));
    }
    if normalized.contains(',') {
        return Err(err(
            "`label` cannot contain a comma — tag filters are comma-separated",
        ));
    }
    if let Some(map) = body.as_object_mut() {
        map.insert("label".into(), serde_json::Value::String(normalized));
    }
    Ok(())
}

/// Reject a provider body that could never load (SKADI-T-0471).
///
/// Validates by attempting exactly the deserialization the provider factory does
/// at startup, so the check cannot drift from what actually loads — the same
/// lesson as SKADI-T-0427's duplicated templates. Before this, an indexer without
/// a `base_url` was stored happily and then skipped with a warning on every
/// reconcile, so the settings UI showed a provider that silently did nothing.
///
/// Only the three provider kinds are checked; `profiles` and `custom_formats`
/// have their own normalisers above.
fn validate_provider_body(kind: &str, body: &serde_json::Value) -> Result<(), ApiError> {
    // The secret field is still on the body at create time and is not part of the
    // typed config; the configs ignore unknown fields, so this is harmless.
    let err = |e: serde_json::Error| ApiError(AppError::Validation(format!("{kind}: {e}")));
    match kind {
        "indexers" => {
            serde_json::from_value::<skadi_indexers::IndexerConfig>(body.clone()).map_err(err)?;
        }
        "downloaders" => {
            serde_json::from_value::<skadi_downloaders::DownloaderConfig>(body.clone())
                .map_err(err)?;
        }
        "notifiers" => {
            serde_json::from_value::<skadi_notify::NotifierConfig>(body.clone()).map_err(err)?;
        }
        _ => {}
    }
    reject_blank_required(kind, body)
}

/// The text fields of a provider config that cannot be blank, by settings
/// kind and config `kind` (SKADI-T-0699). The typed parse above accepts `""`
/// for a `String`, so a form that sent an empty name or URL stored a provider
/// that could never work. The web form checks the same fields before it saves;
/// this is the check for every other client.
fn required_provider_fields(kind: &str, config_kind: &str) -> &'static [&'static str] {
    match (kind, config_kind) {
        ("indexers", "torznab" | "prowlarr") => &["name", "base_url"],
        ("indexers", "cardigann") => &["name", "definition_id"],
        ("indexers", _) => &["name"],
        ("notifiers", "webhook" | "discord") => &["name", "url"],
        ("notifiers", "telegram") => &["name", "chat_id"],
        ("notifiers", "pushover") => &["name", "user_key"],
        ("notifiers", _) => &["name"],
        _ => &[],
    }
}

fn reject_blank_required(kind: &str, body: &serde_json::Value) -> Result<(), ApiError> {
    let config_kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    for field in required_provider_fields(kind, config_kind) {
        let blank = body
            .get(*field)
            .and_then(|v| v.as_str())
            .is_none_or(|v| v.trim().is_empty());
        if blank {
            return Err(ApiError(AppError::field(
                *field,
                format!("{kind}: {field} is required"),
            )));
        }
    }
    Ok(())
}

/// `PATCH /settings/{kind}/{id}` — merge the given fields into the stored body
/// (SKADI-T-0471).
///
/// `PUT` replaces, so changing one field means the client must send every other
/// field back — and a client that forgets one silently drops it. A profile edited
/// through a narrow UI is the obvious case: toggling `upgrade_allowed` should not
/// require re-sending the allowed-quality list.
///
/// Shallow merge on the top-level object: a key present in the patch replaces the
/// stored one, a key absent is left alone. Nested objects are replaced wholesale
/// rather than deep-merged, because a deep merge gives no way to *remove* a
/// nested key. The merged result runs through the same normalisation and
/// validation as a `PUT`, so a patch cannot write a body a `PUT` would reject.
async fn patch(
    State(state): State<Arc<AppState>>,
    Path((kind, id)): Path<(String, String)>,
    Json(patch): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    let existing = store
        .get_setting(&kind, &id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("{kind}/{id} not found"))))?;

    let (serde_json::Value::Object(mut merged), serde_json::Value::Object(fields)) =
        (existing.body.clone(), patch)
    else {
        return Err(ApiError(AppError::Validation(
            "patch body must be a JSON object".into(),
        )));
    };
    for (k, v) in fields {
        merged.insert(k, v);
    }
    let mut body = serde_json::Value::Object(merged);

    if kind == "profiles" {
        normalize_profile_body(&mut body)?;
    } else if kind == "custom_formats" {
        normalize_custom_format_body(&mut body)?;
    } else if kind == "tags" {
        normalize_tag_body(&mut body)?;
        reject_duplicate_tag(store, &body, Some(&id)).await?;
    } else {
        validate_provider_body(&kind, &body)?;
    }
    // A patch may carry a new secret; absent means keep the stored one.
    let secret = extract_secret(&kind, &mut body)?;
    if let Some(secret) = &secret {
        store.set_secret(&kind, &id, secret).await?;
    }
    let record = store.put_setting(&kind, &id, &body).await?;
    let marker = has_secret(store, &kind, &id).await?;
    Ok(Json(SettingDto::from_record(record, marker)))
}

async fn create(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
    Json(mut body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    if kind == "profiles" {
        normalize_profile_body(&mut body)?;
    } else if kind == "custom_formats" {
        normalize_custom_format_body(&mut body)?;
    } else if kind == "tags" {
        normalize_tag_body(&mut body)?;
        reject_duplicate_tag(store, &body, None).await?;
    } else {
        validate_provider_body(&kind, &body)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    if kind == "profiles" {
        clear_other_defaults(store, &body, None).await?;
    }
    let secret = extract_secret(&kind, &mut body)?;
    if let Some(secret) = &secret {
        store.set_secret(&kind, &id, secret).await?;
    }
    let record = store.put_setting(&kind, &id, &body).await?;
    let marker = has_secret(store, &kind, &id).await?;
    Ok((
        StatusCode::CREATED,
        Json(SettingDto::from_record(record, marker)),
    ))
}

async fn fetch(
    State(state): State<Arc<AppState>>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    let record = store
        .get_setting(&kind, &id)
        .await?
        .ok_or_else(|| ApiError(AppError::NotFound(format!("{kind}/{id} not found"))))?;
    let marker = has_secret(store, &kind, &id).await?;
    Ok(Json(SettingDto::from_record(record, marker)))
}

async fn update(
    State(state): State<Arc<AppState>>,
    Path((kind, id)): Path<(String, String)>,
    Json(mut body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    // PUT updates an existing document; 404 if it isn't there.
    if store.get_setting(&kind, &id).await?.is_none() {
        return Err(ApiError(AppError::NotFound(format!(
            "{kind}/{id} not found"
        ))));
    }
    if kind == "profiles" {
        normalize_profile_body(&mut body)?;
    } else if kind == "custom_formats" {
        normalize_custom_format_body(&mut body)?;
    } else {
        validate_provider_body(&kind, &body)?;
    }
    if kind == "profiles" {
        clear_other_defaults(store, &body, Some(&id)).await?;
    }
    // Absent secret field ⇒ keep the stored credential as-is.
    let secret = extract_secret(&kind, &mut body)?;
    if let Some(secret) = &secret {
        store.set_secret(&kind, &id, secret).await?;
    }
    let record = store.put_setting(&kind, &id, &body).await?;
    let marker = has_secret(store, &kind, &id).await?;
    Ok(Json(SettingDto::from_record(record, marker)))
}

/// `POST /settings/{kind}/{id}/test` — build the stored provider and run its
/// connectivity/credential check (SKADI-T-0061).
///
/// Returns 200 with `{ ok: true }` or `{ ok: false, error }` — an HTTP error is
/// reserved for "the request itself was bad" (unknown kind/id, malformed
/// config), so the client can distinguish "test ran and failed" from "couldn't
/// run the test". Provider error text is passed through; the config builders
/// don't echo secrets into their messages.
async fn test(
    State(state): State<Arc<AppState>>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    let provider = crate::providers::build_one(store, &kind, &id).await?;
    let body = match provider.test().await {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    Ok(Json(body))
}

async fn remove(
    State(state): State<Arc<AppState>>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    check_kind(&kind)?;
    let store = store(&state)?;
    // Refuse to delete the default profile (SKADI-T-0532). Deleting it would
    // silently hand the fallback to whichever row sorts first — the arbitrary
    // behaviour this ticket removed — so the operator must nominate a
    // replacement first. Refusing is deliberate over auto-reassigning: which
    // profile becomes the fallback is exactly the decision they should be making.
    if kind == "profiles"
        && let Some(row) = store.get_setting(&kind, &id).await?
        && row
            .body
            .get("default")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        return Err(ApiError(AppError::Validation(
            "this is the default quality profile; mark another profile default before deleting it"
                .into(),
        )));
    }
    let existed = store.delete_setting(&kind, &id).await?;
    if existed {
        // Cascade the credential so nothing orphans.
        if secret_field_for(&kind).is_some() {
            store.delete_secret(&kind, &id).await?;
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(AppError::NotFound(format!(
            "{kind}/{id} not found"
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sparse_profile_body_is_filled_with_standard_defaults() {
        let mut body = json!({ "name": "default" });
        normalize_profile_body(&mut body).unwrap();

        let defs = skadi_quality::default_definitions();
        let standard = skadi_quality::standard_profile(&defs);
        let allowed = body["allowed"].as_array().unwrap();
        assert_eq!(allowed.len(), standard.allowed.len());
        assert_eq!(
            body["cutoff"].as_str().unwrap(),
            standard.cutoff.to_string()
        );
        assert_eq!(body["upgrade_allowed"], json!(true));
        assert_eq!(body["min_format_score"], json!(0));
        assert_eq!(body["name"], json!("default"), "caller fields preserved");
        // SKADI-T-0301: quality profiles are stamped with their media type. An
        // unstamped body is `video` (movies + TV share the video axis).
        assert_eq!(body["media_type"], json!("video"));
    }

    #[test]
    fn profile_media_type_is_stamped_and_validated() {
        // Explicit known types are kept verbatim.
        let mut body = json!({ "name": "p", "media_type": "print" });
        normalize_profile_body(&mut body).unwrap();
        assert_eq!(body["media_type"], json!("print"));

        // An unknown media type is rejected, never silently coerced.
        let mut bad = json!({ "name": "p", "media_type": "holographic" });
        assert!(normalize_profile_body(&mut bad).is_err());
    }

    #[test]
    fn quality_names_resolve_to_canonical_ids() {
        let mut body = json!({ "name": "hd", "cutoff": "Bluray-1080p" });
        normalize_profile_body(&mut body).unwrap();
        let defs = skadi_quality::default_definitions();
        let bluray = defs.iter().find(|d| d.name == "Bluray-1080p").unwrap();
        assert_eq!(
            body["cutoff"].as_str().unwrap(),
            bluray.id.to_string(),
            "name resolved to the canonical id"
        );
    }

    #[test]
    fn unknown_quality_and_cutoff_outside_allowed_are_rejected() {
        let mut body = json!({ "allowed": ["No-Such-Quality"] });
        assert!(normalize_profile_body(&mut body).is_err());

        // SDTV is a real quality but not in the (default-filled) allowed list…
        // so make allowed explicit 1080p-only and cut off at 2160p.
        let mut body = json!({
            "allowed": ["Bluray-1080p"],
            "cutoff": "Bluray-2160p"
        });
        assert!(normalize_profile_body(&mut body).is_err());
    }

    // --- custom_formats normalization (SKADI-T-0183) ---

    #[test]
    fn profile_format_assignments_validate_mode() {
        // SKADI-T-0185: a per-format `mode` is accepted (Preferred|Required|Ignored)…
        let fmt = uuid::Uuid::new_v4().to_string();
        let mut ok = json!({
            "allowed": ["Bluray-1080p"],
            "cutoff": "Bluray-1080p",
            "formats": [{ "format": fmt, "score": 50, "mode": "Required" }]
        });
        normalize_profile_body(&mut ok).unwrap();

        // …and an unknown mode is rejected at write time (not silently dropped).
        let mut bad = json!({
            "allowed": ["Bluray-1080p"],
            "cutoff": "Bluray-1080p",
            "formats": [{ "format": fmt, "score": 50, "mode": "Banana" }]
        });
        assert!(normalize_profile_body(&mut bad).is_err());
    }

    #[test]
    fn custom_format_normalizes_domain_casing_and_keeps_fields() {
        let mut body = json!({
            "domain": "movie",
            "name": "x265",
            "rules": [{ "Codec": "x265" }]
        });
        normalize_custom_format_body(&mut body).unwrap();
        // Domain canonicalized to the MediaKind variant string.
        assert_eq!(body["domain"], json!("Movie"));
        assert_eq!(body["name"], json!("x265"));
        assert_eq!(body["rules"][0]["Codec"], json!("x265"));
    }

    #[test]
    fn custom_format_accepts_audiobook_domain() {
        let mut body = json!({
            "domain": "AUDIOBOOK",
            "name": "M4B",
            "rules": [{ "TitleRegex": "m4b" }]
        });
        normalize_custom_format_body(&mut body).unwrap();
        assert_eq!(body["domain"], json!("Audiobook"));
    }

    #[test]
    fn custom_format_rejects_missing_or_unknown_domain() {
        let mut no_domain = json!({ "name": "x", "rules": [{ "TitleRegex": "x" }] });
        assert!(normalize_custom_format_body(&mut no_domain).is_err());

        let mut bad_domain = json!({
            "domain": "series",
            "name": "x",
            "rules": [{ "TitleRegex": "x" }]
        });
        assert!(normalize_custom_format_body(&mut bad_domain).is_err());
    }

    #[test]
    fn custom_format_rejects_empty_name_empty_rules_and_bad_regex() {
        // Empty name.
        let mut empty_name =
            json!({ "domain": "movie", "name": "  ", "rules": [{ "TitleRegex": "x" }] });
        assert!(normalize_custom_format_body(&mut empty_name).is_err());

        // No rules.
        let mut no_rules = json!({ "domain": "movie", "name": "x", "rules": [] });
        assert!(normalize_custom_format_body(&mut no_rules).is_err());

        // Uncompilable regex is a 422, not a silently-never-matching row.
        let mut bad_regex = json!({
            "domain": "movie",
            "name": "x",
            "rules": [{ "TitleRegex": "(" }]
        });
        assert!(normalize_custom_format_body(&mut bad_regex).is_err());
    }
}
