//! Keyless Audible **catalog** lookup for author monitoring (SKADI-T-0132).
//!
//! Audnexus ([`AudnexusProvider`](crate::AudnexusProvider)) is ASIN-keyed and has
//! **no "list books by author"** endpoint — so the periodic "discover a monitored
//! author's new releases" step has no Audnexus data source. The Audible catalog
//! API does: `GET /1.0/catalog/products?author=<name>` lists an author's products
//! (ASINs), which we then enrich one-by-one through Audnexus.
//!
//! This is the **same upstream Audnexus itself derives from** (the operator chose
//! option (a) for SKADI-T-0132), so it introduces no new vendor relationship —
//! just a second endpoint. It is keyless project-run-adjacent infra with no
//! third-party ToS/SLA: the same "private personal project only" posture as ADR
//! SKADI-A-0001 applies, and every request carries an honest `skadi/<version>`
//! User-Agent.
//!
//! Only the inherent [`list_by_author`](AudibleCatalogProvider::list_by_author) is
//! exposed; this is not a [`MetadataProvider`](crate::MetadataProvider) (it does
//! discovery, not per-ASIN records — that stays Audnexus's job).

use chrono::NaiveDate;
use serde::Deserialize;

use skadi_core::{AppError, AsinId, Result};
use skadi_http::HttpClient;

const DEFAULT_BASE_URL: &str = "https://api.audible.com";
const DEFAULT_REGION: &str = "us";

/// Products per catalog page. **50 is the API's hard maximum** — a larger value is
/// rejected with HTTP 400 "greater than maximum allowed 50 per page" (measured
/// 2026-09-30), so a big author has to be read across several pages.
pub const NUM_RESULTS: u32 = 50;

/// Ceiling on pages read for one author in a single full read (500 products).
/// Stops a pathological or mis-matched author query looping for ever; hitting it
/// is logged. George R. R. Martin, a heavily translated author, is 111 products
/// (3 pages), so this is generous (SKADI-T-0650).
pub const MAX_PAGES_PER_AUTHOR: u32 = 10;

/// Max results for a free-text title search (enough to choose from, not a wall).
const SEARCH_RESULTS: u32 = 24;

/// The honest identity we present (mirrors ADR SKADI-A-0001 condition 1).
const USER_AGENT: &str = concat!("skadi/", env!("CARGO_PKG_VERSION"));

/// A keyless Audible catalog provider for author → products discovery.
pub struct AudibleCatalogProvider {
    base_url: String,
    region: String,
    http: HttpClient,
}

/// One product from the Audible catalog ([`list_by_author`] /
/// [`search`](AudibleCatalogProvider::search)) — enough to show in an add UI and
/// add by ASIN (the full record is fetched from Audnexus during enrichment).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatalogItem {
    pub asin: AsinId,
    pub title: String,
    pub release_date: Option<NaiveDate>,
    /// Author display names (may be empty).
    pub authors: Vec<String>,
    /// The primary (first) author's Audible ASIN, when the `contributors` group
    /// carried it. Aligns with `authors[0]`. Lets an imported library register
    /// author entities so the author-scope browse/Watch works (SKADI-T-0160).
    pub author_asin: Option<AsinId>,
    /// A cover image URL, when the `media` response group is present.
    pub cover_url: Option<String>,
    /// Primary series ASIN (the reading-order series with a real sequence), when
    /// the `series` response group is present (SKADI-I-0018).
    pub series_asin: Option<AsinId>,
    pub series_name: Option<String>,
    /// Position within the primary series, e.g. `"1"` / `"2.5"`.
    pub series_position: Option<String>,
    /// The product's language, lowercased (`"english"`, `"german"`), when the
    /// `product_attrs` response group is present. Carried **inline** on the list
    /// response, so discovery need not spend a per-title request to classify it
    /// (SKADI-T-0650). `None` when absent.
    pub language: Option<String>,
}

/// One page of an author's catalog, plus what the caller needs to decide whether
/// there is another (SKADI-T-0650).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatalogPage {
    /// The page's usable products (rows without an ASIN or title are dropped).
    pub items: Vec<CatalogItem>,
    /// How many products the API returned on this page **before** dropping
    /// incomplete rows. Paging must stop on this, not on `items.len()` — a full
    /// page containing a title-less product would otherwise read as a short final
    /// page and end the read early.
    pub raw_count: usize,
    /// The API's reported total for the query, when present.
    pub total_results: Option<u32>,
}

