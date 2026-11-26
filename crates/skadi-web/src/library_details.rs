//! Settings → Library details (SKADI-T-0774, design SKADI-D-0040): what the
//! library takes on disk.
//!
//! Three reads, each admin-only on the API side: the totals by kind with the
//! state of the storage sweeps (`/library/storage/summary`), the file list
//! (`/library/storage/files`, sorted, filtered and paged by the daemon) and
//! the files no item holds (`/library/storage/orphans`).
//!
//! The page shows what the mount gives: size, space on disk, hard links.
//! Piece counts and checksums read "not measured" until their collectors run.
//!
//! **The gate is a known-good role**, as on the System page: the panels and
//! their fetches live in a child that only an admin mounts (SKADI-T-0639).

use aurora_leptos::components::{PageRange, Pagination, Select, Table};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::loading::{ListLoad, SkeletonKind};
use crate::movies::size_human;
use crate::system::{NO_VALUE, time_label};

/// Rows of one page of the file list and of the orphans.
pub const PAGE_SIZE: usize = 50;

/// What a piece count or a checksum shows before its collector has run.
pub const NOT_MEASURED: &str = "not measured";

/// The kind filter: `(value, label)`. The empty value is every kind.
pub const KIND_FILTERS: &[(&str, &str)] = &[
    ("", "All kinds"),
    ("movie", "Movies"),
    ("series", "TV"),
    ("audiobook", "Audiobooks"),
];

/// The state filter: `(value, label)`.
pub const STATE_FILTERS: &[(&str, &str)] = &[
    ("present", "On disk"),
    ("gone", "Gone"),
    ("all", "On disk and gone"),
];

/// The sort choices: `(value, label)`. Each sorts largest first, but the path
/// A to Z; the daemon picks that direction when none is sent.
pub const SORTS: &[(&str, &str)] = &[
    ("size", "Largest first"),
    ("disk", "Most space on disk"),
    ("links", "Most hard links"),
    ("mtime", "Newest file first"),
    ("path", "Path, A to Z"),
];

/// Whether this role may open the page: the admin only.
pub fn can_open(role: Option<&str>) -> bool {
    role == Some("admin")
}

/// The name of a kind as the page shows it. A kind this client does not know
/// is shown as the daemon sent it.
pub fn kind_label(kind: &str) -> String {
    KIND_FILTERS
        .iter()
        .find(|(v, _)| !v.is_empty() && *v == kind)
        .map_or_else(|| kind.to_string(), |(_, label)| (*label).to_string())
}

/// A byte count for a table cell. Zero is `0 B`, not a dash: an empty kind
/// has a size, and it is nothing.
pub fn bytes_label(bytes: i64) -> String {
    if bytes <= 0 {
        "0 B".into()
    } else {
        size_human(bytes as u64)
    }
}

/// How many of a kind's files a collector has measured: `not measured` while
/// it has done none, else `120 of 1,818`.
pub fn measured_label(measured: i64, files: i64) -> String {
    if measured <= 0 {
        NOT_MEASURED.into()
    } else {
        format!("{} of {}", count_label(measured), count_label(files))
    }
}

