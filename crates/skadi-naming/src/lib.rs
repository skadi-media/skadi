//! `skadi-naming` — the Naming / Path Engine (C21/C22, SKADI-I-0031).
//!
//! A tiny, dependency-free template engine that turns a `{Token}` rename template
//! plus a token map into a sanitized, filesystem-safe library path — the configurable
//! equivalent of Radarr/Sonarr's naming format. It is **pure**: no I/O, no domain
//! knowledge. Domains build the token map (from their metadata + the parsed release)
//! and pick templates (operator config, else the defaults that reproduce the current
//! layout byte-for-byte); the engine substitutes, tidies away empty `()`/`[]` left by
//! missing tokens, and sanitizes each path component.
//!
//! ```
//! use std::collections::HashMap;
//! use std::path::Path;
//! use skadi_naming::render_library_path;
//!
//! let mut t = HashMap::new();
//! t.insert("Title", "The Matrix".to_string());
//! t.insert("Year", "1999".to_string());
//! t.insert("TmdbTag", "{tmdb-603}".to_string());
//! t.insert("EditionFolder", "Theatrical".to_string());
//! t.insert("EditionSuffix", String::new());
//! let p = render_library_path(
//!     Path::new("/movies"),
//!     "{Title} ({Year}) {TmdbTag}/{EditionFolder}",
//!     "{Title} ({Year}){EditionSuffix}",
//!     &t,
//!     ".mkv",
//!     '_',
//! );
//! assert_eq!(p, Path::new("/movies/The_Matrix_(1999)_{tmdb-603}/Theatrical/The_Matrix_(1999).mkv"));
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The token-value map a template renders against. Keys are token names (without the
/// braces); a token absent from the map (or any unknown `{Token}`) renders empty.
pub type Tokens<'a> = HashMap<&'a str, String>;

/// Substitute `{Token}` placeholders in `template` from `tokens` in a single pass.
/// Unknown / absent tokens render to the empty string. Token **values** are inserted
/// verbatim and never re-scanned, so a value containing braces (e.g. `{tmdb-603}`) is
/// safe. An unmatched `{` is emitted literally.
#[must_use]
pub fn render(template: &str, tokens: &Tokens) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if let Some(close) = after.find('}') {
            let spec = &after[..close];
            // Sonarr's numeric padding: `{Season:00}` renders `5` as `05`
            // (SKADI-T-0420). The modifier is a run of zeros giving the minimum
            // width, so `:000` pads to three. Applied only to a value that parses
            // as a number — padding "Title" would be nonsense, and silently
            // mangling it would be worse than ignoring the modifier.
            let (name, pad) = match spec.split_once(':') {
                Some((n, m)) if !m.is_empty() && m.chars().all(|c| c == '0') => (n, m.len()),
                _ => (spec, 0),
            };
            if let Some(val) = tokens.get(name) {
                match (pad, val.parse::<i64>()) {
                    (w, Ok(n)) if w > 0 => out.push_str(&format!("{n:0w$}")),
                    _ => out.push_str(val),
                }
            } else {
                // TRACE, not warn (SKADI-T-0426): templates legitimately name
                // optional tokens ({Edition}, {Year}) that are absent for most
                // items, and `tidy` exists to clean up after them. Still worth a
                // record, because a typo'd token ({Titel}) looks exactly the same
                // from here and silently produces a wrong path.
                tracing::trace!(token = %name, %template, "naming: token not supplied");
            }
            rest = &after[close + 1..];
        } else {
            // No closing brace — emit the rest literally and stop.
            tracing::warn!(%template, "naming: unclosed '{{' in template; emitting the remainder literally");
            out.push_str(&rest[open..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Remove empty bracket pairs left by missing tokens (`({Year})` → `()` → ``). Runs to
/// a fixed point so nested emptiness (`[()]`) also collapses. Brace pairs are **not**
/// touched (token values legitimately contain `{...}` id tags).
#[must_use]
pub fn tidy(s: &str) -> String {
    let mut out = s.to_string();
    loop {
        let before = out.len();
        out = out.replace("()", "").replace("[]", "");
        if out.len() == before {
            return out;
        }
    }
}

/// How a colon in a title is rendered (Sonarr's "Colon Replacement",
/// SKADI-T-0420).
///
/// A colon is illegal in a path component on Windows and awkward over SMB, so it
/// always has to go — but *what it becomes* changes how the title reads, and
/// that is the operator's call, not ours. `Rebel Moon: Part One` is a very
/// different-looking folder as `Rebel Moon_ Part One` versus
/// `Rebel Moon - Part One`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColonStyle {
    /// `_` — the historical behaviour, and still the default so no existing
    /// library silently re-names itself.
    #[default]
    Underscore,
    /// Delete it: `Rebel Moon: Part One` → `Rebel Moon Part One`.
    Delete,
    /// ` - ` (space dash space), Sonarr's most-used option.
    SpaceDash,
    /// ` ` — a plain space.
    Space,
}

impl ColonStyle {
    /// Parse the operator's setting; unknown values fall back to the default
    /// rather than failing a render, since a bad setting must not stop imports.
    #[must_use]
    pub fn from_str_or_default(s: &str) -> Self {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "")
            .as_str()
        {
            "delete" => Self::Delete,
            "spacedash" => Self::SpaceDash,
            "space" => Self::Space,
            _ => Self::Underscore,
        }
    }

    fn replacement(self) -> &'static str {
        match self {
            Self::Underscore => "_",
            Self::Delete => "",
            Self::SpaceDash => " - ",
            Self::Space => " ",
        }
    }
}

/// [`sanitize_component`] with an explicit colon style (SKADI-T-0420).
#[must_use]
pub fn sanitize_component_with(s: &str, space: char, colon: ColonStyle) -> String {
    // Substitute colons first, then sanitize: the replacement may itself contain
    // spaces (` - `), which must go through the same space-replacement and
    // run-collapsing as the rest of the title rather than bypassing it.
    let swapped = if colon == ColonStyle::Underscore {
        s.to_string()
    } else {
        s.replace(':', colon.replacement())
    };
    sanitize_component(&swapped, space)
}

/// Why a template cannot be saved (SKADI-T-0420).
///
/// Checked at **save** time, not render time: a template with a typo'd token
/// renders to a plausible-looking wrong path, and the operator finds out when
/// files land somewhere unexpected. Sonarr validates on save for the same reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateError {
    /// A `{` with no matching `}`.
    UnbalancedBrace { at: usize },
    /// A token the domain does not supply — almost always a typo, since the
    /// alternative (an intentionally-absent optional token) is still a *known*
    /// name.
    UnknownToken(String),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnbalancedBrace { at } => {
                write!(f, "unbalanced brace at position {at}")
            }
            Self::UnknownToken(t) => write!(f, "unknown token {t:?}"),
        }
    }
}

