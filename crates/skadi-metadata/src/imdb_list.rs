//! IMDb lists as an import-list provider (SKADI-T-0733).
//!
//! IMDb has no list API. A public list page is plain HTML, but a direct fetch
//! of imdb.com answers HTTP 202 with an empty body: a bot challenge. So the page
//! is fetched through FlareSolverr, the same solver the native trackers use
//! (`flaresolverr_url`), and the `tt…` ids are read out of the rendered page.
//!
//! The ids are in the `imdb` namespace; the movies add hook resolves each one to
//! a TMDB id before it adds the movie.

use std::collections::HashSet;

use async_trait::async_trait;

use skadi_core::{AppError, Result};
use skadi_indexers::flaresolverr::FlareSolverr;

use crate::import_list::{ImportListProvider, ListItem};

const IMDB_BASE_URL: &str = "https://www.imdb.com";

/// How many list pages one sync walks.
///
/// IMDb shows up to 250 titles a page, so 20 pages is 5,000 titles. A cap for
/// the same reason the TMDB provider has one: a solve takes seconds, and a sync
/// that never ends blocks every other list behind it.
const MAX_PAGES: u32 = 20;

/// IMDb lists, fetched through FlareSolverr.
pub struct ImdbListProvider {
    solver: FlareSolverr,
    base_url: String,
}

impl ImdbListProvider {
    /// A provider that solves through the FlareSolverr at `flaresolverr_url`
    /// (the `flaresolverr_url` config key).
    #[must_use]
    pub fn new(flaresolverr_url: &str) -> Self {
        Self {
            solver: FlareSolverr::new(flaresolverr_url),
            base_url: IMDB_BASE_URL.into(),
        }
    }

    /// Override the IMDb base URL (tests).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn page_url(&self, list_id: &str, page: u32) -> String {
        format!(
            "{}/list/{list_id}/?page={page}",
            self.base_url.trim_end_matches('/')
        )
    }

    /// The `ls…` id from a list's settings. A pasted list address works too:
    /// the id is the part an operator copies, and the address is what they have.
    fn list_id(settings: &serde_json::Value) -> Result<String> {
        let raw = settings
            .get("list_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        find_list_id(raw).ok_or_else(|| {
            AppError::field(
                "list_id",
                "an IMDb list id looks like `ls012345678` (watchlists are not lists)",
            )
        })
    }
}

/// The first `ls` followed by digits in `s`.
fn find_list_id(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while let Some(off) = s[i..].find("ls") {
        let start = i + off;
        let digits = bytes[start + 2..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0 {
            return Some(s[start..start + 2 + digits].to_string());
        }
        i = start + 2;
    }
    None
}

#[async_trait]
impl ImportListProvider for ImdbListProvider {
    fn kind(&self) -> &'static str {
        "imdb_list"
    }

    async fn fetch(&self, settings: &serde_json::Value) -> Result<Vec<ListItem>> {
        let id = Self::list_id(settings)?;
        let mut out: Vec<ListItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for page in 1..=MAX_PAGES {
            let url = self.page_url(&id, page);
            let solved = self
                .solver
                .solve(&url, None)
                .await
                .map_err(|e| AppError::Network(format!("IMDb list {id}: {e}")))?;
            if solved.status != 200 {
                if page == 1 {
                    return Err(match solved.status {
                        404 => AppError::NotFound(format!(
                            "IMDb list {id} does not exist or is private"
                        )),
                        s => AppError::Network(format!("IMDb list {id}: HTTP {s}")),
                    });
                }
                // A later page that fails ends the walk; what came before is
                // still the list.
                tracing::warn!(list_id = %id, page, status = solved.status, "IMDb list page failed");
                break;
            }
            let parsed = parse_list_page(&solved.body);
            let before = out.len();
            for item in parsed.items {
                if seen.insert(item.external_id.clone()) {
                    out.push(item);
                }
            }
            if page == 1 && out.is_empty() {
                // An empty list is rare; a page whose layout we no longer read
                // is not. Say so rather than sync nothing in silence.
                return Err(AppError::Network(format!(
                    "IMDb list {id}: no titles found on the page"
                )));
            }
            // Stop when a page adds nothing (IMDb answers a page past the end
            // with the last one again) or says there is no next page.
            if out.len() == before || parsed.has_next_page == Some(false) {
                break;
            }
            if page == MAX_PAGES {
                tracing::warn!(list_id = %id, "IMDb list truncated at the page cap");
            }
        }
        Ok(out)
    }
}

