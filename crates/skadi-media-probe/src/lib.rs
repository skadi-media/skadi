//! `skadi-media-probe` — pure-Rust media-info probing (SKADI-I-0033).
//!
//! Opens a placed media file and reports its real stream properties — video
//! resolution/codec, runtime, and audio codec/bitrate/channels — so the library can show
//! them and quality decisions can be checked against reality instead of the filename. No
//! external `ffprobe`: audio via [`lofty`], video dimensions/codec via [`mp4`]/[`matroska`].
//! Probing is **best-effort**: an unsupported or corrupt file yields `None`, never an error
//! that could fail an import.

pub mod chapters;
mod mkv_colour;

pub mod ffprobe;

use std::path::Path;

// The data shapes live in `skadi-core` (pure serde, no parser deps) so domains/API can
// store + expose them without pulling in lofty/mp4/matroska; this crate owns the parsing.
pub use skadi_core::{AudioInfo, MediaInfo, VideoInfo};

/// Reads a file's [`MediaInfo`]. Blocking (it opens + parses the file) — call it off the
/// async runtime. Returns `None` for an unsupported/corrupt file rather than erroring.
pub trait MediaProber: Send + Sync {
    fn probe(&self, path: &Path) -> Option<MediaInfo>;
}

/// The default prober: dispatch by file extension to the right pure-Rust reader.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultProber;

impl MediaProber for DefaultProber {
    fn probe(&self, path: &Path) -> Option<MediaInfo> {
        // `None` had three distinct causes and no way to tell them apart from
        // outside (SKADI-T-0426): an extension we do not handle, a reader that
        // failed on the file, and a reader that succeeded but found nothing. An
        // operator seeing "Unknown" quality on a library file could not tell
        // which. Each is now logged at the level its cause deserves.
        let Some(ext) = ext_lower(path) else {
            tracing::debug!(?path, "probe skipped: no file extension");
            return None;
        };
        let info = match ext.as_str() {
            "mkv" | "webm" => probe_matroska(path),
            "mp4" | "m4v" | "mov" => probe_mp4(path),
            // Audio-only containers (and m4b/m4a, which are mp4 audio lofty handles well).
            "m4b" | "m4a" | "mp3" | "flac" | "ogg" | "opus" | "aac" | "wav" => probe_audio(path),
            // Containers no in-process reader handles, routed to ffprobe
            // (SKADI-T-0583). This is exactly the set most likely to need
            // repacking — a Blu-ray `.m2ts` or a `.avi` — so returning `None`
            // here meant the library scan was blind to its own worst files.
            "m2ts" | "mts" | "ts" | "avi" | "vob" | "mpg" | "mpeg" | "wmv" | "m2v" | "divx"
            | "flv" | "3gp" | "ogv" => {
                let probed = ffprobe::probe_standalone(path);
                if probed.is_none() {
                    tracing::debug!(
                        ?path, %ext,
                        "probe skipped: container needs ffprobe and it is unavailable or failed"
                    );
                }
                probed
            }
            _ => {
                // Debug, not warn: a library holds plenty of files we are not
                // meant to probe (.nfo, .srt, artwork), and warning on each would
                // bury the reader failures that actually matter.
                tracing::debug!(?path, %ext, "probe skipped: unsupported extension");
                return None;
            }
        };
        let Some(info) = info else {
            // The reader was the right one and still could not read it — a
            // truncated download or a corrupt remux. Worth a warning.
            tracing::warn!(?path, %ext, "probe failed: reader could not parse the file");
            return None;
        };
        // A reader that returned a fully-empty struct is no better than "unsupported".
        if info.is_empty() {
            tracing::warn!(?path, %ext, "probe empty: reader parsed the file but found no streams");
            return None;
        }
        let mut info = fill_gaps(path, info);
        // Container and size come from the file itself, so they are present even
        // when ffprobe is not installed — and the container is half of the
        // streaming verdict (SKADI-T-0583).
        info.container = Some(ext.clone());
        if info.size_bytes.is_none() {
            info.size_bytes = std::fs::metadata(path).ok().map(|m| m.len());
        }
        // Derive the overall rate when the container would not state it.
        // Matroska never does (measured, SKADI-T-0569), and size over duration is
        // the same number a network link has to sustain.
        if info.overall_bitrate_kbps.is_none()
            && let (Some(bytes), Some(secs)) = (info.size_bytes, info.duration_secs)
            && secs > 0
        {
            info.overall_bitrate_kbps = u32::try_from(bytes * 8 / u64::from(secs) / 1000).ok();
        }
        let info = info;
        tracing::trace!(
            ?path,
            duration_secs = info.duration_secs,
            has_video = info.video.is_some(),
            has_audio = info.audio.is_some(),
            "probed"
        );
        Some(info)
    }
}

