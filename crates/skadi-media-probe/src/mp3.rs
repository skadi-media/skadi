//! MPEG audio (MP3) framing for joining the parts of a multi-file audiobook
//! into one stream (SKADI-T-0738).
//!
//! A part on disk is `[ID3v2] [Xing/Info or VBRI frame] frames… [APEv2] [ID3v1]`.
//! Only the frames join cleanly. A tag in the middle of a stream is at best
//! skipped by the decoder; an Info frame in the middle is read as a frame
//! count and byte count for the whole stream, and the player reports the
//! duration of the first part. [`audio_window`] finds the frame run of one
//! file so the caller serves exactly that.
//!
//! Pure functions over byte slices do the recognising, so they can be tested
//! on synthetic bytes; [`audio_window`] is the one that touches the disk.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes read at the start of the audio to recognise a Xing/Info/VBRI frame.
const HEAD_PROBE: usize = 4096;
/// Bytes read at the end to recognise ID3v1 (128) and an APEv2 footer (32).
const TAIL_PROBE: u64 = 160;

/// Byte range `[start, end)` of the MPEG frames in `path`: past the ID3v2 tag
/// and a leading Xing/Info/VBRI frame, and short of the APEv2 and ID3v1 tail
/// tags. A file with no tags is `[0, len)`. Blocking.
pub fn audio_window(path: &Path) -> std::io::Result<(u64, u64)> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();

    // Head: one or more stacked ID3v2 tags, then the first frame.
    let mut start = 0u64;
    loop {
        let mut hdr = [0u8; 10];
        if start + 10 > len || read_at(&mut f, start, &mut hdr)? < 10 {
            break;
        }
        let tag = id3v2_len(&hdr);
        if tag == 0 {
            break;
        }
        start += tag;
    }
    if start < len {
        let want = HEAD_PROBE.min((len - start) as usize);
        let mut head = vec![0u8; want];
        let got = read_at(&mut f, start, &mut head)?;
        head.truncate(got);
        if let Some(n) = info_frame_len(&head) {
            start += n as u64;
        }
    }

    // Tail: ID3v1 last, an APEv2 footer before it.
    let mut end = len;
    let tail_len = TAIL_PROBE.min(len);
    let mut tail = vec![0u8; tail_len as usize];
    let got = read_at(&mut f, len - tail_len, &mut tail)?;
    tail.truncate(got);
    end = end.saturating_sub(tail_tags_len(&tail));

    if start >= end {
        // Nothing recognisable survived the trimming: serve the file as it is
        // rather than nothing.
        return Ok((0, len));
    }
    Ok((start, end))
}

fn read_at(f: &mut File, at: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    f.seek(SeekFrom::Start(at))?;
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

/// Length of the ID3v2 tag at the start of `head` (header plus body plus a
/// footer when flagged), or 0 when `head` does not start with one.
#[must_use]
pub fn id3v2_len(head: &[u8]) -> u64 {
    if head.len() < 10 || &head[0..3] != b"ID3" || head[3] == 0xFF || head[4] == 0xFF {
        return 0;
    }
    if head[6..10].iter().any(|b| b & 0x80 != 0) {
        return 0;
    }
    let size = head[6..10]
        .iter()
        .fold(0u64, |acc, b| (acc << 7) | u64::from(*b));
    let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
    10 + size + footer
}

/// Length of the Xing/Info or VBRI frame when `audio` (the bytes at the start
/// of the frame run) begins with one, else `None`.
#[must_use]
pub fn info_frame_len(audio: &[u8]) -> Option<usize> {
    let hdr = frame_header(audio)?;
    let n = hdr.len;
    if audio.len() < n.min(40) {
        return None;
    }
    // Xing/Info sits after the side information; VBRI is 32 bytes in.
    let side = match (hdr.mpeg1, hdr.mono) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    let xing = 4 + side;
    let has_xing = audio.len() >= xing + 4
        && (&audio[xing..xing + 4] == b"Xing" || &audio[xing..xing + 4] == b"Info");
    let has_vbri = audio.len() >= 40 && &audio[36..40] == b"VBRI";
    (has_xing || has_vbri).then_some(n)
}

/// Bytes of tail tags in `tail`, the last (up to 160) bytes of the file: an
/// ID3v1 block and an APEv2 tag footer in front of it.
#[must_use]
pub fn tail_tags_len(tail: &[u8]) -> u64 {
    let mut cut = 0u64;
    let mut end = tail.len();
    if end >= 128 && &tail[end - 128..end - 125] == b"TAG" {
        cut += 128;
        end -= 128;
    }
    if end >= 32 && &tail[end - 32..end - 24] == b"APETAGEX" {
        let f = &tail[end - 32..end];
        let size = u64::from(u32::from_le_bytes([f[12], f[13], f[14], f[15]]));
        let flags = u32::from_le_bytes([f[20], f[21], f[22], f[23]]);
        // `size` covers items plus footer; a header, when present, is 32 more.
        let header = if flags & 0x8000_0000 != 0 { 32 } else { 0 };
        cut += size + header;
    }
    cut
}

/// One parsed MPEG audio frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// Whole frame length in bytes, header included.
    pub len: usize,
    /// MPEG-1 (as opposed to MPEG-2 or 2.5): decides the side-info size.
    pub mpeg1: bool,
    /// Single channel: decides the side-info size.
    pub mono: bool,
}