/// What one list page holds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImdbListPage {
    pub items: Vec<ListItem>,
    /// `Some(false)` when the page says it is the last one.
    pub has_next_page: Option<bool>,
}

/// Read the titles out of a rendered IMDb list page.
///
/// The page carries its data twice: as the JSON of the `__NEXT_DATA__` script,
/// with titles and years, and as `/title/tt…/` links. The JSON is read first;
/// the links are the fallback, so a change to the JSON shape still yields the
/// ids.
#[must_use]
pub fn parse_list_page(html: &str) -> ImdbListPage {
    if let Some(page) = next_data(html).and_then(|v| from_next_data(&v)) {
        return page;
    }
    ImdbListPage {
        items: from_links(html),
        has_next_page: None,
    }
}

fn next_data(html: &str) -> Option<serde_json::Value> {
    let marker = html.find("id=\"__NEXT_DATA__\"")?;
    let open = marker + html[marker..].find('>')? + 1;
    let close = open + html[open..].find("</script>")?;
    serde_json::from_str(&html[open..close]).ok()
}

fn from_next_data(v: &serde_json::Value) -> Option<ImdbListPage> {
    // The list's own titles sit under `titleListItemSearch`; elsewhere on the
    // page are other titles (recommendations), so walk only that part when it
    // is there.
    let scope = find_key(v, "titleListItemSearch").unwrap_or(v);
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    collect_titles(scope, &mut items, &mut seen);
    if items.is_empty() {
        return None;
    }
    let has_next_page = scope
        .get("pageInfo")
        .and_then(|p| p.get("hasNextPage"))
        .and_then(serde_json::Value::as_bool);
    Some(ImdbListPage {
        items,
        has_next_page,
    })
}

fn find_key<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|c| find_key(c, key))),
        serde_json::Value::Array(arr) => arr.iter().find_map(|c| find_key(c, key)),
        _ => None,
    }
}

fn collect_titles(v: &serde_json::Value, out: &mut Vec<ListItem>, seen: &mut HashSet<String>) {
    match v {
        serde_json::Value::Object(map) => {
            let id = map.get("id").and_then(serde_json::Value::as_str);
            let title = map
                .get("titleText")
                .and_then(|t| t.get("text"))
                .and_then(serde_json::Value::as_str);
            if let (Some(id), Some(title)) = (id, title) {
                if is_title_id(id) && seen.insert(id.to_string()) {
                    let year = map
                        .get("releaseYear")
                        .and_then(|y| y.get("year"))
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|y| u16::try_from(y).ok());
                    out.push(ListItem {
                        id_kind: "imdb".into(),
                        external_id: id.to_string(),
                        title: Some(title.to_string()),
                        year,
                    });
                }
                return;
            }
            for c in map.values() {
                collect_titles(c, out, seen);
            }
        }
        serde_json::Value::Array(arr) => {
            for c in arr {
                collect_titles(c, out, seen);
            }
        }
        _ => {}
    }
}

fn is_title_id(s: &str) -> bool {
    s.len() > 2 && s.starts_with("tt") && s[2..].bytes().all(|b| b.is_ascii_digit())
}

