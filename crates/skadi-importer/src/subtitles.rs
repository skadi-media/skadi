//! External subtitle files (SKADI-T-0663).
//!
//! Many releases ship subtitles as separate files rather than tracks inside the
//! video: `Movie.en.srt`, `Movie.en.forced.srt`, or a `Subs/` folder of
//! `2_English.srt`, `3_French.srt`. Before this module the importer placed only
//! the video, so those subtitles never reached the library, and nothing listed
//! or served them.
//!
//! Three jobs, all pure apart from directory reads:
//!
//! - **Import** ([`for_import`], [`sidecar_name`]): find the subtitle files that
//!   belong to a source video and name them for the placed video, keeping
//!   language and `forced` / `sdh` tags.
//! - **Library** ([`beside`]): list the subtitle files next to a placed video.
//!   This runs at request time, so subtitles already in the library — from
//!   another tool, or a hand copy — are found without a rescan.
//! - **Web** ([`to_webvtt`]): browsers load only WebVTT, so SubRip and ASS/SSA
//!   are converted. ASS styling is dropped; the text survives.

use std::path::{Path, PathBuf};

/// Subtitle formats carried and served. `.sub`/`.idx` (VobSub) are images and
/// need both files; not handled.
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "vtt", "ass", "ssa"];

/// Folder names releases put subtitles in.
const SUB_FOLDERS: &[&str] = &["subs", "sub", "subtitles", "subtitle"];

/// What a subtitle file's name says about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubTags {
    /// ISO 639-1 code when recognised (`"en"`), else `None`.
    pub language: Option<String>,
    /// Only the foreign-language parts are subtitled.
    pub forced: bool,
    /// For the deaf and hard of hearing (sound cues included).
    pub sdh: bool,
}

/// One subtitle file next to a library video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleFile {
    pub path: PathBuf,
    pub tags: SubTags,
    /// `srt`, `vtt`, `ass` or `ssa`.
    pub format: String,
}

impl SubtitleFile {
    /// A label a viewer reads: "English", "English (forced)", "Unknown (SDH)".
    #[must_use]
    pub fn label(&self) -> String {
        let name = self
            .tags
            .language
            .as_deref()
            .and_then(language_name)
            .unwrap_or("Unknown");
        let mut extras = Vec::new();
        if self.tags.forced {
            extras.push("forced");
        }
        if self.tags.sdh {
            extras.push("SDH");
        }
        if extras.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({})", extras.join(", "))
        }
    }
}

#[must_use]
pub fn is_subtitle_file(path: &Path) -> bool {
    extension(path).is_some_and(|e| SUBTITLE_EXTENSIONS.contains(&e.as_str()))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Languages recognised by name or code: (ISO 639-1, English name, other spellings).
const LANGUAGES: &[(&str, &str, &[&str])] = &[
    ("en", "English", &["eng", "english"]),
    ("fr", "French", &["fre", "fra", "french", "francais"]),
    ("de", "German", &["ger", "deu", "german", "deutsch"]),
    (
        "es",
        "Spanish",
        &["spa", "spanish", "espanol", "castellano"],
    ),
    ("it", "Italian", &["ita", "italian", "italiano"]),
    ("pt", "Portuguese", &["por", "portuguese", "portugues"]),
    ("nl", "Dutch", &["dut", "nld", "dutch", "nederlands"]),
    ("sv", "Swedish", &["swe", "swedish", "svenska"]),
    ("no", "Norwegian", &["nor", "nob", "norwegian", "norsk"]),
    ("da", "Danish", &["dan", "danish", "dansk"]),
    ("fi", "Finnish", &["fin", "finnish", "suomi"]),
    ("pl", "Polish", &["pol", "polish", "polski"]),
    ("ru", "Russian", &["rus", "russian"]),
    ("ja", "Japanese", &["jpn", "japanese"]),
    ("zh", "Chinese", &["chi", "zho", "chinese"]),
    ("ko", "Korean", &["kor", "korean"]),
    ("ar", "Arabic", &["ara", "arabic"]),
    ("he", "Hebrew", &["heb", "hebrew"]),
    ("tr", "Turkish", &["tur", "turkish"]),
    ("el", "Greek", &["gre", "ell", "greek"]),
    ("cs", "Czech", &["cze", "ces", "czech"]),
    ("hu", "Hungarian", &["hun", "hungarian"]),
    ("ro", "Romanian", &["rum", "ron", "romanian"]),
];

fn language_code(token: &str) -> Option<&'static str> {
    let t = token.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(code, _, others)| *code == t || others.contains(&t.as_str()))
        .map(|(code, _, _)| *code)
}