/// A count with thousands separators: `24,014`.
pub fn count_label(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Pieces per GB of the measured files, or `not measured`.
pub fn pieces_per_gb_label(pieces: i64, measured_bytes: i64) -> String {
    if measured_bytes <= 0 {
        NOT_MEASURED.into()
    } else {
        format!("{:.0}", pieces as f64 / (measured_bytes as f64 / 1e9))
    }
}

/// What a sweep is doing, in one line.
pub fn sweep_state(sweep: &api::StorageSweep) -> String {
    if sweep.running {
        "Running now".into()
    } else if let Some(why) = sweep.skipped.as_deref().filter(|w| !w.is_empty()) {
        format!("Last pass not completed: {why}")
    } else if sweep.finished_at.is_some() {
        "Idle".into()
    } else {
        "Waiting for its first pass".into()
    }
}

/// The query string of one page of the file list.
pub fn files_query(kind: &str, state: &str, sort: &str, offset: usize) -> String {
    let mut q = format!("state={state}&sort={sort}&limit={PAGE_SIZE}&offset={offset}");
    if !kind.is_empty() {
        q.push_str(&format!("&kind={kind}"));
    }
    q
}

fn pairs(options: &[(&str, &str)]) -> Vec<(String, String)> {
    options
        .iter()
        .map(|(v, label)| ((*v).to_string(), (*label).to_string()))
        .collect()
}

/// The Library details page. The admin gets the panels; any other known role
/// is told the page is not theirs; an unresolved role sees a loading line and
/// fetches nothing.
#[component]
pub fn LibraryDetailsPage() -> impl IntoView {
    let role = use_context::<crate::subnav::RoleCtx>().map(|r| r.0);
    move || {
        let role = role.and_then(|r| r.get());
        if can_open(role.as_deref()) {
            view! {
                <crate::subnav::SubNav/>
                <div class="page-head"><h2>"Library details"</h2></div>
                <LibraryDetailsPanels/>
            }
            .into_any()
        } else if role.is_none() {
            view! { <p class="muted">"Loading…"</p> }.into_any()
        } else {
            view! { <p class="muted">"This page is for the household admin."</p> }.into_any()
        }
    }
}

/// The panels and their fetches. Mounted for the admin only.
#[component]
fn LibraryDetailsPanels() -> impl IntoView {
    let summary = RwSignal::new(None::<api::StorageSummary>);
    let summary_load = ListLoad::new();
    let files = RwSignal::new(api::StorageFilesPage::default());
    let files_load = ListLoad::new();
    let orphans = RwSignal::new(api::StorageOrphansPage::default());
    let orphans_load = ListLoad::new();

    let kind = RwSignal::new(String::new());
    let state = RwSignal::new(String::from("present"));
    let sort = RwSignal::new(String::from("size"));
    let offset = RwSignal::new(0usize);
    let limit = RwSignal::new(PAGE_SIZE);
    let orphan_offset = RwSignal::new(0usize);

    let load_summary = move || {
        spawn_local(async move {
            if let Some(s) =
                summary_load.settle(api::library_storage_summary().await.map_err(|e| e.0))
            {
                let _ = summary.try_set(Some(s));
            }
        });
    };
    let load_files = move || {
        let query = files_query(
            &kind.get_untracked(),
            &state.get_untracked(),
            &sort.get_untracked(),
            offset.get_untracked(),
        );
        spawn_local(async move {
            if let Some(page) =
                files_load.settle(api::library_storage_files(&query).await.map_err(|e| e.0))
            {
                let _ = files.try_set(page);
            }
        });
    };
    let load_orphans = move || {
        let at = orphan_offset.get_untracked();
        spawn_local(async move {
            if let Some(page) = orphans_load.settle(
                api::library_storage_orphans(PAGE_SIZE, at)
                    .await
                    .map_err(|e| e.0),
            ) {
                let _ = orphans.try_set(page);
            }
        });
    };

    Effect::new(move |_| {
        load_summary();
        load_orphans();
    });
    // A new filter or sort starts at the first page; any of the four fetches
    // the list again. The daemon sorts and pages: the page never holds more
    // than one page of rows.
    Effect::new(move |prev: Option<(String, String, String)>| {
        let now = (kind.get(), state.get(), sort.get());
        if prev.as_ref().is_some_and(|p| *p != now) && offset.get_untracked() != 0 {
            // The offset effect below does the fetch.
            offset.set(0);
        } else {
            load_files();
        }
        now
    });
    Effect::new(move |prev: Option<usize>| {
        let at = offset.get();
        if prev.is_some_and(|p| p != at) {
            load_files();
        }
        at
    });

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Totals"</h3>
                    <p class="muted">
                        "What each kind takes on disk, from the last reading of the library."
                    </p>
                </div>
            </div>
            {summary_load.status_n(SkeletonKind::Rows, Some(3), Callback::new(move |()| load_summary()))}
            {move || summary.get().map(|s| view! {
                <SpaceLine summary=s.clone()/>
                <KindTable kinds=s.kinds.clone() orphans=s.orphans.clone()/>
                <h3 class="storage-subhead">"Readings"</h3>
                <p class="muted">
                    "The daemon reads the size and the hard links of every file on a timer."
                </p>
                <SweepTable sweeps=s.sweeps.clone() kinds=s.kinds/>
            })}
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Files"</h3>
                    <p class="muted">"Every file the library holds."</p>
                </div>
                <div class="page-head-actions">
                    <Select aria_label="Kind" value=kind option_pairs=pairs(KIND_FILTERS)/>
                    <Select aria_label="State" value=state option_pairs=pairs(STATE_FILTERS)/>
                    <Select aria_label="Sort" value=sort option_pairs=pairs(SORTS)/>
                </div>
            </div>
            {files_load.status(SkeletonKind::Rows, Callback::new(move |()| load_files()))}
            {move || files_load.is_loaded().then(|| view! {
                <FileTable files=files.with(|p| p.files.clone())/>
                <div class="checks">
                    <Pagination offset=offset limit=limit
                        total=Signal::derive(move || files.with(|p| p.total.max(0) as usize))
                        range_label=Callback::new(|r: PageRange| {
                            format!("{}–{} of {} files", r.first, r.last, r.total)
                        })
                        on_change=Callback::new(move |(off, _): (usize, usize)| offset.set(off))/>
                </div>
            })}
        </section>

        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Files with no item"</h3>
                    <p class="muted">
                        "Media files in the library folders that no movie, episode or book holds. "
                        "Skadi lists them and changes none."
                    </p>
                </div>
            </div>
            {orphans_load.status(SkeletonKind::Rows, Callback::new(move |()| load_orphans()))}
            {move || orphans_load.is_loaded().then(|| view! {
                <OrphanTable orphans=orphans.with(|p| p.orphans.clone())/>
                <div class="checks">
                    <Pagination offset=orphan_offset limit=limit
                        total=Signal::derive(move || orphans.with(|p| p.total.max(0) as usize))
                        range_label=Callback::new(|r: PageRange| {
                            format!("{}–{} of {} files", r.first, r.last, r.total)
                        })
                        on_change=Callback::new(move |(off, _): (usize, usize)| {
                            orphan_offset.set(off);
                            load_orphans();
                        })/>
                </div>
            })}
        </section>
    }
}

