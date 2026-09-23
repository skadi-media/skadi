//! QuickTime chapter-track extraction for M4B audiobooks (SKADI-T-0330 /
//! I-0048 offline player).
//!
//! M4B chapters in the wild come in two shapes: the Nero `chpl` atom and the
//! QuickTime **chapter text track** (the audio `trak` carries a `tref/chap`
//! box referencing a text `trak` whose samples are the chapter titles and
//! whose `stts` timing gives the start offsets). Skadi's library uses the
//! QuickTime form exclusively (probed 2026-07-05), so that is what this
//! parses; `chpl` support can be bolted on if a file ever needs it.
//!
//! Self-contained box walker over `std::fs` — **blocking**, call from
//! `spawn_blocking` in async contexts. Best-effort by design: any structural
//! surprise yields `Ok(vec![])` rather than an error — a book without
//! readable chapters still plays, the player just shows a plain seek bar.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// One chapter mark: `start_secs..end_secs` with a human title.
#[derive(Clone, Debug, PartialEq)]
pub struct ChapterMark {
    pub index: usize,
    pub title: String,
    pub start_secs: f64,
    pub end_secs: f64,
}

/// Extract chapter marks from an MP4-family audiobook (m4b/m4a/mp4).
/// Empty vec when the file has no recognizable chapter track.
pub fn chapters(path: &Path) -> std::io::Result<Vec<ChapterMark>> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut r = BufReader::new(f);

    // Find the top-level `moov` box (often at the END of an m4b).
    let Some((moov_start, moov_end)) = find_box(&mut r, 0, len, b"moov")? else {
        return Ok(Vec::new());
    };

    // Parse every trak: id, handler, timescale/duration, sample tables, chap refs.
    let mut traks: Vec<Trak> = Vec::new();
    let mut pos = moov_start;
    while let Some((kind, body_start, body_end)) = next_box(&mut r, pos, moov_end)? {
        if &kind == b"trak"
            && let Some(t) = parse_trak(&mut r, body_start, body_end)?
        {
            traks.push(t);
        }
        pos = body_end;
    }

    // The chapter track: referenced via any trak's tref/chap, else the first
    // `text`-handler trak (some encoders omit the tref).
    let referenced: Vec<u32> = traks.iter().flat_map(|t| t.chap_refs.clone()).collect();
    let chapter_trak = traks
        .iter()
        .find(|t| referenced.contains(&t.id))
        .or_else(|| traks.iter().find(|t| &t.handler == b"text"))
        .cloned();
    let Some(ct) = chapter_trak else {
        return Ok(Vec::new());
    };
    if ct.timescale == 0 {
        return Ok(Vec::new());
    }

    // Start time of each sample: cumulative stts deltas.
    let mut starts: Vec<f64> = Vec::new();
    // Cap total expanded SAMPLES, not just stts entries: one entry can claim
    // count=u32::MAX, and 4.3B f64s is ~34GB (review pass 2, server finding 2).
    // No real chapter track exceeds a few thousand samples.
    const MAX_SAMPLES: usize = 100_000;
    let mut acc: u64 = 0;
    'stts: for &(count, delta) in &ct.stts {
        for _ in 0..count {
            if starts.len() >= MAX_SAMPLES {
                break 'stts;
            }
            starts.push(acc as f64 / ct.timescale as f64);
            acc = acc.saturating_add(u64::from(delta));
        }
    }
    let track_end = if ct.duration > 0 {
        ct.duration as f64 / ct.timescale as f64
    } else {
        acc as f64 / ct.timescale as f64
    };

    // Absolute file offset of each sample: chunk offsets + stsc mapping + sizes.
    let n = ct.stsz.len().min(starts.len());
    let offsets = sample_offsets(&ct.stco, &ct.stsc, &ct.stsz, n);

    // Titles: each text sample is a u16 BE length + UTF-8 bytes.
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (Some(&off), Some(&size)) = (offsets.get(i), ct.stsz.get(i)) else {
            break;
        };
        if !(2..=4096).contains(&size) {
            continue; // not a plausible title sample
        }
        r.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; size as usize];
        if r.read_exact(&mut buf).is_err() {
            break;
        }
        let tlen = usize::from(u16::from_be_bytes([buf[0], buf[1]])).min(buf.len() - 2);
        let title = String::from_utf8_lossy(&buf[2..2 + tlen]).into_owned();
        let start = starts.get(i).copied().unwrap_or(0.0);
        let end = starts.get(i + 1).copied().unwrap_or(track_end);
        out.push(ChapterMark {
            index: i,
            title: if title.trim().is_empty() {
                format!("Chapter {}", i + 1)
            } else {
                title.trim().to_string()
            },
            start_secs: start,
            end_secs: end.max(start),
        });
    }
    Ok(out)
}

