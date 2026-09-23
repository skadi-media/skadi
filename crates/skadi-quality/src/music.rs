//! The **music** quality axis (SKADI-T-0017).
//!
//! Music has neither video's resolution/source/codec nor the audiobook's
//! single-file-container model. Its quality is **lossless vs lossy** first,
//! then bitrate within lossy and sample-rate/bit-depth within lossless. This is
//! the music-shaped parallel to [`crate::quality`] and [`crate::audiobook`]:
//! typed dimensions, a ranked definition set sharing the same [`QualityId`]
//! space, and [`to_music_quality`] to map a parsed release onto one.
//!
//! **The music domain does not exist yet** (deferred past v0). This module is
//! the parsing and grading half, which is domain-independent — the quality
//! engine is shared, so the axis can be correct and tested before anything
//! consumes it. Clean-room from observable release-naming conventions.

use serde::{Deserialize, Serialize};
use skadi_core::QualityId;
use uuid::Uuid;

use crate::parsed::ParsedRelease;

/// Music container/codec.
///
/// The **lossless/lossy split is the axis that matters** — an operator choosing
/// music quality is nearly always choosing between "bit-perfect" and "small",
/// and every finer distinction sits inside one of those two groups. Ordered
/// lossy-then-lossless so declaration order is a sane tie-break.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MusicFormat {
    Mp3,
    Aac,
    Ogg,
    Opus,
    Wma,
    /// Lossless from here down.
    Alac,
    Ape,
    WavPack,
    Wav,
    Flac,
}

impl MusicFormat {
    /// Map a release/file token (case-insensitive, leading `.` tolerated).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().trim_start_matches('.') {
            "flac" => Some(Self::Flac),
            "alac" => Some(Self::Alac),
            "ape" | "monkey's audio" => Some(Self::Ape),
            "wv" | "wavpack" => Some(Self::WavPack),
            "wav" | "pcm" => Some(Self::Wav),
            "mp3" => Some(Self::Mp3),
            "aac" | "m4a" => Some(Self::Aac),
            "ogg" | "vorbis" => Some(Self::Ogg),
            "opus" => Some(Self::Opus),
            "wma" => Some(Self::Wma),
            _ => None,
        }
    }

    /// Whether this format preserves the source bit-for-bit.
    ///
    /// The single most load-bearing predicate here: it decides whether a bitrate
    /// tier means anything at all. A lossless file's bitrate is a property of the
    /// music, not of the encode, so ranking FLAC releases by kbps would rank
    /// quiet albums below loud ones.
    #[must_use]
    pub fn is_lossless(self) -> bool {
        matches!(
            self,
            Self::Flac | Self::Alac | Self::Ape | Self::WavPack | Self::Wav
        )
    }
}

/// Lossy bitrate tiers, low → high.
///
/// The named LAME VBR presets (`V2`, `V0`) are tiers of their own rather than
/// being folded into their nominal kbps. They are what release titles actually
/// say, and V0 (~245 kbps average) is widely preferred over CBR 320 despite the
/// smaller number — collapsing them into a kbps bucket would lose exactly the
/// distinction an operator is expressing when they ask for V0.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MusicBitrate {
    /// VBR with no stated preset — nominal rate unknown, so it sorts lowest and
    /// never out-ranks a known tier.
    VbrUnknown,
    Kbps128,
    Kbps192,
    /// LAME `-V2`, ~190 kbps average.
    V2,
    Kbps256,
    /// LAME `-V0`, ~245 kbps average.
    V0,
    Kbps320,
    /// Lossless: bitrate is not a quality dimension.
    Lossless,
}

impl MusicBitrate {
    /// Bucket a nominal kbps value into a tier.
    #[must_use]
    pub fn from_kbps(kbps: u32) -> Self {
        match kbps {
            0..=159 => Self::Kbps128,
            160..=223 => Self::Kbps192,
            224..=287 => Self::Kbps256,
            _ => Self::Kbps320,
        }
    }

    /// Map a token like `320`, `320kbps`, `V0`, `V2` or `VBR` to a tier.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        let t = token.to_ascii_lowercase();
        let t = t.trim().trim_start_matches('-');
        match t {
            "v0" => return Some(Self::V0),
            "v2" => return Some(Self::V2),
            "vbr" => return Some(Self::VbrUnknown),
            _ => {}
        }
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u32>().ok().map(Self::from_kbps)
    }
}

/// A lossless release's sample rate and bit depth, when the title states them.
///
/// Only meaningful for lossless: "24bit 96kHz" on an MP3 is a mislabel, not a
/// hi-res release.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MusicResolution {
    /// 16-bit / 44.1kHz — CD.
    Cd,
    /// Anything above CD: 24-bit, or a sample rate above 48kHz.
    HiRes,
}

