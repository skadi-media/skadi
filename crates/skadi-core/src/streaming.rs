//! Direct-play suitability (SKADI-T-0583): what in the library will not stream,
//! and what repacking it would take.
//!
//! skadi has **no transcoder**, by design. The device decodes what is on disk,
//! so a file the device cannot decode is not a slow stream — it is a broken one,
//! and the only fix is to change the file. This module decides which files those
//! are.
//!
//! The judgements here are about **Android/ExoPlayer**, which is what the native
//! client uses. They are deliberately conservative: a false "fine" shows up as a
//! film that plays with no sound, while a false "repack" costs only disk.

use serde::{Deserialize, Serialize};

use crate::media::MediaInfo;

/// One reason a file will not direct-play well.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamingIssue {
    /// No audio track the device can decode — the film plays **silently**.
    ///
    /// The worst failure mode in the set, because it looks like a working
    /// playback rather than an error, and because it is the cheapest to fix:
    /// adding an AC-3 or AAC track is an audio-only encode, seconds per minute
    /// of runtime, leaving the video untouched.
    NoDecodableAudio { codecs: Vec<String> },
    /// The container seeks badly or is poorly supported over HTTP.
    UnsuitableContainer { container: String },
    /// A video codec or profile that hardware decoders commonly refuse.
    ///
    /// Unlike audio this is expensive to fix — it is a full re-encode — so it is
    /// reported separately rather than lumped in with the cheap repacks.
    RiskyVideo {
        codec: String,
        profile: Option<String>,
    },
    /// Bitrate high enough to stall on a home wifi link.
    ///
    /// Not a correctness problem: the file plays, it just buffers. Reported so
    /// the decision is the operator's.
    HighBitrate { kbps: u32 },
    /// Subtitles exist but all of them are image-based (PGS/VOBSUB), which
    /// cannot be restyled and are not rendered by every player.
    ImageOnlySubtitles { codecs: Vec<String> },
}

impl StreamingIssue {
    /// What it would take to fix. Used to group a report by effort, because
    /// "add an audio track" and "re-encode the video" are not the same ask.
    #[must_use]
    pub fn remedy(&self) -> Remedy {
        match self {
            // Audio-only encode; video and container are copied through.
            Self::NoDecodableAudio { .. } => Remedy::AddAudioTrack,
            // Stream copy into mkv/mp4 — no re-encode at all.
            Self::UnsuitableContainer { .. } => Remedy::Remux,
            Self::RiskyVideo { .. } | Self::HighBitrate { .. } => Remedy::ReencodeVideo,
            Self::ImageOnlySubtitles { .. } => Remedy::ExtractSubtitles,
        }
    }

    /// How badly it breaks playback, highest first.
    #[must_use]
    pub fn severity(&self) -> Severity {
        match self {
            // Silent playback and a refused decode are both "does not work".
            Self::NoDecodableAudio { .. } | Self::RiskyVideo { .. } => Severity::Broken,
            Self::UnsuitableContainer { .. } => Severity::Degraded,
            Self::HighBitrate { .. } | Self::ImageOnlySubtitles { .. } => Severity::Annoying,
        }
    }
}

/// The work a fix implies, cheapest first.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Remedy {
    /// `-c copy` into a friendlier container. Minutes for a whole library.
    Remux,
    /// Demux the subtitle tracks to sidecar files.
    ExtractSubtitles,
    /// Re-encode audio only, copy video. Fast.
    AddAudioTrack,
    /// Full video re-encode. Hours per file, and lossy.
    ReencodeVideo,
}

/// How badly playback is affected.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Plays, with a wrinkle.
    Annoying,
    /// Plays, but worse than it should.
    Degraded,
    /// Does not play, or plays without sound.
    Broken,
}

/// Audio codecs Android/ExoPlayer decodes on essentially any device.
///
/// AC-3 and E-AC-3 are on the list because ExoPlayer ships software decoders for
/// both; DTS is **not**, in any variant — Android has never required a DTS
/// decoder and licensed hardware support is confined to a handful of TV boxes.
const DECODABLE_AUDIO: &[&str] = &[
    "aac", "mp3", "ac3", "eac3", "opus", "vorbis", "flac", "pcm", "alac", "mp2",
];

/// Containers that stream and seek cleanly over HTTP byte ranges.
const GOOD_CONTAINERS: &[&str] = &["mkv", "mp4", "m4v", "webm", "mov"];

/// Video codecs with broad hardware decode support.
const GOOD_VIDEO: &[&str] = &["h264", "hevc", "vp9", "av1", "vp8"];

/// Above this, a 1080p stream starts to outrun a typical home wifi link and
/// the player buffers. Chosen well above ordinary 1080p (8–15 Mbps) so it flags
/// remuxes rather than good encodes.
const HIGH_BITRATE_KBPS: u32 = 30_000;

