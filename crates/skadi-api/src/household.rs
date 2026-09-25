//! Household members (SKADI-I-0059, SKADI-T-0611): the operator plus the
//! people they hand the app to, each with their own bearer token, a role and
//! a content policy.
//!
//! Members are settings records of kind `members` (not exposed through the
//! generic `/settings/{kind}` CRUD — the token must never leak through a list
//! and the role/policy shape deserves validation). The **admin** member is the
//! operator: created on first boot from the existing `api_token` so every
//! phone paired today keeps working, and kept in step with that token when it
//! is rotated. `bearer_auth` resolves the presented token to a member and
//! hands it to handlers as an `Extension<Member>`; [`require_admin`] gates the
//! controller surface (SKADI-T-0612 applies it and the policy).

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use skadi_core::AppError;
use skadi_store::SettingsRepo;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::state::AppState;

pub const MEMBERS_KIND: &str = "members";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The operator: everything, exactly as before members existed.
    Admin,
    /// A household adult: sees and plays everything; changes nothing.
    Member,
    /// A household adult who can also **add media and ask skadi to find it**
    /// (SKADI-T-0625). Everything `Member` can do, plus adding a film, show or
    /// book and starting an automatic search for it.
    ///
    /// Deliberately *not* the operator's tools: no deleting, no picking a
    /// specific release by hand or pasting a link, no settings, no downloads,
    /// no library import. The line is "can add things and ask for them" versus
    /// "can change how skadi works or remove what is in it".
    Contributor,
    /// A child: sees and plays only what their policy permits.
    Kid,
}

impl Role {
    /// Whether this role may add media and start searches (SKADI-T-0625).
    #[must_use]
    pub fn can_contribute(self) -> bool {
        matches!(self, Role::Admin | Role::Contributor)
    }
}

/// Rating ceilings per kind, as source labels ("PG-13", "TV-14"); `None` = no ceiling.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaxRating {
    #[serde(default)]
    pub movie: Option<String>,
    #[serde(default)]
    pub series: Option<String>,
}

/// What a member may see. Enforced server-side by SKADI-T-0612.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Media kinds the member may see at all: `movie`, `series`, `audiobook`.
    #[serde(default = "Policy::all_kinds")]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub max_rating: MaxRating,
    #[serde(default)]
    pub blocked_genres: Vec<String>,
    /// Item ids hidden regardless of anything else.
    #[serde(default)]
    pub blocked_items: Vec<String>,
    /// Item ids shown regardless of rating/genre (an unrated film the parent vetted).
    #[serde(default)]
    pub allowed_items: Vec<String>,
    /// Audiobook ids a kid may see — the only way a kid sees a book (operator decision).
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

/// A member as stored (settings body) and as carried on requests. The token
/// is a secret: it appears on the wire only when created / re-issued and in
/// the pairing payload the admin renders.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// **Legacy single token** (pre-SKADI-I-0061), kept so a row written before
    /// logins existed still deserialises. [`AppState::refresh_members`] folds a
    /// non-empty one into `tokens` and blanks it, which is the whole of the
    /// migration — no device is ever signed out by the upgrade.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    /// One token per signed-in device, so a phone and a browser are separately
    /// revocable and "sign out all devices" means something (SKADI-T-0619).
    #[serde(default)]
    pub tokens: Vec<DeviceToken>,
    /// What this member logs in as — the display name, matched
    /// case-insensitively. `None` until the operator sets credentials, which is
    /// what lets the accounts that predate logins keep working untouched.
    #[serde(default)]
    pub username: Option<String>,
    /// Argon2 PHC string. Unlike a token — random, single-purpose, and in a
    /// record only the operator can read — a password is something a person
    /// reuses elsewhere, so it is never stored recoverably.
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_seen_at: Option<DateTime<Utc>>,
}

impl Member {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    /// The operator in open mode (no `api_token` configured): a synthetic
    /// admin so downstream code always has a member to ask.
    pub fn open_mode_admin() -> Self {
        Self {
            id: "admin".into(),
            name: "Operator".into(),
            role: Role::Admin,
            token: String::new(),
            tokens: Vec::new(),
            // The operator signs in by this name once they set a password; until
            // then `api_token` is the password (SKADI-I-0061 bootstrap).
            username: Some("Operator".into()),
            password_hash: None,
            pin: None,
            policy: Policy::default(),
            created_at: Utc::now(),
            last_seen_at: None,
        }
    }

    fn view(&self) -> MemberView {
        MemberView {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role,
            username: self.username.clone(),
            has_password: self.password_hash.is_some(),
            devices: self.tokens.len(),
            has_pin: self.pin.is_some(),
            policy: self.policy.clone(),
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
        }
    }
}

/// The wire shape of a member: everything but the secrets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// What they log in as; `None` until the operator sets credentials.
    pub username: Option<String>,
    /// Whether a password is set — never the hash, and never a token.
    pub has_password: bool,
    /// How many devices are signed in, for the operator's "sign out all".
    pub devices: usize,
    pub has_pin: bool,
    pub policy: Policy,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

/// One signed-in device's bearer token (SKADI-T-0619).
///
/// Long-lived on purpose: the operator's requirement is that the app is logged
/// into "exactly once", so there is no expiry and no refresh. Control is
/// revocation — this row going away — not a clock.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceToken {
    pub token: String,
    /// What signed in ("Pixel 7", "Firefox"), for the operator's benefit only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
}

impl DeviceToken {
    #[must_use]
    pub fn new(label: Option<String>) -> Self {
        Self {
            token: generate_token(),
            label: label
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
            created_at: Utc::now(),
        }
    }
}

/// Hash a password for storage (argon2id, per-password salt).
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError(AppError::Internal(format!("hashing password: {e}"))))
}

/// Check a password against a stored PHC string. A malformed or absent hash is
/// a `false`, never an error — a corrupt row must not become a way in.
#[must_use]
pub fn verify_password(password: &str, stored: Option<&str>) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    let Some(stored) = stored else { return false };
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Normalised form of a username for comparison: trimmed and lowercased, since
/// nobody in a household should have to remember capitalisation.
#[must_use]
pub fn username_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A fresh bearer token: two v4 UUIDs' worth of randomness, hex, no dashes.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn tokens_match(a: &str, b: &str) -> bool {
    !a.is_empty() && a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// In-memory copy of the members, refreshed from the store on boot, on the
/// config tick (the `api_token` reload) and after every mutation here.
#[derive(Default)]
pub struct MemberDirectory {
    pub members: Vec<Member>,
}

