//! `ffprobe` gap-filler (SKADI-T-0569).
//!
//! **Not a replacement** for the pure-Rust readers — a supplement, run only for
//! fields they structurally cannot reach:
//!
//! * **MKV audio bitrate.** Matroska has no per-track bitrate element, so
//!   deriving it in-process means summing block sizes across every cluster —
//!   reading the whole file. `ffprobe` answers from the header and index.
//! * **Dolby Vision.** Signalled per block rather than in the track header, so it
//!   needs a demux pass; `ffprobe` reports the side-data.
//!
//! Everything else stays with the in-process readers, which are faster (no
//! process spawn), already tested, and correct. SKADI-T-0528's grading pass
//! probes ~20,000 files: a subprocess per file would be far more expensive than a
//! header read, while a subprocess for the handful of MKVs missing a bitrate is
//! not.
//!
//! If `ffprobe` is absent the probe degrades to exactly today's behaviour: these
//! two fields stay `None`, which already means "not saying" everywhere in the
//! codebase. A host without it is less complete, not broken.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

/// What `ffprobe` could tell us that the in-process readers could not.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Supplement {
    /// Audio bitrate in kbps, from the stream or its container fallback.
    pub audio_bitrate_kbps: Option<u32>,
    /// Whether a Dolby Vision configuration record is present.
    pub dolby_vision: bool,
    /// **Every** audio track's codec, in order (SKADI-T-0583). Lowercased.
    pub audio_codecs: Vec<String>,
    /// Every subtitle track's codec, in order. Lowercased.
    pub subtitle_codecs: Vec<String>,
    /// The video track's profile string (`High`, `High 10`, `Main 10`).
    pub video_profile: Option<String>,
    /// Container size in bytes, from `format.size`.
    pub size_bytes: Option<u64>,
    /// Overall bitrate across all streams, in kbps, from `format.bit_rate`.
    ///
    /// Distinct from `audio_bitrate_kbps` above, which is deliberately the audio
    /// track alone. This one is the whole file, which is what a network link has
    /// to carry.
    pub overall_bitrate_kbps: Option<u32>,
    /// Video codec name, lowercased.
    pub video_codec: Option<String>,
    /// Coded frame size.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Runtime in whole seconds, from `format.duration`.
    pub duration_secs: Option<u32>,
    /// Channel count of the first audio track.
    pub audio_channels: Option<u8>,
    /// Sample rate of the first audio track.
    pub audio_sample_rate_hz: Option<u32>,
}

impl Supplement {
    /// Nothing to add — used when `ffprobe` is missing or says nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.audio_bitrate_kbps.is_none()
            && !self.dolby_vision
            && self.audio_codecs.is_empty()
            && self.subtitle_codecs.is_empty()
            && self.video_profile.is_none()
            && self.size_bytes.is_none()
            && self.overall_bitrate_kbps.is_none()
    }
}

/// Whether `ffprobe` is on PATH. Probed once; the answer cannot change for a
/// running daemon, and asking per file would mean a `which` per library item.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("ffprobe")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// How long a single probe may take before it is abandoned.
///
/// A bound because this runs over a network share: a file on a stalled mount
/// would otherwise hang the grading pass indefinitely, and a missing bitrate is
/// never worth that. `ffprobe`'s own `-timeout` covers network *inputs*, not a
/// blocked local read, so the wall-clock guard is the one that matters here.
const PROBE_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Format {
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    duration: Option<String>,
}

#[derive(Deserialize)]
struct Stream {
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    channels: Option<u8>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
}

#[derive(Deserialize)]
struct SideData {
    #[serde(default)]
    side_data_type: Option<String>,
}

