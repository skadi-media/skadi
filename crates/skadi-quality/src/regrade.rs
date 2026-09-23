//! Re-grading library rows whose stored quality was never really assessed
//! (SKADI-T-0399).
//!
//! Adoption used to record `default_definitions()[0]` (SDTV) whenever a file's
//! name yielded no quality, so on a real library almost every imported row read
//! as "below cutoff" and the upgrade sweep tried to re-fetch the whole catalog.
//! Import now records [`crate::UNKNOWN_QUALITY_ID`] instead, but rows written
//! before that fix still carry the floor tier. [`regrade`] decides what such a
//! row should hold, from the only evidence available after the fact: the library
//! file's own name (plus a probed resolution when one was stored).
//!
//! Deliberately conservative — a row is only ever changed when it currently
//! holds the floor tier or Unknown, so a genuine SDTV import that was really
//! graded keeps its quality unless its name says otherwise.

use skadi_core::QualityId;

use crate::parsed::ParsedRelease;
use crate::parser::{parse, reconcile_with_probe, to_quality};
use crate::quality::{QualityDefinition, default_definitions};
use crate::{UNKNOWN_QUALITY_ID, is_unknown_quality};

/// What [`regrade`] decided for one row.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Regrade {
    /// The row holds a quality that was genuinely assessed — leave it alone.
    Keep,
    /// The name (and probe) yield a real tier: store this id.
    Graded(QualityId),
    /// Nothing could be derived: store [`UNKNOWN_QUALITY_ID`] so the upgrade
    /// sweep skips the row instead of treating it as the lowest tier.
    Unknown,
}

/// `true` when `id` is the tier adoption used to fall back to (the ladder's
/// first row, historically `SDTV`) — the fingerprint of a never-assessed row.
#[must_use]
pub fn is_legacy_floor(id: QualityId, definitions: &[QualityDefinition]) -> bool {
    definitions.first().is_some_and(|d| d.id == id)
}

/// Decide the quality a stored row should hold.
///
/// `current` is the row's stored quality, `file_name` the library file's name
/// (not its full path), `probed_resolution` an optional probed tier token such
/// as `"1080p"`. Rows holding anything other than the legacy floor or Unknown
/// are kept as they are.
#[must_use]
pub fn regrade(
    current: QualityId,
    file_name: &str,
    probed_resolution: Option<&str>,
    definitions: &[QualityDefinition],
) -> Regrade {
    if !is_unknown_quality(current) && !is_legacy_floor(current, definitions) {
        return Regrade::Keep;
    }
    let parsed: ParsedRelease = parse(file_name);
    let parsed = probed_resolution.map_or(parsed.clone(), |r| {
        reconcile_with_probe(&parsed, Some(r), None)
    });
    match to_quality(&parsed, definitions) {
        Some(q) if !is_unknown_quality(q.id) => Regrade::Graded(q.id),
        _ => Regrade::Unknown,
    }
}

/// Convenience wrapper over [`regrade`] using the built-in ladder, returning the
/// id to store, or `None` when the row should be left alone.
#[must_use]
pub fn regraded_id(
    current: QualityId,
    file_name: &str,
    probed_resolution: Option<&str>,
) -> Option<QualityId> {
    match regrade(
        current,
        file_name,
        probed_resolution,
        &default_definitions(),
    ) {
        Regrade::Keep => None,
        Regrade::Graded(id) => Some(id),
        Regrade::Unknown => Some(UNKNOWN_QUALITY_ID),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> Vec<QualityDefinition> {
        default_definitions()
    }

    #[test]
    fn a_floor_row_whose_name_carries_quality_is_regraded() {
        let floor = defs()[0].id;
        let got = regrade(
            floor,
            "The Matrix (1999) 1080p BluRay x264-GRP.mkv",
            None,
            &defs(),
        );
        let expect = defs()
            .iter()
            .find(|d| d.name == "Bluray-1080p")
            .map(|d| d.id)
            .unwrap();
        assert_eq!(got, Regrade::Graded(expect));
    }

    #[test]
    fn a_floor_row_with_an_unparseable_name_becomes_unknown() {
        let floor = defs()[0].id;
        assert_eq!(regrade(floor, "movie.mkv", None, &defs()), Regrade::Unknown);
        assert_eq!(
            regraded_id(floor, "movie.mkv", None),
            Some(UNKNOWN_QUALITY_ID)
        );
    }

    #[test]
    fn a_probe_grades_a_row_whose_name_has_only_a_source() {
        let floor = defs()[0].id;
        let got = regrade(floor, "Some.Movie.BluRay.mkv", Some("2160p"), &defs());
        let expect = defs()
            .iter()
            .find(|d| d.name == "Bluray-2160p")
            .map(|d| d.id)
            .unwrap();
        assert_eq!(got, Regrade::Graded(expect));
    }

    #[test]
    fn a_genuinely_graded_row_is_left_alone() {
        let bluray = defs()
            .iter()
            .find(|d| d.name == "Bluray-1080p")
            .map(|d| d.id)
            .unwrap();
        assert_eq!(
            regrade(bluray, "whatever.mkv", None, &defs()),
            Regrade::Keep
        );
        assert_eq!(regraded_id(bluray, "whatever.mkv", None), None);
    }

    #[test]
    fn an_unknown_row_can_still_be_graded_later() {
        let got = regrade(
            UNKNOWN_QUALITY_ID,
            "Show.S01E01.720p.HDTV.x264-GRP.mkv",
            None,
            &defs(),
        );
        let expect = defs()
            .iter()
            .find(|d| d.name == "HDTV-720p")
            .map(|d| d.id)
            .unwrap();
        assert_eq!(got, Regrade::Graded(expect));
    }
}
