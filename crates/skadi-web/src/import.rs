//! Library import view (SKADI-T-0075 / SKADI-T-0078): point Skadi at an on-disk
//! library path, review the proposed TMDB matches, fix/deselect the wrong ones,
//! and import the chosen items **in place** (no move/rename — see [`crate::api`]
//! and the backend SKADI-T-0074).
//!
//! Scanning is parse-only and instant even for a library of thousands; metadata
//! matching is **lazy and paginated** — the page resolves matches only for the
//! candidates currently in view (`POST /library-import/match`), so a big library
//! never triggers thousands of lookups up front (SKADI-T-0078). Commit runs in
//! client-side batches with a live progress count.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;
use crate::path_picker::PathPicker;

/// How many candidates per page (also the per-page match batch size).
const PAGE_SIZE: usize = 50;

/// Shared confidence badge vocabulary (SKADI-T-0328).
use crate::import_common::confidence_class;

/// One reviewable row: the scanned candidate, its (lazily resolved) match, and
/// the operator's edits (selection + TMDB-id override). Plain data — the list
/// re-renders on change.
#[derive(Clone)]
struct ImportRow {
    cand: api::ScanCandidate,
    /// Whether a metadata match has been attempted for this row yet.
    matched: bool,
    proposed: Option<api::ProposedMatch>,
    /// `high` / `low` / `none`, or empty before matching.
    confidence: String,
    already_in_library: bool,
    selected: bool,
    /// Editable TMDB id; prefilled from the proposed match once resolved.
    tmdb: String,
    /// Inline search-picker state (SKADI-T-0328): a wrong match is fixed by
    /// searching a title instead of hand-hunting a TMDB id.
    picker_open: bool,
    search_q: String,
    searching: bool,
    search_results: Vec<api::MovieSearchResult>,
}

impl ImportRow {
    fn from_candidate(c: api::ScanCandidate) -> Self {
        // Prefill the TMDB field from a folder-embedded id so the match step can
        // look it up exactly.
        let tmdb = c.tmdb_id.map(|id| id.to_string()).unwrap_or_default();
        ImportRow {
            cand: c,
            matched: false,
            proposed: None,
            confidence: String::new(),
            already_in_library: false,
            selected: false,
            tmdb,
            picker_open: false,
            search_q: String::new(),
            searching: false,
            search_results: Vec::new(),
        }
    }