/// A named, stable music quality definition — the unit a music
/// [`QualityProfile`](crate::profile::QualityProfile) orders and selects from.
/// Shares the `QualityId` space with the movie and audiobook definitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MusicQualityDefinition {
    pub id: QualityId,
    pub name: String,
    pub format: MusicFormat,
    pub bitrate: MusicBitrate,
    /// `None` for lossy tiers, which have no meaningful resolution.
    pub resolution: Option<MusicResolution>,
}

/// Deterministic `QualityId` for built-in music definitions. Offset by 2000, so
/// they never collide with movie (`1..`) or audiobook (`1000..`) ids.
fn def_id(n: u128) -> QualityId {
    QualityId(Uuid::from_u128(2000 + n))
}

/// The sentinel **Unknown** music quality: a release whose title carries no
/// parseable format or bitrate. Ranked lowest so anything properly tagged
/// outranks it, and still acquirable rather than silently dropped — the same
/// policy as [`crate::audiobook::unknown_audiobook_id`] (SKADI-T-0175).
#[must_use]
pub fn unknown_music_id() -> QualityId {
    def_id(0)
}

/// The baseline ranked list of music quality definitions (low → high).
///
/// **Lossless outranks every lossy tier**, which is the whole point of the axis:
/// a FLAC rip is preferred over MP3-320 even though the MP3 has the larger
/// number attached. Within lossy the order follows perceived quality rather than
/// nominal kbps, which is why V0 sits above 256 and below 320 — see
/// [`MusicBitrate`].
///
/// The list order *is* the rank; [`def_id`] is decoupled from position, so
/// reordering the ladder never changes an already-classified file's `QualityId`.
#[must_use]
pub fn default_music_definitions() -> Vec<MusicQualityDefinition> {
    use MusicBitrate::*;
    use MusicFormat::*;
    use MusicResolution::*;
    // (id, name, format, bitrate, resolution) — listed low → high.
    let rows: &[(
        u128,
        &str,
        MusicFormat,
        MusicBitrate,
        Option<MusicResolution>,
    )] = &[
        (0, "Unknown", Mp3, VbrUnknown, None),
        (1, "MP3-128", Mp3, Kbps128, None),
        (2, "MP3-192", Mp3, Kbps192, None),
        (3, "MP3-V2", Mp3, V2, None),
        (4, "MP3-256", Mp3, Kbps256, None),
        (5, "MP3-V0", Mp3, V0, None),
        (6, "MP3-320", Mp3, Kbps320, None),
        (7, "AAC-256", Aac, Kbps256, None),
        (8, "AAC-320", Aac, Kbps320, None),
        (9, "FLAC", Flac, Lossless, Some(Cd)),
        (10, "ALAC", Alac, Lossless, Some(Cd)),
        (11, "FLAC 24bit", Flac, Lossless, Some(HiRes)),
    ];
    rows.iter()
        .map(
            |(n, name, format, bitrate, resolution)| MusicQualityDefinition {
                id: def_id(*n),
                name: (*name).to_string(),
                format: *format,
                bitrate: *bitrate,
                resolution: *resolution,
            },
        )
        .collect()
}

