//! Web UI logic unit tests (SKADI-T-0118) — the pure helpers of `skadi-web`,
//! run as real wasm in a headless browser via
//! `wasm-pack test --headless --chrome`. No DOM is touched here; these cover the
//! parse/format/derive logic. Component/DOM tests are SKADI-T-0119.

use serde_json::json;
use wasm_bindgen_test::*;

use skadi_web::activity::{
    STAGE_GROUPS, ago, collapse_runs, detail_facts, detail_summary, diagnose, event_label,
    filter_allows, fold_runs, norm_title, parse_found, parse_handed, remedy_for, run_lengths,
    series_key, stage_group,
};
use skadi_web::api::{
    Book, BookFile, Movie, MovieEdition, ReleaseCandidate, SeriesLink, Work, download_progress,
    status_label,
};
use skadi_web::api::{MediaAudio, MediaInfo, MediaVideo, TraceRow};
use skadi_web::api::{TvScanCandidate, TvSeriesStructure, TvStructEpisode, TvStructSeason};
use skadi_web::api::{WantedEdition, WantedItem};
use skadi_web::audiobook_import::confidence_class;
use skadi_web::audiobooks::{
    SeriesTile, book_byline, book_status, clean_overview, extract_asin, file_resettable,
    group_books_by_series, looks_like_asin, merge_owned_missing, series_label,
};
use skadi_web::dashboard::{book_counts, check_class, movie_counts};
use skadi_web::movies::{
    active_profile_name, profile_is_active, size_human, status_class, tmdb_img,
};
use skadi_web::player::{
    chapter_at, fmt_clock, next_chapter_target, next_speed, prev_chapter_target,
};
use skadi_web::tv_import::{
    auto_episode, auto_episodes, clean_folder_title, collision_losers, episode_label,
    episode_option_label, season_label,
};
use skadi_web::wanted::{
    acquirable_path, detail_href, edition_label, filter_items, is_special, kind_counts, kind_label,
    ordered_editions, series_with_specials,
};
use skadi_web::watch::{
    Playability, audio_mime, container_mime, episode_code, next_episode, playability, prev_episode,
    resume_at, video_mime,
};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn clean_folder_title_strips_ids_and_year_but_keeps_numeric_titles() {
    // skadi's canonical folder: id tag + parenthesised year stripped, kebab → spaces.
    assert_eq!(
        clean_folder_title("12-monkeys_(2015)_{tmdb-60948}"),
        ("12 monkeys".to_string(), Some(2015))
    );
    assert_eq!(
        clean_folder_title("3-body-problem_(2024)_{tmdb-108545}"),
        ("3 body problem".to_string(), Some(2024))
    );
    // A plain folder with no decorations passes through.
    assert_eq!(
        clean_folder_title("Doctor Who"),
        ("Doctor Who".to_string(), None)
    );
    // A title that *is* a year survives — only the parenthesised year is stripped.
    assert_eq!(
        clean_folder_title("1899 (2022)"),
        ("1899".to_string(), Some(2022))
    );
    assert_eq!(clean_folder_title("2012"), ("2012".to_string(), None));
}

#[wasm_bindgen_test]
fn clean_folder_title_drops_quality_junk_and_trailing_season() {
    // Quality-junk paren group + trailing bare season token both poison the
    // metadata search (SKADI-T-0325).
    assert_eq!(
        clean_folder_title("MST3K S00 (360p re-dvdrip)"),
        ("MST3K".to_string(), None)
    );
    assert_eq!(
        clean_folder_title("Show Name Season 3"),
        ("Show Name".to_string(), None)
    );
    // A non-junk paren group (subtitle) is kept; an S-token mid-title is kept.
    assert_eq!(
        clean_folder_title("Mobile Suit Gundam (The Origin)"),
        ("Mobile Suit Gundam (The Origin)".to_string(), None)
    );
    // `title_year_tvdbid_imdbid` folders (no NFO): ids stripped, year captured.
    assert_eq!(
        clean_folder_title("better-call-saul_2015_273181_tt3032476"),
        ("better call saul".to_string(), Some(2015))
    );
    // A title ending in a year-like number survives — only ONE trailing year is
    // taken, ids (5+ digits / ttNNN) go first.
    assert_eq!(
        clean_folder_title("blade-runner-2049_2017_305088_tt1856101"),
        ("blade runner 2049".to_string(), Some(2017))
    );
}

#[wasm_bindgen_test]
fn size_human_formats_binary_units() {
    assert_eq!(size_human(0), "—");
    assert_eq!(size_human(512), "512 B");
    assert_eq!(size_human(1024), "1.0 KB");
    assert_eq!(size_human(1536), "1.5 KB");
    assert_eq!(size_human(8_000_000_000), "7.5 GB");
}

#[wasm_bindgen_test]
fn status_label_reads_string_and_object_variants() {
    assert_eq!(status_label(&json!("Missing")), "missing");
    assert_eq!(
        status_label(&json!({"Imported": {"path": "x"}})),
        "imported"
    );
    assert_eq!(status_label(&json!(42)), "unknown");
}

#[wasm_bindgen_test]
fn status_class_maps_labels() {
    assert_eq!(status_class("imported"), "ok");
    assert_eq!(status_class("cutoff"), "ok");
    assert_eq!(status_class("failed"), "bad");
    assert_eq!(status_class("missing"), "muted");
    // searching / snatched / downloading all fall through to pending.
    assert_eq!(status_class("downloading"), "pending");
}

#[wasm_bindgen_test]
fn check_class_maps_health_status() {
    assert_eq!(check_class("ok"), "ok");
    assert_eq!(check_class("warn"), "pending");
    assert_eq!(check_class("fail"), "bad");
    assert_eq!(check_class("anything-else"), "bad");
}

#[wasm_bindgen_test]
fn download_progress_extracts_fraction() {
    assert_eq!(
        download_progress(&json!({"Downloading": {"release": "r", "progress": 0.42}})),
        Some(0.42)
    );
    assert_eq!(download_progress(&json!("Missing")), None);
    assert_eq!(download_progress(&json!({"Imported": {}})), None);
}

#[wasm_bindgen_test]
fn tmdb_img_rewrites_size_segment() {
    assert_eq!(
        tmdb_img("https://image.tmdb.org/t/p/original/abc.jpg", "w342"),
        "https://image.tmdb.org/t/p/w342/abc.jpg"
    );
    // Non-TMDB URLs pass through unchanged.
    assert_eq!(
        tmdb_img("https://example.com/x.jpg", "w342"),
        "https://example.com/x.jpg"
    );
}

#[wasm_bindgen_test]
fn release_candidate_accessors_and_blocklisted() {
    let accepted = ReleaseCandidate {
        relevance: 0.0,
        season_pack: false,
        release: json!({"title": "The.Matrix.1999.1080p", "size": 8_000_000_000u64, "seeders": 42}),
        release_key: "btih:abc".into(),
        quality: "Bluray-1080p".into(),
        age_days: 3,
        accepted: true,
        reason: "Bluray-1080p".into(),
    };
    assert_eq!(accepted.title(), "The.Matrix.1999.1080p");
    assert_eq!(accepted.size_bytes(), 8_000_000_000);
    assert_eq!(accepted.seeders(), Some(42));
    assert!(!accepted.blocklisted());

    let blocked = ReleaseCandidate {
        relevance: 0.0,
        season_pack: false,
        release: json!({"title": "x"}),
        release_key: "k".into(),
        quality: String::new(),
        age_days: 0,
        accepted: false,
        reason: "blocklisted".into(),
    };
    assert!(blocked.blocklisted());
    assert_eq!(blocked.seeders(), None);
    assert_eq!(blocked.size_bytes(), 0);

    // Rejected-but-not-blocklisted is not "blocklisted".
    let rejected = ReleaseCandidate {
        relevance: 0.0,
        season_pack: false,
        reason: "too few seeders (0 < 1)".into(),
        ..blocked.clone()
    };
    assert!(!rejected.blocklisted());
}

#[wasm_bindgen_test]
fn movie_counts_rolls_up_editions() {
    let mk = |monitored: bool, statuses: &[serde_json::Value]| Movie {
        content_rating: None,
        genres: Vec::new(),
        id: "m".into(),
        title: "t".into(),
        year: None,
        overview: None,
        poster_url: None,
        backdrop_url: None,
        monitored,
        editions: statuses
            .iter()
            .enumerate()
            .map(|(i, s)| MovieEdition {
                id: format!("e{i}"),
                kind: "k".into(),
                status: s.clone(),
                media_info: None,
            })
            .collect(),
    };

    let movies = vec![
        mk(true, &[json!({"Imported": {}})]), // imported + monitored
        mk(false, &[json!("Downloading")]),   // in progress
        mk(true, &[json!("Missing")]),        // missing + monitored
    ];
    let c = movie_counts(&movies);
    assert_eq!(c.total, 3);
    assert_eq!(c.monitored, 2);
    assert_eq!(c.imported, 1);
    assert_eq!(c.in_progress, 1);
    assert_eq!(c.missing, 1);

    // A movie with any imported edition counts as imported even if another is missing.
    let mixed = vec![mk(false, &[json!("Missing"), json!({"Imported": {}})])];
    let c = movie_counts(&mixed);
    assert_eq!(c.imported, 1);
    assert_eq!(c.missing, 0);
}

// --- Audiobooks (SKADI-T-0133) ---

fn book_with(statuses: &[serde_json::Value]) -> Book {
    Book {
        id: "b".into(),
        external_ids: Default::default(),
        title: "t".into(),
        subtitle: None,
        author_id: None,
        authors: vec![],
        narrators: vec![],
        series: None,
        year: None,
        overview: None,
        cover_url: None,
        monitored: true,
        files: statuses
            .iter()
            .enumerate()
            .map(|(i, s)| BookFile {
                id: format!("f{i}"),
                book_id: "b".into(),
                status: s.clone(),
                quality: None,
                format_score: 0,
            })
            .collect(),
    }
}

#[wasm_bindgen_test]
fn book_status_rolls_up_files() {
    assert_eq!(
        book_status(&book_with(&[json!("Missing")])),
        ("muted", "missing")
    );
    assert_eq!(
        book_status(&book_with(&[json!("Downloading")])),
        ("pending", "in progress")
    );
    assert_eq!(
        book_status(&book_with(&[json!({"Imported": {}})])),
        ("ok", "imported")
    );
    // Imported wins over a missing sibling file.
    assert_eq!(
        book_status(&book_with(&[json!("Missing"), json!({"Imported": {}})])),
        ("ok", "imported")
    );
    // No files at all → missing.
    assert_eq!(book_status(&book_with(&[])), ("muted", "missing"));
}