/// English name for an ISO 639-1 code this module knows.
#[must_use]
pub fn language_name(code: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, name, _)| *name)
}

/// Read tags from the part of a subtitle's name that is not the video's: for
/// `Movie.en.forced.srt` beside `Movie.mkv` that is `en.forced`; for
/// `Subs/2_English.srt` it is `2_English`. The first recognised language wins.
#[must_use]
pub fn tags_from(rest: &str) -> SubTags {
    let mut tags = SubTags::default();
    for token in rest
        .split(['.', '_', '-', ' ', '[', ']', '(', ')'])
        .filter(|t| !t.is_empty())
    {
        match token.to_ascii_lowercase().as_str() {
            "forced" | "foreign" => tags.forced = true,
            "sdh" | "hi" | "cc" => tags.sdh = true,
            t => {
                if tags.language.is_none()
                    && let Some(code) = language_code(t)
                {
                    tags.language = Some(code.to_string());
                }
            }
        }
    }
    tags
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// The part of `sub_stem` after `video_stem`, when the subtitle is named for
/// that video (`Movie` + `.en.forced`), compared case-insensitively.
fn after_video_stem<'a>(sub_stem: &'a str, video_stem: &str) -> Option<&'a str> {
    let head = sub_stem.get(..video_stem.len())?;
    if !head.eq_ignore_ascii_case(video_stem) {
        return None;
    }
    let rest = &sub_stem[video_stem.len()..];
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(['.', '_', '-', ' '])
    }
}

fn read_dir_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    v.sort();
    v
}

fn sub_folders(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| SUB_FOLDERS.contains(&n.to_ascii_lowercase().as_str()))
        })
        .collect();
    v.sort();
    v
}

/// The subtitle files that belong to `video`, a file in a download, with their
/// tags. A subtitle belongs to it when:
///
/// - it sits beside the video and is named for it (`Movie.en.srt`);
/// - it is in a `Subs/` folder beside the video, in a sub-folder named for the
///   video (`Subs/Show.S01E01/2_English.srt`, how season packs ship them); or
/// - the video is the only one in its folder, and the subtitle is beside it or
///   directly in `Subs/` — whatever its name, since there is nothing else it
///   could belong to.
#[must_use]
pub fn for_import(video: &Path) -> Vec<(PathBuf, SubTags)> {
    let Some(dir) = video.parent() else {
        return Vec::new();
    };
    let stem = file_stem(video);
    let beside = read_dir_files(dir);
    let only_video = beside.iter().filter(|p| crate::is_video_file(p)).count() == 1;
    let mut out = Vec::new();
    for f in beside.iter().filter(|p| is_subtitle_file(p)) {
        let s = file_stem(f);
        match after_video_stem(&s, &stem) {
            Some(rest) => out.push((f.clone(), tags_from(rest))),
            None if only_video => out.push((f.clone(), tags_from(&s))),
            None => {}
        }
    }
    for subs in sub_folders(dir) {
        if only_video {
            for f in read_dir_files(&subs)
                .into_iter()
                .filter(|p| is_subtitle_file(p))
            {
                let s = file_stem(&f);
                let rest = after_video_stem(&s, &stem).unwrap_or(&s).to_string();
                out.push((f, tags_from(&rest)));
            }
        }
        let named = subs.join(&stem);
        for f in read_dir_files(&named)
            .into_iter()
            .filter(|p| is_subtitle_file(p))
        {
            let s = file_stem(&f);
            out.push((f, tags_from(&s)));
        }
    }
    out
}

