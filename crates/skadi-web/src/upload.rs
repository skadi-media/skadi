//! Upload media from this browser (SKADI-T-0631, initiative SKADI-I-0062).
//!
//! The counterpart to `path_picker`, which browses the **server's** filesystem.
//! This one takes a file from the machine you are sitting at, sends it in
//! chunks, and hands the finished path to library-import, which does the
//! matching and placing exactly as it does for a file already on disk.
//!
//! ## Resuming
//!
//! Every chunk is a `Blob` slice of the `File`, so only the piece in flight is
//! ever in memory — the browser reads the rest from disk as it goes. Progress
//! is per chunk rather than per byte: with an 8 MB chunk the bar still moves
//! often enough to read as live, and it avoids an `XmlHttpRequest` and its
//! callback plumbing for a smoothness nobody asked for.
//!
//! The resume point **always comes from the server**. A client that trusted
//! its own memory of how far it had got would append into the wrong place
//! after a failure that landed server-side but never answered.

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;

use crate::api;

/// What the operator picked, and how far it has got.
#[derive(Clone)]
struct Job {
    file: web_sys::File,
    kind: String,
    session: RwSignal<Option<api::UploadSession>>,
    sent: RwSignal<f64>,
    state: RwSignal<JobState>,
    error: RwSignal<Option<String>>,
    done_path: RwSignal<Option<String>>,
}

