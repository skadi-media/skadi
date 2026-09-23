//! The release-title parser.
//!
//! Implemented as ordered extraction passes over the title rather than one
//! mega-regex, so a failure is localizable to a single pass. Each pass
//! contributes fields to a [`ParsedRelease`]. The parser is held to the
//! [`corpus`](crate::corpus) — every change must keep it green.
//!
//! Clean-room: the passes are derived from observable scene/p2p naming
//! conventions, not translated from any GPL `*arr` source.

use std::sync::LazyLock;

use regex::Regex;

use crate::parsed::ParsedRelease;
use crate::quality::{Codec, Modifier, Quality, QualityDefinition, Resolution, Source};

static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap());

/// A whole-run pack: `Complete Series`, `Complete Collection`, `The Complete
/// Series` (SKADI-T-0549). Not a season marker — it spans every season — but it
/// *is* where the work title ends, which is the only thing `parse_tv` needs it
/// for.
static TV_COMPLETE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:the\s+)?complete\s+(?:series|collection|seasons?)\b").unwrap()
});
static RESOLUTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(2160p|1080p|1080i|720p|576p|480p|4k|uhd)\b").unwrap());
// `bd-?remux`/`br-?disk`/`bd-?disk` are matched before the bare `blu-?ray`
// alternatives so the whole single token is captured (SKADI-T-0019 quality
// breadth). A `…remux` source also implies the Remux modifier (set in `parse`).
// Bare `WEB` comes after `web-dl`/`web-rip` for the same reason — first
// alternative wins, so the more specific spellings still capture whole
// (SKADI-T-0432; a bare `WEB` is how a large share of real TV releases are
// tagged, and none of them classified before).
static SOURCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(bd-?remux|br-?disk|bd-?disk|blu-?ray|bdrip|brrip|web-?dl|web-?rip|web|hdtv|dvdrip|dvd|telesync|cam)\b",
    )
    .unwrap()
});
static CODEC_RE: LazyLock<Regex> =
    // `xvid`/`divx` are legacy but still appear on older library material, and
    // `vp9` on WebM-sourced WEB releases; without them those releases parsed with
    // no codec at all (SKADI-T-0444 found this by widening the corpus).
    LazyLock::new(|| {
        Regex::new(r"(?i)\b(x264|x265|h\.?264|h\.?265|hevc|avc|av1|vp9|xvid|divx)\b").unwrap()
    });
static MODIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(remux|proper|repack)\b").unwrap());
/// Audio layout / codec tokens that mark a bracketed group as real metadata
/// (`[5.1]`, `[DTS-HD]`, `[AAC2.0]`) rather than a site tag — SKADI-T-0435.
static AUDIO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\b\d\.\d\b|\b(dts(-?hd)?|truehd|atmos|aac|ac-?3|eac-?3|ddp?\d?|flac|opus|mp3)\b)",
    )
    .unwrap()
});
static EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(mkv|avi|mp4|m4v|mov|wmv)$").unwrap());
/// A trailing bracketed tag, e.g. `[eztv]` or `(rarbg.com)` — repost/site tags
/// that follow the real release group and would otherwise mask it.
static TRAILING_BRACKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\[(][^\[\]()]*[\])]$").unwrap());

/// Edition keyword vocabulary (SKADI-T-0019). One contiguous run of these words
/// in the post-year tag soup is the edition (e.g. `Extended.Collectors.Edition`,
/// `Director's.Cut`, `IMAX`). Built clean-room from observable release naming.
/// The "weak" connective words (`edition`, `cut`, …) only count as part of a
/// longer phrase; a lone weak word is not an edition (see [`extract_edition`]).
const EDITION_WORD: &str = r"(?:imax|extended|unrated|uncut|theatrical|remaster(?:ed)?|redux|restored|criterion|collection|special|limited|ultimate|collector(?:'?s)?|collectors|director(?:'?s)?|directors|final|anniversary|edition|cut|version)";

static EDITION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\b{EDITION_WORD}(?:\s+{EDITION_WORD})*")).unwrap());

/// Words from [`EDITION_WORD`] that are not editions on their own — they need a
/// neighbour (so a stray `Edition`/`Cut` in the tag soup isn't mistaken for one).
const WEAK_EDITION_WORDS: &[&str] = &[
    "edition",
    "cut",
    "collection",
    "version",
    "special",
    "limited",
    "ultimate",
    "final",
    "anniversary",
    "collector",
    "collectors",
    "collector's",
    "director",
    "directors",
    "director's",
];

/// Normalize a raw title before extraction: drop a container extension and any
/// trailing repost/site bracket tags (e.g. `...-2HD[eztv]-[rarbg.com]`).
fn preclean(title: &str) -> String {
    // Underscores are word separators in release / library names (Sonarr/Plex
    // emit `Show_-_S01E01_-_Title`, and skadi's own canonical naming does too).
    // Normalize them to spaces up front: the season/episode + title regexes key
    // off `.`/space/`-` boundaries, but `_` is a regex *word* character, so
    // `_S01E01_` has no word boundary and the SxxEyy match silently fails —
    // leaving every episode an unparsed, per-file "series" (SKADI-I-0047).
    let mut s = title.trim().replace('_', " ");
    if let Some(m) = EXT_RE.find(&s) {
        s.truncate(m.start());
    }
    loop {
        let trimmed = s.trim_end_matches([' ', '.', '-']).to_string();
        if let Some(m) = TRAILING_BRACKET_RE.find(&trimmed) {
            // Not every trailing bracket is a repost/site tag. YTS-style names put
            // the real metadata in brackets — `Movie (2020) [1080p] [BluRay] [5.1]
            // [YTS.MX]` — and stripping them all left the bare title, losing the
            // year and the quality (SKADI-T-0435). Stop at the first bracket whose
            // content is real metadata; everything after it was site noise.
            let inner = &trimmed[m.start() + 1..m.end() - 1];
            if carries_metadata(inner) {
                s = trimmed;
                break;
            }
            s = trimmed[..m.start()].to_string();
        } else {
            s = trimmed;
            break;
        }
    }
    s
}

/// Does a bracketed group hold release metadata (year, resolution, source, codec
/// or an audio layout) rather than a site/repost tag? Used by [`preclean`] to know
/// where the noise ends — see SKADI-T-0435.
fn carries_metadata(inner: &str) -> bool {
    YEAR_RE.is_match(inner)
        || RESOLUTION_RE.is_match(inner)
        || SOURCE_RE.is_match(inner)
        || CODEC_RE.is_match(inner)
        || AUDIO_RE.is_match(inner)
}

/// Languages we recognize as title tags (lowercased). Per SKADI-T-0019 the
/// parser does **not** assume a default: an untagged release leaves `languages`
/// empty rather than guessing English. `multi`/`dual` are kept as-is (they mark
/// multi-language releases). Clean-room from observable release naming.
const LANGUAGES: &[&str] = &[
    "multi",
    "dual",
    "nordic",
    "english",
    "french",
    "spanish",
    "german",
    "italian",
    "japanese",
    "korean",
    "russian",
    "mandarin",
    "cantonese",
    "chinese",
    "hindi",
    "tamil",
    "telugu",
    "portuguese",
    "dutch",
    "swedish",
    "danish",
    "norwegian",
    "finnish",
    "polish",
    "czech",
    "hungarian",
    "turkish",
    "arabic",
    "hebrew",
    "thai",
    "vietnamese",
    "ukrainian",
    "greek",
];

/// Canonical language name for a token, if it is a known language tag.
fn language_of(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    LANGUAGES
        .contains(&lower.as_str())
        .then(|| title_case(&lower))
}

/// Canonicalise one edition word to its display form (handles acronyms +
/// apostrophes that plain title-casing gets wrong).
fn canon_edition_word(word: &str) -> String {
    match word.to_ascii_lowercase().as_str() {
        "imax" => "IMAX".to_string(),
        "director's" | "directors" | "director" => "Director's".to_string(),
        "collector's" | "collectors" | "collector" => "Collectors".to_string(),
        "remaster" | "remastered" => "Remastered".to_string(),
        other => title_case(other),
    }
}