/// Everything we need from one `trak`.
#[derive(Clone, Debug, Default)]
struct Trak {
    id: u32,
    handler: [u8; 4],
    timescale: u32,
    duration: u64,
    /// `(sample_count, sample_delta)` runs from `stts`.
    stts: Vec<(u32, u32)>,
    /// `(first_chunk, samples_per_chunk)` from `stsc` (1-based first_chunk).
    stsc: Vec<(u32, u32)>,
    /// Per-sample sizes from `stsz`.
    stsz: Vec<u32>,
    /// Chunk offsets from `stco`/`co64`.
    stco: Vec<u64>,
    /// Track ids this trak references as chapter tracks (`tref`/`chap`).
    chap_refs: Vec<u32>,
}

fn parse_trak<R: Read + Seek>(r: &mut R, start: u64, end: u64) -> std::io::Result<Option<Trak>> {
    let mut t = Trak::default();
    let mut pos = start;
    while let Some((kind, bs, be)) = next_box(r, pos, end)? {
        match &kind {
            b"tkhd" => {
                let full = read_fullbox_header(r, bs)?;
                // v0: creation(4) modification(4) id(4); v1: 8/8/4.
                let skip = if full.version == 1 { 16 } else { 8 };
                r.seek(SeekFrom::Start(bs + 4 + skip))?;
                t.id = read_u32(r)?;
            }
            b"tref" => {
                let mut p2 = bs;
                while let Some((k2, b2, e2)) = next_box(r, p2, be)? {
                    if &k2 == b"chap" {
                        r.seek(SeekFrom::Start(b2))?;
                        let mut left = e2 - b2;
                        while left >= 4 {
                            t.chap_refs.push(read_u32(r)?);
                            left -= 4;
                        }
                    }
                    p2 = e2;
                }
            }
            b"mdia" => parse_mdia(r, bs, be, &mut t)?,
            _ => {}
        }
        pos = be;
    }
    Ok((t.id != 0).then_some(t))
}

fn parse_mdia<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    t: &mut Trak,
) -> std::io::Result<()> {
    let mut pos = start;
    while let Some((kind, bs, be)) = next_box(r, pos, end)? {
        match &kind {
            b"mdhd" => {
                let full = read_fullbox_header(r, bs)?;
                if full.version == 1 {
                    r.seek(SeekFrom::Start(bs + 4 + 16))?;
                    t.timescale = read_u32(r)?;
                    t.duration = read_u64(r)?;
                } else {
                    r.seek(SeekFrom::Start(bs + 4 + 8))?;
                    t.timescale = read_u32(r)?;
                    t.duration = u64::from(read_u32(r)?);
                }
            }
            b"hdlr" => {
                // fullbox(4) + pre_defined(4) + handler_type(4)
                r.seek(SeekFrom::Start(bs + 8))?;
                let mut h = [0u8; 4];
                r.read_exact(&mut h)?;
                t.handler = h;
            }
            b"minf" => {
                let mut p2 = bs;
                while let Some((k2, b2, e2)) = next_box(r, p2, be)? {
                    if &k2 == b"stbl" {
                        parse_stbl(r, b2, e2, t)?;
                    }
                    p2 = e2;
                }
            }
            _ => {}
        }
        pos = be;
    }
    Ok(())
}