/// Validate `template` against the token names a domain supplies
/// (SKADI-T-0420). `Ok(())` when every `{...}` names a known token and every
/// brace is matched.
///
/// Padding modifiers are stripped before the name is checked, so `{Season:00}`
/// validates exactly as `{Season}` does.
pub fn validate_template(template: &str, known: &[&str]) -> std::result::Result<(), TemplateError> {
    let mut rest = template;
    let mut base = 0usize;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(TemplateError::UnbalancedBrace { at: base + open });
        };
        let spec = &after[..close];
        let name = spec.split_once(':').map_or(spec, |(n, _)| n);
        if !known.contains(&name) {
            return Err(TemplateError::UnknownToken(name.to_string()));
        }
        base += open + 1 + close + 1;
        rest = &after[close + 1..];
    }
    Ok(())
}

/// Make one path component filesystem-safe: map `/\:*?"<>|` to `_`, whitespace to
/// `space` (the configurable space-replacement, default `_`), then collapse runs of
/// `_`/`space` and trim them from the ends. With `space == '_'` this is the historical
/// "no spaces, underscores" layout.
#[must_use]
pub fn sanitize_component(s: &str, space: char) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        let mapped = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_whitespace() => space,
            c => c,
        };
        // Collapse a run of the *same* replacement char (`__` → `_`), but keep an
        // `_` followed by a space etc. so space-preserving mode reads naturally.
        let collapsible = mapped == '_' || mapped == space;
        if collapsible && prev == Some(mapped) {
            continue;
        }
        out.push(mapped);
        prev = Some(mapped);
    }
    let out = out.trim_matches(|c| c == '_' || c == space).to_string();
    // `.` and `..` are not names, they are directions (SKADI-T-0415). A title of
    // ".." rendered `/library/movie/../..mkv`, walking out of the library root
    // that NFR-NAMING.3 promises to stay inside. Note the existing separator
    // mapping does not catch this: "../../etc/passwd" becomes ".._.._etc_passwd",
    // which is harmless, but a title that is *only* dots survives untouched.
    //
    // Checked BEFORE the platform guards, which strip trailing dots
    // (SKADI-T-0421) and would otherwise reduce ".." to an empty component —
    // safe, but it silently loses the whole name instead of standing in for it.
    if !out.is_empty() && out.chars().all(|c| c == '.') {
        return "_".to_string();
    }
    let guarded = apply_platform_guards(&out);
    // An *empty* input stays empty on purpose: callers drop empty components,
    // which is how a book with no series avoids gaining a stray directory. But a
    // name that had content and was guarded down to nothing (all dots and
    // spaces, say) becomes `_` rather than vanishing.
    if guarded.is_empty() && !out.is_empty() {
        return "_".to_string();
    }
    guarded
}

