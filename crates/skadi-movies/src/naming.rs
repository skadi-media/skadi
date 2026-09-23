//! Canonical on-disk naming for the movies library.
//!
//! The single source of truth for where a movie file lives under its root
//! folder. Both the acquire matcher (placing completed downloads) and the
//! library-import commit (restructuring existing files) build destinations
//! through here, so the two flows produce byte-identical layouts and can
//! never drift.
//!
//! Layout (operator-chosen, SKADI-I-0044): kebab-case title, underscore separators,
//! **both** tmdb + imdb ids tagged on the movie folder for exact re-matching, no
//! quality in the filename, and one subfolder per edition:
//!
//! ```text
//! <root>/big-buck-bunny_(2008)_{tmdb-10378}_{imdb-tt1254207}/theatrical/big-buck-bunny_(2008).mkv
//! <root>/the-matrix_(1999)_{tmdb-603}/extended/the-matrix_(1999).mkv
//! ```
//!
//! Trade-off accepted: edition *subfolders* mean Plex/Jellyfin won't group the
//! cuts as one title (they require same-folder files with an edition marker).
//! Templates are operator-configurable (`naming.movie_folder`/`_file`/`naming.space`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skadi_naming::{render_library_path, sanitize_component};

/// Default folder template (SKADI-I-0044): kebab-case title + year + **both** id tags,
/// then a kebab edition subfolder. With `space = _` this renders
/// `big-buck-bunny_(2008)_{tmdb-10378}_{imdb-tt1254207}/theatrical`.
pub const MOVIE_FOLDER_TEMPLATE: &str = skadi_naming::defaults::MOVIE_FOLDER;
/// Default file template: `<title-kebab>_(<year>)` — no quality, no edition suffix
/// (the edition is the subfolder), e.g. `big-buck-bunny_(2008)`.
pub const MOVIE_FILE_TEMPLATE: &str = skadi_naming::defaults::MOVIE_FILE;
/// Whitespace → `_` (the operator-chosen "no spaces" layout).
pub const MOVIE_SPACE: char = skadi_naming::defaults::SPACE;

/// Resolved movie naming templates (SKADI-T-0227): operator config overrides, else the
/// built-in defaults that reproduce the current layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovieNaming {
    pub folder: String,
    pub file: String,
    pub space: char,
}

impl Default for MovieNaming {
    fn default() -> Self {
        Self {
            folder: MOVIE_FOLDER_TEMPLATE.to_string(),
            file: MOVIE_FILE_TEMPLATE.to_string(),
            space: MOVIE_SPACE,
        }
    }
}

impl MovieNaming {
    /// Read `naming.movie_folder`/`naming.movie_file`/`naming.space` from the config
    /// view; an empty/absent template falls back to the built-in default.
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
            .unwrap_or(MOVIE_SPACE);
        Self {
            folder: tpl("naming.movie_folder", MOVIE_FOLDER_TEMPLATE),
            file: tpl("naming.movie_file", MOVIE_FILE_TEMPLATE),
            space,
        }
    }

    /// Build a movie path with these templates (the configurable entry point).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn path(
        &self,
        root: &Path,
        title: &str,
        year: Option<u16>,
        tmdb: Option<u64>,
        imdb: Option<&str>,
        edition_tag: Option<&str>,
        source: &Path,
    ) -> PathBuf {
        movie_path_with_templates(
            root,
            &self.folder,
            &self.file,
            self.space,
            title,
            year,
            tmdb,
            imdb,
            edition_tag,
            source,
        )
    }
}

/// Build the canonical library path for a movie file via the naming engine
/// ([`skadi_naming`]). Default templates reproduce
/// `<root>/<Title>_(<Year>)_{tmdb-<id>}/<EditionFolder>/<Title>_(<Year>)[-<Tag>].<ext>`.
///
/// `edition_tag` is the normalized edition tag for non-Theatrical editions
/// (`None` for Theatrical — folder `Theatrical`, no filename suffix). Year and
/// tmdb segments are omitted when unknown. The extension comes from `source`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn canonical_movie_path(
    root: &Path,
    title: &str,
    year: Option<u16>,
    tmdb: Option<u64>,
    imdb: Option<&str>,
    edition_tag: Option<&str>,
    source: &Path,
) -> PathBuf {
    movie_path_with_templates(
        root,
        MOVIE_FOLDER_TEMPLATE,
        MOVIE_FILE_TEMPLATE,
        MOVIE_SPACE,
        title,
        year,
        tmdb,
        imdb,
        edition_tag,
        source,
    )
}