/// Judge one file's direct-play suitability.
///
/// Returns every issue found, worst first. An empty vec means it streams as-is.
///
/// Unknowns are **not** issues. A probe that could not read the audio codec says
/// nothing about whether it decodes, and reporting silence as a fault would bury
/// the real problems under every file the prober happened to skip.
#[must_use]
pub fn assess(info: &MediaInfo) -> Vec<StreamingIssue> {
    let mut issues = Vec::new();

    // --- audio: is there ANY track this device can decode? ---
    let tracks: Vec<&str> = info
        .audio_tracks
        .iter()
        .filter_map(|t| t.codec.as_deref())
        .chain(
            // Fall back to the single `audio` field for rows probed before
            // `audio_tracks` existed, so an un-rescanned library still gets a
            // verdict instead of silently looking clean.
            info.audio_tracks
                .is_empty()
                .then(|| info.audio.as_ref().and_then(|a| a.codec.as_deref()))
                .flatten(),
        )
        .collect();
    if !tracks.is_empty() && !tracks.iter().any(|c| is_decodable_audio(c)) {
        issues.push(StreamingIssue::NoDecodableAudio {
            codecs: dedup(&tracks),
        });
    }

    // --- container ---
    if let Some(c) = info.container.as_deref() {
        let c = c.to_ascii_lowercase();
        if !GOOD_CONTAINERS.contains(&c.as_str()) {
            issues.push(StreamingIssue::UnsuitableContainer { container: c });
        }
    }

    // --- video codec + profile ---
    if let Some(v) = info.video.as_ref()
        && let Some(codec) = v.codec.as_deref()
    {
        let codec = codec.to_ascii_lowercase();
        let profile = v
            .profile
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // "High 10" is 10-bit H.264. Widely produced by anime encoders and
        // refused by most phone hardware decoders — the codec name alone
        // would call this file fine.
        let hi10 = codec == "h264" && profile.contains("10");
        if !GOOD_VIDEO.contains(&codec.as_str()) || hi10 {
            issues.push(StreamingIssue::RiskyVideo {
                codec,
                profile: v.profile.clone(),
            });
        }
    }

    // --- bitrate ---
    if let Some(kbps) = info.overall_bitrate_kbps.filter(|k| *k > HIGH_BITRATE_KBPS) {
        issues.push(StreamingIssue::HighBitrate { kbps });
    }

    // --- subtitles ---
    if !info.subtitle_codecs.is_empty() && info.subtitle_codecs.iter().all(|c| is_image_subtitle(c))
    {
        issues.push(StreamingIssue::ImageOnlySubtitles {
            codecs: dedup(
                &info
                    .subtitle_codecs
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ),
        });
    }

    issues.sort_by_key(|i| std::cmp::Reverse(i.severity()));
    issues
}

/// Whether this file is **broken** for direct play — it will not play, or will
/// play without sound (SKADI-T-0584).
///
/// The threshold for re-acquiring a file. Deliberately `Broken` only: a high
/// bitrate or image subtitles are reasons to *know* about a file, not reasons to
/// spend a download replacing it.
#[must_use]
pub fn is_broken(info: &MediaInfo) -> bool {
    assess(info)
        .iter()
        .any(|i| i.severity() == Severity::Broken)
}

/// Whether a codec name is one Android decodes. Matches on a prefix so ffprobe's
/// `pcm_s16le` family and `dts` vs `dca` both land correctly.
fn is_decodable_audio(codec: &str) -> bool {
    let c = codec.to_ascii_lowercase();
    // `dca` is ffprobe's name for DTS, and `truehd`/`mlp` are lossless Dolby —
    // neither decodes on a phone. Named explicitly so a future addition to
    // DECODABLE_AUDIO cannot accidentally let them through on a prefix match.
    if c.starts_with("dts") || c == "dca" || c.starts_with("truehd") || c.starts_with("mlp") {
        return false;
    }
    DECODABLE_AUDIO.iter().any(|good| c.starts_with(good))
}

/// Image-based subtitle formats, which cannot be restyled or searched.
fn is_image_subtitle(codec: &str) -> bool {
    let c = codec.to_ascii_lowercase();
    c.contains("pgs") || c.contains("dvd_sub") || c.contains("dvdsub") || c.contains("vobsub")
}

