//! Custom formats: tag-based scoring rules over a parsed release.
//!
//! A [`CustomFormat`] is a named set of [`FormatRule`]s; a release that matches
//! all of a format's rules earns the score a [`QualityProfile`] assigns that
//! format. The aggregate score then gates acceptance via the profile's
//! `min_format_score`. Re-derived clean-room.

use regex::Regex;
use serde::{Deserialize, Serialize};
use skadi_core::CustomFormatId;
use uuid::Uuid;

use crate::parsed::ParsedRelease;

/// How a profile gates a custom format (SKADI-T-0185, ADR [[SKADI-A-0003]]).
/// Folds the legacy "release profile" must-contain / must-not into custom formats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatMode {
    /// Score only: a matching format contributes its score (the default — the
    /// pre-T-0185 behavior).
    #[default]
    Preferred,
    /// Hard must-contain: a release that does **not** match this format is
    /// rejected, independent of the score floor.
    Required,
    /// Hard must-not: a release that **matches** this format is rejected,
    /// independent of the score floor.
    Ignored,
}

/// A profile's contribution for a given custom format: a matching `Preferred`
/// (or `Required`) format adds `score` to the aggregate; `Required`/`Ignored`
/// additionally act as hard gates (SKADI-T-0185).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFormatScore {
    pub format: CustomFormatId,
    pub score: i32,
    /// How the profile gates this format. `#[serde(default)]` = [`Preferred`],
    /// so profiles stored before T-0185 deserialize unchanged.
    #[serde(default)]
    pub mode: FormatMode,
}

/// Metadata about a release beyond its parsed title, used by rules that look
/// past the name (size, indexer flags).
#[derive(Clone, Debug, Default)]
pub struct ReleaseMeta<'a> {
    pub size_bytes: Option<u64>,
    pub indexer_flags: &'a [String],
}

/// One matchable condition within a [`CustomFormat`].
///
/// Rules fall in three families: over the **raw title** ([`TitleRegex`](Self::TitleRegex)),
/// over **release metadata** ([`SizeBetween`](Self::SizeBetween) / [`IndexerFlag`](Self::IndexerFlag)),
/// and over the **parsed fields** ([`Resolution`](Self::Resolution) /
/// [`Source`](Self::Source) / [`Codec`](Self::Codec) / [`Edition`](Self::Edition) /
/// [`Language`](Self::Language), SKADI-T-0183). The parsed-field rules read the
/// already-decomposed [`ParsedRelease`] so a format can target e.g. "source is WEB-DL"
/// without each user re-authoring a brittle title regex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatRule {
    /// Case-insensitive regex over the raw release title.
    TitleRegex(String),
    /// Release size within an inclusive byte range (either bound optional).
    SizeBetween { min: Option<u64>, max: Option<u64> },
    /// An indexer flag is present (case-insensitive), e.g. "freeleech".
    IndexerFlag(String),
    /// `ParsedRelease.resolution` equals this token, case-insensitively
    /// (e.g. "1080p", "2160p"). Tokens are normalized by the parser, so equality
    /// (not substring) is the right test.
    Resolution(String),
    /// `ParsedRelease.source` equals this token, case-insensitively
    /// (e.g. "BluRay", "WEB-DL", "HDTV").
    Source(String),
    /// `ParsedRelease.codec` equals this token, case-insensitively
    /// (e.g. "x265", "x264", "AV1").
    Codec(String),
    /// `ParsedRelease.edition` **contains** this phrase, case-insensitively
    /// (e.g. "Director's Cut", "Extended"). Substring because editions are
    /// free-form phrases, not single normalized tokens.
    Edition(String),
    /// Any of `ParsedRelease.languages` equals this token, case-insensitively
    /// (e.g. "English", "Japanese").
    Language(String),
}

impl FormatRule {
    /// Whether this rule matches the given release.
    #[must_use]
    pub fn matches(&self, title: &str, parsed: &ParsedRelease, meta: &ReleaseMeta) -> bool {
        match self {
            FormatRule::TitleRegex(pattern) => Regex::new(&format!("(?i){pattern}"))
                .map(|re| re.is_match(title))
                .unwrap_or(false),
            FormatRule::SizeBetween { min, max } => match meta.size_bytes {
                Some(size) => min.is_none_or(|m| size >= m) && max.is_none_or(|m| size <= m),
                None => false,
            },
            FormatRule::IndexerFlag(flag) => meta
                .indexer_flags
                .iter()
                .any(|f| f.eq_ignore_ascii_case(flag)),
            FormatRule::Resolution(v) => eq_ci(parsed.resolution.as_deref(), v),
            FormatRule::Source(v) => eq_ci(parsed.source.as_deref(), v),
            FormatRule::Codec(v) => eq_ci(parsed.codec.as_deref(), v),
            FormatRule::Edition(v) => parsed
                .edition
                .as_deref()
                .is_some_and(|e| e.to_lowercase().contains(&v.to_lowercase())),
            FormatRule::Language(v) => parsed.languages.iter().any(|l| l.eq_ignore_ascii_case(v)),
        }
    }

