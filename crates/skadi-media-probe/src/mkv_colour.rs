//! A targeted EBML read for a Matroska video track's `Colour` element
//! (SKADI-T-0543).
//!
//! The `matroska` crate this module sits alongside does not surface colour
//! metadata, and its EBML reader is private, so this walks the handful of
//! elements needed to reach it: `Segment` → `Tracks` → `TrackEntry` → `Video` →
//! `Colour` → `TransferCharacteristics`.
//!
//! **This is a header read, not a demux.** `Tracks` always precedes the first
//! `Cluster`, and every element carries its size, so the walk skips whole
//! subtrees with a seek rather than reading them. Scanning stops at the first
//! `Cluster`: past that point there is no `Tracks` to find and continuing would
//! mean reading the media body, which for a library scan over NFS is the cost
//! that matters.
//!
//! Deliberately not handled: Dolby Vision, which is signalled per block rather
//! than in the track header and so cannot be reached without demuxing.

use std::io::{Read, Seek, SeekFrom};

use skadi_core::media::DynamicRange;

const SEGMENT: u32 = 0x1853_8067;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const VIDEO: u32 = 0xE0;
const COLOUR: u32 = 0x55B0;
const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
const CLUSTER: u32 = 0x1F43_B675;

/// Transfer characteristic values from the Matroska spec (which follows
/// ITU-T H.273). Only the ones that decide the question are named.
const TRANSFER_BT709: u64 = 1;
const TRANSFER_BT601: u64 = 6;
const TRANSFER_SRGB: u64 = 13;
const TRANSFER_BT2020_10: u64 = 14;
const TRANSFER_BT2020_12: u64 = 15;
const TRANSFER_PQ: u64 = 16;
const TRANSFER_HLG: u64 = 18;

/// Map a Matroska transfer characteristic to a dynamic range.
///
/// The BT.2020 entries (14/15) are **SDR**: they are wide-gamut transfer
/// functions, not HDR ones. Reading "2020" as HDR is the obvious mistake here
/// and would mislabel a large amount of ordinary UHD material.
fn range_from_transfer(v: u64) -> DynamicRange {
    match v {
        TRANSFER_PQ => DynamicRange::Hdr10,
        TRANSFER_HLG => DynamicRange::Hlg,
        TRANSFER_BT709 | TRANSFER_BT601 | TRANSFER_SRGB | TRANSFER_BT2020_10
        | TRANSFER_BT2020_12 => DynamicRange::Sdr,
        _ => DynamicRange::Unknown,
    }
}

/// One element's header: its id, how many bytes the header took, and the size of
/// its body (`None` for the "unknown size" encoding, which `Segment` often uses).
struct Header {
    id: u32,
    body: Option<u64>,
}

/// Read an EBML element id (the leading bits give the length, and unlike a data
/// size the marker bit is *kept* — ids are compared as written).
fn read_id<R: Read>(r: &mut R) -> Option<u32> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first).ok()?;
    let len = match first[0].leading_zeros() {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        // A byte of 0x00 has no marker bit: not a valid id, and continuing would
        // walk garbage.
        _ => return None,
    };
    let mut id = u32::from(first[0]);
    for _ in 1..len {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        id = (id << 8) | u32::from(b[0]);
    }
    Some(id)
}

/// Read an EBML data size. The marker bit is stripped; an all-ones payload is
/// the "unknown size" encoding and yields `None`.
fn read_size<R: Read>(r: &mut R) -> Option<Option<u64>> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first).ok()?;
    let len = usize::try_from(first[0].leading_zeros()).ok()? + 1;
    if len > 8 {
        return None;
    }
    // `0xFFu8 >> 8` overflows, and len == 8 is the ordinary encoding for a
    // 1-byte-marker size (first byte 0x01) — so the mask is computed rather than
    // shifted. The synthetic tests all used short sizes and never reached it;
    // the real fixture did, immediately.
    let mask: u8 = if len >= 8 { 0 } else { 0xFF >> len };
    let mut value = u64::from(first[0] & mask);
    let mut all_ones = first[0] & mask == mask;
    for _ in 1..len {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        all_ones &= b[0] == 0xFF;
        value = (value << 8) | u64::from(b[0]);
    }
    Some(if all_ones { None } else { Some(value) })
}