fn parse_stbl<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    t: &mut Trak,
) -> std::io::Result<()> {
    let mut pos = start;
    while let Some((kind, bs, _be)) = next_box(r, pos, end)? {
        let be = _be;
        match &kind {
            b"stts" => {
                r.seek(SeekFrom::Start(bs + 4))?;
                let n = read_u32(r)?.min(100_000);
                for _ in 0..n {
                    let count = read_u32(r)?;
                    let delta = read_u32(r)?;
                    t.stts.push((count, delta));
                }
            }
            b"stsc" => {
                r.seek(SeekFrom::Start(bs + 4))?;
                let n = read_u32(r)?.min(100_000);
                for _ in 0..n {
                    let first_chunk = read_u32(r)?;
                    let per_chunk = read_u32(r)?;
                    let _sdi = read_u32(r)?;
                    t.stsc.push((first_chunk, per_chunk));
                }
            }
            b"stsz" => {
                r.seek(SeekFrom::Start(bs + 4))?;
                let uniform = read_u32(r)?;
                let n = read_u32(r)?.min(100_000);
                if uniform != 0 {
                    t.stsz = vec![uniform; n as usize];
                } else {
                    for _ in 0..n {
                        t.stsz.push(read_u32(r)?);
                    }
                }
            }
            b"stco" => {
                r.seek(SeekFrom::Start(bs + 4))?;
                let n = read_u32(r)?.min(100_000);
                for _ in 0..n {
                    t.stco.push(u64::from(read_u32(r)?));
                }
            }
            b"co64" => {
                r.seek(SeekFrom::Start(bs + 4))?;
                let n = read_u32(r)?.min(100_000);
                for _ in 0..n {
                    t.stco.push(read_u64(r)?);
                }
            }
            _ => {}
        }
        pos = be;
    }
    Ok(())
}

/// Absolute file offset of each of the first `n` samples.
fn sample_offsets(stco: &[u64], stsc: &[(u32, u32)], stsz: &[u32], n: usize) -> Vec<u64> {
    let mut out = Vec::with_capacity(n);
    if stco.is_empty() || stsc.is_empty() {
        return out;
    }
    // Expand stsc: samples-per-chunk for every chunk. Clamp expansion to the
    // number of chunks we actually have — a crafted stsc with first_chunk=1
    // then u32::MAX would otherwise expand ~4B entries (review pass 2, server
    // finding 3). Chunks beyond stco are meaningless anyway.
    let mut per_chunk: Vec<u32> = Vec::with_capacity(stco.len());
    for (i, &(first, per)) in stsc.iter().enumerate() {
        let until = stsc
            .get(i + 1)
            .map(|&(f, _)| f)
            .unwrap_or(stco.len() as u32 + 1);
        for _ in first..until {
            if per_chunk.len() >= stco.len() {
                break;
            }
            per_chunk.push(per);
        }
        if per_chunk.len() >= stco.len() {
            break;
        }
    }
    let mut sample = 0usize;
    'outer: for (ci, &chunk_off) in stco.iter().enumerate() {
        let count = per_chunk.get(ci).copied().unwrap_or(0);
        let mut off = chunk_off;
        for _ in 0..count {
            if sample >= n {
                break 'outer;
            }
            out.push(off);
            off = off.saturating_add(u64::from(stsz.get(sample).copied().unwrap_or(0)));
            sample += 1;
        }
    }
    out
}

// --- low-level box plumbing ---------------------------------------------------

struct FullBox {
    version: u8,
}

fn read_fullbox_header<R: Read + Seek>(r: &mut R, body_start: u64) -> std::io::Result<FullBox> {
    r.seek(SeekFrom::Start(body_start))?;
    let mut vf = [0u8; 4];
    r.read_exact(&mut vf)?;
    Ok(FullBox { version: vf[0] })
}