fn dedup(v: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        let s = s.to_ascii_lowercase();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{AudioInfo, VideoInfo};

    fn audio(codec: &str) -> AudioInfo {
        AudioInfo {
            codec: Some(codec.into()),
            ..Default::default()
        }
    }

    fn video(codec: &str, profile: Option<&str>) -> VideoInfo {
        VideoInfo {
            width: 1920,
            height: 1080,
            codec: Some(codec.into()),
            profile: profile.map(Into::into),
            dynamic_range: None,
        }
    }

    fn base() -> MediaInfo {
        MediaInfo {
            container: Some("mkv".into()),
            video: Some(video("h264", Some("High"))),
            audio_tracks: vec![audio("aac")],
            ..Default::default()
        }
    }

    #[test]
    fn an_ordinary_file_has_no_issues() {
        assert!(assess(&base()).is_empty());
    }

    /// The case this whole module exists for: DTS alone plays **silently**.
    #[test]
    fn dts_only_is_broken() {
        let info = MediaInfo {
            audio_tracks: vec![audio("dts")],
            ..base()
        };
        let issues = assess(&info);
        assert!(matches!(
            issues.as_slice(),
            [StreamingIssue::NoDecodableAudio { .. }]
        ));
        assert_eq!(issues[0].severity(), Severity::Broken);
        assert_eq!(issues[0].remedy(), Remedy::AddAudioTrack);
    }

    /// ...and the same file with an AC-3 track alongside is completely fine.
    /// This is the distinction a single-track `audio` field cannot make, and the
    /// reason `audio_tracks` was added.
    #[test]
    fn dts_with_a_second_decodable_track_is_fine() {
        let info = MediaInfo {
            audio_tracks: vec![audio("dts"), audio("ac3")],
            ..base()
        };
        assert!(
            assess(&info).is_empty(),
            "any decodable track is enough; the player picks it"
        );
    }

    #[test]
    fn ffprobes_own_names_for_dts_and_truehd_are_caught() {
        for codec in ["dca", "truehd", "dts-hd", "mlp"] {
            let info = MediaInfo {
                audio_tracks: vec![audio(codec)],
                ..base()
            };
            assert!(
                !assess(&info).is_empty(),
                "{codec} does not decode on Android and must be flagged"
            );
        }
    }

    #[test]
    fn m2ts_is_an_unsuitable_container() {
        let info = MediaInfo {
            container: Some("m2ts".into()),
            ..base()
        };
        let issues = assess(&info);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].remedy(), Remedy::Remux, "a stream copy fixes it");
    }

    /// 10-bit H.264 is the case the codec name alone gets wrong.
    #[test]
    fn h264_high_10_is_risky_even_though_h264_is_not() {
        assert!(assess(&base()).is_empty());
        let info = MediaInfo {
            video: Some(video("h264", Some("High 10"))),
            ..base()
        };
        assert!(matches!(
            assess(&info).as_slice(),
            [StreamingIssue::RiskyVideo { .. }]
        ));
    }

    #[test]
    fn a_huge_remux_is_flagged_but_only_as_annoying() {
        let info = MediaInfo {
            overall_bitrate_kbps: Some(64_000),
            ..base()
        };
        let issues = assess(&info);
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].severity(),
            Severity::Annoying,
            "it plays, it buffers"
        );
    }

    #[test]
    fn ordinary_bitrates_are_not_flagged() {
        let info = MediaInfo {
            overall_bitrate_kbps: Some(12_000),
            ..base()
        };
        assert!(assess(&info).is_empty());
    }

    #[test]
    fn pgs_only_subtitles_are_flagged_but_a_mixed_set_is_not() {
        let pgs = MediaInfo {
            subtitle_codecs: vec!["hdmv_pgs_subtitle".into()],
            ..base()
        };
        assert_eq!(assess(&pgs).len(), 1);

        let mixed = MediaInfo {
            subtitle_codecs: vec!["hdmv_pgs_subtitle".into(), "subrip".into()],
            ..base()
        };
        assert!(
            assess(&mixed).is_empty(),
            "one text track is enough to render subtitles"
        );
    }

    /// A probe that read nothing must not manufacture problems.
    #[test]
    fn is_broken_is_only_true_for_playback_failures() {
        let dts = MediaInfo {
            audio_tracks: vec![audio("dts")],
            ..base()
        };
        assert!(is_broken(&dts), "silent playback is broken");
        let big = MediaInfo {
            overall_bitrate_kbps: Some(80_000),
            ..base()
        };
        assert!(
            !is_broken(&big),
            "a big file buffers; that is not worth a re-download"
        );
        assert!(!is_broken(&MediaInfo::default()), "unknown is not broken");
    }

    #[test]
    fn an_unprobed_file_reports_nothing() {
        assert!(assess(&MediaInfo::default()).is_empty());
    }

    /// Rows written before `audio_tracks` existed still get judged, via the
    /// legacy single-track field.
    #[test]
    fn the_legacy_single_audio_field_is_still_read() {
        let info = MediaInfo {
            audio: Some(audio("dts")),
            audio_tracks: Vec::new(),
            ..base()
        };
        assert!(
            !assess(&info).is_empty(),
            "a library probed before the rescan must not look clean"
        );
    }

    #[test]
    fn issues_come_back_worst_first() {
        let info = MediaInfo {
            container: Some("m2ts".into()),
            audio_tracks: vec![audio("dts")],
            overall_bitrate_kbps: Some(80_000),
            ..base()
        };
        let issues = assess(&info);
        assert_eq!(issues.len(), 3);
        assert_eq!(issues[0].severity(), Severity::Broken);
        assert_eq!(issues[2].severity(), Severity::Annoying);
    }
}