impl CatalogPage {
    /// Whether page `index` (0-based) is the last one: it came back short, or the
    /// pages read so far cover the reported total.
    #[must_use]
    pub fn is_last(&self, index: u32) -> bool {
        if self.raw_count < NUM_RESULTS as usize {
            return true;
        }
        self.total_results
            .is_some_and(|t| u64::from(index + 1) * u64::from(NUM_RESULTS) >= u64::from(t))
    }
}

/// Build a [`CatalogItem`] from a raw product, or `None` when it lacks an ASIN or
/// title (can't seed a book). Shared by `list_by_author` and `search`.
fn catalog_item(p: CatalogProduct) -> Option<CatalogItem> {
    let title = p.title.filter(|t| !t.is_empty())?;
    if p.asin.is_empty() {
        return None;
    }
    // The primary author's ASIN (first contributor that has a name), aligned with
    // `authors[0]`. `None` if the primary author carried no ASIN.
    let author_asin = p
        .authors
        .iter()
        .find(|c| c.name.as_deref().is_some_and(|n| !n.is_empty()))
        .and_then(|c| c.asin.clone())
        .filter(|a| !a.is_empty())
        .map(AsinId);
    let authors = p
        .authors
        .into_iter()
        .filter_map(|a| a.name)
        .filter(|n| !n.is_empty())
        .collect();
    let cover_url = p
        .product_images
        .get("500")
        .cloned()
        .or_else(|| p.product_images.values().next().cloned());
    // A product can sit in several "series" (e.g. an umbrella collection +
    // the reading-order series). Prefer the first with a real sequence; else the
    // first that has both an asin and a title.
    let primary = p
        .series
        .iter()
        .find(|s| s.sequence.as_deref().is_some_and(|q| !q.trim().is_empty()))
        .or_else(|| {
            p.series
                .iter()
                .find(|s| !s.asin.is_empty() && s.title.is_some())
        });
    Some(CatalogItem {
        asin: AsinId(p.asin),
        title,
        release_date: p
            .release_date
            .as_deref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        authors,
        author_asin,
        cover_url,
        series_asin: primary
            .filter(|s| !s.asin.is_empty())
            .map(|s| AsinId(s.asin.clone())),
        series_name: primary.and_then(|s| s.title.clone()),
        series_position: primary
            .and_then(|s| s.sequence.clone())
            .filter(|q| !q.trim().is_empty()),
        language: p
            .language
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty()),
    })
}