/// Windows/DOS device names, which cannot be used as a file name on Windows or
/// over SMB even with an extension (`CON.mkv` is still `CON`).
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The longest single path component most filesystems accept, in bytes
/// (ext4, APFS, NTFS and SMB all sit at or above this).
const MAX_COMPONENT_BYTES: usize = 255;

/// Make a sanitized component safe on every filesystem we might land on
/// (SKADI-T-0421).
///
/// The library is routinely shared over SMB and read from Windows, so a name that
/// is legal on ext4 is not enough. Four guards, in the order that matters:
///
/// 1. **NFC normalisation.** macOS hands out decomposed filenames and most other
///    sources hand out composed ones, so "Amélie" could otherwise produce two
///    different directories for one title. Done first, since it changes lengths.
/// 2. **Reserved device names.** `CON`, `NUL`, `COM1`… are unusable on Windows
///    whatever the extension.
/// 3. **255-byte truncation**, on a character boundary so the result stays valid
///    UTF-8.
/// 4. **No trailing dot or space**, which Windows silently strips — leaving the
///    on-disk name different from the one we recorded.
fn apply_platform_guards(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let mut out: String = s.nfc().collect();

    if RESERVED_DEVICE_NAMES
        .iter()
        .any(|r| out.eq_ignore_ascii_case(r))
    {
        out.push('_');
    }

    if out.len() > MAX_COMPONENT_BYTES {
        let mut end = MAX_COMPONENT_BYTES;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        // Truncation changes the name on disk from the one the template asked
        // for, which is exactly the kind of silent divergence an operator hunting
        // a "missing" file needs to be able to find (SKADI-T-0426).
        tracing::debug!(
            original_bytes = out.len(),
            truncated_to = end,
            "naming: component truncated to the filesystem limit"
        );
        out.truncate(end);
    }

    // After truncation, because truncating can expose a trailing dot.
    let trimmed = out.trim_end_matches(['.', ' ']);
    if trimmed.len() != out.len() {
        out.truncate(trimmed.len());
    }
    out
}

/// Kebab-case a token value: lowercase, collapse every run of non-alphanumeric
/// characters to a single `-`, and trim `-` from the ends. Used for the
/// `{TitleKebab}`/edition tokens. `"Rebel Moon - Part One: A Child of Fire"` →
/// `"rebel-moon-part-one-a-child-of-fire"`; `"Big Buck Bunny"` → `"big-buck-bunny"`.
#[must_use]
pub fn kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_sep = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

/// Render one component end-to-end: substitute → tidy → sanitize.
#[must_use]
pub fn render_component(template: &str, tokens: &Tokens, space: char) -> String {
    sanitize_component(&tidy(&render(template, tokens)), space)
}