/// Parse `ffprobe -show_streams -show_format -of json` output.
///
/// Split from the process call so the mapping is testable without a binary —
/// which matters, because CI and most dev machines will not have one.
#[must_use]
pub fn parse(json: &str) -> Supplement {
    let Ok(out) = serde_json::from_str::<Output>(json) else {
        return Supplement::default();
    };

    // Prefer the audio stream's own bitrate. The container's is the sum across
    // every stream, so on a video file it is dominated by the video track and
    // would be wildly wrong as an *audio* bitrate.
    let audio_bitrate_kbps = out
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"))
        .and_then(|s| s.bit_rate.as_deref())
        .or_else(|| {
            // Fall back to the container only when there is no video stream to
            // contaminate it — an audio-only file, where the two are the same.
            let has_video = out
                .streams
                .iter()
                .any(|s| s.codec_type.as_deref() == Some("video"));
            if has_video {
                None
            } else {
                out.format.as_ref().and_then(|f| f.bit_rate.as_deref())
            }
        })
        .and_then(|s| s.parse::<u64>().ok())
        // bits/s → kbps. Zero means ffprobe could not determine it; reporting 0
        // would read as "silent", and `None` already means unknown.
        .and_then(|bps| (bps > 0).then_some((bps / 1000) as u32));

    let dolby_vision = out.streams.iter().any(|s| {
        s.side_data_list.iter().any(|d| {
            d.side_data_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("DOVI configuration record"))
        })
    });

    // Every audio track, not just the first (SKADI-T-0583). The streaming
    // question is whether *any* track decodes, so a list is the only shape that
    // can answer it.
    let codecs = |kind: &str| -> Vec<String> {
        out.streams
            .iter()
            .filter(|s| s.codec_type.as_deref() == Some(kind))
            .filter_map(|s| s.codec_name.as_deref())
            .map(str::to_ascii_lowercase)
            .collect()
    };
    let audio_codecs = codecs("audio");
    let subtitle_codecs = codecs("subtitle");

    let video_profile = out
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .and_then(|s| s.profile.clone())
        .filter(|p| !p.is_empty());

    let size_bytes = out
        .format
        .as_ref()
        .and_then(|f| f.size.as_deref())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|b| *b > 0);

    // The whole-file rate. Unlike the audio bitrate above, the container figure
    // is exactly right here — it is the sum across streams, which is what gets
    // pushed down the wire.
    let overall_bitrate_kbps = out
        .format
        .as_ref()
        .and_then(|f| f.bit_rate.as_deref())
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|bps| (bps > 0).then_some((bps / 1000) as u32));

    let video = out
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"));
    let first_audio = out
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"));

    Supplement {
        audio_bitrate_kbps,
        dolby_vision,
        audio_codecs,
        subtitle_codecs,
        video_profile,
        size_bytes,
        overall_bitrate_kbps,
        video_codec: video
            .and_then(|s| s.codec_name.as_deref())
            .map(str::to_ascii_lowercase),
        width: video.and_then(|s| s.width),
        height: video.and_then(|s| s.height),
        duration_secs: out
            .format
            .as_ref()
            .and_then(|f| f.duration.as_deref())
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| *d > 0.0)
            .map(|d| d as u32),
        audio_channels: first_audio.and_then(|s| s.channels),
        audio_sample_rate_hz: first_audio
            .and_then(|s| s.sample_rate.as_deref())
            .and_then(|r| r.parse::<u32>().ok())
            .filter(|r| *r > 0),
    }
}

/// Build a [`MediaInfo`] from `ffprobe` alone (SKADI-T-0583).
///
/// For containers the in-process readers do not handle — `m2ts`, `ts`, `avi`,
/// `vob` — which is precisely the set most likely to need repacking. Before this
/// they returned `None` and the library scan skipped the worst files in it.
#[must_use]
pub fn probe_standalone(path: &Path) -> Option<skadi_core::MediaInfo> {
    let sup = probe(path)?;
    if sup.is_empty() {
        return None;
    }
    let video = sup.width.zip(sup.height).map(|(width, height)| {
        skadi_core::VideoInfo {
            width,
            height,
            codec: sup.video_codec.clone(),
            profile: sup.video_profile.clone(),
            // ffprobe reports colour metadata, but mapping it to DynamicRange is
            // the header readers' job and is not duplicated here; a missing
            // value means "not stated", which is the honest answer.
            dynamic_range: None,
        }
    });
    let audio_tracks: Vec<skadi_core::AudioInfo> = sup
        .audio_codecs
        .iter()
        .map(|c| skadi_core::AudioInfo {
            codec: Some(c.clone()),
            ..Default::default()
        })
        .collect();
    let mut first = audio_tracks.first().cloned();
    if let Some(a) = first.as_mut() {
        a.channels = sup.audio_channels;
        a.sample_rate_hz = sup.audio_sample_rate_hz;
        a.bitrate_kbps = sup.audio_bitrate_kbps;
    }
    Some(skadi_core::MediaInfo {
        duration_secs: sup.duration_secs,
        video,
        audio: first,
        audio_tracks,
        subtitle_codecs: sup.subtitle_codecs.clone(),
        size_bytes: sup.size_bytes,
        overall_bitrate_kbps: sup.overall_bitrate_kbps,
        ..Default::default()
    })
}