/// Parse the frame header at the start of `b`, or `None` when it is not one.
#[must_use]
pub fn frame_header(b: &[u8]) -> Option<FrameHeader> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (b[1] >> 3) & 3; // 0: 2.5, 1: reserved, 2: MPEG-2, 3: MPEG-1
    let layer = (b[1] >> 1) & 3; // 1: III, 2: II, 3: I
    let bitrate_idx = usize::from(b[2] >> 4);
    let rate_idx = usize::from((b[2] >> 2) & 3);
    let padding = usize::from((b[2] >> 1) & 1);
    if version == 1 || layer == 0 || bitrate_idx == 0 || bitrate_idx == 15 || rate_idx == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let bitrate = bitrate_kbps(mpeg1, layer, bitrate_idx)? * 1000;
    let sample_rate = {
        let base = [44_100, 48_000, 32_000][rate_idx];
        match version {
            3 => base,
            2 => base / 2,
            _ => base / 4,
        }
    };
    let len = match layer {
        3 => (12 * bitrate / sample_rate + padding) * 4,
        2 => 144 * bitrate / sample_rate + padding,
        _ if mpeg1 => 144 * bitrate / sample_rate + padding,
        _ => 72 * bitrate / sample_rate + padding,
    };
    Some(FrameHeader {
        len,
        mpeg1,
        mono: (b[3] >> 6) == 3,
    })
}

