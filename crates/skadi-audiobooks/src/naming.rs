//! Canonical on-disk naming for the audiobooks library (SKADI-I-0017).
//!
//! Single source of truth for where an audiobook file lives under its root
//! folder; both the acquire matcher (SKADI-T-0127) and library-import commit
//! (SKADI-T-0134) build destinations through here so the layouts can't drift.
//!
//! Layout (Author → [Series →] Book; no spaces, ASIN-tagged book folder for
//! exact re-matching):
//!
//! ```text
//! <root>/andy-weir/project-hail-mary_{asin-B08G9PRS1K}/project-hail-mary.m4b
//! <root>/brandon-sanderson/stormlight-archive/1_-_the-way-of-kings_{asin-...}/<file>.mp3
//! ```
//!
//! A multi-file audiobook (a folder of MP3s) keeps each source file's name under
//! the book folder; a single-file audiobook (one M4B) is renamed to the title.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skadi_naming::{render_component, sanitize_component};

/// Default audiobook folder template (SKADI-T-0228): `Author / [Series /] book folder`.
/// The `{Series}` segment renders empty (and is dropped) when there's no series; the
/// `{Position}` token carries its `" - "` separator only for a positioned series entry.
/// Rendered + sanitized (`space = '_'`) this reproduces the historical layout.
pub const AUDIOBOOK_FOLDER_TEMPLATE: &str = skadi_naming::defaults::AUDIOBOOK_FOLDER;
/// Default single-file (M4B) filename template.
pub const AUDIOBOOK_FILE_TEMPLATE: &str = skadi_naming::defaults::AUDIOBOOK_FILE;
/// Whitespace → `_` (the "no spaces" layout).
pub const AUDIOBOOK_SPACE: char = skadi_naming::defaults::SPACE;

/// Resolved audiobook naming templates (SKADI-T-0228): operator config overrides, else
/// the built-in defaults reproducing the current layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudiobookNaming {
    pub folder: String,
    pub file: String,
    pub space: char,
}

impl Default for AudiobookNaming {
    fn default() -> Self {
        Self {
            folder: AUDIOBOOK_FOLDER_TEMPLATE.to_string(),
            file: AUDIOBOOK_FILE_TEMPLATE.to_string(),
            space: AUDIOBOOK_SPACE,
        }
    }
}

impl AudiobookNaming {
    /// Read `naming.audiobook_folder`/`naming.audiobook_file`/`naming.space` from the
    /// config view; an empty/absent template falls back to the built-in default.
    #[must_use]
    pub fn from_view(view: &skadi_config::ConfigView) -> Self {
        let tpl = |key: &str, default: &str| {
            view.get_string(key)
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let space = view
            .get_string("naming.space")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or(AUDIOBOOK_SPACE);
        Self {
            folder: tpl("naming.audiobook_folder", AUDIOBOOK_FOLDER_TEMPLATE),
            file: tpl("naming.audiobook_file", AUDIOBOOK_FILE_TEMPLATE),
            space,
        }
    }

    /// Build an audiobook path with these templates.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn path(
        &self,
        root: &Path,
        author: Option<&str>,
        series: Option<&str>,
        series_position: Option<&str>,
        title: &str,
        asin: Option<&str>,
        source: &Path,
        single_file: bool,
    ) -> PathBuf {
        audiobook_path_with_templates(
            root,
            &self.folder,
            &self.file,
            self.space,
            author,
            series,
            series_position,
            title,
            asin,
            source,
            single_file,
        )
    }
}

/// Build the canonical library path for one audiobook file.
///
/// `single_file` controls the filename: `true` renames to `<Title>.<ext>` (a
/// single M4B); `false` keeps the source file's name (a multi-file MP3 folder).
/// Missing author/series/asin segments are omitted.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn canonical_audiobook_path(
    root: &Path,
    author: Option<&str>,
    series: Option<&str>,
    series_position: Option<&str>,
    title: &str,
    asin: Option<&str>,
    source: &Path,
    single_file: bool,
) -> PathBuf {
    audiobook_path_with_templates(
        root,
        AUDIOBOOK_FOLDER_TEMPLATE,
        AUDIOBOOK_FILE_TEMPLATE,
        AUDIOBOOK_SPACE,
        author,
        series,
        series_position,
        title,
        asin,
        source,
        single_file,
    )
}