/// Map a parsed music release to a definition id.
///
/// A lossless format ignores any stated bitrate and matches on resolution
/// instead — see [`MusicFormat::is_lossless`]. Anything unmodeled falls back to
/// **Unknown** (lowest rank) so it stays acquirable. Returns `None` only when the
/// definition set carries no Unknown tier.
#[must_use]
pub fn to_music_quality(
    parsed: &ParsedRelease,
    definitions: &[MusicQualityDefinition],
) -> Option<QualityId> {
    let unknown = unknown_music_id();
    let format = parsed
        .audio_format
        .as_deref()
        .and_then(MusicFormat::from_token);

    if let Some(format) = format {
        if format.is_lossless() {
            // Bitrate is not a dimension here; resolution is. An unstated
            // resolution is CD rather than hi-res: hi-res is always advertised,
            // so silence means the ordinary case.
            let want = if parsed.music_resolution.unwrap_or(false) {
                MusicResolution::HiRes
            } else {
                MusicResolution::Cd
            };
            if let Some(d) = definitions
                .iter()
                .find(|d| d.id != unknown && d.format == format && d.resolution == Some(want))
            {
                return Some(d.id);
            }
            // A lossless format with no modeled resolution row still beats
            // guessing a lossy tier for it.
            if let Some(d) = definitions
                .iter()
                .find(|d| d.id != unknown && d.format == format)
            {
                return Some(d.id);
            }
        } else if let Some(bitrate) = parsed
            .bitrate_preset
            .as_deref()
            .and_then(MusicBitrate::from_token)
            .or_else(|| parsed.bitrate_kbps.map(MusicBitrate::from_kbps))
            && let Some(d) = definitions
                .iter()
                .find(|d| d.id != unknown && d.format == format && d.bitrate == bitrate)
        {
            return Some(d.id);
        }
    }

    definitions.iter().find(|d| d.id == unknown).map(|d| d.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_music;

    fn q(title: &str) -> String {
        let defs = default_music_definitions();
        let parsed = parse_music(title);
        let id = to_music_quality(&parsed, &defs).unwrap();
        defs.iter().find(|d| d.id == id).unwrap().name.clone()
    }

    #[test]
    fn lossless_outranks_every_lossy_tier() {
        let defs = default_music_definitions();
        let rank = |name: &str| defs.iter().position(|d| d.name == name).unwrap();
        // The point of the axis: a FLAC rip beats MP3-320 despite the smaller
        // number attached to it.
        assert!(rank("FLAC") > rank("MP3-320"));
        assert!(rank("ALAC") > rank("MP3-320"));
        assert!(rank("FLAC 24bit") > rank("FLAC"));
    }

    #[test]
    fn the_lame_presets_sit_where_listeners_put_them() {
        let defs = default_music_definitions();
        let rank = |name: &str| defs.iter().position(|d| d.name == name).unwrap();
        // V0 (~245kbps average) is preferred over CBR 256 and below CBR 320.
        // Bucketing V0 by its average kbps would put it below 256 and lose the
        // distinction the operator is expressing by asking for it.
        assert!(rank("MP3-V0") > rank("MP3-256"));
        assert!(rank("MP3-V0") < rank("MP3-320"));
        assert!(rank("MP3-V2") > rank("MP3-192"));
    }

    #[test]
    fn common_release_titles_grade_to_the_right_tier() {
        assert_eq!(q("Artist - Album (2019) [FLAC]"), "FLAC");
        assert_eq!(q("Artist - Album (2019) [MP3 320]"), "MP3-320");
        assert_eq!(q("Artist - Album (2019) [MP3 V0]"), "MP3-V0");
        assert_eq!(q("Artist - Album (2019) [MP3 V2]"), "MP3-V2");
        assert_eq!(q("Artist - Album 1999 [ALAC]"), "ALAC");
    }

    #[test]
    fn a_lossless_bitrate_is_ignored_rather_than_used_to_grade() {
        // FLAC titles often carry a computed bitrate. Grading on it would rank
        // quiet albums below loud ones, which is not a quality difference.
        assert_eq!(q("Artist - Album (2019) [FLAC 1053kbps]"), "FLAC");
        assert_eq!(q("Artist - Album (2019) [FLAC 700 kbps]"), "FLAC");
    }

    #[test]
    fn hi_res_is_recognised_only_for_lossless() {
        assert_eq!(q("Artist - Album (2019) [FLAC 24bit 96kHz]"), "FLAC 24bit");
        assert_eq!(q("Artist - Album (2019) [FLAC 24-96]"), "FLAC 24bit");
        // 16/44.1 is CD, not hi-res.
        assert_eq!(q("Artist - Album (2019) [FLAC 16bit 44.1kHz]"), "FLAC");
        // A "24bit" claim on a lossy file is a mislabel, not a hi-res release.
        assert_eq!(q("Artist - Album (2019) [MP3 320 24bit]"), "MP3-320");
    }

    #[test]
    fn an_unspecced_release_is_unknown_rather_than_dropped() {
        // Still acquirable, ranked lowest — the SKADI-T-0175 policy.
        assert_eq!(q("Artist - Album (2019)"), "Unknown");
        // A modeled format at an unmodeled tier also lands on Unknown rather
        // than being silently promoted to a neighbouring one.
        assert_eq!(q("Artist - Album (2019) [OGG 500]"), "Unknown");
    }

    #[test]
    fn format_tokens_map_case_insensitively_and_by_alias() {
        assert_eq!(MusicFormat::from_token("FLAC"), Some(MusicFormat::Flac));
        assert_eq!(MusicFormat::from_token(".flac"), Some(MusicFormat::Flac));
        assert_eq!(MusicFormat::from_token("wv"), Some(MusicFormat::WavPack));
        assert_eq!(MusicFormat::from_token("vorbis"), Some(MusicFormat::Ogg));
        assert_eq!(MusicFormat::from_token("mkv"), None);
    }

    #[test]
    fn is_lossless_splits_the_two_groups() {
        for f in [
            MusicFormat::Flac,
            MusicFormat::Alac,
            MusicFormat::Ape,
            MusicFormat::WavPack,
            MusicFormat::Wav,
        ] {
            assert!(f.is_lossless(), "{f:?}");
        }
        for f in [
            MusicFormat::Mp3,
            MusicFormat::Aac,
            MusicFormat::Ogg,
            MusicFormat::Opus,
            MusicFormat::Wma,
        ] {
            assert!(!f.is_lossless(), "{f:?}");
        }
    }
}
