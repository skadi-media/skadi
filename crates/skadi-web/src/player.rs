//! Mobile audiobook player (SKADI-T-0331 / initiative SKADI-I-0048).
//!
//! Route: `/listen/:id/:fid`. The player NEVER streams: it downloads the whole
//! book via an authed XHR (the `<audio>` tag can't send a Bearer header) into a
//! Blob and plays the local copy — the same download-then-play model the
//! offline layer (T-0332) formalizes with OPFS. Chapters, ±30s skips, playback
//! speed, a sleep timer, lockscreen controls via the MediaSession API, and a
//! localStorage position memory (T-0334 upgrades it to IndexedDB).

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::api;

/// Playback speeds the speed button cycles through.
pub const SPEED_STEPS: &[f64] = &[0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// Sleep timer choices (label, minutes); `0` = end of current chapter.
pub const SLEEP_CHOICES: &[(&str, u32)] = &[
    ("15 min", 15),
    ("30 min", 30),
    ("45 min", 45),
    ("60 min", 60),
    ("End of chapter", 0),
];

/// `"H:MM:SS"` (or `"M:SS"` under an hour) for a seconds offset.
pub fn fmt_clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Index of the chapter containing `t`, `None` without chapters.
pub fn chapter_at(chapters: &[api::Chapter], t: f64) -> Option<usize> {
    if chapters.is_empty() {
        return None;
    }
    match chapters.iter().rposition(|c| c.start_s <= t) {
        Some(i) => Some(i),
        None => Some(0),
    }
}

/// Where "previous chapter" seeks: >3s into a chapter → its start (the
/// standard double-tap-to-really-go-back convention); else the previous
/// chapter's start.
pub fn prev_chapter_target(chapters: &[api::Chapter], t: f64) -> Option<f64> {
    let i = chapter_at(chapters, t)?;
    let cur = &chapters[i];
    if t - cur.start_s > 3.0 || i == 0 {
        Some(cur.start_s)
    } else {
        Some(chapters[i - 1].start_s)
    }
}

/// Where "next chapter" seeks (`None` in the last chapter).
pub fn next_chapter_target(chapters: &[api::Chapter], t: f64) -> Option<f64> {
    let i = chapter_at(chapters, t)?;
    chapters.get(i + 1).map(|c| c.start_s)
}

/// The next speed after `cur` in the cycle.
pub fn next_speed(cur: f64) -> f64 {
    let i = SPEED_STEPS
        .iter()
        .position(|s| (s - cur).abs() < 0.01)
        .unwrap_or(1);
    SPEED_STEPS[(i + 1) % SPEED_STEPS.len()]
}

/// localStorage keys: playback position and speed per file, and the default
/// speed for a file with none of its own (SKADI-T-0659).
fn pos_key(fid: &str) -> String {
    format!("skadi-pos-{fid}")
}
fn rate_key(fid: &str) -> String {
    format!("skadi-rate-{fid}")
}
const RATE_KEY: &str = "skadi-rate";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn load_f64(key: &str) -> Option<f64> {
    local_storage()?.get_item(key).ok()??.parse().ok()
}

fn store_f64(key: &str, v: f64) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(key, &format!("{v}"));
    }
}

/// What the sleep timer is armed to do (SKADI-T-0657).
///
/// A minutes timer counts **listening** time: it runs only while playing, so a
/// pause neither uses it up nor leaves a deadline that stops the next session.
/// End of chapter is resolved against the position, so it means the chapter
/// playing now — it used to be the absolute end of the chapter playing when it
/// was set, which a seek made wrong. Mirrors Android's `SleepTimer`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sleep {
    /// `left_ms` of listening left as of `since` (epoch ms, `Date::now()`),
    /// which is `Some` only while playing.
    Minutes {
        total_ms: f64,
        left_ms: f64,
        since: Option<f64>,
    },
    EndOfChapter,
}

impl Sleep {
    pub fn minutes(mins: f64, now: f64, playing: bool) -> Self {
        Sleep::Minutes {
            total_ms: mins * 60_000.0,
            left_ms: mins * 60_000.0,
            since: playing.then_some(now),
        }
    }