fn bitrate_kbps(mpeg1: bool, layer: u8, idx: usize) -> Option<usize> {
    const V1_L1: [usize; 15] = [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ];
    const V1_L2: [usize; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ];
    const V1_L3: [usize; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const V2_L1: [usize; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ];
    const V2_L23: [usize; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let table = match (mpeg1, layer) {
        (true, 3) => V1_L1,
        (true, 2) => V1_L2,
        (true, _) => V1_L3,
        (false, 3) => V2_L1,
        (false, _) => V2_L23,
    };
    let v = *table.get(idx)?;
    (v > 0).then_some(v)
}

/// Playing time of an audio file in seconds, from its own headers. `None` for
/// a file lofty cannot read. Blocking.
#[must_use]
pub fn duration_secs(path: &Path) -> Option<f64> {
    use lofty::file::AudioFile;
    let tagged = lofty::read_from_path(path).ok()?;
    let d = tagged.properties().duration();
    (d.as_millis() > 0).then_some(d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 Layer III, 64 kbps, 44.1 kHz, mono: a 208-byte frame.
    pub(crate) const FRAME: [u8; 4] = [0xFF, 0xFB, 0x50, 0xC0];

    fn frame(payload: u8) -> Vec<u8> {
        let mut v = FRAME.to_vec();
        v.resize(208, payload);
        v
    }

    fn id3v2(body_len: usize, footer: bool) -> Vec<u8> {
        let mut v = b"ID3".to_vec();
        v.extend_from_slice(&[4, 0, if footer { 0x10 } else { 0 }]);
        let n = body_len as u32;
        v.extend_from_slice(&[
            ((n >> 21) & 0x7F) as u8,
            ((n >> 14) & 0x7F) as u8,
            ((n >> 7) & 0x7F) as u8,
            (n & 0x7F) as u8,
        ]);
        v.resize(10 + body_len + if footer { 10 } else { 0 }, 0);
        v
    }

    fn xing_frame() -> Vec<u8> {
        let mut v = frame(0);
        v[4 + 17..4 + 21].copy_from_slice(b"Xing");
        v
    }

    #[test]
    fn id3v2_header_is_syncsafe_and_counts_the_footer() {
        assert_eq!(id3v2_len(&id3v2(300, false)), 310);
        assert_eq!(id3v2_len(&id3v2(300, true)), 320);
        // Not a tag: too short, wrong magic, or a non-syncsafe size byte.
        assert_eq!(id3v2_len(b"ID3"), 0);
        assert_eq!(id3v2_len(&frame(0)), 0);
        let mut bad = id3v2(300, false);
        bad[9] = 0x80;
        assert_eq!(id3v2_len(&bad), 0);
    }

    #[test]
    fn frame_header_sizes_the_common_shapes() {
        // 64 kbps, 44.1 kHz, MPEG-1 Layer III: 144 * 64000 / 44100 = 208.
        assert_eq!(
            frame_header(&FRAME),
            Some(FrameHeader {
                len: 208,
                mpeg1: true,
                mono: true
            })
        );
        // Same with padding: 209.
        assert_eq!(frame_header(&[0xFF, 0xFB, 0x52, 0xC0]).unwrap().len, 209);
        // MPEG-2 Layer III, 64 kbps, 22.05 kHz, joint stereo: 72 * 64000 / 22050 = 208.
        let h = frame_header(&[0xFF, 0xF3, 0x80, 0x40]).unwrap();
        assert_eq!((h.len, h.mpeg1, h.mono), (208, false, false));
        // Not a frame: no sync, reserved version, bad bitrate index.
        assert_eq!(frame_header(b"Xing"), None);
        assert_eq!(frame_header(&[0xFF, 0xEB, 0x50, 0xC0]), None);
        assert_eq!(frame_header(&[0xFF, 0xFB, 0xF0, 0xC0]), None);
    }

    #[test]
    fn info_frame_is_recognised_by_its_tag_after_the_side_info() {
        assert_eq!(info_frame_len(&xing_frame()), Some(208));
        let mut info = xing_frame();
        info[21..25].copy_from_slice(b"Info");
        assert_eq!(info_frame_len(&info), Some(208));
        let mut vbri = frame(0);
        vbri[36..40].copy_from_slice(b"VBRI");
        assert_eq!(info_frame_len(&vbri), Some(208));
        // A plain audio frame is not stripped, nor is a non-frame.
        assert_eq!(info_frame_len(&frame(7)), None);
        assert_eq!(info_frame_len(b"junk"), None);
    }

    #[test]
    fn tail_tags_count_id3v1_then_ape() {
        let mut t = vec![1u8; 160];
        assert_eq!(tail_tags_len(&t), 0);
        t[32..35].copy_from_slice(b"TAG");
        assert_eq!(tail_tags_len(&t), 128);
        // An APEv2 footer right before the ID3v1 block: size 100, header flagged.
        t[0..8].copy_from_slice(b"APETAGEX");
        t[12..16].copy_from_slice(&100u32.to_le_bytes());
        t[20..24].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        assert_eq!(tail_tags_len(&t), 128 + 100 + 32);
        // APE alone, no header.
        let mut a = vec![1u8; 64];
        a[32..40].copy_from_slice(b"APETAGEX");
        a[44..48].copy_from_slice(&50u32.to_le_bytes());
        a[52..56].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(tail_tags_len(&a), 50);
    }

    #[test]
    fn audio_window_trims_both_ends_of_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("part.mp3");
        let mut bytes = id3v2(500, false);
        bytes.extend(xing_frame());
        let audio: Vec<u8> = (0..5).flat_map(frame).collect();
        bytes.extend(&audio);
        let mut tag = vec![0u8; 128];
        tag[0..3].copy_from_slice(b"TAG");
        bytes.extend(&tag);
        std::fs::write(&p, &bytes).unwrap();

        let (s, e) = audio_window(&p).unwrap();
        assert_eq!(s, 510 + 208);
        assert_eq!(e, bytes.len() as u64 - 128);
        assert_eq!(&bytes[s as usize..e as usize], &audio[..]);

        // A bare frame run is served whole.
        let q = dir.path().join("bare.mp3");
        std::fs::write(&q, &audio).unwrap();
        assert_eq!(audio_window(&q).unwrap(), (0, audio.len() as u64));

        // Trimming that leaves nothing falls back to the whole file.
        let r = dir.path().join("tagonly.mp3");
        std::fs::write(&r, &tag).unwrap();
        assert_eq!(audio_window(&r).unwrap(), (0, 128));
    }

    #[test]
    fn duration_comes_from_the_frames() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("part.mp3");
        // 400 frames of 1152 samples at 44.1 kHz is 10.45 s.
        let audio: Vec<u8> = (0..400).flat_map(|_| frame(0)).collect();
        std::fs::write(&p, &audio).unwrap();
        let d = duration_secs(&p).unwrap();
        assert!((d - 10.45).abs() < 0.2, "{d}");
        assert_eq!(duration_secs(&dir.path().join("missing.mp3")), None);
    }
}