#[derive(Clone, Copy, PartialEq)]
enum JobState {
    Waiting,
    Sending,
    Finishing,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    fn label(self) -> &'static str {
        match self {
            JobState::Waiting => "waiting",
            JobState::Sending => "uploading",
            JobState::Finishing => "checking",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// How many times a chunk is retried before the upload is called failed.
///
/// A dropped connection is the case this feature exists for, so giving up on
/// the first failure would defeat the point.
const CHUNK_RETRIES: usize = 4;

/// Human file size, matching the movies page's formatting.
fn size_human(bytes: f64) -> String {
    crate::movies::size_human(bytes as u64)
}

/// Send `file` from `from` bytes onward, updating `job` as it goes.
///
/// Returns `Err` only when the upload cannot continue; a chunk that fails
/// transiently is retried with a widening delay first.
async fn send_from(job: Job, id: String, from: f64) -> Result<(), String> {
    let total = job.file.size();
    let chunk = job
        .session
        .get_untracked()
        .map(|s| s.chunk_bytes as f64)
        .unwrap_or(8.0 * 1024.0 * 1024.0);

    let mut offset = from;
    while offset < total {
        if job.state.get_untracked() == JobState::Cancelled {
            return Ok(());
        }
        let end = (offset + chunk).min(total);
        let slice = job
            .file
            .slice_with_f64_and_f64(offset, end)
            .map_err(|_| "could not read that part of the file".to_string())?;

        let mut attempt = 0;
        let sent = loop {
            match api::put_upload_chunk(&id, offset, &slice).await {
                Ok(s) => break s,
                Err(e) => {
                    attempt += 1;
                    if attempt > CHUNK_RETRIES {
                        return Err(e.to_string());
                    }
                    // Widening backoff, then ask the server where it actually
                    // got to rather than assuming the chunk did not land — a
                    // request that succeeded but whose response was lost would
                    // otherwise be re-sent from the wrong offset.
                    gloo_timers::future::TimeoutFuture::new(500 * attempt as u32).await;
                    if let Ok(s) = api::upload_status(&id).await {
                        offset = s.received_bytes as f64;
                        job.sent.set(offset);
                    }
                    continue;
                }
            }
        };
        offset = sent.received_bytes as f64;
        job.sent.set(offset);
        job.session.set(Some(sent));
    }
    Ok(())
}

/// Drive one file from "picked" to "staged".
async fn run(job: Job, kind: String) {
    job.state.set(JobState::Sending);
    job.error.set(None);

    let session = match api::open_upload(&job.file.name(), job.file.size(), &kind).await {
        Ok(s) => s,
        Err(e) => {
            job.error.set(Some(e.to_string()));
            job.state.set(JobState::Failed);
            return;
        }
    };
    let id = session.id.clone();
    job.sent.set(session.received_bytes as f64);
    job.session.set(Some(session));

    if let Err(e) = send_from(job.clone(), id.clone(), job.sent.get_untracked()).await {
        job.error.set(Some(e));
        job.state.set(JobState::Failed);
        return;
    }
    if job.state.get_untracked() == JobState::Cancelled {
        return;
    }

    job.state.set(JobState::Finishing);
    match api::complete_upload(&id).await {
        Ok(done) => {
            job.done_path.set(Some(done.path));
            job.state.set(JobState::Done);
        }
        Err(e) => {
            job.error.set(Some(e.to_string()));
            job.state.set(JobState::Failed);
        }
    }
}

/// `/upload` — pick files, watch them go.
#[component]
pub fn UploadPage() -> impl IntoView {
    let kind = RwSignal::new("movie".to_string());
    let jobs = RwSignal::new(Vec::<Job>::new());
    let resumable = RwSignal::new(Vec::<api::UploadSession>::new());
    let notice = RwSignal::new(None::<String>);

    // Sessions still on the server from a previous visit. The bytes are there;
    // the browser just has to be pointed at the same file again.
    let reload_sessions = move || {
        spawn_local(async move {
            if let Ok(list) = api::list_uploads().await {
                resumable.set(list);
            }
        });
    };
    Effect::new(move |_| reload_sessions());

    let on_pick = move |ev: web_sys::Event| {
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(files) = input.files() else { return };
        let picked = kind.get_untracked();
        for i in 0..files.length() {
            let Some(file) = files.get(i) else { continue };
            let job = Job {
                file,
                kind: picked.clone(),
                session: RwSignal::new(None),
                sent: RwSignal::new(0.0),
                state: RwSignal::new(JobState::Waiting),
                error: RwSignal::new(None),
                done_path: RwSignal::new(None),
            };
            jobs.update(|v| v.push(job.clone()));
            let k = picked.clone();
            spawn_local(async move {
                run(job, k).await;
            });
        }
        // Let the same file be picked again after a cancel.
        input.set_value("");
    };

    let kinds = [
        ("movie", "Film"),
        ("series", "TV episode"),
        ("audiobook", "Audiobook"),
    ];

    view! {
        <section class="upload-page">
            <crate::subnav::SubNav/>
            <div class="page-head"><h2>"Upload"</h2></div>
            <p class="muted">
                "Send a file from this device. Skadi checks it is playable, then \
                 hands it to the import page for the right library, where you \
                 search for the title exactly as you would for a file already on \
                 the server."
            </p>

            <div class="upload-controls">
                <label class="upload-kind">
                    <span class="muted">"What is it?"</span>
                    <select on:change=move |ev| kind.set(event_target_value(&ev))>
                        {kinds
                            .iter()
                            .map(|(v, label)| {
                                view! { <option value=*v>{*label}</option> }
                            })
                            .collect_view()}
                    </select>
                </label>
                <label class="upload-pick btn-link">
                    "Choose files…"
                    <input type="file" multiple on:change=on_pick style="display:none"/>
                </label>
            </div>

            // Audnexus has no title search, so an audiobook can only be
            // matched by ASIN — existing behaviour, but an uploader meeting it
            // for the first time deserves to be told before they send 400 MB.
            {move || {
                (kind.get() == "audiobook")
                    .then(|| {
                        view! {
                            <p class="pending">
                                "Audiobooks are matched by ASIN — skadi cannot search \
                                 Audible by title. Have the ASIN to hand for the \
                                 identify step, or the book cannot be committed."
                            </p>
                        }
                    })
            }}

            {move || notice.get().map(|n| view! { <p class="muted">{n}</p> })}

            // Sessions the server still holds from a previous visit. The bytes
            // survive a closed tab; the File handle does not, so resuming means
            // picking the same file again.
            {move || {
                let live: Vec<_> = resumable
                    .get()
                    .into_iter()
                    .filter(|s| s.received_bytes < s.size_bytes)
                    .collect();
                (!live.is_empty())
                    .then(|| {
                        view! {
                            <div class="upload-resumable">
                                <h3>"Unfinished uploads"</h3>
                                <p class="muted">
                                    "Skadi still holds part of these. Choose the same \
                                     file again and it carries on from where it stopped."
                                </p>
                                <ul>
                                    {live
                                        .into_iter()
                                        .map(|s| {
                                            let id = s.id.clone();
                                            let pct = if s.size_bytes > 0 {
                                                (s.received_bytes as f64 / s.size_bytes as f64
                                                    * 100.0) as u32
                                            } else {
                                                0
                                            };
                                            view! {
                                                <li>
                                                    <span class="mono">{s.filename.clone()}</span>
                                                    <span class="muted">
                                                        {format!(
                                                            " {} of {} ({pct}%)",
                                                            size_human(s.received_bytes as f64),
                                                            size_human(s.size_bytes as f64),
                                                        )}
                                                    </span>
                                                    <button
                                                        type="button"
                                                        class="btn-link"
                                                        on:click=move |_| {
                                                            let id = id.clone();
                                                            spawn_local(async move {
                                                                let _ = api::delete_upload(&id).await;
                                                                if let Ok(list) = api::list_uploads().await {
                                                                    resumable.set(list);
                                                                }
                                                            });
                                                        }
                                                    >
                                                        "Discard"
                                                    </button>
                                                </li>
                                            }
                                        })
                                        .collect_view()}
                                </ul>
                            </div>
                        }
                    })
            }}

            <div class="upload-jobs">
                {move || {
                    jobs.get()
                        .into_iter()
                        .map(|job| view! { <JobRow job=job/> })
                        .collect_view()
                }}
            </div>
        </section>
    }
}

/// One file's progress, and where it went.
#[component]
fn JobRow(job: Job) -> impl IntoView {
    let name = job.file.name();
    let total = job.file.size();
    let j = job.clone();
    let pct = move || {
        if total <= 0.0 {
            return 0u32;
        }
        ((j.sent.get() / total) * 100.0).clamp(0.0, 100.0) as u32
    };
    let j = job.clone();
    let cancel = move |_| {
        j.state.set(JobState::Cancelled);
        if let Some(s) = j.session.get_untracked() {
            spawn_local(async move {
                let _ = api::delete_upload(&s.id).await;
            });
        }
    };
    let state = job.state;
    let error = job.error;
    let done_path = job.done_path;
    let kind = job.kind.clone();

    view! {
        <div class="upload-job">
            <div class="upload-job-head">
                <span class="upload-job-name">{name}</span>
                <span class="muted">{size_human(total)}</span>
                <span class=move || format!("badge {}", match state.get() {
                    JobState::Done => "ok",
                    JobState::Failed => "bad",
                    _ => "muted",
                })>{move || state.get().label()}</span>
                {move || {
                    matches!(state.get(), JobState::Sending | JobState::Waiting)
                        .then(|| {
                            view! {
                                <button type="button" class="btn-link" on:click=cancel.clone()>
                                    "Cancel"
                                </button>
                            }
                        })
                }}
            </div>
            <div class="upload-track">
                <div class="upload-fill" style=move || format!("width:{}%", pct())></div>
            </div>
            {move || error.get().map(|e| view! { <p class="bad">{e}</p> })}
            {move || {
                done_path
                    .get()
                    .map(|path| {
                        // The hand-off. Identity is library-import's job, not
                        // this page's (SKADI-T-0632).
                        let href = crate::upload::import_href(&kind, &path);
                        view! {
                            <p class="ok">
                                "Staged. "
                                <a href=href>"Identify it now →"</a>
                            </p>
                        }
                    })
            }}
        </div>
    }
}

/// Where to send the operator to identify a staged file.
///
/// The import pages take a server path, which is exactly what `complete`
/// answered, so the hand-off is a query parameter rather than any new
/// machinery.
pub fn import_href(kind: &str, path: &str) -> String {
    let page = match kind {
        "series" => "/tv/import",
        "audiobook" => "/audiobooks/import",
        _ => "/movies/import",
    };
    format!("{page}?path={}", percent_encode(path))
}

/// Percent-encode a query value.
///
/// Hand-rolled rather than `js_sys::encode_uri_component`, which panics off
/// wasm and so cannot be unit-tested on the host target — and this is exactly
/// the kind of string-mangling that deserves a test, since staged paths carry
/// the uploaded filename and those are full of spaces and brackets.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode a percent-encoded query value.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        // `+` is NOT treated as a space: these are paths, and a filename with a
        // plus in it is ordinary. The encoder never emits a bare `+` either.
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The staged path an upload handed to an import page, from its query string.
///
/// This is the whole of the hand-off (SKADI-T-0632). The import pages already
/// take a server path and do the scanning, matching and committing; an upload
/// just arranges for that path to be filled in. Nothing about an uploaded file
/// is special once it is on disk, and that is the point.
pub fn staged_path_from_query(search: &str) -> Option<String> {
    let q = search.trim_start_matches('?');
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("path=") {
            let decoded = percent_decode(v);
            if !decoded.trim().is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hand_off_round_trips_a_real_staged_path() {
        // The property that matters: whatever `import_href` encodes, the
        // import page decodes back to the exact path on disk. A path that
        // comes back subtly different scans nothing and looks like an empty
        // staging directory.
        for path in [
            "/data/incoming/9f/Some Film (2024).mkv",
            "/data/incoming/9f/Book #3 - Author's Cut.m4b",
            "/data/incoming/9f/Café & Bar.mkv",
            "/data/incoming/9f/a+b.mkv",
        ] {
            let href = import_href("movie", path);
            let query = href.split_once('?').unwrap().1;
            assert_eq!(
                staged_path_from_query(query).as_deref(),
                Some(path),
                "round trip failed for {path}"
            );
        }
    }

    #[test]
    fn a_page_opened_without_an_upload_has_no_staged_path() {
        assert_eq!(staged_path_from_query(""), None);
        assert_eq!(staged_path_from_query("?"), None);
        assert_eq!(staged_path_from_query("?other=1"), None);
        // An empty value is not a path, and scanning "" would be an error the
        // operator did not cause.
        assert_eq!(staged_path_from_query("?path="), None);
        assert_eq!(staged_path_from_query("?path=%20"), None);
    }

    #[test]
    fn the_path_is_found_wherever_it_sits_in_the_query() {
        assert_eq!(
            staged_path_from_query("?a=1&path=%2Fx%2Fy.mkv&b=2").as_deref(),
            Some("/x/y.mkv")
        );
    }

    #[test]
    fn each_kind_goes_to_its_own_import_page() {
        assert!(import_href("movie", "/x/a.mkv").starts_with("/movies/import?path="));
        assert!(import_href("series", "/x/a.mkv").starts_with("/tv/import?path="));
        assert!(import_href("audiobook", "/x/a.m4b").starts_with("/audiobooks/import?path="));
    }

    #[test]
    fn the_encoder_leaves_a_path_readable_and_escapes_the_rest() {
        // Slashes stay, because the value is a path and escaping them makes it
        // unreadable in a log or an address bar for no benefit.
        assert_eq!(percent_encode("/data/a-b_c.mkv"), "/data/a-b_c.mkv");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("(2024)"), "%282024%29");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        // Multi-byte characters are encoded per UTF-8 byte.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn a_path_with_spaces_survives_the_hand_off() {
        // Staged paths carry the uploaded filename, which very often has
        // spaces and brackets in it.
        let href = import_href("movie", "/data/incoming/9f/Some Film (2024).mkv");
        assert!(href.contains("Some%20Film"), "{href}");
        assert!(!href.contains(' '), "{href}");
    }
}
