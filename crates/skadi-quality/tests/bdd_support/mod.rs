//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
pub mod steps;

use skadi_core::QualityId;
use skadi_quality::{
    CustomFormat, CustomFormatScore, Decision, ParsedRelease, QualityDefinition, QualityProfile,
};

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// The release title under test and its parse.
    pub title: String,
    pub parsed: Option<ParsedRelease>,
    /// Quality engine fixtures.
    pub definitions: Vec<QualityDefinition>,
    pub profile: Option<QualityProfile>,
    pub formats: Vec<CustomFormat>,
    pub scores: Vec<CustomFormatScore>,
    pub current: Option<QualityId>,
    pub decision: Option<Decision>,
    pub classified: Option<QualityId>,
    pub breakdown: Option<skadi_quality::ScoreBreakdown>,
    pub relevance: Option<skadi_quality::TitleRelevance>,
    pub corpus_report: Option<skadi_quality::Report>,
}
