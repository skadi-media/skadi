//! `MovieMatcher` — `AcquirableMatcher` for the movies domain (SKADI-T-0046).
//!
//! Constructed **per acquire run** with a snapshot of the run's `Movie` + its
//! `MovieEdition` rows + the registered `EditionKind`s. `match_file` then has
//! everything in memory — no async, no DB, no global state — and can be
//! exhaustively unit-tested with hand-built inputs.

use std::path::{Path, PathBuf};

use regex::RegexBuilder;
use skadi_importer::{AcquirableMatch, AcquirableMatcher, CompletedDownload};
use skadi_quality::ParsedRelease;

/// Minimum title coverage for a file to be accepted as this movie
/// (SKADI-T-0445). Mirrors the hunter's decide-side threshold so a release the
/// hunter would reject is not placed by the importer either.
const MIN_TITLE_COVERAGE: f32 = 0.6;

use crate::THEATRICAL_KIND_ID;
use crate::edition::{EditionKind, MovieEdition};
use crate::movie::Movie;

/// Files smaller than this are treated as samples. 50 MB rules out the typical
/// 30–40 MB "sample" cuts that *arr release groups bundle without rejecting
/// short standalone shorts (those are rare anyway and only mismatch on
/// theatrical-grade content).
const SAMPLE_SIZE_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024;

/// Domain-supplied matcher built per acquire run.
pub struct MovieMatcher {
    movie: Movie,
    editions: Vec<MovieEdition>,
    kinds: Vec<EditionKind>,
    naming: crate::naming::MovieNaming,
}

impl MovieMatcher {
    /// Build a matcher from a per-run snapshot, using the **default** naming templates.
    /// The caller (typically the hunter's import task) loads the snapshot from the repo.
    #[must_use]
    pub fn new(movie: Movie, editions: Vec<MovieEdition>, kinds: Vec<EditionKind>) -> Self {
        Self::with_naming(
            movie,
            editions,
            kinds,
            crate::naming::MovieNaming::default(),
        )
    }

    /// Build a matcher with explicit (operator-configured) naming templates
    /// (SKADI-T-0227).
    #[must_use]
    pub fn with_naming(
        movie: Movie,
        editions: Vec<MovieEdition>,
        kinds: Vec<EditionKind>,
        naming: crate::naming::MovieNaming,
    ) -> Self {
        Self {
            movie,
            editions,
            kinds,
            naming,
        }
    }

    /// Is this parsed release plausibly *this* movie (SKADI-T-0445)?
    ///
    /// Two independent signals, either of which is enough: the release carries
    /// the movie's own external id (a Radarr-style id match, the strongest
    /// evidence), or its title covers the movie's title well enough and its year,
    /// when both are known, agrees. A release with no parseable title at all is
    /// accepted — the importer only reaches the matcher for a download this movie
    /// requested, and refusing unparseable names would strand manual imports.
    fn is_this_movie(&self, parsed: &ParsedRelease) -> bool {
        let ids = &self.movie.external_ids;
        if let Some(tmdb) = ids.tmdb.as_ref()
            && parsed.title.as_deref().is_some_and(|t| {
                t.contains(&format!("tmdb-{}", tmdb.0)) || t.contains(&format!("tmdbid-{}", tmdb.0))
            })
        {
            return true;
        }
        let Some(release_title) = parsed.title.as_deref() else {
            return true;
        };
        let relevance =
            skadi_quality::title_relevance(std::slice::from_ref(&self.movie.title), release_title);
        if relevance.coverage < MIN_TITLE_COVERAGE {
            return false;
        }
        // A year on both sides must agree: remakes share a title (SKADI-T-0387
        // gates this on the decide side; this is the same rule at placement).
        match (self.movie.year, parsed.year) {
            (Some(want), Some(got)) => want == got,
            _ => true,
        }
    }

    fn resolve_edition_kind(&self, parsed: &ParsedRelease) -> &EditionKind {
        let haystack = parsed.edition.as_deref().unwrap_or("");
        if !haystack.is_empty() {
            for kind in &self.kinds {
                if matches_any(haystack, &kind.match_patterns) {
                    return kind;
                }
            }
        }
        // Fall back to Theatrical by deterministic id; if that row was somehow
        // removed (it can't be — builtin), use the first kind we have.
        self.kinds
            .iter()
            .find(|k| k.id.into_uuid() == THEATRICAL_KIND_ID)
            .or_else(|| self.kinds.first())
            .expect("at least one EditionKind in the registry")
    }