    /// Validate the rule is well-formed, for write-time settings validation
    /// (SKADI-T-0183). Currently only [`TitleRegex`](Self::TitleRegex) can be
    /// malformed (an uncompilable pattern); the rest are always valid. Returns a
    /// human-readable reason on failure so the API can 422 with it.
    pub fn validate(&self) -> Result<(), String> {
        if let FormatRule::TitleRegex(pattern) = self {
            Regex::new(&format!("(?i){pattern}"))
                .map_err(|e| format!("invalid TitleRegex {pattern:?}: {e}"))?;
        }
        Ok(())
    }
}

/// Case-insensitive equality of an optional parsed token against a rule value.
fn eq_ci(field: Option<&str>, value: &str) -> bool {
    field.is_some_and(|f| f.eq_ignore_ascii_case(value))
}

/// A named, scorable custom format: matches when *all* its rules match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFormat {
    pub id: CustomFormatId,
    pub name: String,
    pub rules: Vec<FormatRule>,
}

impl CustomFormat {
    /// Matches when the format has at least one rule and every rule matches.
    #[must_use]
    pub fn matches(&self, title: &str, parsed: &ParsedRelease, meta: &ReleaseMeta) -> bool {
        !self.rules.is_empty() && self.rules.iter().all(|r| r.matches(title, parsed, meta))
    }
}

/// One custom format that matched a release, and the score it contributed —
/// surfaced by [`score_breakdown`] for the "explain a decision" view (SKADI-T-0184).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedFormat {
    pub format: CustomFormatId,
    pub name: String,
    pub score: i32,
}

/// The custom-format scoring breakdown for a release: the aggregate [`total`] and
/// every format that matched with its contributed score (SKADI-T-0184). The
/// single source of truth — [`aggregate_score`] is just `score_breakdown(..).total`.
#[derive(Clone, Debug, Default)]
pub struct ScoreBreakdown {
    pub total: i32,
    pub matched: Vec<MatchedFormat>,
    /// Names of `Required` formats that did **not** match — a hard must-contain
    /// violation (SKADI-T-0185). Non-empty ⇒ the release should be rejected.
    pub missing_required: Vec<String>,
    /// Names of `Ignored` formats that **did** match — a hard must-not violation.
    /// Non-empty ⇒ the release should be rejected.
    pub present_ignored: Vec<String>,
}

/// Compute the custom-format scoring breakdown for a release (SKADI-T-0184/0185):
/// for each profile-assigned format (looked up in `registry`), record whether it
/// matched and apply its [`mode`](FormatMode):
/// - `Preferred`/`Required` that match contribute `score` to `total` (and appear
///   in `matched`, preserving the profile's order);
/// - a `Required` that does **not** match is recorded in `missing_required`;
/// - an `Ignored` that **does** match is recorded in `present_ignored`.
///
/// A score referencing a format not in the registry (deleted) is skipped — a
/// stale reference can neither score nor gate.
#[must_use]
pub fn score_breakdown(
    scores: &[CustomFormatScore],
    registry: &[CustomFormat],
    title: &str,
    parsed: &ParsedRelease,
    meta: &ReleaseMeta,
) -> ScoreBreakdown {
    let mut b = ScoreBreakdown::default();
    for s in scores {
        let Some(f) = registry.iter().find(|f| f.id == s.format) else {
            continue; // dangling reference: cannot score or gate
        };
        let hit = f.matches(title, parsed, meta);
        // Preferred/Required that match contribute their score.
        let scores_now = hit && matches!(s.mode, FormatMode::Preferred | FormatMode::Required);
        if scores_now {
            b.total += s.score;
            b.matched.push(MatchedFormat {
                format: f.id,
                name: f.name.clone(),
                score: s.score,
            });
        }
        match s.mode {
            FormatMode::Required if !hit => b.missing_required.push(f.name.clone()),
            FormatMode::Ignored if hit => b.present_ignored.push(f.name.clone()),
            _ => {}
        }
    }
    b
}