impl MemberDirectory {
    /// The member a presented token belongs to. The admin is matched on the
    /// live `api_token` first (so rotation through the config table keeps
    /// working even before the directory re-syncs), then every stored token.
    pub fn resolve(&self, presented: &str, live_admin_token: Option<&str>) -> Option<Member> {
        if let Some(t) = live_admin_token
            && tokens_match(presented, t)
        {
            return Some(
                self.members
                    .iter()
                    .find(|m| m.is_admin())
                    .cloned()
                    .unwrap_or_else(|| Member {
                        token: t.to_string(),
                        ..Member::open_mode_admin()
                    }),
            );
        }
        self.members
            .iter()
            .find(|m| {
                tokens_match(presented, &m.token)
                    || m.tokens.iter().any(|d| tokens_match(presented, &d.token))
            })
            .cloned()
    }
}

impl AppState {
    /// Re-read the members from the store. Creates the admin from the live
    /// `api_token` when none exists, and keeps the admin's token equal to it
    /// (the config table stays the source of truth for the operator's token).
    pub async fn refresh_members(&self) {
        let Some(store) = &self.store else { return };
        let live = self.live_token.read().await.clone();
        let mut members: Vec<Member> = match store.list_settings(MEMBERS_KIND).await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|r| {
                    serde_json::from_value::<Member>(r.body).ok().map(|mut m| {
                        m.id = r.id;
                        m
                    })
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "members: could not list");
                return;
            }
        };
        // Migration (SKADI-I-0061), and the whole of it: a row written before
        // logins carries one `token`; it becomes that member's first device
        // token so the device holding it keeps working untouched. The admin is
        // left alone — its `token` mirrors the live `api_token`, which is a
        // machine credential rather than a signed-in device.
        // Every account gets a login name, defaulted to its display name, so an
        // account made before logins existed can be signed into as soon as the
        // operator sets it a password (SKADI-I-0061).
        for m in members.iter_mut() {
            if m.username.is_none() {
                m.username = Some(m.name.clone());
                let body = serde_json::to_value(&*m).unwrap_or_default();
                let _ = store.put_setting(MEMBERS_KIND, &m.id, &body).await;
            }
        }
        for m in members.iter_mut().filter(|m| !m.is_admin()) {
            if m.token.is_empty() {
                continue;
            }
            let legacy = std::mem::take(&mut m.token);
            if !m.tokens.iter().any(|d| d.token == legacy) {
                m.tokens.push(DeviceToken {
                    token: legacy,
                    label: Some("paired device".into()),
                    created_at: m.created_at,
                });
            }
            let body = serde_json::to_value(&*m).unwrap_or_default();
            if let Err(e) = store.put_setting(MEMBERS_KIND, &m.id, &body).await {
                tracing::warn!(error = %e, member = %m.id, "members: could not migrate the paired token");
            } else {
                tracing::info!(member = %m.id, "members: paired token migrated to a device token");
            }
        }
        if let Some(token) = live.as_deref().filter(|t| !t.trim().is_empty()) {
            match members.iter_mut().find(|m| m.is_admin()) {
                None => {
                    let admin = Member {
                        token: token.to_string(),
                        ..Member::open_mode_admin()
                    };
                    if let Err(e) = store
                        .put_setting(
                            MEMBERS_KIND,
                            &admin.id,
                            &serde_json::to_value(&admin).unwrap_or_default(),
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "members: could not create the admin");
                    } else {
                        tracing::info!("members: admin created from api_token");
                        members.push(admin);
                    }
                }
                Some(admin) if admin.token != token => {
                    admin.token = token.to_string();
                    let body = serde_json::to_value(&*admin).unwrap_or_default();
                    let _ = store.put_setting(MEMBERS_KIND, &admin.id, &body).await;
                }
                Some(_) => {}
            }
        }
        self.members.write().await.members = members;
    }

    pub async fn resolve_member(&self, presented: &str) -> Option<Member> {
        let live = self.live_token.read().await.clone();
        let member = self
            .members
            .read()
            .await
            .resolve(presented, live.as_deref())?;
        self.touch_member(&member).await;
        Some(member)
    }

    /// Record that a member's token was just used (`last_seen_at`), at most
    /// once every ten minutes per member so the settings table is not written
    /// on every request. The Household page reads it as "seen <date>" versus
    /// "never paired".
    async fn touch_member(&self, m: &Member) {
        let every = chrono::Duration::minutes(10);
        let now = Utc::now();
        if m.last_seen_at.is_some_and(|t| now - t < every) {
            return;
        }
        let Some(store) = &self.store else { return };
        let (id, body) = {
            let mut dir = self.members.write().await;
            let Some(entry) = dir.members.iter_mut().find(|x| x.id == m.id) else {
                return;
            };
            if entry.last_seen_at.is_some_and(|t| now - t < every) {
                return;
            }
            entry.last_seen_at = Some(now);
            (
                entry.id.clone(),
                serde_json::to_value(&*entry).unwrap_or_default(),
            )
        };
        if let Err(e) = store.put_setting(MEMBERS_KIND, &id, &body).await {
            tracing::warn!(error = %e, member = %id, "members: could not record last_seen_at");
        }
    }

    async fn member_by_id(&self, id: &str) -> Option<Member> {
        self.members
            .read()
            .await
            .members
            .iter()
            .find(|m| m.id == id)
            .cloned()
    }
}

impl Policy {
    /// May a member with this policy see an item? (SKADI-T-0612)
    ///
    /// Order matters: an explicit block wins over everything, an explicit
    /// allow over every *rule*, then kind, genre and rating. Unrated items
    /// are hidden from a kid and visible to a member (operator decision).
    /// Audiobooks carry no ratings at all, so a kid sees only `allowed_books`.
    #[must_use]
    pub fn permits(
        &self,
        role: Role,
        kind: skadi_core::MediaKind,
        id: &str,
        rating: Option<&str>,
        genres: &[String],
    ) -> bool {
        use skadi_core::MediaKind;
        if role == Role::Admin {
            return true;
        }
        if self.blocked_items.iter().any(|b| b == id) {
            return false;
        }
        if self.allowed_items.iter().any(|a| a == id) {
            return true;
        }
        let kind_name = match kind {
            MediaKind::Movie => "movie",
            MediaKind::Series => "series",
            MediaKind::Audiobook | MediaKind::Book => "audiobook",
            _ => "other",
        };
        if !self.kinds.iter().any(|k| k == kind_name) {
            return false;
        }
        if matches!(kind, MediaKind::Audiobook | MediaKind::Book) {
            return role != Role::Kid || self.allowed_books.iter().any(|b| b == id);
        }
        if genres.iter().any(|g| {
            self.blocked_genres
                .iter()
                .any(|b| b.eq_ignore_ascii_case(g))
        }) {
            return false;
        }
        let max = match kind {
            MediaKind::Movie => self.max_rating.movie.as_deref(),
            MediaKind::Series => self.max_rating.series.as_deref(),
            _ => None,
        };
        let Some(max) = max else { return true };
        let Some(max) = skadi_core::rating::parse_rating(kind, max) else {
            // An unparseable ceiling is a misconfiguration; fail closed for kids.
            return role != Role::Kid;
        };
        match rating.and_then(|r| skadi_core::rating::parse_rating(kind, r)) {
            Some(r) => r.at_most(&max),
            None => role != Role::Kid,
        }
    }
}