    /// Listening time left on a minutes timer.
    pub fn remaining_ms(&self, now: f64) -> Option<f64> {
        match *self {
            Sleep::Minutes { left_ms, since, .. } => {
                Some((left_ms - since.map_or(0.0, |s| now - s)).max(0.0))
            }
            Sleep::EndOfChapter => None,
        }
    }

    /// Playback paused: bank the time used and stop the clock.
    #[must_use]
    pub fn paused(self, now: f64) -> Self {
        match self {
            Sleep::Minutes { total_ms, .. } => Sleep::Minutes {
                total_ms,
                left_ms: self.remaining_ms(now).unwrap_or(0.0),
                since: None,
            },
            other => other,
        }
    }

    /// Playback started: run the clock from now.
    #[must_use]
    pub fn resumed(self, now: f64) -> Self {
        match self {
            Sleep::Minutes {
                total_ms,
                left_ms,
                since: None,
            } => Sleep::Minutes {
                total_ms,
                left_ms,
                since: Some(now),
            },
            other => other,
        }
    }
}

/// End of the chapter playing at `t`; a position on a boundary belongs to the
/// chapter starting there.
pub fn chapter_end_at(chapters: &[api::Chapter], t: f64) -> Option<f64> {
    chapter_at(chapters, t)
        .map(|i| chapters[i].end_s)
        .or_else(|| chapters.first().map(|c| c.end_s))
}

/// Whether an end-of-chapter stop is due. `timeupdate` fires about every
/// 250 ms of wall time, which is `0.25 × rate` of audio, so stop when the next
/// update would land past the end.
pub fn chapter_stop_due(end_s: f64, t: f64, rate: f64) -> bool {
    t >= end_s - 0.3 * rate.max(0.1)
}

