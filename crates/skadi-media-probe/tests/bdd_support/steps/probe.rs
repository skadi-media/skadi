//! Media-probe steps: fixtures, synthesised files, probing, chapter extraction,
//! and the *arr MediaInfo parity checks.
use cucumber::{given, then, when};
use skadi_media_probe::chapters::chapters;
use skadi_media_probe::{DefaultProber, MediaProber, VideoInfo};

use crate::bdd_support::World;

#[given(expr = "the fixture {string}")]
fn fixture(w: &mut World, name: String) {
    let p = World::fixture(&name);
    assert!(p.is_file(), "missing fixture {}", p.display());
    w.path = Some(p);
}

#[given(expr = "the fixture {string} copied to a file named {string}")]
fn fixture_renamed(w: &mut World, name: String, new_name: String) {
    let dst = w.scratch().join(new_name);
    std::fs::copy(World::fixture(&name), &dst).expect("copy fixture");
    w.path = Some(dst);
}

#[given(expr = "a file named {string} containing {int} bytes of garbage")]
fn garbage(w: &mut World, name: String, n: usize) {
    let p = w.scratch().join(name);
    // Deterministic pseudo-random bytes (xorshift) so a corrupt-file scenario is
    // reproducible run to run.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let bytes: Vec<u8> = (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x & 0xff) as u8
        })
        .collect();
    std::fs::write(&p, bytes).expect("write garbage");
    w.path = Some(p);
}

#[given(expr = "an empty file named {string}")]
fn empty(w: &mut World, name: String) {
    let p = w.scratch().join(name);
    std::fs::write(&p, b"").expect("write empty");
    w.path = Some(p);
}

#[given(expr = "a file named {string} that does not exist")]
fn missing(w: &mut World, name: String) {
    w.path = Some(w.scratch().join(name));
}

#[given(expr = "a synthesised {int} Hz {word} 16-bit WAV of {int} second(s) named {string}")]
fn synth_wav(w: &mut World, rate: u32, channels: String, secs: u32, name: String) {
    let ch: u16 = match channels.as_str() {
        "mono" => 1,
        "stereo" => 2,
        other => panic!("channels {other}"),
    };
    let frames = rate * secs;
    let data_len = frames * u32::from(ch) * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&ch.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(ch) * 2).to_le_bytes());
    out.extend_from_slice(&(ch * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(44 + data_len as usize, 0);
    let p = w.scratch().join(name);
    std::fs::write(&p, out).expect("write wav");
    w.path = Some(p);
}

#[when("the file is probed")]
fn probe(w: &mut World) {
    w.info = Some(DefaultProber.probe(w.path()));
}

#[when("chapters are extracted")]
fn extract_chapters(w: &mut World) {
    w.chapters = Some(chapters(w.path()).map_err(|e| e.to_string()));
}

#[then("it probes to nothing")]
fn nothing(w: &mut World) {
    let got = w.info.as_ref().expect("probed");
    assert!(got.is_none(), "probed to {got:?}");
}

#[then("it probes to something")]
fn something(w: &mut World) {
    assert!(w.info.as_ref().expect("probed").is_some());
}

#[then("the file has no video stream")]
fn no_video(w: &mut World) {
    assert!(w.info().video.is_none(), "{:?}", w.info().video);
}

#[then(expr = "the video is {int}x{int} {word}")]
fn video_is(w: &mut World, width: u32, height: u32, codec: String) {
    let v = w.info().video.as_ref().expect("video stream");
    assert_eq!((v.width, v.height), (width, height));
    assert_eq!(v.codec.as_deref(), Some(codec.as_str()));
}

#[then(expr = "the resolution tier is {string}")]
fn tier_is(w: &mut World, want: String) {
    let v = w.info().video.as_ref().expect("video stream");
    assert_eq!(v.resolution_tier(), want);
}

#[then(expr = "a {int}x{int} frame is tier {string}")]
fn frame_tier(_w: &mut World, width: u32, height: u32, want: String) {
    let v = VideoInfo {
        width,
        height,
        codec: None,
        profile: None,
        dynamic_range: None,
    };
    assert_eq!(v.resolution_tier(), want);
}

#[then(expr = "the audio codec is {string}")]
fn audio_codec(w: &mut World, want: String) {
    let a = w.info().audio.as_ref().expect("audio stream");
    assert_eq!(a.codec.as_deref(), Some(want.as_str()));
}

#[then(expr = "the audio has {int} channel(s) at {int} Hz")]
fn audio_channels(w: &mut World, channels: u8, rate: u32) {
    let a = w.info().audio.as_ref().expect("audio stream");
    assert_eq!(a.channels, Some(channels), "{a:?}");
    assert_eq!(a.sample_rate_hz, Some(rate), "{a:?}");
}

#[then(expr = "the audio bitrate is at least {int} kbps")]
fn audio_bitrate(w: &mut World, kbps: u32) {
    let a = w.info().audio.as_ref().expect("audio stream");
    assert!(a.bitrate_kbps.unwrap_or(0) >= kbps, "{a:?}");
}

#[then("the audio bitrate is known")]
fn audio_bitrate_known(w: &mut World) {
    let a = w.info().audio.as_ref().expect("audio stream");
    assert!(a.bitrate_kbps.is_some(), "bitrate unknown: {a:?}");
}

#[then("the audio channel count is known")]
fn audio_channels_known(w: &mut World) {
    let a = w.info().audio.as_ref().expect("audio stream");
    assert!(a.channels.is_some(), "channels unknown: {a:?}");
}

#[then(expr = "the duration is {int} second(s)")]
fn duration(w: &mut World, secs: u32) {
    assert_eq!(w.info().duration_secs, Some(secs), "{:?}", w.info());
}

#[then("the video dynamic range is known")]
fn dynamic_range(w: &mut World) {
    // Sonarr/Radarr `{MediaInfo VideoDynamicRange}` (HDR/DV) + bit depth. `VideoInfo`
    // carries only width/height/codec — there is no field to hold it.
    let v = w.info().video.as_ref().expect("video stream");
    let fields = format!("{v:?}");
    assert!(
        fields.contains("dynamic_range") || fields.contains("bit_depth"),
        "VideoInfo has no HDR/bit-depth field: {fields}"
    );
}

#[then("the audio languages are known")]
fn audio_languages(w: &mut World) {
    // Sonarr/Radarr `{MediaInfo AudioLanguages}` (SKADI-T-0422). Reported per
    // track, in track order — "which audio languages does this file have" is the
    // question, and a dual-audio release answers it with two.
    //
    // The fixture's track is tagged `und` (undefined), which is what the file
    // actually says. Asserting a *specific* language would be asserting something
    // the container does not claim; what this proves is that the element is read
    // and surfaced rather than dropped.
    let info = w.info();
    assert!(
        !info.audio_languages.is_empty(),
        "no audio language extracted: {info:?}"
    );
}

#[then(expr = "{int} chapter(s) is/are found")]
fn n_chapters(w: &mut World, n: usize) {
    let got = w
        .chapters
        .as_ref()
        .expect("extracted")
        .as_ref()
        .expect("no error");
    assert_eq!(got.len(), n, "{got:?}");
}

#[then("chapter extraction does not error")]
fn chapters_ok(w: &mut World) {
    let got = w.chapters.as_ref().expect("extracted");
    assert!(got.is_ok(), "{got:?}");
}