/// Read the box at `pos` (bounded by `end`): returns `(type, body_start,
/// box_end)` or `None` at the end / on a truncated header.
fn next_box<R: Read + Seek>(
    r: &mut R,
    pos: u64,
    end: u64,
) -> std::io::Result<Option<([u8; 4], u64, u64)>> {
    // checked_add throughout: box sizes are attacker-controlled (torrent m4b);
    // a size near u64::MAX must NOT wrap and send the walk backwards into an
    // infinite loop (review pass 2, server finding 1/8).
    if pos.checked_add(8).is_none_or(|n| n > end) {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(pos))?;
    let size32 = read_u32(r)?;
    let mut kind = [0u8; 4];
    r.read_exact(&mut kind)?;
    let (body_start, box_size, min_size) = if size32 == 1 {
        let large = read_u64(r)?;
        (pos + 16, large, 16u64) // 64-bit size form: 16-byte header
    } else if size32 == 0 {
        (pos + 8, end - pos, 8u64) // box extends to EOF
    } else {
        (pos + 8, u64::from(size32), 8u64)
    };
    let Some(box_end) = pos.checked_add(box_size) else {
        return Ok(None);
    };
    // Reject too-small (header can't fit) and out-of-bounds/backwards boxes.
    if box_size < min_size || box_end > end || box_end <= pos {
        return Ok(None);
    }
    Ok(Some((kind, body_start, box_end)))
}

/// Scan sibling boxes from `pos` for `want`, returning its `(body_start, box_end)`.
fn find_box<R: Read + Seek>(
    r: &mut R,
    mut pos: u64,
    end: u64,
    want: &[u8; 4],
) -> std::io::Result<Option<(u64, u64)>> {
    while let Some((kind, bs, be)) = next_box(r, pos, end)? {
        if &kind == want {
            return Ok(Some((bs, be)));
        }
        pos = be;
    }
    Ok(None)
}

