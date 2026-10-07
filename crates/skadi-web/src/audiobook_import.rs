//! Audiobook library import view (SKADI-T-0134): point Skadi at an on-disk
//! audiobook tree, review the parsed books, paste/correct the Audible ASIN for
//! each, and import the chosen items by **hardlink** into the canonical
//! Author/[Series/]Book layout (adoption move — sources are removed after a
//! fresh link; see the backend `skadi_audiobooks::import`).
//!
//! Matching is **ASIN-driven**: Audnexus has no book-title search, so a row can
//! only be committed once it carries a valid ASIN. The scan extracts one from a
//! canonical `{asin-…}` tag when present; otherwise the operator pastes it. A
//! page's matches resolve lazily (`POST /audiobooks/library-import/match`).

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;
use crate::audiobooks::looks_like_asin;
use crate::path_picker::PathPicker;

/// How many candidates per page (also the per-page match batch size).
const PAGE_SIZE: usize = 50;

/// Shared confidence badge vocabulary (SKADI-T-0328) — re-exported so existing
/// imports/tests keep working.
pub use crate::import_common::confidence_class;

/// One reviewable row: the scanned candidate, its (lazily resolved) match, and
/// the operator's edits (selection + ASIN). Plain data — the list re-renders on
/// change.
#[derive(Clone)]
struct ImportRow {
    cand: api::AbScanCandidate,
    /// Whether a metadata match has been attempted for this row yet.
    matched: bool,
    proposed: Option<api::AbProposedMatch>,
    /// `high` / `none`, or empty before matching.
    confidence: String,
    already_in_library: bool,
    selected: bool,
    /// Editable Audible ASIN; prefilled from the parsed/proposed ASIN.
    asin: String,
    /// Inline search-picker state (SKADI-T-0328): a missing/wrong ASIN is fixed
    /// by searching title/author instead of hand-hunting one on Audible.
    picker_open: bool,
    search_q: String,
    searching: bool,
    search_results: Vec<api::BookSearchResult>,
}

impl ImportRow {
    fn from_candidate(c: api::AbScanCandidate) -> Self {
        let asin = c.asin.clone().unwrap_or_default();
        ImportRow {
            cand: c,
            matched: false,
            proposed: None,
            confidence: String::new(),
            already_in_library: false,
            selected: false,
            asin,
            picker_open: false,
            search_q: String::new(),
            searching: false,
            search_results: Vec::new(),
        }
    }
}

