//! The parts of a multi-file audiobook (SKADI-T-0738).
//!
//! A [`BookFile`](crate::book_file::BookFile) records one path. For a folder
//! of MP3 parts that is the first part; the importer hardlinked the rest
//! beside it. This module finds those parts again and plans how they are
//! served as one stream, so the audio route, the chapters route, the media
//! probe and the file delete all agree on what "the book's audio" is.
//!
//! Parts are the audio files of the recorded file's folder, in natural order
//! (`Part2` before `Part10`). A single-file book has one part and is served
//! exactly as before. Everything here is blocking filesystem work; call it
//! from `spawn_blocking`.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use skadi_api::ranged::{Segment, content_type_for};
use skadi_core::MediaInfo;
use skadi_media_probe::chapters::ChapterMark;

use crate::import::{is_audio, is_excerpt};

/// The audio parts of the book whose recorded file is `primary`: the
/// non-excerpt audio files of its folder in natural order. A folder that
/// cannot be read, or one where `primary` is the only audio file, gives
/// `[primary]`.
#[must_use]
pub fn book_parts(primary: &Path) -> Vec<PathBuf> {
    let Some(dir) = primary.parent() else {
        return vec![primary.to_path_buf()];
    };
    let Ok(read) = std::fs::read_dir(dir) else {
        return vec![primary.to_path_buf()];
    };
    let mut parts: Vec<PathBuf> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_audio(p))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_none_or(|n| !is_excerpt(n))
        })
        .collect();
    if !parts.iter().any(|p| p == primary) {
        // The recorded file is the one truth the row has; never drop it.
        parts.push(primary.to_path_buf());
    }
    parts.sort_by(|a, b| natural_cmp(&name_of(a), &name_of(b)));
    parts
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Natural order: digit runs compare by value, the rest case-insensitively,
/// so `Part2` sorts before `Part10` and `01.mp3` before `2.mp3`.
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut x = a.chars().peekable();
    let mut y = b.chars().peekable();
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(cx), Some(cy)) if cx.is_ascii_digit() && cy.is_ascii_digit() => {
                let nx = take_digits(&mut x);
                let ny = take_digits(&mut y);
                let ord = nx.len().cmp(&ny.len()).then_with(|| nx.cmp(&ny));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(cx), Some(cy)) => {
                let ord = cx.to_lowercase().cmp(cy.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    // Leading zeros do not change the value.
    let trimmed = s.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// How the parts are served as one body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinPlan {
    /// The byte runs, in playing order.
    pub segments: Vec<Segment>,
    /// The newest modification time across the parts: the `If-Range`
    /// validator, so a part replaced mid-download invalidates the resume.
    pub last_modified: Option<SystemTime>,
    /// Content type of the stream.
    pub content_type: &'static str,
}

impl JoinPlan {
    /// Total body length.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.segments.iter().map(|s| s.len).sum()
    }
}

/// Plan the body for `parts`. One part is served whole, as it was before
/// parts existed. Several MP3 parts are each trimmed to their frames
/// ([`skadi_media_probe::mp3::audio_window`]) and joined. Several parts of
/// any other container cannot be joined by concatenation; the first is served
/// whole, which is what the route did before, and a warning says so.
pub fn join_plan(parts: &[PathBuf]) -> std::io::Result<JoinPlan> {
    let Some(first) = parts.first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "audiobook has no parts",
        ));
    };
    let all_mp3 = parts.iter().all(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
    });
    let serve: &[PathBuf] = if parts.len() > 1 && !all_mp3 {
        tracing::warn!(
            parts = parts.len(),
            first = %first.display(),
            "multi-part audiobook is not MP3; serving the first part only"
        );
        std::slice::from_ref(first)
    } else {
        parts
    };
    let joined = serve.len() > 1;
    let mut segments = Vec::with_capacity(serve.len());
    let mut last_modified: Option<SystemTime> = None;
    for p in serve {
        let meta = std::fs::metadata(p)?;
        let len = meta.len();
        let (start, end) = if joined {
            skadi_media_probe::mp3::audio_window(p)?
        } else {
            (0, len)
        };
        segments.push(Segment {
            path: p.clone(),
            offset: start,
            len: end.saturating_sub(start),
        });
        if let Ok(m) = meta.modified() {
            last_modified = Some(last_modified.map_or(m, |cur| cur.max(m)));
        }
    }
    Ok(JoinPlan {
        segments,
        last_modified,
        content_type: content_type_for(first),
    })
}

/// One chapter per part, titled from the file name, timed by each part's own
/// duration. The chapters of a joined book are its parts: a per-chapter rip
/// gets its chapters back, and a four-part book gets four marks.
#[must_use]
pub fn part_chapters(parts: &[PathBuf]) -> Vec<ChapterMark> {
    let mut at = 0.0f64;
    parts
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let d = skadi_media_probe::mp3::duration_secs(p).unwrap_or(0.0);
            let title = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("Part {}", index + 1));
            let mark = ChapterMark {
                index,
                title,
                start_secs: at,
                end_secs: at + d,
            };
            at += d;
            mark
        })
        .collect()
}