/// Aggregate custom-format score for a release: the sum of the profile-assigned
/// scores for every format (looked up in `registry`) that matches. Thin wrapper
/// over [`score_breakdown`] for the hot path that only needs the total.
#[must_use]
pub fn aggregate_score(
    scores: &[CustomFormatScore],
    registry: &[CustomFormat],
    title: &str,
    parsed: &ParsedRelease,
    meta: &ReleaseMeta,
) -> i32 {
    score_breakdown(scores, registry, title, parsed, meta).total
}

fn fmt_id(n: u128) -> CustomFormatId {
    CustomFormatId(Uuid::from_u128(n))
}

/// The baseline default custom formats (deterministic ids), re-derived
/// clean-room. Intentionally small for v0; users add their own.
#[must_use]
pub fn default_formats() -> Vec<CustomFormat> {
    vec![
        CustomFormat {
            id: fmt_id(1),
            name: "Remux".into(),
            rules: vec![FormatRule::TitleRegex(r"\bremux\b".into())],
        },
        CustomFormat {
            id: fmt_id(2),
            name: "x265 / HEVC".into(),
            rules: vec![FormatRule::TitleRegex(r"\b(x265|h\.?265|hevc)\b".into())],
        },
        CustomFormat {
            id: fmt_id(3),
            name: "Repack/Proper".into(),
            rules: vec![FormatRule::TitleRegex(r"\b(repack|proper)\b".into())],
        },
        CustomFormat {
            id: fmt_id(4),
            name: "Freeleech".into(),
            rules: vec![FormatRule::IndexerFlag("freeleech".into())],
        },
        // --- direct-play audio (SKADI-T-0584) ---
        //
        // Definitions only; the SCORE is the operator's policy and lives in the
        // profile. They exist so nobody has to author these regexes by hand to
        // stop the library re-acquiring the same unplayable release.
        CustomFormat {
            id: fmt_id(5),
            // Android has no DTS decoder in any variant, and TrueHD/MLP are
            // lossless Dolby — a release advertising only these plays SILENTLY on
            // a phone. Score it negative to stop re-grabbing the problem.
            name: "Undecodable audio (DTS / TrueHD)".into(),
            rules: vec![FormatRule::TitleRegex(
                r"\b(dts(-?hd)?(\.?ma)?|truehd|mlp)\b".into(),
            )],
        },
        CustomFormat {
            id: fmt_id(6),
            // The counterpart: audio every device decodes. Score it positive so a
            // release that plays wins ties against one that might not.
            name: "Direct-play audio (AAC / AC3 / E-AC3)".into(),
            rules: vec![FormatRule::TitleRegex(
                r"\b(aac|ac-?3|e-?ac-?3|ddp?5?\.?1?|dd\+)\b".into(),
            )],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_parse() -> ParsedRelease {
        ParsedRelease::default()
    }

    #[test]
    fn title_regex_rule_matches_case_insensitively() {
        let rule = FormatRule::TitleRegex(r"\bremux\b".into());
        let meta = ReleaseMeta::default();
        assert!(rule.matches("Movie.2020.REMUX.HEVC-GRP", &empty_parse(), &meta));
        assert!(!rule.matches("Movie.2020.BluRay.x264-GRP", &empty_parse(), &meta));
    }

    #[test]
    fn size_rule_respects_bounds() {
        let rule = FormatRule::SizeBetween {
            min: Some(10),
            max: Some(20),
        };
        let within = ReleaseMeta {
            size_bytes: Some(15),
            indexer_flags: &[],
        };
        let over = ReleaseMeta {
            size_bytes: Some(25),
            indexer_flags: &[],
        };
        assert!(rule.matches("x", &empty_parse(), &within));
        assert!(!rule.matches("x", &empty_parse(), &over));
        // Unknown size never matches a size rule.
        assert!(!rule.matches("x", &empty_parse(), &ReleaseMeta::default()));
    }

    #[test]
    fn indexer_flag_rule() {
        let rule = FormatRule::IndexerFlag("freeleech".into());
        let flags = vec!["FreeLeech".to_string()];
        let meta = ReleaseMeta {
            size_bytes: None,
            indexer_flags: &flags,
        };
        assert!(rule.matches("x", &empty_parse(), &meta));
        assert!(!rule.matches("x", &empty_parse(), &ReleaseMeta::default()));
    }

    // --- parsed-field rules (SKADI-T-0183) ---

    fn parsed_video() -> ParsedRelease {
        ParsedRelease {
            resolution: Some("2160p".into()),
            source: Some("BluRay".into()),
            codec: Some("x265".into()),
            edition: Some("Director's Cut".into()),
            languages: vec!["English".into(), "Japanese".into()],
            ..Default::default()
        }
    }

    #[test]
    fn resolution_source_codec_rules_match_parsed_tokens_case_insensitively() {
        let p = parsed_video();
        let m = ReleaseMeta::default();
        assert!(FormatRule::Resolution("2160p".into()).matches("x", &p, &m));
        assert!(FormatRule::Resolution("2160P".into()).matches("x", &p, &m)); // case-insensitive
        assert!(!FormatRule::Resolution("1080p".into()).matches("x", &p, &m));
        assert!(FormatRule::Source("bluray".into()).matches("x", &p, &m));
        assert!(!FormatRule::Source("WEB-DL".into()).matches("x", &p, &m));
        assert!(FormatRule::Codec("X265".into()).matches("x", &p, &m));
        assert!(!FormatRule::Codec("x264".into()).matches("x", &p, &m));
        // Missing parsed field never matches.
        assert!(!FormatRule::Resolution("2160p".into()).matches("x", &empty_parse(), &m));
    }

    #[test]
    fn edition_rule_is_a_case_insensitive_substring() {
        let p = parsed_video();
        let m = ReleaseMeta::default();
        assert!(FormatRule::Edition("director".into()).matches("x", &p, &m));
        assert!(FormatRule::Edition("Director's Cut".into()).matches("x", &p, &m));
        assert!(!FormatRule::Edition("Extended".into()).matches("x", &p, &m));
    }

    #[test]
    fn language_rule_matches_any_listed_language() {
        let p = parsed_video();
        let m = ReleaseMeta::default();
        assert!(FormatRule::Language("english".into()).matches("x", &p, &m));
        assert!(FormatRule::Language("Japanese".into()).matches("x", &p, &m));
        assert!(!FormatRule::Language("French".into()).matches("x", &p, &m));
    }

    #[test]
    fn custom_format_requires_all_rules() {
        let f = CustomFormat {
            id: fmt_id(99),
            name: "1080p Remux".into(),
            rules: vec![
                FormatRule::TitleRegex(r"\bremux\b".into()),
                FormatRule::TitleRegex(r"1080p".into()),
            ],
        };
        let meta = ReleaseMeta::default();
        assert!(f.matches("X.2020.1080p.Remux-GRP", &empty_parse(), &meta));
        assert!(!f.matches("X.2020.2160p.Remux-GRP", &empty_parse(), &meta));
    }

    #[test]
    fn aggregate_subtracts_negative_scored_formats_as_a_penalty() {
        // The "negative-as-must-not" model (SKADI-S-0012): a format assigned a
        // negative score penalizes a matching release, so a profile floor can
        // reject it. Verify the sum is signed, not clamped.
        let registry = default_formats();
        let remux = registry[0].id; // TitleRegex \bremux\b
        let hevc = registry[1].id; // TitleRegex x265|hevc
        let scores = vec![
            CustomFormatScore {
                format: remux,
                score: -1000, // must-not
                mode: FormatMode::Preferred,
            },
            CustomFormatScore {
                format: hevc,
                score: 50,
                mode: FormatMode::Preferred,
            },
        ];
        let meta = ReleaseMeta::default();
        let s = |title: &str| aggregate_score(&scores, &registry, title, &empty_parse(), &meta);

        // remux + hevc → -1000 + 50 = -950 (penalty dominates).
        assert_eq!(s("M.2160p.Remux.HEVC-G"), -950);
        // hevc only → +50.
        assert_eq!(s("M.1080p.BluRay.HEVC-G"), 50);
        // remux only → -1000.
        assert_eq!(s("M.2160p.Remux.x264-G"), -1000);
    }

    #[test]
    fn score_breakdown_reports_required_absent_and_ignored_present() {
        // SKADI-T-0185: Required must be present, Ignored must be absent.
        let registry = default_formats(); // [Remux, "x265 / HEVC", "Repack/Proper", Freeleech]
        let hevc = registry[1].id;
        let remux = registry[0].id;
        let scores = vec![
            CustomFormatScore {
                format: hevc,
                score: 50,
                mode: FormatMode::Required,
            },
            CustomFormatScore {
                format: remux,
                score: 0,
                mode: FormatMode::Ignored,
            },
        ];
        let m = ReleaseMeta::default();
        let b = |title: &str| score_breakdown(&scores, &registry, title, &empty_parse(), &m);

        // HEVC, not remux → required satisfied (+50), ignored absent → no violation.
        let ok = b("M.1080p.BluRay.HEVC-G");
        assert_eq!(ok.total, 50);
        assert!(ok.missing_required.is_empty() && ok.present_ignored.is_empty());

        // No HEVC → required missing.
        let miss = b("M.1080p.BluRay.x264-G");
        assert_eq!(miss.missing_required, vec!["x265 / HEVC".to_string()]);
        assert!(miss.present_ignored.is_empty());

        // Remux present → ignored violation (and HEVC still missing).
        let ig = b("M.2160p.Remux-G");
        assert_eq!(ig.present_ignored, vec!["Remux".to_string()]);
        assert_eq!(ig.missing_required, vec!["x265 / HEVC".to_string()]);
    }

    #[test]
    fn aggregate_ignores_a_score_referencing_an_unknown_format() {
        // A profile score may reference a format id that no longer exists in the
        // registry (the format was deleted). It contributes nothing rather than
        // panicking — scoring stays robust to stale references.
        let registry = default_formats();
        let scores = vec![CustomFormatScore {
            format: fmt_id(9999), // not in the registry
            score: 500,
            mode: FormatMode::Preferred,
        }];
        let s = aggregate_score(
            &scores,
            &registry,
            "M.2020.1080p.Remux.HEVC-G",
            &empty_parse(),
            &ReleaseMeta::default(),
        );
        assert_eq!(s, 0, "a dangling format reference contributes 0");
    }

    #[test]
    fn empty_rules_format_never_matches() {
        // A format with no rules must not match everything (it would otherwise
        // score every release). `CustomFormat::matches` requires ≥1 rule.
        let f = CustomFormat {
            id: fmt_id(60),
            name: "empty".into(),
            rules: vec![],
        };
        assert!(!f.matches("anything at all", &empty_parse(), &ReleaseMeta::default()));
    }

    #[test]
    fn custom_format_ands_across_rule_kinds() {
        // A format matches only when *every* rule matches, even across rule
        // families (a parsed-field rule + another parsed-field rule).
        let f = CustomFormat {
            id: fmt_id(50),
            name: "1080p WEB-DL".into(),
            rules: vec![
                FormatRule::Resolution("1080p".into()),
                FormatRule::Source("WEB-DL".into()),
            ],
        };
        let meta = ReleaseMeta::default();
        let webdl_1080 = ParsedRelease {
            resolution: Some("1080p".into()),
            source: Some("WEB-DL".into()),
            ..Default::default()
        };
        let bluray_1080 = ParsedRelease {
            resolution: Some("1080p".into()),
            source: Some("BluRay".into()),
            ..Default::default()
        };
        assert!(f.matches("x", &webdl_1080, &meta), "both rules match");
        assert!(
            !f.matches("x", &bluray_1080, &meta),
            "source differs → the AND fails"
        );
    }

    #[test]
    fn aggregate_sums_only_matching_formats() {
        let registry = default_formats();
        let remux = registry[0].id;
        let hevc = registry[1].id;
        let scores = vec![
            CustomFormatScore {
                format: remux,
                score: 100,
                mode: FormatMode::Preferred,
            },
            CustomFormatScore {
                format: hevc,
                score: 25,
                mode: FormatMode::Preferred,
            },
        ];
        let meta = ReleaseMeta::default();

        // HEVC only → 25.
        let s = aggregate_score(
            &scores,
            &registry,
            "M.2020.1080p.BluRay.HEVC-G",
            &empty_parse(),
            &meta,
        );
        assert_eq!(s, 25);

        // Remux + HEVC → 125.
        let s = aggregate_score(
            &scores,
            &registry,
            "M.2020.2160p.Remux.HEVC-G",
            &empty_parse(),
            &meta,
        );
        assert_eq!(s, 125);

        // Neither → 0.
        let s = aggregate_score(
            &scores,
            &registry,
            "M.2020.1080p.BluRay.x264-G",
            &empty_parse(),
            &meta,
        );
        assert_eq!(s, 0);
    }
}