#[wasm_bindgen_test]
fn book_counts_rolls_up_books_for_the_dashboard() {
    let mut imported = book_with(&[json!({"Imported": {}})]);
    imported.monitored = true;
    let mut downloading = book_with(&[json!("Downloading")]);
    downloading.monitored = false;
    let mut missing = book_with(&[json!("Missing")]);
    missing.monitored = true;

    let c = book_counts(&[imported, downloading, missing]);
    assert_eq!(c.total, 3);
    assert_eq!(c.monitored, 2);
    assert_eq!(c.imported, 1);
    assert_eq!(c.in_progress, 1);
    assert_eq!(c.missing, 1);

    // Empty library tallies to all-zero (the "no books yet" dashboard state).
    assert_eq!(book_counts(&[]), Default::default());
}

#[wasm_bindgen_test]
fn book_byline_joins_authors_and_narrators() {
    let mut b = book_with(&[]);
    assert_eq!(book_byline(&b), "");
    b.authors = vec!["Andy Weir".into()];
    assert_eq!(book_byline(&b), "Andy Weir");
    b.narrators = vec!["Ray Porter".into()];
    assert_eq!(book_byline(&b), "Andy Weir · read by Ray Porter");
    b.authors = vec![];
    assert_eq!(book_byline(&b), "read by Ray Porter");
}

#[wasm_bindgen_test]
fn clean_overview_strips_html_into_paragraphs() {
    // The Audnexus shape that used to render verbatim on the detail page.
    let raw = "<p>THE #1 <b>NEW YORK TIMES</b> BESTSELLER</p><p>Ryland Grace is the \
               sole survivor.<br>And he can't remember.</p>";
    let paras = clean_overview(raw);
    assert_eq!(
        paras,
        vec![
            "THE #1 NEW YORK TIMES BESTSELLER".to_string(),
            "Ryland Grace is the sole survivor.".to_string(),
            "And he can't remember.".to_string(),
        ]
    );
    // No literal tags survive anywhere.
    assert!(paras.iter().all(|p| !p.contains('<') && !p.contains('>')));

    // Entities are decoded; plain text passes through as a single paragraph.
    assert_eq!(
        clean_overview("Beck &amp; Call &mdash; a tale"),
        vec!["Beck & Call \u{2014} a tale".to_string()]
    );
    assert_eq!(
        clean_overview("Just plain text."),
        vec!["Just plain text.".to_string()]
    );
    // Empty / whitespace-only yields no paragraphs.
    assert!(clean_overview("   ").is_empty());
}