/// Extract the edition from the post-year tail: the first contiguous run of
/// [`EDITION_RE`] words, canonicalised. A run that is a single "weak" connective
/// word (e.g. a stray `Edition`) is rejected so noise isn't read as an edition.
fn extract_edition(tail_spaced: &str) -> Option<String> {
    let m = EDITION_RE.find(tail_spaced)?;
    let words: Vec<&str> = m.as_str().split_whitespace().collect();
    if words.len() == 1 && WEAK_EDITION_WORDS.contains(&words[0].to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| canon_edition_word(w))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

/// Strip a trailing `-GROUP` (scene release group) and return it. The group is
/// the final dash-delimited segment when it is purely alphanumeric (so embedded
/// dashes like `WEB-DL`, `DTS-HD`, `H.265` are not mistaken for it).
///
/// Also folds in a **single-character** stub directly before the group
/// (e.g. `…x265-D-Z0N3` → `D-Z0N3`, SKADI-T-0019). The one-char limit keeps
/// multi-char quality tails like `WEB-DL`/`DTS-HD` from being mis-read as part
/// of the group.
fn extract_group(s: &mut String) -> Option<String> {
    let idx = s.rfind('-')?;
    let candidate = &s[idx + 1..];
    if candidate.is_empty() || !candidate.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let mut group = candidate.to_string();
    let mut start = idx;
    // Hyphenated short-stub group: a lone 1-char alnum segment before the group.
    let head = &s[..idx];
    if let Some(dash2) = head.rfind('-') {
        let stub = &head[dash2 + 1..];
        if stub.len() == 1 && stub.chars().all(|c| c.is_ascii_alphanumeric()) {
            group = format!("{stub}-{group}");
            start = dash2;
        }
    }
    s.truncate(start);
    Some(group)
}

fn tokens(s: &str) -> impl Iterator<Item = &str> {
    s.split(['.', '_', ' ']).filter(|t| !t.is_empty())
}

/// Byte offset of the earliest resolution or source tag in `s`, if any — the
/// title/tag-soup boundary for a release with no year to split on.
fn first_quality_tag(s: &str) -> Option<usize> {
    [RESOLUTION_RE.find(s), SOURCE_RE.find(s)]
        .into_iter()
        .flatten()
        .map(|m| m.start())
        .min()
}

/// The match for the **release year**, given a precleaned release name.
///
/// Two rules, both learned from titles the naive "first 4-digit token" version
/// got wrong (SKADI-T-0549, found by SKADI-T-0444's corpus):
///
/// 1. **The last candidate wins, not the first.** Scene names put the work title
///    first and the release year after it, so when a title itself contains a
///    year the later token is the release year.
///    `Blade.Runner.2049.2017` is a 2017 film called *Blade Runner 2049* — read
///    first-match-wins it becomes a 2049 film, which then feeds the wrong year
///    into the decision engine's year gate (SKADI-T-0387) and can accept a
///    different film entirely.
///
/// 2. **A year cannot consume the whole title.** `2012` and `1917` are films
///    whose names are years. If taking a candidate would leave no title before
///    it, it is the title, not the year — so `1917.1080p.BluRay` parses as
///    *1917* with no year rather than as a nameless 1917 release.
///
/// Together these also handle `2012.2009`: two candidates, the last one leaves
/// `2012` as the title.
fn release_year(work: &str) -> Option<regex::Match<'_>> {
    YEAR_RE
        .find_iter(work)
        // A candidate is only the year if something is left to be the title.
        // Checked with `tokens` rather than `start > 0` so a prefix of pure
        // separators (`(2019) Movie…`) does not count as a title.
        .filter(|m| tokens(&work[..m.start()]).next().is_some())
        .last()
}

/// Parse a release title into its structured parts.
#[must_use]
pub fn parse(title: &str) -> ParsedRelease {
    let mut work = preclean(title);
    let group = extract_group(&mut work);

    let mut parsed = ParsedRelease {
        group,
        ..Default::default()
    };

    // Year splits the work title from the tag soup that follows it. Without a
    // year (`Movie.1080p.BluRay.x264-GRP`), the first quality tag is the boundary
    // instead — otherwise the tags are never scanned and a yearless release has
    // no quality at all (SKADI-T-0387).
    let (title_part, tail) = match release_year(&work) {
        Some(m) => {
            parsed.year = work[m.start()..m.end()].parse::<u16>().ok();
            (&work[..m.start()], work[m.end()..].to_string())
        }
        None => match first_quality_tag(&work) {
            Some(b) if b > 0 => (&work[..b], work[b..].to_string()),
            _ => (work.as_str(), String::new()),
        },
    };

    let name = tokens(title_part).collect::<Vec<_>>().join(" ");
    // A YTS-style name puts the year in brackets — `Movie (2020) [1080p] …` — so
    // the pre-year part ends with a dangling `(`. Trim opening brackets and
    // separators off the tail of the title (SKADI-T-0435).
    let name = name
        .trim_end_matches([' ', '-', '.', '(', '[', '{'])
        .trim()
        .to_string();
    if !name.is_empty() {
        parsed.title = Some(name);
    }

    if tail.is_empty() {
        return parsed;
    }

    // Quality tags (verbatim matched text, so callers see the original token).
    parsed.resolution = RESOLUTION_RE.find(&tail).map(|m| m.as_str().to_string());
    parsed.source = SOURCE_RE.find(&tail).map(|m| m.as_str().to_string());
    parsed.codec = CODEC_RE.find(&tail).map(|m| m.as_str().to_string());

    // Modifiers, normalized to their canonical names.
    let mut modifiers: Vec<String> = MODIFIER_RE
        .find_iter(&tail)
        .filter_map(|m| Modifier::from_token(m.as_str()).map(|md| format!("{md:?}")))
        .collect();
    // A `…remux` source token (e.g. `BDRemux`) implies the Remux modifier even
    // when it isn't a standalone `Remux` word the modifier regex would catch.
    if parsed
        .source
        .as_deref()
        .is_some_and(|s| s.to_ascii_lowercase().contains("remux"))
        && !modifiers.iter().any(|m| m == "Remux")
    {
        modifiers.push("Remux".to_string());
    }
    parsed.modifiers = modifiers;

    // Languages anywhere in the tail (no default — untagged stays empty).
    parsed.languages = tokens(&tail).filter_map(language_of).collect();

    // Edition: the first contiguous run of edition keywords in the tail
    // (order-independent, so a post-quality edition is still caught, and HDR/
    // audio tokens never leak in).
    let tail_spaced = tail.replace(['.', '_'], " ");
    parsed.edition = extract_edition(&tail_spaced);

    parsed
}

// --- TV parsing (SKADI-I-0037 / SKADI-T-0267) --------------------------------

/// `S01E05-E07` / `S01E05-07` range → (season, first, last).
static TV_RANGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,3})\s*E(\d{1,3})\s*-\s*E?(\d{1,3})\b").unwrap());
/// `S01E05` / `S01E05E06` list → season + a run of `E\d+`.
static TV_SEASON_EP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,3})((?:\s*E\d{1,3})+)\b").unwrap());
static TV_EP_NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)E(\d{1,3})").unwrap());
/// `1x05` / `12x05` → season, episode.
static TV_ALT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})x(\d{2,3})\b").unwrap());
/// A whole-season marker with no episode: `S01` / `Season 1` / `Series 1`.
static TV_SEASON_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:S(\d{1,2})|(?:season|series)[\s._]*(\d{1,2}))\b").unwrap()
});
/// Daily air date `2024.03.01` / `2024-03-01` → (year, month, day).
static TV_DATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{4})[.\-](\d{2})[.\-](\d{2})\b").unwrap());
/// Anime absolute number after the title: ` - 134` (optionally `v2`).
static TV_ABS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s-\s*(\d{1,4})(?:v\d)?(?:\b|$)").unwrap());
/// A leading bracketed group tag (anime), e.g. `[SubsPlease] `.
static LEAD_BRACKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[\[(][^\[\]()]*[\])]\s*").unwrap());

fn cap_u16(c: &regex::Captures, i: usize) -> u16 {
    c.get(i).and_then(|m| m.as_str().parse().ok()).unwrap_or(0)
}

fn valid_ymd(y: &str, mo: &str, d: &str) -> bool {
    let (mo, d) = (mo.parse::<u8>().unwrap_or(0), d.parse::<u8>().unwrap_or(0));
    let y = y.parse::<u16>().unwrap_or(0);
    (1900..=2100).contains(&y) && (1..=12).contains(&mo) && (1..=31).contains(&d)
}

