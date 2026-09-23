//! Import-list providers (SKADI-T-0511, component C19).
//!
//! A provider turns a configured [`ImportList`](skadi_store::ImportList) into a
//! flat set of external ids. It deliberately does **not** add anything: the sync
//! engine owns exclusions, deduplication and the per-domain add, so a new
//! provider is a fetch-and-parse and nothing else.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use skadi_core::{AppError, Result};
use skadi_http::HttpClient;

/// One item a list offers.
///
/// External ids only. A provider that returned full metadata would duplicate the
/// metadata providers and give two paths for the same fact to disagree; the sync
/// engine hands the id to the domain's normal add flow, which is the same path
/// the "add movie" button uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItem {
    /// Which external namespace `external_id` belongs to — `tmdb`, `tvdb`,
    /// `imdb`, `asin`.
    pub id_kind: String,
    pub external_id: String,
    /// For logging and the exclusion UI; not authoritative.
    pub title: Option<String>,
    pub year: Option<u16>,
}

/// A source of list items.
#[async_trait]
pub trait ImportListProvider: Send + Sync {
    /// The provider slug this handles, matching `ImportList.kind`.
    fn kind(&self) -> &'static str;

    /// Everything the list currently offers.
    ///
    /// Returns the **whole** list, not a delta. The engine diffs against the
    /// library and the exclusions, so a provider that tried to track what it had
    /// already returned would need its own state and would silently miss items
    /// whenever that state and the library disagreed.
    async fn fetch(&self, settings: &serde_json::Value) -> Result<Vec<ListItem>>;
}

/// TMDB collections and lists.
///
/// One provider for both because they differ only in the path and the response
/// envelope; splitting them would duplicate the auth, the paging and the parsing
/// to save one `match`.
pub struct TmdbListProvider {
    base_url: String,
    api_key: String,
    http: HttpClient,
    kind: &'static str,
}

/// TMDB pages lists at 20 items; this bounds how many pages a single sync walks.
///
/// A cap rather than "until exhausted": a mis-configured list id can return an
/// unbounded pager, and a sync that never finishes blocks every other list
/// behind it. 50 pages is 1,000 items, well past any hand-curated list.
const MAX_PAGES: u32 = 50;

impl TmdbListProvider {
    /// A provider for TMDB **collections** (`/collection/{id}`) — the "all the
    /// Bond films" case, which is not paged.
    #[must_use]
    pub fn collections(api_key: impl Into<String>, http: HttpClient) -> Self {
        Self {
            base_url: "https://api.themoviedb.org/3".into(),
            api_key: api_key.into(),
            http,
            kind: "tmdb_collection",
        }
    }