    /// Compact "Drama/Sci-Fi · A24 · ★7.9" facts line from the NFO, if any.
    fn facts(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if !self.cand.nfo_genres.is_empty() {
            parts.push(
                self.cand
                    .nfo_genres
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
        if let Some(s) = self.cand.nfo_studio.clone().filter(|s| !s.is_empty()) {
            parts.push(s);
        }
        if let Some(r) = self.cand.nfo_rating.clone().filter(|s| !s.is_empty()) {
            parts.push(format!("★{r}"));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[component]
pub fn LibraryImportPage() -> impl IntoView {
    let path = RwSignal::new(String::new());

    let rows = RwSignal::new(Vec::<ImportRow>::new());
    let scanning = RwSignal::new(false);
    let matching = RwSignal::new(false);
    let importing = RwSignal::new(false);
    let scanned = RwSignal::new(false);
    let page = RwSignal::new(0usize);
    let scan_error = RwSignal::new(None::<String>);
    let result = RwSignal::new(None::<Result<api::CommitResult, String>>);
    // Live import progress: (committed_so_far, total) while a batched import runs.
    let progress = RwSignal::new(None::<(usize, usize)>);

    // No root/profile pickers (SKADI-T-0304): the library root is derived
    // (T-0302) and the upgrade profile defaults server-side.

    // Hide/sink rows already in the library (SKADI-T-0328, TV pattern). The
    // in-library flag resolves lazily per matched page, so the count grows as
    // pages match. `visible` is the display order as **indices** into `rows`
    // (write-backs stay index-stable).
    let hide_imported = RwSignal::new(true);
    let visible = Memo::new(move |_| {
        let hide = hide_imported.get();
        rows.with(|all| {
            let mut v: Vec<(usize, bool)> = all
                .iter()
                .enumerate()
                .map(|(i, r)| (i, r.already_in_library))
                .filter(|(_, in_lib)| !(hide && *in_lib))
                .collect();
            v.sort_by_key(|(_, in_lib)| *in_lib);
            v.into_iter().map(|(i, _)| i).collect::<Vec<usize>>()
        })
    });

    // Resolve metadata matches for the candidates on page `p` that haven't been
    // matched yet, then merge the results back by path. MUST slice the same
    // list the UI renders (`visible`), or hidden rows shift the page under the
    // match and tail rows never match.
    let match_page = move |p: usize| {
        let pending: Vec<api::MatchQuery> = {
            let idxs = visible.get_untracked();
            rows.with_untracked(|all| {
                idxs.iter()
                    .skip(p * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .filter_map(|&i| all.get(i))
                    .filter(|r| !r.matched)
                    .map(|r| api::MatchQuery {
                        path: r.cand.path.clone(),
                        title: r.cand.parsed_title.clone(),
                        year: r.cand.parsed_year,
                        // A folder-parsed (or pre-typed) id drives an exact lookup.
                        tmdb_id: r.tmdb.trim().parse::<u64>().ok(),
                    })
                    .collect()
            })
        };
        if pending.is_empty() {
            return;
        }
        matching.set(true);
        spawn_local(async move {
            match api::library_import_match(&pending).await {
                Ok(results) => {
                    rows.update(|all| {
                        for res in results {
                            if let Some(row) = all.iter_mut().find(|r| r.cand.path == res.path) {
                                row.matched = true;
                                row.confidence = res.confidence.clone();
                                row.already_in_library = res.already_in_library;
                                row.tmdb = res
                                    .proposed
                                    .as_ref()
                                    .map(|m| m.tmdb_id.to_string())
                                    .unwrap_or_default();
                                // Pre-tick confident, non-duplicate, matched rows.
                                row.selected = res.proposed.is_some()
                                    && !res.already_in_library
                                    && res.confidence == "high";
                                row.proposed = res.proposed;
                            }
                        }
                    });
                }
                Err(e) => scan_error.set(Some(e.to_string())),
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
            match api::library_import_scan(&p).await {
                Ok(cands) => {
                    rows.set(cands.into_iter().map(ImportRow::from_candidate).collect());
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
        // Gather the ticked rows across all pages; a ticked row with a
        // non-numeric TMDB id is a hard error (the operator must fix or untick).
        let mut items = Vec::new();
        for r in rows.get_untracked().iter().filter(|r| r.selected) {
            match r.tmdb.trim().parse::<u64>() {
                Ok(id) => items.push(api::CommitItem {
                    path: r.cand.path.clone(),
                    tmdb_id: id,
                    quality_id: r.cand.quality_id.clone(),
                }),
                Err(_) => {
                    result.set(Some(Err(format!(
                        "“{}” has no valid TMDB id — set one or untick it",
                        r.cand.display_name
                    ))));
                    return;
                }
            }
        }
        if items.is_empty() {
            result.set(Some(Err("nothing selected to import".into())));
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
            let mut failed = false;
            for chunk in items.chunks(BATCH) {
                match api::library_import_commit(chunk).await {
                    Ok(res) => {
                        imported += res.imported;
                        skipped += res.skipped;
                        linked += res.linked;
                        in_place += res.in_place;
                        unmatched.extend(res.unmatched);
                        errors.extend(res.errors);
                    }
                    Err(e) => {
                        result.set(Some(Err(e.to_string())));
                        failed = true;
                        break;
                    }
                }
                done += chunk.len();
                progress.set(Some((done, total)));
            }
            importing.set(false);
            progress.set(None);
            if !failed {
                result.set(Some(Ok(api::CommitResult {
                    imported,
                    skipped,
                    linked,
                    in_place,
                    unmatched,
                    errors,
                })));
                // Mark imported rows so a re-view shows them as in-library, and
                // clear their selection.
                rows.update(|all| {
                    for r in all.iter_mut().filter(|r| r.selected) {
                        r.selected = false;
                        r.already_in_library = true;
                    }
                });
            }
        });
    };

    let selected_count = move || rows.with(|all| all.iter().filter(|r| r.selected).count());
    let total_pages = move || visible.get().len().div_ceil(PAGE_SIZE).max(1);
    let cur_page = move || page.get().min(total_pages().saturating_sub(1));

    // Run the row's title search and store the results (picker, SKADI-T-0328).
    let run_search = move |i: usize| {
        let q = rows.with_untracked(|all| all.get(i).map(|r| r.search_q.trim().to_string()));
        let Some(q) = q.filter(|q| !q.is_empty()) else {
            return;
        };
        rows.update(|all| {
            if let Some(r) = all.get_mut(i) {
                r.searching = true;
            }
        });
        spawn_local(async move {
            let results = api::search_movies(&q, None).await.unwrap_or_default();
            rows.update(|all| {
                if let Some(r) = all.get_mut(i) {
                    r.search_results = results;
                    r.searching = false;
                }
            });
        });
    };

    let candidate_rows = move || {
        let start = cur_page() * PAGE_SIZE;
        let idxs = visible.get();
        idxs.into_iter()
            .skip(start)
            .take(PAGE_SIZE)
            .filter_map(|i| rows.with(|all| all.get(i).cloned()).map(|r| (i, r)))
            .map(|(i, row)| {
                let cand = row.cand.clone();
                let in_lib = row.already_in_library;
                let parsed = match (cand.parsed_title.clone(), cand.parsed_year) {
                    (Some(t), Some(y)) => format!("{t} ({y})"),
                    (Some(t), None) => t,
                    _ => "—".to_string(),
                };
                let facts = row.facts();
                let overview = cand.nfo_overview.clone().filter(|s| !s.is_empty());
                let proposed = row.proposed.clone();
                let matched = row.matched;
                let confidence = row.confidence.clone();
                let conf_class = confidence_class(&confidence);
                let conf_title = crate::import_common::confidence_title(&confidence);
                let quality = cand.quality_name.clone().unwrap_or_default();

                let toggle = move |_| {
                    rows.update(|v| {
                        if let Some(r) = v.get_mut(i) {
                            r.selected = !r.selected;
                        }
                    });
                };
                let on_tmdb = move |ev| {
                    let val = event_target_value(&ev);
                    rows.update(|v| {
                        if let Some(r) = v.get_mut(i) {
                            r.tmdb = val;
                        }
                    });
                };
                // Picker: seed the query from the best-known title on first open.
                let seed = cand
                    .parsed_title
                    .clone()
                    .unwrap_or_else(|| cand.display_name.clone());
                let toggle_picker = move |_| {
                    rows.update(|v| {
                        if let Some(r) = v.get_mut(i) {
                            r.picker_open = !r.picker_open;
                            if r.picker_open && r.search_q.trim().is_empty() {
                                r.search_q = seed.clone();
                            }
                        }
                    });
                };
                let on_q = move |ev| {
                    let val = event_target_value(&ev);
                    rows.update(|v| {
                        if let Some(r) = v.get_mut(i) {
                            r.search_q = val;
                        }
                    });
                };

                let picker_row = row.picker_open.then(|| {
                    let results = row.search_results.clone();
                    let searching = row.searching;
                    let none_yet =
                        !searching && results.is_empty() && !row.search_q.trim().is_empty();
                    view! {
                        <tr class="picker-tr">
                            <td></td>
                            <td colspan="5">
                                <div class="import-picker">
                                    <div class="import-picker-search">
                                        <input
                                            class="path-field"
                                            placeholder="Search movie title…"
                                            prop:value=row.search_q.clone()
                                            on:input=on_q
                                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                if ev.key() == "Enter" { run_search(i); }
                                            }
                                        />
                                        <button on:click=move |_| run_search(i)>"Search"</button>
                                    </div>
                                    {searching.then(|| view! { <p class="muted ep-empty">"Searching…"</p> })}
                                    {none_yet.then(|| view! { <p class="muted ep-empty">"No results — try a different title."</p> })}
                                    <div class="import-picker-results">
                                        {results.into_iter().map(|res| {
                                            let yr = res.year.map(|y| format!(" ({y})")).unwrap_or_default();
                                            let title = res.title.clone();
                                            let id = res.tmdb_id;
                                            // The cover is what disambiguates two films sharing a
                                            // title and year — the operator's actual problem
                                            // (SKADI-T-0319). Title + year alone often cannot.
                                            let poster = res.poster_url.clone();
                                            let pick_title = title.clone();
                                            let pick_year = res.year;
                                            let pick = move |_| {
                                                rows.update(|v| {
                                                    if let Some(r) = v.get_mut(i) {
                                                        r.tmdb = id.to_string();
                                                        r.proposed = Some(api::ProposedMatch {
                                                            tmdb_id: id,
                                                            title: pick_title.clone(),
                                                            year: pick_year,
                                                            score: 1.0,
                                                        });
                                                        r.matched = true;
                                                        r.confidence = "manual".to_string();
                                                        r.already_in_library = false;
                                                        r.selected = true;
                                                        r.picker_open = false;
                                                        r.search_results = Vec::new();
                                                    }
                                                });
                                            };
                                            view! {
                                                <button class="import-picker-result" on:click=pick>
                                                    {poster.map(|src| view! {
                                                        <img class="picker-art" src=src alt="" loading="lazy"/>
                                                    })}
                                                    <span class="picker-title">{format!("{title}{yr}")}</span>
                                                    <span class="mono muted">{format!("tmdb {id}")}</span>
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                </div>
                            </td>
                        </tr>
                    }
                });

                let row_class = if in_lib { "in-lib" } else { "" };
                view! {
                    <tr class=row_class>
                        <td data-label="Import">
                            <input
                                type="checkbox"
                                prop:checked=row.selected
                                disabled=in_lib
                                on:change=toggle
                            />
                        </td>
                        <td class="folder" data-label="Folder" title=cand.display_name.clone()>{cand.display_name.clone()}</td>
                        <td data-label="Parsed">
                            {parsed}
                            // NFO facts + synopsis — confirm the match at a glance
                            // (SKADI-T-0328).
                            {facts.map(|f| view! {
                                <div class="import-facts" title=overview.clone().unwrap_or_default()>{f}</div>
                            })}
                        </td>
                        <td data-label="Quality">{quality}</td>
                        <td data-label="Match">
                            {match (matched, proposed) {
                                (false, _) => view! { <span class="muted">"matching…"</span> }.into_any(),
                                (true, Some(p)) => {
                                    let year = p.year.map(|y| format!(" ({y})")).unwrap_or_default();
                                    view! {
                                        <span>{p.title.clone()} {year} " "</span>
                                        <span class=format!("badge {conf_class}") title=conf_title>{confidence.clone()}</span>
                                    }.into_any()
                                }
                                (true, None) => view! {
                                    <span class="muted">{if in_lib { "in library" } else { "no match" }}</span>
                                }.into_any(),
                            }}
                        </td>
                        <td class="import-actions" data-label="TMDB id">
                            <button
                                class="btn-link import-find"
                                disabled=in_lib
                                title="Search for the right movie"
                                on:click=toggle_picker
                            >
                                "🔍"
                            </button>
                            <input
                                class="tmdb-input"
                                type="text"
                                placeholder="603"
                                prop:value=row.tmdb.clone()
                                disabled=in_lib
                                on:change=on_tmdb
                            />
                        </td>
                    </tr>
                    {picker_row}
                }
            })
            .collect_view()
    };

    view! {
        <div class="page-head">
            <h2>"Library import"</h2>
            <A href="/movies" attr:class="gear" attr:title="Back to Movies">"←"</A>
        </div>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Scan a folder"</h3>
                    <p class="muted">
                        "Bring existing media under management. Files are "
                        <strong>"hardlinked"</strong>
                        " into the library's canonical layout (subtitles, nfo and "
                        "artwork ride along); after a successful link the source "
                        "folder is removed (an adoption "
                        <strong>"move"</strong>
                        " — no extra disk is used). Cross-device imports are "
                        "refused — scan through the same mount as the root folder."
                    </p>
                </div>
            </div>
            <div class="form">
                <div class="field">
                    <label>"Library path (as the server sees it, e.g. /library/movies)"</label>
                    <div class="checks">
                        <PathPicker value=path placeholder="/library/movies"/>
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
                    <p class="muted">"New movies are hardlinked into the library (movie subfolder); their quality is detected from the file. Untick anything wrong; fix a TMDB id to re-match. Matches resolve per page."</p>
                </div>
            </div>

            <div class="import-review">
                {move || (!scanned.get()).then(|| view! {
                    <p class="muted">"Scan a folder above to see candidates."</p>
                })}
                {move || (scanned.get() && rows.with(Vec::is_empty)).then(|| view! {
                    <p class="muted">"No importable media found under that path."</p>
                })}

                {move || (!rows.with(Vec::is_empty)).then(|| {
                    let total = visible.get().len();
                    let pages = total_pages();
                    let cur = cur_page();
                    view! {
                        <div class="checks">
                            <span class="muted">
                                {format!("{total} candidates — page {} of {pages}", cur + 1)}
                            </span>
                            <button
                                on:click=move |_| go_page(cur.saturating_sub(1))
                                disabled=move || cur_page() == 0 || matching.get()
                            >"‹ Prev"</button>
                            <button
                                on:click=move |_| go_page(cur + 1)
                                disabled=move || { cur_page() + 1 >= total_pages() || matching.get() }
                            >"Next ›"</button>
                            {matching.get().then(|| view! { <span class="muted">"matching page…"</span> })}
                            <label class="import-hide-toggle muted">
                                <input
                                    type="checkbox"
                                    prop:checked=move || hide_imported.get()
                                    on:change=move |_| {
                                        hide_imported.update(|h| *h = !*h);
                                        page.set(0);
                                        match_page(0);
                                    }
                                />
                                {move || {
                                    let n = rows.with(|all| {
                                        all.iter().filter(|r| r.already_in_library).count()
                                    });
                                    format!(" Hide already-imported ({n})")
                                }}
                            </label>
                        </div>
                    }
                })}

                {move || (!rows.with(Vec::is_empty)).then(|| view! {
                    <table class="import-table">
                        <colgroup>
                            <col class="c-sel"/>
                            <col class="c-folder"/>
                            <col/>
                            <col class="c-qual"/>
                            <col/>
                            <col class="c-tmdb"/>
                        </colgroup>
                        <thead>
                            <tr>
                                <th></th>
                                <th>"Folder"</th>
                                <th>"Parsed"</th>
                                <th>"Quality"</th>
                                <th>"Match"</th>
                                <th>"TMDB id"</th>
                            </tr>
                        </thead>
                        <tbody>{candidate_rows}</tbody>
                    </table>
                })}

                {move || (!rows.with(Vec::is_empty)).then(|| {
                    view! {
                        <button on:click=on_import disabled=move || importing.get() || selected_count() == 0>
                            {move || match progress.get() {
                                Some((d, t)) => format!("Importing… {d} / {t}"),
                                None if importing.get() => "Importing…".to_string(),
                                None => format!("Import {} selected", selected_count()),
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
