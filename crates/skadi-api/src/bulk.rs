//! The wire shape of the library bulk endpoints (SKADI-T-0696).
//!
//! `POST /movies/bulk`, `POST /series/bulk` and `POST /books/bulk` each take a
//! [`BulkRequest`] and answer a [`BulkReport`]. The types live here, not in the
//! domain crates, so the three endpoints cannot drift apart: the web client
//! sends one body shape and reads one report shape for every wall.
//!
//! Each domain handler applies the action to one id at a time **through the
//! same code its single-item route uses** (PATCH for monitoring, the manual
//! acquire for search, DELETE for delete), so a bulk delete removes exactly the files
//! the single delete would (the item's recorded imported files, with the
//! emptied folders pruned up to its root). What this module
//! adds is the validation of the request and the bookkeeping of the outcome.

use serde::{Deserialize, Serialize};
use skadi_core::AppError;

use crate::ApiError;

/// The most ids one bulk request may carry. A whole wall is a few thousand
/// items at most; a bound stops a malformed client from asking for millions.
pub const MAX_BULK_IDS: usize = 5000;

/// What to do to every selected item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BulkAction {
    /// Set `monitored = true`.
    Monitor,
    /// Set `monitored = false`.
    Unmonitor,
    /// Start the manual acquire for each unit of the item that is not imported
    /// and not already being worked.
    Search,
    /// Remove the item (and, with `delete_files`, its imported files).
    Delete,
}

/// `POST /{movies|series|books}/bulk` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkRequest {
    /// The item ids (UUIDs), as the list endpoint returns them.
    pub ids: Vec<String>,
    pub action: BulkAction,
    /// Only with `action: delete`: also remove the imported files.
    #[serde(default)]
    pub delete_files: bool,
}

impl BulkRequest {
    /// Check the request and return its ids parsed, without duplicates, in the
    /// order sent. Refuses (400) an empty or oversized list, an id that is not
    /// a UUID, and `delete_files` on an action other than delete.
    pub fn parsed_ids<T: From<uuid::Uuid>>(&self) -> Result<Vec<(String, T)>, ApiError> {
        if self.ids.is_empty() {
            return Err(validation("ids must name at least one item"));
        }
        if self.ids.len() > MAX_BULK_IDS {
            return Err(validation(&format!(
                "ids may name at most {MAX_BULK_IDS} items"
            )));
        }
        if self.delete_files && self.action != BulkAction::Delete {
            return Err(validation("delete_files is valid only with action delete"));
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(self.ids.len());
        for raw in &self.ids {
            let id = uuid::Uuid::parse_str(raw)
                .map_err(|e| validation(&format!("invalid id {raw:?}: {e}")))?;
            if seen.insert(id) {
                out.push((id.to_string(), T::from(id)));
            }
        }
        Ok(out)
    }
}

fn validation(msg: &str) -> ApiError {
    ApiError(AppError::Validation(msg.to_string()))
}

/// An item the action was not applied to, and why.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BulkSkip {
    pub id: String,
    pub reason: String,
}

/// An item the action failed on, and the error.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BulkFailure {
    pub id: String,
    pub error: String,
}

/// The answer to a bulk request (always 200 once the request is valid). Every
/// distinct id is in exactly one of `done`, `skipped`, `not_found` or `failed`.
#[derive(Debug, Clone, Serialize)]
pub struct BulkReport {
    pub action: BulkAction,
    /// The number of distinct ids.
    pub requested: usize,
    /// The ids the action was applied to.
    pub done: Vec<String>,
    /// The ids with nothing to do (search only: nothing missing, or every
    /// missing unit already in flight).
    pub skipped: Vec<BulkSkip>,
    /// The ids that name no item.
    pub not_found: Vec<String>,
    /// The ids the action failed on.
    pub failed: Vec<BulkFailure>,
    /// Search only: the acquire runs started over all the items.
    pub searches_started: usize,
}

/// What applying the action to one item came to.
#[derive(Debug)]
pub enum BulkOutcome {
    Done,
    /// Search: this many acquire runs were started (0 → skipped).
    Searched(usize),
    NotFound,
}

impl BulkReport {
    #[must_use]
    pub fn new(action: BulkAction, requested: usize) -> Self {
        Self {
            action,
            requested,
            done: Vec::new(),
            skipped: Vec::new(),
            not_found: Vec::new(),
            failed: Vec::new(),
            searches_started: 0,
        }
    }

    /// File one item's outcome under its id.
    pub fn record(&mut self, id: String, outcome: Result<BulkOutcome, ApiError>) {
        match outcome {
            Ok(BulkOutcome::Done) => self.done.push(id),
            Ok(BulkOutcome::Searched(0)) => self.skipped.push(BulkSkip {
                id,
                reason: "nothing to search: every unit is imported or already in flight".into(),
            }),
            Ok(BulkOutcome::Searched(n)) => {
                self.searches_started += n;
                self.done.push(id);
            }
            Ok(BulkOutcome::NotFound) | Err(ApiError(AppError::NotFound(_))) => {
                self.not_found.push(id);
            }
            Err(ApiError(e)) => self.failed.push(BulkFailure {
                id,
                error: e.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(ids: &[&str], action: BulkAction, delete_files: bool) -> BulkRequest {
        BulkRequest {
            ids: ids.iter().map(|s| (*s).to_string()).collect(),
            action,
            delete_files,
        }
    }

    const A: &str = "11111111-1111-1111-1111-111111111111";
    const B: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn ids_are_parsed_deduplicated_and_kept_in_order() {
        let got: Vec<(String, uuid::Uuid)> = req(&[B, A, B], BulkAction::Monitor, false)
            .parsed_ids()
            .unwrap();
        let ids: Vec<&str> = got.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(ids, vec![B, A]);
    }

    #[test]
    fn a_bad_request_is_refused() {
        let parse = |r: BulkRequest| r.parsed_ids::<uuid::Uuid>().is_err();
        assert!(parse(req(&[], BulkAction::Monitor, false)));
        assert!(parse(req(&["nope"], BulkAction::Monitor, false)));
        assert!(parse(req(&[A], BulkAction::Unmonitor, true)));
        assert!(!parse(req(&[A], BulkAction::Delete, true)));
        let many: Vec<String> = (0..=MAX_BULK_IDS)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(parse(req(&refs, BulkAction::Monitor, false)));
    }

    #[test]
    fn the_body_reads_the_documented_shape() {
        let r: BulkRequest = serde_json::from_value(serde_json::json!({
            "ids": [A], "action": "delete", "delete_files": true
        }))
        .unwrap();
        assert_eq!(r.action, BulkAction::Delete);
        assert!(r.delete_files);
        let bad = serde_json::from_value::<BulkRequest>(serde_json::json!({
            "ids": [A], "action": "explode"
        }));
        assert!(bad.is_err());
        let extra = serde_json::from_value::<BulkRequest>(serde_json::json!({
            "ids": [A], "action": "monitor", "force": true
        }));
        assert!(extra.is_err(), "unknown fields are refused");
    }

    #[test]
    fn outcomes_are_filed_once_each() {
        let mut r = BulkReport::new(BulkAction::Search, 4);
        r.record("a".into(), Ok(BulkOutcome::Searched(2)));
        r.record("b".into(), Ok(BulkOutcome::Searched(0)));
        r.record("c".into(), Err(ApiError(AppError::NotFound("x".into()))));
        r.record("d".into(), Err(ApiError(AppError::Internal("boom".into()))));
        assert_eq!(r.done, vec!["a"]);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.not_found, vec!["c"]);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.searches_started, 2);
    }
}