#[component]
pub fn PlayerPage() -> impl IntoView {
    let params = use_params_map();
    // The view-model is the OFFLINE snapshot shape (SKADI-T-0332): built from
    // the OPFS index when the book is on-device, else from the live API.
    let book = RwSignal::new(None::<crate::offline::OfflineBook>);
    let chapters = RwSignal::new(Vec::<api::Chapter>::new());
    let error = RwSignal::new(None::<String>);
    // Download state: fraction 0..1 while the XHR runs, then a blob URL.
    let dl_progress = RwSignal::new(0.0f64);
    let blob_url = RwSignal::new(None::<String>);
    // The downloaded Blob, held so it can be persisted to OPFS once metadata
    // is in (save-on-listen: playing a book once = downloaded for travel).
    let dl_blob = RwSignal::new(None::<web_sys::Blob>);
    let saved = RwSignal::new(false);
    // Playback state mirrored from the <audio> element.
    let duration = RwSignal::new(0.0f64);
    let current = RwSignal::new(0.0f64);
    let playing = RwSignal::new(false);
    let rate = RwSignal::new(load_f64(RATE_KEY).unwrap_or(1.0));
    let default_rate = RwSignal::new(load_f64(RATE_KEY).unwrap_or(1.0));
    let sleep = RwSignal::new(None::<Sleep>);
    // The chapter end an armed End of chapter is waiting for: set on arm and
    // recomputed on every seek, so it follows the chapter actually playing.
    let eoc_end = RwSignal::new(None::<f64>);
    let show_chapters = RwSignal::new(false);
    let last_saved = RwSignal::new(0.0f64);

    let audio_ref = NodeRef::<html::Audio>::new();
    // Monotonic run id: this component is REUSED across `/listen/:id/:fid`
    // changes, so an in-flight download/callback from book A must never write
    // into book B's state (review pass 2, web finding 3). Every async write
    // checks it matches; the in-flight XHR is aborted on switch.
    let generation = RwSignal::new(0u32);
    let xhr_handle = StoredValue::new(None::<web_sys::XmlHttpRequest>);
    let ids = move || {
        let p = params.read();
        (
            p.get("id").unwrap_or_default(),
            p.get("fid").unwrap_or_default(),
        )
    };

    // Revoke the current object URL (if any) and set a new one. The old URL
    // pins its Blob for the document's lifetime otherwise (web finding 1).
    let swap_blob_url = move |new: Option<String>| {
        if let Some(old) = blob_url.get_untracked() {
            let _ = web_sys::Url::revoke_object_url(&old);
        }
        blob_url.set(new);
    };

    // Source selection (SKADI-T-0332): the on-device OPFS copy wins — fully
    // offline playback from its stored metadata snapshot, zero network. No
    // copy → live API metadata + chapters, then the authed download.
    Effect::new(move |_| {
        let (id, fid) = ids();
        if id.is_empty() || fid.is_empty() {
            return;
        }
        // New run: abort any prior download, reset per-book state.
        let run_id = generation.get_untracked().wrapping_add(1);
        generation.set(run_id);
        xhr_handle.update_value(|h| {
            if let Some(x) = h.take() {
                let _ = x.abort();
            }
        });
        swap_blob_url(None);
        dl_blob.set(None);
        saved.set(false);
        dl_progress.set(0.0);
        let alive = move || generation.get_untracked() == run_id;

        spawn_local(async move {
            if let Some(f) = crate::offline::audio_file(&fid).await {
                // Self-heal: audio present but no index entry (a tab killed
                // between write_blob and write_index) must NOT hang at
                // "Loading…" — fetch metadata from the API instead (web
                // finding 5).
                match crate::offline::index_entry(&fid).await {
                    Some(m) => {
                        if !alive() {
                            return;
                        }
                        let _ = chapters.try_set(m.chapters.clone());
                        let _ = book.try_set(Some(m));
                    }
                    None => {
                        if let Ok(b) = api::get_book(&id).await {
                            if !alive() {
                                return;
                            }
                            let _ = book.try_set(Some(crate::offline::snapshot(
                                &b,
                                &fid,
                                Vec::new(),
                                0.0,
                            )));
                        }
                        if let Ok(ch) = api::book_chapters(&id, &fid).await {
                            if !alive() {
                                return;
                            }
                            let _ = chapters.try_set(ch);
                        }
                    }
                }
                if !alive() {
                    return;
                }
                if let Ok(u) = web_sys::Url::create_object_url_with_blob(&f) {
                    let _ = dl_progress.try_set(1.0);
                    let _ = saved.try_set(true);
                    swap_blob_url(Some(u));
                }
                return;
            }
            match api::get_book(&id).await {
                Ok(b) => {
                    if !alive() {
                        return;
                    }
                    let _ = book.try_set(Some(crate::offline::snapshot(&b, &fid, Vec::new(), 0.0)));
                }
                Err(e) => {
                    let _ = error.try_set(Some(e.to_string()));
                }
            }
            if let Ok(ch) = api::book_chapters(&id, &fid).await {
                if !alive() {
                    return;
                }
                let _ = chapters.try_set(ch);
            }
            if !alive() {
                return;
            }
            let x = start_download(
                &id,
                &fid,
                run_id,
                generation,
                dl_progress,
                blob_url,
                dl_blob,
                error,
            );
            xhr_handle.set_value(x);
        });
    });

    // Revoke the object URL + abort any download when the player unmounts, and
    // tear down the lockscreen "now playing" card so it doesn't linger after
    // leaving the player (finding 12, SKADI-T-0346).
    on_cleanup(move || {
        if let Some(old) = blob_url.try_get_untracked().flatten() {
            let _ = web_sys::Url::revoke_object_url(&old);
        }
        xhr_handle.try_update_value(|h| {
            if let Some(x) = h.take() {
                let _ = x.abort();
            }
        });
        clear_media_session();
    });

    // Save-on-listen: once the network download AND metadata are in, persist
    // the book to OPFS so it survives leaving the house (SKADI-T-0332).
    // `chapters` is TRACKED (not get_untracked) so if the download beats the
    // chapters fetch, the re-run re-saves with real chapters instead of
    // freezing an empty list into the offline copy (web finding 8).
    Effect::new(move |_| {
        let chapters_now = chapters.get();
        let (Some(blob), Some(meta)) = (dl_blob.get(), book.get()) else {
            return;
        };
        // Don't persist a chapterless copy if the source has chapters coming;
        // wait for the chapters signal to populate (re-runs this effect).
        if saved.get_untracked() && !chapters_now.is_empty() {
            return;
        }
        saved.set(true);
        let snap = crate::offline::OfflineBook {
            size_bytes: blob.size(),
            chapters: chapters_now,
            ..meta
        };
        spawn_local(async move {
            crate::offline::request_persistence().await;
            if let Err(e) = crate::offline::save_book(snap, &blob).await {
                leptos::logging::warn!("offline save failed: {e}");
                let _ = saved.try_set(false);
            }
        });
    });

    // Keep the element's playbackRate in step with the signal.
    Effect::new(move |_| {
        let r = rate.get();
        if let Some(a) = audio_ref.get() {
            a.set_playback_rate(r);
        }
    });

    // Lockscreen metadata once the book is known.
    Effect::new(move |_| {
        if let Some(b) = book.get() {
            media_session_metadata(&b);
        }
    });
    // Lockscreen action handlers, registered ONCE against the audio element — a
    // blob_url change must not re-register + leak a fresh set of closures
    // (finding 12, SKADI-T-0346).
    let actions_set = StoredValue::new(false);
    Effect::new(move |_| {
        if blob_url.get().is_some() && !actions_set.get_value() {
            media_session_actions(audio_ref);
            actions_set.set_value(true);
        }
    });

    // Minutes sleep timer: a real setTimeout for the listening time left, so
    // it fires even if timeupdate is throttled. Armed only while playing, and
    // re-armed whenever `sleep` changes — which includes every play and pause,
    // since they bank or restart the clock (SKADI-T-0657). The closure is kept
    // alive alongside the handle so it isn't dropped before it fires.
    let sleep_timeout = StoredValue::new(None::<i32>);
    let clear_sleep_timeout = move || {
        sleep_timeout.update_value(|slot| {
            if let Some(id) = slot.take()
                && let Some(w) = web_sys::window()
            {
                w.clear_timeout_with_handle(id);
            }
        });
    };
    Effect::new(move |_| {
        let s = sleep.get();
        clear_sleep_timeout();
        if let Some(sl @ Sleep::Minutes { since: Some(_), .. }) = s {
            let ms = sl.remaining_ms(js_sys::Date::now()).unwrap_or(0.0) as i32;
            let cb = Closure::<dyn FnMut()>::new(move || {
                sleep.set(None);
                if let Some(a) = audio_ref.get_untracked() {
                    let _ = a.pause();
                }
            });
            if let Some(w) = web_sys::window()
                && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.as_ref().unchecked_ref(),
                    ms,
                )
            {
                sleep_timeout.set_value(Some(id));
            }
            // Leak the one-shot closure (a re-arm/cleanup cancels the timeout by
            // id); one tiny closure per arm, matching media_session_actions.
            cb.forget();
        }
    });
    on_cleanup(clear_sleep_timeout);

    let toggle_play = move |_| {
        if let Some(a) = audio_ref.get() {
            if a.paused() {
                let _ = a.play();
            } else {
                let _ = a.pause();
            }
        }
    };
    let seek_to = move |t: f64| {
        if let Some(a) = audio_ref.get() {
            a.set_current_time(t.max(0.0));
        }
    };
    let skip = move |delta: f64| {
        if let Some(a) = audio_ref.get() {
            a.set_current_time((a.current_time() + delta).max(0.0));
        }
    };

    let on_timeupdate = move |_| {
        let Some(a) = audio_ref.get() else { return };
        let t = a.current_time();
        current.set(t);
        // Position memory: throttled to every ~5s of listening (T-0334 will
        // move this to IndexedDB with finished-tracking).
        if (t - last_saved.get_untracked()).abs() > 5.0 {
            let (_, fid) = ids();
            store_f64(&pos_key(&fid), t);
            last_saved.set(t);
        }
        // Sleep timer — the end-of-chapter case only; the minutes case is a
        // real setTimeout above, so it fires even when backgrounded.
        if sleep.get_untracked() == Some(Sleep::EndOfChapter)
            && let Some(end) = eoc_end.get_untracked()
            && chapter_stop_due(end, t, a.playback_rate())
        {
            sleep.set(None);
            eoc_end.set(None);
            let _ = a.pause();
        }
    };
    let on_loaded = move |_| {
        let Some(a) = audio_ref.get() else { return };
        duration.set(a.duration());
        // This book's own speed, else the default (SKADI-T-0659).
        let (_, fid) = ids();
        rate.set(load_f64(&rate_key(&fid)).unwrap_or_else(|| default_rate.get_untracked()));
        a.set_playback_rate(rate.get_untracked());
        // Resume where we left off (if meaningfully into the book).
        if let Some(t) = load_f64(&pos_key(&fid))
            && t > 5.0
            && t < a.duration() - 5.0
        {
            a.set_current_time(t);
        }
    };
    let on_pause_save = move |_| {
        let (_, fid) = ids();
        store_f64(&pos_key(&fid), current.get_untracked());
        playing.set(false);
        if let Some(s) = sleep.get_untracked() {
            sleep.set(Some(s.paused(js_sys::Date::now())));
        }
    };
    let on_play = move |_| {
        playing.set(true);
        if let Some(s) = sleep.get_untracked() {
            sleep.set(Some(s.resumed(js_sys::Date::now())));
        }
    };
    let on_seeked = move |_| {
        if sleep.get_untracked() == Some(Sleep::EndOfChapter)
            && let Some(a) = audio_ref.get_untracked()
        {
            eoc_end.set(chapter_end_at(&chapters.get_untracked(), a.current_time()));
        }
    };

    let body = move || {
        if let Some(e) = error.get() {
            return view! { <p class="bad">"Player error: " {e}</p> }.into_any();
        }
        let Some(b) = book.get() else {
            return view! { <p class="muted">"Loading…"</p> }.into_any();
        };
        let cover = b.cover_url.clone().filter(|u| !u.is_empty());
        let byline = {
            let mut parts = Vec::new();
            if !b.authors.is_empty() {
                parts.push(b.authors.join(", "));
            }
            if !b.narrators.is_empty() {
                parts.push(format!("read by {}", b.narrators.join(", ")));
            }
            parts.join(" · ")
        };
        let series = b
            .series_name
            .as_ref()
            .map(|name| {
                let pos = b
                    .series_position
                    .as_deref()
                    .map(|p| format!(" #{p}"))
                    .unwrap_or_default();
                format!("{name}{pos}")
            })
            .unwrap_or_default();

        let downloading = move || {
            blob_url.get().is_none().then(|| {
                let pct = (dl_progress.get() * 100.0).round();
                view! {
                    <div class="player-dl">
                        <div class="player-dl-bar" style=move || format!("width:{pct}%")></div>
                        <span class="mono muted">{format!("downloading… {pct:.0}%")}</span>
                    </div>
                }
            })
        };

        // Current chapter label + the chapter drawer.
        let chapter_label = move || {
            let ch = chapters.get();
            chapter_at(&ch, current.get())
                .map(|i| ch[i].title.clone())
                .unwrap_or_default()
        };
        let drawer = move || {
            show_chapters.get().then(|| {
                let ch = chapters.get();
                let cur = chapter_at(&ch, current.get());
                let rows = ch
                    .iter()
                    .map(|c| {
                        let cls = if Some(c.index) == cur {
                            "player-ch current"
                        } else {
                            "player-ch"
                        };
                        let start = c.start_s;
                        let title = c.title.clone();
                        let clock = fmt_clock(start);
                        view! {
                            <button type="button" class=cls on:click=move |_| {
                                seek_to(start);
                                show_chapters.set(false);
                            }>
                                <span class="player-ch-title">{title}</span>
                                <span class="mono muted">{clock}</span>
                            </button>
                        }
                    })
                    .collect_view();
                view! { <div class="player-drawer">{rows}</div> }
            })
        };

        let scrub = move |ev: leptos::ev::Event| {
            if let Ok(v) = event_target_value(&ev).parse::<f64>() {
                seek_to(v);
            }
        };
        let has_chapters = move || !chapters.get().is_empty();
        let prev_ch = move |_| {
            if let Some(t) = prev_chapter_target(&chapters.get(), current.get()) {
                seek_to(t);
            }
        };
        let next_ch = move |_| {
            if let Some(t) = next_chapter_target(&chapters.get(), current.get()) {
                seek_to(t);
            }
        };
        let cycle_speed = move |_| {
            let r = next_speed(rate.get());
            rate.set(r);
            let (_, fid) = ids();
            store_f64(&rate_key(&fid), r);
        };
        let arm_sleep = move |ev: leptos::ev::Event| {
            match event_target_value(&ev).as_str() {
                "" => sleep.set(None),
                "eoc" => {
                    if let Some(end) = chapter_end_at(&chapters.get(), current.get()) {
                        eoc_end.set(Some(end));
                        sleep.set(Some(Sleep::EndOfChapter));
                    }
                }
                m => {
                    if let Ok(mins) = m.parse::<f64>() {
                        sleep.set(Some(Sleep::minutes(
                            mins,
                            js_sys::Date::now(),
                            playing.get_untracked(),
                        )));
                    }
                }
            }
            // Snap the menu back to the placeholder so picking the SAME duration
            // again re-arms (it's an action menu, not a persistent selection —
            // finding 9). The placeholder still reads "⏱ armed" while sleep is set.
            if let Some(sel) = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok())
            {
                sel.set_value("");
            }
        };

        view! {
            <div class="player">
                {match cover {
                    Some(src) => view! { <img class="player-cover" src=src alt=""/> }.into_any(),
                    None => view! { <div class="player-cover player-cover-ph">{b.title.clone()}</div> }.into_any(),
                }}
                <h2 class="player-title">{b.title.clone()}</h2>
                <p class="player-byline muted">{byline}</p>
                {(!series.is_empty()).then(|| view! { <p class="player-series mono muted">{series}</p> })}
                {downloading}
                <p class="player-chapter mono">{chapter_label}</p>
                <input
                    class="player-scrub"
                    r#type="range"
                    min="0"
                    max=move || format!("{}", duration.get().max(1.0))
                    step="1"
                    prop:value=move || format!("{}", current.get())
                    on:input=scrub
                />
                <div class="player-clock mono muted">
                    <span>{move || fmt_clock(current.get())}</span>
                    <span>{move || fmt_clock(duration.get())}</span>
                </div>
                <div class="player-controls">
                    <button type="button" class="player-btn" disabled=move || !has_chapters() on:click=prev_ch title="Previous chapter">"⏮"</button>
                    <button type="button" class="player-btn" on:click=move |_| skip(-30.0) title="Back 30s">"↺30"</button>
                    <button type="button" class="player-btn player-play" on:click=toggle_play>
                        {move || if playing.get() { "⏸" } else { "▶" }}
                    </button>
                    <button type="button" class="player-btn" on:click=move |_| skip(30.0) title="Forward 30s">"30↻"</button>
                    <button type="button" class="player-btn" disabled=move || !has_chapters() on:click=next_ch title="Next chapter">"⏭"</button>
                </div>
                <div class="player-sub">
                    <button type="button" class="player-btn-sm mono" on:click=cycle_speed title="Playback speed">
                        {move || format!("{}×", rate.get())}
                    </button>
                    {move || {
                        let r = rate.get();
                        ((r - default_rate.get()).abs() > 0.001).then(|| view! {
                            <button
                                type="button"
                                class="player-btn-sm"
                                title="Use this speed for books you have not set one for"
                                on:click=move |_| {
                                    store_f64(RATE_KEY, r);
                                    default_rate.set(r);
                                }
                            >
                                {format!("Make {r}× default")}
                            </button>
                        })
                    }}
                    <button
                        type="button"
                        class="player-btn-sm"
                        disabled=move || !has_chapters()
                        on:click=move |_| show_chapters.update(|v| *v = !*v)
                    >
                        "Chapters"
                    </button>
                    <select class="player-sleep" on:change=arm_sleep title="Sleep timer">
                        <option value="">{move || {
                            // `current` ticks with timeupdate, so the countdown
                            // refreshes while playing (SKADI-T-0657).
                            let _ = current.get();
                            match sleep.get() {
                                None => "⏱ sleep".to_string(),
                                Some(Sleep::EndOfChapter) => "⏱ chapter".to_string(),
                                Some(s) => format!(
                                    "⏱ {}",
                                    fmt_clock(s.remaining_ms(js_sys::Date::now()).unwrap_or(0.0) / 1000.0)
                                ),
                            }
                        }}</option>
                        <option value="15">"15 min"</option>
                        <option value="30">"30 min"</option>
                        <option value="45">"45 min"</option>
                        <option value="60">"60 min"</option>
                        <option value="eoc">"End of chapter"</option>
                    </select>
                </div>
                {drawer}
                <audio
                    node_ref=audio_ref
                    prop:src=move || blob_url.get().unwrap_or_default()
                    on:timeupdate=on_timeupdate
                    on:loadedmetadata=on_loaded
                    on:play=on_play
                    on:pause=on_pause_save
                    on:seeked=on_seeked
                    on:ended=on_pause_save
                ></audio>
            </div>
        }
        .into_any()
    };

    let back = move || {
        let (id, _) = ids();
        format!("/audiobooks/{id}")
    };
    view! {
        <section class="player-page">
            <div class="page-head">
                <A href=back attr:class="btn-link">"← Book"</A>
            </div>
            {body}
        </section>
    }
}

