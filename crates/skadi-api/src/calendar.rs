//! The cross-domain calendar and its iCal feed (SKADI-T-0465).
//!
//! Sonarr/Radarr expose `/calendar?start=&end=` plus a subscribable
//! `.ics`. Skadi has one calendar across every enabled domain rather than one
//! per app, assembled from the same [`LibraryProvider`] registry `/library`
//! uses — so a domain contributes to both by implementing one trait.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::{Json, Router, routing::get};
use serde::Deserialize;

use crate::error::ApiError;
use crate::library::CalendarEntryDto;
use crate::state::AppState;

/// How far either side of today a window defaults to, when the caller gives no
/// bounds. Sonarr defaults to roughly a week; a month back and forward is more
/// useful here because the interesting question is usually "what aired that I
/// still do not have", which is backward-looking.
const DEFAULT_BACK_DAYS: i64 = 30;
const DEFAULT_FORWARD_DAYS: i64 = 30;

/// The widest window that will be served. A calendar is a browsing view, not a
/// bulk export: without a cap, `start=1900-01-01&end=2100-01-01` asks every
/// provider to walk its whole library and materialise it in memory.
const MAX_WINDOW_DAYS: i64 = 400;

#[derive(Deserialize, Default)]
pub struct CalendarQuery {
    start: Option<String>,
    end: Option<String>,
}

pub fn calendar_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/calendar", get(calendar))
        .route("/calendar.ics", get(calendar_ics))
}

/// Resolve the requested window, or explain why it is not acceptable.
fn window(q: &CalendarQuery) -> Result<(chrono::NaiveDate, chrono::NaiveDate), ApiError> {
    let today = chrono::Utc::now().date_naive();
    let parse = |s: &str, which: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
            ApiError(skadi_core::AppError::Validation(format!(
                "{which} must be an ISO date (YYYY-MM-DD), got {s:?}"
            )))
        })
    };
    let from = match &q.start {
        Some(s) => parse(s, "start")?,
        None => today - chrono::Duration::days(DEFAULT_BACK_DAYS),
    };
    let to = match &q.end {
        Some(s) => parse(s, "end")?,
        None => today + chrono::Duration::days(DEFAULT_FORWARD_DAYS),
    };
    if to < from {
        return Err(ApiError(skadi_core::AppError::Validation(
            "end is before start".into(),
        )));
    }
    if (to - from).num_days() > MAX_WINDOW_DAYS {
        return Err(ApiError(skadi_core::AppError::Validation(format!(
            "window is {} days; the maximum is {MAX_WINDOW_DAYS}",
            (to - from).num_days()
        ))));
    }
    Ok((from, to))
}

/// Gather every enabled provider's entries for the window, sorted by date.
async fn entries(
    state: &AppState,
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
) -> Result<Vec<CalendarEntryDto>, ApiError> {
    let mut out = Vec::new();
    for provider in &state.library {
        // A provider that has not adopted `calendar` returns empty rather than
        // failing, so the view degrades to "the domains that support it".
        match provider.calendar(from, to).await {
            Ok(mut items) => out.append(&mut items),
            // One domain's database trouble should not blank the whole calendar.
            Err(e) => tracing::warn!(
                domain = provider.domain(),
                error = %e,
                "calendar provider failed; omitting its entries"
            ),
        }
    }
    // Date first, then title, so the order is stable across requests rather than
    // following whatever order the providers happen to be registered in.
    out.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.title.cmp(&b.title)));
    Ok(out)
}

/// `GET /calendar?start=&end=` — dated items across every enabled domain.
async fn calendar(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CalendarQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (from, to) = window(&q)?;
    Ok(Json(entries(&state, from, to).await?))
}

/// `GET /calendar.ics` — the same window as a subscribable iCal feed.
///
/// Feed readers cannot send an Authorization header, which is why query-param
/// auth exists (SKADI-T-0466); this route relies on it rather than being exempt
/// from auth, so an unauthenticated fetch still fails.
async fn calendar_ics(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CalendarQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (from, to) = window(&q)?;
    let body = to_ics(&entries(&state, from, to).await?);
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/calendar; charset=utf-8",
        )],
        body,
    ))
}