/// Where a subtitle goes beside a placed video:
/// `{video stem}.{lang}[.forced][.sdh].{ext}`. A name already in `taken` gets a
/// counter (`.en.2.srt`) so two English files both survive.
#[must_use]
pub fn sidecar_name(dest_video: &Path, tags: &SubTags, ext: &str, taken: &[PathBuf]) -> PathBuf {
    let dir = dest_video.parent().unwrap_or(Path::new("."));
    let mut parts = vec![file_stem(dest_video)];
    if let Some(l) = &tags.language {
        parts.push(l.clone());
    }
    if tags.forced {
        parts.push("forced".into());
    }
    if tags.sdh {
        parts.push("sdh".into());
    }
    let base = parts.join(".");
    let ext = ext.to_ascii_lowercase();
    let mut candidate = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = dir.join(format!("{base}.{n}.{ext}"));
        n += 1;
    }
    candidate
}

/// Copy (or hard-link) the subtitles that belong to `src_video` beside
/// `dest_video`, named for it. Best-effort: a subtitle that cannot be placed is
/// logged and skipped — the video import already succeeded. An existing file
/// at the target name is replaced, so a re-import refreshes rather than
/// piling up `.en.2.srt` copies. Returns the placed paths.
pub fn carry(src_video: &Path, dest_video: &Path, allow_hardlink: bool) -> Vec<PathBuf> {
    let mut placed: Vec<PathBuf> = Vec::new();
    for (src, tags) in for_import(src_video) {
        let ext = extension(&src).unwrap_or_else(|| "srt".into());
        let dest = sidecar_name(dest_video, &tags, &ext, &placed);
        let _ = std::fs::remove_file(&dest);
        let linked = allow_hardlink && std::fs::hard_link(&src, &dest).is_ok();
        if linked || std::fs::copy(&src, &dest).is_ok() {
            tracing::info!(from = ?src, to = ?dest, "imported subtitle");
            placed.push(dest);
        } else {
            tracing::warn!(from = ?src, to = ?dest, "could not place subtitle; skipped");
        }
    }
    placed
}

/// Subtitle files beside a library video, named for it, in name order. Tags
/// come from what follows the video's stem.
#[must_use]
pub fn beside(video: &Path) -> Vec<SubtitleFile> {
    let Some(dir) = video.parent() else {
        return Vec::new();
    };
    let stem = file_stem(video);
    read_dir_files(dir)
        .into_iter()
        .filter(|p| is_subtitle_file(p))
        .filter_map(|p| {
            let s = file_stem(&p);
            let tags = tags_from(after_video_stem(&s, &stem)?);
            let format = extension(&p)?;
            Some(SubtitleFile {
                path: p,
                tags,
                format,
            })
        })
        .collect()
}

/// One subtitle as the API lists it (SKADI-T-0663).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SubtitleInfo {
    /// Position in [`beside`]'s order; the `{n}` of the file route.
    pub index: usize,
    pub language: Option<String>,
    pub label: String,
    pub forced: bool,
    pub sdh: bool,
    /// The file's own format; `?format=vtt` on the file route converts.
    pub format: String,
}

/// The API listing for a library video's external subtitles.
#[must_use]
pub fn listing(video: &Path) -> Vec<SubtitleInfo> {
    beside(video)
        .into_iter()
        .enumerate()
        .map(|(index, f)| SubtitleInfo {
            index,
            label: f.label(),
            language: f.tags.language,
            forced: f.tags.forced,
            sdh: f.tags.sdh,
            format: f.format,
        })
        .collect()
}

/// Subtitle `n` of a library video as `(content type, UTF-8 text)`, in its own
/// format, or as WebVTT when `webvtt` is asked for. Text is always re-encoded
/// as UTF-8: ExoPlayer and browsers both assume it, and older releases are
/// Windows-1252. `None` when there is no such subtitle or it cannot be read.
#[must_use]
pub fn body(video: &Path, n: usize, webvtt: bool) -> Option<(&'static str, String)> {
    let f = beside(video).into_iter().nth(n)?;
    let text = decode_text(&std::fs::read(&f.path).ok()?);
    if webvtt {
        return Some(("text/vtt; charset=utf-8", to_webvtt(&text, &f.format)));
    }
    let ct = match f.format.as_str() {
        "vtt" => "text/vtt; charset=utf-8",
        "ass" | "ssa" => "text/x-ssa; charset=utf-8",
        _ => "application/x-subrip; charset=utf-8",
    };
    Some((ct, text))
}

