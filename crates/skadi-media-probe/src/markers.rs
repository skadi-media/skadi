//! Skip-intro and skip-credits markers from a video's chapters (SKADI-T-0666).
//!
//! Decision: markers come from **chapter titles** in the file. Many releases
//! carry chapters named "Intro", "Opening", "Credits" or "Ending" — typically
//! anime and remuxes — and reading them needs no analysis of the audio. Audio
//! fingerprinting across a season (what Plex and Jellyfin's intro skipper do)
//! finds more, at the cost of decoding every episode; that is not done here. An
//! episode without named chapters simply shows no skip button.

use serde::Serialize;

/// A chapter as `ffprobe -show_chapters` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoChapter {
    pub title: String,
    pub start_secs: f64,
    pub end_secs: f64,
}

/// Where the intro and credits are, in seconds; each `None` when the file does
/// not say.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SkipMarkers {
    pub intro_start: Option<f64>,
    pub intro_end: Option<f64>,
    pub credits_start: Option<f64>,
}

fn normalise(title: &str) -> String {
    title
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '-', '.'], " ")
}

fn is_intro(t: &str) -> bool {
    t == "op"
        || [
            "intro",
            "opening",
            "title sequence",
            "main title",
            "theme song",
        ]
        .iter()
        .any(|p| t.starts_with(p))
}

fn is_credits(t: &str) -> bool {
    t == "ed"
        || [
            "credits",
            "end credits",
            "ending",
            "closing",
            "outro",
            "end titles",
        ]
        .iter()
        .any(|p| t.starts_with(p))
}

/// Markers from `chapters` of a video `duration_secs` long. Positions are
/// checked against what the names claim: an intro must start in the first
/// quarter and a credits chapter in the last third, so a mislabelled chapter
/// (an "Opening" scene halfway through) cannot skip the story.
#[must_use]
pub fn skip_markers(chapters: &[VideoChapter], duration_secs: f64) -> SkipMarkers {
    let duration = if duration_secs > 0.0 {
        duration_secs
    } else {
        chapters.iter().map(|c| c.end_secs).fold(0.0, f64::max)
    };
    let mut m = SkipMarkers::default();
    if duration <= 0.0 {
        return m;
    }
    if let Some(c) = chapters
        .iter()
        .find(|c| is_intro(&normalise(&c.title)) && c.start_secs <= duration * 0.25)
        && c.end_secs > c.start_secs
    {
        m.intro_start = Some(c.start_secs);
        m.intro_end = Some(c.end_secs);
    }
    if let Some(c) = chapters
        .iter()
        .find(|c| is_credits(&normalise(&c.title)) && c.start_secs >= duration * (2.0 / 3.0))
    {
        m.credits_start = Some(c.start_secs);
    }
    m
}

/// Parse `ffprobe -show_chapters -print_format json` output.
#[must_use]
pub fn parse_ffprobe_chapters(json: &str) -> Vec<VideoChapter> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["chapters"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .filter_map(|c| {
                    let n = |k: &str| c[k].as_str().and_then(|s| s.parse::<f64>().ok());
                    Some(VideoChapter {
                        title: c["tags"]["title"].as_str().unwrap_or_default().to_string(),
                        start_secs: n("start_time")?,
                        end_secs: n("end_time")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(title: &str, s: f64, e: f64) -> VideoChapter {
        VideoChapter {
            title: title.into(),
            start_secs: s,
            end_secs: e,
        }
    }

    #[test]
    fn named_intro_and_credits_become_markers() {
        let chapters = vec![
            ch("Prologue", 0.0, 90.0),
            ch("Opening", 90.0, 180.0),
            ch("Part A", 180.0, 700.0),
            ch("Part B", 700.0, 1290.0),
            ch("Ending", 1290.0, 1380.0),
            ch("Preview", 1380.0, 1420.0),
        ];
        assert_eq!(
            skip_markers(&chapters, 1420.0),
            SkipMarkers {
                intro_start: Some(90.0),
                intro_end: Some(180.0),
                credits_start: Some(1290.0),
            }
        );
    }

    #[test]
    fn common_spellings() {
        for t in [
            "Intro",
            "OP",
            "opening credits",
            "Title_Sequence",
            "Main Titles",
        ] {
            assert!(is_intro(&normalise(t)), "{t}");
        }
        for t in ["Credits", "ED", "End Credits", "Ending Theme", "Outro"] {
            assert!(is_credits(&normalise(t)), "{t}");
        }
        assert!(
            !is_intro(&normalise("Operation")),
            "OP only as the whole title"
        );
        assert!(
            !is_credits(&normalise("Edward")),
            "ED only as the whole title"
        );
    }

    #[test]
    fn a_misplaced_name_does_not_skip_the_story() {
        let chapters = vec![
            ch("Chapter 1", 0.0, 600.0),
            ch("Opening", 600.0, 700.0),
            ch("Credits", 700.0, 800.0),
            ch("Chapter 3", 800.0, 1800.0),
        ];
        assert_eq!(skip_markers(&chapters, 1800.0), SkipMarkers::default());
    }

    #[test]
    fn numbered_chapters_give_nothing() {
        let chapters = vec![ch("Chapter 1", 0.0, 600.0), ch("Chapter 2", 600.0, 1200.0)];
        assert_eq!(skip_markers(&chapters, 1200.0), SkipMarkers::default());
    }

    #[test]
    fn parses_ffprobe_output() {
        let json = r#"{"chapters":[{"id":0,"start_time":"0.000000","end_time":"91.500000","tags":{"title":"Intro"}},{"id":1,"start_time":"91.500000","end_time":"1300.000000"}]}"#;
        let cs = parse_ffprobe_chapters(json);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0], ch("Intro", 0.0, 91.5));
        assert_eq!(cs[1].title, "");
    }
}