/// Lowercased file extension, or `None` if there isn't one.
/// Fill **Dolby Vision**, the one field the in-process readers structurally
/// cannot reach, using `ffprobe` when it is available (SKADI-T-0569).
///
/// DV is signalled per *block* rather than in the track header, which is why
/// SKADI-T-0543 documented it as out of reach for a header walk. `ffprobe` reads
/// it as stream side-data without decoding.
///
/// Spawns only where DV is actually possible. SKADI-T-0528's grading pass probes
/// ~20,000 files, so a process per file would cost far more than the header reads
/// above; an audio file or an SDR-declared video skips this entirely.
///
/// `ffprobe` absent, failing, or timing out leaves `info` exactly as the
/// in-process readers produced it — those fields stay `None`, which already
/// means "not saying" everywhere in the codebase.
fn fill_gaps(path: &Path, mut info: MediaInfo) -> MediaInfo {
    // **Dolby Vision is the only thing worth spawning for.** Measured on a real
    // 1.5 GB library MKV: ffprobe reports no per-track audio bitrate either — it
    // hits the same Matroska limitation SKADI-T-0543 documented, and only
    // `format.bit_rate` (the whole file, video included, 4.4 Mbps) is available,
    // which would be badly wrong as an audio figure. Deriving a real one means
    // reading packets: a *bounded* 20-second read measured **18 s for one file**,
    // so ~20k library files is days — exactly the cost SKADI-T-0528 cannot pay.
    //
    // So triggering on a missing bitrate would spawn a process per file to learn
    // nothing. DV, by contrast, is side data ffprobe reads from the header region.
    //
    // Only where there is video, and only where the header walk did not already
    // settle the question as SDR — a DV file's base layer is PQ, so `Sdr` is a
    // definite answer that DV cannot contradict.
    let wants_dv = info
        .video
        .as_ref()
        .is_some_and(|v| !matches!(v.dynamic_range, Some(skadi_core::media::DynamicRange::Sdr)));
    if !wants_dv {
        return info;
    }
    let Some(sup) = ffprobe::probe(path) else {
        return info;
    };
    // Opportunistic only: we are already here for DV, so if ffprobe happens to
    // carry a bitrate the in-process reader lacked, take it. Never worth a spawn
    // of its own — see above.
    if let Some(kbps) = sup.audio_bitrate_kbps
        && let Some(a) = info.audio.as_mut()
        && a.bitrate_kbps.is_none()
    {
        a.bitrate_kbps = Some(kbps);
    }
    // DV *overrides* a transfer-function reading: a DV file's base layer is
    // usually PQ, so the header walk correctly says Hdr10 and ffprobe is the only
    // thing that can see the DV layer on top. The more specific answer wins.
    if sup.dolby_vision
        && let Some(v) = info.video.as_mut()
    {
        v.dynamic_range = Some(skadi_core::media::DynamicRange::DolbyVision);
    }

    // --- streaming-suitability fields (SKADI-T-0583) ---
    // The header readers cannot supply these: Matroska records no per-track
    // bitrate, and neither reader enumerates every audio track. Taken from
    // ffprobe unconditionally rather than as a fallback, because "first track"
    // and "all tracks" are different questions and only the latter decides
    // whether a file plays with sound.
    if !sup.audio_codecs.is_empty() {
        info.audio_tracks = sup
            .audio_codecs
            .iter()
            .map(|c| skadi_core::AudioInfo {
                codec: Some(c.clone()),
                ..Default::default()
            })
            .collect();
        // Carry what the header reader already knew about the first track —
        // channels and sample rate come from there, and dropping them would make
        // the richer list poorer than the single field it supplements.
        if let (Some(first), Some(a)) = (info.audio_tracks.first_mut(), info.audio.as_ref()) {
            first.channels = a.channels;
            first.sample_rate_hz = a.sample_rate_hz;
            first.bitrate_kbps = a.bitrate_kbps;
            first.bit_depth = a.bit_depth;
        }
    }
    if !sup.subtitle_codecs.is_empty() {
        info.subtitle_codecs = sup.subtitle_codecs.clone();
    }
    if let Some(profile) = sup.video_profile.clone()
        && let Some(v) = info.video.as_mut()
    {
        v.profile = Some(profile);
    }
    info.size_bytes = info.size_bytes.or(sup.size_bytes);
    info.overall_bitrate_kbps = info.overall_bitrate_kbps.or(sup.overall_bitrate_kbps);
    info
}