impl AudibleCatalogProvider {
    /// Construct against the public endpoint (region `us`).
    #[must_use]
    pub fn new(http: HttpClient) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            region: DEFAULT_REGION.to_string(),
            http,
        }
    }

    /// Override the base URL (used by tests against a fake server).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the Audible region (default `us`).
    #[must_use]
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    /// Every product for an author, newest first, read page by page until the
    /// catalog is exhausted or [`MAX_PAGES_PER_AUTHOR`] is reached.
    ///
    /// Reading only the first page was SKADI-T-0650: for a heavily translated
    /// author the newest 50 products are almost all translations, which the
    /// English-only catalog then discards, so the English back catalog was never
    /// fetched at all. George R. R. Martin's page 0 held 42 translations and none
    /// of the English *A Song of Ice and Fire* novels; they were on pages 1 and 2.
    ///
    /// Callers that need to spread pages across discovery passes use
    /// [`list_by_author_page`](Self::list_by_author_page) instead.
    pub async fn list_by_author(&self, author: &str) -> Result<Vec<CatalogItem>> {
        let mut all = Vec::new();
        for index in 0..MAX_PAGES_PER_AUTHOR {
            let page = self.list_by_author_page(author, index).await?;
            let last = page.is_last(index);
            all.extend(page.items);
            if last {
                return Ok(all);
            }
        }
        tracing::warn!(
            author,
            pages = MAX_PAGES_PER_AUTHOR,
            "audible catalog: page ceiling reached; the rest of this author's catalog was not read"
        );
        Ok(all)
    }

    /// One page (`index`, 0-based) of an author's products, newest first
    /// (`GET /1.0/catalog/products?author={name}&page={index}&products_sort_by=-ReleaseDate`).
    ///
    /// Requests `product_attrs` so each item carries its language inline.
    /// Products without an ASIN or title are skipped (they can't seed a book), but
    /// still counted in [`CatalogPage::raw_count`].
    ///
    /// There is **no query-time language filter**: `language`, `languages`,
    /// `filter_language` and `lang` are all silently ignored by the API (measured
    /// 2026-09-30 — `total_results` unchanged), so filtering has to happen after
    /// the fetch.
    pub async fn list_by_author_page(&self, author: &str, index: u32) -> Result<CatalogPage> {
        let url = format!(
            "{}/1.0/catalog/products",
            self.base_url.trim_end_matches('/')
        );
        let region = self.region.clone();
        let author = author.to_string();
        let num = NUM_RESULTS.to_string();
        let page = index.to_string();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url).header("User-Agent", USER_AGENT).query(&[
                    ("author", author.as_str()),
                    ("num_results", num.as_str()),
                    ("page", page.as_str()),
                    ("products_sort_by", "-ReleaseDate"),
                    // `media` yields product_images → cover_url on each Work, so the
                    // library's missing-book tiles show art, not a text placeholder
                    // (SKADI-T-0353 follow-up). `product_attrs` carries `language`
                    // inline, so classifying a title costs no extra request
                    // (SKADI-T-0650).
                    (
                        "response_groups",
                        "contributors,product_desc,series,media,product_attrs",
                    ),
                    ("region", region.as_str()),
                ])
            })
            .await?;
        let body: CatalogResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding audible catalog: {e}")))?;
        let raw_count = body.products.len();
        Ok(CatalogPage {
            items: body.products.into_iter().filter_map(catalog_item).collect(),
            raw_count,
            total_results: body.total_results,
        })
    }

    /// Search the catalog by free-text keywords (title/author), most-relevant
    /// first (`GET /1.0/catalog/products?keywords={q}`). Backs the audiobooks
    /// "search by title" add flow (SKADI-T-0151). Products without an ASIN/title
    /// are skipped.
    pub async fn search(&self, keywords: &str) -> Result<Vec<CatalogItem>> {
        let url = format!(
            "{}/1.0/catalog/products",
            self.base_url.trim_end_matches('/')
        );
        let region = self.region.clone();
        let keywords = keywords.to_string();
        let num = SEARCH_RESULTS.to_string();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url).header("User-Agent", USER_AGENT).query(&[
                    ("keywords", keywords.as_str()),
                    ("num_results", num.as_str()),
                    ("response_groups", "contributors,product_desc,media"),
                    ("region", region.as_str()),
                ])
            })
            .await?;
        let body: CatalogResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding audible catalog search: {e}")))?;
        Ok(body.products.into_iter().filter_map(catalog_item).collect())
    }

    /// Fetch a single product's **language** (lowercased, e.g. `"english"` /
    /// `"german"`) from the per-title detail endpoint
    /// (`GET /1.0/catalog/products/{asin}?response_groups=product_attrs`).
    ///
    /// `product_attrs` is the response group that carries language (verified
    /// against the live API — `product_extended_attrs` omits it). The author list
    /// now requests it too, so this per-title lookup is only the fallback for a
    /// product that arrived without it (SKADI-T-0650). An earlier version of this
    /// comment said the list endpoints cannot return language; they can, they
    /// were simply not asked to. The works-catalog ingest
    /// classifies each title with this and caches the result on the work
    /// (SKADI-T-0160). `Ok(None)` when the field is absent.
    pub async fn product_language(&self, asin: &AsinId) -> Result<Option<String>> {
        let url = format!(
            "{}/1.0/catalog/products/{}",
            self.base_url.trim_end_matches('/'),
            asin.0
        );
        let region = self.region.clone();
        let resp = self
            .http
            .send_idempotent(|c| {
                c.get(&url).header("User-Agent", USER_AGENT).query(&[
                    ("response_groups", "product_attrs"),
                    ("region", region.as_str()),
                ])
            })
            .await?;
        let body: ProductResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Network(format!("decoding audible product: {e}")))?;
        Ok(body
            .product
            .and_then(|p| p.language)
            .map(|l| l.to_lowercase())
            .filter(|l| !l.is_empty()))
    }
}

#[derive(Deserialize)]
struct ProductResponse {
    #[serde(default)]
    product: Option<ProductDetail>,
}

#[derive(Deserialize)]
struct ProductDetail {
    #[serde(default)]
    language: Option<String>,
}

#[derive(Deserialize)]
struct CatalogResponse {
    #[serde(default)]
    products: Vec<CatalogProduct>,
    /// The query's total across all pages.
    #[serde(default)]
    total_results: Option<u32>,
}

