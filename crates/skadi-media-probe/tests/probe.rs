//! Real-file probing against tiny ffmpeg-generated fixtures (1s, 320×240 / silence).

use std::path::PathBuf;

use skadi_media_probe::{DefaultProber, MediaProber};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn probes_mp3_audio() {
    let info = DefaultProber
        .probe(&fixture("tiny.mp3"))
        .expect("mp3 probed");
    assert!(info.video.is_none(), "mp3 has no video");
    let a = info.audio.expect("audio present");
    assert_eq!(a.codec.as_deref(), Some("mp3"));
    assert_eq!(a.channels, Some(2));
    assert_eq!(a.sample_rate_hz, Some(44100));
    assert!(a.bitrate_kbps.unwrap_or(0) >= 96, "≈128 kbps: {a:?}");
    assert_eq!(info.duration_secs, Some(1));
}

#[test]
fn probes_m4a_aac_audio() {
    let info = DefaultProber
        .probe(&fixture("tiny.m4a"))
        .expect("m4a probed");
    assert!(info.video.is_none());
    let a = info.audio.expect("audio present");
    assert_eq!(a.codec.as_deref(), Some("aac"));
    assert_eq!(a.sample_rate_hz, Some(44100));
    assert!(info.duration_secs.unwrap_or(0) >= 1);
}

#[test]
fn probes_mp4_video_dimensions_and_codec() {
    let info = DefaultProber
        .probe(&fixture("tiny.mp4"))
        .expect("mp4 probed");
    let v = info.video.expect("video present");
    assert_eq!((v.width, v.height), (320, 240));
    assert_eq!(v.codec.as_deref(), Some("h264"));
    assert_eq!(v.resolution_tier(), "SD");
    assert!(info.audio.is_some(), "mp4 also has an audio track");
    assert_eq!(info.duration_secs, Some(1));
}

#[test]
fn probes_mkv_video_dimensions_and_codec() {
    let info = DefaultProber
        .probe(&fixture("tiny.mkv"))
        .expect("mkv probed");
    let v = info.video.expect("video present");
    assert_eq!((v.width, v.height), (320, 240));
    assert_eq!(v.codec.as_deref(), Some("h264"));
    let a = info.audio.expect("audio present");
    assert_eq!(a.codec.as_deref(), Some("aac"));
}