    fn destination_path(&self, source: &Path, kind: &EditionKind) -> PathBuf {
        let edition_tag =
            (kind.id.into_uuid() != THEATRICAL_KIND_ID).then_some(kind.normalized_tag.as_str());
        self.naming.path(
            &self.movie.root_folder.path,
            &self.movie.title,
            self.movie.year,
            self.movie.external_ids.tmdb.as_ref().map(|t| t.0),
            self.movie.external_ids.imdb.as_ref().map(|i| i.0.as_str()),
            edition_tag,
            source,
        )
    }
}

impl AcquirableMatcher for MovieMatcher {
    fn match_file(
        &self,
        parsed: &ParsedRelease,
        source: &Path,
        _completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        if is_sample(source) {
            return vec![];
        }
        // Only a video container can be a film (SKADI-T-0591): artwork, subtitles
        // and sidecars in a release folder are never the edition's file.
        if !skadi_importer::is_video_file(source) {
            return vec![];
        }
        // The file must actually be this movie (SKADI-T-0445). The matcher is the
        // last gate before bytes land in the library, and it used to place any
        // file the importer handed it — a mis-grabbed `Blade.Runner.1982…` became
        // The Matrix's Theatrical edition. Radarr's equivalent refuses with
        // "unable to match to movie" and leaves the file for manual import.
        if !self.is_this_movie(parsed) {
            return vec![];
        }
        let kind = self.resolve_edition_kind(parsed);
        // Find the MovieEdition row for (this movie, this kind). If the user
        // hasn't enabled that edition for this movie, skip — the matcher
        // doesn't create new edition rows on the fly.
        let Some(edition) = self.editions.iter().find(|e| e.kind == kind.id) else {
            return vec![];
        };
        let dest = self.destination_path(source, kind);
        // Generate a Kodi/Jellyfin `.nfo` next to the placed video (SKADI-T-0298).
        let nfo = (
            crate::nfo::nfo_path_for(&dest),
            crate::nfo::movie_nfo_xml(&self.movie),
        );
        let mut m = AcquirableMatch::new(edition.acquirable_ref(), dest).with_sidecars(vec![nfo]);
        // Upgrade (SKADI-T-0218): the edition already holds an imported file — the
        // hunter only re-grabs when it's an upgrade, so supersede the old file
        // (possibly at a different path/quality tag) rather than orphaning it.
        if let Some(existing) = edition.file.as_ref() {
            m = m.superseding(vec![existing.path.clone()]);
        }
        vec![m]
    }
}

// --- helpers ---

/// `true` if `source` looks like a *arr-style sample bundle.
fn is_sample(source: &Path) -> bool {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.contains("sample") {
        return true;
    }
    if source.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("sample"))
    }) {
        return true;
    }
    if let Ok(meta) = std::fs::metadata(source)
        && meta.is_file()
        && meta.len() < SAMPLE_SIZE_THRESHOLD_BYTES
    {
        return true;
    }
    false
}

/// Does any pattern (case-insensitive substring, or regex if it parses)
/// match `haystack`?
fn matches_any(haystack: &str, patterns: &[String]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    for p in patterns {
        if p.is_empty() {
            continue;
        }
        // Plain substring path: cheap and the common case.
        if lower.contains(&p.to_ascii_lowercase()) {
            return true;
        }
        // Regex path: bounded automata + size limit guard against ReDoS even
        // though user-controlled patterns are the only consumers.
        if looks_like_regex(p)
            && let Ok(re) = RegexBuilder::new(p)
                .case_insensitive(true)
                .size_limit(64 * 1024)
                .dfa_size_limit(64 * 1024)
                .build()
            && re.is_match(haystack)
        {
            return true;
        }
    }
    false
}

