//! Browser uploads: resumable sessions that land a file on the library mount
//! (SKADI-T-0628, initiative SKADI-I-0062).
//!
//! Until now a file could only reach the library if it was *already* on a
//! server-visible filesystem — the web UI's path picker browses the server,
//! not the browser (see `skadi-web/src/path_picker.rs`). This module is the
//! missing half: bytes arrive from a browser and end up in a staging
//! directory, where the existing library-import flow adopts them.
//!
//! **It deliberately knows nothing about identifying media.** Library-import
//! already parses names and NFOs, resolves against TMDB/TVDB/Audnexus with a
//! confidence badge and an inline title search, and commits under the naming
//! template. A second matcher here would be a worse one. Completion answers a
//! path; identity is somebody else's job (SKADI-T-0632).
//!
//! ## Why chunks rather than multipart
//!
//! `axum` is built here with `features = ["macros"]` only — the `multipart`
//! feature is not enabled — and axum's default **2 MB** body limit applies to
//! every route unless one is set. Chunked upload sidesteps the parser
//! entirely: each chunk is a raw body at a known offset, the per-request limit
//! stays small and bounded, and resumability falls out of the same mechanism
//! instead of being bolted on afterwards.
//!
//! ## Why staging lives on the library mount
//!
//! `restructure_into` **refuses a cross-device move** and tells the operator
//! to scan through the same mount (`skadi-movies/src/import.rs`). Import is a
//! hardlink or a move, never a copy across filesystems. So the staging
//! directory is `<library.root>/incoming`, beside `movie`, `television` and
//! `audiobook`. A container temp dir would upload perfectly and then fail at
//! commit, which is the worst possible place to discover it.
//!
//! ## Shape of a session
//!
//! ```text
//! <library.root>/incoming/.sessions/<id>/
//!     part            the bytes received so far
//!     manifest.json   filename, kind, size, owner, opened_at
//! ```
//!
//! "How much have you got?" is the length of `part`, so there is no separate
//! ledger to keep in step with the file. Offsets must be **contiguous**: a
//! browser uploading one file sequentially always resumes from a single
//! high-water mark, and supporting holes would mean a range map and a
//! gap-filling assemble step for no gain.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::{get, post, put};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use skadi_core::AppError;

use crate::error::ApiError;
use crate::household::Member;
use crate::state::AppState;

/// How much the client is asked to send per request.
///
/// Small enough that a failed chunk is a cheap retry and the server's
/// per-request limit stays modest; large enough that a multi-gigabyte file is
/// not thousands of round trips.
pub const CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Headroom over [`CHUNK_BYTES`] for the request frame itself.
const CHUNK_BODY_LIMIT: usize = (CHUNK_BYTES as usize) + 64 * 1024;

/// The staging directory's name under the library root.
const INCOMING: &str = "incoming";

/// Where in-flight sessions live. Dot-prefixed so a library scan of
/// `incoming/` never treats a half-uploaded `part` file as media.
const SESSIONS: &str = ".sessions";

/// What a session records about itself, written beside the part file so a
/// daemon restart does not lose in-flight uploads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    /// The name the client sent, already sanitised by [`safe_filename`].
    pub filename: String,
    /// `movie` | `series` | `audiobook` — which import flow this is destined
    /// for. Carried through so the UI can route to the right page later.
    pub kind: String,
    pub size_bytes: u64,
    /// The member who opened it. Ownership is enforced in SKADI-T-0630.
    pub owner_id: String,
    pub opened_at: DateTime<Utc>,
    /// Optional client-supplied checksum, verified at completion when present.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// `POST /uploads` request.
