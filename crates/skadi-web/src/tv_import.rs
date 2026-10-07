//! TV library import view: point Skadi at an on-disk TV library path, then review
//! the proposed TVDB matches in a **hierarchical Show → Season → Episode** tree
//! that mirrors the series detail page. Each scanned file is shown against the
//! episode it maps to; files that didn't auto-map get a manual Season/Episode
//! picker. Import places the chosen files **in place** (no move/rename — see
//! [`crate::api`]).
//!
//! Scanning is parse-only and instant; metadata matching is **lazy and
//! paginated** — the page resolves series matches only for the candidates
//! currently in view (`POST /tv/library-import/match`) and then fetches each
//! matched show's full structure (`GET …/series-structure`), so a big library
//! never triggers thousands of lookups up front. Commit runs in client-side
//! batches with a live progress count.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;
use crate::path_picker::PathPicker;

/// How many candidates per page (also the per-page match batch size).
const PAGE_SIZE: usize = 50;

/// Shared confidence badge vocabulary (SKADI-T-0328) — re-exported so existing
/// imports/tests keep working.
pub use crate::import_common::{confidence_class, confidence_title};

/// Format a `SxxEyy` (or `SxxEyy-Ezz` for multi-episode) label from the scanned
/// season/episode numbers, falling back to absolute numbering (`#123`) when a
/// candidate has no season — and `—` when neither is known.
pub fn episode_label(season: Option<u16>, episodes: &[u16], absolute: &[u16]) -> String {
    if let Some(s) = season {
        if !episodes.is_empty() {
            let eps: Vec<String> = episodes.iter().map(|e| format!("E{e:02}")).collect();
            return format!("S{s:02}{}", eps.join("-"));
        }
        return format!("S{s:02}");
    }
    if !absolute.is_empty() {
        let abs: Vec<String> = absolute.iter().map(|a| a.to_string()).collect();
        return format!("#{}", abs.join(", "));
    }
    "—".to_string()
}

/// The `SxxEyy` code for a structure episode (`Specials` are season 0).
pub fn season_episode_code(season: u16, episode: u16) -> String {
    format!("S{season:02}E{episode:02}")
}

/// A season's display label (`Specials` for season 0, else `Season N`).
pub fn season_label(number: u16) -> String {
    if number == 0 {
        "Specials".to_string()
    } else {
        format!("Season {number}")
    }
}

/// Label for an episode `<option>` in the manual picker: `E02 · Title`, or just
/// `E02` when the episode has no title.
pub fn episode_option_label(number: u16, title: Option<&str>) -> String {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => format!("E{number:02} · {t}"),
        None => format!("E{number:02}"),
    }
}

/// Every episode a scanned candidate **auto-maps** to: its parsed season crossed
/// with each parsed episode number — filtered to episodes that actually exist in
/// the fetched structure. Before the structure has loaded (`None`) we trust the
/// parse so a file isn't spuriously flagged as unmapped mid-fetch. A multi-episode
/// file (`S01E01E02`) maps to BOTH episodes — the backend places both, and the UI
/// must agree (SKADI-T-0325).
pub fn auto_episodes(
    cand: &api::TvScanCandidate,
    structure: Option<&api::TvSeriesStructure>,
) -> Vec<(u16, u16)> {
    let Some(season) = cand.season else {
        return Vec::new();
    };
    let eps: Vec<u16> = match structure {
        Some(s) if !s.episodes.is_empty() => cand
            .episodes
            .iter()
            .copied()
            .filter(|e| {
                s.episodes
                    .iter()
                    .any(|x| x.season == season && x.number == *e)
            })
            .collect(),
        _ => cand.episodes.clone(),
    };
    eps.into_iter().map(|e| (season, e)).collect()
}

/// Back-compat single-episode view of [`auto_episodes`] (first mapping).
pub fn auto_episode(
    cand: &api::TvScanCandidate,
    structure: Option<&api::TvSeriesStructure>,
) -> Option<(u16, u16)> {
    auto_episodes(cand, structure).first().copied()
}

/// The episodes a candidate maps to: a complete manual override wins, otherwise
/// the auto-mapped parse (possibly several for a multi-episode file).
pub fn candidate_mappings(
    cand: &api::TvScanCandidate,
    ov: &HashMap<String, (Option<u16>, Option<u16>)>,
    structure: Option<&api::TvSeriesStructure>,
) -> Vec<(u16, u16)> {
    if let Some((Some(s), Some(e))) = ov.get(&cand.path).copied() {
        return vec![(s, e)];
    }
    auto_episodes(cand, structure)
}

/// Is a scanned file bonus content rather than an episode? True only when
/// nothing identifies a specific **episode** (no SxxEyy episode numbers, no
/// absolute number, no air date — such files are never extras, even inside a
/// "Season 1 + Extras" folder) AND either its filename or a sub-folder between
/// the show folder and the file carries an extras marker (Featurettes/, Extras/,
/// Making of…). A bare season token is not identification — "Trailer - Black
/// Sails Season 2" is still a trailer. Extras don't count against a show being
/// "fully captured" — they have no episode to capture (SKADI-T-0329 feedback).
pub fn is_extra(cand: &api::TvScanCandidate) -> bool {
    if !cand.episodes.is_empty() || !cand.absolute.is_empty() || cand.air_date.is_some() {
        return false;
    }
    const MARKERS: &[&str] = &[
        "featurette",
        "extras",
        "extra stuff",
        "behind the scenes",
        "behind.the.scenes",
        "behind_the_scenes",
        "making of",
        "making.of",
        "making_of",
        "bonus",
        "deleted scene",
        "deleted.scene",
        "interview",
        "blooper",
        "outtake",
        "menu art",
        "wrap reel",
        "trailer",
        "sample",
        "recap",
    ];
    let hay = |s: &str| {
        let s = s.to_lowercase();
        MARKERS.iter().any(|m| s.contains(m))
    };
    if hay(&cand.display_name) {
        return true;
    }
    // Path components strictly below the show folder, excluding the file itself
    // (already checked via display_name) — e.g. ".../<show>/Featurettes/x.mkv".
    if let Some(folder) = &cand.folder
        && let Some(idx) = cand.path.find(folder.as_str())
    {
        let below = &cand.path[idx + folder.len()..];
        let mut comps: Vec<&str> = below.split('/').filter(|c| !c.is_empty()).collect();
        comps.pop(); // the file name
        return comps.iter().any(|c| hay(c));
    }
    false
}

/// Collision guard (SKADI-T-0325): candidates claim their episodes **in order**;
/// a candidate whose episode set intersects already-claimed episodes is a "loser"
/// — every part of a classic multi-part serial after the first parses to the SAME
/// `SxxEyy`, and pretending they're all mapped imports one file (possibly onto the
/// wrong episode) and silently strands the rest. Losers are routed to the manual
/// picker instead. Returns the loser paths.
pub fn collision_losers(
    cands: &[api::TvScanCandidate],
    ov: &HashMap<String, (Option<u16>, Option<u16>)>,
    structure: Option<&api::TvSeriesStructure>,
) -> HashSet<String> {
    let mut claimed: HashSet<(u16, u16)> = HashSet::new();
    let mut losers = HashSet::new();
    for c in cands {
        let m = candidate_mappings(c, ov, structure);
        if m.is_empty() {
            continue;
        }
        if m.iter().any(|p| claimed.contains(p)) {
            losers.insert(c.path.clone());
        } else {
            claimed.extend(m);
        }
    }
    losers
}

/// Parse a first-aired year from an `air_date` (`YYYY-MM-DD`), to help the match
/// disambiguate a series title.
fn year_from_air_date(air_date: &Option<String>) -> Option<u16> {
    air_date
        .as_deref()
        .and_then(|d| d.get(0..4))
        .and_then(|y| y.parse::<u16>().ok())
}