/// Parse a **TV** release title (SKADI-T-0267). Reuses the movie quality passes
/// (resolution/source/codec/modifiers/languages/group) but splits the title from
/// the tag soup on the **episode marker** rather than the year, and fills the TV
/// fields: `season`, `episodes` (ranges expanded), `absolute` (anime), `air_date`
/// (daily), and `full_season` (a season pack). Detection is ordered most- to
/// least-specific so `S01E05` never degrades to the `S01` season-pack form.
#[must_use]
pub fn parse_tv(title: &str) -> ParsedRelease {
    let mut work = preclean(title);
    if let Some(m) = LEAD_BRACKET_RE.find(&work) {
        work = work[m.end()..].to_string();
    }

    let mut p = ParsedRelease::default();
    let mut boundary: Option<usize> = None;

    // A "range" only counts when it ascends (`E05-E07`). A descending pseudo-range
    // like `S01E36-1.28` (a trailing score/number, not a range) must NOT parse as
    // `[36, 1]` — the matcher would mark E01 falsely Imported from an E36 file
    // (SKADI-T-0324 review). Fall through to the plain SxxEyy branch instead.
    if let Some(c) = TV_RANGE_RE.captures(&work).filter(|c| {
        let (a, b) = (cap_u16(c, 2), cap_u16(c, 3));
        a < b && b - a < 100
    }) {
        p.season = Some(cap_u16(&c, 1));
        let (a, b) = (cap_u16(&c, 2), cap_u16(&c, 3));
        p.episodes = (a..=b).collect();
        boundary = Some(c.get(0).unwrap().start());
    } else if let Some(c) = TV_SEASON_EP_RE.captures(&work) {
        p.season = Some(cap_u16(&c, 1));
        p.episodes = TV_EP_NUM_RE
            .captures_iter(&c[2])
            .filter_map(|e| e[1].parse().ok())
            .collect();
        boundary = Some(c.get(0).unwrap().start());
    } else if let Some(c) = TV_ALT_RE.captures(&work) {
        p.season = Some(cap_u16(&c, 1));
        p.episodes = vec![cap_u16(&c, 2)];
        boundary = Some(c.get(0).unwrap().start());
    } else if let Some(c) = TV_DATE_RE
        .captures(&work)
        .filter(|c| valid_ymd(&c[1], &c[2], &c[3]))
    {
        p.air_date = Some(format!("{}-{}-{}", &c[1], &c[2], &c[3]));
        boundary = Some(c.get(0).unwrap().start());
    } else if let Some(c) = TV_SEASON_ONLY_RE.captures(&work)
        && let Some(s) = c
            .get(1)
            .or_else(|| c.get(2))
            .and_then(|m| m.as_str().parse().ok())
    {
        p.season = Some(s);
        p.full_season = true;
        boundary = Some(c.get(0).unwrap().start());
    } else if let Some(c) = TV_ABS_RE.captures(&work)
        && let Ok(n) = c[1].parse::<u32>()
    {
        p.absolute = vec![n];
        boundary = Some(c.get(0).unwrap().start());
    } else if let Some(m) = TV_COMPLETE_RE.find(&work) {
        // A complete-series pack has no season/episode marker at all, so without
        // this the title cleanup below never ran and the *entire* release name —
        // resolution, source, codec, group — was returned as the series title
        // (SKADI-T-0549). Relevance scoring then matched `Angel` against
        // "Angel Complete Series 1080p BluRay x264-GRP".
        //
        // Deliberately does **not** set `full_season`: that means "a season
        // pack", and a complete series spans every season, so there is no season
        // number to pair it with. Marking it would invite a season-scoped match
        // against an unknown season.
        boundary = Some(m.start());
    }

    // Still no marker — a pack or a loose name that names no episode. Fall back
    // to the first quality tag, the same boundary `parse` uses for a yearless
    // release, rather than letting the whole string become the title.
    if boundary.is_none()
        && let Some(b) = first_quality_tag(&work)
        && b > 0
    {
        boundary = Some(b);
    }

    let title_part = boundary.map_or(work.as_str(), |b| &work[..b]);
    let name = tokens(title_part).collect::<Vec<_>>().join(" ");
    // Drop a dangling separator left by `Show - SxxEyy - Title` naming (the title
    // part is `Show - `), so the series key is clean `Show`, not `Show -` — better
    // grouping and a better metadata search (SKADI-I-0047).
    let name = name.trim_end_matches([' ', '-']).trim().to_string();
    if !name.is_empty() {
        p.title = Some(name);
    }
    if let Some(m) = YEAR_RE.find(title_part) {
        p.year = m.as_str().parse().ok();
    }

    // Quality tags: scan a bracket-normalized copy of the *original* title so
    // bracketed quality survives (anime writes `(1080p)`, which `preclean` would
    // strip as a trailing tag). Resolution/source/codec are specific enough that
    // a title-part false positive is negligible.
    // Underscores are separators too, and they are regex *word* characters: in
    // `show_name_-_1x05_-_title_720p_hdtv` there is no `\b` before `720p`, so the
    // resolution and source silently vanished for library-style names — the very
    // names adoption feeds in (SKADI-T-0434). Normalise them like `preclean` does.
    let quality_src = title.replace(['[', ']', '(', ')', '_'], " ");
    let tail = boundary.map_or_else(|| work.clone(), |b| work[b..].to_string());
    p.resolution = RESOLUTION_RE
        .find(&quality_src)
        .map(|m| m.as_str().to_string());
    p.source = SOURCE_RE.find(&quality_src).map(|m| m.as_str().to_string());
    p.codec = CODEC_RE.find(&quality_src).map(|m| m.as_str().to_string());
    let mut modifiers: Vec<String> = MODIFIER_RE
        .find_iter(&quality_src)
        .filter_map(|m| Modifier::from_token(m.as_str()).map(|md| format!("{md:?}")))
        .collect();
    if p.source
        .as_deref()
        .is_some_and(|s| s.to_ascii_lowercase().contains("remux"))
        && !modifiers.iter().any(|m| m == "Remux")
    {
        modifiers.push("Remux".to_string());
    }
    p.modifiers = modifiers;
    p.languages = tokens(&tail).filter_map(language_of).collect();
    let mut tail_for_group = tail.clone();
    p.group = extract_group(&mut tail_for_group);
    p
}

// --- Audiobook parsing (SKADI-I-0017 / SKADI-T-0122) -------------------------

static AUDIO_FORMAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(m4b|m4a|mp3|aac|flac|ogg|opus)\b").unwrap());
static BITRATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{2,3})\s?kbps\b|\b(\d{2,3})\s?k\b").unwrap());
static ABRIDGED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(unabridged|abridged)\b").unwrap());
/// A bracketed series tag with a trailing number, e.g. `[Stormlight Archive 01]`,
/// `(Discworld, Book 5)`, or `(The Dresden Files #1)`. Captures (name, number).
/// The name must contain a letter, so a bare `(2021)` year is not mistaken for a
/// series.
static SERIES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)[\[({]\s*([^\[\](){}]*?[A-Za-z][^\[\](){}]*?)\s*[,#]?\s*(?:book|bk|vol\.?|volume)?\s*#?\s*(\d+(?:\.\d+)?)\s*[\])}]",
    )
    .unwrap()
});
/// Any bracketed segment — stripped from the name part once mined.
static ANY_BRACKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\[({][^\[\](){}]*[\])}]").unwrap());

/// Parse an **audiobook** release title (SKADI-T-0122). Audiobook naming is its
/// own dialect — `Author - Title (Year) [Series ##] {Format Bitrate Abridged}` —
/// so this is a separate path from the movie [`parse`]. It populates the
/// audiobook fields of [`ParsedRelease`] (`author`/`title`/`year`/`series`/
/// `series_position`/`audio_format`/`bitrate_kbps`/`abridged`). A parse *hint*
/// for matching/scoring; Audnexus metadata is canonical. Clean-room from
/// observable release naming.
#[must_use]
pub fn parse_audiobook(title: &str) -> ParsedRelease {
    // Strip a video container extension but KEEP brackets — for audiobooks they
    // carry format/bitrate/series (unlike the movie path, which strips them).
    let mut owned = title.trim().to_string();
    if let Some(m) = EXT_RE.find(&owned) {
        owned.truncate(m.start());
    }
    let work = owned.trim();

    // Audiobook pack markers (SKADI-T-0312): a keyword-attached book/volume range, a
    // collection/omnibus/anthology/discography word, a box set, or "Complete <Series/…>".
    // Deliberately *not* bare "Complete" (common in single-book "Complete & Unabridged").
    static BOOK_PACK_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(?:books?|vols?|volumes?)\s*\.?\s*\d{1,3}\s*[-–—]\s*\d{1,3}\b|\b(?:collection|omnibus|anthology|discography)\b|\bbox\s?set\b|\bcomplete\s+(?:series|collection|set|trilogy|saga|works)\b",
        )
        .unwrap()
    });

    let mut p = ParsedRelease {
        audio_format: AUDIO_FORMAT_RE.find(work).map(|m| m.as_str().to_string()),
        bitrate_kbps: BITRATE_RE.captures(work).and_then(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .and_then(|m| m.as_str().parse::<u32>().ok())
        }),
        abridged: ABRIDGED_RE
            .find(work)
            .map(|m| m.as_str().eq_ignore_ascii_case("abridged")),
        year: YEAR_RE
            .find(work)
            .and_then(|m| m.as_str().parse::<u16>().ok()),
        book_pack: BOOK_PACK_RE.is_match(work),
        ..Default::default()
    };

    if let Some(c) = SERIES_RE.captures(work) {
        let name = c.get(1).map(|m| m.as_str().trim().to_string());
        let num = c.get(2).map(|m| normalize_position(m.as_str()));
        if let (Some(name), Some(num)) = (name, num)
            && !name.is_empty()
        {
            p.series = Some(name);
            p.series_position = Some(num);
        }
    }

    // Name part: drop every bracket, then the recognized tokens, then split on
    // the first " - " into author / title.
    let stripped = ANY_BRACKET_RE.replace_all(work, " ");
    let name = strip_audiobook_tokens(&stripped);
    let (author, book) = split_author_title(&name);
    p.author = author;
    p.title = book;

    p
}