/// Free space of the library's filesystem, when the daemon could read it.
#[component]
pub fn SpaceLine(summary: api::StorageSummary) -> impl IntoView {
    match (summary.free_bytes, summary.total_bytes) {
        (Some(free), Some(total)) if total > 0 => view! {
            <p class="storage-space">
                {format!("{} free of {} on the library disk.", size_human(free), size_human(total))}
            </p>
        }
        .into_any(),
        _ => view! {
            <p class="storage-space muted">"The free space of the library disk is not known."</p>
        }
        .into_any(),
    }
}

/// One row per kind, a total row, and the orphans.
#[component]
pub fn KindTable(kinds: Vec<api::StorageKind>, orphans: api::StorageOrphanTotals) -> impl IntoView {
    if kinds.is_empty() {
        return view! {
            <p class="muted">
                "No file has been read yet. The first reading starts a few minutes after the daemon starts."
            </p>
        }
        .into_any();
    }
    let sum = |f: fn(&api::StorageKind) -> i64| kinds.iter().map(f).sum::<i64>();
    let total = api::StorageKind {
        kind: "All".into(),
        files: sum(|k| k.files),
        size_bytes: sum(|k| k.size_bytes),
        disk_bytes: sum(|k| k.disk_bytes),
        linked: sum(|k| k.linked),
        measured: sum(|k| k.measured),
        measured_bytes: sum(|k| k.measured_bytes),
        pieces: sum(|k| k.pieces),
        checksummed: sum(|k| k.checksummed),
        gone: sum(|k| k.gone),
        last_read_at: None,
    };
    let row = |k: &api::StorageKind, class: &'static str| {
        view! {
            <tr class=class>
                <td data-label="Kind">{kind_label(&k.kind)}</td>
                <td class="nowrap num" data-label="Files">{count_label(k.files)}</td>
                <td class="nowrap num" data-label="Size">{bytes_label(k.size_bytes)}</td>
                <td class="nowrap num" data-label="On disk">{bytes_label(k.disk_bytes)}</td>
                <td class="nowrap num" data-label="Hard-linked">{count_label(k.linked)}</td>
                <td class="nowrap num" data-label="Gone">{count_label(k.gone)}</td>
                <td class="nowrap muted" data-label="Pieces per GB">
                    {pieces_per_gb_label(k.pieces, k.measured_bytes)}
                </td>
                <td class="nowrap muted" data-label="Checksums">
                    {measured_label(k.checksummed, k.files)}
                </td>
            </tr>
        }
    };
    let rows = kinds
        .iter()
        .map(|k| row(k, "storage-kind-row"))
        .collect_view();
    view! {
        <Table class:system-table=true class:storage-kinds=true>
            <thead>
                <tr>
                    <th>"Kind"</th>
                    <th>"Files"</th>
                    <th>"Size"</th>
                    <th>"On disk"</th>
                    <th>"Hard-linked"</th>
                    <th>"Gone"</th>
                    <th>"Pieces per GB"</th>
                    <th>"Checksums"</th>
                </tr>
            </thead>
            <tbody>
                {rows}
                {row(&total, "storage-kind-row storage-total-row")}
            </tbody>
        </Table>
        <p class="storage-orphans-line muted">
            {format!(
                "{} files with no item, {}.",
                count_label(orphans.files),
                bytes_label(orphans.size_bytes)
            )}
        </p>
    }
    .into_any()
}