#[derive(Deserialize)]
struct CatalogProduct {
    #[serde(default)]
    asin: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    authors: Vec<Contributor>,
    /// `{ "500": "https://…/cover.jpg", … }` (present with the `media` group).
    #[serde(default)]
    product_images: std::collections::HashMap<String, String>,
    /// Series memberships (present with the `series` group).
    #[serde(default)]
    series: Vec<CatalogSeries>,
    /// Present with the `product_attrs` group, e.g. `"english"`.
    #[serde(default)]
    language: Option<String>,
}

#[derive(Deserialize)]
struct Contributor {
    #[serde(default)]
    name: Option<String>,
    /// The contributor's own Audible ASIN — present when the `contributors`
    /// response group is requested. Lets us register the author entity (and tag
    /// works with `author_asin`) for an imported library (SKADI-T-0160).
    #[serde(default)]
    asin: Option<String>,
}

#[derive(Deserialize)]
struct CatalogSeries {
    #[serde(default)]
    asin: String,
    #[serde(default)]
    title: Option<String>,
    /// Reading-order position; `""` for umbrella collections.
    #[serde(default)]
    sequence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client() -> HttpClient {
        HttpClient::new(Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn lists_products_for_an_author_skipping_incomplete_rows() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products"))
            .and(query_param("author", "Andy Weir"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "products": [
                    { "asin": "B08G9PRS1K", "title": "Project Hail Mary", "release_date": "2021-05-04",
                      "authors": [{ "name": "Andy Weir", "asin": "B00G0WYW92" }],
                      "product_images": { "500": "https://img/phm.jpg" } },
                    { "asin": "B002V1OF70", "title": "The Martian" },
                    { "asin": "", "title": "no asin — skip" },
                    { "asin": "B999", "title": null }
                ]
            })))
            .mount(&server)
            .await;

        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        let items = provider.list_by_author("Andy Weir").await.unwrap();