/// Normalize a series position: drop leading zeros on plain integers
/// (`"01"` → `"1"`); keep decimals (`"3.5"`) as-is.
fn normalize_position(raw: &str) -> String {
    if raw.contains('.') {
        return raw.to_string();
    }
    raw.parse::<u32>()
        .map_or_else(|_| raw.to_string(), |n| n.to_string())
}

/// Remove audiobook quality/year tokens from the name part and tidy separators.
fn strip_audiobook_tokens(s: &str) -> String {
    let mut out = AUDIO_FORMAT_RE.replace_all(s, " ").into_owned();
    out = BITRATE_RE.replace_all(&out, " ").into_owned();
    out = ABRIDGED_RE.replace_all(&out, " ").into_owned();
    out = YEAR_RE.replace_all(&out, " ").into_owned();
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_matches(|c: char| c == '-' || c == ' ' || c == '.')
        .to_string()
}

/// Split a cleaned `Author - Title` name on the first " - "; with no delimiter
/// the whole string is the title (author unknown).
fn split_author_title(name: &str) -> (Option<String>, Option<String>) {
    let tidy = |s: &str| {
        let t = s.trim().trim_matches('-').trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    name.split_once(" - ")
        .map_or_else(|| (None, tidy(name)), |(a, rest)| (tidy(a), tidy(rest)))
}

/// Map a [`ParsedRelease`] to a [`Quality`] against a set of quality
/// definitions, matching on resolution + source. Returns `None` if either is
/// missing/unrecognized or no definition matches.
#[must_use]
/// Grade an **owned file** from what a probe can actually see (SKADI-T-0528).
///
/// The adopted library is the problem this exists for: 19,803 rows sit at
/// `Unknown` because their paths are skadi's own canonical names, which carry no
/// quality tokens, so there is nothing for [`parse`] to read. `Unknown` is
/// deliberately unjudgeable, so the upgrade sweep skips those rows entirely — the
/// library is invisible to it.
///
/// A probe yields resolution reliably and dynamic range when the container says
/// so. It cannot yield **source**: nothing in the bytes distinguishes a WEB-DL
/// from a BluRay rip. That missing axis is the whole design problem, and the
/// resolution is asymmetric-cost reasoning rather than a guess:
///
/// * Grade too **low** and the sweep believes a good file is upgradeable. It goes
///   and fetches a replacement, spends bandwidth, and can overwrite a better file
///   with a worse one. That is the exact harm SKADI-T-0399 stopped by moving
///   these rows to `Unknown` in the first place.
/// * Grade too **high** and the sweep believes a mediocre file is fine. Nothing
///   is destroyed; an upgrade that could have happened does not.
///
/// So when the source is unknowable this takes the **best** source defined at
/// that resolution, not [`Source::Hdtv`] as `to_quality` does for a title. A
/// title that omits the source is evidence about the release; a probe that cannot
/// see one is evidence about nothing.
///
/// Returns `None` when there is no video track or no definition matches — the
/// row stays `Unknown`, which remains the honest answer for a file that cannot be
/// assessed.
pub fn quality_from_probe(
    info: &skadi_core::MediaInfo,
    definitions: &[QualityDefinition],
) -> Option<Quality> {
    let video = info.video.as_ref()?;
    let resolution = Resolution::from_token(video.resolution_tier())?;
    // 480p/576p normalise to SD for the definition lookup, exactly as
    // `to_quality` does (SKADI-T-0412) — the ladder carries one SD row per
    // source, so an un-normalised lookup falls through to "no quality at all".
    let lookup = match resolution {
        Resolution::R480p | Resolution::R576p => Resolution::Sd,
        other => other,
    };
    let best = definitions
        .iter()
        .filter(|d| d.resolution == lookup)
        .max_by_key(|d| d.source)?;
    Some(Quality {
        id: best.id,
        resolution,
        source: best.source,
        // A probe can see the codec but not a modifier: `Remux`, `Proper` and
        // `Repack` are facts about the *release*, not the file, so there is
        // nothing in the bytes to read them from.
        codec: video.codec.as_deref().and_then(Codec::from_token),
        modifier: None,
    })
}

/// Standard definition implied by a title that names no resolution
/// (SKADI-T-0607). Pre-HD material — 1990s television, DVD rips — is released
/// as `Show.S01E01.XviD-GRP` or `Show.S01E01.DVDRip`: the era's codecs and
/// sources *are* the resolution statement, nobody wrote "480p" in 2004. Before
/// this every such title was "unrecognized quality" and an old show on a
/// profile that allows SD still could not be satisfied. Only legacy SD codecs
/// (XviD/DivX) and the DVD source imply it; a bare `x264` still says nothing.
fn implied_sd(parsed: &ParsedRelease, source: Source) -> Option<Resolution> {
    let legacy_codec = parsed
        .codec
        .as_deref()
        .is_some_and(|c| matches!(c.to_ascii_lowercase().as_str(), "xvid" | "divx"));
    (legacy_codec || source == Source::Dvd).then_some(Resolution::Sd)
}

pub fn to_quality(parsed: &ParsedRelease, definitions: &[QualityDefinition]) -> Option<Quality> {
    // A title that names a resolution but no source still has a quality: Sonarr
    // reads it as the HDTV tier rather than "no quality at all" (SKADI-T-0432).
    // Anime and scene TV releases routinely carry only `(1080p)`, and treating
    // those as unclassifiable made them the single largest opaque `quality`
    // rejection in the decision tally.
    let source = parsed
        .source
        .as_deref()
        .and_then(Source::from_token)
        .unwrap_or(Source::Hdtv);
    // No resolution at all is still a quality when the era says SD (XviD, DVDRip)
    // — see `implied_sd` (SKADI-T-0607).
    let resolution = parsed
        .resolution
        .as_deref()
        .and_then(Resolution::from_token)
        .or_else(|| implied_sd(parsed, source))?;
    // 480p/576p ARE standard definition: the ladder carries one SD row per source
    // (SDTV, DVD), so a probed 720x480 DVD rip must land on `DVD` instead of
    // falling through to "no quality at all", which the hunter then recorded as the
    // profile cutoff — a fabricated "satisfied" (SKADI-T-0412). Only the definition
    // lookup is normalised; the parsed resolution is kept on the returned `Quality`.
    let lookup = match resolution {
        Resolution::R480p | Resolution::R576p => Resolution::Sd,
        other => other,
    };
    let def = definitions
        .iter()
        .find(|d| d.resolution == lookup && d.source == source)?;
    Some(Quality {
        id: def.id,
        resolution,
        source,
        codec: parsed.codec.as_deref().and_then(Codec::from_token),
        modifier: parsed
            .modifiers
            .iter()
            .find_map(|m| Modifier::from_token(m)),
    })
}

/// Override a parse's `resolution`/`bitrate_kbps` with **probed** ground truth before the
/// stored quality is computed (SKADI-T-0240). After a download is placed, the file's real
/// streams are known (SKADI-I-0033); a release titled `…2160p…` whose file is really
/// 1920×1080 must be stored as 1080p so it doesn't fake-satisfy the cutoff. The probe
/// knows the file's pixels + audio bitrate; everything else — notably `source` (BluRay vs
/// WEB, which a container can't reveal), codec, and group — stays from the title.
///
/// Pure: returns a clone with the known fields replaced. `probed_resolution` is applied
/// only when it's a recognised [`Resolution`] token (an unknown/`SD` tier is left as the
/// title's). `None`/`None` ⇒ the parse is unchanged (no probe → title-parsed behaviour).
#[must_use]
pub fn reconcile_with_probe(
    parsed: &ParsedRelease,
    probed_resolution: Option<&str>,
    probed_bitrate_kbps: Option<u32>,
) -> ParsedRelease {
    let mut p = parsed.clone();
    if let Some(tier) = probed_resolution
        && Resolution::from_token(tier).is_some()
    {
        p.resolution = Some(tier.to_string());
    }
    if let Some(b) = probed_bitrate_kbps {
        p.bitrate_kbps = Some(b);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus;
    use crate::quality::default_definitions;

    #[test]
    fn reconcile_with_probe_corrects_resolution_and_bitrate() {
        let defs = default_definitions();
        // A release titled 2160p whose file is really 1080p → stored quality is 1080p.
        let claimed = parse("The.Matrix.1999.2160p.BluRay.x264-GRP");
        assert_eq!(
            to_quality(&claimed, &defs).unwrap().resolution,
            Resolution::R2160p
        );
        let real = reconcile_with_probe(&claimed, Some("1080p"), None);
        assert_eq!(
            to_quality(&real, &defs).unwrap().resolution,
            Resolution::R1080p,
            "fake 4K stored as its real 1080p"
        );

        // An under-claiming title is also corrected up (store truth either way).
        let up = reconcile_with_probe(&parse("Movie.2020.720p.WEB-DL"), Some("1080p"), None);
        assert_eq!(up.resolution.as_deref(), Some("1080p"));

        // An unrecognised probed tier is ignored (keep the title's).
        let kept = reconcile_with_probe(&claimed, Some("weird"), None);
        assert_eq!(kept.resolution.as_deref(), Some("2160p"));

        // Audiobook bitrate is overridden by the real value.
        let ab = reconcile_with_probe(&parse("Book Unabridged 128kbps"), None, Some(64));
        assert_eq!(ab.bitrate_kbps, Some(64));

        // No probe → unchanged.
        assert_eq!(reconcile_with_probe(&claimed, None, None), claimed);
    }

    #[test]
    fn parses_a_simple_bluray_release() {
        let p = parse("The.Matrix.1999.1080p.BluRay.x264-AMIABLE");
        assert_eq!(p.title.as_deref(), Some("The Matrix"));
        assert_eq!(p.year, Some(1999));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(p.source.as_deref(), Some("BluRay"));
        assert_eq!(p.codec.as_deref(), Some("x264"));
        assert_eq!(p.group.as_deref(), Some("AMIABLE"));
        assert!(p.modifiers.is_empty());
        assert!(p.edition.is_none());
    }

    /// No year to split on (SKADI-T-0387): the first quality tag bounds the
    /// title, so the release still gets a quality instead of none.
    #[test]
    fn yearless_release_splits_on_the_first_quality_tag() {
        let p = parse("101.Dalmatians.1080p.BluRay.x264-GRP");
        assert_eq!(p.title.as_deref(), Some("101 Dalmatians"));
        assert_eq!(p.year, None);
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(p.source.as_deref(), Some("BluRay"));
        assert_eq!(p.codec.as_deref(), Some("x264"));
        assert_eq!(p.group.as_deref(), Some("GRP"));
        // Source-first ordering, spaces, and a title-only name.
        let p = parse("101 Dalmatians BluRay 720p x264");
        assert_eq!(p.title.as_deref(), Some("101 Dalmatians"));
        assert_eq!(p.resolution.as_deref(), Some("720p"));
        let p = parse("Just.A.Title");
        assert_eq!(p.title.as_deref(), Some("Just A Title"));
        assert!(p.resolution.is_none());
    }

    #[test]
    fn handles_dotted_h265_and_web_dl() {
        let p = parse("Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.Atmos.H.265-FLUX");
        assert_eq!(p.title.as_deref(), Some("Dune Part Two"));
        assert_eq!(p.source.as_deref(), Some("WEB-DL"));
        assert_eq!(p.codec.as_deref(), Some("H.265"));
        assert_eq!(p.group.as_deref(), Some("FLUX"));
    }

    #[test]
    fn extracts_editions_and_modifiers_and_languages() {
        let p = parse("Avatar.2009.Extended.Collectors.Edition.1080p.BluRay.x264-GROUP");
        assert_eq!(p.edition.as_deref(), Some("Extended Collectors Edition"));

        let p = parse("Top.Gun.Maverick.2022.PROPER.2160p.BluRay.x265-SURCODE");
        assert_eq!(p.modifiers, vec!["Proper".to_string()]);
        assert!(p.edition.is_none());

        let p = parse("Amelie.2001.FRENCH.1080p.BluRay.x264-LOST");
        assert_eq!(p.languages, vec!["French".to_string()]);
        assert!(p.edition.is_none());
    }

    #[test]
    fn edition_keywords_are_recognized_and_canonicalized() {
        // IMAX stays an acronym; director's/final cut canonicalize the apostrophe.
        assert_eq!(
            parse("Tenet.2020.IMAX.2160p.WEB-DL.HEVC-NOGRP")
                .edition
                .as_deref(),
            Some("IMAX")
        );
        assert_eq!(
            parse("Kingdom.of.Heaven.2005.Directors.Cut.1080p.BluRay.x264-GRP")
                .edition
                .as_deref(),
            Some("Director's Cut")
        );
        // An edition AFTER the quality tokens is still found (order-independent).
        assert_eq!(
            parse("Watchmen.2009.1080p.BluRay.Directors.Cut.x264-GRP")
                .edition
                .as_deref(),
            Some("Director's Cut")
        );
        // A lone weak connective word (a stray "Edition") is NOT an edition…
        assert!(
            parse("Some.Movie.2018.1080p.BluRay.Edition.x264-GRP")
                .edition
                .is_none()
        );
        // …but a strong+weak phrase ("Special Edition") is.
        assert_eq!(
            parse("Aliens.1986.Special.Edition.1080p.BluRay.x265-GRP")
                .edition
                .as_deref(),
            Some("Special Edition")
        );
        // No edition tokens at all → none.
        assert!(
            parse("Plain.Movie.2019.1080p.BluRay.x264-GRP")
                .edition
                .is_none()
        );
    }

    #[test]
    fn bdremux_source_implies_remux_and_hdr_is_ignored() {
        let p = parse("Gladiator.2000.2160p.UHD.BDRemux.HDR.DV.HEVC.TrueHD.7.1-GRP");
        assert_eq!(p.source.as_deref(), Some("BDRemux"));
        assert_eq!(p.modifiers, vec!["Remux".to_string()]);
        assert_eq!(p.codec.as_deref(), Some("HEVC"));
        // BDRemux still maps to a Bluray quality.
        let q = to_quality(&p, &default_definitions()).unwrap();
        assert_eq!(q.source, Source::Bluray);
        // HDR/DV/audio tokens never leak into the edition.
        assert!(p.edition.is_none());
    }

    #[test]
    fn hyphenated_short_stub_group_is_kept_whole() {
        let p = parse("The.Northman.2022.2160p.UHD.BluRay.x265-D-Z0N3");
        assert_eq!(p.group.as_deref(), Some("D-Z0N3"));
        // A multi-char tail like WEB-DL is NOT folded into the group.
        let p = parse("Dune.2021.2160p.WEB-DL.H265-GRP");
        assert_eq!(p.group.as_deref(), Some("GRP"));
        assert_eq!(p.source.as_deref(), Some("WEB-DL"));
    }

    #[test]
    fn language_default_is_empty_not_english() {
        // Per the SKADI-T-0019 decision: no default — untagged stays empty.
        assert!(
            parse("Plain.Movie.2019.1080p.BluRay.x264-GRP")
                .languages
                .is_empty()
        );
        // Broadened vocabulary still tags explicit languages.
        assert_eq!(
            parse("RRR.2022.HINDI.2160p.WEB-DL.x265-GRP").languages,
            vec!["Hindi".to_string()]
        );
    }

    #[test]
    fn resolution_less_xvid_and_dvdrip_titles_land_on_sd() {
        let defs = crate::quality::default_definitions();
        let xvid = parse_tv("Mystery.Science.Theater.3000.S01E01.XviD-AFG");
        let q = to_quality(&xvid, &defs).expect("XviD implies SD");
        assert_eq!(q.resolution, Resolution::Sd);
        assert_eq!(q.source, Source::Hdtv, "no source named → SDTV row");
        let dvd = parse_tv("Spellbinder.S01E03.DVDRip.x264-GRP");
        let q = to_quality(&dvd, &defs).expect("DVDRip implies SD");
        assert_eq!((q.resolution, q.source), (Resolution::Sd, Source::Dvd));
        let bare = parse_tv("Some.Show.S01E01.x264-GRP");
        assert!(to_quality(&bare, &defs).is_none(), "x264 alone still says nothing");
    }

    #[test]
    fn maps_to_quality() {
        let p = parse("The.Matrix.1999.1080p.BluRay.x264-AMIABLE");
        let q = to_quality(&p, &default_definitions()).unwrap();
        assert_eq!(q.resolution, Resolution::R1080p);
        assert_eq!(q.source, Source::Bluray);
        assert_eq!(q.codec, Some(Codec::X264));
    }

    /// The corpus is the contract: the parser must reproduce every expected
    /// parse. This test is the CI gate (a failing entry fails CI).
    #[test]
    fn parser_passes_the_corpus() {
        let report = corpus::run(&corpus::seed(), parse);
        assert!(report.is_clean(), "{}", report.summary());
    }

    fn probe(width: u32, height: u32, codec: Option<&str>) -> skadi_core::MediaInfo {
        skadi_core::MediaInfo {
            video: Some(skadi_core::VideoInfo {
                width,
                height,
                codec: codec.map(str::to_string),
                profile: None,
                dynamic_range: None,
            }),
            ..Default::default()
        }
    }

    /// The asymmetry this function exists for (SKADI-T-0528).
    #[test]
    fn a_probe_grades_to_the_best_source_at_its_resolution() {
        let defs = crate::quality::default_definitions();
        let q = quality_from_probe(&probe(1920, 1080, Some("h264")), &defs).expect("graded");
        assert_eq!(q.resolution, Resolution::R1080p);

        // Not HDTV. A probe cannot see the source, and grading an owned file too
        // low tells the sweep a good file is upgradeable — it then spends
        // bandwidth and can replace it with something worse. Grading too high
        // only means a possible upgrade is skipped. Nothing is destroyed either
        // way except by the first mistake, so the tie goes to "leave it alone".
        assert!(
            q.source > Source::Hdtv,
            "a probe must not grade an owned file down to HDTV: {:?}",
            q.source
        );
        let best_1080 = defs
            .iter()
            .filter(|d| d.resolution == Resolution::R1080p)
            .map(|d| d.source)
            .max()
            .unwrap();
        assert_eq!(q.source, best_1080);
    }

    #[test]
    fn standard_definition_normalises_the_way_to_quality_does() {
        let defs = crate::quality::default_definitions();
        // 480p and 576p both normalise to SD for the definition lookup
        // (SKADI-T-0412): the ladder carries one SD row per source, so an
        // un-normalised lookup finds nothing and the row stays Unknown forever.
        //
        // Note `VideoInfo::resolution_tier` has no 576p bucket, so a 720x576 PAL
        // file tiers as 480p. That is a cosmetic inaccuracy in the tier function,
        // not a grading one — both normalise to the same SD definition, so the
        // quality assigned is identical. Asserted as it behaves rather than as
        // one might assume.
        for (w, h) in [(720u32, 480u32), (720, 576)] {
            let q = quality_from_probe(&probe(w, h, None), &defs)
                .unwrap_or_else(|| panic!("{w}x{h} should grade"));
            assert_eq!(q.resolution, Resolution::R480p);
            let def = defs
                .iter()
                .find(|d| d.id == q.id)
                .expect("graded to a real definition");
            assert_eq!(def.resolution, Resolution::Sd);
        }
    }

    #[test]
    fn a_file_that_cannot_be_assessed_stays_unknown() {
        let defs = crate::quality::default_definitions();
        // No video track at all — an audio-only or unreadable file. `None` keeps
        // the row Unknown, which is the honest answer; inventing a tier here is
        // exactly the fabricated "satisfied" SKADI-T-0412 removed.
        assert!(quality_from_probe(&skadi_core::MediaInfo::default(), &defs).is_none());
        // A real video track but no matching definition.
        assert!(quality_from_probe(&probe(1920, 1080, None), &[]).is_none());
    }

    #[test]
    fn the_codec_comes_from_the_probe_but_never_a_modifier() {
        let defs = crate::quality::default_definitions();
        let q = quality_from_probe(&probe(3840, 2160, Some("hevc")), &defs).expect("graded");
        assert_eq!(q.resolution, Resolution::R2160p);
        assert_eq!(q.codec, Some(Codec::X265));
        // Remux/Proper/Repack are facts about the release, not the file — there
        // is nothing in the bytes to read them from, so claiming one would be
        // fabrication.
        assert_eq!(q.modifier, None);
    }

    /// The television corpus gate (SKADI-T-0444) — `parse_tv` must reproduce
    /// every expected episode identity. Separate from the movie gate because the
    /// movie parser deliberately leaves `SxxExx` in the work title.
    #[test]
    fn tv_parser_passes_the_tv_corpus() {
        let report = corpus::run(&corpus::tv_seed(), parse_tv);
        assert!(report.is_clean(), "{}", report.summary());
    }

    /// The known-gap entries must keep failing (SKADI-T-0444 / SKADI-T-0549).
    ///
    /// This looks backwards and is deliberate. A known parser bug with no test
    /// rots; the same bug in the main corpus turns the build red for everyone.
    /// Asserting the *failure* keeps it visible and makes fixing the parser
    /// break this test — which is the prompt to promote these entries into the
    /// real corpus rather than delete the assertion.
    #[test]
    fn the_known_gaps_still_fail() {
        // Empty is the healthy state — it means nothing is known-broken. The
        // guard exists for when an entry is parked here, so it must tolerate the
        // file being empty rather than demanding a bug exist.
        let gaps = corpus::known_gaps();
        let report = corpus::run(&gaps, parse);
        assert_eq!(
            report.failures.len(),
            gaps.len(),
            "a known-gap title now parses correctly — the parser was fixed. \
             Move the passing entries from corpus/known_gaps.jsonl into \
             corpus/expanded.jsonl and close SKADI-T-0549.\n{}",
            report.summary()
        );
    }

    /// As [`the_known_gaps_still_fail`], for `parse_tv`.
    #[test]
    fn the_tv_known_gaps_still_fail() {
        let gaps = corpus::tv_known_gaps();
        assert!(!gaps.is_empty(), "the tv known-gaps corpus is empty");
        let report = corpus::run(&gaps, parse_tv);
        assert_eq!(
            report.failures.len(),
            gaps.len(),
            "a known-gap TV title now parses correctly — promote it from \
             corpus/tv_known_gaps.jsonl into corpus/tv_expanded.jsonl.\n{}",
            report.summary()
        );
    }

    // --- audiobook parser (SKADI-T-0122) ---

    /// The audiobook corpus gate — `parse_audiobook` must reproduce every
    /// expected audiobook parse.
    #[test]
    fn audiobook_parser_passes_the_corpus() {
        let report = corpus::run(&corpus::audiobook_seed(), parse_audiobook);
        assert!(report.is_clean(), "{}", report.summary());
    }

    #[test]
    fn parse_audiobook_extracts_author_title_format_bitrate() {
        let p = parse_audiobook("Andy Weir - Project Hail Mary (2021) [M4B 128kbps]");
        assert_eq!(p.author.as_deref(), Some("Andy Weir"));
        assert_eq!(p.title.as_deref(), Some("Project Hail Mary"));
        assert_eq!(p.year, Some(2021));
        assert_eq!(p.audio_format.as_deref(), Some("M4B"));
        assert_eq!(p.bitrate_kbps, Some(128));
        assert!(p.abridged.is_none(), "no abridgement stated");
    }

    #[test]
    fn parse_audiobook_extracts_series_and_abridgement() {
        let p = parse_audiobook(
            "Brandon Sanderson - The Way of Kings [Stormlight Archive 01] {MP3 64kbps Abridged}",
        );
        assert_eq!(p.series.as_deref(), Some("Stormlight Archive"));
        assert_eq!(
            p.series_position.as_deref(),
            Some("1"),
            "leading zero dropped"
        );
        assert_eq!(p.abridged, Some(true));
        // A bare year bracket is not mistaken for a series.
        let p = parse_audiobook("Frank Herbert - Dune (1965) M4B 192kbps");
        assert!(p.series.is_none());
        assert_eq!(p.year, Some(1965));
    }

    #[test]
    fn parse_audiobook_is_separate_from_movie_parse() {
        // The movie parser never populates audiobook fields…
        let m = parse("The.Matrix.1999.1080p.BluRay.x264-AMIABLE");
        assert!(m.audio_format.is_none() && m.author.is_none());
        // …and the audiobook parser never populates video fields.
        let a = parse_audiobook("Andy Weir - The Martian (2014) M4B 96kbps");
        assert!(a.resolution.is_none() && a.source.is_none() && a.codec.is_none());
    }

    #[test]
    fn parse_audiobook_detects_packs() {
        let pack = |t: &str| parse_audiobook(t).book_pack;
        // Ranges, collections, omnibus, discography, box set, complete-<series>.
        assert!(pack("Dungeon Crawler Carl - Books 1-8 [M4B]"));
        assert!(pack("Brandon Sanderson - Stormlight Archive Vol 1-4"));
        assert!(pack("Andrew Rowe - Arcane Ascension Collection"));
        assert!(pack("The Wheel of Time Omnibus"));
        assert!(pack("Stephen King Discography"));
        assert!(pack("Foundation Box Set"));
        assert!(pack("The Complete Series - Mistborn"));
        // Single books — including the "Complete & Unabridged" trap — are NOT packs.
        assert!(!pack("Andrew Rowe - Soulbrand [M4B]"));
        assert!(!pack("Matt Dinniman - Dungeon Crawler Carl 3"));
        assert!(!pack("Jane Doe - A Novel (Complete & Unabridged)"));
        assert!(!pack("Some Book - Part 1"));
    }

    // --- TV parsing (SKADI-T-0267) ------------------------------------------

    #[test]
    fn tv_single_episode_sxxexx() {
        let p = parse_tv("The.Mandalorian.S02E05.1080p.WEB-DL.x265-GROUP");
        assert_eq!(p.title.as_deref(), Some("The Mandalorian"));
        assert_eq!(p.season, Some(2));
        assert_eq!(p.episodes, vec![5]);
        assert!(p.absolute.is_empty() && !p.full_season);
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(p.source.as_deref(), Some("WEB-DL"));
        assert_eq!(p.codec.as_deref(), Some("x265"));
        assert_eq!(p.group.as_deref(), Some("GROUP"));
    }

    #[test]
    fn tv_underscore_delimited_library_naming() {
        // Sonarr/Plex and skadi's own canonical layout emit `Show_-_SxxEyy_-_Title`.
        // Underscores must normalize so the SxxEyy parses and the series title is
        // clean (no trailing separator) — so re-scanning a library groups by show
        // instead of one unparsed "series" per file (SKADI-I-0047).
        let p = parse_tv("12-monkeys_-_S01E01_-_splinter.mkv");
        assert_eq!(p.title.as_deref(), Some("12-monkeys"));
        assert_eq!(p.season, Some(1));
        assert_eq!(p.episodes, vec![1]);
        // Spaced `Show - SxxEyy - Title` is the same shape and must match too.
        let q = parse_tv("3 Body Problem - S01E03 - Destroyer.mkv");
        assert_eq!(q.title.as_deref(), Some("3 Body Problem"));
        assert_eq!(q.season, Some(1));
        assert_eq!(q.episodes, vec![3]);
    }

    #[test]
    fn tv_alt_numbering_and_lowercase() {
        let p = parse_tv("Breaking Bad 3x07 720p HDTV");
        assert_eq!(p.title.as_deref(), Some("Breaking Bad"));
        assert_eq!(p.season, Some(3));
        assert_eq!(p.episodes, vec![7]);
        assert_eq!(p.resolution.as_deref(), Some("720p"));
    }

    #[test]
    fn tv_multi_episode_range_is_expanded() {
        let p = parse_tv("Show.Name.S01E05-E07.1080p.BluRay.x264-GRP");
        assert_eq!(p.season, Some(1));
        assert_eq!(p.episodes, vec![5, 6, 7]);
    }

    #[test]
    fn tv_multi_episode_consecutive_markers() {
        let p = parse_tv("Show.S01E05E06.1080p.WEB");
        assert_eq!(p.season, Some(1));
        assert_eq!(p.episodes, vec![5, 6]);
    }

    #[test]
    fn tv_descending_pseudo_range_is_not_a_range() {
        // `E36-1.28` is a trailing number, not a range — parsing it as [36, 1]
        // would falsely satisfy E01 with an E36 file (SKADI-T-0324 review).
        let p = parse_tv("death_note-S01E36-1.28-bluray-1080p.mkv");
        assert_eq!(p.season, Some(1));
        assert_eq!(p.episodes, vec![36]);

        let p = parse_tv("some-show-S02E10-2-fast-2-furious.mkv");
        assert_eq!(p.season, Some(2));
        assert_eq!(p.episodes, vec![10]);
    }

    #[test]
    fn tv_full_season_pack() {
        let p = parse_tv("The.Wire.S03.1080p.BluRay.x264-GROUP");
        assert_eq!(p.title.as_deref(), Some("The Wire"));
        assert_eq!(p.season, Some(3));
        assert!(p.full_season, "S03 with no episode is a season pack");
        assert!(p.episodes.is_empty());
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn tv_full_season_worded() {
        let p = parse_tv("Friends.Season.2.COMPLETE.720p.WEB-DL");
        assert_eq!(p.season, Some(2));
        assert!(p.full_season);
    }

    #[test]
    fn tv_anime_absolute_with_group_tag() {
        let p = parse_tv("[SubsPlease] Frieren - 28 (1080p) [ABCD1234]");
        assert_eq!(p.title.as_deref(), Some("Frieren"));
        assert_eq!(p.absolute, vec![28]);
        assert!(p.season.is_none() && p.episodes.is_empty());
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn tv_daily_air_date() {
        let p = parse_tv("The.Daily.Show.2024.03.01.1080p.WEB.h264-GROUP");
        assert_eq!(p.title.as_deref(), Some("The Daily Show"));
        assert_eq!(p.air_date.as_deref(), Some("2024-03-01"));
        assert!(p.season.is_none() && p.episodes.is_empty());
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn tv_year_in_title_is_kept() {
        let p = parse_tv("Doctor.Who.2005.S01E01.1080p.BluRay-GROUP");
        assert_eq!(p.year, Some(2005));
        assert_eq!(p.season, Some(1));
        assert_eq!(p.episodes, vec![1]);
    }

    #[test]
    fn tv_remux_source_implies_modifier() {
        let p = parse_tv("Chernobyl.S01E01.2160p.UHD.BDRemux-GROUP");
        assert_eq!(p.resolution.as_deref(), Some("2160p"));
        assert!(p.modifiers.iter().any(|m| m == "Remux"));
    }

    #[test]
    fn tv_parse_does_not_leak_into_movie_fields() {
        let p = parse_tv("Show.S01E01.1080p.WEB-DL-GRP");
        assert!(p.author.is_none() && p.audio_format.is_none() && p.edition.is_none());
    }
}

#[cfg(test)]
mod sd_quality_tests {
    use super::*;
    use crate::quality::default_definitions;

    /// SKADI-T-0412: a probed 480p DVD rip must map to the DVD tier. Before the
    /// fix `to_quality` found no `R480p` row and returned `None`, which the hunter
    /// stored as the profile cutoff — a file that could never be upgraded.
    #[test]
    fn sd_resolutions_match_the_sd_definitions() {
        let defs = default_definitions();
        let parsed = parse("Movie.2004.DVDRip.XviD-GRP.avi");
        let probed = reconcile_with_probe(&parsed, Some("480p"), None);
        let q = to_quality(&probed, &defs).expect("480p DVD rip maps to a definition");
        assert_eq!(q.resolution, Resolution::R480p, "parsed resolution is kept");
        assert_eq!(
            defs.iter().find(|d| d.id == q.id).map(|d| d.name.as_str()),
            Some("DVD")
        );

        let hdtv = ParsedRelease {
            resolution: Some("576p".into()),
            source: Some("HDTV".into()),
            ..ParsedRelease::default()
        };
        assert_eq!(
            to_quality(&hdtv, &defs)
                .and_then(|q| defs.iter().find(|d| d.id == q.id).map(|d| d.name.clone())),
            Some("SDTV".to_string())
        );
    }

    /// A title with no resolution/source at all still has no quality: callers
    /// record `UNKNOWN_QUALITY_ID`, never the profile cutoff.
    #[test]
    fn unrecognised_release_has_no_quality() {
        assert!(to_quality(&parse("Some Movie"), &default_definitions()).is_none());
        assert!(crate::is_unknown_quality(crate::UNKNOWN_QUALITY_ID));
        assert!(
            !default_definitions()
                .iter()
                .any(|d| crate::is_unknown_quality(d.id)),
            "Unknown is not part of the ladder"
        );
    }
}

// --- Music parsing (SKADI-T-0017) -------------------------------------------

/// Music container/codec tokens. A superset of the audiobook set: music adds the
/// lossless formats (ALAC, APE, WavPack, WAV) that audiobooks never ship in.
static MUSIC_FORMAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(flac|alac|wavpack|wv|ape|wav|mp3|aac|m4a|ogg|vorbis|opus|wma)\b").unwrap()
});
/// A LAME VBR preset, as release titles write it: `V0`, `V2`, `-V0`, or bare
/// `VBR`. Anchored on a word boundary so a group name like "V0LTAGE" is not one.
static MUSIC_PRESET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b-?(V[02])\b|\b(VBR)\b").unwrap());
/// A bitrate: `320`, `320kbps`, `1053 kbps`.
///
/// The bare (suffixless) form is an **explicit list of real CBR values** rather
/// than any three-digit number. Music titles are full of numbers that are not
/// bitrates — track counts, catalogue numbers, `Vol. 320` — and matching them
/// would grade a release on a coincidence.
///
/// Four digits allowed in the suffixed form because lossless
/// titles often carry a computed rate in the thousands — which
/// [`crate::music::to_music_quality`] deliberately ignores, but the parser should
/// still report honestly rather than silently truncating it to three digits.
static MUSIC_BITRATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(\d{2,4})\s?kbps\b|\b(96|112|128|160|192|224|256|288|320)\b").unwrap()
});
/// Hi-res markers: a bit depth of 24 or 32, or a sample rate above 48kHz.
/// `24-96` is the common shorthand for both at once.
static HIRES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(24|32)\s?-?\s?bits?\b|\b(24|32)-(?:44|48|88|96|176|192)\b|\b(88|96|176|192)(?:\.\d)?\s?khz\b")
        .unwrap()
});
/// CD-rate markers, which explicitly say "not hi-res".
static CD_RATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b16\s?-?\s?bits?\b|\b44(?:\.1)?\s?khz\b").unwrap());
/// Disc number in a multi-disc release.
static DISC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:cd|disc|disk)\s?\.?\s?(\d{1,2})\b").unwrap());
/// Music edition markers.
static MUSIC_EDITION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(deluxe|remaster(?:ed)?|anniversary|expanded|special\s+edition|reissue|bonus\s+tracks?|explicit|clean)\b",
    )
    .unwrap()
});