/// Build a full library path from a folder template (which may contain `/` for nested
/// subfolders), a file template, and an extension (`ext` includes the leading dot, or
/// is empty). Each rendered component is sanitized; empty components are dropped so a
/// missing token never leaves a blank directory level.
#[must_use]
pub fn render_library_path(
    root: &Path,
    folder_template: &str,
    file_template: &str,
    tokens: &Tokens,
    ext: &str,
    space: char,
) -> PathBuf {
    let mut out = root.to_path_buf();
    for seg in folder_template.split('/') {
        let c = render_component(seg, tokens, space);
        if c.is_empty() {
            // A folder level that rendered to nothing is skipped, so the file
            // lands one directory shallower than the template describes
            // (SKADI-T-0426). Usually intentional (an optional level), but it is
            // also what a missing token looks like.
            tracing::debug!(segment = %seg, "naming: folder segment rendered empty; skipping the level");
            continue;
        }
        out.push(c);
    }
    let mut file = render_component(file_template, tokens, space);
    file.push_str(ext);
    out.push(file);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(pairs: &[(&'static str, &str)]) -> Tokens<'static> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    #[test]
    fn platform_guards_keep_a_name_usable_over_smb_and_on_windows() {
        // SKADI-T-0421. The library is routinely shared over SMB and read from
        // Windows, so "legal on ext4" is not the bar.
        assert_eq!(
            sanitize_component("Mr. Robot S01E01.", '_'),
            "Mr._Robot_S01E01"
        );
        assert!(!sanitize_component("Trailing space ", '_').ends_with(' '));
        // Reserved device names are unusable on Windows whatever the extension.
        assert_eq!(sanitize_component("CON", '_'), "CON_");
        assert_eq!(sanitize_component("nul", '_'), "nul_");
        assert_eq!(sanitize_component("COM4", '_'), "COM4_");
        // A name that merely contains one is fine.
        assert_eq!(sanitize_component("Contact", '_'), "Contact");
        // Truncated to the filesystem limit, on a character boundary.
        let long = "é".repeat(300);
        let out = sanitize_component(&long, '_');
        assert!(out.len() <= 255, "{} bytes", out.len());
        assert!(
            std::str::from_utf8(out.as_bytes()).is_ok(),
            "still valid UTF-8"
        );
        // One title, one on-disk name: decomposed input normalises to composed.
        let decomposed = "Ame\u{301}lie";
        let composed = "Amélie";
        assert_ne!(decomposed, composed, "the fixture must actually differ");
        assert_eq!(
            sanitize_component(decomposed, '_'),
            sanitize_component(composed, '_')
        );
    }

    #[test]
    fn a_dot_only_component_cannot_walk_out_of_the_root() {
        // SKADI-T-0415: `.` and `..` are directions, not names. The separator
        // mapping already defuses a path-shaped title ("../../etc/passwd" →
        // ".._.._etc_passwd"), but a title that is *only* dots passed through
        // untouched and rendered `/library/movie/../..mkv`.
        assert_eq!(sanitize_component("..", '_'), "_");
        assert_eq!(sanitize_component(".", '_'), "_");
        assert_eq!(sanitize_component("....", '_'), "_");
        // A leading dot is fine — it makes a hidden file, not an escape — and a
        // dot inside a real title must survive.
        assert_eq!(sanitize_component("Mr. Robot", '_'), "Mr._Robot");
        assert_eq!(sanitize_component(".hidden", '_'), ".hidden");
        // An empty component stays empty: callers drop it, which is how a book
        // with no series avoids gaining a stray directory.
        assert_eq!(sanitize_component("", '_'), "");
        assert_eq!(sanitize_component("   ", '_'), "");
    }

    #[test]
    fn render_substitutes_known_and_blanks_unknown() {
        let t = tok(&[("Title", "The Matrix"), ("Year", "1999")]);
        assert_eq!(render("{Title} ({Year})", &t), "The Matrix (1999)");
        // Unknown token → empty; value braces are not re-scanned.
        let t = tok(&[("TmdbTag", "{tmdb-603}")]);
        assert_eq!(render("{Missing}{TmdbTag}", &t), "{tmdb-603}");
        // Unmatched brace is literal.
        assert_eq!(render("a {oops", &tok(&[])), "a {oops");
    }

    #[test]
    fn tidy_removes_only_empty_bracket_pairs() {
        assert_eq!(tidy("The Matrix () {tmdb-603}"), "The Matrix  {tmdb-603}");
        assert_eq!(tidy("a [] b"), "a  b");
        assert_eq!(tidy("[()]"), "");
        assert_eq!(tidy("keep (1999)"), "keep (1999)");
        assert_eq!(tidy("{tmdb-1}"), "{tmdb-1}", "brace pairs untouched");
    }

    #[test]
    fn sanitize_underscores_unsafe_and_collapses() {
        assert_eq!(sanitize_component("What: If?/Maybe", '_'), "What_If_Maybe");
        assert_eq!(
            sanitize_component("The Matrix  (1999)", '_'),
            "The_Matrix_(1999)"
        );
        assert_eq!(sanitize_component("  trim  ", '_'), "trim");
        // Space-preserving mode keeps spaces but still underscores unsafe chars
        // (the `_` from the colon and the following space are kept distinct).
        assert_eq!(sanitize_component("A: B", ' '), "A_ B");
        assert_eq!(sanitize_component("A   B", ' '), "A B", "spaces collapse");
    }

    #[test]
    fn render_library_path_reproduces_the_movie_layout() {
        let folder = "{Title} ({Year}) {TmdbTag}/{EditionFolder}";
        let file = "{Title} ({Year}){EditionSuffix}";

        // Theatrical, full metadata.
        let t = tok(&[
            ("Title", "The Matrix"),
            ("Year", "1999"),
            ("TmdbTag", "{tmdb-603}"),
            ("EditionFolder", "Theatrical"),
            ("EditionSuffix", ""),
        ]);
        assert_eq!(
            render_library_path(Path::new("/movies"), folder, file, &t, ".mkv", '_'),
            PathBuf::from("/movies/The_Matrix_(1999)_{tmdb-603}/Theatrical/The_Matrix_(1999).mkv")
        );

        // Edition with a tag → subfolder + filename suffix.
        let t = tok(&[
            ("Title", "Blade Runner"),
            ("Year", "1982"),
            ("TmdbTag", "{tmdb-78}"),
            ("EditionFolder", "Final Cut"),
            ("EditionSuffix", "-Final Cut"),
        ]);
        assert_eq!(
            render_library_path(Path::new("/movies"), folder, file, &t, ".mp4", '_'),
            PathBuf::from(
                "/movies/Blade_Runner_(1982)_{tmdb-78}/Final_Cut/Blade_Runner_(1982)-Final_Cut.mp4"
            )
        );

        // Missing year + tmdb → those segments collapse away.
        let t = tok(&[
            ("Title", "Blade Runner"),
            ("Year", ""),
            ("TmdbTag", ""),
            ("EditionFolder", "Theatrical"),
            ("EditionSuffix", ""),
        ]);
        assert_eq!(
            render_library_path(Path::new("/movies"), folder, file, &t, ".mkv", '_'),
            PathBuf::from("/movies/Blade_Runner/Theatrical/Blade_Runner.mkv")
        );
    }
}

/// The default rename templates, in the one crate every consumer already depends
/// on (SKADI-T-0427).
///
/// These used to be duplicated in `skadi-api/src/naming.rs` under a "KEEP IN SYNC"
/// comment, because the domain crates depend on `skadi-api` and not the reverse —
/// so the control plane could not read the domains' constants. It drifted, as
/// such comments do: the API's copy was missing the anime and daily episode
/// templates that `skadi-tv` reads, so an operator editing naming through the API
/// could not see or set them.
///
/// `skadi-naming` is a leaf crate that already renders these templates, so owning
/// their defaults puts them below everything that needs them.
pub mod defaults {
    pub const MOVIE_FOLDER: &str = "{TitleKebab} ({Year}) {TmdbTag} {ImdbTag}/{EditionKebab}";
    pub const MOVIE_FILE: &str = "{TitleKebab} ({Year})";
    pub const SERIES_FOLDER: &str = "{SeriesTitleKebab} ({Year}) {TmdbTag}/{SeasonFolder}";
    pub const SERIES_FILE: &str = "{SeriesTitleKebab} - {Episode}{EpisodeTitlePart}";
    /// Anime episodes are numbered absolutely, not by season (SKADI-T-0427).
    pub const SERIES_ANIME_FILE: &str = "{SeriesTitleKebab} - {Absolute}{EpisodeTitlePart}";
    /// Daily shows are keyed by air date.
    pub const SERIES_DAILY_FILE: &str = "{SeriesTitleKebab} - {AirDate}{EpisodeTitlePart}";
    pub const AUDIOBOOK_FOLDER: &str =
        "{AuthorKebab}/{SeriesKebab}/{Position}{TitleKebab} {AsinTag}";
    pub const AUDIOBOOK_FILE: &str = "{TitleKebab}";
    /// What a space in a rendered component becomes on disk.
    pub const SPACE: char = '_';
}
