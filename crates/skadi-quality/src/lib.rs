//! `skadi-quality` — the quality engine.
//!
//! Houses the release-title parser, the quality taxonomy, quality profiles, and
//! custom-format scoring. The release-title parser and the custom-format
//! scoring share this crate because the parsed result ([`ParsedRelease`]) is
//! what drives custom-format matching.
//!
//! This is clean-room work: patterns and reasoning are re-derived from
//! observable release-naming conventions, never copied from GPL `*arr` source.
//! The [`corpus`] is the regression contract the parser must satisfy.

pub mod audiobook;
pub mod corpus;
pub mod format;
pub mod music;
pub mod parsed;
pub mod parser;
pub mod profile;
pub mod quality;
pub mod regrade;
pub mod relevance;

pub use audiobook::{
    Abridgement, AudioBitrate, AudioFormat, AudiobookQualityDefinition,
    default_audiobook_definitions, reconcile_quality_with_format, to_audiobook_quality,
    unknown_audiobook_id,
};
pub use corpus::{CorpusEntry, Report};
pub use format::{
    CustomFormat, CustomFormatScore, FormatMode, FormatRule, MatchedFormat, ReleaseMeta,
    ScoreBreakdown, aggregate_score, default_formats, score_breakdown,
};
pub use parsed::ParsedRelease;
pub use parser::{
    parse, parse_audiobook, parse_tv, quality_from_probe, reconcile_with_probe, to_quality,
};
pub use profile::{Decision, QualityProfile, default_profiles, standard_profile};
pub use quality::{
    Codec, Modifier, Quality, QualityDefinition, Resolution, Source, UNKNOWN_QUALITY_ID,
    default_definitions, is_unknown_quality, max_mb_per_minute, max_size_bytes, unknown_definition,
};
pub use regrade::{Regrade, is_legacy_floor, regrade, regraded_id};
pub use relevance::{TitleRelevance, title_relevance};
