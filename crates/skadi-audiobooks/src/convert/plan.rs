//! The pure half of the M4B conversion (SKADI-T-0139): which files go in, in
//! what order, where the chapters fall, and the text files ffmpeg reads.
//!
//! Nothing here touches the filesystem or spawns a process, so every decision
//! the conversion makes about chapters is testable without ffmpeg.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// One chapter mark of the output file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    pub start: Duration,
    pub end: Duration,
}

/// One source audio file with what the probe read from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub path: PathBuf,
    /// Exact running time; `None` when the probe could not read the file.
    pub duration: Option<Duration>,
    /// The file's title tag, if any.
    pub title: Option<String>,
}

/// Where the chapters came from, for the trace message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChapterSource {
    /// A `.cue` sheet next to the audio.
    Cue,
    /// One chapter per source file.
    Files,
    /// The single source file's own chapters (kept by ffmpeg).
    Embedded,
}

impl ChapterSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Cue => "cue sheet",
            Self::Files => "file names",
            Self::Embedded => "embedded tags",
        }
    }
}

/// Sort key that orders `2.mp3` before `10.mp3`: runs of digits compare as
/// numbers, everything else case-insensitively.
#[must_use]
pub fn natural_key(name: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n: u64 = 0;
            while let Some(&d) = chars.peek() {
                let Some(v) = d.to_digit(10) else { break };
                n = n.saturating_mul(10).saturating_add(u64::from(v));
                chars.next();
            }
            out.push((0, n, String::new()));
        } else {
            let mut s = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    break;
                }
                s.extend(d.to_lowercase());
                chars.next();
            }
            out.push((1, 0, s));
        }
    }
    out
}

/// Sort paths by their file name in natural order.
pub fn sort_natural(paths: &mut [PathBuf]) {
    paths.sort_by_cached_key(|p| {
        natural_key(p.file_name().and_then(|n| n.to_str()).unwrap_or_default())
    });
}

/// A chapter title from a file: the file name with `_` and `.` read as spaces.
/// A name that is only a track number (`01.mp3`) says nothing, so the title tag
/// is used instead, and failing that `Chapter <n>`.
#[must_use]
pub fn title_for(source: &Source, number: usize) -> String {
    let stem = source
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let cleaned = stem
        .replace(['_', '.'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.chars().any(char::is_alphabetic) {
        return cleaned;
    }
    source
        .title
        .clone()
        .unwrap_or_else(|| format!("Chapter {number}"))
}

/// One chapter per source file. `None` when any duration is unknown: a chapter
/// mark placed after an unknown length would point at the wrong audio.
#[must_use]
pub fn chapters_from_files(sources: &[Source]) -> Option<Vec<Chapter>> {
    let mut at = Duration::ZERO;
    let mut out = Vec::with_capacity(sources.len());
    for (i, s) in sources.iter().enumerate() {
        let d = s.duration.filter(|d| !d.is_zero())?;
        out.push(Chapter {
            title: title_for(s, i + 1),
            start: at,
            end: at + d,
        });
        at += d;
    }
    Some(out)
}

/// One track of a cue sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueTrack {
    /// The `FILE` the track is in, when the sheet names one.
    pub file: Option<String>,
    pub title: Option<String>,
    /// `INDEX 01`, relative to the start of `file`.
    pub start: Duration,
}

/// Parse the tracks of a cue sheet. Lenient: unknown commands are ignored, and
/// a track without an `INDEX 01` is dropped. Empty when nothing usable is in it.
#[must_use]
pub fn parse_cue(text: &str) -> Vec<CueTrack> {
    let mut tracks = Vec::new();
    let mut file: Option<String> = None;
    let mut current: Option<(Option<String>, Option<Duration>)> = None;
    let flush = |current: &mut Option<(Option<String>, Option<Duration>)>,
                 file: &Option<String>,
                 tracks: &mut Vec<CueTrack>| {
        if let Some((title, Some(start))) = current.take() {
            tracks.push(CueTrack {
                file: file.clone(),
                title,
                start,
            });
        }
    };
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        match cmd.to_ascii_uppercase().as_str() {
            "FILE" => {
                flush(&mut current, &file, &mut tracks);
                file = Some(quoted_or_first(rest));
            }
            "TRACK" => {
                flush(&mut current, &file, &mut tracks);
                current = Some((None, None));
            }
            "TITLE" => {
                if let Some((title, _)) = current.as_mut() {
                    let t = quoted_or_first(rest);
                    if !t.is_empty() {
                        *title = Some(t);
                    }
                }
            }
            "INDEX" => {
                let mut parts = rest.split_whitespace();
                if parts.next() == Some("01")
                    && let Some(at) = parts.next().and_then(parse_cue_time)
                    && let Some((_, start)) = current.as_mut()
                {
                    *start = Some(at);
                }
            }
            _ => {}
        }
    }
    flush(&mut current, &file, &mut tracks);
    tracks
}

