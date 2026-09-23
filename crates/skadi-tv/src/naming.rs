//! Canonical on-disk naming for the television library (SKADI-T-0268).
//!
//! The single source of truth for where an episode file lives under its root
//! folder — both the acquire matcher (placing completed downloads) and the
//! library-import commit build destinations through here, so they can never
//! drift. Mirrors `skadi-movies`'s naming over the shared [`skadi_naming`] engine.
//!
//! Layout (operator default — underscores, TMDB-tagged series folder, a season
//! subfolder):
//!
//! ```text
//! <root>/the-wire_(2002)_{tmdb-1438}/Season_03/the-wire_-_S03E05_-_straight-and-true.mkv
//! <root>/frieren_{tmdb-209867}/Season_01/frieren_-_028.mkv                 (anime, absolute)
//! <root>/the-daily-show_{tmdb-2224}/Season_2024/the-daily-show_-_2024-03-01.mkv   (daily)
//! ```
//! Kebab title, no quality in the filename (SKADI-T-0300) — `Season NN` stays
//! scanner-standard; `SxxEyy` stays the canonical episode code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skadi_naming::render_library_path;

use crate::series::SeriesType;

/// Default folder template: the series folder (title/year/tmdb tag) then a season
/// subfolder.
pub const SERIES_FOLDER_TEMPLATE: &str = skadi_naming::defaults::SERIES_FOLDER;
/// Standard file: `<series-kebab> - SxxEyy[ - <episode-title-kebab>]` (no quality).
pub const SERIES_FILE_TEMPLATE: &str = skadi_naming::defaults::SERIES_FILE;
/// Anime file: absolute number instead of SxxEyy.
pub const ANIME_FILE_TEMPLATE: &str = skadi_naming::defaults::SERIES_ANIME_FILE;
/// Daily file: air date instead of SxxEyy.
pub const DAILY_FILE_TEMPLATE: &str = skadi_naming::defaults::SERIES_DAILY_FILE;
/// Whitespace → `_` (the operator-chosen "no spaces" layout, as for movies).
pub const SERIES_SPACE: char = skadi_naming::defaults::SPACE;

/// The episode-specific inputs for one filename (built by the matcher/importer
/// from the [`crate::Episode`] + the parsed release).
#[derive(Clone, Debug, Default)]
pub struct EpisodeNaming<'a> {
    pub series_title: &'a str,
    pub year: Option<u16>,
    pub tmdb: Option<u64>,
    pub season: u16,
    /// Episode number(s) for the `SxxEyy` token (multi-aware); empty for a pure
    /// anime/daily file.
    pub episodes: &'a [u16],
    /// Absolute number for anime, when `series_type` is `Anime`.
    pub absolute: Option<u32>,
    /// Air date `YYYY-MM-DD` for daily series.
    pub air_date: Option<&'a str>,
    pub episode_title: Option<&'a str>,
    /// Quality label (e.g. `"1080p"`); bracketed in the name when present.
    pub quality: Option<&'a str>,
    pub series_type: SeriesType,
}

/// Resolved TV naming templates: operator config overrides, else the built-ins
/// that reproduce the default layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesNaming {
    pub folder: String,
    pub standard_file: String,
    pub anime_file: String,
    pub daily_file: String,
    pub space: char,
}

impl Default for SeriesNaming {
    fn default() -> Self {
        Self {
            folder: SERIES_FOLDER_TEMPLATE.to_string(),
            standard_file: SERIES_FILE_TEMPLATE.to_string(),
            anime_file: ANIME_FILE_TEMPLATE.to_string(),
            daily_file: DAILY_FILE_TEMPLATE.to_string(),
            space: SERIES_SPACE,
        }
    }
}

