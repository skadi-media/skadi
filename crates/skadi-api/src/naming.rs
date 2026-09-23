//! Naming configuration + live preview (SKADI-T-0299).
//!
//! Reads/writes the operator's `naming.*` rename templates on the config plane, and
//! renders a sample library path so the UI can preview the layout as the operator
//! types. Rendering uses [`skadi_naming`] directly — the domain crates sit *above*
//! skadi-api, so we can't call their path builders; instead we feed representative
//! per-domain sample tokens. The default strings here therefore must stay **in sync**
//! with each domain's `naming.rs` constants.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

// Defaults come from `skadi-naming`, the leaf crate every consumer already
// depends on (SKADI-T-0427). They used to be copied here under a "KEEP IN SYNC"
// comment — because the domain crates depend on skadi-api and not the reverse —
// and the copy had drifted: the anime and daily templates `skadi-tv` reads were
// missing entirely, so an operator could not see or set them through the API.
use skadi_naming::defaults;

/// Routes for the naming-config + preview surface, merged into the authed router.
pub fn naming_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/naming/settings", get(get_settings).put(set_settings))
        .route("/naming/preview", post(preview))
}

/// The per-domain rename templates + the shared whitespace character.
#[derive(Serialize, Deserialize)]
struct NamingSettings {
    movie_folder: String,
    movie_file: String,
    series_folder: String,
    series_file: String,
    /// Anime episodes are numbered absolutely rather than by season; `skadi-tv`
    /// reads this key and it had no API surface before SKADI-T-0427.
    ///
    /// `default` so a client written against the older seven-field body still
    /// PUTs successfully. The write loop skips empty values, so an omitted field
    /// leaves the stored template alone rather than blanking it — which matters
    /// because blanking would silently reset an operator's customised template.
    #[serde(default)]
    series_anime_file: String,
    /// Daily shows are keyed by air date; likewise previously unreachable.
    #[serde(default)]
    series_daily_file: String,
    audiobook_folder: String,
    audiobook_file: String,
    /// Whitespace replacement (one char), e.g. `_`.
    space: String,
}

/// `GET /naming/settings` — current templates from the config plane (defaults if unset).
async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let view = crate::load_config_view(store).await?;
    let g = |key: &str, default: &str| {
        view.get_string(key)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    Ok(Json(NamingSettings {
        movie_folder: g("naming.movie_folder", defaults::MOVIE_FOLDER),
        movie_file: g("naming.movie_file", defaults::MOVIE_FILE),
        series_folder: g("naming.series_folder", defaults::SERIES_FOLDER),
        series_file: g("naming.series_file", defaults::SERIES_FILE),
        series_anime_file: g("naming.series_anime_file", defaults::SERIES_ANIME_FILE),
        series_daily_file: g("naming.series_daily_file", defaults::SERIES_DAILY_FILE),
        audiobook_folder: g("naming.audiobook_folder", defaults::AUDIOBOOK_FOLDER),
        audiobook_file: g("naming.audiobook_file", defaults::AUDIOBOOK_FILE),
        space: g("naming.space", &defaults::SPACE.to_string()),
    }))
}

/// `PUT /naming/settings` — persist templates (`source = runtime`). Empty fields are
/// skipped (so a blank in the UI falls back to the built-in default, not "").
async fn set_settings(
    State(state): State<Arc<AppState>>,
    Json(s): Json<NamingSettings>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    use skadi_store::{ConfigRepo, ConfigSource};
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(skadi_core::AppError::Internal("no store configured".into())))?;
    let pairs: [(&str, String); 9] = [
        ("naming.movie_folder", s.movie_folder),
        ("naming.movie_file", s.movie_file),
        ("naming.series_folder", s.series_folder),
        ("naming.series_file", s.series_file),
        ("naming.series_anime_file", s.series_anime_file),
        ("naming.series_daily_file", s.series_daily_file),
        ("naming.audiobook_folder", s.audiobook_folder),
        ("naming.audiobook_file", s.audiobook_file),
        ("naming.space", s.space),
    ];
    for (k, v) in pairs {
        let v = v.trim();
        if v.is_empty() {
            // Blank means **reset to the built-in default** (SKADI-T-0524).
            // It used to mean "skip", so once a template had been customised
            // there was no way back to the default through the UI: clearing the
            // box appeared to work and silently changed nothing. Deleting the row
            // lets `get_settings`' existing fallback supply the default, so there
            // is one definition of "the default" rather than two.
            store.delete_config(k).await?;
        } else {
            skadi_config::validate_write(k, v)
                .map_err(|e| ApiError(skadi_core::AppError::field(k, e.to_string())))?;
            store.set_config(k, v, ConfigSource::Runtime).await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `POST /naming/preview`: a domain + the (in-progress) templates to render.
#[derive(Deserialize)]
struct PreviewReq {
    domain: String,
    folder: String,
    file: String,
    #[serde(default)]
    space: String,
}

#[derive(Serialize)]
struct PreviewResp {
    path: String,
}

/// `POST /naming/preview` — render a representative library path for `domain` using the
/// supplied templates, so the UI shows the layout live as the operator edits.
async fn preview(Json(req): Json<PreviewReq>) -> std::result::Result<impl IntoResponse, ApiError> {
    let space = req.space.chars().next().unwrap_or('_');
    let (tokens, ext) = sample_tokens(&req.domain);
    let path = skadi_naming::render_library_path(
        std::path::Path::new(""),
        &req.folder,
        &req.file,
        &tokens,
        ext,
        space,
    );
    Ok(Json(PreviewResp {
        path: path.to_string_lossy().to_string(),
    }))
}

/// Representative tokens + file extension for previewing each domain's layout.
fn sample_tokens(domain: &str) -> (HashMap<&'static str, String>, &'static str) {
    let mut t: HashMap<&'static str, String> = HashMap::new();
    let mut put = |k: &'static str, v: &str| {
        t.insert(k, v.to_string());
    };
    match domain {
        "tv" => {
            put("SeriesTitle", "The Wire");
            put("SeriesTitleKebab", "the-wire");
            put("Year", "2002");
            put("TmdbTag", "{tmdb-1438}");
            put("ImdbTag", "{imdb-tt0306414}");
            put("SeasonFolder", "Season 03");
            put("Episode", "S03E05");
            put("EpisodeTitlePart", " - straight-and-true");
            put("QualityPart", " [1080p]");
            put("Absolute", "028");
            put("AirDate", "2024-03-01");
            (t, ".mkv")
        }
        "audiobook" => {
            put("Author", "Brandon Sanderson");
            put("AuthorKebab", "brandon-sanderson");
            put("Series", "Mistborn");
            put("SeriesKebab", "mistborn");
            put("Position", "1 - ");
            put("Title", "The Final Empire");
            put("TitleKebab", "the-final-empire");
            put("AsinTag", "{asin-B002V1A0WE}");
            (t, ".m4b")
        }
        _ => {
            put("Title", "Big Buck Bunny");
            put("TitleKebab", "big-buck-bunny");
            put("Year", "2008");
            put("TmdbTag", "{tmdb-10378}");
            put("ImdbTag", "{imdb-tt1254207}");
            put("EditionFolder", "Theatrical");
            put("EditionKebab", "theatrical");
            put("EditionSuffix", "");
            (t, ".mkv")
        }
    }
}