/// `info`, probed from the first part, widened to the whole book: duration
/// and size summed over `parts`, the overall bitrate derived from those.
#[must_use]
pub fn joined_media_info(mut info: MediaInfo, parts: &[PathBuf]) -> MediaInfo {
    if parts.len() < 2 {
        return info;
    }
    let secs: f64 = parts
        .iter()
        .filter_map(|p| skadi_media_probe::mp3::duration_secs(p))
        .sum();
    let bytes: u64 = parts
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    if secs > 0.0 {
        info.duration_secs = Some(secs.round() as u32);
    }
    if bytes > 0 {
        info.size_bytes = Some(bytes);
    }
    if secs > 0.0 && bytes > 0 {
        info.overall_bitrate_kbps = Some((bytes as f64 * 8.0 / secs / 1000.0).round() as u32);
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 Layer III, 64 kbps, 44.1 kHz, mono: 208-byte frames.
    fn frames(n: usize, fill: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(n * 208);
        for _ in 0..n {
            v.extend_from_slice(&[0xFF, 0xFB, 0x50, 0xC0]);
            v.resize(v.len() + 204, fill);
        }
        v
    }

    fn id3v1() -> Vec<u8> {
        let mut t = vec![0u8; 128];
        t[0..3].copy_from_slice(b"TAG");
        t
    }

    #[test]
    fn natural_order_counts_numbers_by_value() {
        let mut names = vec![
            "Part10.mp3",
            "Part2.mp3",
            "Part1.mp3",
            "part3.mp3",
            "Part01.mp3",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        // `Part01` and `Part1` tie on value; the tie breaks on the raw string.
        assert_eq!(
            names,
            [
                "Part01.mp3",
                "Part1.mp3",
                "Part2.mp3",
                "part3.mp3",
                "Part10.mp3"
            ]
        );
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
        assert_eq!(natural_cmp("10 Holden", "9 Miller"), Ordering::Greater);
    }

    #[test]
    fn parts_are_the_folders_audio_in_order_and_keep_the_primary() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "Book-Part10.mp3",
            "Book-Part2.mp3",
            "Book-Part1.mp3",
            "cover.jpg",
            "sample.mp3",
            "notes.txt",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let primary = dir.path().join("Book-Part1.mp3");
        let names: Vec<String> = book_parts(&primary).iter().map(|p| name_of(p)).collect();
        assert_eq!(
            names,
            ["Book-Part1.mp3", "Book-Part2.mp3", "Book-Part10.mp3"]
        );

        // A recorded file that is gone from disk is still the one part.
        let gone = dir.path().join("sub").join("missing.mp3");
        assert_eq!(book_parts(&gone), vec![gone.clone()]);
        // A single file alone in its folder is one part.
        let single = tempfile::tempdir().unwrap();
        let m4b = single.path().join("book.m4b");
        std::fs::write(&m4b, b"x").unwrap();
        assert_eq!(book_parts(&m4b), vec![m4b]);
    }

    #[test]
    fn a_single_part_is_served_whole_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("book.mp3");
        let mut bytes = frames(3, 1);
        bytes.extend(id3v1());
        std::fs::write(&p, &bytes).unwrap();
        let plan = join_plan(std::slice::from_ref(&p)).unwrap();
        assert_eq!(
            plan.segments,
            vec![Segment {
                path: p,
                offset: 0,
                len: bytes.len() as u64
            }]
        );
        assert_eq!(plan.content_type, "audio/mpeg");
        assert!(plan.last_modified.is_some());
    }

    #[test]
    fn several_mp3_parts_are_trimmed_and_joined() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.mp3");
        let b = dir.path().join("b.mp3");
        let mut a_bytes = frames(2, 1);
        a_bytes.extend(id3v1());
        std::fs::write(&a, &a_bytes).unwrap();
        std::fs::write(&b, frames(3, 2)).unwrap();
        let plan = join_plan(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(
            plan.segments,
            vec![
                Segment {
                    path: a,
                    offset: 0,
                    len: 2 * 208
                },
                Segment {
                    path: b,
                    offset: 0,
                    len: 3 * 208
                },
            ]
        );
        assert_eq!(plan.total(), 5 * 208);
    }

    #[test]
    fn mixed_containers_fall_back_to_the_first_part() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.m4b");
        let b = dir.path().join("b.m4b");
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bb").unwrap();
        let plan = join_plan(&[a.clone(), b]).unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].path, a);
        assert_eq!(plan.total(), 4);
        assert_eq!(plan.content_type, "audio/mp4");
        assert!(join_plan(&[]).is_err());
        assert!(join_plan(&[dir.path().join("missing.mp3")]).is_err());
    }

    #[test]
    fn chapters_and_media_info_follow_the_parts() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("01 Intro.mp3");
        let b = dir.path().join("02 Body.mp3");
        // 400 frames is 10.45 s; 200 is 5.22 s.
        std::fs::write(&a, frames(400, 0)).unwrap();
        std::fs::write(&b, frames(200, 0)).unwrap();
        let parts = vec![a.clone(), b.clone()];

        let ch = part_chapters(&parts);
        assert_eq!(ch.len(), 2);
        assert_eq!((ch[0].index, ch[0].title.as_str()), (0, "01 Intro"));
        assert_eq!((ch[1].index, ch[1].title.as_str()), (1, "02 Body"));
        assert_eq!(ch[0].start_secs, 0.0);
        assert!((ch[0].end_secs - 10.45).abs() < 0.2, "{}", ch[0].end_secs);
        assert_eq!(ch[1].start_secs, ch[0].end_secs);
        assert!((ch[1].end_secs - 15.67).abs() < 0.3, "{}", ch[1].end_secs);

        let info = joined_media_info(
            MediaInfo {
                duration_secs: Some(10),
                size_bytes: Some(400 * 208),
                ..Default::default()
            },
            &parts,
        );
        assert_eq!(info.duration_secs, Some(16));
        assert_eq!(info.size_bytes, Some(600 * 208));
        assert_eq!(info.overall_bitrate_kbps, Some(64));

        // One part: the probe's own answer stands.
        let one = joined_media_info(
            MediaInfo {
                duration_secs: Some(10),
                ..Default::default()
            },
            &parts[..1],
        );
        assert_eq!(one.duration_secs, Some(10));
        assert_eq!(one.size_bytes, None);
    }
}