/// Kick off the authed XHR download of the audio into a Blob (progress events
/// drive the bar; the `<audio>` element then plays the object URL). Returns
/// the XHR so the caller can `abort()` it on navigation/unmount. `run_id`/`gen_sig`
/// gate the completion callbacks so a stale download can't write into a
/// newer book's state (review pass 2, web finding 3).
fn start_download(
    id: &str,
    fid: &str,
    run_id: u32,
    gen_sig: RwSignal<u32>,
    progress: RwSignal<f64>,
    blob_url: RwSignal<Option<String>>,
    dl_blob: RwSignal<Option<web_sys::Blob>>,
    error: RwSignal<Option<String>>,
) -> Option<web_sys::XmlHttpRequest> {
    let alive = move || gen_sig.try_get_untracked() == Some(run_id);
    let Ok(xhr) = web_sys::XmlHttpRequest::new() else {
        error.set(Some("XHR unavailable".into()));
        return None;
    };
    let url = api::book_audio_url(id, fid);
    if xhr.open("GET", &url).is_err() {
        error.set(Some("could not open audio request".into()));
        return None;
    }
    if let Some(t) = api::auth_token() {
        let _ = xhr.set_request_header("Authorization", &format!("Bearer {t}"));
    }
    xhr.set_response_type(web_sys::XmlHttpRequestResponseType::Blob);

    let on_progress =
        Closure::<dyn FnMut(web_sys::ProgressEvent)>::new(move |e: web_sys::ProgressEvent| {
            if alive() && e.length_computable() && e.total() > 0.0 {
                let _ = progress.try_set(e.loaded() / e.total());
            }
        });
    xhr.set_onprogress(Some(on_progress.as_ref().unchecked_ref()));

    let xhr_done = xhr.clone();
    let on_load = Closure::<dyn FnMut()>::new(move || {
        let status = xhr_done.status().unwrap_or(0);
        if status != 200 && status != 206 {
            if alive() {
                let _ = error.try_set(Some(format!("audio download -> HTTP {status}")));
            }
            return;
        }
        // Stale download (user moved to another book): drop the response so we
        // never mint a leaked object URL or corrupt the new book's OPFS entry.
        if !alive() {
            return;
        }
        if let Ok(resp) = xhr_done.response()
            && let Ok(blob) = resp.dyn_into::<web_sys::Blob>()
            && let Ok(u) = web_sys::Url::create_object_url_with_blob(&blob)
        {
            let _ = progress.try_set(1.0);
            let _ = dl_blob.try_set(Some(blob));
            let _ = blob_url.try_set(Some(u));
        } else {
            let _ = error.try_set(Some("audio download produced no blob".into()));
        }
    });
    xhr.set_onload(Some(on_load.as_ref().unchecked_ref()));

    let on_error = Closure::<dyn FnMut()>::new(move || {
        if alive() {
            let _ = error.try_set(Some("audio download failed (network)".into()));
        }
    });
    xhr.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    if xhr.send().is_err() {
        error.set(Some("could not start audio download".into()));
        return None;
    }
    // Page-lifetime closures: intentionally leaked (aborting the XHR stops them
    // firing; the guard makes a late fire a no-op).
    on_progress.forget();
    on_load.forget();
    on_error.forget();
    Some(xhr)
}

