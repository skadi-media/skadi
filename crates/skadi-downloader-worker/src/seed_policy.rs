//! Seeding policy: ratio + time limits with a stop/remove action (SKADI-T-0209).
//!
//! A torrent client conventionally stops (or removes) a torrent once it reaches
//! a configured share ratio or has seeded for long enough. This is the **pure decision** half of that:
//! given a torrent's uploaded/downloaded bytes and how long it's been seeding, plus
//! the configured limits, decide whether to keep seeding, stop, or remove. The
//! worker computes the inputs from librqbit stats + the row's `completed_at` and
//! acts on the verdict (SKADI-T-0210); keeping the logic pure makes the
//! ratio/time/edge-case behaviour unit-testable without a live session.

/// What to do when a seed limit is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedAction {
    /// Stop seeding; the torrent is terminal but its data is kept.
    Stop,
    /// Stop seeding **and** remove the torrent (data kept — import already
    /// hardlinked it; the seed copy is what's released).
    Remove,
}

impl SeedAction {
    /// Parse the `worker.seed_action` config value (default `Stop` for anything
    /// other than `remove`).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("remove") {
            SeedAction::Remove
        } else {
            SeedAction::Stop
        }
    }
}

/// The configured seeding limits. `None` on an axis ⇒ unlimited on that axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeedPolicy {
    /// Stop once `uploaded / downloaded >= ratio_limit`.
    pub ratio_limit: Option<f64>,
    /// Stop once the torrent has been seeding for at least this long.
    pub time_limit_secs: Option<i64>,
    pub action: SeedAction,
}

/// The decision for one seeding torrent on one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedVerdict {
    Continue,
    Stop,
    Remove,
}

impl SeedPolicy {
    /// No limits — seed forever (the pre-T-0209 behaviour).
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            ratio_limit: None,
            time_limit_secs: None,
            action: SeedAction::Stop,
        }
    }

    /// Build from raw config values: `ratio <= 0` and `time_mins == 0` mean
    /// "unlimited" on their respective axes (the config defaults).
    #[must_use]
    pub fn from_config(ratio: f64, time_mins: u64, action: SeedAction) -> Self {
        Self {
            ratio_limit: (ratio > 0.0).then_some(ratio),
            time_limit_secs: (time_mins > 0).then(|| time_mins as i64 * 60),
            action,
        }
    }

    /// Whether both axes are unlimited (the worker can skip the seeding check).
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.ratio_limit.is_none() && self.time_limit_secs.is_none()
    }

    /// The share ratio, or `None` when nothing was downloaded (avoids div-by-zero
    /// and a meaningless ratio on a zero-byte job).
    #[must_use]
    pub fn ratio(uploaded_bytes: i64, downloaded_bytes: i64) -> Option<f64> {
        (downloaded_bytes > 0).then(|| uploaded_bytes as f64 / downloaded_bytes as f64)
    }

    /// Decide what to do with a torrent that has uploaded `uploaded_bytes` against
    /// `downloaded_bytes` and has been seeding `seeded_secs`.
    #[must_use]
    pub fn verdict(
        &self,
        uploaded_bytes: i64,
        downloaded_bytes: i64,
        seeded_secs: i64,
    ) -> SeedVerdict {
        let ratio_hit = match (
            self.ratio_limit,
            Self::ratio(uploaded_bytes, downloaded_bytes),
        ) {
            (Some(limit), Some(r)) => r >= limit,
            _ => false,
        };
        let time_hit = matches!(self.time_limit_secs, Some(limit) if seeded_secs >= limit);
        if ratio_hit || time_hit {
            match self.action {
                SeedAction::Stop => SeedVerdict::Stop,
                SeedAction::Remove => SeedVerdict::Remove,
            }
        } else {
            SeedVerdict::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_parse_defaults_to_stop() {
        assert_eq!(SeedAction::parse("remove"), SeedAction::Remove);
        assert_eq!(SeedAction::parse("REMOVE"), SeedAction::Remove);
        assert_eq!(SeedAction::parse("stop"), SeedAction::Stop);
        assert_eq!(SeedAction::parse("garbage"), SeedAction::Stop);
        assert_eq!(SeedAction::parse(""), SeedAction::Stop);
    }

    #[test]
    fn from_config_treats_zero_as_unlimited() {
        let p = SeedPolicy::from_config(0.0, 0, SeedAction::Stop);
        assert!(p.is_unlimited());
        let p = SeedPolicy::from_config(1.5, 30, SeedAction::Remove);
        assert_eq!(p.ratio_limit, Some(1.5));
        assert_eq!(p.time_limit_secs, Some(1800)); // 30 min
        assert!(!p.is_unlimited());
    }

    #[test]
    fn unlimited_always_continues() {
        let p = SeedPolicy::unlimited();
        assert_eq!(p.verdict(1_000_000, 1, 999_999), SeedVerdict::Continue);
    }

    #[test]
    fn ratio_limit_triggers_at_or_above() {
        let p = SeedPolicy::from_config(2.0, 0, SeedAction::Stop);
        // 1.9x — keep seeding.
        assert_eq!(p.verdict(190, 100, 0), SeedVerdict::Continue);
        // exactly 2.0x — stop.
        assert_eq!(p.verdict(200, 100, 0), SeedVerdict::Stop);
        // 3x — stop.
        assert_eq!(p.verdict(300, 100, 0), SeedVerdict::Stop);
    }

    #[test]
    fn time_limit_triggers_at_or_after() {
        let p = SeedPolicy::from_config(0.0, 10, SeedAction::Stop); // 600s
        assert_eq!(p.verdict(0, 100, 599), SeedVerdict::Continue);
        assert_eq!(p.verdict(0, 100, 600), SeedVerdict::Stop);
    }

    #[test]
    fn either_axis_triggers_and_action_maps() {
        // Ratio met but time not → Remove (action).
        let p = SeedPolicy::from_config(1.0, 10, SeedAction::Remove);
        assert_eq!(p.verdict(100, 100, 0), SeedVerdict::Remove);
        // Time met but ratio not → Remove.
        assert_eq!(p.verdict(0, 100, 9999), SeedVerdict::Remove);
    }

    #[test]
    fn zero_downloaded_never_trips_ratio() {
        // A 0-byte download must not produce an infinite/NaN ratio that trips.
        let p = SeedPolicy::from_config(1.0, 0, SeedAction::Stop);
        assert_eq!(p.verdict(500, 0, 0), SeedVerdict::Continue);
        assert_eq!(SeedPolicy::ratio(500, 0), None);
    }
}
