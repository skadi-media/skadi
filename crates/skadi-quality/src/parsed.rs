//! The structured parse of a release title.
//!
//! [`ParsedRelease`] is what the release-title parser (SKADI-T-0013) produces
//! and what custom-format matching (SKADI-T-0015) consumes. For this scaffold
//! the quality-ish fields are free-form string hints; they will be enriched
//! into the typed [`crate::quality`] taxonomy (`Resolution`/`Source`/`Codec`/
//! `Modifier`) as the taxonomy (T-0012) and parser (T-0013) land. Keeping them
//! as strings now lets the corpus format and harness exist before either.

use serde::{Deserialize, Serialize};

/// A release title decomposed into its meaningful parts.
///
/// All fields are optional/empty by default so partial parses are representable
/// and the type has a meaningful [`Default`] (an empty parse).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParsedRelease {
    /// The cleaned work title (movie/series name) with separators normalized.
    pub title: Option<String>,
    /// Release year, when present.
    pub year: Option<u16>,
    /// Resolution token (e.g. "1080p", "2160p"); typed later via the taxonomy.
    pub resolution: Option<String>,
    /// Source token (e.g. "BluRay", "WEB-DL", "HDTV").
    pub source: Option<String>,
    /// Video codec token (e.g. "x264", "x265", "AV1").
    pub codec: Option<String>,
    /// Modifier tokens (e.g. "Remux", "Proper", "Repack").
    pub modifiers: Vec<String>,
    /// Release group (the trailing `-GROUP`), when present.
    pub group: Option<String>,
    /// Edition token (e.g. "Director's Cut", "Extended"), when present.
    pub edition: Option<String>,
    /// Detected language tokens.
    pub languages: Vec<String>,
    /// Audiobook container/codec token (e.g. "M4B", "MP3"), when present
    /// (SKADI-I-0017). Populated by the audiobook parse path; empty for video.
    pub audio_format: Option<String>,
    /// Audiobook nominal bitrate in kbps, when present (SKADI-I-0017).
    pub bitrate_kbps: Option<u32>,
    /// Music lossy bitrate **preset** token when the title names one — `V0`,
    /// `V2`, `VBR` (SKADI-T-0017).
    ///
    /// Separate from `bitrate_kbps` because a LAME preset is not a kbps value:
    /// V0 averages ~245 kbps but is preferred over CBR 320, so folding it into a
    /// number loses the distinction the release title is making.
    pub bitrate_preset: Option<String>,
    /// Whether a music release states a hi-res sample rate or bit depth —
    /// 24-bit, or above 48kHz (SKADI-T-0017). `None` when the title says nothing.
    pub music_resolution: Option<bool>,
    /// Music disc number within a multi-disc release, when stated.
    pub disc: Option<u16>,
    /// Music edition markers — `Deluxe`, `Remastered`, `Anniversary` …
    /// (SKADI-T-0017). Distinct from `edition`, which the video path owns.
    pub music_edition: Vec<String>,
    /// Audiobook abridgement: `Some(true)` = abridged, `Some(false)` = explicitly
    /// unabridged, `None` = unstated (SKADI-I-0017).
    pub abridged: Option<bool>,
    /// Audiobook author(s) — the segment before the first " - " in the common
    /// `Author - Title` form (SKADI-I-0017). A parse *hint*; metadata (Audnexus)
    /// is canonical. Empty for video.
    pub author: Option<String>,
    /// Audiobook narrator, when the release clearly names one (best-effort hint).
    pub narrator: Option<String>,
    /// Series the book belongs to, when the release names it (SKADI-I-0017).
    pub series: Option<String>,
    /// Position within the series (e.g. "1", "3.5"), kept as a string to preserve
    /// decimal/part numbering.
    pub series_position: Option<String>,
    /// TV season number (SKADI-I-0037 / T-0267); `None` for non-TV or pure-absolute
    /// (anime) releases. A full-season pack sets `season` + `full_season` with no
    /// `episodes`.
    pub season: Option<u16>,
    /// TV episode number(s), expanded (a `S01E01-E03` range yields `[1, 2, 3]`).
    /// Empty for a season pack or a movie.
    pub episodes: Vec<u16>,
    /// Absolute episode number(s) for anime (e.g. `- 134`); empty otherwise.
    pub absolute: Vec<u32>,
    /// Daily-show air date as a normalized `YYYY-MM-DD` string, when the release is
    /// date-keyed (e.g. `2024.03.01`); the matcher resolves it to an episode.
    pub air_date: Option<String>,
    /// `true` when the release is a whole-season pack (`S01` / `Season 1`) with no
    /// individual episode numbers.
    pub full_season: bool,
    /// `true` when an **audiobook** release is a multi-book pack — a series run
    /// (`Books 1-8`, `Vol 1-12`), a `Collection`/`Omnibus`/`Anthology`, or an author
    /// `Discography` (SKADI-T-0312). The audiobook parallel to [`full_season`]; the hunter
    /// uses it to prefer a pack over individual books (SKADI-T-0314).
    pub book_pack: bool,
}

impl ParsedRelease {
    /// A builder-ish constructor for tests/fixtures: an empty parse with a title.
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty_parse() {
        let p = ParsedRelease::default();
        assert!(p.title.is_none());
        assert!(p.modifiers.is_empty());
    }

    #[test]
    fn json_round_trips() {
        let p = ParsedRelease {
            title: Some("Blade Runner".into()),
            year: Some(1982),
            resolution: Some("2160p".into()),
            source: Some("BluRay".into()),
            codec: Some("x265".into()),
            modifiers: vec!["Remux".into()],
            group: Some("GROUP".into()),
            edition: Some("Final Cut".into()),
            languages: vec!["English".into()],
            audio_format: Some("M4B".into()),
            bitrate_kbps: Some(128),
            bitrate_preset: Some("V0".into()),
            music_resolution: Some(true),
            disc: Some(2),
            music_edition: vec!["Deluxe".into()],
            abridged: Some(false),
            author: Some("Ridley Scott".into()),
            narrator: None,
            series: None,
            series_position: None,
            season: Some(1),
            episodes: vec![1, 2, 3],
            absolute: vec![134],
            air_date: Some("2024-03-01".into()),
            full_season: false,
            book_pack: false,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ParsedRelease = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn partial_json_uses_defaults() {
        // Missing fields fall back to defaults thanks to `#[serde(default)]`.
        let back: ParsedRelease = serde_json::from_str(r#"{"title":"X","year":2020}"#).unwrap();
        assert_eq!(back.title.as_deref(), Some("X"));
        assert_eq!(back.year, Some(2020));
        assert!(back.resolution.is_none());
    }
}