/// `"Some Name.mp3" MP3` → `Some Name.mp3`; an unquoted value is its first word.
fn quoted_or_first(rest: &str) -> String {
    let rest = rest.trim();
    if let Some(body) = rest.strip_prefix('"') {
        return body.split('"').next().unwrap_or_default().to_string();
    }
    rest.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `mm:ss:ff`, with 75 frames a second.
fn parse_cue_time(s: &str) -> Option<Duration> {
    let mut it = s.split(':');
    let m: u64 = it.next()?.parse().ok()?;
    let sec: u64 = it.next()?.parse().ok()?;
    let f: u64 = it.next()?.parse().ok()?;
    if it.next().is_some() || sec >= 60 || f >= 75 {
        return None;
    }
    Some(Duration::from_millis((m * 60 + sec) * 1000 + f * 1000 / 75))
}

/// Chapters from a cue sheet over `sources`. Each track's `FILE` is matched to
/// a source by name (case-insensitive); a sheet with no `FILE`, over a single
/// source, applies to that source. `None` when the sheet does not fit the files:
/// a `FILE` that is not among them, an unknown duration, or starts that do not
/// move forward. The caller then falls back to other chapter sources.
#[must_use]
pub fn chapters_from_cue(tracks: &[CueTrack], sources: &[Source]) -> Option<Vec<Chapter>> {
    if tracks.is_empty() {
        return None;
    }
    // Offset of each source within the joined output.
    let mut offsets = Vec::with_capacity(sources.len());
    let mut at = Duration::ZERO;
    for s in sources {
        offsets.push(at);
        at += s.duration.filter(|d| !d.is_zero())?;
    }
    let total = at;
    let index_of = |name: &Option<String>| -> Option<usize> {
        match name {
            None if sources.len() == 1 => Some(0),
            None => None,
            Some(n) => {
                // A cue sheet often names the file with a path or another
                // extension (a sheet written for the `.wav` of a rip).
                let want = Path::new(n)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(n)
                    .to_ascii_lowercase();
                sources.iter().position(|s| {
                    s.path
                        .file_stem()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| x.to_ascii_lowercase() == want)
                })
            }
        }
    };
    let mut starts = Vec::with_capacity(tracks.len());
    for (i, t) in tracks.iter().enumerate() {
        let src = index_of(&t.file)?;
        let start = offsets[src] + t.start;
        if start >= total
            || starts
                .last()
                .is_some_and(|(s, _): &(Duration, String)| start <= *s)
        {
            return None;
        }
        let title = t
            .title
            .clone()
            .unwrap_or_else(|| format!("Chapter {}", i + 1));
        starts.push((start, title));
    }
    let mut out = Vec::with_capacity(starts.len());
    for (i, (start, title)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(total, |(s, _)| *s);
        out.push(Chapter {
            title: title.clone(),
            start: *start,
            end,
        });
    }
    Some(out)
}