/// A video's chapters via `ffprobe -show_chapters` (SKADI-T-0666), any
/// container. Empty when ffprobe is absent, fails, times out, or the file has
/// none. Blocking: call from the blocking pool.
#[must_use]
pub fn chapters(path: &Path) -> Vec<crate::markers::VideoChapter> {
    if !available() {
        return Vec::new();
    }
    let Ok(mut child) = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_chapters"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            crate::markers::parse_ffprobe_chapters(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

/// Run `ffprobe` against `path`. `None` if it is unavailable, fails, or times out.
#[must_use]
pub fn probe(path: &Path) -> Option<Supplement> {
    if !available() {
        return None;
    }
    let mut child = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Poll rather than `wait_with_output`, which has no timeout: a file on a
    // stalled NFS mount would block the whole grading pass.
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!(?path, "ffprobe timed out; leaving its fields unknown");
                return None;
            }
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let s = parse(&text);
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audio_streams_own_bitrate_wins_over_the_container() {
        // The container bitrate is the sum across streams, so on a video file it
        // is dominated by the video track. Using it as an *audio* bitrate would
        // report ~8 Mbps for a 640 kbps track.
        let json = r#"{
            "streams": [
                {"codec_type": "video", "bit_rate": "8000000"},
                {"codec_type": "audio", "bit_rate": "640000"}
            ],
            "format": {"bit_rate": "8640000"}
        }"#;
        assert_eq!(parse(json).audio_bitrate_kbps, Some(640));
    }

    #[test]
    fn the_container_is_used_only_when_there_is_no_video() {
        // An audio-only file: container and stream are the same thing, so the
        // fallback is safe and gets a value MKV would otherwise never report.
        let audio_only = r#"{
            "streams": [{"codec_type": "audio"}],
            "format": {"bit_rate": "256000"}
        }"#;
        assert_eq!(parse(audio_only).audio_bitrate_kbps, Some(256));

        // With video present and no audio bitrate, we say nothing rather than
        // report the video's.
        let with_video = r#"{
            "streams": [
                {"codec_type": "video", "bit_rate": "8000000"},
                {"codec_type": "audio"}
            ],
            "format": {"bit_rate": "8000000"}
        }"#;
        assert_eq!(parse(with_video).audio_bitrate_kbps, None);
    }

    #[test]
    fn a_zero_bitrate_is_unknown_not_silent() {
        let json = r#"{"streams": [{"codec_type": "audio", "bit_rate": "0"}]}"#;
        assert_eq!(parse(json).audio_bitrate_kbps, None);
    }

    #[test]
    fn dolby_vision_is_detected_from_the_side_data() {
        // The field SKADI-T-0543 documented as out of reach: DV is signalled per
        // block, so no header walk can see it.
        let json = r#"{
            "streams": [
                {"codec_type": "video",
                 "side_data_list": [{"side_data_type": "DOVI configuration record"}]}
            ]
        }"#;
        assert!(parse(json).dolby_vision);

        let hdr10_only = r#"{
            "streams": [
                {"codec_type": "video",
                 "side_data_list": [{"side_data_type": "Mastering display metadata"}]}
            ]
        }"#;
        assert!(
            !parse(hdr10_only).dolby_vision,
            "HDR10 mastering data is not Dolby Vision"
        );
    }

    #[test]
    fn malformed_or_empty_output_says_nothing() {
        // ffprobe failing must never invent a value — `None` means unknown
        // everywhere in the probe, and a wrong number would be worse than a gap.
        for bad in ["", "not json", "{}", r#"{"streams": []}"#] {
            assert!(parse(bad).is_empty(), "{bad}");
        }
    }
}