        assert_eq!(items.len(), 2, "the two complete rows survive");
        assert_eq!(items[0].asin, AsinId("B08G9PRS1K".into()));
        assert_eq!(items[0].title, "Project Hail Mary");
        // The `contributors` group carries the author's ASIN — captured so an
        // imported library can register author entities (SKADI-T-0160).
        assert_eq!(items[0].author_asin, Some(AsinId("B00G0WYW92".into())));
        assert!(items[1].author_asin.is_none(), "no contributor asin → None");
        assert_eq!(
            items[0].release_date,
            Some(NaiveDate::from_ymd_opt(2021, 5, 4).unwrap())
        );
        // The `media` response group surfaces a cover on each catalog work, so the
        // library's missing-book tiles render art (SKADI-T-0353 follow-up).
        assert_eq!(items[0].cover_url.as_deref(), Some("https://img/phm.jpg"));
        assert_eq!(items[1].asin, AsinId("B002V1OF70".into()));
        assert!(
            items[1].release_date.is_none(),
            "missing date is None, not an error"
        );
    }

    #[tokio::test]
    async fn search_returns_items_with_authors_and_cover() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products"))
            .and(query_param("keywords", "hail mary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "products": [
                    {
                        "asin": "B08G9PRS1K",
                        "title": "Project Hail Mary",
                        "release_date": "2021-05-04",
                        "authors": [{ "name": "Andy Weir" }],
                        "product_images": { "500": "https://img/cover.jpg" }
                    },
                    { "asin": "", "title": "skip — no asin" }
                ]
            })))
            .mount(&server)
            .await;

        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        let items = provider.search("hail mary").await.unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].asin, AsinId("B08G9PRS1K".into()));
        assert_eq!(items[0].authors, vec!["Andy Weir".to_string()]);
        assert_eq!(items[0].cover_url.as_deref(), Some("https://img/cover.jpg"));
    }

    #[tokio::test]
    async fn product_language_reads_lowercased_language() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products/B0CWLMZ4DF"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "product": { "asin": "B0CWLMZ4DF", "title": "Titanenkampf", "language": "German" }
            })))
            .mount(&server)
            .await;
        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        let lang = provider
            .product_language(&AsinId("B0CWLMZ4DF".into()))
            .await
            .unwrap();
        assert_eq!(lang.as_deref(), Some("german"));
    }

    #[tokio::test]
    async fn product_language_absent_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products/B999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "product": { "asin": "B999", "title": "No language field" }
            })))
            .mount(&server)
            .await;
        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        assert!(
            provider
                .product_language(&AsinId("B999".into()))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A full page of `n` products in `lang`, ASINs prefixed so pages are told
    /// apart.
    fn page_of(prefix: &str, n: usize, lang: &str) -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| {
                serde_json::json!({
                    "asin": format!("{prefix}{i:03}"),
                    "title": format!("{prefix} title {i}"),
                    "language": lang,
                    "authors": [{ "name": "George R. R. Martin", "asin": "B000APIGH4" }]
                })
            })
            .collect()
    }

    async fn mount_page(server: &MockServer, index: u32, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products"))
            .and(query_param("page", index.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// The SKADI-T-0650 shape, from the live measurement: page 0 is entirely
    /// translations, and the English works only appear on later pages. Reading
    /// one page found none of them.
    #[tokio::test]
    async fn reads_past_a_page_of_translations_to_the_english_works() {
        let server = MockServer::start().await;
        let total = 50 + 50 + 11;
        mount_page(
            &server,
            0,
            serde_json::json!({ "total_results": total, "products": page_of("DE", 50, "german") }),
        )
        .await;
        let mut p1 = page_of("FR", 45, "french");
        p1.extend(page_of("EN1", 5, "English"));
        mount_page(
            &server,
            1,
            serde_json::json!({ "total_results": total, "products": p1 }),
        )
        .await;
        mount_page(
            &server,
            2,
            serde_json::json!({ "total_results": total, "products": page_of("EN2", 11, "english") }),
        )
        .await;

        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        let items = provider
            .list_by_author("George R. R. Martin")
            .await
            .unwrap();

        assert_eq!(items.len(), total, "all three pages are read");
        let english: Vec<_> = items
            .iter()
            .filter(|i| i.language.as_deref() == Some("english"))
            .collect();
        assert_eq!(
            english.len(),
            16,
            "the English works on pages 1 and 2 are found"
        );
        assert!(
            items.iter().all(|i| i.language.is_some()),
            "language arrives inline, lowercased — no per-title lookup needed"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            3,
            "stops after the short final page; does not ask for page 3"
        );
    }

    #[tokio::test]
    async fn stops_at_the_page_ceiling() {
        let server = MockServer::start().await;
        // Every page is full and the total claims far more than the ceiling covers.
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "total_results": 100_000, "products": page_of("X", 50, "english") }),
            ))
            .mount(&server)
            .await;
        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        let items = provider.list_by_author("Prolific").await.unwrap();
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            MAX_PAGES_PER_AUTHOR as usize,
            "never reads past the ceiling"
        );
        assert_eq!(items.len(), (MAX_PAGES_PER_AUTHOR * NUM_RESULTS) as usize);
    }

    /// A full page that contains unusable rows must not read as the last page —
    /// paging decides on the raw count, not on what survived filtering.
    #[tokio::test]
    async fn a_full_page_with_incomplete_rows_is_not_mistaken_for_the_last() {
        let server = MockServer::start().await;
        let mut p0 = page_of("A", 48, "english");
        p0.push(serde_json::json!({ "asin": "", "title": "no asin" }));
        p0.push(serde_json::json!({ "asin": "B999", "title": null }));
        mount_page(
            &server,
            0,
            serde_json::json!({ "total_results": 60, "products": p0 }),
        )
        .await;
        mount_page(
            &server,
            1,
            serde_json::json!({ "total_results": 60, "products": page_of("B", 10, "english") }),
        )
        .await;
        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        let items = provider.list_by_author("Someone").await.unwrap();
        assert_eq!(items.len(), 58, "48 usable from page 0, 10 from page 1");
    }

    #[test]
    fn last_page_follows_the_raw_count_and_the_reported_total() {
        let page = |raw_count, total| CatalogPage {
            items: vec![],
            raw_count,
            total_results: total,
        };
        assert!(page(11, Some(111)).is_last(2), "short page ends it");
        assert!(!page(50, Some(111)).is_last(0), "full page, more to come");
        assert!(
            page(50, Some(100)).is_last(1),
            "two full pages cover a total of 100"
        );
        assert!(
            !page(50, None).is_last(4),
            "full page and no total: keep going"
        );
    }

    #[tokio::test]
    async fn empty_catalog_is_an_empty_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/1.0/catalog/products"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "products": [] })),
            )
            .mount(&server)
            .await;
        let provider = AudibleCatalogProvider::new(client()).with_base_url(server.uri());
        assert!(provider.list_by_author("Nobody").await.unwrap().is_empty());
    }
}
