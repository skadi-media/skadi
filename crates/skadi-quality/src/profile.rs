//! Quality profiles and the accept/reject/upgrade/cutoff decision.
//!
//! A [`QualityProfile`] is a user's policy over qualities: which are allowed (in
//! preference order), where the upgrade cutoff sits, whether upgrades are
//! permitted, and the custom-format scoring floor.

use serde::{Deserialize, Serialize};
use skadi_core::{ProfileId, QualityId};

use crate::format::CustomFormatScore;
use crate::quality::{Quality, QualityDefinition, Resolution};

/// The outcome of evaluating a candidate quality against a profile.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Not allowed by the profile, or not an improvement.
    Reject,
    /// Acceptable as a (first) acquisition.
    Accept,
    /// Strictly better than what we have and upgrades are allowed.
    Upgrade,
    /// What we already have meets/exceeds the cutoff — stop upgrading.
    MeetsCutoff,
}

/// A user's accept/reject/cutoff policy over qualities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityProfile {
    pub id: ProfileId,
    pub name: String,
    /// Allowed quality definitions, ordered low → high (index = rank).
    pub allowed: Vec<QualityId>,
    /// At/above this quality, no further upgrades are pursued.
    pub cutoff: QualityId,
    pub upgrade_allowed: bool,
    /// Per-custom-format score contributions.
    pub formats: Vec<CustomFormatScore>,
    /// A release must reach this aggregate custom-format score to be accepted.
    pub min_format_score: i32,
}

/// The recommended out-of-the-box profile (operator-approved, SKADI-I-0012):
/// every 720p/1080p/2160p definition allowed ranked low→high (SD excluded),
/// cutoff at **Bluray-1080p**, upgrades on, no custom-format floor. Used as
/// the daemon's no-config fallback and to fill sparse `profiles` settings
/// rows at write time.
#[must_use]
pub fn standard_profile(defs: &[QualityDefinition]) -> QualityProfile {
    let allowed: Vec<QualityId> = defs
        .iter()
        .filter(|d| d.resolution >= Resolution::R720p)
        .map(|d| d.id)
        .collect();
    let cutoff = defs
        .iter()
        .find(|d| d.name == "Bluray-1080p")
        .map(|d| d.id)
        .or_else(|| allowed.last().copied())
        .expect("built-in quality definitions are non-empty");
    QualityProfile {
        id: ProfileId::new(),
        name: "Standard".into(),
        allowed,
        cutoff,
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    }
}

/// The built-in set of quality profiles a fresh install ships with, mirroring
/// the *arr convention: **Any / SD / HD-720p / HD-1080p / HD-720p/1080p /
/// Ultra-HD** (SKADI-T-0145). Each `allowed` list is ordered low→high (the
/// definition order) and the cutoff is that band's Bluray tier (or its top).
/// Upgrades on, no custom-format floor. Seeded once into the `profiles` settings
/// on first boot — operators can edit or delete them afterward.
#[must_use]
pub fn default_profiles(defs: &[QualityDefinition]) -> Vec<QualityProfile> {
    let build = |name: &str, keep: &dyn Fn(Resolution) -> bool, cutoff_name: &str| {
        let allowed: Vec<QualityId> = defs
            .iter()
            .filter(|d| keep(d.resolution))
            .map(|d| d.id)
            .collect();
        let cutoff = defs
            .iter()
            .find(|d| d.name == cutoff_name)
            .map(|d| d.id)
            .or_else(|| allowed.last().copied())
            .expect("built-in quality definitions are non-empty");
        QualityProfile {
            id: ProfileId::new(),
            name: name.into(),
            allowed,
            cutoff,
            upgrade_allowed: true,
            formats: vec![],
            min_format_score: 0,
        }
    };
    vec![
        build("Any", &|_| true, "Bluray-2160p"),
        build("SD", &|r| r <= Resolution::R576p, "DVD"),
        build("HD-720p", &|r| r == Resolution::R720p, "Bluray-720p"),
        build("HD-1080p", &|r| r == Resolution::R1080p, "Bluray-1080p"),
        build(
            "HD-720p/1080p",
            &|r| r == Resolution::R720p || r == Resolution::R1080p,
            "Bluray-1080p",
        ),
        build("Ultra-HD", &|r| r == Resolution::R2160p, "Bluray-2160p"),
    ]
}