/// Escape a text value per RFC 5545 §3.3.11.
///
/// Backslash first — escaping it after the others would double-escape the
/// backslashes they introduce.
fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Render entries as an iCalendar document.
///
/// Each entry is an all-day `VEVENT`: `DTSTART;VALUE=DATE` with `DTEND` the
/// following day, because RFC 5545 makes the end exclusive — using the same date
/// for both produces a zero-length event that several readers simply do not
/// draw.
fn to_ics(entries: &[CalendarEntryDto]) -> String {
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//skadi//calendar//EN\r\nCALSCALE:GREGORIAN\r\n",
    );
    for e in entries {
        let summary = match (&e.series, &e.episode) {
            (Some(series), Some(ep)) => format!("{series} {ep} — {}", e.title),
            (Some(series), None) => format!("{series} — {}", e.title),
            _ => e.title.clone(),
        };
        let status = if e.has_file { "have" } else { "missing" };
        out.push_str("BEGIN:VEVENT\r\n");
        // Stable across regenerations: a reader that re-fetches must see the same
        // event, not a duplicate. Keyed on the item and the date, which is what
        // identifies the occurrence.
        out.push_str(&format!(
            "UID:{}-{}@skadi\r\n",
            e.id,
            e.date.format("%Y%m%d")
        ));
        out.push_str(&format!(
            "DTSTART;VALUE=DATE:{}\r\n",
            e.date.format("%Y%m%d")
        ));
        out.push_str(&format!(
            "DTEND;VALUE=DATE:{}\r\n",
            (e.date + chrono::Duration::days(1)).format("%Y%m%d")
        ));
        out.push_str(&format!("SUMMARY:{}\r\n", ics_escape(&summary)));
        out.push_str(&format!(
            "DESCRIPTION:{}\r\n",
            ics_escape(&format!("{} · {status}", e.kind))
        ));
        out.push_str("END:VEVENT\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, date: &str, has_file: bool) -> CalendarEntryDto {
        CalendarEntryDto {
            kind: "series".into(),
            id: "abc".into(),
            title: title.into(),
            series: Some("Severance".into()),
            episode: Some("S02E05".into()),
            date: chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            has_file,
            monitored: true,
        }
    }

    #[test]
    fn an_all_day_event_ends_the_following_day() {
        // RFC 5545 makes DTEND exclusive: same-date start and end is a
        // zero-length event that several readers do not draw at all.
        let ics = to_ics(&[entry("Chikhai Bardo", "2026-09-08", false)]);
        assert!(ics.contains("DTSTART;VALUE=DATE:20260908\r\n"), "{ics}");
        assert!(ics.contains("DTEND;VALUE=DATE:20260909\r\n"), "{ics}");
    }

    #[test]
    fn the_uid_is_stable_so_a_refetch_does_not_duplicate_the_event() {
        let a = to_ics(&[entry("Chikhai Bardo", "2026-09-08", false)]);
        let b = to_ics(&[entry("Chikhai Bardo", "2026-09-08", true)]);
        let uid = |s: &str| {
            s.lines()
                .find(|l| l.starts_with("UID:"))
                .unwrap()
                .to_string()
        };
        assert_eq!(
            uid(&a),
            uid(&b),
            "the UID must not move when the item's state changes"
        );
    }

    #[test]
    fn text_is_escaped_per_rfc_5545() {
        let mut e = entry("Hard, Fast; and\\Loose", "2026-09-08", false);
        e.series = None;
        e.episode = None;
        let ics = to_ics(&[e]);
        // Backslash is escaped first, so the escapes the others introduce are not
        // themselves doubled.
        assert!(ics.contains(r"SUMMARY:Hard\, Fast\; and\\Loose"), "{ics}");
    }

    #[test]
    fn the_document_is_well_formed_even_with_no_entries() {
        let ics = to_ics(&[]);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        // An empty calendar is a valid calendar — a reader subscribing before
        // anything is scheduled must not get a parse error.
        assert!(!ics.contains("BEGIN:VEVENT"));
    }

    #[test]
    fn a_window_is_bounded_and_dates_must_parse() {
        let q = |s: &str, e: &str| CalendarQuery {
            start: Some(s.into()),
            end: Some(e.into()),
        };
        assert!(window(&q("2026-09-01", "2026-09-30")).is_ok());
        // A calendar is a browsing view, not a bulk export: an unbounded window
        // asks every provider to materialise its whole library.
        assert!(window(&q("1900-01-01", "2100-01-01")).is_err());
        assert!(
            window(&q("2026-09-30", "2026-09-01")).is_err(),
            "end < start"
        );
        assert!(window(&q("not-a-date", "2026-09-30")).is_err());
        // No bounds at all is fine — it defaults to a window around today.
        assert!(window(&CalendarQuery::default()).is_ok());
    }
}