fn ext_lower(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

/// Audio (and m4b/m4a) via lofty: duration + bitrate + channels + sample rate + a codec
/// from the detected file type.
fn probe_audio(path: &Path) -> Option<MediaInfo> {
    use lofty::file::{AudioFile, TaggedFileExt};
    let tagged = lofty::read_from_path(path).ok()?;
    let props = tagged.properties();
    let audio = AudioInfo {
        codec: file_type_codec(tagged.file_type()),
        channels: props.channels(),
        bitrate_kbps: props.audio_bitrate(),
        sample_rate_hz: props.sample_rate(),
        // lofty exposes bit depth for lossless formats (SKADI-T-0422); `None`
        // for a lossy codec, where it is not a meaningful property.
        bit_depth: props.bit_depth(),
    };
    let dur = props.duration().as_secs();
    Some(MediaInfo {
        duration_secs: (dur > 0).then_some(dur as u32),
        video: None,
        audio: Some(audio),
        // An audio container carries one stream and no track-language element.
        ..Default::default()
    })
}

fn file_type_codec(ft: lofty::file::FileType) -> Option<String> {
    use lofty::file::FileType;
    Some(
        match ft {
            FileType::Mpeg => "mp3",
            FileType::Flac => "flac",
            FileType::Opus => "opus",
            FileType::Vorbis => "vorbis",
            FileType::Mp4 => "aac",
            FileType::Wav => "pcm",
            FileType::Aac => "aac",
            _ => return None,
        }
        .to_string(),
    )
}

/// MP4/M4V via the `mp4` crate: the video track's dimensions + codec, audio track props,
/// and the movie duration.
fn probe_mp4(path: &Path) -> Option<MediaInfo> {
    let f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let reader = std::io::BufReader::new(f);
    let mp4 = mp4::Mp4Reader::read_header(reader, size).ok()?;

    let mut info = MediaInfo {
        duration_secs: {
            let d = mp4.duration().as_secs();
            (d > 0).then_some(d as u32)
        },
        ..Default::default()
    };
    for track in mp4.tracks().values() {
        match track.track_type() {
            Ok(mp4::TrackType::Video) => {
                info.video = Some(VideoInfo {
                    width: u32::from(track.width()),
                    height: u32::from(track.height()),
                    codec: track.media_type().ok().map(mp4_media_codec),
                    // MP4 carries this in the `colr` box, which the `mp4` crate
                    // does not expose (SKADI-T-0543). Left unknown rather than
                    // assumed: the MKV path is where a real library's HDR lives.
                    // Profile is an ffprobe-only field; the header readers do not
                    // expose it (SKADI-T-0583).
                    profile: None,
                    dynamic_range: None,
                });
            }
            Ok(mp4::TrackType::Audio) => {
                info.audio = Some(AudioInfo {
                    codec: track.media_type().ok().map(mp4_media_codec),
                    // Sonarr's `MediaInfo AudioChannels` (SKADI-T-0422). The
                    // AAC channel configuration maps 1:1 onto a count except for
                    // 5.1/7.1, which carry an LFE the config index does not.
                    channels: track.channel_config().ok().map(channel_count),
                    bitrate_kbps: {
                        let b = track.bitrate() / 1000;
                        (b > 0).then_some(b)
                    },
                    sample_rate_hz: track.sample_freq_index().ok().map(|f| f.freq()),
                    // Not recorded in an MP4 audio sample entry.
                    bit_depth: None,
                });
            }
            _ => {}
        }
    }
    Some(info)
}

fn mp4_media_codec(m: mp4::MediaType) -> String {
    match m {
        mp4::MediaType::H264 => "h264",
        mp4::MediaType::H265 => "hevc",
        mp4::MediaType::VP9 => "vp9",
        mp4::MediaType::AAC => "aac",
        mp4::MediaType::TTXT => "ttxt",
    }
    .to_string()
}

/// MKV/WebM via the `matroska` crate: the first video track's pixel dimensions + codec,
/// the first audio track, and the segment duration.
fn probe_matroska(path: &Path) -> Option<MediaInfo> {
    let mkv = matroska::Matroska::open(std::fs::File::open(path).ok()?).ok()?;
    let mut info = MediaInfo {
        duration_secs: mkv.info.duration.map(|d| d.as_secs() as u32),
        ..Default::default()
    };
    // Track languages (SKADI-T-0422), in track order. Collected for *every*
    // matching track, not just the first: "which audio languages does this file
    // have" is the question an operator asks, and a dual-audio release answers it
    // with two.
    for track in &mkv.tracks {
        let lang = track.language.as_ref().map(language_code);
        match track.tracktype {
            matroska::Tracktype::Audio => {
                if let Some(l) = lang.clone() {
                    info.audio_languages.push(l);
                }
            }
            matroska::Tracktype::Subtitle => {
                if let Some(l) = lang {
                    info.subtitle_languages.push(l);
                }
            }
            _ => {}
        }
    }
    for track in &mkv.tracks {
        match &track.settings {
            matroska::Settings::Video(v) => {
                if info.video.is_none() {
                    info.video = Some(VideoInfo {
                        width: v.pixel_width as u32,
                        height: v.pixel_height as u32,
                        codec: Some(normalize_mkv_codec(&track.codec_id)),
                        // A second, targeted pass for the `Colour` element the
                        // `matroska` crate does not surface (SKADI-T-0543). It
                        // reads headers only and stops at the first Cluster, so
                        // it does not add a file read. `None` when the container
                        // declares nothing — which is not the same as SDR.
                        // Profile is an ffprobe-only field; the header readers do not
                        // expose it (SKADI-T-0583).
                        profile: None,
                        dynamic_range: std::fs::File::open(path)
                            .ok()
                            .and_then(|mut f| crate::mkv_colour::dynamic_range(&mut f)),
                    });
                }
            }
            matroska::Settings::Audio(a) => {
                if info.audio.is_none() {
                    info.audio = Some(AudioInfo {
                        codec: Some(normalize_mkv_codec(&track.codec_id)),
                        channels: u8::try_from(a.channels).ok(),
                        // Matroska has no bitrate element — it is derived from
                        // stream size, which the header does not carry per track.
                        //
                        // Deliberately left `None` rather than computed
                        // (SKADI-T-0543): deriving it means summing the track's
                        // block sizes across every cluster, i.e. reading the
                        // whole file. That is precisely the cost SKADI-T-0528
                        // cannot pay — ~20k library files over NFS — and the
                        // value is a nice-to-have next to resolution and
                        // dynamic range, which are both header-resident. MP4
                        // keeps its real per-track bitrate, so callers comparing
                        // across containers must treat `None` as unknown rather
                        // than as low.
                        bitrate_kbps: None,
                        sample_rate_hz: Some(a.sample_rate as u32),
                        bit_depth: a.bit_depth.and_then(|d| u8::try_from(d).ok()),
                    });
                }
            }
            matroska::Settings::None => {}
        }
    }
    Some(info)
}

/// The AAC channel-configuration index as a channel count (SKADI-T-0422).
fn channel_count(c: mp4::ChannelConfig) -> u8 {
    match c {
        mp4::ChannelConfig::Mono => 1,
        mp4::ChannelConfig::Stereo => 2,
        mp4::ChannelConfig::Three => 3,
        mp4::ChannelConfig::Four => 4,
        mp4::ChannelConfig::Five => 5,
        // 5.1 and 7.1 carry an LFE channel the config index does not count.
        mp4::ChannelConfig::FiveOne => 6,
        mp4::ChannelConfig::SevenOne => 8,
    }
}

/// A Matroska track language as a plain code (SKADI-T-0422).
///
/// Matroska carries either legacy ISO-639-2 (`eng`) or an IETF tag (`en-GB`);
/// both are returned as written rather than normalised, because a consumer that
/// cares about the distinction should see it, and one that does not can compare
/// the prefix.
fn language_code(l: &matroska::Language) -> String {
    match l {
        matroska::Language::ISO639(s) | matroska::Language::IETF(s) => s.clone(),
    }
}

/// Map a Matroska `CodecID` (`V_MPEG4/ISO/AVC`, `A_AAC`, …) to a short codec name.
fn normalize_mkv_codec(codec_id: &str) -> String {
    let id = codec_id.to_ascii_uppercase();
    if id.contains("AVC") || id.contains("H264") {
        "h264"
    } else if id.contains("HEVC") || id.contains("H265") {
        "hevc"
    } else if id.contains("AV1") {
        "av1"
    } else if id.contains("VP9") {
        "vp9"
    } else if id.contains("AAC") {
        "aac"
    } else if id.contains("MP3") || id.contains("MPEG/L3") {
        "mp3"
    } else if id.contains("FLAC") {
        "flac"
    } else if id.contains("OPUS") {
        "opus"
    } else if id.contains("VORBIS") {
        "vorbis"
    } else if id.contains("AC3") {
        "ac3"
    } else {
        return codec_id.to_string();
    }
    .to_string()
}

#[cfg(test)]
mod tests {

    /// SKADI-T-0422: what the fixtures actually carry, so the scenarios assert
    /// reality rather than hope. Extended for SKADI-T-0543's dynamic range.
    #[test]
    fn fixtures_report_the_new_fields() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mkv = DefaultProber
            .probe(&base.join("tiny.mkv"))
            .expect("mkv probes");
        let mp4 = DefaultProber
            .probe(&base.join("tiny.mp4"))
            .expect("mp4 probes");

        let mkv_audio = mkv.audio.as_ref().expect("mkv has an audio track");
        assert_eq!(mkv_audio.bit_depth, Some(32));
        // Matroska carries no per-track bitrate element; it is derived from
        // stream size, which the header does not hold (SKADI-T-0543).
        assert_eq!(mkv_audio.bitrate_kbps, None);
        assert_eq!(mkv.audio_languages, vec!["und".to_string()]);

        let mp4_audio = mp4.audio.as_ref().expect("mp4 has an audio track");
        assert_eq!(mp4_audio.channels, Some(2));
        assert_eq!(mp4_audio.bit_depth, None);

        // Neither fixture declares colour metadata, so both are `None` — "not
        // saying", not SDR. This is the assertion that proves the parser does
        // not invent a value when the element is absent; the mapping itself is
        // covered against synthetic EBML in `mkv_colour`, where the transfer
        // characteristic can actually be varied.
        assert_eq!(
            mkv.video.as_ref().and_then(|v| v.dynamic_range),
            None,
            "the fixture has no Colour element, so nothing should be reported"
        );
        assert_eq!(mp4.video.as_ref().and_then(|v| v.dynamic_range), None);
    }
    use super::*;

    // `resolution_tier` is tested in `skadi-core::media`; here we cover dispatch.
    #[test]
    fn unknown_extension_probes_to_none() {
        assert!(DefaultProber.probe(Path::new("/x/file.txt")).is_none());
        assert!(DefaultProber.probe(Path::new("/x/noext")).is_none());
    }
}

