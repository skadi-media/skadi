//! Catalog **works** (SKADI-I-0018 / SKADI-T-0154).
//!
//! A [`Work`] is the record of a book that exists per the Audible catalog —
//! **known but not necessarily owned**. It's keyed by Audible ASIN and links to a
//! library [`Book`](crate::book::Book) by that ASIN when owned. Works are what let
//! us show true series completeness ("have N of M") and browse a body of work,
//! decoupled from acquisition (a [watcher] drives acquiring missing works).

use chrono::{DateTime, Utc};

use skadi_core::AsinId;

/// One catalog work — a book Audible knows about, owned or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Work {
    pub asin: AsinId,
    pub title: String,
    /// Author display names (catalog order).
    pub authors: Vec<String>,
    /// Primary author's ASIN, when known (links to `authors`/other works).
    pub author_asin: Option<AsinId>,
    pub series_name: Option<String>,
    /// Audible series ASIN, when captured (lets works group by series id).
    pub series_asin: Option<AsinId>,
    /// Position within the series (`"1"`, `"2.5"`), as a string on the wire.
    pub series_position: Option<String>,
    pub cover_url: Option<String>,
    /// Release date as an ISO `YYYY-MM-DD` string (kept verbatim).
    pub release_date: Option<String>,
    /// Audible product language, lowercased (`"english"`, `"german"`, …), cached
    /// from the per-title detail endpoint. `None` = not yet classified; treated as
    /// "show" so a lookup failure never hides a work (SKADI-T-0160).
    pub language: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The English-only catalog gate (SKADI-T-0160 / SKADI-I-0051): keep English and
/// keep **unclassified** (`None`) — a transient language-lookup failure must never
/// drop a real English title — but reject a positively-non-English work. Shared by
/// the HTTP catalog views, catalog *ingest* (don't store foreign works), and
/// *watcher resolution* (don't acquire foreign works).
#[must_use]
pub fn is_catalog_language(lang: Option<&str>) -> bool {
    lang.map(|l| l.eq_ignore_ascii_case("english"))
        .unwrap_or(true)
}

/// The scope a [`Watcher`] applies to (SKADI-T-0156). A watcher is the
/// acquisition switch: known works it covers, that aren't owned, get acquired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchScope {
    /// All of an author's works (keyed by author ASIN).
    Author,
    /// All works in a series (keyed by series ASIN).
    Series,
    /// A single work (keyed by the work/book ASIN).
    Book,
}

impl WatchScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WatchScope::Author => "author",
            WatchScope::Series => "series",
            WatchScope::Book => "book",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "author" => Some(WatchScope::Author),
            "series" => Some(WatchScope::Series),
            "book" => Some(WatchScope::Book),
            _ => None,
        }
    }
}

/// An acquisition watcher: a `scope` (author/series/book) + the ASIN it keys on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watcher {
    pub scope: WatchScope,
    pub key: String,
}

impl Work {
    /// A freshly-seen work (`first_seen == updated_at == now`); fill the rest with
    /// the builder-ish setters or struct update syntax.
    #[must_use]
    pub fn new(asin: AsinId, title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            asin,
            title: title.into(),
            authors: Vec::new(),
            author_asin: None,
            series_name: None,
            series_asin: None,
            series_position: None,
            cover_url: None,
            release_date: None,
            language: None,
            first_seen: now,
            updated_at: now,
        }
    }
}