#[derive(Debug, Deserialize)]
pub struct OpenRequest {
    pub filename: String,
    pub size_bytes: u64,
    pub kind: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// What a client needs to drive (or resume) an upload.
#[derive(Debug, Serialize)]
pub struct SessionView {
    pub id: String,
    pub filename: String,
    pub kind: String,
    pub size_bytes: u64,
    /// The resume point. A client trusts this over anything it remembers.
    pub received_bytes: u64,
    pub chunk_bytes: u64,
    pub opened_at: DateTime<Utc>,
}

/// `POST /uploads/{id}/complete` response.
#[derive(Debug, Serialize)]
pub struct CompleteResponse {
    /// Where the finished file now sits — a server path, ready to be handed to
    /// library-import.
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct ChunkParams {
    pub offset: u64,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// A filename reduced to something safe to place on disk.
///
/// Keeps the basename only (so `../../etc/passwd` becomes `passwd`), strips
/// anything outside a conservative set, and refuses to produce a dotfile or an
/// empty string. The extension matters — the probe and the import both key off
/// it — so it is preserved rather than stripped.
pub fn safe_filename(raw: &str) -> Result<String, AppError> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ' | '(' | ')' | '\'') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        return Err(AppError::Validation(format!(
            "{raw:?} has no usable filename"
        )));
    }
    if !cleaned.contains('.') {
        return Err(AppError::Validation(format!(
            "{cleaned:?} has no extension — skadi needs one to tell what the file is"
        )));
    }
    Ok(cleaned)
}

/// A session id is generated by us, but it arrives back through a URL, so it is
/// validated like anything else that becomes a path component.
fn safe_id(raw: &str) -> Result<&str, AppError> {
    let ok = !raw.is_empty()
        && raw.len() <= 64
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(raw)
    } else {
        Err(AppError::NotFound(format!("no upload session {raw:?}")))
    }
}

/// The library root, from config. Uploads must land on the same filesystem the
/// library lives on — see the module docs.
async fn library_root(state: &AppState) -> Result<PathBuf, AppError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| AppError::Internal("store not configured".into()))?;
    let view = crate::bootstrap::load_config_view(store)
        .await
        .map_err(|e| AppError::Internal(format!("reading config: {e}")))?;
    view.get_path("library.root")
        .ok()
        .flatten()
        .ok_or_else(|| AppError::Internal("library.root is not configured".into()))
}

/// What the guards need, read together so open and complete agree.
struct Guards {
    root: PathBuf,
    /// Free bytes to keep on the library filesystem, from `import.min_free_mb`.
    min_free_bytes: u64,
    /// `uploads.allowed_extensions`, when the operator has set one.
    ext_override: Option<String>,
}

async fn guards(state: &AppState) -> Result<Guards, AppError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| AppError::Internal("store not configured".into()))?;
    let view = crate::bootstrap::load_config_view(store)
        .await
        .map_err(|e| AppError::Internal(format!("reading config: {e}")))?;
    let root = view
        .get_path("library.root")
        .ok()
        .flatten()
        .ok_or_else(|| AppError::Internal("library.root is not configured".into()))?;
    // Uploads honour the same free-space floor imports do, rather than growing
    // a parallel knob the operator has to discover and keep in step.
    let min_free_bytes = skadi_config::import_guards(&view).min_free_bytes;
    // `get_opt_string` rather than `get_string`: an unset key and an empty one
    // both mean "use the built-in list", and an unregistered key errors.
    let ext_override = view
        .get_opt_string("uploads.allowed_extensions")
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty());
    Ok(Guards {
        root,
        min_free_bytes,
        ext_override,
    })
}

