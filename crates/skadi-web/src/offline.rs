//! On-device book storage for the offline player (SKADI-T-0332 / I-0048).
//!
//! Books live in **OPFS** (Origin Private File System): `{fid}.m4b` plus a
//! single `index.json` catalog holding every downloaded book's metadata
//! snapshot (title/authors/series/cover/chapters/size). The index file
//! replaces the planned IndexedDB catalog — enumerating a self-maintained
//! JSON file is simpler than IndexedDB boilerplate and avoids web-sys's
//! unstable async-iterator bindings for directory listing. Playback positions
//! stay in localStorage (see `player.rs`).
//!
//! Everything is async over JS promises; failures return `Err(String)` and
//! callers degrade gracefully (a failed save just means "not available
//! offline yet").

use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::api;
use leptos::component;

/// Everything the player needs to work with NO network: the metadata snapshot
/// stored beside the audio at download time.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OfflineBook {
    pub book_id: String,
    pub file_id: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub narrators: Vec<String>,
    #[serde(default)]
    pub series_name: Option<String>,
    #[serde(default)]
    pub series_position: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub size_bytes: f64,
    #[serde(default)]
    pub chapters: Vec<api::Chapter>,
}

fn err(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

async fn opfs_root() -> Result<web_sys::FileSystemDirectoryHandle, String> {
    let storage = web_sys::window().ok_or("no window")?.navigator().storage();
    JsFuture::from(storage.get_directory())
        .await
        .map_err(err)?
        .dyn_into::<web_sys::FileSystemDirectoryHandle>()
        .map_err(|_| "OPFS unavailable".into())
}

/// Ask the browser to protect this origin's storage from eviction (Android
/// honors this for installed/engaged PWAs). Returns whether it's granted.
pub async fn request_persistence() -> bool {
    let Some(w) = web_sys::window() else {
        return false;
    };
    let storage = w.navigator().storage();
    match JsFuture::from(
        storage
            .persist()
            .unwrap_or_else(|_| js_sys::Promise::resolve(&JsValue::FALSE)),
    )
    .await
    {
        Ok(v) => v.as_bool().unwrap_or(false),
        Err(_) => false,
    }
}

async fn file_handle(name: &str, create: bool) -> Result<web_sys::FileSystemFileHandle, String> {
    let root = opfs_root().await?;
    let opts = web_sys::FileSystemGetFileOptions::new();
    opts.set_create(create);
    JsFuture::from(root.get_file_handle_with_options(name, &opts))
        .await
        .map_err(err)?
        .dyn_into::<web_sys::FileSystemFileHandle>()
        .map_err(|_| "not a file handle".into())
}

async fn writable(name: &str) -> Result<web_sys::FileSystemWritableFileStream, String> {
    let handle = file_handle(name, true).await?;
    JsFuture::from(handle.create_writable())
        .await
        .map_err(err)?
        .dyn_into()
        .map_err(|_| "not a writable stream".into())
}

async fn write_blob(name: &str, data: &web_sys::Blob) -> Result<(), String> {
    let stream = writable(name).await?;
    JsFuture::from(stream.write_with_blob(data).map_err(err)?)
        .await
        .map_err(err)?;
    JsFuture::from(stream.close()).await.map_err(err)?;
    Ok(())
}

async fn write_text(name: &str, data: &str) -> Result<(), String> {
    let stream = writable(name).await?;
    JsFuture::from(stream.write_with_str(data).map_err(err)?)
        .await
        .map_err(err)?;
    JsFuture::from(stream.close()).await.map_err(err)?;
    Ok(())
}

/// The stored audio for a book file, as a `File` (a `Blob`) — `None` when the
/// book isn't on this device.
pub async fn audio_file(fid: &str) -> Option<web_sys::File> {
    let handle = file_handle(&format!("{fid}.m4b"), false).await.ok()?;
    JsFuture::from(handle.get_file())
        .await
        .ok()?
        .dyn_into()
        .ok()
}

async fn read_text(name: &str) -> Result<String, String> {
    let handle = file_handle(name, false).await?;
    let file: web_sys::File = JsFuture::from(handle.get_file())
        .await
        .map_err(err)?
        .dyn_into()
        .map_err(|_| "not a file".to_string())?;
    let text = JsFuture::from(file.text()).await.map_err(err)?;
    text.as_string().ok_or_else(|| "not text".into())
}

const INDEX: &str = "index.json";

/// All books stored on this device.
pub async fn read_index() -> Vec<OfflineBook> {
    match read_text(INDEX).await {
        Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

async fn write_index(list: &[OfflineBook]) -> Result<(), String> {
    let json = serde_json::to_string(list).map_err(|e| e.to_string())?;
    write_text(INDEX, &json).await
}

/// Run `body` while holding the cross-tab `navigator.locks` exclusive lock on
/// the index (review pass 2, web finding 4): the pairing flow explicitly
/// produces a PWA window PLUS a browser tab, and two interleaved index
/// read-modify-writes would drop entries. Degrades to running `body` directly
/// where the Web Locks API is absent.
async fn with_index_lock<F, Fut>(body: F) -> Result<(), String>
where
    F: FnOnce() -> Fut + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + 'static,
{
    let locks = web_sys::window()
        .and_then(|w| js_sys::Reflect::get(w.navigator().as_ref(), &"locks".into()).ok())
        .filter(|v| !v.is_undefined() && !v.is_null());
    let Some(locks) = locks else {
        return body().await;
    };
    let request = match js_sys::Reflect::get(&locks, &"request".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
    {
        Some(f) => f,
        None => return body().await,
    };

    // The callback holds the lock until its returned promise settles; we bridge
    // our Rust future into that promise and shuttle the Result out via a cell.
    use std::cell::RefCell;
    use std::rc::Rc;
    let out: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
    let out2 = out.clone();
    let mut body_holder = Some(body);
    let cb = Closure::once(move |_lock: JsValue| -> js_sys::Promise {
        let body = body_holder.take().unwrap();
        let out3 = out2.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            *out3.borrow_mut() = Some(body().await);
            Ok(JsValue::UNDEFINED)
        })
    });
    let promise = request
        .call2(
            &locks,
            &JsValue::from_str(INDEX),
            cb.as_ref().unchecked_ref(),
        )
        .map_err(err)?;
    JsFuture::from(js_sys::Promise::from(promise))
        .await
        .map_err(err)?;
    out.borrow_mut()
        .take()
        .unwrap_or(Err("index lock body did not run".into()))
}

/// One stored book's index entry, if present.
pub async fn index_entry(fid: &str) -> Option<OfflineBook> {
    read_index().await.into_iter().find(|b| b.file_id == fid)
}

/// Persist a book on-device: audio blob + catalog entry (idempotent per fid).
pub async fn save_book(meta: OfflineBook, audio: &web_sys::Blob) -> Result<(), String> {
    write_blob(&format!("{}.m4b", meta.file_id), audio).await?;
    with_index_lock(move || async move {
        let mut list = read_index().await;
        list.retain(|b| b.file_id != meta.file_id);
        list.push(meta);
        write_index(&list).await
    })
    .await
}

/// Remove a book from the device (audio + catalog entry).
pub async fn delete_book(fid: &str) -> Result<(), String> {
    let root = opfs_root().await?;
    let _ = JsFuture::from(root.remove_entry(&format!("{fid}.m4b"))).await;
    let fid = fid.to_string();
    with_index_lock(move || async move {
        let mut list = read_index().await;
        list.retain(|b| b.file_id != fid);
        write_index(&list).await
    })
    .await
}

/// Build the offline snapshot from live API data at download time.
pub fn snapshot(b: &api::Book, fid: &str, chapters: Vec<api::Chapter>, size: f64) -> OfflineBook {
    OfflineBook {
        book_id: b.id.clone(),
        file_id: fid.to_string(),
        title: b.title.clone(),
        authors: b.authors.clone(),
        narrators: b.narrators.clone(),
        series_name: b.series.as_ref().map(|s| s.name.clone()),
        series_position: b.series.as_ref().and_then(|s| s.position.clone()),
        cover_url: b.cover_url.clone(),
        size_bytes: size,
        chapters,
    }
}

/// `/listen` — the on-device shelf (SKADI-T-0332): what's downloaded, how big,
/// delete, and tap-to-play. T-0335 adds Continue Listening + the storage meter.
#[component]
pub fn ListenPage() -> impl leptos::IntoView {
    use leptos::prelude::*;
    use leptos_router::components::A;
    let books = RwSignal::new(Vec::<OfflineBook>::new());
    let loaded = RwSignal::new(false);
    let reload = move || {
        leptos::task::spawn_local(async move {
            let list = read_index().await;
            let _ = books.try_set(list);
            let _ = loaded.try_set(true);
        });
    };
    Effect::new(move |_| reload());

    let shelf = move || {
        let list = books.get();
        if !loaded.get() {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        }
        if list.is_empty() {
            return view! {
                <p class="muted">
                    "Nothing downloaded yet — open a book in "
                    <A href="/audiobooks">"Audiobooks"</A>
                    " and press ▶ Listen: playing a book stores it on this device."
                </p>
            }
            .into_any();
        }
        list.into_iter()
            .map(|b| {
                let href = format!("/listen/{}/{}", b.book_id, b.file_id);
                let fid = b.file_id.clone();
                let del = move |_| {
                    let fid = fid.clone();
                    leptos::task::spawn_local(async move {
                        let _ = delete_book(&fid).await;
                        reload();
                    });
                };
                let cover = b.cover_url.clone().filter(|u| !u.is_empty());
                let size = crate::movies::size_human(b.size_bytes as u64);
                let byline = b.authors.join(", ");
                view! {
                    <div class="shelf-row">
                        <A href=href attr:class="shelf-main">
                            {match cover {
                                Some(src) => view! { <img class="shelf-cover" src=src alt=""/> }.into_any(),
                                None => view! { <div class="shelf-cover shelf-cover-ph"></div> }.into_any(),
                            }}
                            <span class="shelf-text">
                                <span class="shelf-title">{b.title.clone()}</span>
                                <span class="muted">{byline}</span>
                                <span class="mono muted">{size}</span>
                            </span>
                        </A>
                        <button type="button" class="danger" on:click=del>"Remove"</button>
                    </div>
                }
                .into_any()
            })
            .collect_view()
            .into_any()
    };

    view! {
        <section class="listen-page">
            <crate::subnav::SubNav/>
            <div class="page-head"><h2>"On this device"</h2></div>
            <p class="muted">"Books this browser has stored for offline play. Setting a device up lives under Players."</p>
            {shelf}
        </section>
    }
}

/// `/players` — getting a player onto a phone (SKADI-T-0627).
///
/// Split out of `/listen`, which had grown two unrelated jobs: setting a
/// device up, and listing what *this* browser has downloaded. They are
/// different questions asked at different times — one is once per phone, the
/// other is every time you wonder where a book went — so they are two
/// sections now rather than two halves of one page.
#[component]
pub fn PlayersPage() -> impl leptos::IntoView {
    use leptos::prelude::*;
    // "Pair your phone" (SKADI-T-0333 Tier 1): a QR of this server's URL —
    // scan on the phone → skadi opens → token auto-injects → install prompt.
    // The address is EDITABLE, prefilled from this page's own origin: a phone
    // can only use an address that isn't loopback, and the server (in Docker)
    // can't know the host's LAN IP — but the operator's browser knows what it
    // used, and the operator can correct it (SKADI-T-0333 feedback: "can't
    // pair because i don't know the network ip").
    let qr = RwSignal::new(None::<api::PairQr>);
    let qr_err = RwSignal::new(None::<String>);
    let qr_open = RwSignal::new(false);
    let qr_url = RwSignal::new(String::new());
    let fetch_qr = move || {
        let url = qr_url.get_untracked();
        if url.trim().is_empty() {
            return;
        }
        // Drop any stale QR while regenerating so a failed update can't leave the
        // previous (e.g. loopback) code on screen (finding 10, SKADI-T-0346).
        qr.set(None);
        qr_err.set(None);
        leptos::task::spawn_local(async move {
            match api::pair_qr(&url).await {
                Ok(p) => {
                    let _ = qr.try_set(Some(p));
                }
                Err(e) => {
                    let _ = qr.try_set(None);
                    let _ = qr_err.try_set(Some(e.to_string()));
                }
            }
        });
    };
    let toggle_qr = move |_| {
        let now = !qr_open.get_untracked();
        qr_open.set(now);
        if now && qr.get_untracked().is_none() {
            if qr_url.get_untracked().is_empty()
                && let Some(w) = web_sys::window()
                && let Ok(origin) = w.location().origin()
            {
                // Deliberately the shelf, not this page: the phone that scans
                // this is the device doing the listening, so it should land on
                // what it has downloaded, not on the setup screen it just came
                // from (SKADI-T-0627 split /players out of /listen).
                qr_url.set(format!("{origin}/listen"));
            }
            fetch_qr();
        }
    };
    let is_loopback = move || {
        // Parse the host via the URL API rather than substring-matching the whole
        // string (which misfires on e.g. a "localhost.example.com" host) — finding 10.
        web_sys::Url::new(&qr_url.get())
            .ok()
            .map(|u| u.hostname())
            .map(|h| {
                matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]")
                    || h.starts_with("127.")
            })
            .unwrap_or(false)
    };
    let qr_panel = move || {
        if !qr_open.get() {
            return ().into_any();
        }
        let body = match (qr.get(), qr_err.get()) {
            (Some(p), _) => view! {
                <div class="pair-qr" inner_html=p.qr_svg.clone()></div>
            }
            .into_any(),
            (None, Some(e)) => view! {
                <p class="bad">"Couldn't generate the QR: " {e}</p>
            }
            .into_any(),
            (None, None) => view! { <p class="muted">"Generating…"</p> }.into_any(),
        };
        view! {
            <div class="pair-card">
                {body}
                <div class="pair-url-row">
                    <input
                        class="path-field mono"
                        r#type="text"
                        prop:value=move || qr_url.get()
                        on:input=move |ev| qr_url.set(event_target_value(&ev))
                        on:change=move |_| fetch_qr()
                    />
                    <button type="button" on:click=move |_| fetch_qr()>"Update"</button>
                </div>
                {move || is_loopback().then(|| view! {
                    <p class="pending">
                        "⚠ This is a loopback address — your phone can't reach it. Replace the host with this computer's LAN IP (e.g. 192.168.x.x or 10.x.x.x) and press Update."
                    </p>
                })}
                <p class="muted">"Scan with the phone's camera, then use the browser's Install / Add to Home Screen."</p>
            </div>
        }
        .into_any()
    };

    // The Android APP path (SKADI-T-0349): two QRs in sequence — (1) install
    // the APK, (2) in the app tap "Scan QR" to connect (skadi://pair with host
    // + token, so pairing is one scan, no typed IP). This is the recommended
    // path; the web player below is the fallback.
    let apk = RwSignal::new(None::<api::ApkInstall>);
    let app_pair = RwSignal::new(None::<api::PairQr>);
    let app_open = RwSignal::new(false);
    let toggle_app = move |_| {
        let now = !app_open.get_untracked();
        app_open.set(now);
        if now && apk.get_untracked().is_none() {
            leptos::task::spawn_local(async move {
                if let Ok(a) = api::pair_apk().await {
                    let _ = apk.try_set(Some(a));
                }
                if let Ok(p) = api::pair_app_qr("").await {
                    let _ = app_pair.try_set(Some(p));
                }
            });
        }
    };
    let app_panel = move || {
        if !app_open.get() {
            return ().into_any();
        }
        let install = match apk.get() {
            Some(a) if a.available => {
                let ver = a
                    .version_name
                    .clone()
                    .map(|v| format!(" v{v}"))
                    .unwrap_or_default();
                // The QR is for the phone in your hand; the link is for every
                // other case (operator, 2026-09-23) — a scanner that will not
                // focus, a tablet with no camera, or sending the address to
                // someone. Same file either way.
                let url = a.url.clone().unwrap_or_default();
                let copy_url = url.clone();
                let copied = RwSignal::new(false);
                view! {
                    <div class="pair-step">
                        <p class="pair-step-label">{"1 · Install the app"}{ver}</p>
                        <div class="pair-qr" inner_html=a.qr_svg.clone().unwrap_or_default()></div>
                        <p class="muted">"Scan it, or use the link:"</p>
                        <div class="pair-url-row">
                            <a class="btn-link mono" href=url.clone() download="">"⬇ Download the APK"</a>
                            <button type="button" on:click=move |_| {
                                if let Some(w) = web_sys::window() {
                                    let _ = w.navigator().clipboard().write_text(&copy_url);
                                    copied.set(true);
                                }
                            }>{move || if copied.get() { "Copied" } else { "Copy link" }}</button>
                        </div>
                        <p class="muted mono pair-url">{url}</p>
                        <p class="muted">"Open the downloaded file to install (allow installs from the browser when prompted)."</p>
                    </div>
                }
                .into_any()
            }
            Some(_) => view! {
                <p class="muted">"No app build published yet — run deploy/publish-apk.sh."</p>
            }
            .into_any(),
            None => view! { <p class="muted">"Checking…"</p> }.into_any(),
        };
        let pair = app_pair.get().map(|p| view! {
            <div class="pair-step">
                <p class="pair-step-label">"2 · In the app, tap “Scan QR” to connect"</p>
                <div class="pair-qr" inner_html=p.qr_svg.clone()></div>
                <p class="muted">"Connects the app to this server on your network — no address to type."</p>
            </div>
        });
        view! {
            <div class="pair-card pair-app">
                {install}
                {pair}
            </div>
        }
        .into_any()
    };

    view! {
        <section class="listen-page">
            <crate::subnav::SubNav/>
            <div class="page-head"><h2>"Players"</h2></div>
            <p class="muted">"Download audiobooks to a device and play them with no connection to skadi — on a plane, off the grid, anywhere."</p>

            <div class="listen-paths">
                <div class="listen-path">
                    <div class="listen-path-head">
                        <strong>"Android app"</strong>
                        <span class="badge ok">"recommended"</span>
                        <button type="button" class="btn-link" on:click=toggle_app>
                            {move || if app_open.get() { "Hide" } else { "Set up" }}
                        </button>
                    </div>
                    <p class="muted">"Best offline experience: background playback, lockscreen controls, auto-discovery on your network."</p>
                    {app_panel}
                </div>

                <div class="listen-path">
                    <div class="listen-path-head">
                        <strong>"Web player"</strong>
                        <button type="button" class="btn-link" on:click=toggle_qr>
                            {move || if qr_open.get() { "Hide" } else { "Use on this phone" }}
                        </button>
                    </div>
                    <p class="muted">"No install: open Skadi in the phone's browser and Add to Home Screen. Works, but no background audio."</p>
                    {qr_panel}
                </div>
            </div>
        </section>
    }
}