#[component]
pub fn AudiobookImportPage() -> impl IntoView {
    let path = RwSignal::new(String::new());

    let rows = RwSignal::new(Vec::<ImportRow>::new());
    let scanning = RwSignal::new(false);
    let matching = RwSignal::new(false);
    let importing = RwSignal::new(false);
    let scanned = RwSignal::new(false);
    let page = RwSignal::new(0usize);
    let scan_error = RwSignal::new(None::<String>);
    let result = RwSignal::new(None::<Result<api::CommitResult, String>>);
    let progress = RwSignal::new(None::<(usize, usize)>);

    // Audiobooks rank via the built-in M4B-first ladder (no profile, SKADI-T-0301)
    // and the library root is derived server-side (SKADI-T-0302), so the import
    // needs no profile/root pickers — just a source path to scan.

    // Hide/sink rows already in the library (SKADI-T-0328, TV pattern). The
    // in-library flag resolves lazily per matched page.
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

    // Resolve matches for the candidates on page `p` that carry an ASIN and
    // haven't been matched yet, then merge the results back by path. Slices the
    // same `visible` list the UI renders (SKADI-T-0328).
    let match_page = move |p: usize| {
        let pending: Vec<api::AbMatchQuery> = {
            let idxs = visible.get_untracked();
            rows.with_untracked(|all| {
                idxs.iter()
                    .skip(p * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .filter_map(|&i| all.get(i))
                    .filter(|r| !r.matched)
                    .map(|r| api::AbMatchQuery {
                        path: r.cand.path.clone(),
                        asin: {
                            let a = r.asin.trim();
                            (!a.is_empty()).then(|| a.to_string())
                        },
                    })
                    .collect()
            })
        };
        if pending.is_empty() {
            return;
        }
        matching.set(true);
        spawn_local(async move {
            match api::ab_library_import_match(&pending).await {
                Ok(results) => {
                    rows.update(|all| {
                        for res in results {
                            if let Some(row) = all.iter_mut().find(|r| r.cand.path == res.path) {
                                row.matched = true;
                                row.confidence = res.confidence.clone();
                                row.already_in_library = res.already_in_library;
                                if let Some(m) = &res.proposed {
                                    row.asin = m.asin.clone();
                                }
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
            match api::ab_library_import_scan(&p).await {
                Ok(cands) => {
                    rows.set(cands.into_iter().map(ImportRow::from_candidate).collect());
                    page.set(0);
                    scanned.set(true);
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
        // Gather ticked rows; a ticked row needs a plausible ASIN.
        let mut items = Vec::new();
        for r in rows.get_untracked().iter().filter(|r| r.selected) {
            let asin = r.asin.trim().to_string();
            if looks_like_asin(&asin) {
                items.push(api::AbCommitItem {
                    files: r.cand.files.clone(),
                    asin,
                    quality_id: None,
                });
            } else {
                result.set(Some(Err(format!(
                    "“{}” has no valid Audible ASIN — paste one or untick it",
                    r.cand.display_name
                ))));
                return;
            }
        }
        if items.is_empty() {
            result.set(Some(Err("nothing selected to import".into())));
            return;
        }

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
                match api::ab_library_import_commit(None, chunk).await {
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

    // Run the row's Audible title/author search (picker, SKADI-T-0328).
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
            let results = api::search_books(&q).await.unwrap_or_default();
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
                let parsed = match (cand.parsed_author.clone(), cand.parsed_title.clone()) {
                    (Some(a), Some(t)) => format!("{a} — {t}"),
                    (None, Some(t)) => t,
                    _ => "—".to_string(),
                };
                let kind = if cand.single_file { "file" } else { "folder" };
                let proposed = row.proposed.clone();
                let matched = row.matched;
                let confidence = row.confidence.clone();
                let conf_class = confidence_class(&confidence);

                let toggle = move |_| {
                    rows.update(|v| {
                        if let Some(r) = v.get_mut(i) {
                            r.selected = !r.selected;
                        }
                    });
                };
                let on_asin = move |ev| {
                    let val = event_target_value(&ev);
                    rows.update(|v| {
                        if let Some(r) = v.get_mut(i) {
                            r.asin = val;
                        }
                    });
                };
                // Picker: seed the query from author+title on first open.
                let seed = match (cand.parsed_author.clone(), cand.parsed_title.clone()) {
                    (Some(a), Some(t)) => format!("{a} {t}"),
                    (_, Some(t)) => t,
                    _ => cand.display_name.clone(),
                };
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
                                            placeholder="Search title / author…"
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
                                            let by = if res.authors.is_empty() {
                                                String::new()
                                            } else {
                                                format!(" — {}", res.authors.join(", "))
                                            };
                                            let yr = res.year.map(|y| format!(" ({y})")).unwrap_or_default();
                                            let asin = res.asin.clone();
                                            // Cover art (SKADI-T-0319): an audiobook has editions
                                            // that differ only by narrator and cover, so the image
                                            // is often the only thing that tells them apart.
                                            let cover = res.cover_url.clone();
                                            let label = format!("{}{by}{yr}", res.title);
                                            let pick_asin = asin.clone();
                                            let pick = move |_| {
                                                rows.update(|v| {
                                                    if let Some(r) = v.get_mut(i) {
                                                        r.asin = pick_asin.clone();
                                                        // The next match round resolves the
                                                        // full book from this exact ASIN.
                                                        r.matched = false;
                                                        r.proposed = None;
                                                        r.confidence = "manual".to_string();
                                                        r.picker_open = false;
                                                        r.search_results = Vec::new();
                                                    }
                                                });
                                                match_page(cur_page());
                                            };
                                            view! {
                                                <button class="import-picker-result" on:click=pick>
                                                    {cover.map(|src| view! {
                                                        <img class="picker-art" src=src alt="" loading="lazy"/>
                                                    })}
                                                    <span class="picker-title">{label}</span>
                                                    <span class="mono muted">{asin}</span>
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
                        <td class="folder" data-label="Folder / file" title=cand.display_name.clone()>{cand.display_name.clone()}</td>
                        <td data-label="Parsed">{parsed}</td>
                        <td data-label="Kind">{kind}</td>
                        <td data-label="Match">
                            {match (matched, proposed) {
                                (false, _) => view! { <span class="muted">"matching…"</span> }.into_any(),
                                (true, Some(p)) => {
                                    let year = p.year.map(|y| format!(" ({y})")).unwrap_or_default();
                                    view! {
                                        <span>{p.title.clone()} {year} " "</span>
                                        <span class=format!("badge {conf_class}") title=crate::import_common::confidence_title(&confidence)>{confidence.clone()}</span>
                                    }.into_any()
                                }
                                (true, None) => view! {
                                    <span class="muted">{if in_lib { "in library" } else { "search or paste an ASIN" }}</span>
                                }.into_any(),
                            }}
                        </td>
                        <td class="import-actions" data-label="ASIN">
                            <button
                                class="btn-link import-find"
                                disabled=in_lib
                                title="Search the Audible catalog"
                                aria-label="Search the Audible catalog"
                                on:click=toggle_picker
                            >
                                "🔍"
                            </button>
                            <input
                                class="tmdb-input"
                                type="text"
                                placeholder="B08G9PRS1K"
                                aria-label="ASIN"
                                prop:value=row.asin.clone()
                                disabled=in_lib
                                on:change=on_asin
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
            <h2>"Audiobook import"</h2>
            <A href="/audiobooks" attr:class="gear" attr:title="Back to Audiobooks">"←"</A>
        </div>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Scan a folder"</h3>
                    <p class="muted">
                        "Bring existing audiobooks under management. Files are "
                        <strong>"hardlinked"</strong>
                        " into the library's canonical Author/Series/Book layout; "
                        "after a successful link the source files are removed (an "
                        "adoption "
                        <strong>"move"</strong>
                        " — no extra disk is used). Cross-device imports are "
                        "refused — scan through the same mount as the root folder. "
                        "Matching is by Audible ASIN."
                    </p>
                </div>
            </div>
            <div class="form">
                <div class="field">
                    <label>"Library path (as the server sees it, e.g. /mnt/storage/audiobooks)"</label>
                    <div class="checks">
                        <PathPicker value=path placeholder="/mnt/storage/audiobooks"/>
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
                    <p class="muted">"New books are ranked by the built-in audiobook ladder and hardlinked into the library (audiobook subfolder). Paste an Audible ASIN for anything unmatched; untick anything wrong."</p>
                </div>
            </div>

            <div class="import-review">
                {move || (!scanned.get()).then(|| view! {
                    <p class="muted">"Scan a folder above to see candidates."</p>
                })}
                {move || (scanned.get() && rows.with(Vec::is_empty)).then(|| view! {
                    <p class="muted">"No importable audiobooks found under that path."</p>
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
                                <th><span class="sr-only">"Import"</span></th>
                                <th>"Folder / file"</th>
                                <th>"Parsed"</th>
                                <th>"Kind"</th>
                                <th>"Match"</th>
                                <th>"ASIN"</th>
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
