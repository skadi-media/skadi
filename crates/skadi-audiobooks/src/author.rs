//! [`Author`] and [`Series`] — the organizational entities above books
//! (SKADI-I-0017). Neither is a [`LibraryItem`](skadi_core::LibraryItem); they
//! group and (for authors) drive monitoring. The user-tracked entity is
//! [`Book`](crate::book::Book).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{AsinId, AuthorId, BookSeriesId};

/// An audiobook author — an organizational entity above books, monitorable so a
/// monitored author's new releases can be discovered + added (SKADI-T-0132).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Author {
    pub id: AuthorId,
    /// Audnexus/Audible author ASIN, when known.
    #[serde(default)]
    pub asin: Option<AsinId>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    /// Library-axis status: discover + add this author's new books?
    pub monitored: bool,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub last_metadata_refresh: Option<DateTime<Utc>>,
}

impl Author {
    /// A fresh, monitored author to be enriched by metadata sync + persisted.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: AuthorId::new(),
            asin: None,
            name: name.into(),
            description: None,
            image_url: None,
            monitored: true,
            added_at: Utc::now(),
            last_metadata_refresh: None,
        }
    }
}

/// An audiobook series (e.g. "Stormlight Archive") — groups + orders books.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Series {
    pub id: BookSeriesId,
    /// Audnexus/Audible series ASIN, when known.
    #[serde(default)]
    pub asin: Option<AsinId>,
    pub name: String,
}

impl Series {
    /// A fresh series.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: BookSeriesId::new(),
            asin: None,
            name: name.into(),
        }
    }
}

/// A book's membership in a series, carried inline on [`Book`](crate::book::Book)
/// for naming/display. `position` is a string to preserve decimal/part numbering
/// (e.g. `"1"`, `"3.5"`). The repo (SKADI-T-0125) stores the link in a
/// `book_series` table and populates this on load.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SeriesLink {
    pub series_id: BookSeriesId,
    pub name: String,
    #[serde(default)]
    pub position: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_round_trips_and_defaults_monitored() {
        let a = Author::new("Andy Weir");
        assert!(a.monitored);
        let json = serde_json::to_string(&a).unwrap();
        let back: Author = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn series_and_link_round_trip() {
        let s = Series::new("Stormlight Archive");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(s, serde_json::from_str::<Series>(&json).unwrap());

        let link = SeriesLink {
            series_id: s.id,
            name: s.name.clone(),
            position: Some("1".into()),
        };
        let json = serde_json::to_string(&link).unwrap();
        assert_eq!(link, serde_json::from_str::<SeriesLink>(&json).unwrap());
    }
}