/// One reviewable **series**: all the scanned episode files that parsed to the
/// same show title, the single (lazily resolved) TVDB match they share, the
/// fetched season/episode structure, and the operator's edits. Grouping by series
/// is what keeps the review usable — a real library is thousands of episode files
/// but only a few hundred shows (SKADI-I-0047 feedback).
///
/// All the *interactive* state (selection, TVDB id, collapse, manual mappings) is
/// held in [`RwSignal`]s so it survives the list re-rendering when match results
/// or structures stream in — cloning the group shares those signal handles.
#[derive(Clone)]
struct SeriesGroup {
    /// The group key — the show folder under the scan root (or the parsed title for
    /// loose files in the root, else `—`).
    key: String,
    /// Clean, human/match title derived from the folder name (id tags + year
    /// stripped, separators normalized), e.g. `12-monkeys_(2015)_{tmdb-60948}` →
    /// `12 monkeys`. Used for display and the metadata search.
    title: String,
    /// First-aired year (the folder's `(YYYY)`, else an episode air date), to
    /// disambiguate the match.
    year: Option<u16>,
    /// Precomputed "Drama/Fantasy · Ended · HBO · ★8.3" facts line from the NFO,
    /// shown on the card so an operator can confirm the match at a glance.
    facts: Option<String>,
    /// One-line synopsis from the NFO `<plot>`.
    overview: Option<String>,
    /// Every scanned episode file under this series.
    candidates: Vec<api::TvScanCandidate>,
    /// Whether a metadata match has been attempted yet.
    matched: bool,
    proposed: Option<api::TvProposedMatch>,
    /// `high` / `low` / `none`, or empty before matching.
    confidence: String,
    already_in_library: bool,
    /// The show's full season/episode tree, fetched after the match resolves.
    structure: Option<api::TvSeriesStructure>,
    /// Whether a structure fetch has been kicked off (avoids re-fetching).
    structure_fetched: bool,
    /// Why the last structure fetch failed, if it did — rendered in the card body
    /// (instead of an eternal "Loading…") and cleared on retry. A failed fetch
    /// resets `structure_fetched` so collapsing + re-expanding retries.
    structure_error: Option<String>,
    /// Ticked to import.
    selected: RwSignal<bool>,
    /// Editable TVDB id; prefilled from the proposed match once resolved.
    tvdb: RwSignal<String>,
    /// Show card collapsed (the top-level Show toggle).
    collapsed: RwSignal<bool>,
    /// Which season numbers are expanded (default: all collapsed).
    expanded_seasons: RwSignal<HashSet<u16>>,
    /// Manual `(season, episode)` overrides for unmapped files, keyed by path.
    overrides: RwSignal<HashMap<String, (Option<u16>, Option<u16>)>>,
    /// Series-picker state: whether the search box is open, its query (seeded from
    /// the title), an in-flight flag, and the last results. Lets the operator fix a
    /// wrong/empty match by searching a title instead of typing a raw TVDB id
    /// (SKADI-T-0324 / T-0319).
    picker_open: RwSignal<bool>,
    search_q: RwSignal<String>,
    searching: RwSignal<bool>,
    search_results: RwSignal<Vec<api::SeriesSearchResult>>,
}

impl SeriesGroup {
    /// The representative path used as the match key (the backend keys match
    /// results by path; we send one query per group with its first file's path).
    fn key_path(&self) -> Option<&str> {
        self.candidates.first().map(|c| c.path.as_str())
    }

    /// The episodes a candidate maps to (override wins; multi-episode aware).
    fn mappings_of(
        &self,
        cand: &api::TvScanCandidate,
        ov: &HashMap<String, (Option<u16>, Option<u16>)>,
    ) -> Vec<(u16, u16)> {
        candidate_mappings(cand, ov, self.structure.as_ref())
    }

    /// Collision losers for this show under a given override set.
    fn losers(&self, ov: &HashMap<String, (Option<u16>, Option<u16>)>) -> HashSet<String> {
        collision_losers(&self.candidates, ov, self.structure.as_ref())
    }

    /// Paths of this show's extras (bonus files) — unless the operator manually
    /// mapped one to an episode, which overrides the classification.
    fn extra_paths(&self, ov: &HashMap<String, (Option<u16>, Option<u16>)>) -> HashSet<String> {
        self.candidates
            .iter()
            .filter(|c| is_extra(c) && !matches!(ov.get(&c.path), Some((Some(_), Some(_)))))
            .map(|c| c.path.clone())
            .collect()
    }

    /// `(mapped_files, importable_total, extras)` for this show under a given
    /// override set — collision losers do NOT count as mapped (they need a manual
    /// pick), and extras are counted separately: bonus content has no episode to
    /// capture, so it never drags down "fully captured" (SKADI-T-0329).
    fn file_counts(
        &self,
        ov: &HashMap<String, (Option<u16>, Option<u16>)>,
    ) -> (usize, usize, usize) {
        let losers = self.losers(ov);
        let extras = self.extra_paths(ov);
        let mapped = self
            .candidates
            .iter()
            .filter(|c| {
                !extras.contains(&c.path)
                    && !losers.contains(&c.path)
                    && !self.mappings_of(c, ov).is_empty()
            })
            .count();
        (mapped, self.candidates.len() - extras.len(), extras.len())
    }

    /// How many distinct episodes the winners map — what "Import" will actually
    /// record (a multi-episode file counts each of its episodes).
    fn episode_count(&self, ov: &HashMap<String, (Option<u16>, Option<u16>)>) -> usize {
        let losers = self.losers(ov);
        let mut claimed: HashSet<(u16, u16)> = HashSet::new();
        for c in &self.candidates {
            if !losers.contains(&c.path) {
                claimed.extend(self.mappings_of(c, ov));
            }
        }
        claimed.len()
    }
}