#[wasm_bindgen_test]
fn group_books_by_series_groups_orders_and_separates_standalone() {
    let mk = |title: &str, series: Option<(&str, &str, &str)>| {
        let mut b = book_with(&[json!("Missing")]);
        b.title = title.into();
        b.series = series.map(|(id, name, pos)| SeriesLink {
            series_id: id.into(),
            name: name.into(),
            position: Some(pos.into()),
        });
        b
    };
    let books = vec![
        mk("Standalone One", None),
        mk("Dresden 2", Some(("d", "The Dresden Files", "2"))),
        mk("Dresden 1", Some(("d", "The Dresden Files", "1"))),
        mk("Mistborn 1", Some(("m", "Mistborn", "1"))),
    ];
    let (groups, standalone) = group_books_by_series(books);

    // Groups sorted by name (case-insensitive): "mistborn" < "the dresden files".
    assert_eq!(
        groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
        vec!["Mistborn", "The Dresden Files"]
    );
    // Books within a series ordered by position.
    let dresden = groups.iter().find(|g| g.series_id == "d").unwrap();
    assert_eq!(
        dresden
            .books
            .iter()
            .map(|b| b.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Dresden 1", "Dresden 2"]
    );
    // Standalone books separated out.
    assert_eq!(standalone.len(), 1);
    assert_eq!(standalone[0].title, "Standalone One");
}

#[wasm_bindgen_test]
fn merge_owned_missing_orders_by_position_and_dedups() {
    // Owned #2 and #4; catalog knows #1..#4. #4 is also present as a Work but we
    // own it (by ASIN) → must not double as a missing tile.
    let owned_book = |asin: &str, pos: &str| {
        let mut b = book_with(&[json!({"Imported": {}})]);
        b.external_ids.asin = Some(asin.into());
        b.series = Some(SeriesLink {
            series_id: "dcc".into(),
            name: "Dungeon Crawler Carl".into(),
            position: Some(pos.into()),
        });
        b
    };
    let work = |asin: &str, pos: &str, owned: bool| Work {
        asin: asin.into(),
        title: format!("Book {pos}"),
        authors: vec!["Matt Dinniman".into()],
        series_name: Some("Dungeon Crawler Carl".into()),
        series_position: Some(pos.into()),
        cover_url: None,
        release_date: None,
        owned,
        book_id: None,
        watched: false,
    };
    let owned = vec![owned_book("OWN2", "2"), owned_book("OWN4", "4")];
    let works = vec![
        work("MISS1", "1", false),
        work("OWN2", "2", true),
        work("MISS3", "3", false),
        work("OWN4", "4", true), // owned by ASIN → excluded from missing
    ];
    let tiles = merge_owned_missing(owned, &works);
    // Position order 1,2,3,4 with 1 & 3 missing, 2 & 4 owned; no duplicate #4.
    let shape: Vec<(&str, f64)> = tiles
        .iter()
        .map(|t| match t {
            SeriesTile::Owned(_) => ("owned", t.pos_key()),
            SeriesTile::Missing(_) => ("missing", t.pos_key()),
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("missing", 1.0),
            ("owned", 2.0),
            ("missing", 3.0),
            ("owned", 4.0),
        ]
    );
}

#[wasm_bindgen_test]
fn series_label_formats_position() {
    let with_pos = SeriesLink {
        series_id: "s".into(),
        name: "Stormlight Archive".into(),
        position: Some("1".into()),
    };
    assert_eq!(series_label(&with_pos), "Stormlight Archive #1");
    let no_pos = SeriesLink {
        series_id: "s".into(),
        name: "Stormlight Archive".into(),
        position: None,
    };
    assert_eq!(series_label(&no_pos), "Stormlight Archive");
    let empty_pos = SeriesLink {
        position: Some(String::new()),
        ..no_pos.clone()
    };
    assert_eq!(series_label(&empty_pos), "Stormlight Archive");
}

#[wasm_bindgen_test]
fn looks_like_asin_validates_ten_alphanumerics() {
    assert!(looks_like_asin("B08G9PRS1K"));
    assert!(
        looks_like_asin("1234567890"),
        "all-digit ASINs exist (SKADI-T-0150)"
    );
    assert!(looks_like_asin("  B08G9PRS1K  "), "trims surrounding space");
    assert!(!looks_like_asin("B08G9PRS1"), "too short");
    assert!(!looks_like_asin("B08G9PRS1KK"), "too long");
    assert!(!looks_like_asin("B08G9-RS1K"), "non-alphanumeric");
    assert!(!looks_like_asin(""));
}

#[wasm_bindgen_test]
fn extract_asin_pulls_asin_from_pasted_text() {
    // A bare ASIN (B-prefixed or all-digit) passes through, upper-cased.
    assert_eq!(extract_asin("b08g9prs1k"), Some("B08G9PRS1K".into()));
    assert_eq!(extract_asin("1478916591"), Some("1478916591".into()));
    // The reported case: pasting the whole "Title [ASIN]" string (SKADI-T-0150).
    assert_eq!(
        extract_asin("A Little Hatred [1478916591]"),
        Some("1478916591".into())
    );
    assert_eq!(
        extract_asin("Project Hail Mary {asin-B08G9PRS1K}"),
        Some("B08G9PRS1K".into())
    );
    // No 10-char token → None.
    assert_eq!(extract_asin("Project Hail Mary (2021)"), None);
    assert_eq!(extract_asin(""), None);
}

#[wasm_bindgen_test]
fn confidence_class_maps_audiobook_match_confidence() {
    // ASIN-driven matching yields `high` (resolved) or `none` (paste an ASIN).
    assert_eq!(confidence_class("high"), "ok");
    assert_eq!(confidence_class("low"), "pending");
    assert_eq!(confidence_class("none"), "muted");
    assert_eq!(confidence_class(""), "muted");
}

// --- Activity history readability (SKADI-T-0140) ---

#[wasm_bindgen_test]
fn detail_summary_splits_failure_reason_from_payload() {
    // A download failure carries the magnet as the Debug payload: friendly
    // label inline, the long magnet tucked behind the expandable.
    let magnet = "magnet:?xt=urn:btih:abcdef0123456789&dn=Some.Book&tr=udp://t";
    let raw = format!("DownloadFailed({magnet:?})");
    let (summary, full) = detail_summary(&raw);
    assert_eq!(summary, "Download failed");
    assert_eq!(full.as_deref(), Some(magnet));

    // Import failure maps likewise.
    let (s, f) = detail_summary("ImportFailed(\"no parse match\")");
    assert_eq!(s, "Import failed");
    assert_eq!(f.as_deref(), Some("no parse match"));
}

#[wasm_bindgen_test]
fn detail_summary_handles_bare_variants_labels_and_other() {
    // Bare variant, no payload.
    assert_eq!(
        detail_summary("NoSuitableRelease"),
        ("No suitable release".to_string(), None)
    );
    // A plain quality name (imported event) passes through, no disclosure.
    assert_eq!(
        detail_summary("Bluray-1080p"),
        ("Bluray-1080p".to_string(), None)
    );
    // Empty detail yields empty summary.
    assert_eq!(detail_summary(""), (String::new(), None));
    // `Other` shows its (short) message inline rather than the word "Error".
    assert_eq!(
        detail_summary("Other(\"disk full\")"),
        ("disk full".to_string(), None)
    );
    // A long `Other` message is truncated inline and kept in full on expand.
    let long = "x".repeat(200);
    let (s, f) = detail_summary(&format!("Other({long:?})"));
    assert!(s.ends_with('…'));
    assert!(s.chars().count() < long.chars().count());
    assert_eq!(f.as_deref(), Some(long.as_str()));
}

#[wasm_bindgen_test]
fn run_lengths_collapses_adjacent_equal_items() {
    assert_eq!(run_lengths::<i32>(&[]), Vec::<usize>::new());
    assert_eq!(run_lengths(&["a", "a", "b", "a"]), vec![2, 1, 1]);
    assert_eq!(run_lengths(&["a", "b", "c"]), vec![1, 1, 1]);
    assert_eq!(run_lengths(&["a", "a", "a"]), vec![3]);
}

#[wasm_bindgen_test]
fn stage_group_folds_backend_stages_into_pipeline_order() {
    // The five operator-facing groups, in pipeline order.
    assert_eq!(
        STAGE_GROUPS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![
            "Searching",
            "Deciding",
            "Grabbing",
            "Downloading",
            "Importing"
        ]
    );
    // Every backend stage maps to the right group.
    assert_eq!(stage_group("running"), 0);
    assert_eq!(stage_group("searching"), 0);
    assert_eq!(stage_group("deciding"), 1);
    assert_eq!(stage_group("grabbing"), 2);
    assert_eq!(stage_group("snatching"), 2);
    assert_eq!(stage_group("downloading"), 3);
    assert_eq!(stage_group("importing"), 4);
    assert_eq!(stage_group("notifying"), 4);
    // An unknown stage is never dropped — it falls into the first group.
    assert_eq!(stage_group("wat"), 0);
}

#[wasm_bindgen_test]
fn file_resettable_matches_movies_semantics() {
    assert!(file_resettable("searching"));
    assert!(file_resettable("snatched"));
    assert!(file_resettable("downloading"));
    assert!(file_resettable("failed"));
    assert!(!file_resettable("missing"));
    assert!(!file_resettable("imported"));
    assert!(!file_resettable("cutoff"));
}

#[wasm_bindgen_test]
fn episode_label_formats_season_episode_absolute() {
    // Standard SxxEyy, zero-padded.
    assert_eq!(episode_label(Some(1), &[2], &[]), "S01E02");
    // Multi-episode files join with a dash.
    assert_eq!(episode_label(Some(2), &[5, 6], &[]), "S02E05-E06");
    // Season-only fallback when no episode numbers parsed.
    assert_eq!(episode_label(Some(3), &[], &[]), "S03");
    // No season → absolute numbering.
    assert_eq!(episode_label(None, &[], &[123]), "#123");
    // Neither known.
    assert_eq!(episode_label(None, &[], &[]), "—");
}

fn cand(season: Option<u16>, episodes: Vec<u16>) -> TvScanCandidate {
    TvScanCandidate {
        path: "/lib/x.mkv".into(),
        display_name: "x.mkv".into(),
        series_title: Some("Show".into()),
        season,
        episodes,
        absolute: vec![],
        air_date: None,
        quality_id: None,
        quality_name: None,
        folder: None,
        nfo_tvdb_id: None,
        nfo_title: None,
        nfo_year: None,
        nfo_overview: None,
        nfo_genres: vec![],
        nfo_status: None,
        nfo_network: None,
        nfo_rating: None,
    }
}

#[wasm_bindgen_test]
fn auto_episode_maps_against_structure() {
    let structure = TvSeriesStructure {
        seasons: vec![TvStructSeason {
            number: 2,
            episode_count: 13,
        }],
        episodes: vec![
            TvStructEpisode {
                season: 2,
                number: 1,
                title: Some("The Keys".into()),
            },
            TvStructEpisode {
                season: 2,
                number: 2,
                title: None,
            },
        ],
    };
    // Parsed (season, first episode) that exists in the tree → maps there.
    assert_eq!(
        auto_episode(&cand(Some(2), vec![1]), Some(&structure)),
        Some((2, 1))
    );
    // Multi-episode files map to their first episode.
    assert_eq!(
        auto_episode(&cand(Some(2), vec![2, 3]), Some(&structure)),
        Some((2, 2))
    );
    // An episode the tree doesn't contain → unmapped.
    assert_eq!(
        auto_episode(&cand(Some(2), vec![99]), Some(&structure)),
        None
    );
    // No parsed season or no episode numbers → unmapped.
    assert_eq!(auto_episode(&cand(None, vec![1]), Some(&structure)), None);
    assert_eq!(auto_episode(&cand(Some(2), vec![]), Some(&structure)), None);
    // Before the structure has loaded we trust the parse (don't flag unmapped).
    assert_eq!(auto_episode(&cand(Some(5), vec![7]), None), Some((5, 7)));

    // Multi-episode files map to EVERY episode the structure contains
    // (SKADI-T-0325) — the backend places both, the UI must agree.
    assert_eq!(
        auto_episodes(&cand(Some(2), vec![1, 2]), Some(&structure)),
        vec![(2, 1), (2, 2)]
    );
    // Nonexistent episodes are filtered out, existing ones kept.
    assert_eq!(
        auto_episodes(&cand(Some(2), vec![2, 99]), Some(&structure)),
        vec![(2, 2)]
    );
}

fn cand_at(path: &str, season: Option<u16>, episodes: Vec<u16>) -> TvScanCandidate {
    let mut c = cand(season, episodes);
    c.path = path.to_string();
    c
}

#[wasm_bindgen_test]
fn series_lib_status_ignores_missing_specials() {
    use skadi_web::tv::series_lib_status;
    let ep = |season: u16, number: u16, status: serde_json::Value| skadi_web::api::Episode {
        id: format!("e{season}-{number}"),
        season,
        number,
        absolute_number: None,
        title: None,
        air_date: None,
        monitored: true,
        status,
        media_info: None,
    };
    let series = |episodes: Vec<skadi_web::api::Episode>| skadi_web::api::Series {
        content_rating: None,
        genres: Vec::new(),
        id: "s".into(),
        external_ids: Default::default(),
        title: "t".into(),
        year: None,
        overview: None,
        status: None,
        network: None,
        series_type: None,
        monitored: true,
        poster_url: None,
        backdrop_url: None,
        seasons: vec![],
        episodes,
    };
    let imported = || json!({"Imported": {"path": "x"}});

    // All regular episodes owned + a MISSING special → still owned
    // (specials don't count against completeness, SKADI-T-0329).
    let s = series(vec![
        ep(1, 1, imported()),
        ep(1, 2, imported()),
        ep(0, 1, json!("Missing")),
    ]);
    assert_eq!(series_lib_status(&s), "owned");

    // A missing REGULAR episode → wanted.
    let s = series(vec![ep(1, 1, imported()), ep(1, 2, json!("Missing"))]);
    assert_eq!(series_lib_status(&s), "wanted");

    // A downloading special still surfaces activity.
    let s = series(vec![
        ep(1, 1, imported()),
        ep(
            0,
            1,
            json!({"Downloading": {"release": "r", "progress": 0.5}}),
        ),
    ]);
    assert_eq!(series_lib_status(&s), "downloading");

    // Specials-ONLY show is judged by its specials.
    let s = series(vec![ep(0, 1, imported())]);
    assert_eq!(series_lib_status(&s), "owned");
    let s = series(vec![ep(0, 1, json!("Missing"))]);
    assert_eq!(series_lib_status(&s), "wanted");
}

#[wasm_bindgen_test]
fn extras_are_classified_but_parseable_episodes_never_are() {
    use skadi_web::tv_import::is_extra;
    let mk = |path: &str, folder: &str, season: Option<u16>, eps: Vec<u16>| {
        let mut c = cand(season, eps);
        c.path = path.to_string();
        c.display_name = path.rsplit('/').next().unwrap().to_string();
        c.folder = Some(folder.to_string());
        c
    };
    // Featurette under an extras dir, no parse → extra.
    assert!(is_extra(&mk(
        "/tv/black-sails/Season 1 + Extras/Featurettes/A Place In History.mkv",
        "black-sails",
        None,
        vec![]
    )));
    // Extras marker in the filename alone → extra.
    assert!(is_extra(&mk(
        "/tv/tftc/season_07/tftc.7.extras.dvdrip.avi",
        "tftc",
        None,
        vec![]
    )));
    // A REAL episode inside a "+ Extras" folder: has SxxEyy → never an extra.
    assert!(!is_extra(&mk(
        "/tv/black-sails/Season 1 + Extras/Black Sails S01E01.mkv",
        "black-sails",
        Some(1),
        vec![1]
    )));
    // Plain unparseable file with no extras marker anywhere → not an extra
    // (needs the manual picker, still counts as "to fix").
    assert!(!is_extra(&mk(
        "/tv/show/season_01/Some Oddly Named Episode.mkv",
        "show",
        None,
        vec![]
    )));
    // A bare season token doesn't identify an episode: season-parsed trailers
    // and recaps are still extras.
    assert!(is_extra(&mk(
        "/tv/black-sails/S2 stuff/Trailer - Black Sails Season 2.mkv",
        "black-sails",
        Some(2),
        vec![]
    )));
    assert!(is_extra(&mk(
        "/tv/black-sails/S2 stuff/Season 2 Recap.mkv",
        "black-sails",
        Some(2),
        vec![]
    )));
}

#[wasm_bindgen_test]
fn collision_losers_first_claim_wins() {
    use std::collections::HashMap;
    let ov = HashMap::new();

    // Classic multi-part serial: four files all parse to S03E04 — the first
    // claims it, the rest are losers routed to the manual picker (SKADI-T-0325).
    let cands = vec![
        cand_at("/tv/a1.avi", Some(3), vec![4]),
        cand_at("/tv/a2.avi", Some(3), vec![4]),
        cand_at("/tv/a3.avi", Some(3), vec![4]),
        cand_at("/tv/b1.avi", Some(3), vec![5]),
    ];
    let losers = collision_losers(&cands, &ov, None);
    assert!(!losers.contains("/tv/a1.avi"), "first claim wins");
    assert!(losers.contains("/tv/a2.avi") && losers.contains("/tv/a3.avi"));
    assert!(!losers.contains("/tv/b1.avi"), "distinct episode is clean");

    // A multi-episode file blocks a later single that overlaps either episode.
    let cands = vec![
        cand_at("/tv/m.mkv", Some(1), vec![1, 2]),
        cand_at("/tv/e2.mkv", Some(1), vec![2]),
    ];
    let losers = collision_losers(&cands, &ov, None);
    assert!(losers.contains("/tv/e2.mkv"));

    // A manual override out of the way un-loses a duplicate.
    let mut ov2 = HashMap::new();
    ov2.insert("/tv/a2.avi".to_string(), (Some(0u16), Some(7u16)));
    let cands = vec![
        cand_at("/tv/a1.avi", Some(3), vec![4]),
        cand_at("/tv/a2.avi", Some(3), vec![4]),
    ];
    let losers = collision_losers(&cands, &ov2, None);
    assert!(losers.is_empty(), "override resolves the collision");

    // Unmapped files (no parse) never collide.
    let cands = vec![
        cand_at("/tv/x.mkv", None, vec![]),
        cand_at("/tv/y.mkv", None, vec![]),
    ];
    assert!(collision_losers(&cands, &ov, None).is_empty());
}

#[wasm_bindgen_test]
fn episode_option_and_season_labels_format() {
    assert_eq!(episode_option_label(2, Some("Primary")), "E02 · Primary");
    assert_eq!(episode_option_label(13, None), "E13");
    assert_eq!(episode_option_label(1, Some("")), "E01");
    assert_eq!(season_label(0), "Specials");
    assert_eq!(season_label(3), "Season 3");
}

#[wasm_bindgen_test]
fn player_clock_and_chapter_navigation() {
    use skadi_web::api::Chapter;
    // Clock formatting.
    assert_eq!(fmt_clock(0.0), "0:00");
    assert_eq!(fmt_clock(75.4), "1:15");
    assert_eq!(fmt_clock(3661.0), "1:01:01");
    assert_eq!(fmt_clock(-5.0), "0:00");

    let ch = |i: usize, s: f64, e: f64| Chapter {
        index: i,
        title: format!("Ch {i}"),
        start_s: s,
        end_s: e,
    };
    let chapters = vec![ch(0, 0.0, 10.0), ch(1, 10.0, 30.0), ch(2, 30.0, 60.0)];

    // Which chapter contains t.
    assert_eq!(chapter_at(&chapters, 0.0), Some(0));
    assert_eq!(chapter_at(&chapters, 10.0), Some(1));
    assert_eq!(chapter_at(&chapters, 59.9), Some(2));
    assert_eq!(chapter_at(&[], 5.0), None);

    // Prev: >3s into a chapter goes to ITS start; near the start goes back one.
    assert_eq!(prev_chapter_target(&chapters, 20.0), Some(10.0));
    assert_eq!(prev_chapter_target(&chapters, 11.0), Some(0.0));
    assert_eq!(prev_chapter_target(&chapters, 1.0), Some(0.0)); // first chapter clamps

    // Next: start of the following chapter; None in the last.
    assert_eq!(next_chapter_target(&chapters, 5.0), Some(10.0));
    assert_eq!(next_chapter_target(&chapters, 45.0), None);

    // Speed cycle wraps.
    assert_eq!(next_speed(1.0), 1.25);
    assert_eq!(next_speed(3.0), 0.75);
    assert_eq!(next_speed(999.0), 1.25); // unknown → sane default step
}

#[wasm_bindgen_test]
async fn offline_index_round_trip_in_opfs() {
    use skadi_web::offline::{OfflineBook, delete_book, index_entry, read_index, save_book};
    let meta = OfflineBook {
        book_id: "b1".into(),
        file_id: "f-test-rt".into(),
        title: "Test Book".into(),
        authors: vec!["A. Uthor".into()],
        narrators: vec![],
        series_name: Some("Testiverse".into()),
        series_position: Some("2".into()),
        cover_url: None,
        size_bytes: 0.0,
        chapters: vec![],
    };
    let blob =
        web_sys::Blob::new_with_str_sequence(&js_sys::Array::of1(&"fake audio".into())).unwrap();
    save_book(meta.clone(), &blob).await.expect("save");
    let got = index_entry("f-test-rt").await.expect("indexed");
    assert_eq!(got.title, "Test Book");
    assert_eq!(got.series_name.as_deref(), Some("Testiverse"));
    let audio = skadi_web::offline::audio_file("f-test-rt")
        .await
        .expect("audio stored");
    assert!(audio.size() > 0.0);
    delete_book("f-test-rt").await.expect("delete");
    assert!(index_entry("f-test-rt").await.is_none());
    assert!(skadi_web::offline::audio_file("f-test-rt").await.is_none());
    let _ = read_index().await;
}

// ---------------------------------------------------------------------------
// Active quality profile resolution (SKADI-T-0379).
//
// The daemon enforces exactly ONE profile and picks it as the first row of
// `list_settings("profiles")`, which the store orders by `id ASC` — so the
// winner is whichever profile holds the lowest UUID, not one the operator
// chose. A stale SD-only profile sorting first silently rejected every HD
// release in production (2026-09-01). These pin the rule the UI reports.
// ---------------------------------------------------------------------------

fn profile_row(id: &str, name: serde_json::Value) -> skadi_web::api::Setting {
    skadi_web::api::Setting {
        id: id.into(),
        body: json!({ "name": name }),
        has_secret: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

#[wasm_bindgen_test]
fn active_profile_is_the_first_row_not_the_nicest_name() {
    let rows = vec![
        profile_row("55573fc8-0000-0000-0000-000000000000", "SD".into()),
        profile_row("83f3803b-0000-0000-0000-000000000000", "Any".into()),
    ];
    // Lowest-UUID row wins even though "Any" is the permissive one an operator
    // would assume is in force.
    assert_eq!(active_profile_name(&rows), "SD");
}

#[wasm_bindgen_test]
fn active_profile_falls_back_to_the_builtin_default() {
    assert_eq!(active_profile_name(&[]), "built-in default");
}

#[wasm_bindgen_test]
fn active_profile_names_an_unnamed_row() {
    let rows = vec![profile_row("a", serde_json::Value::Null)];
    assert_eq!(active_profile_name(&rows), "(unnamed)");
    // An empty string is as useless as a missing one.
    let rows = vec![profile_row("a", "".into())];
    assert_eq!(active_profile_name(&rows), "(unnamed)");
}

#[wasm_bindgen_test]
fn profile_is_active_only_for_the_first_row() {
    let rows = vec![
        profile_row("aaa", "SD".into()),
        profile_row("bbb", "Any".into()),
    ];
    assert!(profile_is_active(&rows, Some("aaa")));
    assert!(!profile_is_active(&rows, Some("bbb")));
    // An unsaved (brand-new) profile is never the enforced one.
    assert!(!profile_is_active(&rows, None));
    assert!(!profile_is_active(&[], Some("aaa")));
}

// ---------------------------------------------------------------------------
// Structured trace detail (SKADI-T-0380).
//
// The hunter writes JSON into a trace event's `detail`: what a search asked for,
// and what each decide gate rejected. Older rows (and history failures) carry a
// Rust-Debug payload instead, so the parser must take only JSON and leave the
// rest to `detail_summary`.
// ---------------------------------------------------------------------------

fn fact<'a>(facts: &'a [skadi_web::activity::DetailFact], label: &str) -> Option<&'a str> {
    facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
}

#[wasm_bindgen_test]
fn detail_facts_reads_a_search_payload() {
    let d = r#"{"searched":["101 Dalmatians","101.Dalmatians"],"year":1996,
                "ids":{"imdb":"tt0115433","tmdb":"11674"},"season":null,"episode":null}"#;
    let facts = detail_facts(d);
    assert_eq!(
        fact(&facts, "searched"),
        Some("101 Dalmatians · 101.Dalmatians")
    );
    assert_eq!(fact(&facts, "year"), Some("1996"));
    let ids = fact(&facts, "ids").unwrap();
    assert!(
        ids.contains("imdb:tt0115433") && ids.contains("tmdb:11674"),
        "{ids}"
    );
    // A movie has no season scope — the row is omitted, not blank.
    assert!(fact(&facts, "scope").is_none());
}