/// `navigator.mediaSession`, if the browser has one. Driven via `Reflect`
/// because web-sys's MediaSession types are unstable-gated (would require
/// `--cfg=web_sys_unstable_apis` through trunk, wasm-pack AND the Docker
/// build) — the dynamic surface here is four calls.
fn media_session() -> Option<js_sys::Object> {
    let nav = web_sys::window()?.navigator();
    js_sys::Reflect::get(nav.as_ref(), &"mediaSession".into())
        .ok()?
        .dyn_into::<js_sys::Object>()
        .ok()
}

/// Lockscreen "now playing" card (MediaSession metadata).
fn media_session_metadata(b: &crate::offline::OfflineBook) {
    let Some(session) = media_session() else {
        return;
    };
    let Some(w) = web_sys::window() else { return };
    // new MediaMetadata({title, artist, artwork: [{src}]})
    let Ok(ctor) = js_sys::Reflect::get(w.as_ref(), &"MediaMetadata".into()) else {
        return;
    };
    let Ok(ctor) = ctor.dyn_into::<js_sys::Function>() else {
        return;
    };
    let init = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&init, &"title".into(), &b.title.as_str().into());
    let _ = js_sys::Reflect::set(&init, &"artist".into(), &b.authors.join(", ").into());
    if let Some(cover) = b.cover_url.as_deref().filter(|u| !u.is_empty()) {
        let entry = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&entry, &"src".into(), &cover.into());
        let art = js_sys::Array::of1(&entry);
        let _ = js_sys::Reflect::set(&init, &"artwork".into(), &art);
    }
    let args = js_sys::Array::of1(&init);
    if let Ok(md) = js_sys::Reflect::construct(&ctor, &args) {
        let _ = js_sys::Reflect::set(&session, &"metadata".into(), &md);
    }
}