impl QualityProfile {
    /// Rank of a quality within `allowed` (lower = worse), if allowed at all.
    fn rank(&self, id: QualityId) -> Option<usize> {
        self.allowed.iter().position(|q| *q == id)
    }

    /// Decide what to do with a `candidate` quality given the `current` one (if
    /// the acquirable is already imported). Pure — no I/O. Thin wrapper over the
    /// axis-agnostic [`decide_id`](Self::decide_id).
    #[must_use]
    pub fn decide(&self, candidate: &Quality, current: Option<&Quality>) -> Decision {
        self.decide_id(candidate.id, current.map(|q| q.id))
    }

    /// The id-based core of [`decide`](Self::decide): rank/cutoff/upgrade logic
    /// over bare [`QualityId`]s. This is **axis-agnostic** — it works for movie
    /// or audiobook qualities alike (the audiobook scoring path calls this
    /// directly, since audiobook qualities have no movie `Quality` fields).
    /// SKADI-I-0017.
    #[must_use]
    pub fn decide_id(&self, candidate: QualityId, current: Option<QualityId>) -> Decision {
        let Some(cand_rank) = self.rank(candidate) else {
            return Decision::Reject;
        };
        let cand_rank = cand_rank as isize;

        match current {
            None => Decision::Accept,
            Some(cur) => {
                // An unknown current quality ranks below everything allowed.
                let cur_rank = self.rank(cur).map_or(-1, |r| r as isize);
                let cutoff_rank = self.rank(self.cutoff).map_or(isize::MAX, |r| r as isize);

                if cur_rank >= cutoff_rank {
                    Decision::MeetsCutoff
                } else if self.upgrade_allowed && cand_rank > cur_rank {
                    Decision::Upgrade
                } else {
                    Decision::Reject
                }
            }
        }
    }

    /// Whether an aggregate custom-format `score` clears the profile floor.
    #[must_use]
    pub fn accepts_format_score(&self, score: i32) -> bool {
        score >= self.min_format_score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::{Quality, Resolution, Source, default_definitions};

    #[test]
    fn default_profiles_are_the_arr_style_set() {
        let defs = default_definitions();
        let profiles = default_profiles(&defs);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Any",
                "SD",
                "HD-720p",
                "HD-1080p",
                "HD-720p/1080p",
                "Ultra-HD"
            ]
        );

        let by_name = |n: &str| profiles.iter().find(|p| p.name == n).unwrap();
        let name_of = |id: QualityId| defs.iter().find(|d| d.id == id).unwrap().name.clone();

        // Each band allows the right resolutions, low→high, with a sensible cutoff.
        assert_eq!(by_name("Any").allowed.len(), defs.len());
        assert_eq!(name_of(by_name("Any").cutoff), "Bluray-2160p");

        let sd = by_name("SD");
        assert_eq!(
            sd.allowed.iter().map(|id| name_of(*id)).collect::<Vec<_>>(),
            vec!["SDTV", "DVD"]
        );
        assert_eq!(name_of(sd.cutoff), "DVD");

        assert_eq!(name_of(by_name("HD-720p").cutoff), "Bluray-720p");
        assert_eq!(name_of(by_name("HD-1080p").cutoff), "Bluray-1080p");
        assert_eq!(by_name("HD-720p/1080p").allowed.len(), 8); // 4×720p + 4×1080p
        assert_eq!(name_of(by_name("Ultra-HD").cutoff), "Bluray-2160p");