/// Subtitle bytes as text. Files are UTF-8 (with or without a BOM) or, from
/// older releases, Windows-1252; anything that is not valid UTF-8 is read as
/// 1252.
#[must_use]
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// Convert subtitle text of `format` to WebVTT, for browsers.
#[must_use]
pub fn to_webvtt(text: &str, format: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    match format {
        "vtt" => text,
        "ass" | "ssa" => ass_to_webvtt(&text),
        _ => srt_to_webvtt(&text),
    }
}

/// SubRip differs from WebVTT in the header and the decimal comma.
fn srt_to_webvtt(text: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for line in text.lines() {
        if line.contains("-->") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// ASS/SSA keeps its cues in `[Events]` as `Dialogue:` lines whose fields are
/// named by the `Format:` line; the text is the last field and may contain
/// commas. Override blocks (`{\i1}`) are dropped, `\N` becomes a line break.
fn ass_to_webvtt(text: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    let mut fields: Vec<String> = Vec::new();
    let mut in_events = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_events = line.eq_ignore_ascii_case("[events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(f) = line.strip_prefix("Format:") {
            fields = f
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect();
            continue;
        }
        let Some(body) = line.strip_prefix("Dialogue:") else {
            continue;
        };
        if fields.is_empty() {
            continue;
        }
        let values: Vec<&str> = body.splitn(fields.len(), ',').collect();
        let get = |name: &str| {
            fields
                .iter()
                .position(|f| f == name)
                .and_then(|i| values.get(i))
                .map(|v| v.trim())
        };
        let (Some(start), Some(end), Some(raw)) = (get("start"), get("end"), get("text")) else {
            continue;
        };
        let (Some(start), Some(end)) = (ass_time(start), ass_time(end)) else {
            continue;
        };
        let cue = strip_ass_overrides(raw)
            .replace("\\N", "\n")
            .replace("\\n", "\n")
            .replace("\\h", " ");
        if cue.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("{start} --> {end}\n{}\n\n", cue.trim()));
    }
    out
}

/// `0:01:02.34` (centiseconds) → `00:01:02.340`.
fn ass_time(t: &str) -> Option<String> {
    let mut parts = t.split(':');
    let h: u32 = parts.next()?.trim().parse().ok()?;
    let m: u32 = parts.next()?.trim().parse().ok()?;
    let (s, cs) = parts.next()?.trim().split_once('.')?;
    let s: u32 = s.parse().ok()?;
    let cs: u32 = cs.parse().ok()?;
    Some(format!("{h:02}:{m:02}:{s:02}.{:03}", cs * 10))
}