impl SeriesNaming {
    /// Read `naming.series_*` / `naming.space` from config; empty/absent falls back
    /// to the built-in default.
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
            .unwrap_or(SERIES_SPACE);
        Self {
            folder: tpl("naming.series_folder", SERIES_FOLDER_TEMPLATE),
            standard_file: tpl("naming.series_file", SERIES_FILE_TEMPLATE),
            anime_file: tpl("naming.series_anime_file", ANIME_FILE_TEMPLATE),
            daily_file: tpl("naming.series_daily_file", DAILY_FILE_TEMPLATE),
            space,
        }
    }

    /// Build the canonical episode path with these templates.
    #[must_use]
    pub fn path(&self, root: &Path, ep: &EpisodeNaming, source: &Path) -> PathBuf {
        let file_template = match ep.series_type {
            SeriesType::Anime if ep.absolute.is_some() => &self.anime_file,
            SeriesType::Daily if ep.air_date.is_some() => &self.daily_file,
            _ => &self.standard_file,
        };

        let mut tokens: HashMap<&str, String> = HashMap::new();
        tokens.insert("SeriesTitle", ep.series_title.to_string());
        // Kebab-case variant for the operator's default scheme (SKADI-T-0300).
        tokens.insert("SeriesTitleKebab", skadi_naming::kebab(ep.series_title));
        tokens.insert("Year", ep.year.map(|y| y.to_string()).unwrap_or_default());
        tokens.insert(
            "TmdbTag",
            ep.tmdb
                .map(|id| format!("{{tmdb-{id}}}"))
                .unwrap_or_default(),
        );
        tokens.insert("SeasonFolder", season_folder(ep.season));
        tokens.insert("Episode", episode_token(ep.season, ep.episodes));
        tokens.insert(
            "Absolute",
            ep.absolute.map(|a| format!("{a:03}")).unwrap_or_default(),
        );
        tokens.insert("AirDate", ep.air_date.unwrap_or_default().to_string());
        // Self-punctuating optional parts so a missing one leaves no dangling
        // separator (the engine collapses/trims runs, but ` - ` would survive).
        tokens.insert(
            "EpisodeTitlePart",
            ep.episode_title
                .filter(|t| !t.is_empty())
                .map(|t| format!(" - {}", skadi_naming::kebab(t)))
                .unwrap_or_default(),
        );
        tokens.insert(
            "QualityPart",
            ep.quality
                .filter(|q| !q.is_empty())
                .map(|q| format!(" [{q}]"))
                .unwrap_or_default(),
        );

        let ext = source
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();

        render_library_path(root, &self.folder, file_template, &tokens, &ext, self.space)
    }
}

/// The season subfolder name: `Specials` for season 0, else `Season NN`.
#[must_use]
pub fn season_folder(season: u16) -> String {
    if season == 0 {
        "Specials".to_string()
    } else {
        format!("Season {season:02}")
    }
}

/// The `SxxEyy` token: single (`S01E05`), a contiguous range (`S01E05-E07`), or a
/// concatenated list (`S01E05E08`). Empty episode list yields just the season.
#[must_use]
pub fn episode_token(season: u16, episodes: &[u16]) -> String {
    match episodes {
        [] => format!("S{season:02}"),
        [e] => format!("S{season:02}E{e:02}"),
        eps => {
            let (min, max) = (*eps.iter().min().unwrap(), *eps.iter().max().unwrap());
            if usize::from(max - min) == eps.len() - 1 {
                format!("S{season:02}E{min:02}-E{max:02}")
            } else {
                let mut s = format!("S{season:02}");
                for e in eps {
                    s.push_str(&format!("E{e:02}"));
                }
                s
            }
        }
    }
}