/// The read/play surface a non-admin may reach (SKADI-T-0612). Everything
/// else on the API is the operator's. Paths are as the nested API sees them
/// (no `/api/v1`). Kids get the narrower list: no author/series/works
/// rollups, which would name books their policy hides.
#[must_use]
pub fn path_allowed(role: Role, method: &axum::http::Method, path: &str) -> bool {
    if role == Role::Admin {
        return true;
    }
    // Signing out and changing your own password are the two things every
    // account must be able to do for itself (SKADI-T-0620). They are POSTs, so
    // they have to be named before the read-only rule below.
    if path == "/auth/logout" || path == "/auth/password" {
        return true;
    }
    // A contributor's extra surface (SKADI-T-0625): add a thing, and ask skadi
    // to find it. Named before the read-only rule because both are POSTs.
    if role.can_contribute() {
        let seg: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let adding = matches!(seg.as_slice(), ["movies"] | ["series"] | ["books"]);
        let searching = matches!(
            seg.as_slice(),
            ["movies", _, "editions", _, "acquire"]
                | ["series", _, "episodes", _, "acquire"]
                | ["books", _, "files", _, "acquire"]
        );
        if method == axum::http::Method::POST && (adding || searching) {
            return true;
        }
        // Uploading media (SKADI-T-0630). The same intent as "add a thing and
        // ask skadi to find it", with the file already in hand — so it belongs
        // to the role that already means that.
        //
        // These arms are listed **before** the read-only rule below on purpose:
        // an upload is a sequence of POSTs, PUTs and a DELETE, none of which
        // survive that rule. `["uploads", _]` is deliberately the *last* of
        // them, because a match wins by order and it would otherwise swallow
        // `/uploads/{id}/chunk` — the exact shadowing that let a read-only
        // member reach catalog search in SKADI-T-0625.
        let uploading = matches!(
            seg.as_slice(),
            ["uploads"] | ["uploads", _, "chunk"] | ["uploads", _, "complete"] | ["uploads", _]
        );
        if uploading {
            return true;
        }
    }
    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        return false;
    }
    let seg: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match seg.as_slice() {
        ["me"] | ["health"] | ["health", "ready"] => true,
        // These MUST precede the `["movies", _]` style arms below: a match arm
        // wins by order, and `["movies", _]` happily matches `/movies/lookup`
        // as though "lookup" were an id. It did exactly that until a live check
        // showed a read-only member reaching the catalog search
        // (SKADI-T-0625).
        ["movies", "lookup"] | ["series", "lookup"] | ["books", "lookup"] | ["books", "search"] => {
            role.can_contribute()
        }
        ["wanted"] => role.can_contribute(),
        ["settings", "profiles"] => role.can_contribute(),
        ["movies"] | ["movies", _] => true,
        ["movies", _, "editions", _, "video"] => true,
        ["series"] | ["series", _] => true,
        ["series", _, "episodes", _, "video"] => true,
        ["books"] | ["books", _] => true,
        ["books", _, "files", _, "audio"] | ["books", _, "files", _, "chapters"] => true,
        ["library", "genres"] => true,
        // Which media kinds exist and are enabled. Not sensitive, and the
        // sidebar builds its library links from it — so gating this to the
        // admin meant a read-only member saw **no Movies, TV or Audiobooks
        // links at all**, in the one role that exists purely to browse and
        // play. Found by the per-role UI sweep (SKADI-T-0639, 2026-09-25).
        ["domains"] => true,
        ["authors"] | ["audiobooks", "series"] | ["audiobooks", "works"] => role != Role::Kid,
        _ => false,
    }
}

/// Middleware: after `bearer_auth` has named the member, refuse anything off
/// the read/play surface for non-admins with the app's "not allowed" 403.
pub async fn household_gate(
    member: Option<Extension<Member>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // No member on the request means no bearer layer above us (a test
    // harness mounting the router bare): that is the operator.
    let member = member_or_admin(member);
    if path_allowed(member.role, request.method(), request.uri().path()) {
        next.run(request).await
    } else {
        forbidden()
    }
}

/// The member on a request, or the operator when a router is mounted without
/// `bearer_auth` (domain crates' own test harnesses).
#[must_use]
pub fn member_or_admin(ext: Option<Extension<Member>>) -> Member {
    ext.map(|Extension(m)| m)
        .unwrap_or_else(Member::open_mode_admin)
}

/// A 401 for a login that did not match. Deliberately one message for both
/// halves: saying "no such user" would turn this into a way to enumerate the
/// household (SKADI-T-0620).
fn bad_credentials(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "unauthorized", "message": message})),
    )
        .into_response()
}

fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({"error": "forbidden", "message": "not allowed on this account"})),
    )
        .into_response()
}

/// Middleware: only the admin passes. Members and kids get 403 with the
/// message the app shows verbatim.
pub async fn require_admin(
    Extension(member): Extension<Member>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if member.is_admin() {
        next.run(request).await
    } else {
        forbidden()
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateMember {
    name: String,
    role: Role,
    /// Defaults to `name` — the display name is the login name.
    #[serde(default)]
    username: Option<String>,
    /// Set now, or later from the Household page.
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    pin: Option<String>,
    #[serde(default)]
    policy: Option<Policy>,
}

#[derive(Debug, Deserialize)]
struct PatchMember {
    name: Option<String>,
    role: Option<Role>,
    username: Option<String>,
    /// Setting a password **signs every one of that member's devices out**
    /// (SKADI-I-0061): a password change is the remedy when one has leaked, and
    /// a remedy that leaves the old devices signed in is cosmetic.
    password: Option<String>,
    /// `Some(None)` is not expressible in JSON here; send `""` to clear.
    pin: Option<String>,
    policy: Option<Policy>,
}

#[derive(Debug, Serialize)]
struct CreatedMember {
    member: MemberView,
    /// Shown once. The pairing QR carries it too.
    token: String,
}

fn store(state: &AppState) -> Result<&skadi_store::Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("store not configured".into())))
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    if n.is_empty() || n.len() > 64 {
        return Err(ApiError(AppError::Validation(
            "member name must be 1–64 characters".into(),
        )));
    }
    Ok(n.to_string())
}

