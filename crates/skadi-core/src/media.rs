//! Probed media-info types (SKADI-I-0033) — the pure, serde-only shapes that the
//! prober ([`skadi-media-probe`](https://docs.rs/skadi-media-probe)) fills and the
//! domains/API store + expose. Kept in `skadi-core` so consumers don't pull in the
//! heavy container-parsing crates just for the type.

use serde::{Deserialize, Serialize};

/// Probed properties of one media file. Either or both of `video`/`audio` may be present
/// (a movie has both; an audiobook has only `audio`).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaInfo {
    /// Total runtime in whole seconds, when known.
    pub duration_secs: Option<u32>,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    /// Audio track languages, in track order (SKADI-T-0422). Sonarr's
    /// `MediaInfo AudioLanguages`. Empty when the container records none — which
    /// is common, so absence means "not stated", never "English".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_languages: Vec<String>,
    /// Subtitle track languages, in track order. Sonarr's
    /// `MediaInfo SubtitleLanguages`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitle_languages: Vec<String>,
    /// Container, lowercased and without the dot (`mkv`, `mp4`, `m2ts`, `avi`)
    /// — SKADI-T-0583.
    ///
    /// Recorded because the container decides as much about direct play as the
    /// codecs inside it: an `m2ts` holding perfectly ordinary H.264 still seeks
    /// badly over HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// File size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// **Overall** bitrate across all tracks, in kbps.
    ///
    /// The streaming number that matters, and deliberately not the audio
    /// track's. Matroska does not record a per-track bitrate (measured, see
    /// SKADI-T-0569), so this is derived from size and duration when the
    /// container will not say — which is exactly the figure a wifi link has to
    /// carry anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_bitrate_kbps: Option<u32>,
    /// **Every** audio track, in order.
    ///
    /// `audio` above is the first track and stays the primary for quality
    /// grading. This list exists because the streaming question is not "what is
    /// the first track" but "is there *any* track this device can decode": a
    /// file with DTS first and AC-3 second plays fine, and one with DTS alone
    /// plays silent. Collapsing them to one track cannot tell those apart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_tracks: Vec<AudioInfo>,
    /// Subtitle codecs, in track order (`subrip`, `hdmv_pgs_subtitle`, …).
    ///
    /// Separate from `subtitle_languages`, which answers *which* language;
    /// this answers *what format*, and image-based formats (PGS, VOBSUB) cannot
    /// be restyled or rendered by every player.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitle_codecs: Vec<String>,
}

impl MediaInfo {
    /// Whether nothing was probed (no duration, no streams).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.duration_secs.is_none() && self.video.is_none() && self.audio.is_none()
    }
}

/// Video stream properties.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Normalized codec name (`h264`/`hevc`/`av1`/`vp9`/…), when recognised.
    pub codec: Option<String>,
    /// Codec profile as the container states it (`High`, `High 10`, `Main 10`)
    /// — SKADI-T-0583.
    ///
    /// Kept because the profile, not the codec, is what a hardware decoder
    /// refuses: `h264` is universally supported and `h264 High 10` is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Dynamic range as the *container* declares it (SKADI-T-0543).
    ///
    /// `None` means the container carried no colour metadata — **not** SDR. A
    /// file with no `Colour` element is simply not saying, and treating silence
    /// as SDR would contradict a release title that says HDR on the strength of
    /// no evidence at all.
    pub dynamic_range: Option<DynamicRange>,
}

/// Transfer function a video track declares, normalised across containers
/// (SKADI-T-0543).
///
/// Derived from the transfer characteristics rather than the colour primaries:
/// BT.2020 primaries appear on plenty of SDR wide-gamut material, whereas PQ and
/// HLG are only used for HDR. Dolby Vision is not detected here — it is signalled
/// per-block rather than in the track header, so it needs a demux pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DynamicRange {
    /// A conventional transfer function (BT.709, BT.601, sRGB…).
    Sdr,
    /// SMPTE ST 2084 (PQ) — HDR10 and HDR10+.
    Hdr10,
    /// ARIB STD-B67 — hybrid log-gamma.
    Hlg,
    /// Dolby Vision (SKADI-T-0569). A distinct answer, not a flavour of
    /// `Hdr10`: DV carries its own dynamic metadata and releases are titled for
    /// it separately, so collapsing the two would make the probe disagree with a
    /// correctly-named release.
    ///
    /// Only reachable via the optional `ffprobe` pass — DV is signalled per
    /// block rather than in the track header, which is why SKADI-T-0543
    /// documented it as out of reach for a header walk.
    DolbyVision,
    /// Colour metadata was present but the transfer function is one we do not
    /// map. Distinct from `None`, which means nothing was declared.
    Unknown,
}

impl VideoInfo {
    /// The *arr-style resolution tier from the frame height (`"2160p"`, `"1080p"`,
    /// `"720p"`, `"480p"`, else `"SD"`). Pure. Uses the standard ≥ thresholds (and a
    /// 16:9-equivalent of the width) so a letterboxed 1920×800 still reads `1080p`.
    #[must_use]
    pub fn resolution_tier(&self) -> &'static str {
        match self.height.max(self.width * 9 / 16) {
            h if h >= 1980 => "2160p",
            h if h >= 1000 => "1080p",
            h if h >= 700 => "720p",
            h if h >= 450 => "480p",
            _ => "SD",
        }
    }
}

/// Audio stream properties.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AudioInfo {
    /// Normalized codec name (`aac`/`mp3`/`flac`/`opus`/`vorbis`/`alac`/…).
    pub codec: Option<String>,
    pub channels: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    /// Bits per sample, when the container records it (SKADI-T-0422). Sonarr's
    /// `MediaInfo AudioBitDepth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_tier_buckets_by_height() {
        let tier = |w: u32, h: u32| {
            VideoInfo {
                width: w,
                height: h,
                codec: None,
                profile: None,
                dynamic_range: None,
            }
            .resolution_tier()
        };
        assert_eq!(tier(3840, 2160), "2160p");
        assert_eq!(tier(1920, 1080), "1080p");
        assert_eq!(tier(1920, 800), "1080p", "letterboxed wide still 1080p");
        assert_eq!(tier(1280, 720), "720p");
        assert_eq!(tier(854, 480), "480p");
        assert_eq!(tier(640, 360), "SD");
    }

    #[test]
    fn empty_media_info_is_empty() {
        assert!(MediaInfo::default().is_empty());
        assert!(
            !MediaInfo {
                duration_secs: Some(1),
                ..Default::default()
            }
            .is_empty()
        );
    }
}
