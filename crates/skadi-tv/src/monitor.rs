//! Monitoring modes (SKADI-T-0271) — Sonarr's "monitor" options, applied at
//! add-time to seed each episode's `monitored` flag and togglable per
//! season/episode afterwards.
//!
//! The mode is a **pure selection policy**: given an episode's `(season, number,
//! air_date)` plus the series' first/last season and today's date, it decides
//! whether that episode starts monitored. The sweep ([`crate::SeriesWantedQuery`])
//! then only searches monitored episodes, so the mode flows straight through to
//! acquisition without any special-casing there.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// What to monitor when a series is added (mirrors Sonarr's set).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MonitorMode {
    /// Every regular episode. Specials (season 0) are **opt-in**: switch the
    /// Specials season on from the series page (`POST /series/{id}/seasons/0/monitor`,
    /// which cascades to its episodes) — SKADI-T-0389.
    #[default]
    All,
    /// Only episodes that haven't aired yet.
    Future,
    /// Aired episodes (at add-time, everything we'd need to go grab).
    Missing,
    /// Only episodes already in the library — at add-time (no files yet) this
    /// monitors nothing; library-import sets `monitored` on what it finds.
    Existing,
    /// Only the first (lowest-numbered, non-special) season.
    FirstSeason,
    /// Only the latest (highest-numbered) season.
    LastSeason,
    /// Only the pilot — S01E01.
    Pilot,
    /// Nothing — add the series unmonitored.
    None,
}

impl MonitorMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MonitorMode::All => "all",
            MonitorMode::Future => "future",
            MonitorMode::Missing => "missing",
            MonitorMode::Existing => "existing",
            MonitorMode::FirstSeason => "firstSeason",
            MonitorMode::LastSeason => "lastSeason",
            MonitorMode::Pilot => "pilot",
            MonitorMode::None => "none",
        }
    }

    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "future" => MonitorMode::Future,
            "missing" => MonitorMode::Missing,
            "existing" => MonitorMode::Existing,
            "firstSeason" | "first_season" => MonitorMode::FirstSeason,
            "lastSeason" | "last_season" => MonitorMode::LastSeason,
            "pilot" => MonitorMode::Pilot,
            "none" => MonitorMode::None,
            _ => MonitorMode::All,
        }
    }

    /// Does this mode monitor the given episode? **No mode monitors specials**
    /// (season 0) — they are opt-in per series via the season toggle
    /// (SKADI-T-0389). `air_date == None` is treated as not-yet-aired (un-dated
    /// episodes are usually upcoming).
    #[must_use]
    pub fn monitors(
        self,
        season: u16,
        number: u16,
        air_date: Option<NaiveDate>,
        today: NaiveDate,
        first_season: u16,
        last_season: u16,
    ) -> bool {
        let aired = air_date.is_some_and(|d| d <= today);
        let is_special = season == 0;
        match self {
            MonitorMode::None => false,
            MonitorMode::All => !is_special,
            MonitorMode::Future => !aired && !is_special,
            // At add-time we have no files, so "missing" = aired (worth grabbing)
            // and "existing" = nothing yet.
            MonitorMode::Missing => aired && !is_special,
            MonitorMode::Existing => false,
            MonitorMode::FirstSeason => !is_special && season == first_season,
            MonitorMode::LastSeason => !is_special && season == last_season,
            MonitorMode::Pilot => !is_special && season == first_season && number == 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn round_trips_through_str() {
        for m in [
            MonitorMode::All,
            MonitorMode::Future,
            MonitorMode::Missing,
            MonitorMode::Existing,
            MonitorMode::FirstSeason,
            MonitorMode::LastSeason,
            MonitorMode::Pilot,
            MonitorMode::None,
        ] {
            assert_eq!(MonitorMode::from_str_lossy(m.as_str()), m);
        }
        assert_eq!(MonitorMode::from_str_lossy("garbage"), MonitorMode::All);
    }

    #[test]
    fn selection_matrix() {
        let today = d(2026, 6, 21);
        let aired = Some(d(2011, 4, 17));
        let future = Some(d(2099, 1, 1));
        // (first_season, last_season) = (1, 8)
        let m = |mode: MonitorMode, season, number, date| {
            mode.monitors(season, number, date, today, 1, 8)
        };

        // all: every regular episode, aired or not — never specials (T-0389)
        assert!(m(MonitorMode::All, 1, 1, aired));
        assert!(m(MonitorMode::All, 5, 3, future));
        assert!(!m(MonitorMode::All, 0, 1, aired), "specials are opt-in");
        // none: nothing
        assert!(!m(MonitorMode::None, 1, 1, aired));
        // future: only un-aired (and un-dated)
        assert!(m(MonitorMode::Future, 8, 1, future));
        assert!(m(MonitorMode::Future, 8, 2, None));
        assert!(!m(MonitorMode::Future, 1, 1, aired));
        assert!(!m(MonitorMode::Future, 0, 9, future), "specials are opt-in");
        // missing: aired non-specials
        assert!(m(MonitorMode::Missing, 1, 1, aired));
        assert!(!m(MonitorMode::Missing, 0, 1, aired), "specials excluded");
        assert!(!m(MonitorMode::Missing, 8, 1, future), "un-aired excluded");
        // existing: nothing at add-time
        assert!(!m(MonitorMode::Existing, 1, 1, aired));
        // first/last season
        assert!(m(MonitorMode::FirstSeason, 1, 4, aired));
        assert!(!m(MonitorMode::FirstSeason, 2, 1, aired));
        assert!(m(MonitorMode::LastSeason, 8, 1, future));
        assert!(!m(MonitorMode::LastSeason, 7, 1, aired));
        // pilot: S01E01 only
        assert!(m(MonitorMode::Pilot, 1, 1, aired));
        assert!(!m(MonitorMode::Pilot, 1, 2, aired));
    }
}