/// A username that is non-empty and not already taken, case-insensitively.
/// `skip` is the member being renamed, so it does not collide with itself.
async fn validate_username(
    state: &AppState,
    username: &str,
    skip: Option<&str>,
) -> Result<String, ApiError> {
    let name = username.trim();
    if name.is_empty() {
        return Err(ApiError(AppError::Validation(
            "a username cannot be blank".into(),
        )));
    }
    let key = username_key(name);
    let taken = state.members.read().await.members.iter().any(|m| {
        Some(m.id.as_str()) != skip
            && m.username.as_deref().map(username_key).as_deref() == Some(key.as_str())
    });
    if taken {
        return Err(ApiError(AppError::Validation(format!(
            "the username {name:?} is already taken"
        ))));
    }
    Ok(name.to_string())
}

async fn list_members(State(state): State<Arc<AppState>>) -> Json<Vec<MemberView>> {
    let dir = state.members.read().await;
    Json(dir.members.iter().map(Member::view).collect())
}

async fn create_member(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateMember>,
) -> Result<impl IntoResponse, ApiError> {
    if req.role == Role::Admin {
        return Err(ApiError(AppError::Validation(
            "there is one admin: the operator".into(),
        )));
    }
    let name = validate_name(&req.name)?;
    // The display name *is* the login name (SKADI-I-0061): a household does not
    // need a second identifier to remember, and "Robin" is already on the page.
    let username = req.username.unwrap_or_else(|| name.clone());
    let username = validate_username(&state, &username, None).await?;
    let password_hash = match req.password.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(p) => Some(hash_password(p)?),
        None => None,
    };
    // A first device token is still minted here so the existing "shown once"
    // hand-over keeps working for a device that is not going to log in.
    let first = DeviceToken::new(Some("initial".into()));
    let member = Member {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        role: req.role,
        token: String::new(),
        tokens: vec![first.clone()],
        username: Some(username),
        password_hash,
        pin: req.pin.filter(|p| !p.trim().is_empty()),
        policy: req.policy.unwrap_or_default(),
        created_at: Utc::now(),
        last_seen_at: None,
    };
    store(&state)?
        .put_setting(
            MEMBERS_KIND,
            &member.id,
            &serde_json::to_value(&member).unwrap_or_default(),
        )
        .await?;
    state.refresh_members().await;
    Ok((
        StatusCode::CREATED,
        Json(CreatedMember {
            member: member.view(),
            token: first.token.clone(),
        }),
    ))
}