#[wasm_bindgen_test]
fn detail_facts_formats_tv_scope() {
    let ep = detail_facts(r#"{"searched":["Lioness"],"season":2,"episode":6}"#);
    assert_eq!(fact(&ep, "scope"), Some("S02E06"));
    let pack = detail_facts(r#"{"searched":["Lioness"],"season":2,"episode":null}"#);
    assert_eq!(fact(&pack, "scope"), Some("S02 (season pack)"));
}

#[wasm_bindgen_test]
fn detail_facts_orders_rejections_busiest_first() {
    let d = r#"{"considered":144,"rejected":{"size":12,"quality":87,"relevance":45}}"#;
    let facts = detail_facts(d);
    assert_eq!(fact(&facts, "considered"), Some("144"));
    // Total leads, then the gates worth acting on, busiest first.
    assert_eq!(
        fact(&facts, "rejected"),
        Some("144 — 87 quality, 45 relevance, 12 size")
    );
}

#[wasm_bindgen_test]
fn detail_facts_flags_a_weak_title_match() {
    // The "101 Dalmatians" → "101 Dalmatians II" shape: cleared the gate, but
    // only just. Must be marked so it stands out in the log.
    let weak = detail_facts(r#"{"considered":181,"rejected":{},"chosen_relevance":0.42}"#);
    let m = weak.iter().find(|f| f.label == "title match").unwrap();
    assert_eq!(m.value, "0.42 — weak match");
    assert!(m.warn, "a weak match must render with emphasis");

    // A confident match is reported without the warning.
    let strong = detail_facts(r#"{"considered":10,"rejected":{},"chosen_relevance":1.0}"#);
    let m = strong.iter().find(|f| f.label == "title match").unwrap();
    assert_eq!(m.value, "1.00");
    assert!(!m.warn);
}

#[wasm_bindgen_test]
fn detail_facts_ignores_non_json_and_malformed_payloads() {
    // Legacy Debug payloads belong to `detail_summary`, not here.
    assert!(detail_facts("DownloadFailed(\"magnet:?xt=urn:btih:abc\")").is_empty());
    assert!(detail_facts("Bluray-1080p").is_empty());
    assert!(detail_facts("").is_empty());
    // Truncated JSON must degrade to nothing, never panic — a broken diagnostic
    // must not be louder than the thing it is diagnosing.
    assert!(detail_facts(r#"{"considered":144,"rejected":{"#).is_empty());
    // An empty rejection map contributes no row.
    assert!(detail_facts(r#"{"rejected":{}}"#).is_empty());
}

// ---------------------------------------------------------------------------
// Per-item acquisition diagnosis (SKADI-T-0381).
//
// Turns one item's trace rows into "here is what we tried, which gate ate the
// candidates, and what to do" — replacing a hand-written trace_events query.
// ---------------------------------------------------------------------------

fn trace(event: &str, at: &str, message: &str, detail: Option<&str>) -> skadi_web::api::TraceRow {
    skadi_web::api::TraceRow {
        id: format!("t-{at}"),
        at: at.into(),
        run_id: None,
        kind: "movie".into(),
        acquirable_ref: "ed-1".into(),
        stage: "deciding".into(),
        event: event.into(),
        message: message.into(),
        detail: detail.map(String::from),
    }
}

#[wasm_bindgen_test]
fn diagnose_reads_the_latest_decision_and_ranks_gates() {
    // Newest first, as the API returns them.
    let rows = vec![
        trace(
            "no_release",
            "2026-09-01T12:52:34Z",
            "no suitable release — 144 rejected, mostly quality (87)",
            Some(r#"{"considered":144,"rejected":{"quality":87,"relevance":45,"size":12}}"#),
        ),
        trace(
            "candidates_found",
            "2026-09-01T12:52:33Z",
            "found 144 candidates",
            None,
        ),
        trace(
            "no_release",
            "2026-09-01T11:46:25Z",
            "no suitable release",
            Some(r#"{"considered":120,"rejected":{"quality":120}}"#),
        ),
    ];
    let d = diagnose(&rows);
    assert_eq!(d.considered, 144);
    assert_eq!(d.rejected_total(), 144);
    // Busiest gate leads, and carries its share.
    let (gate, n, share) = d.top_gate().unwrap();
    assert_eq!((gate, n), ("quality", 87));
    assert!((share - 0.604).abs() < 0.01, "share was {share}");
    // Both traced attempts came up empty.
    assert_eq!(d.failed_attempts, 2);
    assert_eq!(d.last_attempt.as_deref(), Some("2026-09-01T12:52:34Z"));
    // Guidance matches the busiest gate.
    assert_eq!(d.remedy, remedy_for("quality"));
    assert!(d.remedy.unwrap().contains("cutoff"));
    // Older attempts are counted, never merged into the breakdown — mixing
    // tallies from different settings would describe a state that never existed.
    assert_eq!(d.rejected.iter().map(|(_, n)| n).sum::<usize>(), 144);
}

#[wasm_bindgen_test]
fn diagnose_extracts_the_chosen_title_and_weak_relevance() {
    let rows = vec![trace(
        "decision",
        "2026-09-01T12:52:34Z",
        "chose \"101 Dalmatians II: Patch's London Adventure (2003) 1080p\" of 181 candidates — weak title match (0.42)",
        Some(r#"{"considered":181,"rejected":{},"chosen_relevance":0.42}"#),
    )];
    let d = diagnose(&rows);
    assert_eq!(
        d.chosen.as_deref(),
        Some("101 Dalmatians II: Patch's London Adventure (2003) 1080p")
    );
    assert_eq!(d.chosen_relevance, Some(0.42));
    // A grab happened, so nothing failed.
    assert_eq!(d.failed_attempts, 0);
    assert!(!d.is_empty());
}

#[wasm_bindgen_test]
fn diagnose_is_empty_without_usable_traces() {
    // No traces at all — the panel must render nothing rather than an empty shell.
    assert!(diagnose(&[]).is_empty());
    // Only unrelated events.
    let rows = vec![trace(
        "snatched",
        "2026-09-01T12:00:00Z",
        "sent to worker",
        None,
    )];
    assert!(diagnose(&rows).is_empty());
    // A decision with no detail payload still yields the title, so it is NOT empty.
    let rows = vec![trace(
        "decision",
        "2026-09-01T12:00:00Z",
        "chose \"Some.Release.1080p\" of 3 candidates",
        None,
    )];
    let d = diagnose(&rows);
    assert!(!d.is_empty());
    assert_eq!(d.chosen.as_deref(), Some("Some.Release.1080p"));
    assert_eq!(d.considered, 0, "no payload ⇒ no count to report");
}

#[wasm_bindgen_test]
fn diagnose_survives_a_malformed_payload() {
    let rows = vec![trace(
        "no_release",
        "2026-09-01T12:00:00Z",
        "no suitable release",
        Some("{not json"),
    )];
    // Degrades to the attempt count rather than panicking.
    let d = diagnose(&rows);
    assert_eq!(d.failed_attempts, 1);
    assert_eq!(d.considered, 0);
    assert!(d.rejected.is_empty());
    assert_eq!(d.remedy, None);
}

#[wasm_bindgen_test]
fn remedy_is_specific_per_gate_and_absent_for_unknown_ones() {
    // Each gate points at a different knob — generic advice would be useless.
    assert!(remedy_for("quality").unwrap().contains("cutoff"));
    assert!(remedy_for("seeders").unwrap().contains("seeders"));
    assert!(remedy_for("blocklisted").unwrap().contains("blocklist"));
    assert!(remedy_for("relevance").unwrap().contains("manual search"));
    assert!(remedy_for("category").unwrap().contains("categor"));
    // A gate added later must produce no advice rather than wrong advice.
    assert_eq!(remedy_for("some-future-gate"), None);
}

/// SKADI-T-0474: a rotated `api_token` leaves the open tab holding the stale one
/// injected at page load. Without this flag the UI showed whichever raw error
/// surfaced first, with nothing saying why or what to do.
#[wasm_bindgen_test]
fn auth_expired_flag_round_trips() {
    skadi_web::api::clear_auth_expired();
    assert!(!skadi_web::api::auth_expired());
    skadi_web::api::note_auth_expired_for_test();
    assert!(
        skadi_web::api::auth_expired(),
        "a 401 must be visible to the shell"
    );
    // Cleared on reload / re-auth so the banner does not persist forever.
    skadi_web::api::clear_auth_expired();
    assert!(!skadi_web::api::auth_expired());
}

// --- config registry form (SKADI-T-0540) -----------------------------------

fn cfg_key(k: &str, kind: &str, source: Option<&str>) -> skadi_web::api::ConfigKey {
    serde_json::from_value(serde_json::json!({
        "key": k,
        "kind": kind,
        "default": "",
        "isSet": source.is_some(),
        "editable": true,
        "source": source,
    }))
    .expect("ConfigKey fixture")
}

/// The prefix is the grouping, and it needs no extra metadata on the key.
#[wasm_bindgen_test]
fn config_groups_by_prefix_with_the_tuned_groups_first() {
    let groups = skadi_web::config::group_config(vec![
        cfg_key("http.proxy_url", "string", None),
        cfg_key("import.min_free_mb", "u64", None),
        cfg_key("library.root", "path", None),
        cfg_key("import.placement", "string", None),
        // No dot at all — must not be dropped, and must not invent a group.
        cfg_key("sweep_max_concurrent", "u16", None),
    ]);
    let names: Vec<&str> = groups.iter().map(|(g, _)| g.as_str()).collect();
    assert_eq!(names, vec!["library", "import", "http", "general"]);

    // Keys inside a group are ordered, so the form does not reshuffle between
    // loads just because the API returned a different order.
    let import = &groups[1].1;
    assert_eq!(import[0].key, "import.min_free_mb");
    assert_eq!(import[1].key, "import.placement");
}

/// The row label drops the prefix, so a section headed "import" does not repeat
/// "import." on every line.
#[wasm_bindgen_test]
fn a_config_row_label_drops_the_group_prefix() {
    assert_eq!(
        cfg_key("import.placement", "string", None).leaf(),
        "placement"
    );
    // A key with no prefix keeps its whole name rather than losing it.
    assert_eq!(
        cfg_key("sweep_max_concurrent", "u16", None).leaf(),
        "sweep_max_concurrent"
    );
}

/// Controls are typed from `kind`, not all text boxes.
#[wasm_bindgen_test]
fn config_controls_are_typed_from_the_registry_kind() {
    assert_eq!(skadi_web::config::input_kind("bool"), "checkbox");
    assert_eq!(skadi_web::config::input_kind("u16"), "number");
    assert_eq!(skadi_web::config::input_kind("u64"), "number");
    assert_eq!(skadi_web::config::input_kind("path"), "text");
    // An unrecognised kind falls back to text rather than vanishing.
    assert_eq!(skadi_web::config::input_kind("something-new"), "text");
}

/// The criterion most likely to be skipped, and the one with a real operator
/// consequence: an env-sourced value is rewritten by the seeder on the next
/// boot, so a runtime write to it does not survive a restart. A form that lets
/// someone edit it without saying so is worse than no form — it looks like it
/// worked.
#[wasm_bindgen_test]
fn an_env_sourced_config_key_is_flagged_as_not_surviving_a_restart() {
    assert!(cfg_key("http.proxy_url", "string", Some("env")).overwritten_by_env());
    assert!(!cfg_key("http.proxy_url", "string", Some("runtime")).overwritten_by_env());
    // Unset is not env-backed — nothing will overwrite a value that isn't there.
    assert!(!cfg_key("http.proxy_url", "string", None).overwritten_by_env());
}

// ---------------------------------------------------------------------------
// Activity: trace rows folded into runs (SKADI-T-0582)
// ---------------------------------------------------------------------------

fn trace_row(
    id: &str,
    run: Option<&str>,
    at: &str,
    item: &str,
    stage: &str,
    event: &str,
    msg: &str,
    detail: Option<&str>,
) -> TraceRow {
    TraceRow {
        id: id.into(),
        at: at.into(),
        run_id: run.map(str::to_string),
        kind: "tv".into(),
        acquirable_ref: item.into(),
        stage: stage.into(),
        event: event.into(),
        message: msg.into(),
        detail: detail.map(str::to_string),
    }
}

#[wasm_bindgen_test]
fn parse_found_reads_the_candidate_count() {
    assert_eq!(parse_found("found 576 candidates"), Some(576));
    assert_eq!(parse_found("no suitable release — 31 rejected"), None);
}

/// Newest-first rows for one run collapse to one summary whose outcome is the
/// newest row and whose candidate count comes from the search row.
#[wasm_bindgen_test]
fn fold_runs_makes_one_summary_per_run_with_the_latest_outcome() {
    let rows = vec![
        trace_row(
            "2",
            Some("r1"),
            "2026-09-16T00:07:19Z",
            "ep1",
            "deciding",
            "decision",
            "chose \"X\" of 576 candidates",
            Some("{\"considered\":576}"),
        ),
        trace_row(
            "1",
            Some("r1"),
            "2026-09-16T00:07:19Z",
            "ep1",
            "searching",
            "candidates_found",
            "found 576 candidates",
            Some("{\"searched\":[\"The Ark\"]}"),
        ),
        trace_row(
            "0",
            None,
            "2026-09-15T23:00:00Z",
            "ep0",
            "importing",
            "imported",
            "imported x.mkv",
            None,
        ),
    ];
    let runs = fold_runs(&rows);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].event, "decision");
    assert_eq!(runs[0].candidates, Some(576));
    assert_eq!(runs[0].detail.as_deref(), Some("{\"considered\":576}"));
    assert_eq!(
        runs[0].search_detail.as_deref(),
        Some("{\"searched\":[\"The Ark\"]}")
    );
    // A row without a run id stands alone, keyed by its own id.
    assert_eq!(runs[1].key, "0");
    assert_eq!(runs[1].event, "imported");
}

#[wasm_bindgen_test]
fn series_key_splits_episode_and_season_labels_only() {
    assert_eq!(
        series_key("Um, Actually S07E12"),
        ("Um, Actually".into(), Some("S07E12".into()))
    );
    assert_eq!(
        series_key("Foo Season 3"),
        ("Foo".into(), Some("Season 3".into()))
    );
    assert_eq!(
        series_key("Light Perpetual"),
        ("Light Perpetual".into(), None)
    );
    assert_eq!(series_key("2012"), ("2012".into(), None));
}

/// The case the fold exists for: a sweep walking a season with the same
/// outcome is one line; a different outcome or a different show breaks it.
#[wasm_bindgen_test]
fn collapse_runs_folds_a_season_walk_with_one_outcome() {
    let mk = |id: &str, item: &str, event: &str| {
        trace_row(
            id,
            Some(id),
            "2026-09-16T00:00:00Z",
            item,
            "deciding",
            event,
            "no suitable release",
            None,
        )
    };
    let rows = vec![
        mk("a", "ep12", "no_release"),
        mk("b", "ep08", "no_release"),
        mk("c", "ep07", "no_release"),
        mk("d", "ep06", "decision"),
        mk("e", "other", "no_release"),
    ];
    let names = |r: &str| match r {
        "ep12" => "Um, Actually S07E12".to_string(),
        "ep08" => "Um, Actually S07E08".to_string(),
        "ep07" => "Um, Actually S07E07".to_string(),
        "ep06" => "Um, Actually S07E06".to_string(),
        other => other.to_string(),
    };
    let groups = collapse_runs(fold_runs(&rows), names);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].label, "Um, Actually");
    assert_eq!(groups[0].items, vec!["S07E12", "S07E08", "S07E07"]);
    assert_eq!(groups[1].items, vec!["S07E06"]);
    assert_eq!(groups[1].head.event, "decision");
    assert_eq!(groups[2].label, "other");
    assert_eq!(groups[2].items, vec!["other"]);
}

#[wasm_bindgen_test]
fn log_filters_select_by_event_family() {
    assert!(filter_allows("all", "candidates_found"));
    assert!(filter_allows("decisions", "no_release"));
    assert!(!filter_allows("decisions", "snatched"));
    assert!(filter_allows("failures", "download_failed"));
    assert!(filter_allows("failures", "import_failed"));
    assert!(!filter_allows("failures", "imported"));
    assert!(filter_allows("imports", "imported"));
    assert!(filter_allows("imports", "snatched"));
    assert_eq!(event_label("no_release"), "no release");
}

#[wasm_bindgen_test]
fn ago_reads_as_a_person_would_say_it() {
    let t = js_sys::Date::parse("2026-09-16T00:00:00Z");
    assert_eq!(ago("2026-09-16T00:00:00Z", t + 30_000.0), "just now");
    assert_eq!(ago("2026-09-16T00:00:00Z", t + 5.0 * 60_000.0), "5 min ago");
    assert_eq!(
        ago("2026-09-16T00:00:00Z", t + 3.0 * 3_600_000.0),
        "3 h ago"
    );
    // Older than a day, or unparseable, or in the future: the date itself.
    assert_eq!(
        ago("2026-09-16T00:00:00Z", t + 2.0 * 86_400_000.0),
        "2026-09-16 00:00"
    );
    assert_eq!(
        ago("2026-09-16T00:00:00Z", t - 60_000.0),
        "2026-09-16 00:00"
    );
    assert_eq!(ago("garbage", t), " ");
}

#[wasm_bindgen_test]
fn parse_handed_reads_the_release_title_out_of_a_snatch_message() {
    assert_eq!(
        parse_handed(
            "The Sinner S01E08 — handed \"The.Sinner.S01E08.REPACK.720p.HEVC.x265-MeGusta\" to b22bafbb"
        ),
        Some("The.Sinner.S01E08.REPACK.720p.HEVC.x265-MeGusta".into())
    );
    assert_eq!(parse_handed("found 3 candidates"), None);
}

#[wasm_bindgen_test]
fn norm_title_makes_a_library_label_a_prefix_of_its_release_name() {
    let label = norm_title("Um, Actually... S03E12");
    let release = norm_title("Um Actually S03E12 720p WEB-DL AAC2 0 H 264-NTb");
    assert_eq!(label, "um actually s03e12");
    assert!(release.starts_with(&label));
    assert_eq!(
        norm_title("The.Sinner.S01E08.REPACK"),
        "the sinner s01e08 repack"
    );
}

// ---------------------------------------------------------------------------
// Watch: browser playability (SKADI-T-0585)
// ---------------------------------------------------------------------------

fn media(container: &str, video: &str, audio: &[&str]) -> MediaInfo {
    MediaInfo {
        container: Some(container.into()),
        video: Some(MediaVideo {
            codec: Some(video.into()),
            width: Some(1920),
            height: Some(1080),
        }),
        audio: None,
        audio_tracks: audio
            .iter()
            .map(|c| MediaAudio {
                codec: Some(c.to_string()),
                channels: Some(2),
            })
            .collect(),
        duration_secs: Some(6000.0),
    }
}

#[wasm_bindgen_test]
fn a_file_every_browser_decodes_is_ok() {
    let mi = media("mp4", "h264", &["aac"]);
    assert_eq!(playability(Some(&mi), |_| true), Playability::Ok);
}

/// The Chrome case: Matroska opens, H.264 decodes, AC-3 does not — picture
/// with no sound, and the page must say so rather than let it happen.
#[wasm_bindgen_test]
fn ac3_only_audio_is_a_silent_warning_where_the_browser_lacks_it() {
    let mi = media("mkv", "h264", &["ac3"]);
    let chrome = |m: &str| !m.contains("ac-3") && !m.contains("ec-3");
    match playability(Some(&mi), chrome) {
        Playability::Warn(msg) => {
            assert!(msg.contains("ac3"), "{msg}");
            assert!(msg.contains("no sound"), "{msg}");
        }
        other => panic!("expected Warn, got {other:?}"),
    }
    // A second, decodable track rescues it.
    let mi = media("mkv", "h264", &["ac3", "aac"]);
    assert_eq!(playability(Some(&mi), chrome), Playability::Ok);
}

/// The Safari case: the codecs are fine, the container is not.
#[wasm_bindgen_test]
fn a_container_the_browser_wont_open_is_blocked() {
    let mi = media("mkv", "h264", &["aac"]);
    let safari = |m: &str| !m.starts_with("video/webm");
    match playability(Some(&mi), safari) {
        Playability::Blocked(msg) => assert!(msg.contains("mkv"), "{msg}"),
        other => panic!("expected Blocked, got {other:?}"),
    }
    // And a container no browser opens is blocked before any probe.
    let mi = media("avi", "h264", &["mp3"]);
    assert!(matches!(
        playability(Some(&mi), |_| true),
        Playability::Blocked(_)
    ));
}

#[wasm_bindgen_test]
fn dts_only_audio_warns_and_an_unscanned_file_warns() {
    let mi = media("mkv", "hevc", &["dts"]);
    assert!(matches!(
        playability(Some(&mi), |_| true),
        Playability::Warn(_)
    ));
    assert!(matches!(playability(None, |_| true), Playability::Warn(_)));
}

#[wasm_bindgen_test]
fn mime_probes_cover_the_library_codecs() {
    assert!(video_mime("hevc").is_some());
    assert!(video_mime("mpeg4").is_none());
    assert!(audio_mime("eac3").is_some());
    assert!(audio_mime("truehd").is_none());
    assert_eq!(container_mime("m4v"), Some("video/mp4"));
    assert_eq!(container_mime("ts"), None);
}

#[wasm_bindgen_test]
fn resume_refuses_the_first_seconds_and_the_last_minute() {
    assert_eq!(resume_at(Some(4.0), 6000.0), None);
    assert_eq!(resume_at(Some(120.0), 6000.0), Some(120.0));
    assert_eq!(resume_at(Some(5970.0), 6000.0), None);
    assert_eq!(resume_at(None, 6000.0), None);
}

/// Next / previous follow broadcast order, skip specials and anything not
/// on disk, and cross season boundaries.
#[wasm_bindgen_test]
fn next_and_prev_episode_follow_broadcast_order_over_what_is_on_disk() {
    let ep = |season: u16, number: u16, imported: bool| skadi_web::api::Episode {
        id: format!("e{season}-{number}"),
        season,
        number,
        absolute_number: None,
        title: None,
        air_date: None,
        monitored: true,
        status: if imported {
            json!({"Imported": {}})
        } else {
            json!("Missing")
        },
        media_info: None,
    };
    let eps = vec![
        ep(0, 1, true),
        ep(1, 1, true),
        ep(1, 2, false),
        ep(1, 3, true),
        ep(2, 1, true),
    ];
    assert_eq!(
        next_episode(&eps, 1, 1).map(|e| e.id.as_str()),
        Some("e1-3")
    );
    assert_eq!(
        next_episode(&eps, 1, 3).map(|e| e.id.as_str()),
        Some("e2-1")
    );
    assert!(next_episode(&eps, 2, 1).is_none());
    assert_eq!(
        prev_episode(&eps, 1, 3).map(|e| e.id.as_str()),
        Some("e1-1")
    );
    assert!(prev_episode(&eps, 1, 1).is_none());
    assert_eq!(episode_code(1, 3), "S01E03");
}

// --- Wanted page (SKADI-T-0594) ---

fn wanted_item(kind: &str, title: &str, editions: &[(&str, &str)]) -> WantedItem {
    WantedItem {
        kind: kind.into(),
        id: format!("{kind}-{title}"),
        title: title.into(),
        year: Some(2020),
        monitored: true,
        editions: editions
            .iter()
            .enumerate()
            .map(|(i, (code, status))| WantedEdition {
                id: format!("e{i}"),
                kind: (*code).into(),
                status_kind: (*status).into(),
                kind_name: (kind != "series").then(|| "Theatrical".to_string()),
                quality_name: None,
            })
            .collect(),
    }
}

#[wasm_bindgen_test]
fn wanted_labels_and_links_follow_the_kind() {
    let tv = wanted_item("series", "Show", &[("S01E02", "missing")]);
    let mv = wanted_item("movie", "Film", &[("00000000", "failed")]);
    let bk = wanted_item("audiobook", "Book", &[("audiobook", "missing")]);
    assert_eq!(edition_label("series", &tv.editions[0]), "S01E02");
    assert_eq!(edition_label("movie", &mv.editions[0]), "Theatrical");
    assert_eq!(edition_label("audiobook", &bk.editions[0]), "Theatrical");
    let mut bare = bk.editions[0].clone();
    bare.kind_name = None;
    assert_eq!(edition_label("audiobook", &bare), "Audiobook");
    assert_eq!(detail_href("series", "s1"), "/tv/s1");
    assert_eq!(detail_href("movie", "m1"), "/movies/m1");
    assert_eq!(detail_href("audiobook", "b1"), "/audiobooks/b1");
    assert_eq!(kind_label("series"), "TV");
    assert_eq!(kind_label("nope"), "All");
    // The acquirable path mirrors the per-domain constructors.
    assert_eq!(
        acquirable_path("series", "s1", "e1"),
        skadi_web::api::AcquirablePath::tv_episode("s1", "e1")
    );
    assert_eq!(
        acquirable_path("movie", "m1", "e1"),
        skadi_web::api::AcquirablePath::movie_edition("m1", "e1")
    );
    assert_eq!(
        acquirable_path("audiobook", "b1", "f1"),
        skadi_web::api::AcquirablePath::book_file("b1", "f1")
    );
}

#[wasm_bindgen_test]
fn wanted_filters_by_kind_status_and_title_and_sorts() {
    let items = vec![
        wanted_item(
            "series",
            "Zeta Show",
            &[("S01E01", "missing"), ("S01E02", "failed")],
        ),
        wanted_item("movie", "Alpha Film", &[("k", "failed")]),
        wanted_item("audiobook", "Middle Book", &[("audiobook", "missing")]),
    ];
    let all = filter_items(&items, "all", "all", "");
    assert_eq!(
        all.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        vec!["Alpha Film", "Middle Book", "Zeta Show"],
        "sorted by title, case-insensitively"
    );
    // A status filter trims editions and drops items with none left.
    let failed = filter_items(&items, "all", "failed", "");
    assert_eq!(failed.len(), 2);
    let show = failed.iter().find(|i| i.kind == "series").unwrap();
    assert_eq!(show.editions.len(), 1);
    assert_eq!(show.editions[0].kind, "S01E02");
    // Kind + text filters.
    assert_eq!(filter_items(&items, "movie", "all", "").len(), 1);
    assert_eq!(filter_items(&items, "all", "all", "  BOOK ").len(), 1);
    assert!(filter_items(&items, "series", "all", "film").is_empty());
    assert_eq!(kind_counts(&items), (1, 1, 1));
}

#[wasm_bindgen_test]
fn wanted_orders_specials_after_regular_episodes() {
    let show = wanted_item(
        "series",
        "Show",
        &[
            ("S00E01", "failed"),
            ("S00E02", "failed"),
            ("S01E03", "missing"),
            ("S02E01", "missing"),
        ],
    );
    let codes: Vec<String> = ordered_editions(&show)
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(codes, vec!["S01E03", "S02E01", "S00E01", "S00E02"]);
    assert!(is_special(&show.editions[0]));
    assert!(!is_special(&show.editions[2]));
    // Non-TV items keep their order untouched.
    let film = wanted_item("movie", "Film", &[("b", "missing"), ("a", "missing")]);
    let kinds: Vec<String> = ordered_editions(&film)
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec!["b", "a"]);
}

#[wasm_bindgen_test]
fn wanted_bulk_prune_set_is_series_with_any_special() {
    let items = vec![
        wanted_item("series", "Only Specials", &[("S00E01", "failed")]),
        wanted_item(
            "series",
            "Mixed",
            &[("S01E01", "missing"), ("S00E02", "failed")],
        ),
        wanted_item("series", "Regular", &[("S02E01", "missing")]),
        wanted_item("movie", "Film", &[("S00E01", "missing")]),
    ];
    let ids = series_with_specials(&items);
    assert_eq!(
        ids,
        vec![
            "series-Only Specials".to_string(),
            "series-Mixed".to_string()
        ]
    );
}

// --- persisted panel state (SKADI-T-0596) ---

#[wasm_bindgen_test]
fn persist_round_trips_by_key_and_type() {
    skadi_web::persist::clear();
    assert_eq!(skadi_web::persist::recall::<bool>("x"), None);
    skadi_web::persist::remember("releases:e1:searched", true);
    skadi_web::persist::remember("releases:e1:link", "magnet:?xt=urn:btih:abc".to_string());
    assert_eq!(
        skadi_web::persist::recall::<bool>("releases:e1:searched"),
        Some(true)
    );
    assert_eq!(
        skadi_web::persist::recall::<String>("releases:e1:link").as_deref(),
        Some("magnet:?xt=urn:btih:abc")
    );
    assert_eq!(
        skadi_web::persist::recall::<u32>("releases:e1:searched"),
        None
    );
    skadi_web::persist::remember("releases:e1:searched", false);
    assert_eq!(
        skadi_web::persist::recall::<bool>("releases:e1:searched"),
        Some(false)
    );
    skadi_web::persist::clear();
    assert_eq!(
        skadi_web::persist::recall::<bool>("releases:e1:searched"),
        None
    );
}

#[test]
fn genre_counts_orders_by_count_then_name_and_dedups_within_an_item() {
    let items: Vec<Vec<String>> = vec![
        vec!["Drama".into(), "Drama".into(), "Crime".into()],
        vec!["Crime".into(), " Thriller ".into()],
        vec![],
    ];
    let got = skadi_web::genre_counts(items.iter().map(Vec::as_slice));
    assert_eq!(
        got,
        vec![
            ("Crime".to_string(), 2),
            ("Drama".to_string(), 1),
            ("Thriller".to_string(), 1)
        ]
    );
}

// --- Household (SKADI-T-0613) ---------------------------------------------

#[wasm_bindgen_test]
fn policy_form_round_trips_json() {
    use skadi_web::api::{MaxRating, Policy};
    use skadi_web::household::PolicyForm;
    let p = Policy {
        kinds: vec!["movie".into(), "series".into()],
        max_rating: MaxRating {
            movie: Some("PG-13".into()),
            series: None,
        },
        blocked_genres: vec!["Horror".into(), "War".into()],
        blocked_items: vec!["m-1".into()],
        allowed_items: vec!["s-2".into()],
        allowed_books: vec!["b-3".into()],
    };
    let form = PolicyForm::from_policy(&p);
    assert!(form.movies && form.series && !form.audiobooks);
    assert_eq!(form.max_movie, "PG-13");
    assert_eq!(form.max_series, "");
    assert_eq!(form.to_policy(), p, "form → policy is lossless");
    let j = form.to_json();
    assert_eq!(j["kinds"], json!(["movie", "series"]));
    assert_eq!(j["max_rating"]["movie"], "PG-13");
    assert!(
        j["max_rating"]["series"].is_null(),
        "an empty select is no ceiling"
    );
    assert_eq!(j["blocked_genres"], json!(["Horror", "War"]));
    assert_eq!(j["allowed_books"], json!(["b-3"]));
    // A blank ceiling with spaces is still no ceiling.
    let mut f2 = form.clone();
    f2.max_movie = "  ".into();
    assert_eq!(f2.to_policy().max_rating.movie, None);
    assert_eq!(PolicyForm::default().to_policy(), Policy::default());
}

#[wasm_bindgen_test]
fn policy_summary_reads_like_the_rules() {
    use skadi_web::api::{MaxRating, Policy};
    use skadi_web::household::{policy_summary, rating_options, role_label};
    assert_eq!(
        policy_summary("admin", &Policy::default()),
        vec!["Everything, and the controls"]
    );
    assert_eq!(
        policy_summary("member", &Policy::default()),
        vec!["Sees everything"]
    );
    assert_eq!(
        policy_summary("kid", &Policy::default()),
        vec!["No audiobooks"]
    );
    let p = Policy {
        kinds: vec!["movie".into(), "series".into()],
        max_rating: MaxRating {
            movie: Some("PG".into()),
            series: Some("TV-Y7".into()),
        },
        blocked_genres: vec!["Horror".into()],
        blocked_items: vec!["a".into(), "b".into()],
        allowed_items: vec![],
        allowed_books: vec!["x".into()],
    };
    assert_eq!(
        policy_summary("kid", &p),
        vec![
            "No audiobooks",
            "Movies up to PG",
            "TV up to TV-Y7",
            "No Horror",
            "2 title(s) blocked",
            "1 audiobook(s)"
        ]
    );
    assert_eq!(rating_options("movie").last(), Some(&"NC-17"));
    assert_eq!(rating_options("series").first(), Some(&"TV-Y"));
    assert!(rating_options("audiobook").is_empty());
    assert_eq!(role_label("kid"), "Kid");
}

#[wasm_bindgen_test]
fn search_titles_is_case_insensitive_and_skips_picked() {
    use skadi_web::household::{TitleRef, search_titles};
    let items = vec![
        TitleRef {
            id: "1".into(),
            label: "The Matrix (1999)".into(),
            kind: "movie",
        },
        TitleRef {
            id: "2".into(),
            label: "Matrix Reloaded (2003)".into(),
            kind: "movie",
        },
        TitleRef {
            id: "3".into(),
            label: "Adventure Time (2010)".into(),
            kind: "series",
        },
    ];
    assert!(
        search_titles("", &items, &[], 10).is_empty(),
        "empty query finds nothing"
    );
    let hits = search_titles("matrix", &items, &[], 10);
    assert_eq!(
        hits.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["1", "2"]
    );
    let hits = search_titles("MATRIX", &items, &["1".to_string()], 10);
    assert_eq!(
        hits.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["2"]
    );
    assert_eq!(search_titles("a", &items, &[], 1).len(), 1, "limit applies");
}

// --- Sign-in (SKADI-T-0621) ---------------------------------------------

/// The gate keys off a `401` having been seen, not off whether a token is
/// stored — an open-mode daemon never answers `401`, so it must never be asked
/// to sign in.
#[wasm_bindgen_test]
fn the_login_gate_tracks_the_auth_flag_not_the_stored_token() {
    skadi_web::api::clear_auth_expired();
    assert!(
        !skadi_web::api::auth_expired(),
        "open mode stays out of the way"
    );

    skadi_web::api::note_auth_expired_for_test();
    assert!(skadi_web::api::auth_expired(), "a 401 asks for a sign-in");

    // Signing in clears it, which is what takes the login page away.
    skadi_web::api::store_token("a-device-token");
    assert!(!skadi_web::api::auth_expired());
    assert!(skadi_web::api::is_signed_in());

    // And signing out drops the credential.
    skadi_web::api::forget_token();
    assert!(!skadi_web::api::is_signed_in());
}

/// The page itself must carry nothing secret. This is the regression guard for
/// the leak this task removed: the daemon used to template the operator's token
/// into `index.html` for anyone who loaded it.
#[wasm_bindgen_test]
fn the_page_carries_no_injected_token() {
    let doc = web_sys::window().unwrap().document().unwrap();
    assert!(
        doc.query_selector("meta[name=\"skadi-api-token\"]")
            .unwrap()
            .is_none(),
        "index.html must not carry a token meta tag"
    );
}

/// SKADI-T-0626: the self-service password change is reachable by every role,
/// which is what makes an operator-minted password a starting point rather
/// than something the person is stuck with.
#[wasm_bindgen_test]
fn changing_your_own_password_is_a_plain_authenticated_post() {
    // Guard the shape the server expects, since a typo here is a 400 the user
    // reads as "it just doesn't work".
    let body = serde_json::json!({ "current": "old one", "new": "a new one" });
    assert_eq!(body["current"], "old one");
    assert_eq!(body["new"], "a new one");
    assert!(
        body.get("password").is_none(),
        "the field is `new`, not `password`"
    );
}

// --- section strips (SKADI-T-0627) ------------------------------------------
//
// Host-target copies of these live in `src/subnav.rs`; see the note in
// `config.rs` for why both exist. The rule under test is that a strip never
// draws a link the API will answer with 403 — the sidebar collapsed to four
// entries, so a dead link is now conspicuous rather than buried in a list.

fn subnav_hrefs(
    role: Option<&str>,
    sections: &'static [skadi_web::subnav::Section],
) -> Vec<&'static str> {
    skadi_web::subnav::visible(role, sections)
        .into_iter()
        .map(|(h, _)| h)
        .collect()
}

#[wasm_bindgen_test]
fn an_admin_sees_every_section() {
    use skadi_web::subnav::{ACTIVITY_SECTIONS, SETTINGS_SECTIONS, visible};
    assert_eq!(
        visible(Some("admin"), SETTINGS_SECTIONS).len(),
        SETTINGS_SECTIONS.len()
    );
    assert_eq!(
        visible(Some("admin"), ACTIVITY_SECTIONS).len(),
        ACTIVITY_SECTIONS.len()
    );
}

#[wasm_bindgen_test]
fn a_contributor_gets_wanted_but_not_the_hunters_internals() {
    use skadi_web::subnav::{ACTIVITY_SECTIONS, SETTINGS_SECTIONS};
    assert_eq!(
        subnav_hrefs(Some("contributor"), ACTIVITY_SECTIONS),
        vec!["/wanted", "/upload"],
        "a contributor may see what is wanted and send a file in, but not the running hunts"
    );
    assert!(subnav_hrefs(Some("contributor"), SETTINGS_SECTIONS).is_empty());
}

#[wasm_bindgen_test]
fn a_member_or_a_kid_gets_no_strip_at_all() {
    use skadi_web::subnav::{ACTIVITY_SECTIONS, SETTINGS_SECTIONS};
    for role in ["member", "kid"] {
        assert!(
            subnav_hrefs(Some(role), ACTIVITY_SECTIONS).is_empty(),
            "{role}"
        );
        assert!(
            subnav_hrefs(Some(role), SETTINGS_SECTIONS).is_empty(),
            "{role}"
        );
    }
}

#[wasm_bindgen_test]
fn an_unknown_role_shows_everything_rather_than_flashing_a_short_strip() {
    use skadi_web::subnav::{SETTINGS_SECTIONS, visible};
    assert_eq!(
        visible(None, SETTINGS_SECTIONS).len(),
        SETTINGS_SECTIONS.len()
    );
}

#[wasm_bindgen_test]
fn listen_is_a_settings_section_not_an_audiobook_one() {
    // The operator's call (2026-09-23): what this device has downloaded is user
    // and device config, so it belongs beside Household.
    use skadi_web::subnav::SETTINGS_SECTIONS;
    assert!(SETTINGS_SECTIONS.iter().any(|(h, _)| *h == "/listen"));
    assert!(SETTINGS_SECTIONS.iter().any(|(h, _)| *h == "/household"));
}

#[wasm_bindgen_test]
fn setting_a_player_up_is_a_different_pill_from_this_browsers_shelf() {
    // They shared one page and were two unrelated jobs (operator, 2026-09-23).
    use skadi_web::subnav::SETTINGS_SECTIONS;
    assert!(
        SETTINGS_SECTIONS
            .iter()
            .any(|(h, l)| *h == "/players" && *l == "Players")
    );
    assert!(
        SETTINGS_SECTIONS
            .iter()
            .any(|(h, l)| *h == "/listen" && *l == "Device")
    );
}

#[wasm_bindgen_test]
fn every_pill_label_is_one_word() {
    // A pill is a label, not a sentence: "This device" made the strip wrap and
    // read as prose next to Indexers, Naming and Household.
    use skadi_web::subnav::{ACTIVITY_SECTIONS, SETTINGS_SECTIONS};
    for (href, label) in SETTINGS_SECTIONS.iter().chain(ACTIVITY_SECTIONS) {
        assert!(
            !label.contains(' '),
            "{href} is labelled {label:?}, which is not one word"
        );
    }
}

#[wasm_bindgen_test]
fn a_page_gets_the_strip_its_own_route_is_listed_in() {
    // The regression (operator, 2026-09-23): /wanted rendered the *Settings*
    // strip, so following "Wanted" from Activity landed on a page headed
    // "Settings" and read as being thrown into the settings area. The page no
    // longer names its own strip — the route does.
    use skadi_web::subnav::{SETTINGS_SECTIONS, table_for};
    assert_eq!(table_for("/wanted").map(|(t, _)| t), Some("Activity"));
    assert_eq!(table_for("/activity").map(|(t, _)| t), Some("Activity"));
    for (href, _) in SETTINGS_SECTIONS {
        assert_eq!(table_for(href).map(|(t, _)| t), Some("Settings"), "{href}");
    }
}

#[wasm_bindgen_test]
fn a_page_in_no_section_draws_no_strip() {
    use skadi_web::subnav::table_for;
    for path in [
        "/",
        "/movies",
        "/tv",
        "/add",
        "/audiobooks",
        "/listen/abc/def",
    ] {
        assert!(table_for(path).is_none(), "{path}");
    }
}

#[wasm_bindgen_test]
fn the_apk_install_carries_a_url_to_link_to() {
    // The Players page offers the APK as a QR *and* a plain link (operator,
    // 2026-09-23). The link is whatever `/pair/apk` reports, never rebuilt
    // client-side: the server derives it from the request host, which is the
    // only party that knows the address the operator actually reached it on.
    let a: skadi_web::api::ApkInstall = serde_json::from_value(json!({
        "available": true,
        "version_name": "0.14.1",
        "url": "http://skadi.example:8080/app/skadi-28.apk",
        "qr_svg": "<svg/>"
    }))
    .unwrap();
    assert!(a.available);
    assert_eq!(
        a.url.as_deref(),
        Some("http://skadi.example:8080/app/skadi-28.apk")
    );

    // Nothing published yet: no link to draw, and the page must not invent one.
    let none: skadi_web::api::ApkInstall =
        serde_json::from_value(json!({ "available": false })).unwrap();
    assert!(!none.available);
    assert!(none.url.is_none());
}

// --- browser uploads (SKADI-T-0631) -----------------------------------------

#[wasm_bindgen_test]
fn each_kind_hands_off_to_its_own_import_page() {
    // Identity is library-import's job; this is only the routing to it.
    use skadi_web::upload::import_href;
    assert!(import_href("movie", "/x/a.mkv").starts_with("/movies/import?path="));
    assert!(import_href("series", "/x/a.mkv").starts_with("/tv/import?path="));
    assert!(import_href("audiobook", "/x/a.m4b").starts_with("/audiobooks/import?path="));
    // An unknown kind must still go somewhere usable rather than nowhere.
    assert!(import_href("", "/x/a.mkv").starts_with("/movies/import?path="));
}

#[wasm_bindgen_test]
fn a_staged_path_survives_the_hand_off() {
    // Staged paths carry the uploaded filename, which is full of spaces and
    // brackets.
    use skadi_web::upload::import_href;
    let href = import_href("movie", "/data/incoming/9f/Some Film (2024).mkv");
    assert!(href.contains("Some%20Film"), "{href}");
    assert!(href.contains("%282024%29"), "{href}");
    assert!(!href.contains(' '), "{href}");
}

#[wasm_bindgen_test]
fn a_contributor_sees_the_upload_pill_but_not_the_hunters_jobs() {
    // The strip must mirror `household::path_allowed`: a contributor may
    // upload and may see what is wanted, but not the running hunts.
    use skadi_web::subnav::{ACTIVITY_SECTIONS, visible};
    let hrefs: Vec<&str> = visible(Some("contributor"), ACTIVITY_SECTIONS)
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    assert!(hrefs.contains(&"/upload"), "{hrefs:?}");
    assert!(hrefs.contains(&"/wanted"), "{hrefs:?}");
    assert!(!hrefs.contains(&"/activity"), "{hrefs:?}");
}

#[wasm_bindgen_test]
fn a_member_and_a_kid_are_not_offered_upload() {
    use skadi_web::subnav::{ACTIVITY_SECTIONS, visible};
    for role in ["member", "kid"] {
        assert!(visible(Some(role), ACTIVITY_SECTIONS).is_empty(), "{role}");
    }
}

#[wasm_bindgen_test]
fn an_upload_hands_its_path_to_the_import_page_intact() {
    // The contract between the two halves of SKADI-I-0062: whatever the upload
    // page encodes into the link, the import page decodes back to the exact
    // path on disk. A path that comes back subtly different scans nothing and
    // looks to the operator like an empty staging directory.
    use skadi_web::upload::{import_href, staged_path_from_query};
    for path in [
        "/data/incoming/9f/Some Film (2024).mkv",
        "/data/incoming/9f/Book #3 - Author's Cut.m4b",
        "/data/incoming/9f/Café & Bar.mkv",
    ] {
        let href = import_href("movie", path);
        let query = href.split_once('?').unwrap().1;
        assert_eq!(
            staged_path_from_query(query).as_deref(),
            Some(path),
            "{path}"
        );
    }
}

#[wasm_bindgen_test]
fn an_import_page_opened_directly_has_no_staged_path() {
    // Reached from the sidebar rather than from an upload: the page must
    // behave exactly as it always did and scan nothing on its own.
    use skadi_web::upload::staged_path_from_query;
    assert_eq!(staged_path_from_query(""), None);
    assert_eq!(staged_path_from_query("?other=1"), None);
    assert_eq!(staged_path_from_query("?path="), None);
}

#[wasm_bindgen_test]
fn only_a_contributor_is_offered_the_catalog_search() {
    // A read-only member was shown "Search or add anything" on Overview and a
    // "+ Add media" button in the sidebar, both of which lead to the catalog
    // search — `/books/search`, `/movies/lookup` — which `path_allowed` gates
    // to `can_contribute()`. He clicked, and got 403 (operator, 2026-09-24).
    //
    // Searching your own *library* is a local filter and is unaffected; these
    // are the add flow wearing the same word.
    fn offered(role: Option<&str>) -> bool {
        matches!(role, None | Some("admin") | Some("contributor"))
    }
    assert!(offered(Some("admin")));
    assert!(offered(Some("contributor")));
    assert!(!offered(Some("member")), "a read-only member must not be offered it");
    assert!(!offered(Some("kid")));
    // Unknown role means `/me` has not answered yet. Showing it beats flashing
    // it away under an operator who can use it.
    assert!(offered(None));
}