/// Tear down the lockscreen card on leaving the player (finding 12): null the
/// metadata and drop every action handler so the OS "now playing" notification
/// doesn't linger after the player unmounts.
fn clear_media_session() {
    let Some(session) = media_session() else {
        return;
    };
    let _ = js_sys::Reflect::set(&session, &"metadata".into(), &JsValue::NULL);
    let Ok(set_handler) = js_sys::Reflect::get(&session, &"setActionHandler".into()) else {
        return;
    };
    let Ok(set_handler) = set_handler.dyn_into::<js_sys::Function>() else {
        return;
    };
    for action in ["play", "pause", "seekbackward", "seekforward"] {
        let _ = set_handler.call2(&session, &action.into(), &JsValue::NULL);
    }
}

/// Lockscreen transport controls → the audio element.
fn media_session_actions(audio_ref: NodeRef<html::Audio>) {
    let Some(session) = media_session() else {
        return;
    };
    let Ok(set_handler) = js_sys::Reflect::get(&session, &"setActionHandler".into()) else {
        return;
    };
    let Ok(set_handler) = set_handler.dyn_into::<js_sys::Function>() else {
        return;
    };
    for (action, delta) in [
        ("play", 0.0),
        ("pause", 0.0),
        ("seekbackward", -30.0),
        ("seekforward", 30.0),
    ] {
        let cb = Closure::<dyn FnMut()>::new(move || {
            let Some(a) = audio_ref.get_untracked() else {
                return;
            };
            match action {
                "play" => {
                    let _ = a.play();
                }
                "pause" => {
                    let _ = a.pause();
                }
                _ => a.set_current_time((a.current_time() + delta).max(0.0)),
            }
        });
        let _ = set_handler.call2(&session, &action.into(), cb.as_ref().unchecked_ref());
        cb.forget();
    }
}