async fn get_member(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<MemberView>, ApiError> {
    state
        .member_by_id(&id)
        .await
        .map(|m| Json(m.view()))
        .ok_or_else(|| ApiError(AppError::NotFound(format!("member {id} not found"))))
}

async fn patch_member(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<PatchMember>,
) -> Result<Json<MemberView>, ApiError> {
    let mut member = state
        .member_by_id(&id)
        .await
        .ok_or_else(|| ApiError(AppError::NotFound(format!("member {id} not found"))))?;
    if let Some(n) = req.name {
        let new_name = validate_name(&n)?;
        // The login name follows the display name, because the design says they
        // are the same thing (SKADI-I-0061) — but only when it was *tracking*
        // the name. A username deliberately set to something else is left
        // alone. Without this, renaming an account silently left the old login
        // name behind and the rename looked like it had not worked at all.
        let was_tracking = member
            .username
            .as_deref()
            .is_some_and(|u| username_key(u) == username_key(&member.name));
        if was_tracking && req.username.is_none() {
            member.username = Some(validate_username(&state, &new_name, Some(&member.id)).await?);
        }
        member.name = new_name;
    }
    if let Some(r) = req.role {
        if member.is_admin() != (r == Role::Admin) {
            return Err(ApiError(AppError::Validation(
                "the admin role cannot be given or taken away".into(),
            )));
        }
        member.role = r;
    }
    if let Some(u) = req.username {
        member.username = Some(validate_username(&state, &u, Some(&member.id)).await?);
    }
    if let Some(p) = req.password.filter(|p| !p.trim().is_empty()) {
        member.password_hash = Some(hash_password(&p)?);
        // The remedy path: a password is changed *because* the old one reached
        // someone it should not have, so every device holding a token minted
        // under it is signed out (SKADI-I-0061).
        member.tokens.clear();
        member.token.clear();
    }
    if let Some(p) = req.pin {
        member.pin = Some(p).filter(|p| !p.trim().is_empty());
    }
    if let Some(p) = req.policy {
        member.policy = p;
    }
    store(&state)?
        .put_setting(
            MEMBERS_KIND,
            &member.id,
            &serde_json::to_value(&member).unwrap_or_default(),
        )
        .await?;
    state.refresh_members().await;
    Ok(Json(member.view()))
}

async fn delete_member(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let member = state
        .member_by_id(&id)
        .await
        .ok_or_else(|| ApiError(AppError::NotFound(format!("member {id} not found"))))?;
    if member.is_admin() {
        return Err(ApiError(AppError::Validation(
            "the admin cannot be removed".into(),
        )));
    }
    store(&state)?.delete_setting(MEMBERS_KIND, &id).await?;
    state.refresh_members().await;
    Ok(StatusCode::NO_CONTENT)
}

async fn reissue_token(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<CreatedMember>, ApiError> {
    let mut member = state
        .member_by_id(&id)
        .await
        .ok_or_else(|| ApiError(AppError::NotFound(format!("member {id} not found"))))?;
    if member.is_admin() {
        return Err(ApiError(AppError::Validation(
            "rotate the operator's token via the api_token config".into(),
        )));
    }
    // Re-issue means the old ones stop working, which is what an operator
    // reaches for when a device is lost.
    let fresh = DeviceToken::new(Some("re-issued".into()));
    member.token.clear();
    member.tokens = vec![fresh.clone()];
    store(&state)?
        .put_setting(
            MEMBERS_KIND,
            &member.id,
            &serde_json::to_value(&member).unwrap_or_default(),
        )
        .await?;
    state.refresh_members().await;
    Ok(Json(CreatedMember {
        member: member.view(),
        token: fresh.token.clone(),
    }))
}

/// The widening delay that makes a password guess expensive (SKADI-T-0620).
///
/// Deliberately **not** a lockout: on a household server a lockout is a way for
/// one sibling to lock another out of the TV. A streak of failures just costs
/// more and more time, and one success clears it.
///
/// Keyed on the username alone. Keying on the source address too would be
/// better in principle, but the daemon is not started with axum's
/// `ConnectInfo`, so a peer address is not available to a handler without
/// changing `serve` and every test harness that calls `router(state)`. For a
/// family media server the attack that matters is guessing a member's password,
/// and that is what this raises the cost of.
#[derive(Default)]
pub struct LoginThrottle {
    attempts: std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>,
}

/// Longest a failed attempt is made to wait.
const MAX_LOGIN_DELAY: Duration = Duration::from_secs(8);
/// A streak this stale is forgotten — a wrong password last week should not
/// slow down someone typing the right one today.
const LOGIN_STREAK_TTL: Duration = Duration::from_secs(15 * 60);

impl LoginThrottle {
    /// How long this attempt should be made to wait before it is answered.
    #[must_use]
    pub fn penalty(&self, key: &str) -> Duration {
        let Ok(map) = self.attempts.lock() else {
            return Duration::ZERO;
        };
        match map.get(key) {
            Some((n, at)) if at.elapsed() < LOGIN_STREAK_TTL && *n > 0 => {
                let ms = 250u64.saturating_mul(1u64 << (*n - 1).min(6));
                Duration::from_millis(ms).min(MAX_LOGIN_DELAY)
            }
            _ => Duration::ZERO,
        }
    }

    fn record_failure(&self, key: &str) {
        if let Ok(mut map) = self.attempts.lock() {
            map.retain(|_, (_, at)| at.elapsed() < LOGIN_STREAK_TTL);
            let e = map.entry(key.to_string()).or_insert((0, Instant::now()));
            e.0 = e.0.saturating_add(1);
            e.1 = Instant::now();
        }
    }

    fn clear(&self, key: &str) {
        if let Ok(mut map) = self.attempts.lock() {
            map.remove(key);
        }
    }
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    /// What is signing in ("Pixel 7"), shown to the operator. Optional.
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PasswordChange {
    current: String,
    new: String,
}

/// `POST /auth/login` — the one unauthenticated route (SKADI-T-0620).
///
/// Answers identically for a wrong password and an unknown username, in body
/// and in time: an unknown username still runs a verify against a throwaway
/// hash, so the response cannot be used to enumerate the household.
async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let key = username_key(&req.username);
    tokio::time::sleep(state.login_throttle.penalty(&key)).await;

    let found = {
        let dir = state.members.read().await;
        dir.members
            .iter()
            .find(|m| m.username.as_deref().map(username_key).as_deref() == Some(key.as_str()))
            .cloned()
    };
    // Always spend the verify, so "no such user" and "wrong password" cost the
    // same. `DUMMY_HASH` is a real argon2 hash of a value nothing can present.
    // Bootstrap (SKADI-I-0061): the operator has always authenticated with
    // `api_token` and has no password until they set one. Without this, the
    // deploy that adds the login page locks them out of their own console —
    // the token is not a *new* credential, it is the one that already grants
    // admin, and this path disappears the moment a real password is set.
    let admin_bootstrap = match &found {
        Some(m) if m.is_admin() && m.password_hash.is_none() => {
            let live = state.live_token.read().await.clone();
            live.as_deref()
                .filter(|t| !t.trim().is_empty())
                .is_some_and(|t| tokens_match(&req.password, t))
        }
        _ => false,
    };
    let ok = admin_bootstrap
        || match &found {
            Some(m) => verify_password(&req.password, m.password_hash.as_deref()),
            None => {
                let _ = verify_password(&req.password, Some(dummy_hash()));
                false
            }
        };
    let Some(mut member) = found.filter(|_| ok) else {
        state.login_throttle.record_failure(&key);
        return Ok(bad_credentials("that username and password do not match"));
    };
    state.login_throttle.clear(&key);

    // A long-lived device token: the operator's requirement is that the app is
    // logged into "exactly once", so there is no expiry and no refresh. The
    // control is revocation, not a clock (SKADI-I-0061).
    let device = DeviceToken::new(req.label);
    member.tokens.push(device.clone());
    persist_member(&state, &member).await?;
    Ok(Json(CreatedMember {
        member: member.view(),
        token: device.token,
    })
    .into_response())
}

/// A stable argon2 hash used only to keep the unknown-username path as slow as
/// the wrong-password path.
fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        hash_password("a password nothing presents").unwrap_or_else(|_| String::from("$"))
    })
}