fn read_header<R: Read>(r: &mut R) -> Option<Header> {
    let id = read_id(r)?;
    let body = read_size(r)?;
    Some(Header { id, body })
}

/// Read an unsigned integer body of `size` bytes (EBML uints are big-endian and
/// variable width).
fn read_uint<R: Read>(r: &mut R, size: u64) -> Option<u64> {
    if size > 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    let n = usize::try_from(size).ok()?;
    r.read_exact(&mut buf[..n]).ok()?;
    Some(
        buf[..n]
            .iter()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b)),
    )
}

/// Walk the children of the element the reader is positioned at, descending into
/// `descend` and returning as soon as `found` yields a value.
///
/// `end` bounds the scan: `None` means "until the reader runs out", which is
/// what an unknown-size `Segment` needs.
fn scan<R: Read + Seek>(
    r: &mut R,
    end: Option<u64>,
    depth: u8,
    want: &[u32],
) -> Option<DynamicRange> {
    // The nesting we care about is five deep; anything past that is a malformed
    // file walking us in circles.
    if depth > 6 {
        return None;
    }
    loop {
        let pos = r.stream_position().ok()?;
        if end.is_some_and(|e| pos >= e) {
            return None;
        }
        let h = read_header(r)?;
        let after_header = r.stream_position().ok()?;

        // Tracks always precedes the first Cluster, so once media begins there is
        // nothing left to find and reading on would mean reading the body.
        if h.id == CLUSTER {
            return None;
        }

        if h.id == TRANSFER_CHARACTERISTICS && want.contains(&TRANSFER_CHARACTERISTICS) {
            let size = h.body?;
            return read_uint(r, size).map(range_from_transfer);
        }

        let descend = matches!(h.id, SEGMENT | TRACKS | TRACK_ENTRY | VIDEO | COLOUR);
        if descend {
            let child_end = h.body.map(|len| after_header + len);
            if let Some(found) = scan(r, child_end, depth + 1, want) {
                return Some(found);
            }
            // Nothing in this subtree; carry on with the next sibling. An
            // unknown-size child we could not resolve leaves the cursor wherever
            // the failed scan stopped, so there is nothing sane to seek to.
            match child_end {
                Some(e) => {
                    r.seek(SeekFrom::Start(e)).ok()?;
                }
                None => return None,
            }
        } else {
            // Skip the whole subtree without reading it.
            let len = h.body?;
            r.seek(SeekFrom::Start(after_header + len)).ok()?;
        }
    }
}