/// The storage sweeps: what runs now, and when each kind was last read.
#[component]
pub fn SweepTable(sweeps: Vec<api::StorageSweep>, kinds: Vec<api::StorageKind>) -> impl IntoView {
    if sweeps.is_empty() {
        return view! { <p class="muted">"No reading has started since the daemon started."</p> }
            .into_any();
    }
    let rows = sweeps
        .into_iter()
        .map(|s| {
            // The time a kind was last read comes from the file rows, so it
            // is known after a restart, before the first pass of this run.
            let last_read = kinds
                .iter()
                .find(|k| k.kind == s.kind)
                .and_then(|k| k.last_read_at.clone());
            let found = s.last.as_ref().map_or_else(
                || NO_VALUE.to_string(),
                |r| {
                    format!(
                        "{} files, {} gone, {} with no item",
                        count_label(r.files as i64),
                        count_label(r.gone as i64),
                        count_label(r.orphans as i64)
                    )
                },
            );
            let class = if s.running {
                "sweep-row running"
            } else {
                "sweep-row"
            };
            view! {
                <tr class=class>
                    <td data-label="Kind">{kind_label(&s.kind)}</td>
                    <td class="sweep-state" data-label="State">{sweep_state(&s)}</td>
                    <td class="nowrap mono muted" data-label="Last read (UTC)">
                        {time_label(last_read.as_deref())}
                    </td>
                    <td class="muted" data-label="Last pass found">{found}</td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <Table class:system-table=true class:storage-sweeps=true>
            <thead>
                <tr>
                    <th>"Kind"</th>
                    <th>"State"</th>
                    <th>"Last read (UTC)"</th>
                    <th>"Last pass found"</th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </Table>
    }
    .into_any()
}

/// One page of the file list.
#[component]
pub fn FileTable(files: Vec<api::StorageFile>) -> impl IntoView {
    if files.is_empty() {
        return view! { <p class="muted">"No files match."</p> }.into_any();
    }
    let rows = files
        .into_iter()
        .map(|f| {
            let pieces = match (f.pieces, f.pieces_at.as_deref()) {
                (Some(n), Some(_)) => count_label(n),
                _ => NOT_MEASURED.to_string(),
            };
            let checksum = if f.checksum_at.is_some() {
                "yes"
            } else {
                NOT_MEASURED
            };
            let class = if f.gone_at.is_some() {
                "storage-file-row gone"
            } else {
                "storage-file-row"
            };
            view! {
                <tr class=class>
                    <td class="storage-path mono" data-label="File">
                        {f.path.clone()}
                        {f.gone_at.as_ref().map(|at| view! {
                            <div class="muted storage-gone">
                                {format!("Not found since {} UTC", time_label(Some(at)))}
                            </div>
                        })}
                    </td>
                    <td class="nowrap" data-label="Kind">{kind_label(&f.kind)}</td>
                    <td class="nowrap num" data-label="Size">{bytes_label(f.size_bytes)}</td>
                    <td class="nowrap num" data-label="On disk">{bytes_label(f.disk_bytes)}</td>
                    <td class="nowrap num" data-label="Links">{count_label(f.links)}</td>
                    <td class="nowrap muted" data-label="Pieces">{pieces}</td>
                    <td class="nowrap muted" data-label="Checksum">{checksum}</td>
                    <td class="nowrap mono muted" data-label="Changed (UTC)">
                        {time_label(Some(&f.mtime))}
                    </td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <Table class:system-table=true class:storage-files=true>
            <thead>
                <tr>
                    <th>"File"</th>
                    <th>"Kind"</th>
                    <th>"Size"</th>
                    <th>"On disk"</th>
                    <th>"Links"</th>
                    <th>"Pieces"</th>
                    <th>"Checksum"</th>
                    <th>"Changed (UTC)"</th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </Table>
    }
    .into_any()
}

/// One page of the files no item holds.
#[component]
pub fn OrphanTable(orphans: Vec<api::StorageOrphan>) -> impl IntoView {
    if orphans.is_empty() {
        return view! { <p class="muted">"Every media file in the library folders belongs to an item."</p> }
            .into_any();
    }
    let rows = orphans
        .into_iter()
        .map(|o| {
            view! {
                <tr class="storage-orphan-row">
                    <td class="storage-path mono" data-label="File">{o.path.clone()}</td>
                    <td class="nowrap num" data-label="Size">{bytes_label(o.size_bytes)}</td>
                    <td class="nowrap mono muted" data-label="Changed (UTC)">
                        {time_label(Some(&o.mtime))}
                    </td>
                    <td class="nowrap mono muted" data-label="First seen (UTC)">
                        {time_label(Some(&o.first_seen))}
                    </td>
                </tr>
            }
        })
        .collect_view();
    view! {
        <Table class:system-table=true class:storage-orphans=true>
            <thead>
                <tr>
                    <th>"File"</th>
                    <th>"Size"</th>
                    <th>"Changed (UTC)"</th>
                    <th>"First seen (UTC)"</th>
                </tr>
            </thead>
            <tbody>{rows}</tbody>
        </Table>
    }
    .into_any()
}