/// The ids of the `/title/tt…` links, in page order, each with the first
/// non-empty link text as its title.
fn from_links(html: &str) -> Vec<ListItem> {
    let mut out: Vec<ListItem> = Vec::new();
    let mut i = 0;
    while let Some(off) = html[i..].find("/title/tt") {
        let start = i + off + "/title/".len();
        let digits = html[start + 2..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let id_end = start + 2 + digits;
        i = id_end;
        if digits == 0 {
            continue;
        }
        let id = &html[start..id_end];
        let text = anchor_text(&html[id_end..]);
        match out.iter_mut().find(|it| it.external_id == id) {
            Some(existing) => {
                if existing.title.is_none() {
                    existing.title = text;
                }
            }
            None => out.push(ListItem {
                id_kind: "imdb".into(),
                external_id: id.to_string(),
                title: text,
                year: None,
            }),
        }
    }
    out
}

/// The text of the `<a>` whose `href` ends just before `rest`, without tags,
/// the list's "12. " number, or entities.
fn anchor_text(rest: &str) -> Option<String> {
    let open = rest.find('>')?;
    // The href must still be inside its tag: a `<` first means this was text.
    if rest[..open].contains('<') {
        return None;
    }
    let body = &rest[open + 1..];
    let close = body.find("</a>")?;
    let mut text = String::new();
    let mut in_tag = false;
    for c in body[..close].chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = decode_entities(text.trim());
    let text = strip_number(&text).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn strip_number(s: &str) -> &str {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && s[digits..].starts_with(". ") {
        &s[digits + 2..]
    } else {
        s
    }
}

fn decode_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A list page shaped like IMDb's: the `__NEXT_DATA__` JSON with the list's
    /// titles, plus a recommended title outside the list that must not be read.
    fn next_data_page(items: &[(&str, &str, u16)], has_next: bool) -> String {
        let edges: Vec<serde_json::Value> = items
            .iter()
            .map(|(id, title, year)| {
                serde_json::json!({
                    "listItem": {
                        "id": id,
                        "titleText": {"text": title},
                        "releaseYear": {"year": year}
                    }
                })
            })
            .collect();
        let data = serde_json::json!({
            "props": {"pageProps": {
                "mainColumnData": {"list": {"titleListItemSearch": {
                    "edges": edges,
                    "total": items.len(),
                    "pageInfo": {"hasNextPage": has_next}
                }}},
                "moreLikeThis": [{"id": "tt9999999", "titleText": {"text": "Not On The List"}}]
            }}
        });
        format!(
            "<html><head><title>Sci-fi - IMDb</title></head><body>\
             <script id=\"__NEXT_DATA__\" type=\"application/json\">{data}</script>\
             </body></html>"
        )
    }

    const LINKS_PAGE: &str = r#"<html><body><ul>
        <li><a href="/title/tt0133093/?ref_=ls_i_1"><img alt="poster"></a>
            <a href="/title/tt0133093/?ref_=ls_t_1" class="ipc-title-link-wrapper"><h3 class="ipc-title__text">1. The Matrix</h3></a>
            <span class="dli-title-metadata-item">1999</span></li>
        <li><a href="/title/tt0088763/?ref_=ls_t_2"><h3 class="ipc-title__text">2. Back to the Future</h3></a></li>
        <li><a href="/title/tt0110912/"><h3>3. Pulp Fiction &amp; Friends</h3></a></li>
        <li><a href="/title/ttabc/">not an id</a></li>
    </ul></body></html>"#;

    #[test]
    fn the_next_data_json_gives_ids_titles_and_years() {
        let html = next_data_page(
            &[
                ("tt0133093", "The Matrix", 1999),
                ("tt0088763", "Back to the Future", 1985),
            ],
            false,
        );
        let page = parse_list_page(&html);
        assert_eq!(page.has_next_page, Some(false));
        assert_eq!(page.items.len(), 2, "the recommendation is not on the list");
        assert_eq!(
            page.items[0],
            ListItem {
                id_kind: "imdb".into(),
                external_id: "tt0133093".into(),
                title: Some("The Matrix".into()),
                year: Some(1999),
            }
        );
        assert_eq!(page.items[1].year, Some(1985));
    }

    #[test]
    fn the_links_are_the_fallback() {
        let page = parse_list_page(LINKS_PAGE);
        let ids: Vec<&str> = page.items.iter().map(|i| i.external_id.as_str()).collect();
        assert_eq!(ids, ["tt0133093", "tt0088763", "tt0110912"]);
        // The poster link has no text; the title link after it gives the title.
        assert_eq!(page.items[0].title.as_deref(), Some("The Matrix"));
        assert_eq!(
            page.items[2].title.as_deref(),
            Some("Pulp Fiction & Friends")
        );
        assert!(page.items.iter().all(|i| i.id_kind == "imdb"));
        assert_eq!(page.has_next_page, None);
    }

    #[test]
    fn a_page_without_titles_is_empty() {
        assert!(
            parse_list_page("<html><body>Nothing here</body></html>")
                .items
                .is_empty()
        );
    }

    #[test]
    fn a_list_id_or_a_list_address_is_accepted() {
        let id =
            |v: serde_json::Value| ImdbListProvider::list_id(&serde_json::json!({"list_id": v}));
        assert_eq!(id("ls012345678".into()).unwrap(), "ls012345678");
        assert_eq!(
            id("https://www.imdb.com/list/ls012345678/?ref_=x".into()).unwrap(),
            "ls012345678"
        );
        assert!(id("ur12345".into()).is_err(), "a user id is not a list");
        assert!(id("".into()).is_err());
        assert!(ImdbListProvider::list_id(&serde_json::json!({})).is_err());
    }

    fn solved(body: &str, status: u16) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": "",
            "solution": {
                "url": "https://www.imdb.com/",
                "status": status,
                "response": body,
                "userAgent": "Mozilla/5.0",
                "cookies": []
            }
        }))
    }

    fn provider(server: &MockServer) -> ImdbListProvider {
        ImdbListProvider::new(&server.uri()).with_base_url("https://imdb.example")
    }

    #[tokio::test]
    async fn fetch_walks_pages_through_flaresolverr() {
        let server = MockServer::start().await;
        let p1 = next_data_page(&[("tt0133093", "The Matrix", 1999)], true);
        let p2 = next_data_page(&[("tt0088763", "Back to the Future", 1985)], false);
        Mock::given(method("POST"))
            .and(path("/v1"))
            .and(body_partial_json(serde_json::json!({
                "cmd": "request.get",
                "url": "https://imdb.example/list/ls012345678/?page=1"
            })))
            .respond_with(solved(&p1, 200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .and(body_partial_json(serde_json::json!({
                "url": "https://imdb.example/list/ls012345678/?page=2"
            })))
            .respond_with(solved(&p2, 200))
            .expect(1)
            .mount(&server)
            .await;

        let items = provider(&server)
            .fetch(&serde_json::json!({"list_id": "ls012345678"}))
            .await
            .unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.external_id.as_str()).collect();
        assert_eq!(ids, ["tt0133093", "tt0088763"]);
    }

    #[tokio::test]
    async fn a_page_that_adds_nothing_ends_the_walk() {
        // No `hasNextPage` in the links layout; IMDb answers a page past the
        // end with the last one again.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(solved(LINKS_PAGE, 200))
            .expect(2)
            .mount(&server)
            .await;
        let items = provider(&server)
            .fetch(&serde_json::json!({"list_id": "ls1"}))
            .await
            .unwrap();
        assert_eq!(items.len(), 3);
    }

    #[tokio::test]
    async fn a_missing_list_says_so() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(solved("<html>404</html>", 404))
            .mount(&server)
            .await;
        let err = provider(&server)
            .fetch(&serde_json::json!({"list_id": "ls1"}))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
        assert!(err.to_string().contains("ls1"), "{err}");
    }

    #[tokio::test]
    async fn a_failed_solve_is_a_network_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "error",
                "message": "Error solving the challenge. Timeout after 60.0 seconds."
            })))
            .mount(&server)
            .await;
        let err = provider(&server)
            .fetch(&serde_json::json!({"list_id": "ls1"}))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Network(_)), "{err:?}");
        assert!(err.to_string().contains("Timeout"), "{err}");
    }

    #[tokio::test]
    async fn a_page_with_no_titles_is_an_error_not_an_empty_list() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(solved("<html><body>Just a moment...</body></html>", 200))
            .mount(&server)
            .await;
        let err = provider(&server)
            .fetch(&serde_json::json!({"list_id": "ls1"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no titles"), "{err}");
    }
}
