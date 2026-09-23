//! Content ratings on the two US scales the metadata sources speak
//! (SKADI-T-0610): MPAA for films, the TV parental guidelines for series. A
//! household policy compares a title's rating against a member's ceiling, so
//! the scales are *ordered* here and parsing is forgiving about punctuation
//! ("TV-PG", "TVPG", "tv pg" are one rating).

use crate::MediaKind;

/// Which ladder a rating sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// G < PG < PG-13 < R < NC-17
    Mpaa,
    /// TV-Y < TV-Y7 < TV-G < TV-PG < TV-14 < TV-MA
    TvParental,
}

/// A parsed rating: its scale, its rung (0 = mildest) and the canonical label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rating {
    pub scale: Scale,
    pub rank: u8,
    pub label: &'static str,
}

const MPAA: &[&str] = &["G", "PG", "PG-13", "R", "NC-17"];
const TV: &[&str] = &["TV-Y", "TV-Y7", "TV-G", "TV-PG", "TV-14", "TV-MA"];

fn key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The scale a media kind is rated on (`None`: audiobooks carry no rating).
#[must_use]
pub fn scale_for(kind: MediaKind) -> Option<Scale> {
    match kind {
        MediaKind::Movie => Some(Scale::Mpaa),
        MediaKind::Series => Some(Scale::TvParental),
        _ => None,
    }
}

/// Parse a source label on the kind's scale. Unknown labels ("Not Rated",
/// "Approved", "16") are `None` — unrated, never a guess.
#[must_use]
pub fn parse_rating(kind: MediaKind, label: &str) -> Option<Rating> {
    let scale = scale_for(kind)?;
    let k = key(label);
    let (ladder, scale) = match scale {
        Scale::Mpaa => (MPAA, Scale::Mpaa),
        Scale::TvParental => (TV, Scale::TvParental),
    };
    ladder.iter().position(|r| key(r) == k).map(|i| Rating {
        scale,
        rank: i as u8,
        label: ladder[i],
    })
}

/// Every label on a kind's scale, mildest first — for policy editors.
#[must_use]
pub fn ladder(kind: MediaKind) -> &'static [&'static str] {
    match scale_for(kind) {
        Some(Scale::Mpaa) => MPAA,
        Some(Scale::TvParental) => TV,
        None => &[],
    }
}

impl Rating {
    /// Is this rating at or below `max` on the same scale? Different scales
    /// never compare (a film ceiling says nothing about a series).
    #[must_use]
    pub fn at_most(&self, max: &Rating) -> bool {
        self.scale == max.scale && self.rank <= max.rank
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_scales_forgivingly() {
        assert_eq!(
            parse_rating(MediaKind::Movie, "PG-13").unwrap().label,
            "PG-13"
        );
        assert_eq!(parse_rating(MediaKind::Movie, "pg13").unwrap().rank, 2);
        assert_eq!(parse_rating(MediaKind::Series, "TV-PG").unwrap().rank, 3);
        assert_eq!(
            parse_rating(MediaKind::Series, "tv pg").unwrap().label,
            "TV-PG"
        );
        assert_eq!(
            parse_rating(MediaKind::Series, "TVMA").unwrap().label,
            "TV-MA"
        );
        assert!(parse_rating(MediaKind::Movie, "Not Rated").is_none());
        assert!(
            parse_rating(MediaKind::Movie, "TV-14").is_none(),
            "wrong scale is unrated"
        );
        assert!(parse_rating(MediaKind::Audiobook, "PG").is_none());
    }

    #[test]
    fn ceilings_compare_within_a_scale_only() {
        let pg = parse_rating(MediaKind::Movie, "PG").unwrap();
        let r = parse_rating(MediaKind::Movie, "R").unwrap();
        let tv14 = parse_rating(MediaKind::Series, "TV-14").unwrap();
        assert!(pg.at_most(&r));
        assert!(!r.at_most(&pg));
        assert!(pg.at_most(&pg));
        assert!(!tv14.at_most(&r));
        assert_eq!(ladder(MediaKind::Series).len(), 6);
    }
}
