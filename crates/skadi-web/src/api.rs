//! Typed HTTP client core for the Skadi API (SKADI-T-0065).
//!
//! All requests are relative to `/api/v1`, so the UI works when served from the
//! daemon at the same origin (and via `trunk serve`'s proxy in dev). v1 is
//! open-mode: there is **no** auth token — see initiative SKADI-I-0010.
//!
//! Later tasks (T-0066/T-0067) extend this with settings/domains methods; this
//! task ships only the `health()` smoke call that proves the pipeline.

use gloo_net::http::{Request, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Base path for every API call. Relative on purpose (same-origin / proxy).
pub const API_BASE: &str = "/api/v1";

/// Where the signed-in member's token lives (SKADI-T-0621).
const TOKEN_KEY: &str = "skadi.token";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// The token this browser is signed in with, or `None` when it is not.
///
/// Read from storage each call — cheap, and it means a sign-in or sign-out in
/// one place is seen everywhere without plumbing a signal through.
///
/// It used to come from a `<meta>` tag the daemon templated with the
/// *operator's* token for anyone who loaded the page (SKADI-T-0068). That is
/// gone: the page carries no credential and the UI logs in.
fn api_token() -> Option<String> {
    let t = storage()?.get_item(TOKEN_KEY).ok()??;
    (!t.is_empty()).then_some(t)
}

/// Whether this browser holds a token at all. `false` is not the same as
/// "needs to log in" — an open-mode daemon needs none.
#[must_use]
pub fn is_signed_in() -> bool {
    api_token().is_some()
}

/// Remember the token from a successful login, and clear the expired flag that
/// sent us to the login page.
pub fn store_token(token: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(TOKEN_KEY, token);
    }
    clear_auth_expired();
}

/// Forget the token this browser holds.
pub fn forget_token() {
    if let Some(s) = storage() {
        let _ = s.remove_item(TOKEN_KEY);
    }
}

/// Set once any API response comes back `401` (SKADI-T-0474).
///
/// After the operator rotates `api_token`, the token injected into `index.html`
/// at page load is stale — every request then fails and the UI showed the raw
/// error from whichever call happened to run, with nothing saying *why* or what
/// to do. This flag is the single fact the shell needs to say "sign in again".
///
/// A plain `AtomicBool` rather than a signal: the wasm UI is single-threaded, and
/// this is set from deep inside `api` where no reactive scope is in hand. The
/// shell polls it, which is the same shape the rest of the UI already uses for
/// background state.
static AUTH_EXPIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether any request has been rejected as unauthenticated.
#[must_use]
pub fn auth_expired() -> bool {
    AUTH_EXPIRED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the flag directly. Test-only seam: the real path is a `401` arriving
/// through [`Noted`], which needs a live response to exercise.
#[doc(hidden)]
pub fn note_auth_expired_for_test() {
    AUTH_EXPIRED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Clear the flag — after a reload, or once the operator supplies a new token.
pub fn clear_auth_expired() {
    AUTH_EXPIRED.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Record a `401` on the way past (SKADI-T-0474).
///
/// Implemented as an extension on the `Result` so every call site is a
/// suffix — `.send().await.noted()?` — rather than a rewrite. Forty-eight call
/// sites with no shared response handler is exactly how the gap survived; a
/// suffix is something a reviewer can see is applied everywhere.
pub(crate) trait Noted: Sized {
    fn noted(self) -> Self;
}

impl Noted for Result<gloo_net::http::Response, gloo_net::Error> {
    fn noted(self) -> Self {
        if let Ok(r) = &self
            && r.status() == 401
        {
            AUTH_EXPIRED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self
    }
}

/// Apply the bearer token (when configured) to a request builder. Open-mode
/// daemons inject an empty token, so this is a no-op there.
fn authed(builder: RequestBuilder) -> RequestBuilder {
    match api_token() {
        Some(t) => builder.header("Authorization", &format!("Bearer {t}")),
        None => builder,
    }
}

fn get(url: &str) -> RequestBuilder {
    authed(Request::get(url))
}
fn post(url: &str) -> RequestBuilder {
    authed(Request::post(url))
}
fn put(url: &str) -> RequestBuilder {
    authed(Request::put(url))
}
fn patch(url: &str) -> RequestBuilder {
    authed(Request::patch(url))
}
fn delete(url: &str) -> RequestBuilder {
    authed(Request::delete(url))
}

/// Anything that can go wrong talking to the API, flattened to a message the UI
/// can render. Kept deliberately simple for v1.
#[derive(Debug, Clone)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<gloo_net::Error> for ApiError {
    fn from(e: gloo_net::Error) -> Self {
        ApiError(e.to_string())
    }
}

/// Mirror of the daemon's `/health` response body (skadi-api `health.rs`).
/// `status` is part of the wire contract even though the UI only renders the
/// version today.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Health {
    pub status: String,
    pub version: String,
}

/// `GET /api/v1/health` — liveness probe. Used as the end-to-end smoke test for
/// the whole serve/embed pipeline.
pub async fn health() -> Result<Health, ApiError> {
    let url = format!("{API_BASE}/health");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("health returned HTTP {}", resp.status())));
    }
    let body = resp.json::<Health>().await?;
    Ok(body)
}

// ---------------------------------------------------------------------------
// Settings (SKADI-T-0066) — the generic `(kind, id)` config store.
//
// `kind` here is the settings entity: "indexers" | "downloaders" | "notifiers"
// (and "profiles" | "root_folders" for T-0067). The `body` is the typed config
// document; for provider kinds it carries the inner config `kind` tag
// ("torznab" | "skadi" | "webhook") plus its fields. The secret field
// (api_key / password / secret) is write-only: never present in a GET response,
// only sent on create/update. `has_secret` reports whether one is stored.
// ---------------------------------------------------------------------------

/// One stored settings document as returned by the API. Mirrors `SettingDto`
/// in skadi-api `settings.rs`. `created_at`/`updated_at` are decoded for
/// completeness though not shown yet.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Setting {
    pub id: String,
    pub body: Value,
    /// Present only for secret-carrying kinds; `true` once a secret is stored.
    #[serde(default)]
    pub has_secret: Option<bool>,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of `POST /settings/{kind}/{id}/test`. The endpoint returns 200 with
/// `ok: false` when the test ran but failed (vs an HTTP error for "couldn't run
/// the test at all").
#[derive(Debug, Clone, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// When the job was enqueued (RFC 3339, UTC, fixed width, so it sorts as a
    /// string). `None` from a daemon older than SKADI-T-0686.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// `GET /settings/{kind}` — list stored documents for a kind.
pub async fn list_settings(kind: &str) -> Result<Vec<Setting>, ApiError> {
    let url = format!("{API_BASE}/settings/{kind}");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list {kind} -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Setting>>().await?)
}

/// `POST /settings/{kind}` — create a document. `body` is the full config JSON,
/// including the secret field when the user supplied one.
pub async fn create_setting(kind: &str, body: &Value) -> Result<Setting, ApiError> {
    let url = format!("{API_BASE}/settings/{kind}");
    let resp = post(&url).json(body)?.send().await.noted()?;
    read_setting_response(kind, resp).await
}

/// `PUT /settings/{kind}/{id}` — replace a document. Omit the secret field from
/// `body` to keep the stored credential; include it (non-empty) to rotate it.
pub async fn update_setting(kind: &str, id: &str, body: &Value) -> Result<Setting, ApiError> {
    let url = format!("{API_BASE}/settings/{kind}/{id}");
    let resp = put(&url).json(body)?.send().await.noted()?;
    read_setting_response(kind, resp).await
}

/// `DELETE /settings/{kind}/{id}` — remove a document (cascades its credential).
pub async fn delete_setting(kind: &str, id: &str) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/settings/{kind}/{id}");
    let resp = delete(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "delete {kind}/{id} -> HTTP {}",
            resp.status()
        )));
    }
    Ok(())
}

/// `POST /settings/{kind}/{id}/test` — run the provider's connectivity check.
pub async fn test_setting(kind: &str, id: &str) -> Result<TestResult, ApiError> {
    let url = format!("{API_BASE}/settings/{kind}/{id}/test");
    let resp = post(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "test {kind}/{id} -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<TestResult>().await?)
}

/// One user-facing setting of a cardigann definition (for the dynamic add form).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogSetting {
    pub name: String,
    #[serde(default)]
    pub label: String,
    /// `text` / `password` / `checkbox` / `select`.
    pub kind: String,
    #[serde(default)]
    pub default: Option<String>,
}

/// A cardigann definition summary from the catalog (add-tracker picker).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub privacy: String,
    #[serde(default)]
    pub needs_login: bool,
    #[serde(default)]
    pub settings: Vec<CatalogSetting>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub search_modes: Vec<String>,
}