fn read_u32<R: Read>(r: &mut R) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // -- tiny box builders -------------------------------------------------

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full(version: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![version, 0, 0, 0];
        out.extend_from_slice(body);
        out
    }

    fn u32s(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// Build a minimal m4b-shaped file: an audio trak referencing a text
    /// chapter trak (id 2) whose 3 samples live in an mdat we control.
    fn synth_m4b(dir: &std::path::Path) -> std::path::PathBuf {
        // Chapter titles as text samples: u16 BE len + bytes.
        let titles = ["Intro", "The Middle", "The End"];
        let samples: Vec<Vec<u8>> = titles
            .iter()
            .map(|t| {
                let mut s = (t.len() as u16).to_be_bytes().to_vec();
                s.extend_from_slice(t.as_bytes());
                s
            })
            .collect();
        let sizes: Vec<u32> = samples.iter().map(|s| s.len() as u32).collect();

        // File layout: [mdat with samples][moov]. Compute mdat sample offsets.
        let mdat_body: Vec<u8> = samples.concat();
        let mdat = boxed(b"mdat", &mdat_body);
        let first_sample_off = 8u32; // mdat header

        // Chapter trak (id 2): text handler, timescale 1000, duration 60s,
        // stts: 3 samples with deltas 10s, 20s, 30s → starts 0, 10, 30.
        let tkhd2 = boxed(b"tkhd", &full(0, &u32s(&[0, 0, 2, 0])));
        let mdhd2 = boxed(b"mdhd", &full(0, &u32s(&[0, 0, 1000, 60_000])));
        let hdlr2 = {
            let mut b = full(0, &u32s(&[0]));
            b.extend_from_slice(b"text");
            b.extend_from_slice(&[0u8; 12]);
            boxed(b"hdlr", &b)
        };
        let stts = boxed(
            b"stts",
            &full(0, &u32s(&[3, 1, 10_000, 1, 20_000, 1, 30_000])),
        );
        let stsc = boxed(b"stsc", &full(0, &u32s(&[1, 1, 3, 1])));
        let mut stsz_body = u32s(&[0, 3]);
        stsz_body.extend_from_slice(&u32s(&sizes));
        let stsz = boxed(b"stsz", &full(0, &stsz_body));
        let stco = boxed(b"stco", &full(0, &u32s(&[1, first_sample_off])));
        let stbl = boxed(b"stbl", &[stts, stsc, stsz, stco].concat());
        let minf = boxed(b"minf", &stbl);
        let mdia2 = boxed(b"mdia", &[mdhd2, hdlr2, minf].concat());
        let trak2 = boxed(b"trak", &[tkhd2, mdia2].concat());

        // Audio trak (id 1) referencing trak 2 via tref/chap.
        let tkhd1 = boxed(b"tkhd", &full(0, &u32s(&[0, 0, 1, 0])));
        let chap = boxed(b"chap", &u32s(&[2]));
        let tref = boxed(b"tref", &chap);
        let trak1 = boxed(b"trak", &[tkhd1, tref].concat());

        let moov = boxed(b"moov", &[trak1, trak2].concat());

        let path = dir.join("synth.m4b");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&mdat).unwrap();
        f.write_all(&moov).unwrap();
        path
    }

    #[test]
    fn extracts_quicktime_chapter_track() {
        let dir = tempfile::tempdir().unwrap();
        let path = synth_m4b(dir.path());
        let ch = chapters(&path).unwrap();
        assert_eq!(ch.len(), 3, "{ch:?}");
        assert_eq!(ch[0].title, "Intro");
        assert_eq!(ch[1].title, "The Middle");
        assert_eq!(ch[2].title, "The End");
        assert_eq!(ch[0].start_secs, 0.0);
        assert_eq!(ch[1].start_secs, 10.0);
        assert_eq!(ch[2].start_secs, 30.0);
        // Ends chain to the next start; the last ends at the track duration.
        assert_eq!(ch[0].end_secs, 10.0);
        assert_eq!(ch[1].end_secs, 30.0);
        assert_eq!(ch[2].end_secs, 60.0);
    }

    #[test]
    fn chapterless_or_garbage_files_yield_empty_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        // No moov at all.
        let p1 = dir.path().join("noise.m4b");
        std::fs::write(&p1, b"this is not an mp4 file at all............").unwrap();
        assert!(chapters(&p1).unwrap().is_empty());
        // moov with an audio-only trak (no text track, no tref).
        let tkhd = boxed(b"tkhd", &full(0, &u32s(&[0, 0, 7, 0])));
        let moov = boxed(b"moov", &boxed(b"trak", &tkhd));
        let p2 = dir.path().join("nochap.m4b");
        std::fs::write(&p2, moov).unwrap();
        assert!(chapters(&p2).unwrap().is_empty());
    }

    #[test]
    fn crafted_boxes_dont_hang_or_oom() {
        // Untrusted torrent m4b hardening (review pass 2, server findings
        // 1/2/3/8): these must TERMINATE quickly with empty output, never
        // loop forever or allocate gigabytes.
        let dir = tempfile::tempdir().unwrap();

        // 64-bit size near u64::MAX inside moov — checked_add must not wrap.
        let mut giant = Vec::new();
        giant.extend_from_slice(&1u32.to_be_bytes()); // size32 == 1 (large form)
        giant.extend_from_slice(b"trak");
        giant.extend_from_slice(&(u64::MAX - 4).to_be_bytes()); // huge box size
        giant.extend_from_slice(&[0u8; 8]);
        let moov = boxed(b"moov", &giant);
        let p1 = dir.path().join("giant.m4b");
        std::fs::write(&p1, moov).unwrap();
        assert!(chapters(&p1).unwrap().is_empty());

        // 8..16-byte box in the 64-bit form (body_start > box_end) — must not
        // underflow.
        let mut small = Vec::new();
        small.extend_from_slice(&1u32.to_be_bytes());
        small.extend_from_slice(b"trak");
        small.extend_from_slice(&12u64.to_be_bytes()); // < 16, invalid large box
        let moov2 = boxed(b"moov", &small);
        let p2 = dir.path().join("small.m4b");
        std::fs::write(&p2, moov2).unwrap();
        assert!(chapters(&p2).unwrap().is_empty());
    }
}