        // Every profile is usable: non-empty, upgrades on, cutoff is allowed.
        for p in &profiles {
            assert!(!p.allowed.is_empty(), "{} empty", p.name);
            assert!(p.upgrade_allowed);
            assert!(
                p.allowed.contains(&p.cutoff),
                "{} cutoff not allowed",
                p.name
            );
        }
    }

    // Build a profile over three ranks from the default definitions:
    // HDTV-720p < Bluray-1080p < Bluray-2160p, cutoff at Bluray-1080p.
    fn defs() -> (QualityId, QualityId, QualityId) {
        let d = default_definitions();
        let find = |name: &str| d.iter().find(|q| q.name == name).unwrap().id;
        (
            find("HDTV-720p"),
            find("Bluray-1080p"),
            find("Bluray-2160p"),
        )
    }

    fn profile() -> (QualityProfile, QualityId, QualityId, QualityId) {
        let (low, mid, high) = defs();
        let p = QualityProfile {
            id: ProfileId::new(),
            name: "HD".into(),
            allowed: vec![low, mid, high],
            cutoff: mid,
            upgrade_allowed: true,
            formats: vec![],
            min_format_score: 0,
        };
        (p, low, mid, high)
    }

    fn quality(id: QualityId) -> Quality {
        Quality {
            id,
            resolution: Resolution::R1080p,
            source: Source::Bluray,
            codec: None,
            modifier: None,
        }
    }

    #[test]
    fn rejects_not_allowed() {
        let (p, ..) = profile();
        let stranger = quality(QualityId::new());
        assert_eq!(p.decide(&stranger, None), Decision::Reject);
    }

    #[test]
    fn accepts_first_acquisition() {
        let (p, low, ..) = profile();
        assert_eq!(p.decide(&quality(low), None), Decision::Accept);
    }

    #[test]
    fn upgrades_below_cutoff() {
        let (p, low, mid, _high) = profile();
        // current = low, candidate = mid → upgrade.
        assert_eq!(
            p.decide(&quality(mid), Some(&quality(low))),
            Decision::Upgrade
        );
    }

    #[test]
    fn does_not_downgrade() {
        let (p, low, mid, _high) = profile();
        assert_eq!(
            p.decide(&quality(low), Some(&quality(mid))),
            Decision::MeetsCutoff
        );
    }

    #[test]
    fn stops_at_cutoff() {
        let (p, _low, mid, high) = profile();
        // current = mid (the cutoff) → already satisfied, even a higher candidate.
        assert_eq!(
            p.decide(&quality(high), Some(&quality(mid))),
            Decision::MeetsCutoff
        );
    }

    #[test]
    fn respects_upgrade_disabled() {
        let (mut p, low, mid, _high) = profile();
        p.upgrade_allowed = false;
        assert_eq!(
            p.decide(&quality(mid), Some(&quality(low))),
            Decision::Reject
        );
    }

    #[test]
    fn format_score_floor() {
        let (mut p, ..) = profile();
        p.min_format_score = 50;
        assert!(!p.accepts_format_score(49));
        assert!(p.accepts_format_score(50));
    }

    #[test]
    fn standard_profile_is_720p_up_with_bluray_1080p_cutoff_and_upgrades() {
        let defs = crate::quality::default_definitions();
        let std = standard_profile(&defs);
        // SD definitions excluded; everything 720p+ allowed in rank order.
        assert!(std.allowed.iter().all(|id| {
            defs.iter()
                .any(|d| d.id == *id && d.resolution >= crate::quality::Resolution::R720p)
        }));
        assert_eq!(
            std.allowed.len(),
            defs.iter()
                .filter(|d| d.resolution >= crate::quality::Resolution::R720p)
                .count()
        );
        let cutoff_def = defs.iter().find(|d| d.id == std.cutoff).unwrap();
        assert_eq!(cutoff_def.name, "Bluray-1080p");
        assert!(std.upgrade_allowed);
        assert_eq!(std.min_format_score, 0);
        // A 2160p candidate over a 1080p current is an upgrade; over 2160p we stop.
        let q = |name: &str| {
            let d = defs.iter().find(|d| d.name == name).unwrap();
            quality_from_def(d)
        };
        assert_eq!(
            std.decide(&q("Bluray-2160p"), Some(&q("WEBDL-1080p"))),
            Decision::Upgrade
        );
        assert_eq!(
            std.decide(&q("Bluray-2160p"), Some(&q("Bluray-1080p"))),
            Decision::MeetsCutoff
        );
        // SDTV is rejected outright.
        let sd = defs.iter().find(|d| d.name == "SDTV").unwrap();
        assert_eq!(std.decide(&quality_from_def(sd), None), Decision::Reject);
    }

    fn quality_from_def(d: &QualityDefinition) -> Quality {
        Quality {
            id: d.id,
            resolution: d.resolution,
            source: d.source,
            codec: None,
            modifier: None,
        }
    }
}