/// [`canonical_movie_path`] with explicit (operator-configurable) templates. The token
/// map is the seam through which extra `ParsedRelease` tokens (`{Quality}`, `{Source}`,
/// …) can later be added without touching call sites.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn movie_path_with_templates(
    root: &Path,
    folder_template: &str,
    file_template: &str,
    space: char,
    title: &str,
    year: Option<u16>,
    tmdb: Option<u64>,
    imdb: Option<&str>,
    edition_tag: Option<&str>,
    source: &Path,
) -> PathBuf {
    let mut tokens: HashMap<&str, String> = HashMap::new();
    tokens.insert("Title", title.to_string());
    // Kebab-case variant for the operator's default scheme (SKADI-I-0044).
    tokens.insert("TitleKebab", skadi_naming::kebab(title));
    tokens.insert("Year", year.map(|y| y.to_string()).unwrap_or_default());
    // Self-punctuating optional id tags; the literal braces survive one render pass.
    tokens.insert(
        "TmdbTag",
        tmdb.map(|id| format!("{{tmdb-{id}}}")).unwrap_or_default(),
    );
    tokens.insert(
        "ImdbTag",
        imdb.map(|id| format!("{{imdb-{id}}}")).unwrap_or_default(),
    );
    tokens.insert(
        "EditionFolder",
        edition_tag.unwrap_or("Theatrical").to_string(),
    );
    tokens.insert(
        "EditionKebab",
        skadi_naming::kebab(edition_tag.unwrap_or("Theatrical")),
    );
    tokens.insert(
        "EditionSuffix",
        edition_tag.map(|t| format!("-{t}")).unwrap_or_default(),
    );

    // `{Quality Full}` and `{Release Group}` (SKADI-T-0420), parsed from the
    // source filename — which *is* the release name, and where Sonarr/Radarr take
    // them from too. No new parameters for data already in hand: this function
    // already takes ten.
    //
    // Empty when the name carries no such token, so `tidy` collapses the brackets
    // around it rather than leaving `Movie () -`.
    let parsed = skadi_quality::parse(
        &source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    // Sonarr's `{Quality Full}` is the classified tier name (`Bluray-1080p`),
    // which is source + resolution — not the raw tokens.
    tokens.insert(
        "Quality Full",
        match (&parsed.source, &parsed.resolution) {
            (Some(src), Some(res)) => format!("{src}-{res}"),
            (None, Some(res)) => res.clone(),
            _ => String::new(),
        },
    );
    tokens.insert("Release Group", parsed.group.clone().unwrap_or_default());

    let ext = source
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    render_library_path(root, folder_template, file_template, &tokens, &ext, space)
}

/// Replace filesystem-unsafe chars and whitespace with `_` — the canonical sanitizer,
/// now delegating to the shared [`skadi_naming`] engine (SKADI-T-0226).
pub(crate) fn sanitize_path_component(s: &str) -> String {
    sanitize_component(s, MOVIE_SPACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theatrical_goes_in_kebab_subfolder_with_dual_id_tagged_folder() {
        let p = canonical_movie_path(
            Path::new("/movies"),
            "The Matrix",
            Some(1999),
            Some(603),
            Some("tt0133093"),
            None,
            Path::new("/dl/The.Matrix.1999.1080p.mkv"),
        );
        assert_eq!(
            p,
            PathBuf::from(
                "/movies/the-matrix_(1999)_{tmdb-603}_{imdb-tt0133093}/theatrical/the-matrix_(1999).mkv"
            )
        );
    }

    #[test]
    fn edition_gets_its_own_kebab_subfolder_no_filename_suffix() {
        let p = canonical_movie_path(
            Path::new("/movies"),
            "Blade Runner",
            Some(1982),
            Some(78),
            None,
            Some("Final Cut"),
            Path::new("/dl/br.final.cut.mp4"),
        );
        assert_eq!(
            p,
            PathBuf::from(
                "/movies/blade-runner_(1982)_{tmdb-78}/final-cut/blade-runner_(1982).mp4"
            )
        );
    }

    #[test]
    fn missing_year_and_id_segments_are_omitted() {
        let p = canonical_movie_path(
            Path::new("/movies"),
            "Blade Runner",
            None,
            None,
            None,
            None,
            Path::new("/dl/br.mkv"),
        );
        assert_eq!(
            p,
            PathBuf::from("/movies/blade-runner/theatrical/blade-runner.mkv")
        );
    }

    #[test]
    fn movie_naming_from_view_overrides_then_defaults() {
        // Empty view → built-in defaults.
        let def = MovieNaming::from_view(&skadi_config::ConfigView::default());
        assert_eq!(def, MovieNaming::default());

        // Operator overrides folder + file + space with raw {Title} tokens.
        let view = skadi_config::ConfigView::from_pairs([
            (
                "naming.movie_folder".to_string(),
                "{Title} ({Year})".to_string(),
            ),
            (
                "naming.movie_file".to_string(),
                "{Title} ({Year})".to_string(),
            ),
            ("naming.space".to_string(), " ".to_string()),
        ]);
        let n = MovieNaming::from_view(&view);
        assert_eq!(n.folder, "{Title} ({Year})");
        assert_eq!(n.space, ' ');

        // And it renders the override (raw title, spaces preserved).
        let p = n.path(
            Path::new("/m"),
            "The Matrix",
            Some(1999),
            Some(603),
            None,
            None,
            Path::new("/dl/x.mkv"),
        );
        assert_eq!(
            p,
            PathBuf::from("/m/The Matrix (1999)/The Matrix (1999).mkv")
        );
    }

    #[test]
    fn a_custom_template_retemplates_the_layout() {
        // A flat, space-preserving Plex-style layout via custom templates + space=' '.
        let p = movie_path_with_templates(
            Path::new("/movies"),
            "{Title} ({Year})",
            "{Title} ({Year})",
            ' ',
            "The Matrix",
            Some(1999),
            Some(603),
            None,
            None,
            Path::new("/dl/x.mkv"),
        );
        assert_eq!(
            p,
            PathBuf::from("/movies/The Matrix (1999)/The Matrix (1999).mkv")
        );
    }

    #[test]
    fn kebab_strips_unsafe_chars_and_lowercases() {
        let p = canonical_movie_path(
            Path::new("/movies"),
            "What: If?/Maybe",
            Some(2020),
            Some(1),
            None,
            None,
            Path::new("/dl/x.mkv"),
        );
        let s = p.to_string_lossy();
        assert!(
            !s.contains(':') && !s.contains('?') && !s.contains(' '),
            "sanitized: {s}"
        );
        assert!(s.ends_with("what-if-maybe_(2020).mkv"), "got {s}");
    }
}