/// Choose the chapters: a cue sheet when one fits, else one per file for a
/// multi-file book, else the single file's own chapters (`Embedded`, with no
/// marks of our own). An empty `Vec` is never returned for `Cue` or `Files`.
#[must_use]
pub fn plan_chapters(cue: Option<&str>, sources: &[Source]) -> (ChapterSource, Vec<Chapter>) {
    if let Some(text) = cue
        && let Some(ch) = chapters_from_cue(&parse_cue(text), sources)
    {
        return (ChapterSource::Cue, ch);
    }
    if sources.len() > 1
        && let Some(ch) = chapters_from_files(sources)
    {
        return (ChapterSource::Files, ch);
    }
    (ChapterSource::Embedded, Vec::new())
}

/// Book-level tags for the output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub author: Option<String>,
}

/// Escape a value for ffmpeg's ffmetadata format: `=`, `;`, `#`, `\` and
/// newlines are backslash-escaped.
#[must_use]
pub fn ffmeta_escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '=' | ';' | '#' | '\\' | '\n' => {
                out.push('\\');
                out.push(c);
            }
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

/// The ffmetadata file: the book's tags, then one `[CHAPTER]` per chapter in
/// milliseconds.
#[must_use]
pub fn ffmetadata(tags: &Tags, chapters: &[Chapter]) -> String {
    let mut s = String::from(";FFMETADATA1\n");
    s.push_str(&format!("title={}\n", ffmeta_escape(&tags.title)));
    s.push_str(&format!("album={}\n", ffmeta_escape(&tags.title)));
    if let Some(a) = &tags.author {
        s.push_str(&format!("artist={}\n", ffmeta_escape(a)));
        s.push_str(&format!("album_artist={}\n", ffmeta_escape(a)));
    }
    s.push_str("genre=Audiobook\n");
    for c in chapters {
        s.push_str(&format!(
            "\n[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n",
            c.start.as_millis(),
            c.end.as_millis(),
            ffmeta_escape(&c.title)
        ));
    }
    s
}

/// The concat-demuxer list for `paths`. Single quotes in a path are closed,
/// escaped and reopened, the way the format requires.
#[must_use]
pub fn concat_list(paths: &[PathBuf]) -> String {
    let mut s = String::from("ffconcat version 1.0\n");
    for p in paths {
        let quoted = p.to_string_lossy().replace('\'', r"'\''");
        s.push_str(&format!("file '{quoted}'\n"));
    }
    s
}

/// How the audio is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audio {
    /// Re-encode to AAC at this many kbps.
    Encode(u32),
    /// The sources are AAC already: copy the stream, no re-encode.
    Copy,
}

/// The AAC bitrate for a re-encode: 128 kbps for a source at 112 kbps or more,
/// else 64. Those are the two M4B tiers a converted book lands on, and a lossy
/// re-encode gains nothing above its source's rate.
#[must_use]
pub fn target_kbps(source_kbps: Option<u32>) -> u32 {
    match source_kbps {
        Some(k) if k >= 112 => 128,
        _ => 64,
    }
}

/// `true` when every source is an MP4-family file (`.m4a`/`.mp4`), whose AAC
/// stream can be copied into the M4B without a re-encode.
#[must_use]
pub fn can_copy(paths: &[PathBuf]) -> bool {
    !paths.is_empty()
        && paths.iter().all(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "m4a" | "mp4"))
        })
}

/// What one ffmpeg run reads and writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    /// The single source file, or the concat list for several.
    pub input: PathBuf,
    /// `input` is a concat list.
    pub concat: bool,
    pub metadata: PathBuf,
    /// Take chapters from the metadata file (`true`) or from the input itself.
    pub chapters_from_metadata: bool,
    pub audio: Audio,
    pub output: PathBuf,
}