/// Refuse now if `need` bytes would not fit, rather than after a long upload.
///
/// `available_space` is a `statvfs` call and blocking, so it runs off the async
/// runtime. It answers `None` when it cannot tell, which makes the check a
/// no-op — the same fail-open the importer uses, since guessing wrong in the
/// other direction would refuse a perfectly good upload.
async fn check_space(dir: &FsPath, need: u64, reserve: u64) -> Result<(), AppError> {
    let probe = dir.to_path_buf();
    let available = tokio::task::spawn_blocking(move || skadi_importer::available_space(&probe))
        .await
        .unwrap_or(None);
    match skadi_importer::space_check(need, available, reserve) {
        skadi_importer::SpaceVerdict::Fits => Ok(()),
        skadi_importer::SpaceVerdict::Insufficient { need, available } => {
            Err(AppError::Validation(format!(
                "that needs {} but only {} is free once skadi's reserve is kept back",
                human_bytes(need),
                human_bytes(available)
            )))
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `<library.root>/incoming` — where a finished upload is published.
pub async fn incoming_dir(state: &AppState) -> Result<PathBuf, AppError> {
    Ok(library_root(state).await?.join(INCOMING))
}

/// `<library.root>/incoming/.sessions` — where in-flight uploads live.
pub async fn sessions_dir(state: &AppState) -> Result<PathBuf, AppError> {
    Ok(incoming_dir(state).await?.join(SESSIONS))
}

fn manifest_path(dir: &FsPath) -> PathBuf {
    dir.join("manifest.json")
}

fn part_path(dir: &FsPath) -> PathBuf {
    dir.join("part")
}

/// Resolve a session and prove the caller may touch it (SKADI-T-0630).
///
/// A `404` rather than a `403` for someone else's session, deliberately: a
/// contributor has no business learning which session ids exist, and the
/// difference between "no such session" and "not yours" is exactly that.
/// The admin keeps its blanket access, as everywhere else.
async fn owned_session(
    state: &AppState,
    id: &str,
    member: Option<axum::Extension<Member>>,
) -> Result<(PathBuf, Manifest), AppError> {
    let member = crate::household::member_or_admin(member);
    let dir = sessions_dir(state).await?.join(safe_id(id)?);
    let m = read_manifest(&dir).await?;
    if m.owner_id != member.id && !member.is_admin() {
        return Err(AppError::NotFound(format!("no upload session {id:?}")));
    }
    Ok((dir, m))
}

async fn read_manifest(dir: &FsPath) -> Result<Manifest, AppError> {
    let raw = tokio::fs::read_to_string(manifest_path(dir))
        .await
        .map_err(|_| AppError::NotFound("no such upload session".into()))?;
    serde_json::from_str(&raw)
        .map_err(|e| AppError::Internal(format!("upload manifest is unreadable: {e}")))
}

/// Bytes received so far == the length of the part file. One source of truth,
/// so a crash between "write bytes" and "record progress" is not a thing that
/// can happen.
async fn received_bytes(dir: &FsPath) -> u64 {
    tokio::fs::metadata(part_path(dir))
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

fn view_of(m: &Manifest, received: u64) -> SessionView {
    SessionView {
        id: m.id.clone(),
        filename: m.filename.clone(),
        kind: m.kind.clone(),
        size_bytes: m.size_bytes,
        received_bytes: received,
        chunk_bytes: CHUNK_BYTES,
        opened_at: m.opened_at,
    }
}

/// Extensions accepted per kind, mirroring what `skadi-media-probe` actually
/// dispatches on — there is no point accepting a file the probe at completion
/// is certain to reject.
///
/// Deliberately **narrower** than the prober for video: the prober also routes
/// `.avi`, `.vob`, `.wmv` and friends to ffprobe, which is optional in this
/// deploy, so accepting an upload whose validation depends on a binary that
/// may not be installed would turn a missing dependency into a failed 40 GB
/// upload. The operator can widen it without a rebuild — see
/// [`allowed_extensions`].
const VIDEO_EXT: &[&str] = &["mkv", "mp4", "m4v", "mov", "webm"];
const AUDIO_EXT: &[&str] = &["m4b", "m4a", "mp3", "flac", "ogg", "opus", "aac", "wav"];

/// The accepted extensions for `kind`, with the operator's override applied.
///
/// `uploads.allowed_extensions` is a comma-separated list that **replaces** the
/// built-in set when non-empty. A replacement rather than an addition, because
/// the case that matters is an operator who needs to get one odd file in and
/// should not have to reason about what they are also re-enabling.
pub fn allowed_extensions(kind: &str, override_csv: Option<&str>) -> Vec<String> {
    if let Some(csv) = override_csv {
        let list: Vec<String> = csv
            .split(',')
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if !list.is_empty() {
            return list;
        }
    }
    match kind {
        "audiobook" => AUDIO_EXT,
        _ => VIDEO_EXT,
    }
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// The lowercased extension of `name`, if it has one.
fn extension_of(name: &str) -> Option<String> {
    FsPath::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn check_extension(kind: &str, filename: &str, override_csv: Option<&str>) -> Result<(), AppError> {
    let allowed = allowed_extensions(kind, override_csv);
    let ext = extension_of(filename)
        .ok_or_else(|| AppError::Validation(format!("{filename:?} has no extension")))?;
    if allowed.contains(&ext) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        ".{ext} is not something skadi can take as a {kind}. Accepted: {}. \
         Widen `uploads.allowed_extensions` if you need another.",
        allowed.join(", ")
    )))
}

/// The kinds an upload can be destined for — the three domains that have a
/// library-import flow to hand it to.
fn check_kind(kind: &str) -> Result<(), AppError> {
    match kind {
        "movie" | "series" | "audiobook" => Ok(()),
        other => Err(AppError::Validation(format!(
            "{other:?} is not a media kind skadi can import — expected movie, series or audiobook"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn uploads_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/uploads", post(open_session).get(list_sessions))
        .route("/uploads/{id}", get(session_status).delete(delete_session))
        .route(
            "/uploads/{id}/chunk",
            // This route, and only this route, carries a large body limit.
            // Everything else stays small — see SKADI-T-0630.
            put(put_chunk).layer(DefaultBodyLimit::max(CHUNK_BODY_LIMIT)),
        )
        .route("/uploads/{id}/complete", post(complete_session))
}

/// `POST /uploads` — open a session.
async fn open_session(
    State(state): State<Arc<AppState>>,
    member: Option<axum::Extension<Member>>,
    Json(req): Json<OpenRequest>,
) -> Result<Json<SessionView>, ApiError> {
    let member = crate::household::member_or_admin(member);
    check_kind(&req.kind)?;
    let filename = safe_filename(&req.filename)?;
    if req.size_bytes == 0 {
        return Err(AppError::Validation("that file is empty".into()).into());
    }

    let g = guards(&state).await?;
    check_extension(&req.kind, &filename, g.ext_override.as_deref())?;
    // Checked before a single byte is accepted: discovering at 95% that the
    // disk was never going to hold it is the worst version of this failure.
    check_space(&g.root, req.size_bytes, g.min_free_bytes).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let dir = sessions_dir(&state).await?.join(&id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("creating the upload staging dir: {e}")))?;

    let manifest = Manifest {
        id: id.clone(),
        filename,
        kind: req.kind,
        size_bytes: req.size_bytes,
        owner_id: member.id.clone(),
        opened_at: Utc::now(),
        sha256: req.sha256,
    };
    write_manifest(&dir, &manifest).await?;
    // Create the part file immediately so `received_bytes` is 0 rather than
    // "no file", and so a full disk is discovered now rather than mid-upload.
    tokio::fs::File::create(part_path(&dir))
        .await
        .map_err(|e| AppError::Internal(format!("creating the upload part file: {e}")))?;

    Ok(Json(view_of(&manifest, 0)))
}

async fn write_manifest(dir: &FsPath, m: &Manifest) -> Result<(), AppError> {
    let raw = serde_json::to_vec_pretty(m)
        .map_err(|e| AppError::Internal(format!("serialising the upload manifest: {e}")))?;
    tokio::fs::write(manifest_path(dir), raw)
        .await
        .map_err(|e| AppError::Internal(format!("writing the upload manifest: {e}")))
}

/// `GET /uploads` — this member's live sessions, so a page reload does not
/// strand an upload that is still half-done on disk.
async fn list_sessions(
    State(state): State<Arc<AppState>>,
    member: Option<axum::Extension<Member>>,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    let member = crate::household::member_or_admin(member);
    let root = sessions_dir(&state).await?;
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(&root).await {
        Ok(e) => e,
        // No staging directory yet just means nobody has uploaded anything.
        Err(_) => return Ok(Json(out)),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let dir = entry.path();
        let Ok(m) = read_manifest(&dir).await else {
            continue;
        };
        if m.owner_id != member.id && !member.is_admin() {
            continue;
        }
        let received = received_bytes(&dir).await;
        out.push(view_of(&m, received));
    }
    out.sort_by(|a, b| a.opened_at.cmp(&b.opened_at));
    Ok(Json(out))
}

/// `GET /uploads/{id}` — the resume point.
async fn session_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    member: Option<axum::Extension<Member>>,
) -> Result<Json<SessionView>, ApiError> {
    let (dir, m) = owned_session(&state, &id, member).await?;
    let received = received_bytes(&dir).await;
    Ok(Json(view_of(&m, received)))
}

/// `PUT /uploads/{id}/chunk?offset=` — append raw bytes.
///
/// **Idempotent.** A chunk whose offset is behind the high-water mark is a
/// retry of something already held: answer the current state rather than an
/// error, so a client that timed out waiting for our `200` can safely re-send
/// without corrupting the file. An offset *ahead* of the mark would leave a
/// hole and is refused with what was expected.
async fn put_chunk(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<ChunkParams>,
    member: Option<axum::Extension<Member>>,
    body: Body,
) -> Result<Json<SessionView>, ApiError> {
    let (dir, m) = owned_session(&state, &id, member).await?;
    let have = received_bytes(&dir).await;

    if q.offset < have {
        // Already have these bytes. Nothing to do, and saying so is the whole
        // point of being idempotent.
        return Ok(Json(view_of(&m, have)));
    }
    if q.offset > have {
        return Err(AppError::Validation(format!(
            "chunk starts at {} but only {} bytes have arrived — send from {have}",
            q.offset, have
        ))
        .into());
    }

    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(part_path(&dir))
        .await
        .map_err(|e| AppError::Internal(format!("opening the upload part file: {e}")))?;

    // Streamed frame by frame, never collected: memory stays flat whatever the
    // file's size.
    let mut written = have;
    let mut stream = body.into_data_stream();
    while let Some(frame) = stream.next().await {
        let bytes = frame.map_err(|e| AppError::Validation(format!("upload stream ended: {e}")))?;
        if written + bytes.len() as u64 > m.size_bytes {
            return Err(AppError::Validation(format!(
                "that is more than the {} bytes this upload was opened for",
                m.size_bytes
            ))
            .into());
        }
        file.write_all(&bytes)
            .await
            .map_err(|e| AppError::Internal(format!("writing the upload chunk: {e}")))?;
        written += bytes.len() as u64;
    }
    file.flush()
        .await
        .map_err(|e| AppError::Internal(format!("flushing the upload chunk: {e}")))?;

    Ok(Json(view_of(&m, written)))
}

/// `POST /uploads/{id}/complete` — publish into the staging directory.
async fn complete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    member: Option<axum::Extension<Member>>,
) -> Result<Json<CompleteResponse>, ApiError> {
    let (dir, m) = owned_session(&state, &id, member).await?;
    let have = received_bytes(&dir).await;

    if have != m.size_bytes {
        return Err(AppError::Validation(format!(
            "{have} of {} bytes have arrived — the upload is not finished",
            m.size_bytes
        ))
        .into());
    }

    // A size match is weak evidence: it cannot tell an intact file from one
    // whose middle went wrong. When the client offered a checksum, hold it to
    // it — this is the only way a corrupt upload is caught before it is
    // adopted into the library.
    if let Some(want) = m.sha256.as_deref() {
        let got = sha256_file(&part_path(&dir)).await?;
        if !got.eq_ignore_ascii_case(want) {
            return Err(AppError::Validation(
                "the uploaded bytes do not match the checksum you sent — the upload is corrupt"
                    .into(),
            )
            .into());
        }
    }

    // **The probe is what this whole module adds over library-import.**
    // Adoption from disk does not probe — quality comes from the filename
    // parse, which is why `skadi-library-scan` exists to backfill what was
    // never measured. That is tolerable for files the operator already had.
    // It is not tolerable for a byte stream that arrived from a browser and
    // could be anything at all, so an upload proves it is readable media
    // before it is allowed to become adoptable.
    // The prober dispatches on **file extension**, and the part file has none
    // — probing it directly always answers `None`, which would reject every
    // upload ever made. So give it its real name first, inside the session
    // directory, and probe that. (Found by the integration test, which is
    // exactly the bug a unit test of the helpers could never have caught.)
    let named = dir.join(&m.filename);
    tokio::fs::rename(part_path(&dir), &named)
        .await
        .map_err(|e| AppError::Internal(format!("naming the finished upload: {e}")))?;

    let probe_path = named.clone();
    let info = tokio::task::spawn_blocking(move || {
        use skadi_media_probe::MediaProber;
        skadi_media_probe::DefaultProber.probe(&probe_path)
    })
    .await
    .map_err(|e| AppError::Internal(format!("probing the upload: {e}")))?;
    if info.is_none() {
        // Leave nothing behind: a file skadi will not take is not a file it
        // should be holding on the library mount.
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(AppError::Validation(format!(
            "{} does not read as media skadi can play — it may be corrupt, or \
             truncated, or not what its extension says it is",
            m.filename
        ))
        .into());
    }

    // Space is checked again here: the reserve is about the state of the disk
    // now, not when the session opened, and a long upload gives plenty of time
    // for something else to fill it.
    let g = guards(&state).await?;
    check_space(&g.root, 0, g.min_free_bytes).await?;

    let incoming = incoming_dir(&state).await?;
    tokio::fs::create_dir_all(&incoming)
        .await
        .map_err(|e| AppError::Internal(format!("creating the staging dir: {e}")))?;

    // One directory per upload, named by the session, so two people uploading
    // "movie.mkv" do not collide and so the published path is traceable back
    // to who sent it.
    let dest_dir = incoming.join(&m.id);
    tokio::fs::create_dir_all(&dest_dir)
        .await
        .map_err(|e| AppError::Internal(format!("creating the staging dir: {e}")))?;
    let dest = dest_dir.join(&m.filename);

    tokio::fs::rename(&named, &dest).await.map_err(|e| {
        AppError::Internal(format!("publishing the upload into the staging dir: {e}"))
    })?;
    // The session is spent; its manifest has served its purpose.
    let _ = tokio::fs::remove_dir_all(&dir).await;

    Ok(Json(CompleteResponse {
        path: dest.to_string_lossy().into_owned(),
        kind: m.kind,
        size_bytes: m.size_bytes,
    }))
}

/// `DELETE /uploads/{id}` — abandon.
async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    member: Option<axum::Extension<Member>>,
) -> Result<axum::http::StatusCode, ApiError> {
    // Resolving it proves it exists and is ours before anything is removed.
    let (dir, _) = owned_session(&state, &id, member).await?;
    tokio::fs::remove_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("removing the upload session: {e}")))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Hash the part file in bounded chunks. Reading a multi-gigabyte upload into
/// memory to check it would undo the point of streaming it in.
async fn sha256_file(path: &FsPath) -> Result<String, AppError> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::Internal(format!("reading the upload to checksum it: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| AppError::Internal(format!("reading the upload to checksum it: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// How long an untouched session is kept before the sweep removes it.
///
/// Generous, because the whole point of resumability is that someone comes
/// back to an upload later — but not unbounded, because a half-sent 40 GB film
/// otherwise sits on the library mount forever.
pub const SESSION_TTL_HOURS: i64 = 48;

/// Remove sessions nothing has touched in [`SESSION_TTL_HOURS`].
///
/// Keyed on the part file's **modified time**, not the manifest's `opened_at`:
/// a slow upload that has been running for three days is alive, and deleting
/// it because it *started* long ago would be the worst possible bug here.
pub fn sweep_sessions(sessions: &FsPath, ttl_hours: i64) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return removed;
    };
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs((ttl_hours.max(1) as u64) * 3600);
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let touched = std::fs::metadata(dir.join("part"))
            .or_else(|_| std::fs::metadata(dir.join("manifest.json")))
            .and_then(|m| m.modified())
            .ok();
        // A session we cannot date is left alone. Removing something because
        // its timestamp was unreadable would be guessing with someone's bytes.
        let Some(touched) = touched else { continue };
        if touched < cutoff && std::fs::remove_dir_all(&dir).is_ok() {
            removed.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    removed
}

/// Expire abandoned upload sessions on a schedule (SKADI-T-0629).
///
/// Modelled on `backup::recycle_sweep_loop`, including running the blocking
/// filesystem work off the async runtime.
pub async fn session_sweep_loop(state: Arc<AppState>, cancel: tokio_util::sync::CancellationToken) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
    ticker.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let Ok(dir) = sessions_dir(&state).await else { continue };
                let removed = tokio::task::spawn_blocking(move || {
                    sweep_sessions(&dir, SESSION_TTL_HOURS)
                })
                .await
                .unwrap_or_default();
                if !removed.is_empty() {
                    tracing::info!(
                        count = removed.len(),
                        sessions = ?removed,
                        "abandoned upload sessions swept"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filename_keeps_its_extension_and_loses_its_path() {
        assert_eq!(
            safe_filename("/tmp/Movie (2024).mkv").unwrap(),
            "Movie (2024).mkv"
        );
        assert_eq!(safe_filename("C:\\rips\\Movie.mkv").unwrap(), "Movie.mkv");
    }

    #[test]
    fn traversal_cannot_survive_a_filename() {
        // The basename is all that is kept, so the climb is simply gone.
        assert_eq!(safe_filename("../../etc/passwd.mkv").unwrap(), "passwd.mkv");
        assert_eq!(safe_filename("../../../a.mkv").unwrap(), "a.mkv");
        // Nothing but dots is not a filename at all.
        assert!(safe_filename("../..").is_err());
    }

    #[test]
    fn a_dotfile_cannot_be_created() {
        // Leading dots are stripped, so an upload cannot write `.skadi-root`
        // and convince the importer that the staging dir is a library root.
        assert_eq!(safe_filename(".skadi-root.mkv").unwrap(), "skadi-root.mkv");
        assert!(safe_filename(".skadi-root").is_err());
    }

    #[test]
    fn a_file_with_no_extension_is_refused() {
        // Both the probe and the importer dispatch on the extension, so a file
        // without one has nowhere to go.
        let err = safe_filename("README").unwrap_err();
        assert!(format!("{err}").contains("extension"), "{err}");
    }

    #[test]
    fn odd_characters_are_replaced_rather_than_dropped() {
        // Dropping them silently could collapse two different names into one.
        assert_eq!(safe_filename("a;b&c.mkv").unwrap(), "a_b_c.mkv");
    }

    #[test]
    fn the_built_in_extensions_are_ones_the_probe_can_actually_read() {
        // Accepting a file the probe at completion is certain to reject would
        // waste a whole upload before saying no.
        for ext in super::VIDEO_EXT.iter().chain(super::AUDIO_EXT) {
            assert!(
                matches!(
                    *ext,
                    "mkv"
                        | "webm"
                        | "mp4"
                        | "m4v"
                        | "mov"
                        | "m4b"
                        | "m4a"
                        | "mp3"
                        | "flac"
                        | "ogg"
                        | "opus"
                        | "aac"
                        | "wav"
                ),
                "{ext} is accepted but skadi-media-probe has no in-process reader for it"
            );
        }
    }

    #[test]
    fn video_and_audio_kinds_accept_different_files() {
        assert!(check_extension("movie", "a.mkv", None).is_ok());
        assert!(check_extension("series", "a.mp4", None).is_ok());
        assert!(check_extension("audiobook", "a.m4b", None).is_ok());
        // A film is not an audiobook and the error should say so rather than
        // leaving the uploader to guess.
        let err = check_extension("audiobook", "a.mkv", None).unwrap_err();
        assert!(format!("{err}").contains("audiobook"), "{err}");
        assert!(check_extension("movie", "a.m4b", None).is_err());
    }

    #[test]
    fn an_executable_dressed_as_a_film_is_refused_at_the_door() {
        // The probe would catch it at completion, but only after the whole
        // thing had been uploaded.
        assert!(check_extension("movie", "payload.exe", None).is_err());
        assert!(check_extension("movie", "notes.pdf", None).is_err());
        assert!(check_extension("movie", "archive.zip", None).is_err());
    }

    #[test]
    fn the_operator_override_replaces_the_list_rather_than_extending_it() {
        // Replacement, so an operator letting one odd file through does not
        // have to reason about what else they just re-enabled.
        let csv = Some("avi, .VOB");
        assert!(check_extension("movie", "old.avi", csv).is_ok());
        assert!(
            check_extension("movie", "old.vob", csv).is_ok(),
            "case and dot are tolerated"
        );
        assert!(
            check_extension("movie", "new.mkv", csv).is_err(),
            "an override replaces the built-in set"
        );
        // An empty or whitespace override is not an override.
        assert!(check_extension("movie", "new.mkv", Some("  ")).is_ok());
    }

    #[test]
    fn bytes_are_reported_in_units_a_person_reads() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn the_sweep_leaves_a_session_that_is_still_being_written() {
        let tmp = tempfile::tempdir().unwrap();
        let live = tmp.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("part"), b"bytes").unwrap();
        std::fs::write(live.join("manifest.json"), b"{}").unwrap();

        // A TTL far in the future: nothing is old enough.
        let removed = sweep_sessions(tmp.path(), 24 * 365);
        assert!(removed.is_empty(), "{removed:?}");
        assert!(live.exists());
    }

    #[test]
    fn the_sweep_will_not_guess_about_a_session_it_cannot_date() {
        // An empty directory has no part file and no manifest. Removing it
        // because its timestamp was unreadable would be guessing with
        // somebody's bytes.
        let tmp = tempfile::tempdir().unwrap();
        let odd = tmp.path().join("odd");
        std::fs::create_dir_all(&odd).unwrap();
        let removed = sweep_sessions(tmp.path(), 0);
        assert!(removed.is_empty(), "{removed:?}");
        assert!(odd.exists());
    }

    #[test]
    fn a_session_id_must_look_like_one() {
        assert!(safe_id("9f8c6b3a-0000-4444-8888-aaaaaaaaaaaa").is_ok());
        assert!(safe_id("../../etc").is_err());
        assert!(safe_id("").is_err());
        assert!(safe_id("a/b").is_err());
    }

    #[test]
    fn only_the_three_importable_kinds_are_accepted() {
        for k in ["movie", "series", "audiobook"] {
            assert!(check_kind(k).is_ok(), "{k}");
        }
        // No library-import flow exists for anything else, so accepting one
        // would open a session that could never be finished.
        let err = check_kind("music").unwrap_err();
        assert!(
            format!("{err}").contains("movie, series or audiobook"),
            "{err}"
        );
    }
}