/// A compact "S1–S3" / "S1, S3" season summary over a group's episode files.
pub fn season_summary(cands: &[api::TvScanCandidate]) -> String {
    let mut seasons: Vec<u16> = cands.iter().filter_map(|c| c.season).collect();
    seasons.sort_unstable();
    seasons.dedup();
    let Some(&min) = seasons.first() else {
        return String::new();
    };
    let max = *seasons.last().unwrap();
    let label = |n: u16| {
        if n == 0 {
            "Specials".to_string()
        } else {
            format!("S{n}")
        }
    };
    if seasons.len() == 1 {
        label(min)
    } else if seasons.len() as u16 == max - min + 1 {
        format!("{}–{}", label(min), label(max))
    } else {
        seasons
            .iter()
            .map(|&n| label(n))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Drop `{…}` bracketed id tags (e.g. `{tmdb-60948}`) from a folder name.
fn strip_id_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for ch in s.chars() {
        match ch {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Does a paren-group's content look like quality/release junk rather than part
/// of the title? (`360p re-dvdrip`, `1080p WEB-DL`…) — such groups are dropped
/// from the search title (SKADI-T-0325).
fn is_quality_junk(inside: &str) -> bool {
    const TOKENS: &[&str] = &[
        "dvdrip", "webdl", "web-dl", "webrip", "bluray", "brrip", "bdrip", "hdtv", "x264", "x265",
        "hevc", "remux", "dvd", "vhs", "upscale",
    ];
    inside.split_whitespace().any(|w| {
        let w = w.to_lowercase();
        // A resolution token: 3-4 digits + 'p' (480p / 1080p / 2160p).
        let res = w
            .strip_suffix('p')
            .is_some_and(|d| (3..=4).contains(&d.len()) && d.chars().all(|c| c.is_ascii_digit()));
        res || TOKENS.iter().any(|t| w.contains(t))
    })
}

/// Derive a clean series title + year from a show-folder name. Strips `{…}`/`[…]`
/// id tags, pulls a parenthesised `(YYYY)` year, drops quality-junk paren groups
/// (`(360p re-dvdrip)`), trailing bare season tokens (`… S00`), and normalizes
/// `_`/`.`/`-` to spaces — so `12-monkeys_(2015)_{tmdb-60948}` →
/// (`12 monkeys`, `Some(2015)`) and `MST3K S00 (360p re-dvdrip)` → (`MST3K`, None).
/// Only a *parenthesised* 4-digit run is treated as the year, so a title that is
/// itself a year (`1899`, `2012`) survives.
pub fn clean_folder_title(folder: &str) -> (String, Option<u16>) {
    let spaced = strip_id_tags(folder).replace(['_', '.'], " ");
    let mut year = None;
    let mut out = String::with_capacity(spaced.len());
    let mut rest = spaced.as_str();
    while let Some(open) = rest.find('(') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if let Some(close) = after.find(')') {
            let inside = after[..close].trim();
            let is_year = inside.len() == 4
                && inside.chars().all(|c| c.is_ascii_digit())
                && inside
                    .parse::<u16>()
                    .ok()
                    .is_some_and(|y| (1900..=2100).contains(&y));
            if is_year {
                year = inside.parse::<u16>().ok();
            } else if !is_quality_junk(inside) {
                out.push('(');
                out.push_str(&after[..close]);
                out.push(')');
            }
            rest = &after[close + 1..];
        } else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    let title = out.replace('-', " ");
    let mut words: Vec<&str> = title.split_whitespace().collect();
    // Drop trailing id/marker tokens that poison the metadata search
    // (SKADI-T-0325/T-0328):
    // - bare season tokens (`S00`, `Season 3`) — the season is carried per-file,
    // - a trailing IMDB id (`tt3032476`) and bare numeric ids (5+ digits) from
    //   `title_year_tvdbid_imdbid`-style folders,
    // - at most ONE trailing bare year (captured for match confidence) — so a
    //   title that ends in a year-like number (`blade runner 2049`) survives.
    let mut year_taken = false;
    while let Some(&last) = words.last() {
        let is_sxx = (last.starts_with('S') || last.starts_with('s'))
            && last.len() >= 2
            && last.len() <= 3
            && last[1..].chars().all(|c| c.is_ascii_digit());
        let is_season_n = last.chars().all(|c| c.is_ascii_digit())
            && last.len() <= 2
            && words.len() >= 2
            && matches!(
                words[words.len() - 2].to_lowercase().as_str(),
                "season" | "series"
            );
        let is_imdb = last.len() > 2
            && last.starts_with("tt")
            && last[2..].chars().all(|c| c.is_ascii_digit());
        let is_bare_id = last.len() >= 5 && last.chars().all(|c| c.is_ascii_digit());
        let is_year = !year_taken
            && last.len() == 4
            && last.chars().all(|c| c.is_ascii_digit())
            && last
                .parse::<u16>()
                .ok()
                .is_some_and(|y| (1900..=2100).contains(&y));
        if is_season_n {
            words.truncate(words.len() - 2);
        } else if (is_sxx || is_imdb || is_bare_id) && words.len() > 1 {
            words.truncate(words.len() - 1);
        } else if is_year && words.len() > 1 {
            year = year.or_else(|| last.parse().ok());
            year_taken = true;
            words.truncate(words.len() - 1);
        } else {
            break;
        }
    }
    (words.join(" "), year)
}

/// Group scanned candidates by their **show folder** (loose files in the root fall
/// back to their parsed title), sorted by key. Grouping by folder — not by the
/// per-file parsed title — is what keeps a show's stray extras under the *right*
/// show instead of each spawning a bogus one-file show (SKADI-T-0324).
fn group_candidates(cands: Vec<api::TvScanCandidate>) -> Vec<SeriesGroup> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, Vec<api::TvScanCandidate>> = BTreeMap::new();
    for c in cands {
        let key = c
            .folder
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| c.series_title.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "—".to_string());
        map.entry(key).or_default().push(c);
    }
    map.into_iter()
        .map(|(key, candidates)| {
            // Authoritative `tvshow.nfo` hint (exact TVDB id + real title/year),
            // when the folder had one — overrides the folder-name guess and lets us
            // resolve the show *without* any metadata search (SKADI-T-0324).
            let nfo_tvdb = candidates.iter().find_map(|c| c.nfo_tvdb_id);
            let nfo_title = candidates
                .iter()
                .find_map(|c| c.nfo_title.clone())
                .filter(|s| !s.is_empty());
            let nfo_year = candidates.iter().find_map(|c| c.nfo_year);

            // Title + year: NFO first, then the folder name, then an episode air date.
            let folder = candidates
                .iter()
                .find_map(|c| c.folder.clone())
                .filter(|s| !s.is_empty());
            let (folder_title, folder_year) = match &folder {
                Some(f) => clean_folder_title(f),
                None => (key.clone(), None),
            };
            let mut title = nfo_title.clone().unwrap_or(folder_title);
            if title.is_empty() {
                title = key.clone();
            }
            let year = nfo_year.or(folder_year).or_else(|| {
                candidates
                    .iter()
                    .find_map(|c| year_from_air_date(&c.air_date))
            });

            // NFO display metadata → a compact facts line + a synopsis snippet, so
            // the operator can confirm the auto-match without leaving the page.
            let genres = candidates
                .iter()
                .find(|c| !c.nfo_genres.is_empty())
                .map(|c| c.nfo_genres.clone())
                .unwrap_or_default();
            let overview = candidates
                .iter()
                .find_map(|c| c.nfo_overview.clone())
                .filter(|s| !s.is_empty());
            let mut facts_parts: Vec<String> = Vec::new();
            if !genres.is_empty() {
                facts_parts.push(genres.into_iter().take(3).collect::<Vec<_>>().join("/"));
            }
            for f in [
                candidates.iter().find_map(|c| c.nfo_status.clone()),
                candidates.iter().find_map(|c| c.nfo_network.clone()),
            ]
            .into_iter()
            .flatten()
            {
                if !f.is_empty() {
                    facts_parts.push(f);
                }
            }
            if let Some(r) = candidates
                .iter()
                .find_map(|c| c.nfo_rating.clone())
                .filter(|s| !s.is_empty())
            {
                facts_parts.push(format!("★{r}"));
            }
            let facts = (!facts_parts.is_empty()).then(|| facts_parts.join(" · "));

            // An NFO id resolves the show up front: pre-match it (no fuzzy search),
            // pre-fill the TVDB id, and pre-tick it when it has episode files —
            // but never pre-tick a group with duplicate-SxxEyy collisions (classic
            // multi-part serials); those need eyes first (SKADI-T-0325).
            let has_episode = candidates
                .iter()
                .any(|c| c.season.is_some() && !c.episodes.is_empty());
            let empty_ov = HashMap::new();
            let has_collisions = !collision_losers(&candidates, &empty_ov, None).is_empty();
            let proposed = nfo_tvdb.map(|id| api::TvProposedMatch {
                tvdb_id: id,
                title: title.clone(),
                year,
                poster_url: None,
            });
            SeriesGroup {
                key,
                title,
                year,
                facts,
                overview,
                candidates,
                matched: nfo_tvdb.is_some(),
                confidence: if nfo_tvdb.is_some() {
                    "nfo".to_string()
                } else {
                    String::new()
                },
                already_in_library: false,
                structure: None,
                structure_fetched: false,
                structure_error: None,
                selected: RwSignal::new(nfo_tvdb.is_some() && has_episode && !has_collisions),
                tvdb: RwSignal::new(nfo_tvdb.map(|id| id.to_string()).unwrap_or_default()),
                proposed,
                collapsed: RwSignal::new(true),
                expanded_seasons: RwSignal::new(HashSet::new()),
                overrides: RwSignal::new(HashMap::new()),
                picker_open: RwSignal::new(false),
                search_q: RwSignal::new(String::new()),
                searching: RwSignal::new(false),
                search_results: RwSignal::new(Vec::new()),
            }
        })
        .collect()
}

#[component]
pub fn TvImportPage() -> impl IntoView {
    let path = RwSignal::new(String::new());

    // Reviewed one card per **series** (grouped from per-episode scan candidates).
    let groups = RwSignal::new(Vec::<SeriesGroup>::new());
    let scanning = RwSignal::new(false);
    let matching = RwSignal::new(false);
    let importing = RwSignal::new(false);
    let scanned = RwSignal::new(false);
    let page = RwSignal::new(0usize);
    let scan_error = RwSignal::new(None::<String>);
    let result = RwSignal::new(None::<Result<api::CommitResult, String>>);
    // Live import progress: (committed_so_far, total) while a batched import runs.
    let progress = RwSignal::new(None::<(usize, usize)>);

    // TVDB ids already in the library, so we can hide/sink shows that are already
    // imported (SKADI-T-0324). Loaded once on mount.
    let lib_tvdbs = RwSignal::new(std::collections::HashSet::<u64>::new());
    let hide_imported = RwSignal::new(true);
    spawn_local(async move {
        if let Ok(series) = api::list_series().await {
            lib_tvdbs.set(series.iter().filter_map(|s| s.external_ids.tvdb).collect());
        }
    });
    // Is this show already in the library? (post-commit flag, or its resolved TVDB
    // id is already present.)
    let is_imported = move |g: &SeriesGroup| {
        g.already_in_library
            || g.tvdb
                .get_untracked()
                .trim()
                .parse::<u64>()
                .ok()
                .is_some_and(|id| lib_tvdbs.get().contains(&id))
    };

    // The shows to display, as **original indices** into `groups` so `show_card`
    // keeps its stable write-back index: already-imported shows hidden (toggle)
    // or sunk to the bottom, so newly-scannable shows lead (SKADI-T-0324). A
    // `Memo` of indices instead of a per-caller clone+sort of the whole vec —
    // at 400 shows × 19k candidates the old closure deep-cloned everything 4-6×
    // per render (SKADI-T-0326 perf). `match_page` MUST slice this same list —
    // slicing the raw vec left tail shows displayed-but-never-matched.
    let visible = Memo::new(move |_| {
        let hide = hide_imported.get();
        let lib = lib_tvdbs.get();
        groups.with(|all| {
            let mut v: Vec<(usize, bool)> = all
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let imported = g.already_in_library
                        || g.tvdb
                            .get_untracked()
                            .trim()
                            .parse::<u64>()
                            .ok()
                            .is_some_and(|id| lib.contains(&id));
                    (i, imported)
                })
                .filter(|(_, imported)| !(hide && *imported))
                .collect();
            v.sort_by_key(|(_, imported)| *imported);
            v.into_iter().map(|(i, _)| i).collect::<Vec<usize>>()
        })
    });

    // A failed /match call — rendered next to the cards with a Retry, NOT under
    // the scan form where nobody connects it to the stuck "matching…" labels
    // (SKADI-T-0326).
    let match_error = RwSignal::new(None::<String>);

    // Background-prefetch the episode structures for page `p`'s matched shows so
    // the mapped counts verify against the real TVDB episode list without the
    // operator expanding 400 cards one by one (SKADI-T-0326). Bounded to one
    // page (≤ PAGE_SIZE lookups); already-fetched/in-library shows are skipped.
    let prefetch_structures = move |p: usize| {
        let targets: Vec<(usize, u64)> = {
            let idxs = visible.get_untracked();
            groups.with_untracked(|all| {
                idxs.iter()
                    .skip(p * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .filter_map(|&i| {
                        let g = all.get(i)?;
                        if g.structure_fetched || g.already_in_library {
                            return None;
                        }
                        g.proposed.as_ref().map(|pm| (i, pm.tvdb_id))
                    })
                    .collect()
            })
        };
        if targets.is_empty() {
            return;
        }
        groups.update(|all| {
            for (i, _) in &targets {
                if let Some(g) = all.get_mut(*i) {
                    g.structure_fetched = true;
                    g.structure_error = None;
                }
            }
        });
        for (i, tvdb) in targets {
            spawn_local(async move {
                let fetched = api::tv_import_series_structure(tvdb).await;
                groups.update(|all| {
                    if let Some(g) = all.get_mut(i) {
                        match fetched {
                            Ok(st) => g.structure = Some(st),
                            Err(e) => {
                                g.structure_error = Some(e.to_string());
                                g.structure_fetched = false;
                            }
                        }
                    }
                });
            });
        }
    };

    // Resolve the TVDB match for the series on page `p` that aren't matched yet
    // (one lookup per show, not per file), then merge results back by key path,
    // then prefetch the page's structures so counts verify (SKADI-T-0326).
    let match_page = move |p: usize| {
        let pending: Vec<api::TvMatchQuery> = {
            let idxs = visible.get_untracked();
            groups.with_untracked(|all| {
                idxs.iter()
                    .skip(p * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .filter_map(|&i| all.get(i))
                    .filter(|g| !g.matched)
                    .filter_map(|g| {
                        g.key_path().map(|kp| api::TvMatchQuery {
                            path: kp.to_string(),
                            series_title: Some(g.title.clone()),
                            year: g.year,
                        })
                    })
                    .collect()
            })
        };
        if pending.is_empty() {
            // Already matched (revisiting a page) — still firm up its counts.
            prefetch_structures(p);
            return;
        }
        matching.set(true);
        spawn_local(async move {
            match api::tv_library_import_match(&pending).await {
                Ok(results) => {
                    match_error.set(None);
                    groups.update(|all| {
                        for res in results {
                            if let Some(g) = all
                                .iter_mut()
                                .find(|g| g.key_path() == Some(res.path.as_str()))
                            {
                                g.matched = true;
                                g.confidence = res.confidence.clone();
                                g.already_in_library = res.already_in_library;
                                g.tvdb.set(
                                    res.proposed
                                        .as_ref()
                                        .map(|m| m.tvdb_id.to_string())
                                        .unwrap_or_default(),
                                );
                                // Pre-tick only a confident, non-duplicate match
                                // that actually has a parseable episode file — so a
                                // folder of pure extras (no SxxEyy) is never
                                // auto-selected and silently misfiled (SKADI-T-0324)
                                // — and never a group with duplicate-SxxEyy
                                // collisions (SKADI-T-0325).
                                let has_episode = g
                                    .candidates
                                    .iter()
                                    .any(|c| c.season.is_some() && !c.episodes.is_empty());
                                let empty_ov = HashMap::new();
                                let clean =
                                    collision_losers(&g.candidates, &empty_ov, None).is_empty();
                                g.selected.set(
                                    res.proposed.is_some()
                                        && !res.already_in_library
                                        && res.confidence == "high"
                                        && has_episode
                                        && clean,
                                );
                                g.proposed = res.proposed;
                            }
                        }
                    });
                    prefetch_structures(p);
                }
                Err(e) => match_error.set(Some(e.to_string())),
            }
            matching.set(false);
        });
    };

    let do_scan = move || {
        let p = path.get_untracked();
        if p.trim().is_empty() {
            scan_error.set(Some("enter a path to scan".into()));
            return;
        }
        scanning.set(true);
        scan_error.set(None);
        result.set(None);
        spawn_local(async move {
            match api::tv_library_import_scan(&p).await {
                Ok(cands) => {
                    groups.set(group_candidates(cands));
                    page.set(0);
                    scanned.set(true);
                    // Match the first page eagerly so there's something to review.
                    match_page(0);
                }
                Err(e) => scan_error.set(Some(e.to_string())),
            }
            scanning.set(false);
        });
    };
    let on_scan = move |_| do_scan();

    // An upload hands its staged path over as `?path=` (SKADI-T-0632) and the
    // page picks up from there — scanning straight away, since the operator
    // has already said what the file is and clicking Scan would be a step that
    // asks nothing. Nothing else about an uploaded file is special: from here
    // it is matched and committed exactly like a file that was always on disk.
    Effect::new(move |_| {
        let search = leptos_router::hooks::use_location().search.get();
        if let Some(staged) = crate::upload::staged_path_from_query(&search)
            && path.get_untracked().is_empty()
        {
            path.set(staged);
            do_scan();
        }
    });

    let go_page = move |p: usize| {
        page.set(p);
        match_page(p);
    };

    let on_import = move |_| {
        // Build commit items for every ticked show: auto-mapped files go in
        // unqualified (the backend re-parses), manually-mapped files carry their
        // chosen season+episode, and unmapped/un-picked files are skipped.
        // `item_group[i]` records which group produced item `i`, so a mid-batch
        // failure can mark exactly the groups whose items all committed
        // (SKADI-T-0326).
        let mut items = Vec::new();
        let mut item_group: Vec<usize> = Vec::new();
        let build = groups.with_untracked(|all| {
            for (gi, g) in all
                .iter()
                .enumerate()
                .filter(|(_, g)| g.selected.get_untracked() && !is_imported(g))
            {
                let id = match g.tvdb.get_untracked().trim().parse::<u64>() {
                    Ok(id) => id,
                    Err(_) => {
                        return Err(format!(
                            "“{}” has no valid TVDB id — set one or untick it",
                            g.key
                        ));
                    }
                };
                let ov = g.overrides.get_untracked();
                // Collision losers (duplicate SxxEyy) are never sent — importing
                // them would land on an episode another file already claims
                // (SKADI-T-0325).
                let losers = g.losers(&ov);
                for c in &g.candidates {
                    if let Some((Some(s), Some(e))) = ov.get(&c.path).copied() {
                        items.push(api::TvCommitItem {
                            path: c.path.clone(),
                            tvdb_id: id,
                            quality_id: c.quality_id.clone(),
                            season: Some(s),
                            episode: Some(e),
                        });
                        item_group.push(gi);
                    } else if !losers.contains(&c.path)
                        && !auto_episodes(c, g.structure.as_ref()).is_empty()
                    {
                        items.push(api::TvCommitItem {
                            path: c.path.clone(),
                            tvdb_id: id,
                            quality_id: c.quality_id.clone(),
                            season: None,
                            episode: None,
                        });
                        item_group.push(gi);
                    }
                    // else: unmapped / un-picked / colliding → skip.
                }
            }
            Ok(())
        });
        if let Err(e) = build {
            result.set(Some(Err(e)));
            return;
        }
        if items.is_empty() {
            result.set(Some(Err("nothing mapped to import".into())));
            return;
        }

        // Commit in client-side batches so the user sees live progress and no
        // single request has to carry hundreds of metadata syncs.
        const BATCH: usize = 25;
        let total = items.len();
        importing.set(true);
        result.set(None);
        progress.set(Some((0, total)));
        spawn_local(async move {
            let mut imported = 0usize;
            let mut skipped = 0usize;
            let mut linked = 0usize;
            let mut in_place = 0usize;
            let mut unmatched = Vec::new();
            let mut errors = Vec::new();
            let mut done = 0usize;
            for chunk in items.chunks(BATCH) {
                match api::tv_library_import_commit(chunk, None).await {
                    Ok(res) => {
                        imported += res.imported;
                        skipped += res.skipped;
                        linked += res.linked;
                        in_place += res.in_place;
                        unmatched.extend(res.unmatched);
                        errors.extend(res.errors);
                    }
                    Err(e) => {
                        // Mid-batch failure: keep the partial tallies — the
                        // operator must see what DID land (SKADI-T-0326).
                        errors.push(format!(
                            "commit failed after {done} of {total} files: {e} — \
                             the remaining files were not attempted"
                        ));
                        break;
                    }
                }
                done += chunk.len();
                progress.set(Some((done, total)));
            }
            importing.set(false);
            progress.set(None);
            // Mark only the groups whose items ALL committed (items are grouped
            // contiguously, so a group is complete iff every item index < done).
            let completed: HashSet<usize> = item_group
                .iter()
                .enumerate()
                .fold(HashMap::<usize, bool>::new(), |mut m, (ii, &gi)| {
                    *m.entry(gi).or_insert(true) &= ii < done;
                    m
                })
                .into_iter()
                .filter(|(_, complete)| *complete)
                .map(|(gi, _)| gi)
                .collect();
            result.set(Some(Ok(api::CommitResult {
                imported,
                skipped,
                linked,
                in_place,
                unmatched,
                errors,
            })));
            groups.update(|all| {
                for gi in completed {
                    if let Some(g) = all.get_mut(gi) {
                        g.selected.set(false);
                        g.already_in_library = true;
                    }
                }
            });
        });
    };

    let selected_count = move || {
        groups.with(|all| {
            all.iter()
                .filter(|g| g.selected.get() && !is_imported(g))
                .count()
        })
    };
    let total_pages = move || visible.get().len().div_ceil(PAGE_SIZE).max(1);
    // The current page, clamped — a commit with hide-imported on can shrink the
    // visible set under the page index and strand the operator on an empty
    // "page 4 of 2" (SKADI-T-0326).
    let cur_page = move || page.get().min(total_pages().saturating_sub(1));
    // Total distinct episodes the ticked shows will record — what "Import" will
    // actually place (multi-episode files count each episode; collision losers
    // count nothing, SKADI-T-0325).
    let selected_files = move || {
        groups.with(|all| {
            all.iter()
                .filter(|g| g.selected.get() && !is_imported(g))
                .map(|g| {
                    let ov = g.overrides.get();
                    g.episode_count(&ov)
                })
                .sum::<usize>()
        })
    };

    // Bulk selection (SKADI-T-0326): most shows arrive pre-ticked via NFO — a
    // cautious first pass shouldn't mean hundreds of unticks.
    let select_all_visible = move |_| {
        let idxs = visible.get_untracked();
        groups.with_untracked(|all| {
            for &i in &idxs {
                if let Some(g) = all.get(i)
                    && !is_imported(g)
                {
                    g.selected.set(true);
                }
            }
        });
    };
    let clear_selection = move |_| {
        groups.with_untracked(|all| {
            for g in all {
                g.selected.set(false);
            }
        });
    };

    let cards = move || {
        let start = cur_page() * PAGE_SIZE;
        let idxs = visible.get();
        idxs.into_iter()
            .skip(start)
            .take(PAGE_SIZE)
            .filter_map(|i| groups.with(|all| all.get(i).cloned()).map(|g| (i, g)))
            .map(|(i, g)| show_card(groups, i, g))
            .collect_view()
    };

    view! {
        <div class="page-head">
            <h2>"TV import"</h2>
            <A href="/tv" attr:class="gear" attr:title="Back to TV">"←"</A>
        </div>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Scan a folder"</h3>
                    <p class="muted">
                        "Bring an existing TV library under management. Episode "
                        "files are "
                        <strong>"hardlinked"</strong>
                        " into the library's canonical Series/Season layout "
                        "(each episode's subtitles and sidecars ride along); "
                        "after a successful link the source copies are removed "
                        "(an adoption "
                        <strong>"move"</strong>
                        " — no extra disk is used). Cross-device imports are "
                        "refused — scan through the same mount as the root folder."
                    </p>
                </div>
            </div>
            <div class="form">
                <div class="field">
                    <label>"Library path (as the server sees it, e.g. /library/tv)"</label>
                    <div class="checks">
                        <PathPicker value=path placeholder="/library/tv"/>
                        <button on:click=on_scan disabled=move || scanning.get()>
                            {move || if scanning.get() { "Scanning…" } else { "Scan" }}
                        </button>
                    </div>
                </div>
                {move || scanning.get().then(|| view! {
                    <p class="muted">"Scanning the folder (parse-only — fast). Matches are looked up a page at a time below."</p>
                })}
                {move || scan_error.get().map(|e| view! { <p class="bad">{e}</p> })}
            </div>
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Review & import"</h3>
                    <p class="muted">"One collapsible card per show — expand to see every season and episode, with each scanned file shown against the episode it maps to. Fix a TVDB id to re-fetch the tree; map any leftover files by hand under “Unmapped files”. Importing hardlinks the mapped files into the canonical Series/Season layout (quality detected per file)."</p>
                </div>
            </div>

            <div class="import-review">
                {move || (!scanned.get()).then(|| view! {
                    <p class="muted">"Scan a folder above to see candidates."</p>
                })}
                {move || (scanned.get() && groups.with(Vec::is_empty)).then(|| view! {
                    <p class="muted">"No importable episodes found under that path."</p>
                })}

                {move || match_error.get().map(|e| view! {
                    <p class="bad">
                        {format!("Matching failed: {e} ")}
                        <button on:click=move |_| match_page(cur_page())>"Retry matching"</button>
                    </p>
                })}

                {move || (!groups.with(Vec::is_empty)).then(|| {
                    let idxs = visible.get();
                    let shows = idxs.len();
                    let files: usize = groups.with(|all| {
                        idxs.iter()
                            .filter_map(|&i| all.get(i))
                            .map(|g| g.candidates.len())
                            .sum()
                    });
                    let pages = total_pages();
                    let cur = cur_page();
                    view! {
                        <div class="checks">
                            <span class="muted">
                                {if pages > 1 {
                                    format!("{shows} shows ({files} episode files) — page {} of {pages}", cur + 1)
                                } else {
                                    format!("{shows} shows · {files} episode files")
                                }}
                            </span>
                            {(pages > 1).then(|| view! {
                                <>
                                    <button
                                        on:click=move |_| go_page(cur.saturating_sub(1))
                                        disabled=move || cur_page() == 0 || matching.get()
                                    >"‹ Prev"</button>
                                    <button
                                        on:click=move |_| go_page(cur + 1)
                                        disabled=move || { cur_page() + 1 >= total_pages() || matching.get() }
                                    >"Next ›"</button>
                                </>
                            })}
                            {matching.get().then(|| view! { <span class="muted">"matching…"</span> })}
                            <button class="secondary" on:click=select_all_visible>"Select all"</button>
                            <button class="secondary" on:click=clear_selection>"Clear"</button>
                            <label class="import-hide-toggle muted">
                                <input
                                    type="checkbox"
                                    prop:checked=move || hide_imported.get()
                                    on:change=move |_| {
                                        hide_imported.update(|h| *h = !*h);
                                        page.set(0);
                                        // Toggling changes which shows land on page
                                        // 0 — make sure they get matched.
                                        match_page(0);
                                    }
                                />
                                {move || {
                                    // How many shows are already in the library —
                                    // regardless of whether they're currently
                                    // hidden or merely sunk (a "(0)" while
                                    // unchecked told the operator nothing).
                                    let n = groups.with(|all| {
                                        all.iter().filter(|g| is_imported(g)).count()
                                    });
                                    format!(" Hide already-imported ({n})")
                                }}
                            </label>
                        </div>
                    }
                })}

                {move || (!groups.with(Vec::is_empty)).then(|| view! {
                    <div class="card-editions import-shows">{cards}</div>
                })}

                {move || (!groups.with(Vec::is_empty)).then(|| {
                    view! {
                        <button on:click=on_import disabled=move || importing.get() || selected_count() == 0>
                            {move || match progress.get() {
                                Some((d, t)) => format!("Importing… {d} / {t}"),
                                None if importing.get() => "Importing…".to_string(),
                                None => {
                                    let shows = selected_count();
                                    let files = selected_files();
                                    format!("Import {shows} show{} ({files} episodes)", if shows == 1 { "" } else { "s" })
                                }
                            }}
                        </button>
                    }
                })}
                {move || progress.get().map(|(d, t)| view! {
                    <p class="muted">{format!("Imported {d} of {t}…")}</p>
                })}

                {move || result.get().map(|r| match r {
                    Ok(res) => crate::import_common::commit_result_view(&res),
                    Err(m) => view! { <p class="bad">{m}</p> }.into_any(),
                })}
            </div>
        </section>
    }
}

/// One show rendered as a collapsible Show → Season → Episode card, reusing the
/// detail page's `season-block` visual language. `idx` is the group's stable
/// index in `groups` (used only to write back a re-fetched structure).
fn show_card(groups: RwSignal<Vec<SeriesGroup>>, idx: usize, g: SeriesGroup) -> AnyView {
    let in_lib = g.already_in_library;
    // Append the year only when the title doesn't already carry it — TVDB
    // disambiguated titles ("Archer (2009)") were rendering "(2009) (2009)".
    let with_year = |title: &str, year: Option<u16>| -> String {
        match year {
            Some(y) if !title.contains(&format!("({y})")) => format!("{title} ({y})"),
            _ => title.to_string(),
        }
    };
    let title_text = g
        .proposed
        .as_ref()
        .map(|p| with_year(&p.title, p.year))
        .unwrap_or_else(|| with_year(&g.title, g.year));
    let confidence = g.confidence.clone();
    let conf_class = confidence_class(&confidence);
    let matched = g.matched;
    let has_match = g.proposed.is_some();

    let selected = g.selected;
    let tvdb = g.tvdb;
    let collapsed = g.collapsed;
    let overrides = g.overrides;
    let facts = g.facts.clone();
    let overview = g.overview.clone();
    let picker_open = g.picker_open;
    let search_q = g.search_q;
    let searching = g.searching;
    let search_results = g.search_results;
    let seed_title = g.title.clone();

    // Header count: mapped/total files. Until the episode structure has been
    // fetched the count only trusts the filename parse, so it renders as
    // "~x/y" (unverified) instead of a confident green — a collapsed card used
    // to claim "48/48 mapped" for files mapping to nonexistent episodes
    // (SKADI-T-0326). Once verified, shows needing manual work get an amber
    // "N to fix" so they're findable without expanding.
    let count_g = g.clone();
    let verified = g.structure.is_some();
    let header_count = move || {
        let ov = overrides.get();
        let (mapped, total, extras) = count_g.file_counts(&ov);
        // Extras are bonus content — they have no episode to capture, so they
        // never count against "fully captured" (SKADI-T-0329).
        let extras_note = if extras > 0 {
            format!(" · {extras} extra{}", if extras == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        if !verified {
            return view! {
                <span
                    class="season-count mono muted"
                    title="Unverified — assumes the filename parse is right. Counts firm up once the episode list loads (automatic for this page, or expand the card)."
                >
                    {format!("~{mapped}/{total} mapped{extras_note}")}
                </span>
            }
            .into_any();
        }
        if total == 0 && extras > 0 {
            // A folder of pure bonus content — nothing to capture.
            return view! {
                <span class="season-count mono muted" title="Bonus content only — no episodes to import">
                    {format!("{extras} extras")}
                </span>
            }
            .into_any();
        }
        if total > 0 && mapped == total {
            view! {
                <span class="season-count mono ok">{format!("{mapped}/{total} mapped{extras_note}")}</span>
            }
            .into_any()
        } else {
            view! {
                <span class="season-count mono pending" title="Expand to map the leftover files by hand">
                    {format!("{mapped}/{total} mapped · {} to fix{extras_note}", total - mapped)}
                </span>
            }
            .into_any()
        }
    };

    // Fetch this show's episode tree (shared by expand / id edit / picker pick).
    // On failure the error is stored and `structure_fetched` is reset so a
    // collapse + re-expand retries — never an eternal "Loading…" (SKADI-T-0324
    // review).
    let fetch_structure = move |id: u64| {
        groups.update(|all| {
            if let Some(gg) = all.get_mut(idx) {
                gg.structure = None;
                gg.structure_error = None;
                gg.structure_fetched = true;
            }
        });
        spawn_local(async move {
            let fetched = api::tv_import_series_structure(id).await;
            groups.update(|all| {
                if let Some(gg) = all.get_mut(idx) {
                    match fetched {
                        Ok(st) => gg.structure = Some(st),
                        Err(e) => {
                            gg.structure_error = Some(e.to_string());
                            gg.structure_fetched = false; // re-expand retries
                        }
                    }
                }
            });
        });
    };

    // TVDB id edit: update the id and re-fetch the show's structure for it. The
    // old proposed match no longer describes this id — drop it and mark the
    // match "manual", or the card keeps showing the previous show's title with
    // a green nfo/high badge next to a different id (SKADI-T-0326).
    let on_tvdb = move |ev| {
        let val = event_target_value(&ev);
        tvdb.set(val.clone());
        if let Ok(id) = val.trim().parse::<u64>() {
            groups.update(|all| {
                if let Some(gg) = all.get_mut(idx) {
                    gg.proposed = None;
                    gg.matched = true;
                    gg.confidence = "manual".to_string();
                }
            });
            fetch_structure(id);
        }
    };

    // Series picker: toggle the search box (seeding the query with the title on
    // first open), run a title search, and apply a chosen result as the match.
    let toggle_picker = move |_| {
        let opening = !picker_open.get();
        picker_open.update(|o| *o = !*o);
        if opening && search_q.get_untracked().trim().is_empty() {
            search_q.set(seed_title.clone());
        }
    };
    let run_search = move || {
        let q = search_q.get_untracked().trim().to_string();
        if q.is_empty() {
            return;
        }
        searching.set(true);
        spawn_local(async move {
            match api::search_series(&q).await {
                Ok(results) => search_results.set(results),
                Err(_) => search_results.set(Vec::new()),
            }
            searching.set(false);
        });
    };
    // Apply a picked search result as the show's match: exact id + real metadata,
    // re-fetch the structure, and close the picker.
    let apply_pick = move |res: api::SeriesSearchResult| {
        let id = res.tvdb_id;
        tvdb.set(id.to_string());
        groups.update(|all| {
            if let Some(gg) = all.get_mut(idx) {
                gg.proposed = Some(api::TvProposedMatch {
                    tvdb_id: id,
                    title: res.title.clone(),
                    year: res.year,
                    poster_url: res.poster_url.clone(),
                });
                gg.matched = true;
                gg.confidence = "manual".to_string();
                gg.already_in_library = false;
                gg.selected.set(true);
            }
        });
        picker_open.set(false);
        search_results.set(Vec::new());
        fetch_structure(id);
    };

    let body_g = g.clone();
    let body = move || {
        let g = body_g.clone();
        if in_lib {
            return view! { <p class="muted ep-empty">"Already in library."</p> }.into_any();
        }
        if let Some(err) = g.structure_error.clone() {
            return view! {
                <p class="bad ep-empty">
                    {format!("Couldn't load the episode tree: {err} — collapse and re-expand to retry.")}
                </p>
            }
            .into_any();
        }
        // A loaded structure renders regardless of match state — a raw-id edit
        // drops `proposed` but the tree is what the operator asked for.
        let Some(structure) = g.structure.clone() else {
            if matched && !has_match && !g.structure_fetched {
                return view! { <p class="muted ep-empty">"No match — use 🔍 Find show above to pick the right series."</p> }
                    .into_any();
            }
            return view! { <p class="muted ep-empty">"Loading episode tree…"</p> }.into_any();
        };

        let ov = overrides.get();

        // Seasons ascending, Specials (0) last — mirrors the detail page.
        let mut seasons = structure.seasons.clone();
        seasons.sort_by_key(|s| if s.number == 0 { u16::MAX } else { s.number });
        let season_blocks = seasons
            .into_iter()
            .map(|sea| import_season_block(&g, &structure, &ov, sea))
            .collect_view();

        // Files needing a manual pick: no mapping at all, or a duplicate-SxxEyy
        // collision loser (another file already claims its episode, SKADI-T-0325).
        // Extras (bonus content) are split into their own muted section — they
        // aren't missing episodes, so they don't wear the warning (SKADI-T-0329);
        // the picker stays available in case one really is an episode.
        let losers = g.losers(&ov);
        let extras_set = g.extra_paths(&ov);
        let unmapped: Vec<api::TvScanCandidate> = g
            .candidates
            .iter()
            .filter(|c| {
                !extras_set.contains(&c.path)
                    && (losers.contains(&c.path) || g.mappings_of(c, &ov).is_empty())
            })
            .cloned()
            .collect();
        let extras: Vec<api::TvScanCandidate> = g
            .candidates
            .iter()
            .filter(|c| extras_set.contains(&c.path))
            .cloned()
            .collect();
        // Episodes the winners already occupy — the picker warns when a manual
        // pick would double-book one (SKADI-T-0326).
        let claimed: HashSet<(u16, u16)> = g
            .candidates
            .iter()
            .filter(|c| !losers.contains(&c.path))
            .flat_map(|c| g.mappings_of(c, &ov))
            .collect();
        let n_un = unmapped.len();
        let unmapped_block_v = (!unmapped.is_empty())
            .then(|| {
                unmapped_block(
                    &structure,
                    overrides,
                    unmapped,
                    losers,
                    claimed.clone(),
                    format!("⚠ Unmapped files ({n_un})"),
                    false,
                )
            })
            .into_view();
        let n_ex = extras.len();
        let extras_block_v = (!extras.is_empty())
            .then(|| {
                unmapped_block(
                    &structure,
                    overrides,
                    extras,
                    HashSet::new(),
                    claimed,
                    format!("Extras ({n_ex}) — bonus content, not counted"),
                    true,
                )
            })
            .into_view();

        view! { <>{season_blocks}{unmapped_block_v}{extras_block_v}</> }.into_any()
    };

    view! {
        <div class="edition-block season-block import-show" class:in-lib=in_lib>
            <div class="edition-row season-head">
                <input
                    type="checkbox"
                    prop:checked=move || selected.get()
                    disabled=in_lib
                    on:change=move |_| selected.update(|s| *s = !*s)
                />
                <button
                    class="btn-link season-toggle"
                    on:click=move |_| {
                        let opening = collapsed.get();
                        collapsed.update(|c| *c = !*c);
                        // Lazily fetch this show's episode tree the first time it's
                        // opened — never for the whole page at once (Sonarr-style),
                        // so a big library doesn't hammer the metadata provider.
                        if opening {
                            let need = groups.with_untracked(|all| {
                                all.get(idx)
                                    .is_some_and(|g| !g.structure_fetched && !g.already_in_library)
                            });
                            if need && let Ok(id) = tvdb.get_untracked().trim().parse::<u64>() {
                                fetch_structure(id);
                            }
                        }
                    }
                >
                    <span class="chevron">{move || if collapsed.get() { "▸" } else { "▾" }}</span>
                    <strong>{title_text}</strong>
                </button>
                {(matched && has_match).then(|| view! {
                    <span class=format!("badge {conf_class}") title=confidence_title(&confidence)>
                        {confidence.clone()}
                    </span>
                })}
                {(!matched).then(|| view! { <span class="muted">"matching…"</span> })}
                {(matched && has_match).then(header_count)}
                <button
                    class="btn-link import-find"
                    disabled=in_lib
                    title="Search for the right show"
                    on:click=toggle_picker
                >
                    "🔍 Find show"
                </button>
                <input
                    class="tmdb-input import-tvdb"
                    type="text"
                    placeholder="tvdb id"
                    prop:value=move || tvdb.get()
                    disabled=in_lib
                    on:change=on_tvdb
                />
            </div>
            // NFO facts line + synopsis, to confirm the auto-match at a glance.
            {(facts.is_some() || overview.is_some()).then(|| view! {
                <div class="import-show-meta">
                    {facts.map(|f| view! { <span class="import-facts">{f}</span> })}
                    {overview.map(|o| view! { <span class="import-overview">{o}</span> })}
                </div>
            })}
            {move || picker_open.get().then(|| view! {
                <div class="import-picker">
                    <div class="import-picker-search">
                        <input
                            class="path-field"
                            placeholder="Search series title…"
                            prop:value=move || search_q.get()
                            on:input=move |ev| search_q.set(event_target_value(&ev))
                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                if ev.key() == "Enter" { run_search(); }
                            }
                        />
                        <button class="btn" on:click=move |_| run_search()>"Search"</button>
                    </div>
                    {move || searching.get().then(|| view! {
                        <p class="muted ep-empty">"Searching…"</p>
                    })}
                    {move || (!searching.get() && search_results.get().is_empty()
                        && !search_q.get().trim().is_empty()).then(|| view! {
                        <p class="muted ep-empty">"No results — try a different title."</p>
                    })}
                    <div class="import-picker-results">
                        {move || search_results.get().into_iter().map(|res| {
                            let yr = res.year.map(|y| format!(" ({y})")).unwrap_or_default();
                            // Poster (SKADI-T-0319). Reboots and remakes share a
                            // title and sit years apart; the artwork is what an
                            // operator recognises at a glance.
                            let poster = res.poster_url.clone();
                            let r = res.clone();
                            view! {
                                <button
                                    class="import-picker-result"
                                    on:click=move |_| apply_pick(r.clone())
                                >
                                    {poster.map(|src| view! {
                                        <img class="picker-art" src=src alt="" loading="lazy"/>
                                    })}
                                    <span class="picker-title">{format!("{}{yr}", res.title)}</span>
                                    <span class="mono muted">{format!("tvdb {}", res.tvdb_id)}</span>
                                </button>
                            }
                        }).collect_view()}
                    </div>
                </div>
            })}
            <div class="season-eps import-show-body" class:collapsed=move || collapsed.get()>
                // Render the episode tree ONLY while expanded — a collapsed show is
                // just its header, so a library of hundreds of shows (each with
                // hundreds of episodes) stays light instead of pre-building tens of
                // thousands of DOM nodes (SKADI-T-0324 perf).
                {move || (!collapsed.get()).then(&body)}
            </div>
        </div>
    }
    .into_any()
}

/// One season block inside a show card: a collapsible header (`mapped/episode_count
/// mapped`, green when full) over an episode row per structure episode.
fn import_season_block(
    g: &SeriesGroup,
    structure: &api::TvSeriesStructure,
    ov: &HashMap<String, (Option<u16>, Option<u16>)>,
    sea: api::TvStructSeason,
) -> AnyView {
    let num = sea.number;
    let label = season_label(num);

    // Episodes of this season from the structure, in order.
    let mut eps: Vec<api::TvStructEpisode> = structure
        .episodes
        .iter()
        .filter(|e| e.season == num)
        .cloned()
        .collect();
    eps.sort_by_key(|e| e.number);

    // The file (if any) mapped to each episode, by episode number. Collision
    // losers are excluded (they live in the manual-picker block); a multi-episode
    // file appears against each of its episodes (SKADI-T-0325).
    let losers = g.losers(ov);
    let mut mapped_by_ep: HashMap<u16, String> = HashMap::new();
    for c in &g.candidates {
        if losers.contains(&c.path) {
            continue;
        }
        for (s, e) in g.mappings_of(c, ov) {
            if s == num {
                mapped_by_ep
                    .entry(e)
                    .or_insert_with(|| c.display_name.clone());
            }
        }
    }

    let total = sea.episode_count.max(eps.len() as u16);
    let mapped = eps
        .iter()
        .filter(|e| mapped_by_ep.contains_key(&e.number))
        .count();
    let complete = total > 0 && mapped as u16 == total;
    let count_cls = if complete {
        "season-count mono ok"
    } else {
        "season-count mono muted"
    };

    let rows = eps
        .into_iter()
        .map(|e| {
            let code = season_episode_code(num, e.number);
            let title = e.title.clone().unwrap_or_default();
            match mapped_by_ep.get(&e.number) {
                Some(file) => view! {
                    <div class="edition-row episode-row">
                        <span class="mono">{code}</span>
                        <span class="episode-title">{title}</span>
                        <span class="ep-file mono">{format!("← {file}")}</span>
                    </div>
                }
                .into_any(),
                None => view! {
                    <div class="edition-row episode-row ep-empty">
                        <span class="mono">{code}</span>
                        <span class="episode-title">{title}</span>
                        <span class="ep-file muted">"(no file)"</span>
                    </div>
                }
                .into_any(),
            }
        })
        .collect_view();

    // The expanded-season set lives on the group so collapse survives re-renders.
    let expanded = g.expanded_seasons;
    let is_open = move || expanded.get().contains(&num);

    view! {
        <div class="edition-block season-block">
            <div class="edition-row season-head">
                <button
                    class="btn-link season-toggle"
                    on:click=move |_| expanded.update(|s| {
                        if !s.remove(&num) {
                            s.insert(num);
                        }
                    })
                >
                    <span class="chevron">{move || if is_open() { "▾" } else { "▸" }}</span>
                    <strong>{label}</strong>
                </button>
                <span class=count_cls>{format!("{mapped}/{total} mapped")}</span>
            </div>
            <div class="season-eps" class:collapsed=move || !is_open()>
                {rows}
            </div>
        </div>
    }
    .into_any()
}

/// The `⚠ Unmapped files (N)` group: a Season + Episode `<select>` per file that
/// didn't auto-map — including duplicate-SxxEyy collision losers, which carry a
/// "duplicate SxxEyy" tag so the operator knows *why* they're here
/// (SKADI-T-0325). Picking both records an override (the file then counts as
/// mapped and slides under its episode on the next re-render).
fn unmapped_block(
    structure: &api::TvSeriesStructure,
    overrides: RwSignal<HashMap<String, (Option<u16>, Option<u16>)>>,
    files: Vec<api::TvScanCandidate>,
    losers: HashSet<String>,
    claimed: HashSet<(u16, u16)>,
    heading: String,
    muted: bool,
) -> AnyView {
    let mut seasons = structure.seasons.clone();
    seasons.sort_by_key(|s| if s.number == 0 { u16::MAX } else { s.number });
    let claimed = StoredValue::new(claimed);

    let rows = files
        .into_iter()
        .map(|c| {
            let is_loser = losers.contains(&c.path);
            let dup_code = is_loser
                .then(|| episode_label(c.season, &c.episodes, &c.absolute))
                .filter(|l| l != "—");
            let seasons = seasons.clone();
            let episodes = structure.episodes.clone();
            // The path keys the override map; a StoredValue keeps the read
            // closures `Copy` so they can drive several reactive attributes.
            let path = StoredValue::new(c.path.clone());

            let cur_season = move || overrides.get().get(&path.get_value()).and_then(|x| x.0);
            let cur_episode = move || overrides.get().get(&path.get_value()).and_then(|x| x.1);

            let on_season = move |ev| {
                let v = event_target_value(&ev).parse::<u16>().ok();
                overrides.update(|m| {
                    let e = m.entry(path.get_value()).or_default();
                    e.0 = v;
                    e.1 = None; // reset the episode when the season changes
                });
            };
            let on_episode = move |ev| {
                let v = event_target_value(&ev).parse::<u16>().ok();
                overrides.update(|m| {
                    m.entry(path.get_value()).or_default().1 = v;
                });
            };

            // Episode options depend on the currently-picked season (reactive).
            let ep_episodes = episodes.clone();
            let ep_options = move || {
                let Some(s) = cur_season() else {
                    return ().into_any();
                };
                let mut list: Vec<api::TvStructEpisode> = ep_episodes
                    .iter()
                    .filter(|e| e.season == s)
                    .cloned()
                    .collect();
                list.sort_by_key(|e| e.number);
                let chosen = cur_episode();
                list.into_iter()
                    .map(|e| {
                        let lbl = episode_option_label(e.number, e.title.as_deref());
                        view! {
                            <option value=e.number.to_string() selected=chosen == Some(e.number)>
                                {lbl}
                            </option>
                        }
                    })
                    .collect_view()
                    .into_any()
            };

            let resolved = move || cur_season().is_some() && cur_episode().is_some();
            let season_opts = seasons.clone();

            view! {
                <div class="edition-row unmapped-row" class:resolved=resolved>
                    <span class="episode-title" title=c.display_name.clone()>{c.display_name.clone()}</span>
                    {dup_code.map(|code| view! {
                        <span
                            class="badge pending"
                            title="Another file already claims this episode — pick where this one really belongs"
                        >
                            {format!("duplicate {code}")}
                        </span>
                    })}
                    <select class="tmdb-input" on:change=on_season>
                        <option value="" selected=move || cur_season().is_none()>"Season"</option>
                        {season_opts.into_iter().map(|s| {
                            let lbl = season_label(s.number);
                            let num = s.number;
                            view! {
                                <option value=num.to_string() selected=move || cur_season() == Some(num)>
                                    {lbl}
                                </option>
                            }
                        }).collect_view()}
                    </select>
                    <select class="tmdb-input" disabled=move || cur_season().is_none() on:change=on_episode>
                        <option value="" selected=move || cur_episode().is_none()>"Episode"</option>
                        {ep_options}
                    </select>
                    // Live double-booking warning: the picked episode already has
                    // a file mapped to it (SKADI-T-0326).
                    {move || {
                        let pick = cur_season().zip(cur_episode());
                        pick.filter(|p| claimed.with_value(|c| c.contains(p))).map(|(s, e)| view! {
                            <span class="bad mono" title="Another file is already mapped to this episode — pick a different one or the import will skip this file as a duplicate">
                                {format!("⚠ {} taken", season_episode_code(s, e))}
                            </span>
                        })
                    }}
                </div>
            }
        })
        .collect_view();

    view! {
        <div class="edition-block unmapped" class:extras-block=muted>
            <div class="edition-row unmapped-head">
                <strong class=if muted { "muted" } else { "" }>{heading}</strong>
            </div>
            {rows}
        </div>
    }
    .into_any()
}