/// [`SeriesNaming::path`] with the built-in default templates.
#[must_use]
pub fn canonical_episode_path(root: &Path, ep: &EpisodeNaming, source: &Path) -> PathBuf {
    SeriesNaming::default().path(root, ep, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep<'a>() -> EpisodeNaming<'a> {
        EpisodeNaming {
            series_title: "The Wire",
            year: Some(2002),
            tmdb: Some(1438),
            season: 3,
            episodes: &[],
            absolute: None,
            air_date: None,
            episode_title: None,
            quality: None,
            series_type: SeriesType::Standard,
        }
    }

    #[test]
    fn standard_single_episode() {
        let e = EpisodeNaming {
            episodes: &[5],
            episode_title: Some("Straight and True"),
            quality: Some("1080p"),
            ..ep()
        };
        let p = canonical_episode_path(Path::new("/tv"), &e, Path::new("/dl/x.mkv"));
        assert_eq!(
            p,
            PathBuf::from(
                "/tv/the-wire_(2002)_{tmdb-1438}/Season_03/the-wire_-_S03E05_-_straight-and-true.mkv"
            )
        );
    }

    #[test]
    fn missing_title_and_quality_leave_no_dangling_separators() {
        let e = EpisodeNaming {
            episodes: &[5],
            ..ep()
        };
        let p = canonical_episode_path(Path::new("/tv"), &e, Path::new("/dl/x.mkv"));
        assert_eq!(
            p,
            PathBuf::from("/tv/the-wire_(2002)_{tmdb-1438}/Season_03/the-wire_-_S03E05.mkv")
        );
    }

    #[test]
    fn multi_episode_contiguous_range() {
        let e = EpisodeNaming {
            episodes: &[5, 6, 7],
            ..ep()
        };
        let p = canonical_episode_path(Path::new("/tv"), &e, Path::new("/dl/x.mkv"));
        assert!(
            p.to_string_lossy().contains("S03E05-E07"),
            "got {}",
            p.display()
        );
    }

    #[test]
    fn multi_episode_non_contiguous_concatenates() {
        assert_eq!(episode_token(3, &[5, 8]), "S03E05E08");
    }

    #[test]
    fn specials_go_in_specials_folder() {
        let e = EpisodeNaming {
            season: 0,
            episodes: &[1],
            ..ep()
        };
        let p = canonical_episode_path(Path::new("/tv"), &e, Path::new("/dl/x.mkv"));
        assert!(
            p.to_string_lossy().contains("/Specials/"),
            "got {}",
            p.display()
        );
    }

    #[test]
    fn anime_uses_absolute_number() {
        let e = EpisodeNaming {
            series_title: "Frieren",
            year: None,
            tmdb: Some(209867),
            season: 1,
            episodes: &[],
            absolute: Some(28),
            episode_title: None,
            quality: Some("1080p"),
            series_type: SeriesType::Anime,
            ..ep()
        };
        let p = canonical_episode_path(Path::new("/tv"), &e, Path::new("/dl/x.mkv"));
        assert_eq!(
            p,
            PathBuf::from("/tv/frieren_{tmdb-209867}/Season_01/frieren_-_028.mkv")
        );
    }

    #[test]
    fn daily_uses_air_date() {
        let e = EpisodeNaming {
            series_title: "The Daily Show",
            year: None,
            tmdb: Some(2224),
            season: 2024,
            episodes: &[],
            air_date: Some("2024-03-01"),
            series_type: SeriesType::Daily,
            ..ep()
        };
        let p = canonical_episode_path(Path::new("/tv"), &e, Path::new("/dl/x.mkv"));
        assert!(
            p.to_string_lossy().contains("the-daily-show_-_2024-03-01"),
            "got {}",
            p.display()
        );
    }

    #[test]
    fn from_view_overrides_then_defaults() {
        let def = SeriesNaming::from_view(&skadi_config::ConfigView::default());
        assert_eq!(def, SeriesNaming::default());

        let view = skadi_config::ConfigView::from_pairs([(
            "naming.series_file".to_string(),
            "{SeriesTitle} {Episode}".to_string(),
        )]);
        let n = SeriesNaming::from_view(&view);
        assert_eq!(n.standard_file, "{SeriesTitle} {Episode}");
        assert_eq!(n.folder, SERIES_FOLDER_TEMPLATE, "unset → default");
    }
}