#[cfg(test)]
mod gap_filler_tests {
    use super::*;
    use skadi_core::media::DynamicRange;

    fn video(dr: Option<DynamicRange>) -> MediaInfo {
        MediaInfo {
            video: Some(skadi_core::VideoInfo {
                width: 1920,
                height: 1080,
                codec: None,
                profile: None,
                dynamic_range: dr,
            }),
            ..Default::default()
        }
    }

    /// The gap-filler must not spawn a process for files that cannot carry DV
    /// (SKADI-T-0569). SKADI-T-0528's grading pass probes ~20,000 files, so a
    /// spawn per file would cost far more than the header reads it supplements.
    #[test]
    fn audio_only_files_never_reach_ffprobe() {
        let audio = MediaInfo {
            audio: Some(skadi_core::AudioInfo {
                bitrate_kbps: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        // No video ⇒ no DV question ⇒ untouched, even though the bitrate is
        // missing. Triggering on a missing bitrate would spawn to learn nothing:
        // ffprobe reports no per-track audio bitrate for MKV either.
        assert_eq!(
            fill_gaps(std::path::Path::new("/nonexistent.mp3"), audio.clone()),
            audio
        );
    }

    #[test]
    fn a_file_already_known_sdr_is_not_re_probed() {
        // `Sdr` is a definite answer from the header walk, and a DV file's base
        // layer is PQ — so DV cannot contradict it and there is nothing to ask.
        let sdr = video(Some(DynamicRange::Sdr));
        assert_eq!(
            fill_gaps(std::path::Path::new("/nonexistent.mkv"), sdr.clone()),
            sdr
        );
    }

    /// With `ffprobe` absent — the state of most dev machines and CI — the probe
    /// must behave exactly as it did before this feature existed.
    #[test]
    fn a_missing_or_failing_ffprobe_changes_nothing() {
        for dr in [None, Some(DynamicRange::Hdr10), Some(DynamicRange::Unknown)] {
            let before = video(dr);
            // The path does not exist, so ffprobe (if installed at all) fails.
            let after = fill_gaps(std::path::Path::new("/nonexistent-xyz.mkv"), before.clone());
            assert_eq!(
                after, before,
                "ffprobe unavailable or failing must leave the in-process result untouched"
            );
        }
    }
}