fn looks_like_regex(s: &str) -> bool {
    // Any of the common metacharacters → treat as regex.
    s.chars().any(|c| {
        matches!(
            c,
            '\\' | '[' | ']' | '(' | ')' | '|' | '+' | '*' | '?' | '^' | '$' | '{' | '}'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::{EditionKindId, ExternalIds, MovieId, ProfileId, RootFolder, TmdbId};
    use skadi_quality::parse;

    fn theatrical_kind() -> EditionKind {
        EditionKind {
            id: EditionKindId::from(THEATRICAL_KIND_ID),
            name: "Theatrical".into(),
            normalized_tag: "Theatrical".into(),
            match_patterns: vec!["theatrical".into()],
            builtin: true,
        }
    }
    fn extended_kind() -> EditionKind {
        EditionKind {
            id: EditionKindId::from(uuid::uuid!("00000000-0000-0000-0000-000000000002")),
            name: "Extended".into(),
            normalized_tag: "Extended".into(),
            match_patterns: vec!["extended".into(), "extended cut".into()],
            builtin: true,
        }
    }
    fn user_kind() -> EditionKind {
        EditionKind {
            id: EditionKindId::new(),
            name: "Star Wars 4K Digital Film Scan".into(),
            normalized_tag: "SW 4K Scan".into(),
            // Regex pattern (whitespace + word-anchored); tests the regex path.
            // The parser joins edition tokens with spaces, so we need `\s+`.
            match_patterns: vec![r"4k\s+digital\s+film\s+scan".into()],
            builtin: false,
        }
    }

    fn matrix_movie() -> Movie {
        let mut m = Movie::new(
            ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            "The Matrix",
            ProfileId::new(),
            RootFolder::new("/movies"),
        );
        m.id = MovieId::new();
        m.year = Some(1999);
        m
    }

    fn edition_for(movie_id: MovieId, kind_id: EditionKindId) -> MovieEdition {
        MovieEdition::missing(movie_id, kind_id)
    }

    fn make_completed() -> CompletedDownload {
        CompletedDownload {
            handle: skadi_downloaders::DownloadHandle {
                native_id: "h".into(),
                category: "2000".into(),
            },
            files: vec![],
            category: "2000".into(),
        }
    }

    /// Artwork with the film's name is not the film (SKADI-T-0591).
    #[test]
    fn non_video_files_are_rejected() {
        let m = matrix_movie();
        let theatrical = theatrical_kind();
        let editions = vec![edition_for(m.id, theatrical.id)];
        let matcher = MovieMatcher::new(m, editions, vec![theatrical]);
        let parsed = parse("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv");
        for name in [
            "poster.jpg",
            "The.Matrix.1999.1080p.BluRay.x264-GRP.srt",
            "fanart.png",
        ] {
            let source = PathBuf::from(format!("/dl/The.Matrix.1999.1080p.BluRay.x264-GRP/{name}"));
            assert!(
                matcher
                    .match_file(&parsed, &source, &make_completed())
                    .is_empty(),
                "{name} must not be placed as the film"
            );
        }
    }

    #[test]
    fn samples_are_rejected_by_filename() {
        let m = matrix_movie();
        let theatrical = theatrical_kind();
        let editions = vec![edition_for(m.id, theatrical.id)];
        let matcher = MovieMatcher::new(m, editions, vec![theatrical]);

        let parsed = parse("Movie.2020.1080p.BluRay.x264-GRP.mkv");
        let source = Path::new("/dl/Movie.2020.1080p.BluRay.x264-GRP/Sample-Movie.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());
        assert!(out.is_empty(), "filename-based sample rejected");
    }

    #[test]
    fn samples_are_rejected_by_sample_directory() {
        let m = matrix_movie();
        let theatrical = theatrical_kind();
        let editions = vec![edition_for(m.id, theatrical.id)];
        let matcher = MovieMatcher::new(m, editions, vec![theatrical]);

        let parsed = parse("Movie.2020.1080p.BluRay.x264-GRP.mkv");
        let source = Path::new("/dl/Movie.2020.1080p.BluRay.x264-GRP/Sample/main.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());
        assert!(out.is_empty(), "Sample/ directory rejected");
    }

    #[test]
    fn theatrical_with_no_edition_tag_emits_match_without_edition_suffix() {
        let m = matrix_movie();
        let theatrical = theatrical_kind();
        let edition = edition_for(m.id, theatrical.id);
        let edition_ref = edition.acquirable_ref();
        let matcher = MovieMatcher::new(m, vec![edition], vec![theatrical]);

        let parsed = parse("The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv");
        let source = Path::new("/dl/The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());

        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.acquirable, edition_ref);
        assert_eq!(
            m.dest,
            PathBuf::from("/movies/the-matrix_(1999)_{tmdb-603}/theatrical/the-matrix_(1999).mkv")
        );
    }

    #[test]
    fn existing_edition_file_is_superseded_on_upgrade() {
        let m = matrix_movie();
        let theatrical = theatrical_kind();
        let mut edition = edition_for(m.id, theatrical.id);
        // The edition already holds a lower-quality import at a different filename.
        let old = PathBuf::from("/movies/The_Matrix_(1999)_{tmdb-603}/Theatrical/old_720p.mkv");
        edition.file = Some(skadi_core::FileRef { path: old.clone() });
        let matcher = MovieMatcher::new(m, vec![edition], vec![theatrical]);

        let parsed = parse("The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv");
        let source = Path::new("/dl/The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());

        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].supersedes,
            vec![old],
            "old import marked for cleanup"
        );
        assert_eq!(
            out[0].on_collision,
            skadi_importer::CollisionPolicy::Overwrite,
            "upgrade overwrites if same path"
        );
    }

    #[test]
    fn extended_edition_routes_to_its_own_kebab_subfolder() {
        let movie = matrix_movie();
        let theatrical = theatrical_kind();
        let extended = extended_kind();
        let theatrical_e = edition_for(movie.id, theatrical.id);
        let extended_e = edition_for(movie.id, extended.id);
        let extended_ref = extended_e.acquirable_ref();
        let matcher = MovieMatcher::new(
            movie,
            vec![theatrical_e, extended_e],
            vec![theatrical, extended],
        );

        let parsed = parse("The.Matrix.1999.Extended.1080p.BluRay.x264-EXT.mkv");
        let source = Path::new("/dl/The.Matrix.1999.Extended.1080p.BluRay.x264-EXT.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].acquirable, extended_ref);
        assert_eq!(
            out[0].dest,
            PathBuf::from("/movies/the-matrix_(1999)_{tmdb-603}/extended/the-matrix_(1999).mkv")
        );
    }

    #[test]
    fn user_added_regex_kind_matches_when_pattern_appears_in_parsed_edition() {
        let movie = matrix_movie();
        let theatrical = theatrical_kind();
        let user = user_kind();
        let theatrical_e = edition_for(movie.id, theatrical.id);
        let user_e = edition_for(movie.id, user.id);
        let user_ref = user_e.acquirable_ref();
        let matcher = MovieMatcher::new(movie, vec![theatrical_e, user_e], vec![user, theatrical]);

        let parsed = ParsedRelease {
            edition: Some("4K Digital Film Scan".into()),
            ..Default::default()
        };
        let source = Path::new("/dl/SomeMovie/SomeMovie.4K.Digital.Film.Scan.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].acquirable, user_ref);
        let dest = out[0].dest.to_string_lossy();
        assert!(
            dest.contains("/sw-4k-scan/") && dest.ends_with("the-matrix_(1999).mkv"),
            "got {dest}"
        );
    }

    #[test]
    fn unknown_edition_with_no_matching_row_emits_no_match() {
        let movie = matrix_movie();
        let theatrical = theatrical_kind();
        // Movie has ONLY a Theatrical edition row; the file is Extended.
        let theatrical_e = edition_for(movie.id, theatrical.id);
        let extended = extended_kind();
        let matcher = MovieMatcher::new(movie, vec![theatrical_e], vec![theatrical, extended]);

        let parsed = parse("The.Matrix.1999.Extended.1080p.BluRay.x264-EXT.mkv");
        let source = Path::new("/dl/The.Matrix.1999.Extended.1080p.BluRay.x264-EXT.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());
        assert!(
            out.is_empty(),
            "kind resolves to Extended but no MovieEdition for it on this movie"
        );
    }

    #[test]
    fn sanitization_strips_filesystem_unsafe_chars() {
        let mut movie = matrix_movie();
        movie.title = "Title: with / unsafe \\ chars?".into();
        let theatrical = theatrical_kind();
        let e = edition_for(movie.id, theatrical.id);
        let matcher = MovieMatcher::new(movie, vec![e], vec![theatrical]);

        let parsed = parse("title.with.unsafe.chars.1999.1080p.BluRay.x264-GRP.mkv");
        let source = Path::new("/dl/x.mkv");
        let out = matcher.match_file(&parsed, source, &make_completed());
        assert_eq!(out.len(), 1);
        let dest = out[0].dest.to_string_lossy().to_string();
        assert!(
            !dest.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'][1])
                && !dest.contains([':', '*', '?', '"', '<', '>', '|'][0]),
            "no unsafe chars in dest path; got {dest}"
        );
    }
}
