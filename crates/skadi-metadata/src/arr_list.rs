//! Import lists sourced from another *arr instance (SKADI-T-0564).
//!
//! The migration path: point skadi at an existing Radarr or Sonarr and it adopts
//! that library as a list, so someone moving over does not re-add hundreds of
//! items by hand.
//!
//! Both expose the same shape — `GET /api/v3/{movie,series}` with an
//! `X-Api-Key` header, returning objects carrying `tmdbId` / `tvdbId`. One
//! provider covers both because they differ only in the path and which id field
//! to read; splitting them would duplicate the auth, the error handling and the
//! paging to save one `match`.

use async_trait::async_trait;
use serde::Deserialize;

use skadi_core::{AppError, Result};
use skadi_http::HttpClient;

use crate::import_list::{ImportListProvider, ListItem};

/// Which *arr, and therefore which endpoint and id namespace.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Flavour {
    Radarr,
    Sonarr,
}

impl Flavour {
    fn path(self) -> &'static str {
        match self {
            Self::Radarr => "/api/v3/movie",
            Self::Sonarr => "/api/v3/series",
        }
    }
    fn id_kind(self) -> &'static str {
        match self {
            Self::Radarr => "tmdb",
            Self::Sonarr => "tvdb",
        }
    }
}

/// Another *arr instance as a list source.
pub struct ArrInstanceProvider {
    flavour: Flavour,
    http: HttpClient,
    kind: &'static str,
}

impl ArrInstanceProvider {
    /// Radarr's movie library.
    #[must_use]
    pub fn radarr(http: HttpClient) -> Self {
        Self {
            flavour: Flavour::Radarr,
            http,
            kind: "radarr",
        }
    }

    /// Sonarr's series library.
    #[must_use]
    pub fn sonarr(http: HttpClient) -> Self {
        Self {
            flavour: Flavour::Sonarr,
            http,
            kind: "sonarr",
        }
    }

    /// `base_url` and `api_key` from the list's settings.
    ///
    /// Both are required and neither has a sensible default: a missing key gives
    /// a 401 that reads like the remote being broken, which is a bad error for a
    /// setting the operator simply has not filled in.
    fn config(settings: &serde_json::Value) -> Result<(String, String)> {
        let get = |k: &str| {
            settings
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let base = get("base_url")
            .ok_or_else(|| AppError::field("base_url", "an *arr import list needs a `base_url`"))?;
        let key = get("api_key")
            .ok_or_else(|| AppError::field("api_key", "an *arr import list needs an `api_key`"))?;
        Ok((base.trim_end_matches('/').to_string(), key))
    }
}

/// One row of `/api/v3/movie` or `/api/v3/series`.
///
/// Deliberately a tiny subset. The real payload carries files, quality profiles,
/// statistics and history; taking only the ids means a schema change on their
/// side cannot break the sync unless it moves the one field that matters.
#[derive(Deserialize)]
struct ArrItem {
    #[serde(default)]
    #[serde(rename = "tmdbId")]
    tmdb_id: Option<i64>,
    #[serde(default)]
    #[serde(rename = "tvdbId")]
    tvdb_id: Option<i64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    year: Option<i64>,
}

#[async_trait]
impl ImportListProvider for ArrInstanceProvider {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn fetch(&self, settings: &serde_json::Value) -> Result<Vec<ListItem>> {
        let (base, key) = Self::config(settings)?;
        let url = format!("{base}{}", self.flavour.path());
        let resp = self
            .http
            .send_idempotent(|c| c.get(&url).header("X-Api-Key", key.as_str()))
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Name the instance and the status. "list sync failed" for a wrong
            // API key sends the operator to skadi's logs rather than to the
            // setting that is wrong.
            return Err(AppError::Network(format!(
                "{} at {base} answered HTTP {status} — check `api_key` and that the \
                 instance is reachable",
                self.kind
            )));
        }

        let body: Vec<ArrItem> = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding {} library: {e}", self.kind)))?;

        let want_tmdb = self.flavour == Flavour::Radarr;
        Ok(body
            .into_iter()
            .filter_map(|i| {
                // An *arr row with no external id is one it never matched. It
                // cannot be added here either, so it is skipped rather than
                // failing the whole sync for the rest of the library.
                let id = if want_tmdb { i.tmdb_id } else { i.tvdb_id }.filter(|n| *n > 0)?;
                Some(ListItem {
                    id_kind: self.flavour.id_kind().to_string(),
                    external_id: id.to_string(),
                    title: i.title,
                    year: i.year.and_then(|y| u16::try_from(y).ok()),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_and_api_key_are_both_required() {
        let ok = serde_json::json!({"base_url": "http://radarr:7878/", "api_key": "k"});
        let (base, key) = ArrInstanceProvider::config(&ok).unwrap();
        // The trailing slash is trimmed, so the joined path cannot become `//`.
        assert_eq!(base, "http://radarr:7878");
        assert_eq!(key, "k");

        // A missing key must name the field. Left to the request, it becomes a
        // 401 that reads like the remote is broken.
        for bad in [
            serde_json::json!({"base_url": "http://x"}),
            serde_json::json!({"api_key": "k"}),
            serde_json::json!({"base_url": "  ", "api_key": "k"}),
            serde_json::json!({}),
        ] {
            assert!(ArrInstanceProvider::config(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn each_flavour_reads_its_own_id_field() {
        assert_eq!(Flavour::Radarr.id_kind(), "tmdb");
        assert_eq!(Flavour::Sonarr.id_kind(), "tvdb");
        assert_eq!(Flavour::Radarr.path(), "/api/v3/movie");
        assert_eq!(Flavour::Sonarr.path(), "/api/v3/series");
    }

    #[test]
    fn rows_without_a_usable_id_are_skipped_not_fatal() {
        // An *arr row that never matched has no external id. It cannot be added
        // here either, and failing the sync would cost the rest of the library.
        let rows: Vec<ArrItem> = serde_json::from_str(
            r#"[{"tmdbId": 603, "title": "The Matrix", "year": 1999},
                {"tmdbId": 0, "title": "Unmatched"},
                {"title": "No ids at all"}]"#,
        )
        .unwrap();
        let kept: Vec<_> = rows
            .into_iter()
            .filter_map(|i| i.tmdb_id.filter(|n| *n > 0).map(|n| (n, i.title)))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0, 603);
    }

    #[test]
    fn a_sonarr_row_yields_a_tvdb_item() {
        let row: ArrItem =
            serde_json::from_str(r#"{"tvdbId": 121361, "title": "Game of Thrones", "year": 2011}"#)
                .unwrap();
        assert_eq!(row.tvdb_id, Some(121361));
        assert_eq!(row.tmdb_id, None);
        assert_eq!(row.year, Some(2011));
    }
}