/// The dynamic range declared by the first video track that declares one.
///
/// `None` means no colour metadata was found — the file is not saying, which is
/// different from saying SDR.
pub fn dynamic_range<R: Read + Seek>(r: &mut R) -> Option<DynamicRange> {
    r.seek(SeekFrom::Start(0)).ok()?;
    scan(r, None, 0, &[TRANSFER_CHARACTERISTICS])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// EBML element: id bytes, then a 1-byte size, then the body.
    fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        assert!(body.len() < 0x7F, "test helper only emits 1-byte sizes");
        v.push(0x80 | u8::try_from(body.len()).unwrap());
        v.extend_from_slice(body);
        v
    }

    /// A minimal Segment → Tracks → TrackEntry → Video → Colour → Transfer
    /// nesting carrying `transfer`.
    fn mkv_with_transfer(transfer: u8) -> Vec<u8> {
        let tc = el(&[0x55, 0xBA], &[transfer]);
        let colour = el(&[0x55, 0xB0], &tc);
        let video = el(&[0xE0], &colour);
        let entry = el(&[0xAE], &video);
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &entry);
        el(&[0x18, 0x53, 0x80, 0x67], &tracks)
    }

    fn range_of(bytes: &[u8]) -> Option<DynamicRange> {
        dynamic_range(&mut Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn maps_the_transfer_function_to_a_dynamic_range() {
        assert_eq!(range_of(&mkv_with_transfer(16)), Some(DynamicRange::Hdr10));
        assert_eq!(range_of(&mkv_with_transfer(18)), Some(DynamicRange::Hlg));
        assert_eq!(range_of(&mkv_with_transfer(1)), Some(DynamicRange::Sdr));
    }

    #[test]
    fn bt2020_transfer_functions_are_sdr_not_hdr() {
        // 14 and 15 are BT.2020 10-bit and 12-bit: *wide gamut*, not HDR.
        // Reading "2020" as HDR is the obvious mistake and would mislabel a
        // large amount of ordinary UHD material as HDR.
        assert_eq!(range_of(&mkv_with_transfer(14)), Some(DynamicRange::Sdr));
        assert_eq!(range_of(&mkv_with_transfer(15)), Some(DynamicRange::Sdr));
    }

    #[test]
    fn an_unmapped_transfer_is_unknown_not_sdr() {
        // Present but unrecognised is a different answer from absent, and from
        // a confident SDR.
        assert_eq!(range_of(&mkv_with_transfer(7)), Some(DynamicRange::Unknown));
    }

    #[test]
    fn a_file_that_declares_nothing_returns_none() {
        // Video track with no Colour child at all. `None` means "not saying" —
        // it must not become SDR, or a release correctly labelled HDR would be
        // contradicted on the strength of no evidence.
        let video = el(&[0xE0], &el(&[0xB0], &[0x02, 0x80])); // PixelWidth only
        let entry = el(&[0xAE], &video);
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &entry);
        let seg = el(&[0x18, 0x53, 0x80, 0x67], &tracks);
        assert_eq!(range_of(&seg), None);
    }

    #[test]
    fn skips_sibling_subtrees_to_reach_tracks() {
        // Info before Tracks, as every real file has. The walk must step over it
        // by size rather than reading into it and losing alignment.
        let info = el(&[0x15, 0x49, 0xA9, 0x66], &[0xFF; 8]);
        let tc = el(&[0x55, 0xBA], &[16]);
        let colour = el(&[0x55, 0xB0], &tc);
        let video = el(&[0xE0], &colour);
        let entry = el(&[0xAE], &video);
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &entry);
        let mut body = info;
        body.extend_from_slice(&tracks);
        let seg = el(&[0x18, 0x53, 0x80, 0x67], &body);
        assert_eq!(range_of(&seg), Some(DynamicRange::Hdr10));
    }

    #[test]
    fn stops_at_the_first_cluster_rather_than_reading_the_body() {
        // A Cluster before Tracks should end the scan: past the first Cluster
        // there is no Tracks to find, and continuing would mean reading the
        // media body — the cost this whole approach exists to avoid.
        let cluster = el(&[0x1F, 0x43, 0xB6, 0x75], &[0x00; 4]);
        let tc = el(&[0x55, 0xBA], &[16]);
        let colour = el(&[0x55, 0xB0], &tc);
        let video = el(&[0xE0], &colour);
        let entry = el(&[0xAE], &video);
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &entry);
        let mut body = cluster;
        body.extend_from_slice(&tracks);
        let seg = el(&[0x18, 0x53, 0x80, 0x67], &body);
        assert_eq!(
            range_of(&seg),
            None,
            "the walk read past a Cluster to find Tracks"
        );
    }

    #[test]
    fn handles_every_data_size_width_without_overflowing() {
        // A size byte of 0x01 is the 8-byte-width marker, which made the mask
        // `0xFFu8 >> 8` and panicked in debug. Every width from 1 to 8 must
        // parse, and a width past 8 must be refused rather than shifted.
        for width in 1u32..=8 {
            let marker = 0x80u8 >> (width - 1);
            let mut bytes = vec![0x18, 0x53, 0x80, 0x67, marker];
            bytes.extend(std::iter::repeat_n(
                0u8,
                usize::try_from(width).unwrap() - 1,
            ));
            // Truncated body, so the answer is None — the point is that it does
            // not panic on the way there.
            assert_eq!(range_of(&bytes), None, "width {width}");
        }
    }

    #[test]
    fn malformed_input_returns_none_rather_than_looping_or_panicking() {
        for bytes in [
            vec![],
            vec![0x00, 0x00, 0x00],             // no valid element id
            vec![0x18, 0x53, 0x80, 0x67],       // truncated before its size
            vec![0x18, 0x53, 0x80, 0x67, 0xFF], // unknown-size Segment, no body
            vec![0xFF; 64],
        ] {
            assert_eq!(range_of(&bytes), None, "{bytes:?}");
        }
    }
}