/// Parse a **music** release title (SKADI-T-0017).
///
/// Brackets are kept, as in [`parse_audiobook`] and unlike the movie path: for
/// music they carry the format, bitrate and hi-res markers that are the entire
/// quality signal.
///
/// The music domain does not exist yet, so nothing calls this in production. It
/// is the half of SKADI-T-0017 that does not depend on the domain — the quality
/// engine is shared — and having it tested now means the domain, when it lands,
/// inherits a graded parser rather than starting one.
#[must_use]
pub fn parse_music(title: &str) -> ParsedRelease {
    let mut owned = title.trim().to_string();
    if let Some(m) = EXT_RE.find(&owned) {
        owned.truncate(m.start());
    }
    let work = owned.trim();

    // Hi-res only when the title says so and does *not* also state CD rates: a
    // "16bit 44.1kHz" release naming 24 elsewhere (a track count, a year) is not
    // hi-res, and the explicit CD marker is the stronger signal.
    let music_resolution = if CD_RATE_RE.is_match(work) {
        Some(false)
    } else if HIRES_RE.is_match(work) {
        Some(true)
    } else {
        None
    };

    let mut p = ParsedRelease {
        audio_format: MUSIC_FORMAT_RE.find(work).map(|m| m.as_str().to_string()),
        bitrate_kbps: MUSIC_BITRATE_RE.captures(work).and_then(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .and_then(|m| m.as_str().parse::<u32>().ok())
        }),
        bitrate_preset: MUSIC_PRESET_RE.captures(work).and_then(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().to_ascii_uppercase())
        }),
        music_resolution,
        disc: DISC_RE
            .captures(work)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<u16>().ok()),
        music_edition: {
            let mut seen: Vec<String> = Vec::new();
            for m in MUSIC_EDITION_RE.find_iter(work) {
                let v = m.as_str().to_string();
                if !seen.iter().any(|s| s.eq_ignore_ascii_case(&v)) {
                    seen.push(v);
                }
            }
            seen
        },
        year: YEAR_RE
            .find(work)
            .and_then(|m| m.as_str().parse::<u16>().ok()),
        ..Default::default()
    };

    // Artist / album: the same `Artist - Album` convention audiobooks use for
    // `Author - Title`, so it reuses the same splitter rather than growing a
    // second one that would drift from it.
    let stripped = ANY_BRACKET_RE.replace_all(work, " ");
    let name = strip_music_tokens(&stripped);
    let (artist, album) = split_author_title(&name);
    p.author = artist;
    p.title = album;

    p
}

