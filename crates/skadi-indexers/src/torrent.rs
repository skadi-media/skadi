//! Reading a `.torrent` file well enough to identify it (SKADI-T-0497).
//!
//! A tracker behind Cloudflare, or one that gates downloads on a login, answers a
//! bare `.torrent` link with an HTML challenge or sign-in page — with HTTP 200.
//! Handing that to the download client produced only "error decoding torrent",
//! naming neither the link nor the reason.
//!
//! The indexer already holds the session that *can* fetch the file (cookies, and
//! FlareSolverr for challenges). So the grab resolves the link through that
//! client and turns the bytes into a magnet, which needs exactly one thing from
//! the torrent: its info-hash, the SHA-1 of the `info` dictionary's **raw**
//! bencoded bytes. Re-encoding a parsed structure would not do — the hash is over
//! the original byte span, so this scans for that span instead of decoding into
//! owned values.

use sha1_smol::Sha1;

/// The SHA-1 info-hash of a `.torrent` file, lowercase hex.
///
/// `None` when the bytes are not a bencoded dictionary containing an `info` key —
/// which is how a challenge page, a login page or a truncated download is caught.
#[must_use]
pub fn infohash_from_torrent(bytes: &[u8]) -> Option<String> {
    let (start, end) = info_span(bytes)?;
    let mut h = Sha1::new();
    h.update(&bytes[start..end]);
    Some(h.digest().to_string())
}

/// Byte range of the value under the top-level `info` key.
fn info_span(bytes: &[u8]) -> Option<(usize, usize)> {
    // The file must be a dictionary.
    if bytes.first()? != &b'd' {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b'e' {
        // Keys in a dictionary are always byte strings.
        let (key, after_key) = read_bytes(bytes, i)?;
        let value_end = skip_value(bytes, after_key)?;
        if key == b"info" {
            return Some((after_key, value_end));
        }
        i = value_end;
    }
    None
}

/// Read a bencoded byte string at `i`, returning it and the index after it.
fn read_bytes(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let colon = bytes.get(i..)?.iter().position(|b| *b == b':')? + i;
    let len: usize = std::str::from_utf8(bytes.get(i..colon)?)
        .ok()?
        .parse()
        .ok()?;
    let start = colon + 1;
    let end = start.checked_add(len)?;
    if end > bytes.len() {
        return None;
    }
    Some((&bytes[start..end], end))
}

/// Index just past the bencoded value starting at `i`.
fn skip_value(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes.get(i)? {
        // Integer: i<digits>e
        b'i' => bytes
            .get(i..)?
            .iter()
            .position(|b| *b == b'e')
            .map(|p| i + p + 1),
        // List or dictionary: recurse until the matching terminator.
        b'l' | b'd' => {
            let mut j = i + 1;
            while *bytes.get(j)? != b'e' {
                j = skip_value(bytes, j)?;
            }
            Some(j + 1)
        }
        // Byte string: <len>:<bytes>
        b'0'..=b'9' => read_bytes(bytes, i).map(|(_, end)| end),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but real torrent: `d8:announce…4:infod…ee`.
    fn torrent(info: &[u8]) -> Vec<u8> {
        let mut v = b"d8:announce19:http://tracker.test4:info".to_vec();
        v.extend_from_slice(info);
        v.extend_from_slice(b"e");
        v
    }

    #[test]
    fn the_hash_is_over_the_raw_info_bytes() {
        let info = b"d6:lengthi1024e4:name8:file.mkv12:piece lengthi16384ee";
        let bytes = torrent(info);
        let got = infohash_from_torrent(&bytes).expect("a torrent");

        // The expected value is the SHA-1 of the info dict's own bytes — computed
        // here from the same slice, which is the property that matters: the hash
        // must not depend on anything outside that span.
        let mut h = Sha1::new();
        h.update(info);
        assert_eq!(got, h.digest().to_string());
        assert_eq!(got.len(), 40, "lowercase hex sha1");
    }

    #[test]
    fn a_challenge_page_is_not_a_torrent() {
        // The exact shape SKADI-T-0497 was filed for: HTTP 200, HTML body.
        assert_eq!(
            infohash_from_torrent(b"<html><title>Just a moment...</title></html>"),
            None
        );
        assert_eq!(infohash_from_torrent(b""), None);
        // Bencode, but not a torrent — no `info` key.
        assert_eq!(infohash_from_torrent(b"d8:announce3:abce"), None);
    }

    #[test]
    fn nested_structures_before_info_are_skipped_correctly() {
        // `announce-list` is a list of lists, and `creation date` an integer:
        // both must be stepped over without confusing the scanner.
        let info = b"d4:name3:abce";
        let mut v =
            b"d13:announce-listll19:http://tracker.testee13:creation datei1700000000e4:info"
                .to_vec();
        v.extend_from_slice(info);
        v.extend_from_slice(b"e");
        let mut h = Sha1::new();
        h.update(info);
        assert_eq!(infohash_from_torrent(&v), Some(h.digest().to_string()));
    }

    #[test]
    fn a_truncated_file_does_not_panic_or_over_read() {
        let full = torrent(b"d4:name3:abce");
        for cut in 0..full.len() {
            let _ = infohash_from_torrent(&full[..cut]);
        }
    }
}