    /// A provider for TMDB **lists** (`/list/{id}`), which are paged.
    #[must_use]
    pub fn lists(api_key: impl Into<String>, http: HttpClient) -> Self {
        Self {
            base_url: "https://api.themoviedb.org/3".into(),
            api_key: api_key.into(),
            http,
            kind: "tmdb_list",
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// The `list_id` a list's settings must carry.
    fn list_id(settings: &serde_json::Value) -> Result<String> {
        settings
            .get("list_id")
            .and_then(|v| match v {
                // Accept a number as well as a string: TMDB ids are numeric, and
                // a settings blob hand-written as JSON will very often carry
                // `"list_id": 12345` — failing that is a bad error for a
                // difference that does not matter.
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| AppError::field("list_id", "import list settings need a `list_id`"))
    }
}

#[derive(Deserialize)]
struct TmdbCollection {
    #[serde(default)]
    parts: Vec<TmdbItem>,
}

#[derive(Deserialize)]
struct TmdbListPage {
    #[serde(default)]
    items: Vec<TmdbItem>,
    #[serde(default)]
    total_pages: Option<u32>,
}

#[derive(Deserialize)]
struct TmdbItem {
    id: i64,
    #[serde(default)]
    title: Option<String>,
    /// TV entries in a mixed list carry `name` instead of `title`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
}

impl From<TmdbItem> for ListItem {
    fn from(i: TmdbItem) -> Self {
        let year = i
            .release_date
            .as_deref()
            .or(i.first_air_date.as_deref())
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok());
        ListItem {
            id_kind: "tmdb".into(),
            external_id: i.id.to_string(),
            title: i.title.or(i.name),
            year,
        }
    }
}

#[async_trait]
impl ImportListProvider for TmdbListProvider {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn fetch(&self, settings: &serde_json::Value) -> Result<Vec<ListItem>> {
        let id = Self::list_id(settings)?;
        if self.kind == "tmdb_collection" {
            let url = self.url(&format!("/collection/{id}"));
            let api_key = self.api_key.clone();
            let resp = self
                .http
                .send_idempotent(|c| c.get(&url).query(&[("api_key", api_key.as_str())]))
                .await?;
            let body: TmdbCollection = resp
                .json()
                .await
                .map_err(|e| AppError::Network(format!("decoding TMDB collection: {e}")))?;
            return Ok(body.parts.into_iter().map(ListItem::from).collect());
        }

        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let url = self.url(&format!("/list/{id}"));
            let api_key = self.api_key.clone();
            let page_s = page.to_string();
            let resp = self
                .http
                .send_idempotent(|c| {
                    c.get(&url)
                        .query(&[("api_key", api_key.as_str()), ("page", page_s.as_str())])
                })
                .await?;
            let body: TmdbListPage = resp
                .json()
                .await
                .map_err(|e| AppError::Network(format!("decoding TMDB list: {e}")))?;
            let empty = body.items.is_empty();
            out.extend(body.items.into_iter().map(ListItem::from));
            let last = body.total_pages.unwrap_or(1);
            // Stop on an empty page as well as on the reported total: a list
            // whose `total_pages` is wrong (or absent) would otherwise spin to
            // MAX_PAGES fetching nothing.
            if empty || page >= last || page >= MAX_PAGES {
                if page >= MAX_PAGES && page < last {
                    tracing::warn!(
                        list_id = %id,
                        total_pages = last,
                        "TMDB list truncated at the page cap"
                    );
                }
                break;
            }
            page += 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_id_may_be_a_number_or_a_string() {
        assert_eq!(
            TmdbListProvider::list_id(&serde_json::json!({"list_id": "8"})).unwrap(),
            "8"
        );
        // The case a hand-written settings blob actually produces.
        assert_eq!(
            TmdbListProvider::list_id(&serde_json::json!({"list_id": 8})).unwrap(),
            "8"
        );
        assert!(TmdbListProvider::list_id(&serde_json::json!({})).is_err());
        // Present but useless is an error, not an empty fetch that looks like an
        // empty list.
        assert!(TmdbListProvider::list_id(&serde_json::json!({"list_id": "  "})).is_err());
    }

    #[test]
    fn a_tv_entry_in_a_mixed_list_still_yields_a_title_and_year() {
        let item: ListItem = serde_json::from_str::<TmdbItem>(
            r#"{"id": 1399, "name": "Game of Thrones", "first_air_date": "2011-04-17"}"#,
        )
        .unwrap()
        .into();
        assert_eq!(item.external_id, "1399");
        assert_eq!(item.title.as_deref(), Some("Game of Thrones"));
        assert_eq!(item.year, Some(2011));
        assert_eq!(item.id_kind, "tmdb");
    }

    #[test]
    fn a_movie_entry_uses_title_and_release_date() {
        let item: ListItem = serde_json::from_str::<TmdbItem>(
            r#"{"id": 603, "title": "The Matrix", "release_date": "1999-03-30"}"#,
        )
        .unwrap()
        .into();
        assert_eq!(item.title.as_deref(), Some("The Matrix"));
        assert_eq!(item.year, Some(1999));
    }

    #[test]
    fn a_missing_or_malformed_date_is_not_an_error() {
        // TMDB returns "" for unreleased items; a list should still offer them.
        let item: ListItem =
            serde_json::from_str::<TmdbItem>(r#"{"id": 1, "title": "X", "release_date": ""}"#)
                .unwrap()
                .into();
        assert_eq!(item.year, None);
        assert_eq!(item.external_id, "1");
    }
}