/// Drop the tokens `parse_music` has already consumed, so what remains is the
/// artist/album text.
fn strip_music_tokens(s: &str) -> String {
    let mut out = s.to_string();
    for re in [
        &*MUSIC_FORMAT_RE,
        &*MUSIC_PRESET_RE,
        &*MUSIC_BITRATE_RE,
        &*HIRES_RE,
        &*CD_RATE_RE,
        &*DISC_RE,
        &*MUSIC_EDITION_RE,
        &*YEAR_RE,
    ] {
        out = re.replace_all(&out, " ").into_owned();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod music_parser_tests {
    use super::*;

    #[test]
    fn artist_album_and_year_come_off_the_common_form() {
        let p = parse_music("Pink Floyd - The Wall (1979) [FLAC]");
        assert_eq!(p.author.as_deref(), Some("Pink Floyd"));
        assert_eq!(p.title.as_deref(), Some("The Wall"));
        assert_eq!(p.year, Some(1979));
        assert_eq!(p.audio_format.as_deref(), Some("FLAC"));
    }

    #[test]
    fn a_lame_preset_is_kept_as_a_preset_not_a_bitrate() {
        // The distinction the whole tier list depends on: V0 must not become
        // "about 245 kbps" on the way through the parser.
        let p = parse_music("Artist - Album (2019) [MP3 V0]");
        assert_eq!(p.bitrate_preset.as_deref(), Some("V0"));
        assert_eq!(p.bitrate_kbps, None);

        let p = parse_music("Artist - Album (2019) [MP3-V2]");
        assert_eq!(p.bitrate_preset.as_deref(), Some("V2"));
    }

    #[test]
    fn a_group_name_that_contains_a_preset_is_not_a_preset() {
        // "V0LTAGE" must not read as V0 — the word boundary is load-bearing.
        let p = parse_music("Artist - Album (2019) [MP3 320]-V0LTAGE");
        assert_eq!(p.bitrate_preset, None);
        assert_eq!(p.bitrate_kbps, Some(320));
    }

    #[test]
    fn hi_res_and_cd_markers_are_distinguished() {
        assert_eq!(
            parse_music("Artist - Album [FLAC 24bit 96kHz]").music_resolution,
            Some(true)
        );
        assert_eq!(
            parse_music("Artist - Album [FLAC 24-96]").music_resolution,
            Some(true)
        );
        // An explicit CD rate wins over an incidental 24 elsewhere.
        assert_eq!(
            parse_music("Artist - Album [FLAC 16bit 44.1kHz]").music_resolution,
            Some(false)
        );
        // Unstated stays unstated rather than defaulting to a claim.
        assert_eq!(parse_music("Artist - Album [FLAC]").music_resolution, None);
    }

    #[test]
    fn disc_numbers_and_edition_markers_are_captured() {
        let p = parse_music("Artist - Album (Deluxe Edition) (2019) CD2 [FLAC]");
        assert_eq!(p.disc, Some(2));
        assert!(
            p.music_edition
                .iter()
                .any(|e| e.eq_ignore_ascii_case("deluxe"))
        );

        let p = parse_music("Artist - Album [Remastered] [Anniversary] [MP3 320]");
        assert_eq!(p.music_edition.len(), 2);
    }

    #[test]
    fn a_four_digit_lossless_bitrate_is_reported_not_truncated() {
        // The grader ignores it, but reporting 105 for "1053kbps" would be a lie
        // that later shows up as a wrong tier if anything ever does read it.
        let p = parse_music("Artist - Album [FLAC 1053kbps]");
        assert_eq!(p.bitrate_kbps, Some(1053));
    }
}