/// The ffmpeg arguments for `inv`. Progress goes to stdout as `key=value`
/// lines; only errors go to stderr.
#[must_use]
pub fn ffmpeg_args(inv: &Invocation) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "error",
        "-y",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    if inv.concat {
        a.extend(["-f", "concat", "-safe", "0"].map(String::from));
    }
    a.push("-i".into());
    a.push(inv.input.to_string_lossy().into_owned());
    a.extend(["-f", "ffmetadata", "-i"].map(String::from));
    a.push(inv.metadata.to_string_lossy().into_owned());
    a.extend(["-map", "0:a:0", "-map_metadata", "1", "-map_chapters"].map(String::from));
    a.push(if inv.chapters_from_metadata { "1" } else { "0" }.into());
    match inv.audio {
        Audio::Encode(kbps) => {
            // The native encoder's fast coder (SKADI-T-0139): about 2 to 3 times
            // quicker than the default `twoloop`, at no audible cost for speech at
            // these bitrates.
            a.extend(["-c:a", "aac", "-aac_coder", "fast", "-b:a"].map(String::from));
            a.push(format!("{kbps}k"));
        }
        Audio::Copy => a.extend(["-c:a", "copy"].map(String::from)),
    }
    a.extend(
        [
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-f",
            "ipod",
        ]
        .map(String::from),
    );
    a.push(inv.output.to_string_lossy().into_owned());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(name: &str, secs: u64) -> Source {
        Source {
            path: PathBuf::from(format!("/lib/book/{name}")),
            duration: Some(Duration::from_secs(secs)),
            title: None,
        }
    }

    #[test]
    fn natural_order_puts_two_before_ten() {
        let mut v: Vec<PathBuf> = ["10.mp3", "2.mp3", "1.mp3", "Part 3.mp3", "part 21.mp3"]
            .iter()
            .map(PathBuf::from)
            .collect();
        sort_natural(&mut v);
        let names: Vec<_> = v.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(
            names,
            ["1.mp3", "2.mp3", "10.mp3", "Part 3.mp3", "part 21.mp3"]
        );
    }

    #[test]
    fn titles_come_from_names_then_tags_then_a_number() {
        assert_eq!(
            title_for(&src("01_-_The_Beginning.mp3", 1), 1),
            "01 - The Beginning"
        );
        let mut bare = src("07.mp3", 1);
        assert_eq!(title_for(&bare, 7), "Chapter 7");
        bare.title = Some("The Storm".into());
        assert_eq!(title_for(&bare, 7), "The Storm");
    }

    #[test]
    fn one_chapter_per_file_back_to_back() {
        let ch = chapters_from_files(&[src("a.mp3", 3), src("b.mp3", 4), src("c.mp3", 5)]).unwrap();
        assert_eq!(ch.len(), 3);
        assert_eq!(ch[1].start, Duration::from_secs(3));
        assert_eq!(ch[2].end, Duration::from_secs(12));
        let mut unknown = src("d.mp3", 0);
        unknown.duration = None;
        assert!(chapters_from_files(&[src("a.mp3", 3), unknown]).is_none());
    }

    const SINGLE_CUE: &str = "REM GENRE Audiobook\nPERFORMER \"Sam\"\nFILE \"book.mp3\" MP3\n  TRACK 01 AUDIO\n    TITLE \"Opening\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Middle\"\n    INDEX 01 01:30:37\n  TRACK 03 AUDIO\n    INDEX 01 03:00:00\n";

    #[test]
    fn cue_sheet_over_one_file() {
        let tracks = parse_cue(SINGLE_CUE);
        assert_eq!(tracks.len(), 3);
        assert_eq!(
            tracks[1].start,
            Duration::from_millis(90_000 + 37 * 1000 / 75)
        );
        let ch = chapters_from_cue(&tracks, &[src("book.mp3", 600)]).unwrap();
        assert_eq!(ch.len(), 3);
        assert_eq!(ch[0].title, "Opening");
        assert_eq!(
            ch[2].title, "Chapter 3",
            "a track without a TITLE is numbered"
        );
        assert_eq!(ch[2].end, Duration::from_secs(600));
    }

    #[test]
    fn cue_sheet_over_several_files_uses_file_offsets() {
        let cue = "FILE \"one.wav\" WAVE\nTRACK 01 AUDIO\nTITLE \"A\"\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nTITLE \"B\"\nINDEX 01 00:02:00\nFILE \"two.wav\" WAVE\nTRACK 03 AUDIO\nTITLE \"C\"\nINDEX 01 00:00:00\n";
        let ch =
            chapters_from_cue(&parse_cue(cue), &[src("one.mp3", 5), src("two.mp3", 5)]).unwrap();
        let starts: Vec<_> = ch.iter().map(|c| c.start.as_secs()).collect();
        assert_eq!(starts, [0, 2, 5]);
    }

    #[test]
    fn a_cue_sheet_that_does_not_fit_is_ignored() {
        // Names a file that is not there.
        let cue = "FILE \"other.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
        assert!(chapters_from_cue(&parse_cue(cue), &[src("book.mp3", 60)]).is_none());
        // Starts past the end.
        let late = "FILE \"book.mp3\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 90:00:00\n";
        assert!(chapters_from_cue(&parse_cue(late), &[src("book.mp3", 60)]).is_none());
        // Garbage.
        assert!(parse_cue("not a cue sheet").is_empty());
    }

    #[test]
    fn plan_prefers_cue_then_files_then_embedded() {
        let one = [src("book.mp3", 600)];
        assert_eq!(plan_chapters(Some(SINGLE_CUE), &one).0, ChapterSource::Cue);
        let (kind, ch) = plan_chapters(None, &one);
        assert_eq!(kind, ChapterSource::Embedded);
        assert!(ch.is_empty());
        let many = [src("a.mp3", 3), src("b.mp3", 3)];
        assert_eq!(plan_chapters(Some("junk"), &many).0, ChapterSource::Files);
    }

    #[test]
    fn ffmetadata_escapes_and_lists_chapters() {
        let text = ffmetadata(
            &Tags {
                title: "A=B; #1".into(),
                author: Some("Robin".into()),
            },
            &[Chapter {
                title: "One".into(),
                start: Duration::ZERO,
                end: Duration::from_millis(1500),
            }],
        );
        assert!(text.starts_with(";FFMETADATA1\n"));
        assert!(text.contains(r"title=A\=B\; \#1"));
        assert!(text.contains("artist=Robin\n"));
        assert!(text.contains("[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1500\ntitle=One\n"));
    }

    #[test]
    fn concat_list_quotes_paths() {
        let s = concat_list(&[PathBuf::from("/lib/it's/a.mp3")]);
        assert_eq!(s, "ffconcat version 1.0\nfile '/lib/it'\\''s/a.mp3'\n");
    }

    #[test]
    fn bitrate_and_copy_choices() {
        assert_eq!(target_kbps(None), 64);
        assert_eq!(target_kbps(Some(64)), 64);
        assert_eq!(target_kbps(Some(128)), 128);
        assert!(can_copy(&[PathBuf::from("a.m4a"), PathBuf::from("b.MP4")]));
        assert!(!can_copy(&[PathBuf::from("a.m4a"), PathBuf::from("b.mp3")]));
        assert!(!can_copy(&[]));
    }

    #[test]
    fn args_for_a_multi_file_encode() {
        let a = ffmpeg_args(&Invocation {
            input: "/t/list.txt".into(),
            concat: true,
            metadata: "/t/meta.txt".into(),
            chapters_from_metadata: true,
            audio: Audio::Encode(64),
            output: "/lib/book/.book.m4b.partial".into(),
        });
        let s = a.join(" ");
        assert!(s.contains("-f concat -safe 0 -i /t/list.txt -f ffmetadata -i /t/meta.txt"));
        assert!(s.contains(
            "-map 0:a:0 -map_metadata 1 -map_chapters 1 -c:a aac -aac_coder fast -b:a 64k"
        ));
        assert!(s.ends_with("-f ipod /lib/book/.book.m4b.partial"));
    }
}