/// `GET /indexers/definitions` — the synced cardigann definition catalog.
pub async fn list_definitions() -> Result<Vec<CatalogDef>, ApiError> {
    let url = format!("{API_BASE}/indexers/definitions");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("definitions -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<CatalogDef>>().await?)
}

/// `POST /indexers/definitions/refresh` — re-sync the library; returns the count
/// of definitions written.
pub async fn refresh_definitions() -> Result<usize, ApiError> {
    let url = format!("{API_BASE}/indexers/definitions/refresh");
    let resp = post(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("refresh -> HTTP {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        written: usize,
    }
    Ok(resp.json::<R>().await?.written)
}

/// Shared decode for create/update: on a non-2xx, surface the server's error
/// message (validation errors carry useful text) rather than a bare status.
async fn read_setting_response(
    kind: &str,
    resp: gloo_net::http::Response,
) -> Result<Setting, ApiError> {
    if resp.ok() {
        return Ok(resp.json::<Setting>().await?);
    }
    let status = resp.status();
    let detail = resp
        .text()
        .await
        .ok()
        .and_then(|t| extract_error_message(&t))
        .unwrap_or_default();
    if detail.is_empty() {
        Err(ApiError(format!("{kind} write -> HTTP {status}")))
    } else {
        Err(ApiError(detail))
    }
}

/// Pull the human message out of the API error envelope
/// (`{ "error": "<kind>", "message": "<text>" }`, see skadi-api `error.rs`),
/// falling back to the `error` kind or the raw text.
fn extract_error_message(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    if let Some(s) = v.get("message").and_then(|m| m.as_str()) {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("error").and_then(|e| e.as_str()) {
        return Some(s.to_string());
    }
    Some(text.to_string())
}

// ---------------------------------------------------------------------------
// Domains (SKADI-T-0067) — enable/disable compiled-in domains.
// ---------------------------------------------------------------------------

/// A compiled-in domain joined with its runtime enable state.
#[derive(Debug, Clone, Deserialize)]
pub struct Domain {
    pub name: String,
    pub kind: String,
    pub enabled: bool,
}

/// `GET /domains` — list domains with their enabled state.
pub async fn list_domains() -> Result<Vec<Domain>, ApiError> {
    let resp = get(&format!("{API_BASE}/domains")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list domains -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Domain>>().await?)
}

/// `PUT /domains/{name}` — toggle a domain on/off.
pub async fn set_domain_enabled(name: &str, enabled: bool) -> Result<Domain, ApiError> {
    let url = format!("{API_BASE}/domains/{name}");
    let resp = put(&url)
        .json(&serde_json::json!({ "enabled": enabled }))?
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "toggle domain {name} -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<Domain>().await?)
}

// ---------------------------------------------------------------------------
// Quality definitions (SKADI-T-0067) — read-only registry the profile UI picks
// from. Ordered low→high (rank = index).
// ---------------------------------------------------------------------------

/// One built-in quality definition (mirror of skadi-api `quality.rs`).
/// `resolution`/`rank` are decoded for completeness; the UI renders in the
/// server's low→high order.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct QualityDef {
    pub id: String,
    pub name: String,
    pub resolution: String,
    pub rank: usize,
}

/// `GET /quality/definitions` — the named qualities a profile can allow.
pub async fn list_quality_definitions() -> Result<Vec<QualityDef>, ApiError> {
    let resp = get(&format!("{API_BASE}/quality/definitions"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "quality definitions -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<Vec<QualityDef>>().await?)
}

// ---------------------------------------------------------------------------
// Edition kinds (SKADI-T-0067) — the movies-domain registry the matcher uses.
// Served under /edition-kinds (NOT /settings) by the movies module.
// ---------------------------------------------------------------------------

/// One edition-kind registry row (mirror of skadi-movies `EditionKindDto`).
#[derive(Debug, Clone, Deserialize)]
pub struct EditionKind {
    pub id: String,
    pub name: String,
    pub normalized_tag: String,
    #[serde(default)]
    pub match_patterns: Vec<String>,
    /// Built-in rows can't be deleted (the matcher needs a Theatrical fallback).
    pub builtin: bool,
}

/// `GET /edition-kinds` — list the registry.
pub async fn list_edition_kinds() -> Result<Vec<EditionKind>, ApiError> {
    let resp = get(&format!("{API_BASE}/edition-kinds"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "list edition-kinds -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<Vec<EditionKind>>().await?)
}

/// `POST /edition-kinds` — create a user edition kind. Body:
/// `{ name, normalized_tag, match_patterns }`.
pub async fn create_edition_kind(body: &Value) -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/edition-kinds"))
        .json(body)?
        .send()
        .await?;
    write_ok("create edition-kind", resp).await
}

/// `PUT /edition-kinds/{id}` — update a user edition kind.
pub async fn update_edition_kind(id: &str, body: &Value) -> Result<(), ApiError> {
    let resp = put(&format!("{API_BASE}/edition-kinds/{id}"))
        .json(body)?
        .send()
        .await?;
    write_ok("update edition-kind", resp).await
}

/// `DELETE /edition-kinds/{id}` — remove a user edition kind (builtins refuse).
pub async fn delete_edition_kind(id: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/edition-kinds/{id}"))
        .send()
        .await?;
    write_ok("delete edition-kind", resp).await
}

/// Shared "did the write succeed?" decode that surfaces the server's error text.
async fn write_ok(what: &str, resp: gloo_net::http::Response) -> Result<(), ApiError> {
    if resp.ok() {
        return Ok(());
    }
    let status = resp.status();
    let detail = resp
        .text()
        .await
        .ok()
        .and_then(|t| extract_error_message(&t))
        .unwrap_or_default();
    if detail.is_empty() {
        Err(ApiError(format!("{what} -> HTTP {status}")))
    } else {
        Err(ApiError(detail))
    }
}

// ---------------------------------------------------------------------------
// Movies domain (SKADI-T-0069) — the first domain view.
// ---------------------------------------------------------------------------

/// One edition row of a movie. `status` is the raw, externally-tagged
/// `AcquisitionStatus` (a string for unit variants, a single-key object
/// otherwise); use [`status_label`] for a coarse label. Extra fields on the
/// server struct are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct MovieEdition {
    pub id: String,
    /// The edition-kind id (UUID); resolve to a name via [`list_edition_kinds`].
    pub kind: String,
    pub status: Value,
    /// What the library scan found in the file, when it has run (SKADI-T-0585).
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
}

/// The slice of the scanner's `MediaInfo` a browser player needs to decide
/// whether it can play a file at all (SKADI-T-0585). Everything optional: the
/// scan may not have run, and older rows carry less.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct MediaInfo {
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub video: Option<MediaVideo>,
    #[serde(default)]
    pub audio: Option<MediaAudio>,
    #[serde(default)]
    pub audio_tracks: Vec<MediaAudio>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct MediaVideo {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct MediaAudio {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub channels: Option<u32>,
}

impl MediaInfo {
    /// Audio codecs present, in track order; falls back to the summary
    /// `audio` for rows scanned before per-track data existed.
    pub fn audio_codecs(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .audio_tracks
            .iter()
            .filter_map(|a| a.codec.clone())
            .collect();
        if v.is_empty() {
            v.extend(self.audio.as_ref().and_then(|a| a.codec.clone()));
        }
        v
    }
}

/// A movie library record (subset of the server `Movie`; extra fields ignored).
#[derive(Debug, Clone, Deserialize)]
pub struct Movie {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub backdrop_url: Option<String>,
    pub monitored: bool,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub content_rating: Option<String>,
    #[serde(default)]
    pub editions: Vec<MovieEdition>,
}

/// Coarse status label from a raw `AcquisitionStatus` value: the string for unit
/// variants (`"Missing"`), or the single key for data variants (`{"Imported":…}`
/// → `"imported"`). Lowercased.
pub fn status_label(status: &Value) -> String {
    match status {
        Value::String(s) => s.to_lowercase(),
        Value::Object(map) => map
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "unknown".into())
            .to_lowercase(),
        _ => "unknown".into(),
    }
}

/// One metadata search result (mirror of skadi-movies `LookupResult`).
#[derive(Debug, Clone, Deserialize)]
pub struct MovieSearchResult {
    pub tmdb_id: u64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[allow(dead_code)]
    pub score: f32,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
}

/// `GET /movies` — list the movie library (works regardless of domain enabled).
pub async fn list_movies() -> Result<Vec<Movie>, ApiError> {
    let resp = get(&format!("{API_BASE}/movies")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list movies -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Movie>>().await?)
}

/// `GET /movies/{id}` — fetch one movie (with editions) for the detail page.
/// GET `url` and parse it only when the body differs from `last` (SKADI-T-0596):
/// a polling page that re-sets its signal on every tick rebuilt its whole body
/// each time, wiping open panels and search results. `Ok(None)` means
/// "unchanged, do nothing". `last` is updated on change.
pub async fn get_if_changed<T: serde::de::DeserializeOwned>(
    url: &str,
    what: &str,
    last: &mut Option<String>,
) -> Result<Option<T>, ApiError> {
    let resp = get(url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("{what} -> HTTP {}", resp.status())));
    }
    let text = resp.text().await?;
    if last.as_deref() == Some(text.as_str()) {
        return Ok(None);
    }
    let parsed: T = serde_json::from_str(&text).map_err(|e| ApiError(format!("{what}: {e}")))?;
    *last = Some(text);
    Ok(Some(parsed))
}

/// `GET /movies/{id}`, only when it changed since `last` (see [`get_if_changed`]).
pub async fn get_movie_if_changed(
    id: &str,
    last: &mut Option<String>,
) -> Result<Option<Movie>, ApiError> {
    get_if_changed(&format!("{API_BASE}/movies/{id}"), "get movie", last).await
}

pub async fn get_movie(id: &str) -> Result<Movie, ApiError> {
    let resp = get(&format!("{API_BASE}/movies/{id}"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("get movie -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Movie>().await?)
}

/// `GET /movies/lookup?q=&year=` — metadata search for the add flow.
pub async fn search_movies(q: &str, year: Option<u16>) -> Result<Vec<MovieSearchResult>, ApiError> {
    let mut url = format!("{API_BASE}/movies/lookup?q={}", encode(q));
    if let Some(y) = year {
        url.push_str(&format!("&year={y}"));
    }
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("movie search -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<MovieSearchResult>>().await?)
}

/// `POST /movies` — add a movie by TMDB id with a profile. The library root is
/// derived server-side (`<library.root>/movie`, SKADI-T-0302).
pub async fn add_movie(tmdb_id: u64, profile: &str) -> Result<(), ApiError> {
    let body = serde_json::json!({
        "tmdb_id": tmdb_id,
        "profile": profile,
    });
    let resp = post(&format!("{API_BASE}/movies"))
        .json(&body)?
        .send()
        .await?;
    write_ok("add movie", resp).await
}

/// `PATCH /movies/{id}` — toggle the monitored flag.
pub async fn set_movie_monitored(id: &str, monitored: bool) -> Result<(), ApiError> {
    let resp = patch(&format!("{API_BASE}/movies/{id}"))
        .json(&serde_json::json!({ "monitored": monitored }))?
        .send()
        .await?;
    write_ok("update movie", resp).await
}

/// `DELETE /movies/{id}` — remove a movie (library rows only; no file deletion).
pub async fn delete_movie(id: &str, delete_files: bool) -> Result<(), ApiError> {
    let resp = delete(&format!(
        "{API_BASE}/movies/{id}?delete_files={delete_files}"
    ))
    .send()
    .await?;
    write_ok("delete movie", resp).await
}

/// `POST /movies/{id}/editions/{eid}/acquire` — trigger a manual acquire.
/// Surfaces the 409 "domain disabled" message when movies isn't enabled.
pub async fn acquire_edition(at: &AcquirablePath) -> Result<(), ApiError> {
    let url = at.url("acquire");
    let resp = post(&url).send().await.noted()?;
    write_ok("acquire", resp).await
}

/// `POST /movies/{id}/editions/{eid}/reset` — unwedge an edition stuck in a
/// non-terminal acquire state, returning it to Missing (SKADI-T-0112).
pub async fn reset_edition(at: &AcquirablePath) -> Result<(), ApiError> {
    let url = at.url("reset");
    let resp = post(&url).send().await.noted()?;
    write_ok("reset", resp).await
}

// ---------------------------------------------------------------------------
// Television (SKADI-T-0275). Series → Season → Episode, keyed by TVDB id.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeriesExternalIds {
    #[serde(default)]
    pub tvdb: Option<u64>,
    #[serde(default)]
    pub tmdb: Option<u64>,
    #[serde(default)]
    pub imdb: Option<String>,
}

/// One season of a series.
#[derive(Debug, Clone, Deserialize)]
pub struct Season {
    pub number: u16,
    pub monitored: bool,
    #[serde(default)]
    pub episode_count: u16,
    #[serde(default)]
    pub aired_count: u16,
}

/// One episode (the acquirable). `status` is the raw `AcquisitionStatus` value;
/// use [`status_label`] for a coarse label.
#[derive(Debug, Clone, Deserialize)]
pub struct Episode {
    pub id: String,
    pub season: u16,
    pub number: u16,
    #[serde(default)]
    pub absolute_number: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub air_date: Option<String>,
    pub monitored: bool,
    pub status: Value,
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
}

/// A series library record (subset of the server `Series`; extra fields ignored).
#[derive(Debug, Clone, Deserialize)]
pub struct Series {
    pub id: String,
    #[serde(default)]
    pub external_ids: SeriesExternalIds,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub series_type: Option<String>,
    pub monitored: bool,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub content_rating: Option<String>,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub backdrop_url: Option<String>,
    #[serde(default)]
    pub seasons: Vec<Season>,
    #[serde(default)]
    pub episodes: Vec<Episode>,
}

/// One series metadata search result (mirror of skadi-tv `LookupResult`).
#[derive(Debug, Clone, Deserialize)]
pub struct SeriesSearchResult {
    pub tvdb_id: u64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
}

/// `GET /series?view=summary` — list the series library.
///
/// Asks for the summary projection (SKADI-T-0494): every caller here renders a
/// library list — badges, counts, the add-page's "already have it" check — and
/// none reads an episode's title, air date, file or media info. The full shape
/// was **17.5 MB** on a prod-sized library and took 304 ms; the projection keeps
/// the fields `series_lib_status` actually reads.
///
/// `Episode` here already declares those extra fields `#[serde(default)]`, so
/// this deserialises unchanged. Use `get_series` for the detail page, which does
/// need the full episode.
pub async fn list_series() -> Result<Vec<Series>, ApiError> {
    let resp = get(&format!("{API_BASE}/series?view=summary"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list series -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Series>>().await?)
}

/// `GET /series/{id}` — fetch one series (with seasons + episodes).
/// `GET /series/{id}`, only when it changed since `last` (see [`get_if_changed`]).
pub async fn get_series_if_changed(
    id: &str,
    last: &mut Option<String>,
) -> Result<Option<Series>, ApiError> {
    get_if_changed(&format!("{API_BASE}/series/{id}"), "get series", last).await
}

pub async fn get_series(id: &str) -> Result<Series, ApiError> {
    let resp = get(&format!("{API_BASE}/series/{id}"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("get series -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Series>().await?)
}

/// `GET /series/lookup?q=` — series metadata search for the add flow.
pub async fn search_series(q: &str) -> Result<Vec<SeriesSearchResult>, ApiError> {
    let url = format!("{API_BASE}/series/lookup?q={}", encode(q));
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("series search -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<SeriesSearchResult>>().await?)
}

/// `POST /series` — add a series by TVDB id with a profile + monitor mode
/// (`all`/`future`/`missing`/`firstSeason`/`pilot`/`none`/…). The library root is
/// derived server-side (`<library.root>/television`, SKADI-T-0302).
pub async fn add_series(tvdb_id: u64, profile: &str, monitor: &str) -> Result<(), ApiError> {
    let body = serde_json::json!({
        "tvdb_id": tvdb_id,
        "profile": profile,
        "monitor": monitor,
    });
    let resp = post(&format!("{API_BASE}/series"))
        .json(&body)?
        .send()
        .await?;
    write_ok("add series", resp).await
}

/// `PATCH /series/{id}` — toggle the series monitored flag.
pub async fn set_series_monitored(id: &str, monitored: bool) -> Result<(), ApiError> {
    let resp = patch(&format!("{API_BASE}/series/{id}"))
        .json(&serde_json::json!({ "monitored": monitored }))?
        .send()
        .await?;
    write_ok("update series", resp).await
}

/// `DELETE /series/{id}` — remove a series (library rows only).
pub async fn delete_series(id: &str, delete_files: bool) -> Result<(), ApiError> {
    let resp = delete(&format!(
        "{API_BASE}/series/{id}?delete_files={delete_files}"
    ))
    .send()
    .await?;
    write_ok("delete series", resp).await
}

/// `POST /series/{id}/seasons/{n}/monitor` — toggle a whole season (cascades to
/// its episodes server-side).
pub async fn monitor_season(series_id: &str, season: u16, monitored: bool) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/series/{series_id}/seasons/{season}/monitor");
    let resp = post(&url)
        .json(&serde_json::json!({ "monitored": monitored }))?
        .send()
        .await?;
    write_ok("monitor season", resp).await
}

/// `POST /series/{id}/episodes/{eid}/monitor` — toggle one episode.
pub async fn monitor_episode(
    series_id: &str,
    episode_id: &str,
    monitored: bool,
) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/series/{series_id}/episodes/{episode_id}/monitor");
    let resp = post(&url)
        .json(&serde_json::json!({ "monitored": monitored }))?
        .send()
        .await?;
    write_ok("monitor episode", resp).await
}

// ---------------------------------------------------------------------------
// Wanted (SKADI-T-0594): the acquisition backlog across every domain.
// ---------------------------------------------------------------------------

/// One unsatisfied edition / episode / book file of a wanted item (mirror of
/// skadi-api `LibraryEditionDto`, trimmed to what the page shows).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WantedEdition {
    pub id: String,
    /// `S01E02` for an episode, the edition-kind id for a movie, `audiobook`
    /// for a book file.
    pub kind: String,
    /// `missing` / `failed` / `searching` / …
    pub status_kind: String,
    #[serde(default)]
    pub kind_name: Option<String>,
    #[serde(default)]
    pub quality_name: Option<String>,
}

/// A monitored item that still needs at least one file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WantedItem {
    /// `series` / `movie` / `audiobook`.
    pub kind: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub editions: Vec<WantedEdition>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WantedSummary {
    #[serde(default)]
    pub items: usize,
    #[serde(default)]
    pub editions: usize,
    #[serde(default)]
    pub by_status: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WantedResponse {
    #[serde(default)]
    pub summary: WantedSummary,
    #[serde(default)]
    pub items: Vec<WantedItem>,
}

/// `GET /wanted` — every monitored item with an unsatisfied edition, all domains.
pub async fn wanted() -> Result<WantedResponse, ApiError> {
    let resp = get(&format!("{API_BASE}/wanted")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("wanted -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<WantedResponse>().await?)
}

// ---------------------------------------------------------------------------
// Interactive search + grab + blocklist (SKADI-T-0114/T-0115/T-0117).
// ---------------------------------------------------------------------------

/// One scored release candidate (mirror of skadi-movies `ReleaseCandidate`).
/// `release` is the raw object echoed back to [`grab_release`]; display fields
/// are read off it via the accessors.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseCandidate {
    /// Echo this back to grab.
    pub release: Value,
    /// Canonical blocklist identity (block/unblock without recomputing).
    pub release_key: String,
    pub quality: String,
    pub age_days: i64,
    pub accepted: bool,
    pub reason: String,
    /// Title-match score, 0.0-1.0 (SKADI-T-0181). The list arrives sorted by it,
    /// so the UI can surface it without re-deriving anything.
    #[serde(default)]
    pub relevance: f32,
    /// Whether this release is a whole-season pack (SKADI-T-0558, television
    /// only). Grabbing one satisfies every episode in the season, not just the
    /// row it was clicked from — the UI has to say so *before* the click.
    #[serde(default)]
    pub season_pack: bool,
}

impl ReleaseCandidate {
    pub fn title(&self) -> String {
        self.release
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(untitled)")
            .to_string()
    }
    pub fn size_bytes(&self) -> u64 {
        self.release
            .get("size")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }
    pub fn seeders(&self) -> Option<u64> {
        self.release.get("seeders").and_then(|v| v.as_u64())
    }
    pub fn blocklisted(&self) -> bool {
        !self.accepted && self.reason == "blocklisted"
    }
}

/// The REST path of one acquirable unit: `/movies/{id}/editions/{eid}` or
/// `/series/{id}/episodes/{eid}`.
///
/// Since SKADI-T-0558 the manual-acquire routes hang off both identically, so
/// the API helpers and the releases panel take this rather than a pair of ids
/// plus a hardcoded domain. Without it, television would need a second copy of
/// each helper and of the panel's markup — the duplication SKADI-T-0427 warned
/// about, where two copies drift and only one gets the fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcquirablePath(String);

impl AcquirablePath {
    #[must_use]
    pub fn movie_edition(movie_id: &str, edition_id: &str) -> Self {
        Self(format!("/movies/{movie_id}/editions/{edition_id}"))
    }

    #[must_use]
    pub fn tv_episode(series_id: &str, episode_id: &str) -> Self {
        Self(format!("/series/{series_id}/episodes/{episode_id}"))
    }

    #[must_use]
    pub fn book_file(book_id: &str, file_id: &str) -> Self {
        Self(format!("/books/{book_id}/files/{file_id}"))
    }

    fn url(&self, action: &str) -> String {
        format!("{API_BASE}{}/{action}", self.0)
    }
}

/// `GET …/releases` — live interactive search for one acquirable.
pub async fn list_releases(at: &AcquirablePath) -> Result<Vec<ReleaseCandidate>, ApiError> {
    let url = at.url("releases");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("search releases -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<Vec<ReleaseCandidate>>().await?)
}

/// `POST …/grab` — grab a specific release.
pub async fn grab_release(at: &AcquirablePath, release: &Value) -> Result<(), ApiError> {
    let url = at.url("grab");
    let resp = post(&url)
        .json(&serde_json::json!({ "release": release }))?
        .send()
        .await?;
    write_ok("grab", resp).await
}

/// `POST /movies/{id}/editions/{eid}/grab-link` — manual acquisition: hand a pasted
/// magnet or `.torrent` URL to the edition's acquire pipeline (SKADI-I-0043).
pub async fn grab_link(
    at: &AcquirablePath,
    link: &str,
    title: Option<&str>,
) -> Result<(), ApiError> {
    let url = at.url("grab-link");
    let resp = post(&url)
        .json(&serde_json::json!({ "link": link, "title": title }))?
        .send()
        .await?;
    write_ok("grab link", resp).await
}

/// One blocklist entry (subset of the server DTO).
#[derive(Debug, Clone, Deserialize)]
pub struct BlocklistEntry {
    pub id: String,
    pub release_key: String,
}

/// `GET /blocklist[?acquirable=]` — blocklisted releases (optionally scoped).
pub async fn list_blocklist(acquirable: Option<&str>) -> Result<Vec<BlocklistEntry>, ApiError> {
    let url = match acquirable {
        Some(a) => format!("{API_BASE}/blocklist?acquirable={}", encode(a)),
        None => format!("{API_BASE}/blocklist"),
    };
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "list blocklist -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<Vec<BlocklistEntry>>().await?)
}

/// `POST /blocklist` — manually block a release.
pub async fn block_release(
    release_key: &str,
    title: &str,
    acquirable_ref: &str,
) -> Result<(), ApiError> {
    let body = serde_json::json!({
        "release_key": release_key,
        "title": title,
        "acquirable_ref": acquirable_ref,
        "reason": "manual",
    });
    let resp = post(&format!("{API_BASE}/blocklist"))
        .json(&body)?
        .send()
        .await?;
    write_ok("block", resp).await
}

/// `DELETE /blocklist/{id}` — clear one blocklist entry.
pub async fn unblock_release(id: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/blocklist/{id}"))
        .send()
        .await
        .noted()?;
    write_ok("unblock", resp).await
}

// ---------------------------------------------------------------------------
// Diagnostics (SKADI-T-0116) — health checks + root-folder free space.
// ---------------------------------------------------------------------------

/// One daemon health check (mirror of skadi-api `CheckResult`; `name`,
/// `status` and `detail` are its legacy projection).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HealthCheck {
    /// The check id (`database`, `indexers`, `indexer:<name>`, …).
    pub name: String,
    /// `ok` | `warn` | `fail` (legacy: `warn` also covers pending).
    pub status: String,
    pub detail: String,
    /// `ok` | `warn` | `error` | `pending`. Absent from a daemon older than
    /// the severity model.
    #[serde(default)]
    pub severity: Option<String>,
    /// Human name for the check (SKADI-T-0679). Absent from an older daemon.
    #[serde(default)]
    pub label: Option<String>,
    /// How to fix it; set for `warn` and `error`.
    #[serde(default)]
    pub remediation: Option<String>,
    /// RFC 3339 time of the last run; `None` while the check is pending.
    #[serde(default)]
    pub checked_at: Option<String>,
}

impl HealthCheck {
    /// The level of the check: the daemon's `severity`, or, from an older
    /// daemon, the legacy `status` mapped onto it.
    pub fn level(&self) -> &str {
        match self.severity.as_deref() {
            Some(s) => s,
            None => match self.status.as_str() {
                "ok" => "ok",
                "warn" => "warn",
                _ => "error",
            },
        }
    }
}

/// Free-space + writability report for one root folder (mirror of
/// skadi-api `RootFolderReport`).
#[derive(Debug, Clone, Deserialize)]
pub struct RootFolder {
    pub path: String,
    pub exists: bool,
    pub writable: bool,
    #[serde(default)]
    pub free_bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
}

/// `GET /health/checks` — aggregated daemon/db/domain/provider health.
pub async fn health_checks() -> Result<Vec<HealthCheck>, ApiError> {
    let resp = get(&format!("{API_BASE}/health/checks"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("health checks -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<HealthCheck>>().await?)
}

/// `POST /health/checks/run` — run every check now and return them all, as
/// `GET /health/checks` would (SKADI-T-0680). Admin only.
pub async fn run_health_checks() -> Result<Vec<HealthCheck>, ApiError> {
    let resp = post(&format!("{API_BASE}/health/checks/run"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "run health checks -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<Vec<HealthCheck>>().await?)
}

/// One domain in `GET /system/status`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemDomain {
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub worker_failures: u64,
}

/// `GET /system/status` (mirror of skadi-api `SystemStatus`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub version: String,
    /// The build commit (SKADI-T-0684): a git sha, or `unknown`. Absent from a
    /// daemon older than the field.
    #[serde(default)]
    pub commit: Option<String>,
    pub start_time: String,
    pub uptime_seconds: i64,
    /// `postgres` or `sqlite`.
    pub database: String,
    pub library_root: String,
    #[serde(default)]
    pub domains: Vec<SystemDomain>,
}

/// One recurring job in `GET /system/task`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemTask {
    pub name: String,
    pub interval_seconds: u64,
    #[serde(default)]
    pub what: String,
    /// Empty until the daemon records task runs (SKADI-T-0440).
    #[serde(default)]
    pub last_run: Option<String>,
    #[serde(default)]
    pub next_run: Option<String>,
}

/// One line of the daemon's log ring (`GET /log`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub time: String,
    /// `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE`.
    pub level: String,
    #[serde(default)]
    pub target: String,
    pub message: String,
}

/// `GET /system/status` — version, commit, uptime, database, library root.
pub async fn system_status() -> Result<SystemStatus, ApiError> {
    let resp = get(&format!("{API_BASE}/system/status"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("system status -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<SystemStatus>().await?)
}

/// `GET /system/task` — the recurring jobs and their cadence.
pub async fn system_tasks() -> Result<Vec<SystemTask>, ApiError> {
    let resp = get(&format!("{API_BASE}/system/task"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("system tasks -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<SystemTask>>().await?)
}

/// `GET /log?pageSize=n` — the newest `page_size` log lines, newest first.
pub async fn system_log(page_size: usize) -> Result<Vec<LogLine>, ApiError> {
    let resp = get(&format!("{API_BASE}/log?pageSize={page_size}"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("log -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<LogLine>>().await?)
}

/// `GET /root-folders` — per root folder free/total bytes + writability.
pub async fn root_folders() -> Result<Vec<RootFolder>, ApiError> {
    let resp = get(&format!("{API_BASE}/root-folders"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("root folders -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<RootFolder>>().await?)
}

/// Download progress (0.0–1.0) from a raw `AcquisitionStatus`, when it's a
/// `Downloading { progress }` variant; otherwise `None`.
pub fn download_progress(status: &Value) -> Option<f32> {
    status
        .get("Downloading")?
        .get("progress")
        .and_then(|v| v.as_f64())
        .map(|p| p as f32)
}

// ---------------------------------------------------------------------------
// Library import (SKADI-T-0075) — scan an on-disk library + commit in place.
// ---------------------------------------------------------------------------

/// The best metadata match proposed for a scanned candidate (mirror of
/// skadi-movies `ProposedMatch`).
#[derive(Debug, Clone, Deserialize)]
pub struct ProposedMatch {
    pub tmdb_id: u64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[allow(dead_code)]
    pub score: f32,
}

/// One scanned library candidate, **parse-only** (mirror of skadi-movies
/// `ScanCandidateDto`). Metadata matching is a separate, lazy step — see
/// [`library_import_match`].
#[derive(Debug, Clone, Deserialize)]
pub struct ScanCandidate {
    /// Existing video file path (recorded verbatim on import — in place); also
    /// the stable key used to merge match results back in.
    pub path: String,
    pub display_name: String,
    #[serde(default)]
    pub parsed_title: Option<String>,
    #[serde(default)]
    pub parsed_year: Option<u16>,
    /// TMDB id parsed from the folder/file name (Radarr-style) or the NFO
    /// sidecar, when present.
    #[serde(default)]
    pub tmdb_id: Option<u64>,
    #[serde(default)]
    pub quality_id: Option<String>,
    #[serde(default)]
    pub quality_name: Option<String>,
    /// Display metadata from `movie.nfo` (SKADI-T-0328).
    #[serde(default)]
    pub nfo_overview: Option<String>,
    #[serde(default)]
    pub nfo_genres: Vec<String>,
    #[serde(default)]
    pub nfo_rating: Option<String>,
    #[serde(default)]
    pub nfo_studio: Option<String>,
}

/// The metadata match resolved for one candidate (mirror of `MatchDto`), keyed
/// by `path`.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchResult {
    pub path: String,
    #[serde(default)]
    pub proposed: Option<ProposedMatch>,
    /// `high` (parsed year agrees), `low` (mismatch/unsure), or `none`.
    pub confidence: String,
    pub already_in_library: bool,
}

/// Outcome of a commit (mirror of skadi-movies `CommitResult`).
#[derive(Debug, Clone, Deserialize)]
pub struct CommitResult {
    pub imported: usize,
    pub skipped: usize,
    /// Of `imported`: hardlinked into the canonical layout (adoption move — the
    /// source copy is removed after the link, SKADI-T-0303).
    #[serde(default)]
    pub linked: usize,
    /// Of `imported`: already at their canonical path — no fs op.
    #[serde(default)]
    pub in_place: usize,
    /// Files left in place because they matched no TVDB episode or were already
    /// imported — benign, not errors (TV library-import, SKADI-T-0324).
    #[serde(default)]
    pub unmatched: Vec<String>,
    pub errors: Vec<String>,
}

/// `POST /library-import/scan` — scan `path` into proposed candidates (read-only).
pub async fn library_import_scan(path: &str) -> Result<Vec<ScanCandidate>, ApiError> {
    let body = serde_json::json!({ "path": path });
    let resp = post(&format!("{API_BASE}/library-import/scan"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("scan -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<Vec<ScanCandidate>>().await?)
}

/// One candidate to resolve a match for: its `path` key, parsed title/year, and
/// an optional TMDB id (folder-parsed or operator-typed) that forces an exact
/// lookup.
pub struct MatchQuery {
    pub path: String,
    pub title: Option<String>,
    pub year: Option<u16>,
    pub tmdb_id: Option<u64>,
}

/// `POST /library-import/match` — resolve metadata matches for a page of scanned
/// candidates; results come back keyed by `path`. Called lazily per page so a
/// big library never matches all at once.
pub async fn library_import_match(items: &[MatchQuery]) -> Result<Vec<MatchResult>, ApiError> {
    let items: Vec<Value> = items
        .iter()
        .map(|q| {
            serde_json::json!({
                "path": q.path,
                "title": q.title,
                "year": q.year,
                "tmdb_id": q.tmdb_id,
            })
        })
        .collect();
    let body = serde_json::json!({ "items": items });
    let resp = post(&format!("{API_BASE}/library-import/match"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("match -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<MatchResult>>().await?)
}

/// One confirmed item to import in place (`path` + chosen `tmdb_id` + detected
/// `quality_id`). Built into the commit request body.
pub struct CommitItem {
    pub path: String,
    pub tmdb_id: u64,
    pub quality_id: Option<String>,
}

/// `POST /library-import/commit` — create the confirmed items as `Imported`
/// movies in place. No profile/root: the upgrade profile defaults server-side
/// and the root is derived (SKADI-T-0304/0302); the file's current quality is
/// detected per item.
pub async fn library_import_commit(items: &[CommitItem]) -> Result<CommitResult, ApiError> {
    let items: Vec<Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "path": i.path,
                "tmdb_id": i.tmdb_id,
                "quality_id": i.quality_id,
            })
        })
        .collect();
    let body = serde_json::json!({
        "items": items,
    });
    let resp = post(&format!("{API_BASE}/library-import/commit"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("commit -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<CommitResult>().await?)
}

// ---------------------------------------------------------------------------
// Audiobook library import (SKADI-T-0134) — scan an on-disk audiobook tree +
// commit by ASIN (hardlink into the canonical layout; adoption move — sources
// are removed after a fresh link, SKADI-T-0303).
// Namespaced under `/audiobooks/library-import/*` because movies owns the bare
// `/library-import/*` and both merge under `/api/v1`.
// ---------------------------------------------------------------------------

/// The Audnexus match proposed for a scanned audiobook (mirror of the backend
/// `ProposedMatch`). Matching is ASIN-driven (Audnexus has no title search).
#[derive(Debug, Clone, Deserialize)]
pub struct AbProposedMatch {
    pub asin: String,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub year: Option<u16>,
}

/// One scanned audiobook candidate, **parse-only** (mirror of the backend
/// `ScanCandidateDto`).
#[derive(Debug, Clone, Deserialize)]
pub struct AbScanCandidate {
    /// The representative audio source path (first file) — also the stable key
    /// used to merge match results back in.
    pub path: String,
    /// All audio source files placed on commit.
    pub files: Vec<String>,
    pub display_name: String,
    #[serde(default)]
    pub parsed_author: Option<String>,
    #[serde(default)]
    pub parsed_title: Option<String>,
    #[serde(default)]
    pub parsed_series: Option<String>,
    #[serde(default)]
    pub parsed_series_position: Option<String>,
    /// ASIN parsed from the folder/file name (canonical `{asin-…}` tag), if any.
    #[serde(default)]
    pub asin: Option<String>,
    pub single_file: bool,
}

/// The ASIN-keyed match resolved for one candidate (mirror of `MatchDto`).
#[derive(Debug, Clone, Deserialize)]
pub struct AbMatchResult {
    pub path: String,
    #[serde(default)]
    pub proposed: Option<AbProposedMatch>,
    /// `high` (ASIN looked up) or `none` (operator must paste an ASIN).
    pub confidence: String,
    pub already_in_library: bool,
}

/// `POST /audiobooks/library-import/scan` — scan `path` into candidates (read-only).
pub async fn ab_library_import_scan(path: &str) -> Result<Vec<AbScanCandidate>, ApiError> {
    let body = serde_json::json!({ "path": path });
    let resp = post(&format!("{API_BASE}/audiobooks/library-import/scan"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("scan -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<Vec<AbScanCandidate>>().await?)
}

/// One candidate to resolve a match for: its `path` key and an optional ASIN
/// (folder-parsed or operator-typed) — Audnexus needs an ASIN to look up.
pub struct AbMatchQuery {
    pub path: String,
    pub asin: Option<String>,
}

/// `POST /audiobooks/library-import/match` — resolve ASIN-keyed matches for a page.
pub async fn ab_library_import_match(
    items: &[AbMatchQuery],
) -> Result<Vec<AbMatchResult>, ApiError> {
    let items: Vec<Value> = items
        .iter()
        .map(|q| serde_json::json!({ "path": q.path, "asin": q.asin }))
        .collect();
    let body = serde_json::json!({ "items": items });
    let resp = post(&format!("{API_BASE}/audiobooks/library-import/match"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("match -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<AbMatchResult>>().await?)
}

/// One confirmed audiobook to import (`files` + confirmed `asin` + optional
/// detected `quality_id`).
pub struct AbCommitItem {
    pub files: Vec<String>,
    pub asin: String,
    pub quality_id: Option<String>,
}

/// `POST /audiobooks/library-import/commit` — build each item from Audnexus and
/// hardlink into the canonical layout. The root is derived server-side
/// (`<library.root>/audiobook`, SKADI-T-0302); `profile` is omitted (`None`) so
/// the backend binds the built-in audiobook ladder (SKADI-T-0301).
pub async fn ab_library_import_commit(
    profile: Option<&str>,
    items: &[AbCommitItem],
) -> Result<CommitResult, ApiError> {
    let items: Vec<Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "files": i.files,
                "asin": i.asin,
                "quality_id": i.quality_id,
            })
        })
        .collect();
    let body = serde_json::json!({
        "profile": profile,
        "items": items,
    });
    let resp = post(&format!("{API_BASE}/audiobooks/library-import/commit"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("commit -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<CommitResult>().await?)
}

// ---------------------------------------------------------------------------
// TV library import — scan an on-disk TV tree + commit in place by TVDB id.
// Namespaced under `/tv/library-import/*` because movies owns the bare
// `/library-import/*` and all merge under `/api/v1`. The scan unit is one
// episode file; rows are reviewed grouped by series → SxxExx.
// ---------------------------------------------------------------------------

/// The best series match proposed for a scanned episode (mirror of the backend
/// `ProposedMatch`).
#[derive(Debug, Clone, Deserialize)]
pub struct TvProposedMatch {
    pub tvdb_id: u64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub poster_url: Option<String>,
}

/// One scanned TV candidate (one episode file), **parse-only** (mirror of the
/// backend `ScanCandidateDto`). Metadata matching is a separate, lazy step —
/// see [`tv_library_import_match`].
#[derive(Debug, Clone, Deserialize)]
pub struct TvScanCandidate {
    /// Existing episode file path (recorded verbatim on import — in place); also
    /// the stable key used to merge match results back in.
    pub path: String,
    pub display_name: String,
    #[serde(default)]
    pub series_title: Option<String>,
    #[serde(default)]
    pub season: Option<u16>,
    #[serde(default)]
    pub episodes: Vec<u16>,
    #[serde(default)]
    pub absolute: Vec<u16>,
    #[serde(default)]
    pub air_date: Option<String>,
    #[serde(default)]
    pub quality_id: Option<String>,
    #[serde(default)]
    pub quality_name: Option<String>,
    /// Show folder under the scan root — the import UI groups by this.
    #[serde(default)]
    pub folder: Option<String>,
    /// Exact TVDB id + real title/year from the folder's `tvshow.nfo`, when present
    /// — an authoritative match that skips the fuzzy search.
    #[serde(default)]
    pub nfo_tvdb_id: Option<u64>,
    #[serde(default)]
    pub nfo_title: Option<String>,
    #[serde(default)]
    pub nfo_year: Option<u16>,
    /// Display metadata from `tvshow.nfo` to confirm a match at a glance.
    #[serde(default)]
    pub nfo_overview: Option<String>,
    #[serde(default)]
    pub nfo_genres: Vec<String>,
    #[serde(default)]
    pub nfo_status: Option<String>,
    #[serde(default)]
    pub nfo_network: Option<String>,
    #[serde(default)]
    pub nfo_rating: Option<String>,
}

/// The series match resolved for one candidate (mirror of `MatchDto`), keyed by
/// `path`.
#[derive(Debug, Clone, Deserialize)]
pub struct TvMatchResult {
    pub path: String,
    #[serde(default)]
    pub proposed: Option<TvProposedMatch>,
    /// `high` / `low` / `none`.
    pub confidence: String,
    #[serde(default)]
    pub needs_review: bool,
    pub already_in_library: bool,
}

/// `POST /tv/library-import/scan` — scan `path` into proposed candidates (read-only).
pub async fn tv_library_import_scan(path: &str) -> Result<Vec<TvScanCandidate>, ApiError> {
    let body = serde_json::json!({ "path": path });
    let resp = post(&format!("{API_BASE}/tv/library-import/scan"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("scan -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<Vec<TvScanCandidate>>().await?)
}

/// One candidate to resolve a match for: its `path` key, parsed series title,
/// and an optional first-aired year that helps disambiguate.
#[derive(Serialize)]
pub struct TvMatchQuery {
    pub path: String,
    pub series_title: Option<String>,
    pub year: Option<u16>,
}

/// `POST /tv/library-import/match` — resolve series matches for a page of scanned
/// candidates; results come back keyed by `path`. Called lazily per page so a
/// big library never matches all at once.
pub async fn tv_library_import_match(
    items: &[TvMatchQuery],
) -> Result<Vec<TvMatchResult>, ApiError> {
    let body = serde_json::json!({ "items": items });
    let resp = post(&format!("{API_BASE}/tv/library-import/match"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("match -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<TvMatchResult>>().await?)
}

/// One confirmed episode to import in place (`path` + chosen `tvdb_id` +
/// detected `quality_id`). Built into the commit request body.
///
/// `season` + `episode` are an **optional manual mapping**: when both are set
/// the backend places the file at that exact episode; when absent it re-parses
/// the filename as before. Auto-mapped files therefore serialize exactly as
/// they used to (the two fields are skipped).
#[derive(Serialize)]
pub struct TvCommitItem {
    pub path: String,
    pub tvdb_id: u64,
    pub quality_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u16>,
}

/// One season in a series' full structure (`number` + how many episodes it has).
#[derive(Debug, Clone, Deserialize)]
pub struct TvStructSeason {
    pub number: u16,
    #[serde(default)]
    pub episode_count: u16,
}

/// One episode in a series' full structure (its `season`, `number`, and title).
#[derive(Debug, Clone, Deserialize)]
pub struct TvStructEpisode {
    pub season: u16,
    pub number: u16,
    #[serde(default)]
    pub title: Option<String>,
}

/// A series' complete season/episode tree, used to drive the hierarchical
/// library-import review (no DB write — pure metadata).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TvSeriesStructure {
    #[serde(default)]
    pub seasons: Vec<TvStructSeason>,
    #[serde(default)]
    pub episodes: Vec<TvStructEpisode>,
}

/// `GET /tv/library-import/series-structure?tvdb=<id>` — the show's full
/// season/episode tree, fetched lazily per show so each import card can render
/// every episode (mapped or not). Read-only; never touches the library DB.
pub async fn tv_import_series_structure(tvdb: u64) -> Result<TvSeriesStructure, ApiError> {
    let url = format!("{API_BASE}/tv/library-import/series-structure?tvdb={tvdb}");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "series-structure -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<TvSeriesStructure>().await?)
}

/// `POST /tv/library-import/commit` — create the confirmed episodes as imported
/// in place under their series. `profile` is omitted (`None`) so the backend
/// binds its default; the file's current quality is detected per item.
pub async fn tv_library_import_commit(
    items: &[TvCommitItem],
    profile: Option<&str>,
) -> Result<CommitResult, ApiError> {
    let body = serde_json::json!({
        "profile": profile,
        "items": items,
    });
    let resp = post(&format!("{API_BASE}/tv/library-import/commit"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("commit -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<CommitResult>().await?)
}

// ---------------------------------------------------------------------------
// Activity (live) + History (persistent) — SKADI-T-0082.
// ---------------------------------------------------------------------------

/// One in-flight acquire run (mirror of skadi-hunter `RunMeta`).
#[derive(Debug, Clone, Deserialize)]
pub struct ActivityRun {
    #[allow(dead_code)]
    pub run_id: String,
    pub acquirable_ref: String,
    #[serde(default)]
    pub started_at: Option<String>,
    pub current_stage: String,
    /// The release `decide` chose, once chosen (acquisition transparency).
    #[serde(default)]
    pub chosen_title: Option<String>,
    /// How many candidate releases `decide` weighed before choosing.
    #[serde(default)]
    pub candidates_considered: Option<usize>,
    /// The profile decision for the chosen release (`Accept`/`Upgrade`/…).
    #[serde(default)]
    pub decision: Option<String>,
    /// The item's name (`Title`, `Show S01E02`, `Show Season 1`), from the
    /// server (SKADI-T-0690).
    #[serde(default)]
    pub title: Option<String>,
    /// The transfer's download row, merged in server-side for a run at
    /// `snatching`/`downloading` (SKADI-T-0690); the fields below come from it.
    #[serde(default)]
    pub download_id: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub downloaded_bytes: Option<i64>,
    #[serde(default)]
    pub eta_seconds: Option<i64>,
    #[serde(default)]
    pub down_speed_bps: Option<i64>,
}

/// One persistent history row (mirror of skadi-api `HistoryDto`).
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryRow {
    pub id: String,
    /// RFC 3339 timestamp.
    pub at: String,
    #[allow(dead_code)]
    pub kind: String,
    #[allow(dead_code)]
    pub acquirable_ref: String,
    pub label: String,
    /// `grabbed` / `imported` / `failed`.
    pub event: String,
    #[serde(default)]
    pub detail: Option<String>,
    /// Structured failure reason code (SKADI-T-0200), set on `failed` events.
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// `GET /activity` — in-flight acquire runs (empty when idle).
pub async fn activity() -> Result<Vec<ActivityRun>, ApiError> {
    let resp = get(&format!("{API_BASE}/activity")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("activity -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<ActivityRun>>().await?)
}

/// `GET /history?limit=` — persistent acquisition history, newest first.
pub async fn history(limit: u32) -> Result<Vec<HistoryRow>, ApiError> {
    let resp = get(&format!("{API_BASE}/history?limit={limit}"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("history -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<HistoryRow>>().await?)
}

/// `GET /history?acquirable=&limit=` — the timeline for ONE acquirable (item), newest
/// first: its grabbed / imported / failed events, with failure reason codes (SKADI-T-0315).
pub async fn item_history(acquirable_ref: &str, limit: u32) -> Result<Vec<HistoryRow>, ApiError> {
    let url = format!("{API_BASE}/history?acquirable={acquirable_ref}&limit={limit}");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("item history -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<HistoryRow>>().await?)
}

/// One hunter trace event (mirror of the api `TraceDto`, SKADI-T-0323): a
/// structured per-step record of what the acquire pipeline did.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceRow {
    #[allow(dead_code)]
    pub id: String,
    /// RFC 3339 timestamp.
    pub at: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub run_id: Option<String>,
    #[allow(dead_code)]
    pub kind: String,
    pub acquirable_ref: String,
    /// `searching` / `deciding` / `grabbing` / `downloading` / `importing`.
    pub stage: String,
    /// `candidates_found` / `decision` / `no_release` / `snatched` / `download_failed` / …
    pub event: String,
    pub message: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// `GET /traces?limit=` — the hunter's structured per-step trace stream, newest first.
pub async fn traces(limit: u32) -> Result<Vec<TraceRow>, ApiError> {
    let resp = get(&format!("{API_BASE}/traces?limit={limit}"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("traces -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<TraceRow>>().await?)
}

/// `GET /traces?acquirable=` — the trace stream for one acquirable, newest
/// first (SKADI-T-0381). Powers the per-item "why isn't this grabbed?" panel.
pub async fn item_traces(acquirable: &str) -> Result<Vec<TraceRow>, ApiError> {
    // Refs are UUIDs or `season-<uuid>-<n>` — URL-safe as-is, same as
    // `item_history` above.
    let url = format!("{API_BASE}/traces?acquirable={acquirable}");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("item traces -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<TraceRow>>().await?)
}

/// `POST /history/{id}/blocklist-and-search` — ban a bad grab and re-acquire (arr parity,
/// SKADI-T-0315).
pub async fn blocklist_and_search(history_id: &str) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/history/{history_id}/blocklist-and-search");
    let resp = post(&url).send().await.noted()?;
    write_ok("blocklist-and-search", resp).await
}

/// One active download job with live torrent metrics (mirror of the api
/// `DownloadDto`, SKADI-T-0166). Speeds are bytes/sec; `percent`/`ratio` computed.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Download {
    pub id: String,
    pub acquirable_ref: String,
    pub status: String,
    pub progress_bytes: i64,
    pub total_bytes: i64,
    pub percent: f64,
    #[serde(default)]
    pub down_speed_bps: Option<i64>,
    #[serde(default)]
    pub up_speed_bps: Option<i64>,
    #[serde(default)]
    pub ratio: Option<f64>,
    #[serde(default)]
    pub peers: Option<i32>,
    #[serde(default)]
    pub peers_seen: Option<i32>,
    #[serde(default)]
    pub eta_seconds: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
    /// When the job was enqueued (RFC 3339, UTC, fixed width, so it sorts as a
    /// string). `None` from a daemon older than SKADI-T-0686.
    #[serde(default)]
    pub created_at: Option<String>,
    /// What a manual import of this transfer needs, and how its import went
    /// (SKADI-T-0689). `None` unless it finished and its grab target is known.
    #[serde(default)]
    pub import: Option<DownloadImport>,
}

/// The manual-import facts of one finished transfer (mirror of the api
/// `DownloadImportDto`, SKADI-T-0689).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct DownloadImport {
    /// The media kind as `POST /downloads/import` takes it (`movie`, `series`, …).
    pub kind: String,
    /// The item the transfer was grabbed for.
    pub acquirable_ref: String,
    /// The file, or the folder of the files, to scan.
    pub path: String,
    /// `imported`, `failed`, `pending` or `not_imported`.
    pub state: String,
    /// Why the import failed, for `failed`.
    #[serde(default)]
    pub error: Option<String>,
}

/// The body of `POST /downloads/import` and `/downloads/import/preview`
/// (SKADI-T-0222 / SKADI-T-0224).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ManualImportRequest {
    pub path: String,
    pub kind: String,
    pub acquirable_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl ManualImportRequest {
    /// The request that imports `facts` (the row's own path, kind and target).
    pub fn for_download(facts: &DownloadImport) -> Self {
        Self {
            path: facts.path.clone(),
            kind: facts.kind.clone(),
            acquirable_ref: facts.acquirable_ref.clone(),
            category: None,
        }
    }
}

/// `POST /downloads/import/preview` answer: what an import would do, nothing
/// touched. `would_import` is `(acquirable_ref, dest, action)`, action `place` or
/// `replace`; `would_reject` is `(path, reason)`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ImportPlan {
    #[serde(default)]
    pub would_import: Vec<(String, String, String)>,
    #[serde(default)]
    pub would_reject: Vec<(String, String)>,
    #[serde(default)]
    pub would_replace: Vec<String>,
}

/// `POST /downloads/import` answer: what the import did. `rejected` and
/// `failed` are `(path, reason)`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ImportOutcome {
    #[serde(default)]
    pub imported: Vec<String>,
    #[serde(default)]
    pub rejected: Vec<(String, String)>,
    #[serde(default)]
    pub replaced: Vec<String>,
    #[serde(default)]
    pub failed: Vec<(String, String)>,
}

/// POST `body` as JSON to `url` and read a `T` back; a refusal becomes the
/// server's own message (else `what -> HTTP <status>`).
async fn post_json<T: serde::de::DeserializeOwned>(
    what: &str,
    url: &str,
    body: &impl Serialize,
) -> Result<T, ApiError> {
    let resp = post(url).json(body)?.send().await.noted()?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("{what} -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<T>().await?)
}

/// `POST /downloads/import/preview` — dry-run a manual import (SKADI-T-0689).
pub async fn preview_import(req: &ManualImportRequest) -> Result<ImportPlan, ApiError> {
    post_json(
        "import preview",
        &format!("{API_BASE}/downloads/import/preview"),
        req,
    )
    .await
}

/// `POST /downloads/import` — run a manual import (SKADI-T-0689).
pub async fn run_import(req: &ManualImportRequest) -> Result<ImportOutcome, ApiError> {
    post_json("import", &format!("{API_BASE}/downloads/import"), req).await
}

/// `GET /downloads` — the active download queue (queued + downloading) with live
/// torrent metrics. Empty when nothing is downloading.
pub async fn list_downloads() -> Result<Vec<Download>, ApiError> {
    let resp = get(&format!("{API_BASE}/downloads")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("downloads -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Download>>().await?)
}

/// Built-in torrent worker liveness (SKADI-T-0288) — from its DB heartbeat, so it
/// reflects the actual worker *process*, not whether a downloader is registered.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerStatus {
    pub running: bool,
    #[serde(default)]
    pub version: Option<String>,
    /// Seconds since the last heartbeat (`None` if never seen).
    #[serde(default)]
    pub age_secs: Option<i64>,
    #[allow(dead_code)]
    pub stale_after_secs: i64,
    /// Free / total bytes on the download filesystem (always-on disk stat).
    #[serde(default)]
    pub free_bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
}

/// `GET /downloads/worker` — is the built-in torrent worker alive?
pub async fn worker_status() -> Result<WorkerStatus, ApiError> {
    let resp = get(&format!("{API_BASE}/downloads/worker"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("worker status -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<WorkerStatus>().await?)
}

/// VPN tunnel state from gluetun (SKADI-T-0292).
#[derive(Debug, Clone, Deserialize)]
pub struct VpnStatus {
    pub reachable: bool,
    pub connected: bool,
    #[serde(default)]
    pub exit_ip: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

/// `GET /downloads/vpn` — gluetun tunnel state + exit IP/location (read-only;
/// the VPN is always-on by design — no disable control).
pub async fn vpn_status() -> Result<VpnStatus, ApiError> {
    let resp = get(&format!("{API_BASE}/downloads/vpn"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("vpn -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<VpnStatus>().await?)
}

/// `DELETE /downloads/{id}?delete_data=` — remove a download (SKADI-T-0168). The
/// worker tells librqbit to forget (keep files) or delete (drop files). `false`
/// keeps the downloaded data (the imported library copy is a hardlink either way).
pub async fn remove_download(id: &str, delete_data: bool) -> Result<(), ApiError> {
    let resp = delete(&format!(
        "{API_BASE}/downloads/{id}?delete_data={delete_data}"
    ))
    .send()
    .await?;
    write_ok("remove download", resp).await
}

/// `POST /downloads/{id}/pause` — pause an active download (SKADI-T-0168).
pub async fn pause_download(id: &str) -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/downloads/{id}/pause"))
        .send()
        .await?;
    write_ok("pause download", resp).await
}

/// `POST /downloads/{id}/resume` — resume a paused download (SKADI-T-0168).
pub async fn resume_download(id: &str) -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/downloads/{id}/resume"))
        .send()
        .await?;
    write_ok("resume download", resp).await
}

/// `POST /downloads/pause-all` — pause every in-flight transfer (SKADI-T-0290).
pub async fn pause_all() -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/downloads/pause-all"))
        .send()
        .await?;
    write_ok("pause all", resp).await
}

/// `POST /downloads/resume-all` — resume every paused transfer (SKADI-T-0290).
pub async fn resume_all() -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/downloads/resume-all"))
        .send()
        .await?;
    write_ok("resume all", resp).await
}

/// Hot-reloadable worker settings (SKADI-T-0291) — caps + seed policy. `0` ⇒
/// unlimited on an axis; the worker applies changes within a tick (no restart).
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadSettings {
    pub max_active: u64,
    pub down_limit_bps: u64,
    pub up_limit_bps: u64,
    pub seed_ratio: f64,
    pub seed_time_mins: u64,
    pub seed_action: String,
}

/// `GET /downloads/settings` — current worker settings.
pub async fn download_settings() -> Result<DownloadSettings, ApiError> {
    let resp = get(&format!("{API_BASE}/downloads/settings"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("settings -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<DownloadSettings>().await?)
}

/// `PUT /downloads/settings` — persist worker settings; applied live (SKADI-T-0291).
pub async fn set_download_settings(s: &DownloadSettings) -> Result<(), ApiError> {
    let body = serde_json::json!({
        "max_active": s.max_active,
        "down_limit_bps": s.down_limit_bps,
        "up_limit_bps": s.up_limit_bps,
        "seed_ratio": s.seed_ratio,
        "seed_time_mins": s.seed_time_mins,
        "seed_action": s.seed_action,
    });
    let resp = put(&format!("{API_BASE}/downloads/settings"))
        .json(&body)?
        .send()
        .await?;
    write_ok("save download settings", resp).await
}

/// Per-domain rename templates + the shared whitespace char (SKADI-T-0299).
#[derive(Debug, Clone, Deserialize)]
pub struct NamingSettings {
    pub movie_folder: String,
    pub movie_file: String,
    pub series_folder: String,
    pub series_file: String,
    pub audiobook_folder: String,
    pub audiobook_file: String,
    pub space: String,
}

/// `GET /naming/settings` — current naming templates (defaults where unset).
pub async fn naming_settings() -> Result<NamingSettings, ApiError> {
    let resp = get(&format!("{API_BASE}/naming/settings"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("naming -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<NamingSettings>().await?)
}

/// `PUT /naming/settings` — persist naming templates.
pub async fn set_naming_settings(s: &NamingSettings) -> Result<(), ApiError> {
    let body = serde_json::json!({
        "movie_folder": s.movie_folder,
        "movie_file": s.movie_file,
        "series_folder": s.series_folder,
        "series_file": s.series_file,
        "audiobook_folder": s.audiobook_folder,
        "audiobook_file": s.audiobook_file,
        "space": s.space,
    });
    let resp = put(&format!("{API_BASE}/naming/settings"))
        .json(&body)?
        .send()
        .await?;
    write_ok("save naming", resp).await
}

/// `POST /naming/preview` — render a sample library path for `domain` (movie | tv |
/// audiobook) with the given templates. Returns the rendered relative path.
pub async fn naming_preview(
    domain: &str,
    folder: &str,
    file: &str,
    space: &str,
) -> Result<String, ApiError> {
    let body = serde_json::json!({
        "domain": domain, "folder": folder, "file": file, "space": space,
    });
    let resp = post(&format!("{API_BASE}/naming/preview"))
        .json(&body)?
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("preview -> HTTP {}", resp.status())));
    }
    let v = resp.json::<serde_json::Value>().await?;
    Ok(v.get("path")
        .and_then(|p| p.as_str())
        .unwrap_or_default()
        .to_string())
}

// ---------------------------------------------------------------------------
// Audiobooks domain (SKADI-T-0133) — Author → Series → Book, mirroring the
// movies client. Books are the library item; a `BookFile` is the acquirable
// (one per book). Served under `/authors` and `/books` by the audiobooks module.
// ---------------------------------------------------------------------------

/// An audiobook author (mirror of `skadi_audiobooks::Author`). Organizational
/// entity above books; `monitored` drives new-release discovery.
#[derive(Debug, Clone, Deserialize)]
pub struct Author {
    pub id: String,
    #[serde(default)]
    pub asin: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    pub monitored: bool,
}

/// A book's series membership (mirror of `skadi_audiobooks::SeriesLink`).
#[derive(Debug, Clone, Deserialize)]
pub struct SeriesLink {
    #[allow(dead_code)]
    pub series_id: String,
    pub name: String,
    #[serde(default)]
    pub position: Option<String>,
}

/// One acquirable audiobook file (mirror of `skadi_audiobooks::BookFile`).
/// `status` is the same externally-tagged `AcquisitionStatus` the movies UI
/// renders; use [`status_label`] / [`download_progress`] on it.
#[derive(Debug, Clone, Deserialize)]
pub struct BookFile {
    pub id: String,
    #[allow(dead_code)]
    pub book_id: String,
    pub status: Value,
    #[serde(default)]
    #[allow(dead_code)]
    pub quality: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub format_score: i32,
}

/// A book library record (subset of the server `Book`; extra fields ignored).
/// `external_ids.asin` is the audiobook key.
#[derive(Debug, Clone, Deserialize)]
pub struct Book {
    pub id: String,
    #[serde(default)]
    pub external_ids: BookExternalIds,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub author_id: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub narrators: Vec<String>,
    #[serde(default)]
    pub series: Option<SeriesLink>,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    pub monitored: bool,
    #[serde(default)]
    pub files: Vec<BookFile>,
}

/// Just the audiobook-relevant external id (the ASIN). Other provider ids are
/// ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BookExternalIds {
    #[serde(default)]
    pub asin: Option<String>,
}

/// One author lookup candidate (mirror of the server `AuthorLookupResult`):
/// enriched with a portrait + bio snippet for disambiguation (SKADI-T-0152).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorLookupResult {
    pub asin: String,
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// The Audnexus metadata preview for an ASIN (subset of `MetadataRecord`).
#[derive(Debug, Clone, Deserialize)]
pub struct BookLookup {
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub narrators: Vec<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub series_position: Option<String>,
}

/// `GET /authors[?monitored=]` — list authors.
pub async fn list_authors(monitored: Option<bool>) -> Result<Vec<Author>, ApiError> {
    let url = match monitored {
        Some(m) => format!("{API_BASE}/authors?monitored={m}"),
        None => format!("{API_BASE}/authors"),
    };
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list authors -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Author>>().await?)
}

/// `GET /authors/{id}` — fetch one author for the detail page.
pub async fn get_author(id: &str) -> Result<Author, ApiError> {
    let resp = get(&format!("{API_BASE}/authors/{id}"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("get author -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Author>().await?)
}

/// `GET /authors/lookup?name=` — author search candidates for the add flow.
pub async fn lookup_authors(name: &str) -> Result<Vec<AuthorLookupResult>, ApiError> {
    let url = format!("{API_BASE}/authors/lookup?name={}", encode(name));
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("author lookup -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<AuthorLookupResult>>().await?)
}

/// `POST /authors` — add (or return existing) author by Audnexus ASIN.
pub async fn add_author(asin: &str) -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/authors"))
        .json(&serde_json::json!({ "asin": asin }))?
        .send()
        .await?;
    write_ok("add author", resp).await
}

/// `PATCH /authors/{id}` — toggle whether the author is monitored (a monitored
/// author has their new releases auto-discovered + added, SKADI-T-0132).
pub async fn set_author_monitored(id: &str, monitored: bool) -> Result<(), ApiError> {
    let resp = patch(&format!("{API_BASE}/authors/{id}"))
        .json(&serde_json::json!({ "monitored": monitored }))?
        .send()
        .await?;
    write_ok("update author", resp).await
}

/// `DELETE /authors/{id}` — remove an author (library rows only).
pub async fn delete_author(id: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/authors/{id}"))
        .send()
        .await
        .noted()?;
    write_ok("delete author", resp).await
}

/// `GET /books[?monitored=&author=]` — list the book library.
pub async fn list_books(
    monitored: Option<bool>,
    author: Option<&str>,
) -> Result<Vec<Book>, ApiError> {
    let mut params: Vec<String> = Vec::new();
    if let Some(m) = monitored {
        params.push(format!("monitored={m}"));
    }
    if let Some(a) = author {
        params.push(format!("author={}", encode(a)));
    }
    let url = if params.is_empty() {
        format!("{API_BASE}/books")
    } else {
        format!("{API_BASE}/books?{}", params.join("&"))
    };
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list books -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Book>>().await?)
}

/// `GET /books/{id}` — fetch one book (with files) for the detail page.
/// `GET /books/{id}`, only when it changed since `last` (see [`get_if_changed`]).
pub async fn get_book_if_changed(
    id: &str,
    last: &mut Option<String>,
) -> Result<Option<Book>, ApiError> {
    get_if_changed(&format!("{API_BASE}/books/{id}"), "get book", last).await
}

pub async fn get_book(id: &str) -> Result<Book, ApiError> {
    let resp = get(&format!("{API_BASE}/books/{id}"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("get book -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Book>().await?)
}

/// One chapter mark of a book file (mirror of the server ChapterDto,
/// SKADI-T-0330). Times are seconds from the start of the audio.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chapter {
    pub index: usize,
    pub title: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// `GET /books/{id}/files/{fid}/chapters` — embedded chapter marks (empty when
/// the file has none; the player falls back to a plain seek bar).
pub async fn book_chapters(id: &str, fid: &str) -> Result<Vec<Chapter>, ApiError> {
    let resp = get(&format!("{API_BASE}/books/{id}/files/{fid}/chapters"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("chapters -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Chapter>>().await?)
}

/// URL of a book file's audio bytes (SKADI-T-0329). The `<audio>` element can't
/// send an Authorization header, so the player downloads this via XHR (see
/// [`auth_token`]) into a Blob and plays the local copy — which is the offline
/// product model anyway (SKADI-I-0048).
/// Direct-play video URLs for the browser's `<video>` element (SKADI-T-0585).
/// A media element cannot send a bearer header, so the token rides in the
/// query string — the same `?apikey=` the phone uses (see `auth.rs`).
pub fn movie_video_url(id: &str, eid: &str) -> String {
    with_api_key(format!("{API_BASE}/movies/{id}/editions/{eid}/video"))
}

pub fn episode_video_url(sid: &str, eid: &str) -> String {
    with_api_key(format!("{API_BASE}/series/{sid}/episodes/{eid}/video"))
}

fn with_api_key(url: String) -> String {
    match api_token() {
        Some(t) => format!("{url}?apikey={t}"),
        None => url,
    }
}

/// A subtitle file beside a video (SKADI-T-0663), as `GET …/subtitles` lists it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SubtitleTrack {
    pub index: usize,
    pub language: Option<String>,
    pub label: String,
    #[serde(default)]
    pub forced: bool,
    pub format: String,
}

/// The subtitle files of the video at `video_path` (`…/editions/{eid}/video` or
/// `…/episodes/{eid}/video`, without the key). Empty on any failure: the film
/// still plays.
pub async fn video_subtitles(video_path: &str) -> Vec<SubtitleTrack> {
    let url = format!("{}/subtitles", video_path.trim_end_matches("/video"));
    match get(&url).send().await {
        Ok(r) if r.ok() => r.json().await.unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A `<track>` URL for subtitle `index` of that video, as WebVTT — browsers
/// load nothing else — with the key in the query, since a `<track>` cannot send
/// a header either.
pub fn video_subtitle_vtt_url(video_path: &str, index: usize) -> String {
    let base = format!(
        "{}/subtitles/{index}",
        video_path.trim_end_matches("/video")
    );
    match api_token() {
        Some(t) => format!("{base}?format=vtt&apikey={t}"),
        None => format!("{base}?format=vtt"),
    }
}

pub fn book_audio_url(id: &str, fid: &str) -> String {
    format!("{API_BASE}/books/{id}/files/{fid}/audio")
}

/// The same bytes, as a URL a plain `<a download>` can fetch (SKADI-T-0635).
///
/// The offline player uses [`book_audio_url`] with an `Authorization` header
/// because it goes through XHR. An anchor cannot set a header, so this carries
/// the key in the query the way the video routes do.
#[must_use]
pub fn book_audio_download_url(id: &str, fid: &str) -> String {
    with_api_key(book_audio_url(id, fid))
}

/// The injected bearer token, for requests that can't go through the normal
/// builder (XHR downloads with progress events). `None` in open mode.
pub fn auth_token() -> Option<String> {
    api_token()
}

/// The phone-pairing QR (SKADI-T-0333): the server URL as scannable SVG.
#[derive(Debug, Clone, Deserialize)]
pub struct PairQr {
    pub url: String,
    pub qr_svg: String,
}

/// The published native-app build's install QR (SKADI-T-0340).
#[derive(Debug, Clone, Deserialize)]
pub struct ApkInstall {
    pub available: bool,
    #[serde(default)]
    pub version_name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub qr_svg: Option<String>,
}

/// `GET /pair/apk` — install QR for the published APK (`available:false` when
/// none is published).
pub async fn pair_apk() -> Result<ApkInstall, ApiError> {
    let resp = get(&format!("{API_BASE}/pair/apk")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("pair apk -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<ApkInstall>().await?)
}

/// `GET /pair/qr` — QR for `url` (the pair card always passes one explicitly,
/// so what's rendered is exactly what the operator sees in the field).
pub async fn pair_qr(url: &str) -> Result<PairQr, ApiError> {
    let resp = get(&format!("{API_BASE}/pair/qr?url={}", encode(url)))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("pair qr -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<PairQr>().await?)
}

/// `GET /pair/app` — the native-app pairing QR (a `skadi://pair?host=&token=`
/// URI the Android app scans to connect in one step). `url` overrides the host.
pub async fn pair_app_qr(url: &str) -> Result<PairQr, ApiError> {
    let path = if url.trim().is_empty() {
        format!("{API_BASE}/pair/app")
    } else {
        format!("{API_BASE}/pair/app?url={}", encode(url))
    };
    let resp = get(&path).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("pair app -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<PairQr>().await?)
}

/// `GET /books/lookup?asin=` — Audnexus metadata preview for an ASIN (Audnexus
/// has no title search, so the add flow is keyed by ASIN).
pub async fn lookup_book(asin: &str) -> Result<BookLookup, ApiError> {
    let url = format!("{API_BASE}/books/lookup?asin={}", encode(asin));
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("book lookup -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<BookLookup>().await?)
}

/// `POST /books` — add an audiobook by ASIN. `profile`/`root` are optional (the
/// server falls back to the first registered ones); `search` kicks off an
/// acquire run on add when the domain is enabled.
pub async fn add_book(asin: &str, profile: Option<&str>, search: bool) -> Result<(), ApiError> {
    let mut body = serde_json::json!({ "asin": asin, "search": search });
    if let Some(p) = profile {
        body["profile"] = serde_json::json!(p);
    }
    let resp = post(&format!("{API_BASE}/books"))
        .json(&body)?
        .send()
        .await?;
    write_ok("add book", resp).await
}

/// Series completeness rollup (mirror of `SeriesRollupDto`, SKADI-I-0018):
/// `owned` of `total` known works in the series.
#[derive(Debug, Clone, Deserialize)]
pub struct SeriesRollup {
    pub name: String,
    #[serde(default)]
    pub series_asin: Option<String>,
    /// Positioned members only (SKADI-T-0654).
    pub total: usize,
    pub owned: usize,
    /// Members with no series position, shown under "Related" and not counted.
    #[serde(default)]
    pub related: usize,
    #[serde(default)]
    pub watched: bool,
}

/// `GET /audiobooks/series` — per-series owned-of-total rollups + watch state.
#[allow(dead_code)]
pub async fn list_book_series() -> Result<Vec<SeriesRollup>, ApiError> {
    let resp = get(&format!("{API_BASE}/audiobooks/series"))
        .send()
        .await
        .noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list series -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<SeriesRollup>>().await?)
}

/// `PUT /watchers/{scope}/{key}` — start watching (author/series/book) for acquire.
pub async fn set_watcher(scope: &str, key: &str) -> Result<(), ApiError> {
    let resp = put(&format!("{API_BASE}/watchers/{scope}/{}", encode(key)))
        .send()
        .await?;
    write_ok("set watcher", resp).await
}

/// `DELETE /watchers/{scope}/{key}` — stop watching.
pub async fn clear_watcher(scope: &str, key: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/watchers/{scope}/{}", encode(key)))
        .send()
        .await?;
    write_ok("clear watcher", resp).await
}

/// An active watcher (mirror of the server `WatcherDto`): `scope` is
/// `author`/`series`/`book`, `key` the corresponding ASIN.
#[derive(Debug, Clone, Deserialize)]
pub struct Watcher {
    pub scope: String,
    pub key: String,
}

/// `GET /watchers` — all active watchers, so the UI can reflect current state of
/// author/series/book toggles.
pub async fn list_watchers() -> Result<Vec<Watcher>, ApiError> {
    let resp = get(&format!("{API_BASE}/watchers")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list watchers -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Watcher>>().await?)
}

/// One known work in a body-of-work browse (mirror of the server `WorkDto`,
/// SKADI-T-0159): catalog record + whether it's owned (and which book) and
/// whether a watcher covers it.
#[derive(Debug, Clone, Deserialize)]
pub struct Work {
    pub asin: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub series_name: Option<String>,
    #[serde(default)]
    pub series_position: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    pub owned: bool,
    #[serde(default)]
    pub book_id: Option<String>,
    pub watched: bool,
}

/// `POST /audiobooks/catalog/refresh` — kick off a catalog ingest pass so the
/// known-works store is refreshed for every library author (SKADI-T-0160). Returns
/// once the pass is *queued* (it runs in the background on the server).
pub async fn refresh_catalog() -> Result<(), ApiError> {
    let resp = post(&format!("{API_BASE}/audiobooks/catalog/refresh"))
        .send()
        .await?;
    write_ok("refresh catalog", resp).await
}

/// `GET /audiobooks/works?author=<asin>` — every known work for an author (owned +
/// unowned), for the body-of-work browse on the author page.
pub async fn list_works_by_author(asin: &str) -> Result<Vec<Work>, ApiError> {
    let resp = get(&format!(
        "{API_BASE}/audiobooks/works?author={}",
        encode(asin)
    ))
    .send()
    .await?;
    if !resp.ok() {
        return Err(ApiError(format!("list works -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Work>>().await?)
}

/// `GET /audiobooks/works?series=<asin>` — every known work in a series (owned +
/// unowned), position-ordered, so the library can render missing entries as muted
/// tiles alongside the owned ones.
pub async fn list_works_by_series(asin: &str) -> Result<Vec<Work>, ApiError> {
    let resp = get(&format!(
        "{API_BASE}/audiobooks/works?series={}",
        encode(asin)
    ))
    .send()
    .await?;
    if !resp.ok() {
        return Err(ApiError(format!("list works -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Work>>().await?)
}

/// `GET /audiobooks/discover?limit=N` (SKADI-T-0317) — library-driven recommendations: unowned
/// known works ranked upcoming → recently-released → undated. Reuses the [`Work`] shape.
pub async fn discover_audiobooks(limit: u32) -> Result<Vec<Work>, ApiError> {
    let resp = get(&format!("{API_BASE}/audiobooks/discover?limit={limit}"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("discover -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Work>>().await?)
}

/// One Audible catalog search hit (mirror of the audiobooks `BookSearchDto`).
#[derive(Debug, Clone, Deserialize)]
pub struct BookSearchResult {
    pub asin: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub cover_url: Option<String>,
}

/// `GET /books/search?q=` — free-text title/author search over the Audible catalog.
pub async fn search_books(q: &str) -> Result<Vec<BookSearchResult>, ApiError> {
    let resp = get(&format!("{API_BASE}/books/search?q={}", encode(q)))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!("book search -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<BookSearchResult>>().await?)
}

/// One row of the built-in audiobook quality ladder (mirror of the audiobooks
/// `AudiobookQualityDto`). Best → fallback order; `kbps == 0` means VBR/unknown.
#[derive(Debug, Clone, Deserialize)]
pub struct AudiobookQuality {
    pub name: String,
    pub format: String,
    pub kbps: u32,
}

/// `GET /audiobooks/quality` — the built-in, format-first audiobook quality
/// ladder (best → fallback). Read-only; the Audiobooks · Config page displays it.
pub async fn audiobook_quality() -> Result<Vec<AudiobookQuality>, ApiError> {
    let resp = get(&format!("{API_BASE}/audiobooks/quality"))
        .send()
        .await?;
    if !resp.ok() {
        return Err(ApiError(format!(
            "audiobook quality -> HTTP {}",
            resp.status()
        )));
    }
    Ok(resp.json::<Vec<AudiobookQuality>>().await?)
}

/// `PATCH /books/{id}` — toggle the monitored flag.
pub async fn set_book_monitored(id: &str, monitored: bool) -> Result<(), ApiError> {
    let resp = patch(&format!("{API_BASE}/books/{id}"))
        .json(&serde_json::json!({ "monitored": monitored }))?
        .send()
        .await?;
    write_ok("update book", resp).await
}

/// `DELETE /books/{id}` — remove a book (library rows only; no file deletion).
pub async fn delete_book(id: &str, delete_files: bool) -> Result<(), ApiError> {
    let resp = delete(&format!(
        "{API_BASE}/books/{id}?delete_files={delete_files}"
    ))
    .send()
    .await?;
    write_ok("delete book", resp).await
}

/// `POST /books/{id}/files/{fid}/acquire` — trigger a manual acquire.
pub async fn acquire_book_file(book_id: &str, file_id: &str) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/books/{book_id}/files/{file_id}/acquire");
    let resp = post(&url).send().await.noted()?;
    write_ok("acquire", resp).await
}

/// `POST /books/{id}/files/{fid}/reset` — unwedge a file back to Missing.
pub async fn reset_book_file(book_id: &str, file_id: &str) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/books/{book_id}/files/{file_id}/reset");
    let resp = post(&url).send().await.noted()?;
    write_ok("reset", resp).await
}

/// Where an imported book file lives on disk (mirror of the audiobooks
/// `FileLocationDto`, SKADI-T-0147).
#[derive(Debug, Clone, Deserialize)]
pub struct BookFileLocation {
    pub folder: String,
    pub format: String,
    pub file_count: usize,
    pub total_bytes: u64,
    #[serde(default)]
    pub imported_at: Option<String>,
}

/// `GET /books/{id}/files/{fid}/location` — folder + size/count for an imported
/// file. `Ok(None)` when the file isn't imported yet (404).
pub async fn book_file_location(
    book_id: &str,
    file_id: &str,
) -> Result<Option<BookFileLocation>, ApiError> {
    let url = format!("{API_BASE}/books/{book_id}/files/{file_id}/location");
    let resp = get(&url).send().await.noted()?;
    if resp.status() == 404 {
        return Ok(None);
    }
    if !resp.ok() {
        return Err(ApiError(format!("file location -> HTTP {}", resp.status())));
    }
    Ok(Some(resp.json::<BookFileLocation>().await?))
}

// ---------------------------------------------------------------------------
// Server-side folder browser (SKADI-T-0149) — backs the path-input folder picker.
// ---------------------------------------------------------------------------

/// One subdirectory in a folder listing.
#[derive(Debug, Clone, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
}

/// A directory listing (mirror of the api `FsListing`).
#[derive(Debug, Clone, Deserialize)]
pub struct FsListing {
    pub path: String,
    /// `None` when at a browse root (can't go above the media mounts).
    #[serde(default)]
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
}

/// `GET /fs/browse?path=` — list subdirectories of `path` (or the first root).
pub async fn fs_browse(path: Option<&str>) -> Result<FsListing, ApiError> {
    let url = match path {
        Some(p) if !p.trim().is_empty() => {
            format!("{API_BASE}/fs/browse?path={}", encode(p))
        }
        _ => format!("{API_BASE}/fs/browse"),
    };
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("browse -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<FsListing>().await?)
}

/// `GET /books/{id}/files/{fid}/releases` — live interactive search.
pub async fn list_book_releases(
    book_id: &str,
    file_id: &str,
) -> Result<Vec<ReleaseCandidate>, ApiError> {
    let url = format!("{API_BASE}/books/{book_id}/files/{file_id}/releases");
    let resp = get(&url).send().await.noted()?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp
            .text()
            .await
            .ok()
            .and_then(|t| extract_error_message(&t))
            .unwrap_or_default();
        return Err(ApiError(if detail.is_empty() {
            format!("search releases -> HTTP {status}")
        } else {
            detail
        }));
    }
    Ok(resp.json::<Vec<ReleaseCandidate>>().await?)
}

/// `POST /books/{id}/files/{fid}/grab` — grab a specific release.
pub async fn grab_book_release(
    book_id: &str,
    file_id: &str,
    release: &Value,
) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/books/{book_id}/files/{file_id}/grab");
    let resp = post(&url)
        .json(&serde_json::json!({ "release": release }))?
        .send()
        .await?;
    write_ok("grab", resp).await
}

/// `POST /books/{id}/files/{fid}/grab-link` — manual acquisition: hand a pasted
/// magnet or `.torrent` URL to the book file's acquire pipeline (SKADI-I-0043).
pub async fn grab_link_book(
    book_id: &str,
    file_id: &str,
    link: &str,
    title: Option<&str>,
) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/books/{book_id}/files/{file_id}/grab-link");
    let resp = post(&url)
        .json(&serde_json::json!({ "link": link, "title": title }))?
        .send()
        .await?;
    write_ok("grab link", resp).await
}

/// Minimal percent-encoding for a query value (space + the handful of chars that
/// break a query string). Good enough for movie titles.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// --- config registry (SKADI-T-0540) ---

/// One registry key as `GET /config` reports it. Mirrors the API's
/// `ConfigKeyDto` (SKADI-T-0535).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConfigKey {
    pub key: String,
    /// `string` | `bool` | `u16` | `u64` | `path`.
    pub kind: String,
    /// The registry default, so the form can show what "unset" means.
    pub default: String,
    /// The value in force. `None` for a redacted key.
    #[serde(default)]
    pub value: Option<String>,
    /// Whether a value is stored at all — the only signal for a redacted key.
    #[serde(rename = "isSet", default)]
    pub is_set: bool,
    /// Whether the value is withheld rather than absent.
    #[serde(default)]
    pub redacted: bool,
    /// `env` or `runtime` when a row exists. An **env**-sourced value is
    /// rewritten by the seeder on the next boot, so a runtime write to it does
    /// not survive a restart.
    #[serde(default)]
    pub source: Option<String>,
    /// Tier0 keys are read before the table exists and cannot be written here.
    #[serde(default)]
    pub editable: bool,
}

impl ConfigKey {
    /// The prefix the settings form groups by (`import.min_free_mb` →
    /// `import`). A key with no dot groups under `general`.
    #[must_use]
    pub fn group(&self) -> &str {
        match self.key.split_once('.') {
            Some((prefix, _)) => prefix,
            None => "general",
        }
    }

    /// The label shown for the key within its group — the part after the prefix,
    /// so a section headed "import" does not repeat "import." on every row.
    #[must_use]
    pub fn leaf(&self) -> &str {
        self.key
            .split_once('.')
            .map_or(&*self.key, |(_, rest)| rest)
    }

    /// Whether editing this key here will survive a restart.
    ///
    /// An env-sourced value is overwritten by the env seeder on the next boot,
    /// so a runtime write to it silently does not stick. The form has to say so
    /// — letting someone edit it without warning is worse than not offering the
    /// field, because it looks like it worked.
    #[must_use]
    pub fn overwritten_by_env(&self) -> bool {
        self.source.as_deref() == Some("env")
    }
}

/// `GET /config` — every registry key with its value, default, kind and tier.
pub async fn list_config() -> Result<Vec<ConfigKey>, ApiError> {
    let resp = get(&format!("{API_BASE}/config")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("list config -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<ConfigKey>>().await?)
}

/// `PUT /config/{key}` — set one key.
pub async fn set_config(key: &str, value: &str) -> Result<(), ApiError> {
    let resp = put(&format!("{API_BASE}/config/{key}"))
        .json(&serde_json::json!({ "value": value }))?
        .send()
        .await?;
    write_ok("set config", resp).await
}

/// `DELETE /config/{key}` — clear the stored row so the default applies again.
///
/// Distinct from writing the default value: that would store a row that merely
/// happens to equal the default today and would not follow it if the default
/// changed.
pub async fn clear_config(key: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/config/{key}")).send().await?;
    write_ok("clear config", resp).await
}

#[cfg(test)]
mod acquirable_path_tests {
    use super::AcquirablePath;

    /// One path type serves all three domains since SKADI-T-0558 gave television
    /// the same manual-acquire routes (SKADI-T-0559). Getting a segment wrong
    /// would 404 at runtime with nothing to point at the cause, so the shapes are
    /// pinned here.
    #[test]
    fn each_domain_builds_its_own_route_shape() {
        assert_eq!(
            AcquirablePath::movie_edition("m1", "e1").url("releases"),
            "/api/v1/movies/m1/editions/e1/releases"
        );
        assert_eq!(
            AcquirablePath::tv_episode("s1", "ep1").url("grab-link"),
            "/api/v1/series/s1/episodes/ep1/grab-link"
        );
        assert_eq!(
            AcquirablePath::book_file("b1", "f1").url("reset"),
            "/api/v1/books/b1/files/f1/reset"
        );
    }
}

// ---------------------------------------------------------------------------
// Household (SKADI-T-0611 / SKADI-T-0613) — members, policies, pairing.
// ---------------------------------------------------------------------------

/// Rating ceilings per kind, as source labels ("PG-13", "TV-14"); `None` = no ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaxRating {
    #[serde(default)]
    pub movie: Option<String>,
    #[serde(default)]
    pub series: Option<String>,
}

/// What a member may see (mirror of skadi-api `household::Policy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default = "Policy::all_kinds")]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub max_rating: MaxRating,
    #[serde(default)]
    pub blocked_genres: Vec<String>,
    #[serde(default)]
    pub blocked_items: Vec<String>,
    #[serde(default)]
    pub allowed_items: Vec<String>,
    #[serde(default)]
    pub allowed_books: Vec<String>,
}

impl Policy {
    fn all_kinds() -> Vec<String> {
        vec!["movie".into(), "series".into(), "audiobook".into()]
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            kinds: Self::all_kinds(),
            max_rating: MaxRating::default(),
            blocked_genres: Vec::new(),
            blocked_items: Vec::new(),
            allowed_items: Vec::new(),
            allowed_books: Vec::new(),
        }
    }
}

/// A household member as listed (never carries the token).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    /// `admin` | `member` | `kid`.
    pub role: String,
    /// What they sign in as; `None` for an account that predates logins.
    #[serde(default)]
    pub username: Option<String>,
    /// Whether a password is set — never the hash.
    #[serde(default)]
    pub has_password: bool,
    /// Signed-in devices, for "sign out all".
    #[serde(default)]
    pub devices: usize,
    #[serde(default)]
    pub has_pin: bool,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

/// `POST /members` and `POST /members/{id}/token` answer with the member and
/// the raw token, shown once.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatedMember {
    pub member: Member,
    pub token: String,
}

/// `GET /me` — who this browser is signed in as.
pub async fn me() -> Result<Member, ApiError> {
    let resp = get(&format!("{API_BASE}/me")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("me -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Member>().await?)
}

pub async fn list_members() -> Result<Vec<Member>, ApiError> {
    let resp = get(&format!("{API_BASE}/members")).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("members -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<Vec<Member>>().await?)
}

/// Decode a JSON response, surfacing the server's own error text when it is
/// not a success. Named for members once; it is the generic read for every
/// endpoint that answers a body (SKADI-T-0631 renamed it rather than adding a
/// second copy).
async fn read_json<T: serde::de::DeserializeOwned>(
    what: &str,
    resp: gloo_net::http::Response,
) -> Result<T, ApiError> {
    if resp.ok() {
        return Ok(resp.json::<T>().await?);
    }
    let status = resp.status();
    let detail = resp
        .text()
        .await
        .ok()
        .and_then(|t| extract_error_message(&t))
        .unwrap_or_default();
    if detail.is_empty() {
        Err(ApiError(format!("{what} -> HTTP {status}")))
    } else {
        Err(ApiError(detail))
    }
}

pub async fn create_member(body: &Value) -> Result<CreatedMember, ApiError> {
    let resp = post(&format!("{API_BASE}/members"))
        .header("content-type", "application/json")
        .body(body.to_string())?
        .send()
        .await
        .noted()?;
    read_json("create member", resp).await
}

pub async fn update_member(id: &str, body: &Value) -> Result<Member, ApiError> {
    let resp = patch(&format!("{API_BASE}/members/{}", encode(id)))
        .header("content-type", "application/json")
        .body(body.to_string())?
        .send()
        .await
        .noted()?;
    read_json("update member", resp).await
}

pub async fn reissue_member_token(id: &str) -> Result<CreatedMember, ApiError> {
    let resp = post(&format!("{API_BASE}/members/{}/token", encode(id)))
        .send()
        .await
        .noted()?;
    read_json("re-issue token", resp).await
}

pub async fn delete_member(id: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/members/{}", encode(id)))
        .send()
        .await
        .noted()?;
    write_ok("delete member", resp).await
}

/// `GET /pair/app?member=` — the native-app pairing QR carrying that member's
/// token (SKADI-T-0611). `url` overrides the host, as for the operator's QR.
pub async fn pair_app_qr_for(member: &str, url: &str) -> Result<PairQr, ApiError> {
    let mut path = format!("{API_BASE}/pair/app?member={}", encode(member));
    if !url.trim().is_empty() {
        path.push_str(&format!("&url={}", encode(url)));
    }
    let resp = get(&path).send().await.noted()?;
    if !resp.ok() {
        return Err(ApiError(format!("pair app -> HTTP {}", resp.status())));
    }
    Ok(resp.json::<PairQr>().await?)
}

// ---------------------------------------------------------------------------
// Sign in (SKADI-T-0621)
// ---------------------------------------------------------------------------

/// `POST /auth/login` — exchange a username and password for this browser's
/// token. Unauthenticated by design; it is how a caller *gets* a credential.
pub async fn login(username: &str, password: &str) -> Result<Member, ApiError> {
    let body = serde_json::json!({
        "username": username,
        "password": password,
        "label": "Browser",
    });
    let resp = Request::post(&format!("{API_BASE}/auth/login"))
        .header("content-type", "application/json")
        .body(body.to_string())?
        .send()
        .await?;
    if resp.status() == 401 {
        return Err(ApiError(
            "That username and password do not match.".to_string(),
        ));
    }
    if !resp.ok() {
        return Err(ApiError(format!("sign in -> HTTP {}", resp.status())));
    }
    let created = resp.json::<CreatedMember>().await?;
    store_token(&created.token);
    Ok(created.member)
}

/// `POST /auth/logout` — sign this browser out, server-side and locally. The
/// local half happens even if the call fails, because the alternative is a
/// button that visibly does nothing.
pub async fn logout() {
    let _ = post(&format!("{API_BASE}/auth/logout")).send().await;
    forget_token();
}

/// `POST /members/{id}/signout` — sign every one of a member's devices out.
pub async fn signout_member(id: &str) -> Result<Member, ApiError> {
    let resp = post(&format!("{API_BASE}/members/{}/signout", encode(id)))
        .send()
        .await
        .noted()?;
    read_json("sign out devices", resp).await
}

/// `POST /auth/password` — change your own password (SKADI-T-0626).
///
/// Signs your *other* devices out and keeps this one signed in, which is the
/// point: you are cutting off whoever learned the old one.
pub async fn change_password(current: &str, new: &str) -> Result<(), ApiError> {
    let body = serde_json::json!({ "current": current, "new": new });
    let resp = post(&format!("{API_BASE}/auth/password"))
        .header("content-type", "application/json")
        .body(body.to_string())?
        .send()
        .await
        .noted()?;
    if resp.status() == 401 {
        return Err(ApiError("That is not your current password.".into()));
    }
    write_ok("change password", resp).await
}

// --- browser uploads (SKADI-T-0631) -----------------------------------------

/// A resumable upload session, as the daemon reports it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct UploadSession {
    pub id: String,
    pub filename: String,
    pub kind: String,
    pub size_bytes: u64,
    /// The resume point. **Always trust this over anything remembered locally**
    /// — it is the length of the file the server actually holds.
    pub received_bytes: u64,
    pub chunk_bytes: u64,
}

/// What `complete` answers: where the file landed, ready for library-import.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct UploadDone {
    pub path: String,
    /// The directory to hand library-import — its scan walks a directory and
    /// refuses a file. Server-decided; see `uploads::CompleteResponse`.
    pub scan_path: String,
    pub kind: String,
}

/// The `POST /uploads` body.
///
/// **`size_bytes` must serialise as an integer.** A browser reports a file's
/// size as `f64` (`web_sys::File::size`), and handing that straight to
/// `serde_json` writes `16044.0`, which the daemon rejects outright:
/// `invalid type: floating point, expected u64`. Every upload from the UI
/// failed on this and nothing caught it — the integration tests speak JSON
/// directly and naturally send an integer, so only a real browser ever
/// produced the broken shape (SKADI-I-0062, found 2026-09-23).
///
/// Split out from the request purely so a test can assert the wire shape.
fn open_upload_body(filename: &str, size_bytes: f64, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "filename": filename,
        "size_bytes": size_bytes as u64,
        "kind": kind,
    })
}

/// Open a session. `kind` is `movie` | `series` | `audiobook`.
pub async fn open_upload(
    filename: &str,
    size_bytes: f64,
    kind: &str,
) -> Result<UploadSession, ApiError> {
    let body = open_upload_body(filename, size_bytes, kind);
    let resp = post(&format!("{API_BASE}/uploads"))
        .header("content-type", "application/json")
        .body(body.to_string())?
        .send()
        .await
        .noted()?;
    read_json("open upload", resp).await
}

/// Sessions this browser's account still has in flight.
pub async fn list_uploads() -> Result<Vec<UploadSession>, ApiError> {
    let resp = get(&format!("{API_BASE}/uploads")).send().await.noted()?;
    read_json("uploads", resp).await
}

/// How much the server holds — the resume point after a drop.
pub async fn upload_status(id: &str) -> Result<UploadSession, ApiError> {
    let resp = get(&format!("{API_BASE}/uploads/{id}"))
        .send()
        .await
        .noted()?;
    read_json("upload status", resp).await
}

/// Send one slice of the file at `offset`.
///
/// The body is a `Blob` slice rather than a byte vector, so only the piece in
/// flight is ever held in memory — the browser streams it from the file on
/// disk.
pub async fn put_upload_chunk(
    id: &str,
    offset: f64,
    blob: &web_sys::Blob,
) -> Result<UploadSession, ApiError> {
    // `as u64` for the same reason as the body above: an offset is a byte
    // count, and a query string carrying `offset=8022.5` is nonsense.
    let offset = offset as u64;
    let resp = put(&format!("{API_BASE}/uploads/{id}/chunk?offset={offset}"))
        .header("content-type", "application/octet-stream")
        .body(blob.clone())?
        .send()
        .await
        .noted()?;
    read_json("upload chunk", resp).await
}

/// Verify, probe and publish. Answers the staging path.
pub async fn complete_upload(id: &str) -> Result<UploadDone, ApiError> {
    let resp = post(&format!("{API_BASE}/uploads/{id}/complete"))
        .send()
        .await
        .noted()?;
    read_json("complete upload", resp).await
}

/// Abandon a session and remove its bytes.
pub async fn delete_upload(id: &str) -> Result<(), ApiError> {
    let resp = delete(&format!("{API_BASE}/uploads/{id}"))
        .send()
        .await
        .noted()?;
    write_ok("cancel upload", resp).await
}

#[cfg(test)]
mod upload_wire_tests {
    use super::*;

    #[test]
    fn the_upload_body_sends_a_whole_number_of_bytes() {
        // The bug this pins: a browser reports file size as f64, and passing
        // it through unchanged wrote `16044.0`, which the daemon refuses with
        // "invalid type: floating point, expected u64". Every upload from the
        // UI failed; no server test could see it, because JSON written by hand
        // naturally carries an integer.
        let body = open_upload_body("tone.wav", 16044.0, "audiobook");
        assert_eq!(body["size_bytes"], serde_json::json!(16044u64));
        assert!(
            body["size_bytes"].is_u64(),
            "size_bytes must be an integer, got {}",
            body["size_bytes"]
        );
        assert!(!body["size_bytes"].to_string().contains('.'));
        assert_eq!(body["filename"], "tone.wav");
        assert_eq!(body["kind"], "audiobook");
    }

    #[test]
    fn a_large_file_size_survives_the_conversion() {
        // 40 GB is the size this feature exists for; f64 holds it exactly.
        let big = 40.0 * 1024.0 * 1024.0 * 1024.0;
        let body = open_upload_body("film.mkv", big, "movie");
        assert_eq!(body["size_bytes"], serde_json::json!(42_949_672_960u64));
    }
}

// --- saving a file to your own machine (SKADI-T-0635) -----------------------

/// A filename for a downloaded media file.
///
/// Built from the library's own metadata rather than the path on disk: the
/// stored name is slugged for the filesystem (`batteries-not-included_(1987)`),
/// which is right for a library and wrong for something landing in someone's
/// Downloads folder.
///
/// `container` is the extension the scanner found. When the scan has not run
/// there is nothing honest to guess, so the extension is omitted and the
/// browser keeps whatever the response implies — better than confidently
/// writing `.mkv` onto an `.m4v`.
#[must_use]
pub fn download_filename(stem: &str, container: Option<&str>) -> String {
    let mut name = sanitise_filename(stem);
    if name.is_empty() {
        name = "download".into();
    }
    match container.map(str::trim).filter(|c| !c.is_empty()) {
        Some(ext) => format!(
            "{name}.{}",
            ext.trim_start_matches('.').to_ascii_lowercase()
        ),
        None => name,
    }
}

/// Strip what a filesystem will not take, and collapse the gaps.
///
/// The browser sanitises a `download` attribute itself, but it does so
/// silently and differently per platform; doing it here means the name is the
/// one we intended on every machine.
fn sanitise_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod download_name_tests {
    use super::*;

    #[test]
    fn a_title_and_year_become_a_readable_filename() {
        assert_eq!(
            download_filename("The Matrix (1999)", Some("mkv")),
            "The Matrix (1999).mkv"
        );
    }

    #[test]
    fn path_separators_and_reserved_characters_cannot_survive() {
        // A title is arbitrary text from a metadata provider. "9 1/2 Weeks"
        // and "Face/Off" are real films.
        assert_eq!(
            download_filename("Face/Off (1997)", Some("mp4")),
            "Face Off (1997).mp4"
        );
        assert_eq!(download_filename("A: B? C*", Some("mkv")), "A B C.mkv");
        assert!(!download_filename("../../etc/passwd", Some("mkv")).contains('/'));
    }

    #[test]
    fn an_unknown_container_leaves_the_extension_off() {
        // Guessing would put the wrong extension on the file, which is worse
        // than none: the operating system would open it with the wrong thing.
        assert_eq!(download_filename("Some Film", None), "Some Film");
        assert_eq!(download_filename("Some Film", Some("  ")), "Some Film");
    }

    #[test]
    fn the_extension_is_normalised() {
        assert_eq!(download_filename("X", Some(".MKV")), "X.mkv");
    }

    #[test]
    fn a_title_that_sanitises_to_nothing_still_names_the_file() {
        assert_eq!(download_filename("///", Some("mkv")), "download.mkv");
    }
}