/// [`canonical_audiobook_path`] with explicit (operator-configurable) templates. The
/// folder is rendered through the [`skadi_naming`] engine; the **filename** keeps the
/// domain's single-file-vs-multi-file branch (a multi-file MP3 folder preserves each
/// source name verbatim) so the file template applies only to the single-file case.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn audiobook_path_with_templates(
    root: &Path,
    folder_template: &str,
    file_template: &str,
    space: char,
    author: Option<&str>,
    series: Option<&str>,
    series_position: Option<&str>,
    title: &str,
    asin: Option<&str>,
    source: &Path,
    single_file: bool,
) -> PathBuf {
    let mut tokens: HashMap<&str, String> = HashMap::new();
    tokens.insert(
        "Author",
        author
            .filter(|a| !a.is_empty())
            .unwrap_or("Unknown Author")
            .to_string(),
    );
    tokens.insert(
        "Series",
        series.filter(|s| !s.is_empty()).unwrap_or("").to_string(),
    );
    // Position carries its " - " separator, and only when the entry has a series.
    tokens.insert(
        "Position",
        match series_position {
            Some(pos) if series.filter(|s| !s.is_empty()).is_some() => format!("{pos} - "),
            _ => String::new(),
        },
    );
    tokens.insert("Title", title.to_string());
    tokens.insert(
        "AsinTag",
        asin.map(|a| format!("{{asin-{a}}}")).unwrap_or_default(),
    );
    // Kebab-case variants for the operator's default scheme (SKADI-T-0300).
    tokens.insert(
        "AuthorKebab",
        skadi_naming::kebab(author.filter(|a| !a.is_empty()).unwrap_or("Unknown Author")),
    );
    tokens.insert(
        "SeriesKebab",
        skadi_naming::kebab(series.filter(|s| !s.is_empty()).unwrap_or("")),
    );
    tokens.insert("TitleKebab", skadi_naming::kebab(title));

    let ext = source
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    let mut out = root.to_path_buf();
    for seg in folder_template.split('/') {
        let c = render_component(seg, &tokens, space);
        if !c.is_empty() {
            out.push(c);
        }
    }
    let file_name = if single_file {
        format!("{}{ext}", render_component(file_template, &tokens, space))
    } else {
        // Multi-file: keep the source file's own name (no rename).
        source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}{ext}", sanitize_component(title, space)))
    };
    out.push(file_name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audiobook_naming_from_view_overrides_then_defaults() {
        let def = AudiobookNaming::from_view(&skadi_config::ConfigView::default());
        assert_eq!(def, AudiobookNaming::default());

        let view = skadi_config::ConfigView::from_pairs([(
            "naming.audiobook_folder".to_string(),
            "{Author}/{Title}".to_string(),
        )]);
        let n = AudiobookNaming::from_view(&view);
        assert_eq!(n.folder, "{Author}/{Title}");
        assert_eq!(n.file, AUDIOBOOK_FILE_TEMPLATE, "unset → default");
        // Renders the flattened (no series/asin) override.
        let p = n.path(
            Path::new("/a"),
            Some("Andy Weir"),
            None,
            None,
            "Project Hail Mary",
            Some("B08"),
            Path::new("/dl/x.m4b"),
            true,
        );
        assert_eq!(
            p,
            PathBuf::from("/a/Andy_Weir/Project_Hail_Mary/project-hail-mary.m4b")
        );
    }

    #[test]
    fn single_file_no_series_renames_to_title() {
        let p = canonical_audiobook_path(
            Path::new("/audiobooks"),
            Some("Andy Weir"),
            None,
            None,
            "Project Hail Mary",
            Some("B08G9PRS1K"),
            Path::new("/dl/Andy Weir - Project Hail Mary.m4b"),
            true,
        );
        assert_eq!(
            p,
            PathBuf::from(
                "/audiobooks/andy-weir/project-hail-mary_{asin-B08G9PRS1K}/project-hail-mary.m4b"
            )
        );
    }

    #[test]
    fn series_with_position_and_multi_file_keeps_source_name() {
        let p = canonical_audiobook_path(
            Path::new("/audiobooks"),
            Some("Brandon Sanderson"),
            Some("Stormlight Archive"),
            Some("1"),
            "The Way of Kings",
            Some("B003"),
            Path::new("/dl/WoK/Chapter_01.mp3"),
            false,
        );
        assert_eq!(
            p,
            PathBuf::from(
                "/audiobooks/brandon-sanderson/stormlight-archive/1_-_the-way-of-kings_{asin-B003}/Chapter_01.mp3"
            )
        );
    }

    #[test]
    fn missing_author_and_asin_segments_are_handled() {
        let p = canonical_audiobook_path(
            Path::new("/audiobooks"),
            None,
            None,
            None,
            "Some Title",
            None,
            Path::new("/dl/x.mp3"),
            true,
        );
        assert_eq!(
            p,
            PathBuf::from("/audiobooks/unknown-author/some-title/some-title.mp3")
        );
    }

    #[test]
    fn unsafe_chars_and_spaces_are_underscored() {
        let p = canonical_audiobook_path(
            Path::new("/audiobooks"),
            Some("A/B: C"),
            None,
            None,
            "What? If*",
            None,
            Path::new("/dl/x.m4b"),
            true,
        );
        let s = p.to_string_lossy();
        assert!(!s.contains(':') && !s.contains('?') && !s.contains('*') && !s.contains(' '));
    }
}