/// `POST /auth/logout` — sign **this** device out, leaving the member's others.
async fn logout(
    State(state): State<Arc<AppState>>,
    Extension(member): Extension<Member>,
    presented: Option<Extension<crate::auth::PresentedToken>>,
) -> Result<StatusCode, ApiError> {
    let Some(Extension(crate::auth::PresentedToken(token))) = presented else {
        // Open mode: nothing was presented, so there is nothing to revoke.
        return Ok(StatusCode::NO_CONTENT);
    };
    let mut member = member;
    let before = member.tokens.len();
    member.tokens.retain(|d| d.token != token);
    if member.tokens.len() != before {
        persist_member(&state, &member).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /auth/password` — a member changes their own.
///
/// Signs their **other** devices out and keeps the caller signed in: the point
/// is to cut off whoever learned the old password, not to lock yourself out of
/// the device in your hand.
async fn change_password(
    State(state): State<Arc<AppState>>,
    Extension(member): Extension<Member>,
    presented: Option<Extension<crate::auth::PresentedToken>>,
    Json(req): Json<PasswordChange>,
) -> Result<Response, ApiError> {
    let mut member = member;
    if !verify_password(&req.current, member.password_hash.as_deref()) {
        return Ok(bad_credentials("that is not the current password"));
    }
    let new = req.new.trim();
    if new.len() < 6 {
        return Err(ApiError(AppError::Validation(
            "a password needs at least 6 characters".into(),
        )));
    }
    member.password_hash = Some(hash_password(new)?);
    let keep = presented.map(|Extension(t)| t.0).unwrap_or_default();
    member.tokens.retain(|d| d.token == keep);
    member.token.clear();
    persist_member(&state, &member).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /members/{id}/signout` — the operator signs every device out.
async fn signout_all(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<MemberView>, ApiError> {
    let mut member = state
        .member_by_id(&id)
        .await
        .ok_or_else(|| ApiError(AppError::NotFound(format!("member {id} not found"))))?;
    member.tokens.clear();
    member.token.clear();
    persist_member(&state, &member).await?;
    Ok(Json(member.view()))
}

/// Write a member back and refresh the in-memory directory.
async fn persist_member(state: &AppState, member: &Member) -> Result<(), ApiError> {
    store(state)?
        .put_setting(
            MEMBERS_KIND,
            &member.id,
            &serde_json::to_value(member).unwrap_or_default(),
        )
        .await?;
    state.refresh_members().await;
    Ok(())
}

/// `GET /me` — who the caller is. Any role.
async fn me(Extension(member): Extension<Member>) -> Json<MemberView> {
    Json(member.view())
}

pub fn household_router() -> Router<Arc<AppState>> {
    let admin_only = Router::new()
        .route("/members", get(list_members).post(create_member))
        .route(
            "/members/{id}",
            get(get_member).patch(patch_member).delete(delete_member),
        )
        .route("/members/{id}/token", post(reissue_token))
        .route("/members/{id}/signout", post(signout_all))
        .layer(axum::middleware::from_fn(require_admin));
    Router::new()
        .route("/me", get(me))
        // Unauthenticated — `bearer_auth` exempts exactly this path.
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/password", post(change_password))
        .merge(admin_only)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member whose single device holds `token` — the shape a row takes once
    /// it has been through the SKADI-I-0061 migration.
    fn member(role: Role, token: &str) -> Member {
        Member {
            id: token.to_string(),
            name: token.to_string(),
            role,
            token: String::new(),
            tokens: vec![DeviceToken {
                token: token.to_string(),
                label: None,
                created_at: Utc::now(),
            }],
            username: Some(token.to_string()),
            password_hash: None,
            pin: None,
            policy: Policy::default(),
            created_at: Utc::now(),
            last_seen_at: None,
        }
    }

    /// The pre-login shape: one `token` field, no device list. Still has to
    /// authenticate, because nothing is signed out by the upgrade.
    fn legacy_member(role: Role, token: &str) -> Member {
        Member {
            token: token.to_string(),
            tokens: Vec::new(),
            ..member(role, token)
        }
    }

    #[test]
    fn a_token_paired_before_logins_still_authenticates() {
        let dir = MemberDirectory {
            members: vec![legacy_member(Role::Kid, "paired-long-ago")],
        };
        let found = dir.resolve("paired-long-ago", Some("operator")).unwrap();
        assert_eq!(found.role, Role::Kid);
    }

    #[test]
    fn every_device_token_authenticates_and_others_do_not() {
        let mut m = member(Role::Member, "phone");
        m.tokens.push(DeviceToken::new(Some("browser".into())));
        let browser = m.tokens[1].token.clone();
        let dir = MemberDirectory { members: vec![m] };
        assert!(dir.resolve("phone", None).is_some(), "the phone");
        assert!(dir.resolve(&browser, None).is_some(), "and the browser");
        assert!(dir.resolve("neither", None).is_none());
    }

    #[test]
    fn passwords_round_trip_and_a_wrong_one_fails() {
        let hash = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", Some(&hash)));
        assert!(
            !verify_password("Correct Horse", Some(&hash)),
            "case matters"
        );
        assert!(!verify_password("wrong", Some(&hash)));
        // The hash is never the password, and a member without one cannot pass.
        assert!(!hash.contains("correct horse"));
        assert!(!verify_password("anything", None));
        // A corrupt stored value is a refusal, not a way in and not a panic.
        assert!(!verify_password("anything", Some("not-a-phc-string")));
    }

    #[test]
    fn usernames_compare_case_and_space_insensitively() {
        assert_eq!(username_key("  Robin "), "robin");
        assert_eq!(username_key("ROBIN"), username_key("robin"));
    }

    #[test]
    fn the_wire_shape_carries_no_secret() {
        // Distinct from the id/name/username, so finding the string in the JSON
        // means a *token* leaked rather than the member's own name.
        let mut m = member(Role::Kid, "robin");
        m.tokens = vec![DeviceToken {
            token: "secret-token-value".into(),
            label: None,
            created_at: Utc::now(),
        }];
        m.password_hash = Some(hash_password("hunter2").unwrap());
        let json = serde_json::to_string(&m.view()).unwrap();
        assert!(!json.contains("secret-token-value"), "no token: {json}");
        assert!(!json.contains("hunter2"));
        assert!(!json.contains("argon2"), "no hash either: {json}");
        assert!(json.contains("\"has_password\":true"));
        assert!(json.contains("\"devices\":1"));
    }

    #[test]
    fn resolves_admin_by_live_token_and_members_by_their_own() {
        let dir = MemberDirectory {
            members: vec![
                member(Role::Admin, "stale-admin-token"),
                member(Role::Kid, "kid-token"),
            ],
        };
        let admin = dir.resolve("live-admin", Some("live-admin")).unwrap();
        assert!(
            admin.is_admin(),
            "the live api_token is the admin even before the row is re-synced"
        );
        assert_eq!(
            dir.resolve("kid-token", Some("live-admin")).unwrap().role,
            Role::Kid
        );
        assert!(dir.resolve("nope", Some("live-admin")).is_none());
        assert!(
            dir.resolve("", Some("live-admin")).is_none(),
            "an empty token never matches"
        );
        assert!(dir.resolve("kid-tokeN", Some("live-admin")).is_none());
    }

    #[test]
    fn policy_round_trips_and_defaults_open() {
        let p: Policy = serde_json::from_str("{}").unwrap();
        assert_eq!(p.kinds.len(), 3);
        assert!(p.max_rating.movie.is_none());
        let p: Policy = serde_json::from_str(r#"{"kinds":["movie"],"max_rating":{"movie":"PG"},"blocked_genres":["Horror"],"allowed_books":["b1"]}"#).unwrap();
        let back: Policy = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(p, back);
        assert_eq!(back.max_rating.movie.as_deref(), Some("PG"));
    }

    #[test]
    fn policy_permits_follows_block_allow_kind_genre_rating_order() {
        use skadi_core::MediaKind::{Audiobook, Movie, Series};
        let kid = Role::Kid;
        let p = Policy {
            kinds: vec!["movie".into(), "series".into(), "audiobook".into()],
            max_rating: MaxRating {
                movie: Some("PG".into()),
                series: Some("TV-PG".into()),
            },
            blocked_genres: vec!["Horror".into()],
            blocked_items: vec!["blocked-1".into()],
            allowed_items: vec!["vetted-unrated".into()],
            allowed_books: vec!["book-ok".into()],
        };
        let g = |x: &str| vec![x.to_string()];
        assert!(p.permits(kid, Movie, "m1", Some("PG"), &g("Family")));
        assert!(
            !p.permits(kid, Movie, "m2", Some("R"), &g("Action")),
            "over the ceiling"
        );
        assert!(
            !p.permits(kid, Movie, "m3", None, &g("Family")),
            "unrated hidden from a kid"
        );
        assert!(
            p.permits(Role::Member, Movie, "m3", None, &g("Family")),
            "unrated visible to a member"
        );
        assert!(
            p.permits(kid, Movie, "vetted-unrated", None, &[]),
            "explicit allow beats the rating rule"
        );
        assert!(
            !p.permits(kid, Movie, "blocked-1", Some("G"), &[]),
            "explicit block beats everything"
        );
        assert!(
            !p.permits(kid, Movie, "m4", Some("G"), &g("Horror")),
            "blocked genre"
        );
        assert!(p.permits(kid, Series, "s1", Some("TV-Y7"), &[]));
        assert!(!p.permits(kid, Series, "s2", Some("TV-14"), &[]));
        assert!(
            !p.permits(kid, Movie, "m5", Some("TV-PG"), &[]),
            "a TV label on a film is unrated for a kid"
        );
        assert!(p.permits(kid, Audiobook, "book-ok", None, &[]));
        assert!(
            !p.permits(kid, Audiobook, "book-no", None, &[]),
            "kids: allow-list only"
        );
        assert!(p.permits(Role::Member, Audiobook, "book-no", None, &[]));
        assert!(p.permits(Role::Admin, Movie, "blocked-1", Some("NC-17"), &g("Horror")));
        // A ceiling binds whoever *holds* that policy, whatever their role — it
        // is a property of the policy, not of being a kid.
        assert!(!p.permits(Role::Contributor, Movie, "m2", Some("R"), &g("Action")));
        // What the role changes is the unrated default and the audiobook rule:
        // an adult sees unrated and un-allow-listed books, a kid does not.
        assert!(
            p.permits(Role::Contributor, Movie, "m3", None, &g("Family")),
            "unrated visible"
        );
        assert!(p.permits(Role::Contributor, Audiobook, "book-no", None, &[]));
        // An adult with no ceiling set sees everything.
        let open = Policy::default();
        assert!(open.permits(Role::Contributor, Movie, "m2", Some("NC-17"), &g("Horror")));
        assert!(open.permits(Role::Member, Movie, "m2", Some("NC-17"), &g("Horror")));

        let no_movies = Policy {
            kinds: vec!["series".into()],
            ..Policy::default()
        };
        assert!(
            !no_movies.permits(Role::Member, Movie, "m1", Some("G"), &[]),
            "kind off the list"
        );
    }

    #[test]
    fn everyone_can_sign_themselves_out_and_change_their_own_password() {
        use axum::http::Method;
        for role in [Role::Member, Role::Kid] {
            assert!(path_allowed(role, &Method::POST, "/auth/logout"));
            assert!(path_allowed(role, &Method::POST, "/auth/password"));
            // But not anyone else's account.
            assert!(!path_allowed(role, &Method::POST, "/members/abc/signout"));
            assert!(!path_allowed(role, &Method::POST, "/members/abc/token"));
        }
    }

    #[test]
    fn the_delay_widens_with_failures_and_a_success_clears_it() {
        let t = LoginThrottle::default();
        assert_eq!(t.penalty("robin"), Duration::ZERO, "a first try is free");
        t.record_failure("robin");
        let one = t.penalty("robin");
        t.record_failure("robin");
        let two = t.penalty("robin");
        assert!(one > Duration::ZERO);
        assert!(two > one, "{two:?} should exceed {one:?}");
        // Bounded, so a long streak cannot wedge the route for the real user.
        for _ in 0..20 {
            t.record_failure("robin");
        }
        assert!(t.penalty("robin") <= MAX_LOGIN_DELAY);
        // And one success wipes the slate.
        t.clear("robin");
        assert_eq!(t.penalty("robin"), Duration::ZERO);
        // One member's streak never slows another down.
        t.record_failure("robin");
        assert_eq!(t.penalty("sam"), Duration::ZERO);
    }

    /// SKADI-T-0625: the contributor's line is "can add things and ask skadi to
    /// find them", not "can change how skadi works or remove what is in it".
    #[test]
    fn a_contributor_can_add_and_search_but_not_administer() {
        use axum::http::Method;
        let c = Role::Contributor;
        assert!(path_allowed(c, &Method::POST, "/movies"));
        assert!(path_allowed(c, &Method::POST, "/series"));
        assert!(path_allowed(c, &Method::POST, "/books"));
        assert!(path_allowed(c, &Method::GET, "/movies/lookup"));
        assert!(path_allowed(c, &Method::GET, "/books/search"));
        assert!(path_allowed(c, &Method::GET, "/wanted"));
        assert!(
            path_allowed(c, &Method::GET, "/settings/profiles"),
            "the add flow needs a profile to add against"
        );
        assert!(path_allowed(
            c,
            &Method::POST,
            "/movies/abc/editions/def/acquire"
        ));
        assert!(path_allowed(
            c,
            &Method::POST,
            "/series/abc/episodes/def/acquire"
        ));

        // Everything that changes how skadi works, or removes something, stays
        // the operator's — including picking a release by hand, which is where
        // a wrong grab comes from.
        assert!(!path_allowed(c, &Method::DELETE, "/movies/abc"));
        assert!(!path_allowed(c, &Method::PATCH, "/movies/abc"));
        assert!(!path_allowed(
            c,
            &Method::GET,
            "/movies/abc/editions/def/releases"
        ));
        assert!(!path_allowed(
            c,
            &Method::POST,
            "/movies/abc/editions/def/grab"
        ));
        assert!(!path_allowed(c, &Method::POST, "/settings/profiles"));
        assert!(!path_allowed(c, &Method::GET, "/downloads"));
        assert!(!path_allowed(c, &Method::GET, "/members"));
        assert!(!path_allowed(c, &Method::GET, "/pair/app"));

        // A read-only adult gets none of the extra surface, but still sees and
        // plays everything.
        let m = Role::Member;
        assert!(!path_allowed(m, &Method::POST, "/movies"));
        assert!(!path_allowed(
            m,
            &Method::POST,
            "/movies/abc/editions/def/acquire"
        ));
        assert!(!path_allowed(m, &Method::GET, "/wanted"));
        assert!(path_allowed(m, &Method::GET, "/movies"));
        assert!(path_allowed(
            m,
            &Method::GET,
            "/movies/abc/editions/def/video"
        ));
        // Arm order matters here: `["movies", _]` matches "/movies/lookup" too,
        // so without the specific arm first a read-only member reached the
        // catalog search. Caught live, not by the first version of this test.
        assert!(!path_allowed(m, &Method::GET, "/movies/lookup"));
        assert!(!path_allowed(m, &Method::GET, "/series/lookup"));
        assert!(!path_allowed(m, &Method::GET, "/books/search"));

        // And a kid gets nothing extra at all.
        let k = Role::Kid;
        assert!(!path_allowed(k, &Method::GET, "/movies/lookup"));
        assert!(!path_allowed(k, &Method::POST, "/movies"));
        assert!(!path_allowed(k, &Method::GET, "/wanted"));
        assert!(
            !path_allowed(k, &Method::GET, "/authors"),
            "rollups would name hidden books"
        );
    }

    #[test]
    fn non_admins_reach_only_the_read_play_surface() {
        use axum::http::Method;
        let get = Method::GET;
        let post = Method::POST;
        for role in [Role::Member, Role::Kid] {
            assert!(path_allowed(role, &get, "/movies"));
            assert!(path_allowed(role, &get, "/movies/abc"));
            assert!(path_allowed(role, &get, "/movies/abc/editions/def/video"));
            assert!(path_allowed(role, &get, "/series/abc/episodes/def/video"));
            assert!(path_allowed(role, &get, "/books/abc/files/def/audio"));
            assert!(path_allowed(role, &get, "/me"));
            assert!(
                !path_allowed(role, &post, "/movies"),
                "adding is the operator's"
            );
            assert!(!path_allowed(role, &get, "/wanted"));
            assert!(!path_allowed(role, &get, "/downloads"));
            assert!(!path_allowed(
                role,
                &get,
                "/movies/abc/editions/def/releases"
            ));
            assert!(!path_allowed(role, &post, "/movies/abc/editions/def/grab"));
            assert!(!path_allowed(role, &get, "/settings/profiles"));
            assert!(!path_allowed(role, &get, "/pair/app"));
        }
        assert!(path_allowed(Role::Member, &get, "/authors"));
        assert!(
            !path_allowed(Role::Kid, &get, "/authors"),
            "rollups would name hidden books"
        );
        assert!(path_allowed(Role::Admin, &post, "/movies"));
    }

    #[test]
    fn every_role_can_read_the_domain_list() {
        // The sidebar builds its library links from `/domains`. Gating it to
        // the admin left a read-only member with no Movies, TV or Audiobooks
        // links at all — in the role that exists purely to browse and play
        // (found by the per-role UI sweep, SKADI-T-0639).
        use axum::http::Method;
        for role in [Role::Admin, Role::Contributor, Role::Member, Role::Kid] {
            assert!(
                path_allowed(role, &Method::GET, "/domains"),
                "{role:?} must be able to see which media kinds exist"
            );
        }
        // Still not writable by anyone but the admin.
        assert!(!path_allowed(Role::Contributor, &Method::POST, "/domains"));
        assert!(!path_allowed(Role::Member, &Method::POST, "/domains"));
    }

    #[test]
    fn a_contributor_may_upload_and_a_read_only_member_may_not() {
        use axum::http::Method;
        let (post, put, del, get) = (Method::POST, Method::PUT, Method::DELETE, Method::GET);
        for (m, p) in [
            (&post, "/uploads"),
            (&get, "/uploads"),
            (&get, "/uploads/abc"),
            (&put, "/uploads/abc/chunk"),
            (&post, "/uploads/abc/complete"),
            (&del, "/uploads/abc"),
        ] {
            assert!(
                path_allowed(Role::Contributor, m, p),
                "a contributor should reach {m} {p}"
            );
            assert!(path_allowed(Role::Admin, m, p), "{m} {p}");
            // Uploading writes into the library. Looking is all a member and a
            // kid may do.
            assert!(
                !path_allowed(Role::Member, m, p),
                "a read-only member must not reach {m} {p}"
            );
            assert!(
                !path_allowed(Role::Kid, m, p),
                "a kid must not reach {m} {p}"
            );
        }
    }

    #[test]
    fn the_upload_wildcard_arm_does_not_swallow_the_named_ones() {
        // Arm order is load-bearing: `["uploads", _]` matches
        // `/uploads/{id}/chunk` as though "chunk" were an id if it is listed
        // first. That exact shadowing let a read-only member reach catalog
        // search in SKADI-T-0625, so it is asserted rather than assumed.
        use axum::http::Method;
        assert!(path_allowed(
            Role::Contributor,
            &Method::PUT,
            "/uploads/9f8c/chunk"
        ));
        assert!(path_allowed(
            Role::Contributor,
            &Method::POST,
            "/uploads/9f8c/complete"
        ));
        // And nothing deeper than the routes that exist is admitted.
        assert!(!path_allowed(
            Role::Contributor,
            &Method::PUT,
            "/uploads/9f8c/chunk/extra"
        ));
        assert!(!path_allowed(
            Role::Contributor,
            &Method::POST,
            "/uploads/9f8c/complete/extra"
        ));
    }

    #[test]
    fn uploading_does_not_widen_anything_else_for_a_contributor() {
        // The new arms are POST/PUT/DELETE and sit before the read-only rule,
        // so the risk is that they let some *other* write through.
        use axum::http::Method;
        for p in [
            "/settings/profiles",
            "/members",
            "/config",
            "/downloads",
            "/library-import/commit",
        ] {
            assert!(
                !path_allowed(Role::Contributor, &Method::POST, p),
                "a contributor must not POST {p}"
            );
            assert!(
                !path_allowed(Role::Contributor, &Method::DELETE, p),
                "a contributor must not DELETE {p}"
            );
        }
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