fn strip_ass_overrides(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn tags_from_common_names() {
        assert_eq!(
            tags_from("en"),
            SubTags {
                language: Some("en".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            tags_from("eng.forced"),
            SubTags {
                language: Some("en".into()),
                forced: true,
                sdh: false
            }
        );
        assert_eq!(tags_from("2_English").language.as_deref(), Some("en"));
        assert!(tags_from("en.sdh").sdh);
        assert!(tags_from("English.HI").sdh);
        assert_eq!(
            tags_from("Français").language,
            None,
            "non-ASCII names are unknown, not wrong"
        );
        assert_eq!(tags_from("").language, None);
    }

    #[test]
    fn imports_subtitles_named_for_the_video_only() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("Show.S01E01.mkv");
        let b = d.path().join("Show.S01E02.mkv");
        touch(&a);
        touch(&b);
        touch(&d.path().join("Show.S01E01.en.srt"));
        touch(&d.path().join("Show.S01E02.en.srt"));
        touch(&d.path().join("random.srt"));
        let found = for_import(&a);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].0.ends_with("Show.S01E01.en.srt"));
    }

    #[test]
    fn a_lone_video_takes_its_subs_folder() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("Movie.2019.1080p.mkv");
        touch(&v);
        touch(&d.path().join("Subs/2_English.srt"));
        touch(&d.path().join("Subs/3_French.srt"));
        let mut langs: Vec<_> = for_import(&v)
            .into_iter()
            .map(|(_, t)| t.language.unwrap())
            .collect();
        langs.sort();
        assert_eq!(langs, vec!["en", "fr"]);
    }

    #[test]
    fn a_season_pack_uses_per_episode_subs_folders() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("Show.S01E01.mkv");
        touch(&a);
        touch(&d.path().join("Show.S01E02.mkv"));
        touch(&d.path().join("Subs/Show.S01E01/2_English.srt"));
        touch(&d.path().join("Subs/Show.S01E02/2_English.srt"));
        let found = for_import(&a);
        assert_eq!(found.len(), 1);
        assert!(
            found[0]
                .0
                .to_string_lossy()
                .contains("Show.S01E01/2_English")
        );
    }

    #[test]
    fn sidecar_names_keep_tags_and_never_collide() {
        let dest = Path::new("/lib/Movie (2019)/Movie (2019).mkv");
        let en = SubTags {
            language: Some("en".into()),
            ..Default::default()
        };
        let first = sidecar_name(dest, &en, "SRT", &[]);
        assert_eq!(first, Path::new("/lib/Movie (2019)/Movie (2019).en.srt"));
        let second = sidecar_name(dest, &en, "srt", std::slice::from_ref(&first));
        assert_eq!(second, Path::new("/lib/Movie (2019)/Movie (2019).en.2.srt"));
        let forced = SubTags { forced: true, ..en };
        assert_eq!(
            sidecar_name(dest, &forced, "ass", &[]),
            Path::new("/lib/Movie (2019)/Movie (2019).en.forced.ass")
        );
    }

    #[test]
    fn lists_only_subtitles_named_for_the_video() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("Movie (2019).mkv");
        touch(&v);
        touch(&d.path().join("Movie (2019).en.srt"));
        touch(&d.path().join("Movie (2019).en.forced.srt"));
        touch(&d.path().join("Movie (2019).srt"));
        touch(&d.path().join("Other.en.srt"));
        let subs = beside(&v);
        let labels: Vec<_> = subs.iter().map(SubtitleFile::label).collect();
        assert_eq!(
            labels,
            vec!["English (forced)", "English", "Unknown"],
            "name order"
        );
    }

    #[test]
    fn carry_places_subtitles_beside_the_imported_video() {
        let dl = tempfile::tempdir().unwrap();
        let lib = tempfile::tempdir().unwrap();
        let src = dl.path().join("Movie.2019.1080p.mkv");
        touch(&src);
        touch(&dl.path().join("Subs/2_English.srt"));
        touch(&dl.path().join("Subs/3_English.srt"));
        touch(&dl.path().join("Movie.2019.1080p.en.forced.srt"));
        let dest = lib.path().join("Movie (2019)/Movie (2019).mkv");
        touch(&dest);
        let mut names: Vec<_> = carry(&src, &dest, false)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "Movie (2019).en.2.srt",
                "Movie (2019).en.forced.srt",
                "Movie (2019).en.srt"
            ]
        );
        assert_eq!(beside(&dest).len(), 3, "and the library listing finds them");
        assert_eq!(
            carry(&src, &dest, false).len(),
            3,
            "a re-import replaces, not duplicates"
        );
        assert_eq!(beside(&dest).len(), 3);
    }

    #[test]
    fn srt_becomes_webvtt() {
        let srt = "1\r\n00:00:01,000 --> 00:00:02,500\r\nHello, world\r\n\r\n";
        assert_eq!(
            to_webvtt(srt, "srt"),
            "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello, world\n\n"
        );
    }

    #[test]
    fn ass_becomes_webvtt_without_styling() {
        let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.23,0:00:04.50,Default,,0,0,0,,{\\i1}Hello{\\i0}, there\\Nfriend\n";
        assert_eq!(
            to_webvtt(ass, "ass"),
            "WEBVTT\n\n00:00:01.230 --> 00:00:04.500\nHello, there\nfriend\n\n"
        );
    }

    #[test]
    fn windows_1252_text_is_decoded() {
        assert_eq!(decode_text(b"caf\xe9 \x93hi\x94"), "café “hi”");
        assert_eq!(decode_text("\u{feff}plain".as_bytes()), "plain");
    }
}
