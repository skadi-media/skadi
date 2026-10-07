//! Browser video player (SKADI-T-0585): play a movie edition or an episode
//! in the page with the platform `<video>` element.
//!
//! **Direct play only**, like the phone: the daemon serves the file's bytes
//! with range support and no transcoding. The difference is what the client
//! can decode. The phone's ExoPlayer handles nearly the whole library; a
//! browser is narrower and *varies by browser* — Chrome opens Matroska but
//! will not decode AC-3/E-AC-3 audio; Safari decodes AC-3 but will not open
//! Matroska at all. So before the element is asked to play anything, the
//! file's scanned `media_info` is checked against what *this* browser says
//! it can play (`canPlayType`), and the page says plainly what will happen:
//! fine, silent, or not at all — and that the app plays it. A player that
//! simply goes black, or plays picture with no sound, is worse than a
//! sentence.
//!
//! Resume position is kept in `localStorage` under the same key scheme the
//! phone uses (`movie:<id>:<edition>`, `episode:<series>:<episode>`), with
//! the same two refusals: nothing under fifteen seconds is worth restoring,
//! and the last minute counts as finished.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::api;

/// The verdict for a file in this browser.
#[derive(Clone, Debug, PartialEq)]
pub enum Playability {
    /// Everything decodes.
    Ok,
    /// It will play, but with a caveat worth a sentence (usually: no sound).
    Warn(String),
    /// The browser cannot open it; the element will fail.
    Blocked(String),
}

/// A `canPlayType` probe string for a container, or `None` for containers no
/// browser opens. Matroska is probed as WebM: Chrome's WebM demuxer opens
/// `.mkv` with H.264/HEVC inside, and reports `video/webm` as playable, while
/// Safari and Firefox report it as not — which is exactly the split.
pub fn container_mime(container: &str) -> Option<&'static str> {
    match container.to_ascii_lowercase().as_str() {
        "mkv" | "webm" | "matroska" => Some("video/webm"),
        "mp4" | "m4v" | "mov" => Some("video/mp4"),
        _ => None,
    }
}

/// A `canPlayType` probe for a video codec, or `None` for codecs no browser
/// decodes. Probed inside MP4 so the codec is judged on its own; the
/// container is judged separately.
pub fn video_mime(codec: &str) -> Option<&'static str> {
    match codec.to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" => Some(r#"video/mp4; codecs="avc1.640028""#),
        "hevc" | "h265" | "hvc1" => Some(r#"video/mp4; codecs="hvc1.1.6.L120.B0""#),
        "av1" => Some(r#"video/mp4; codecs="av01.0.08M.08""#),
        "vp9" => Some(r#"video/webm; codecs="vp9""#),
        "vp8" => Some(r#"video/webm; codecs="vp8""#),
        _ => None,
    }
}

/// A `canPlayType` probe for an audio codec, or `None` for codecs no browser
/// decodes (DTS, TrueHD, raw PCM variants).
pub fn audio_mime(codec: &str) -> Option<&'static str> {
    match codec.to_ascii_lowercase().as_str() {
        "aac" => Some(r#"audio/mp4; codecs="mp4a.40.2""#),
        "ac3" => Some(r#"audio/mp4; codecs="ac-3""#),
        "eac3" => Some(r#"audio/mp4; codecs="ec-3""#),
        "opus" => Some(r#"audio/webm; codecs="opus""#),
        "vorbis" => Some(r#"audio/webm; codecs="vorbis""#),
        "flac" => Some("audio/flac"),
        "mp3" | "mp2" => Some("audio/mpeg"),
        _ => None,
    }
}

/// Decide what will happen if this browser is handed the file. `can` is the
/// browser's `canPlayType`, reduced to a bool — injected so the decision is
/// testable without a browser.
pub fn playability(mi: Option<&api::MediaInfo>, can: impl Fn(&str) -> bool) -> Playability {
    let Some(mi) = mi else {
        return Playability::Warn(
            "This file hasn't been scanned yet, so there is no way to tell in advance whether it will play here."
                .into(),
        );
    };
    if let Some(c) = mi.container.as_deref() {
        match container_mime(c) {
            None => {
                return Playability::Blocked(format!(
                    "{c} files don't open in a browser. The Android app plays this file."
                ));
            }
            Some(m) if !can(m) => {
                return Playability::Blocked(format!(
                    "This browser can't open {c} files. Chrome can, and so can the Android app."
                ));
            }
            _ => {}
        }
    }
    if let Some(v) = mi.video.as_ref().and_then(|v| v.codec.as_deref()) {
        match video_mime(v) {
            None => {
                return Playability::Blocked(format!(
                    "{v} video isn't decodable in a browser. The Android app plays this file."
                ));
            }
            Some(m) if !can(m) => {
                return Playability::Blocked(format!(
                    "This browser can't decode {v} video. The Android app plays this file."
                ));
            }
            _ => {}
        }
    }
    let codecs = mi.audio_codecs();
    if codecs.is_empty() {
        return Playability::Ok;
    }
    let any = codecs.iter().any(|c| audio_mime(c).is_some_and(&can));
    if !any {
        return Playability::Warn(format!(
            "This browser can't decode the audio ({}): the picture will play with no sound. The Android app plays it with sound.",
            codecs.join(", ")
        ));
    }
    Playability::Ok
}

/// Whether an episode has bytes to play.
fn playable(e: &api::Episode) -> bool {
    matches!(api::status_label(&e.status).as_str(), "imported" | "cutoff")
}

/// The next playable episode after (`season`, `number`) in broadcast order,
/// crossing into later seasons. Specials (season 0) are skipped: after S03E10
/// nobody wants S00E04. Same rule as the phone's Up next.
pub fn next_episode(episodes: &[api::Episode], season: u16, number: u16) -> Option<&api::Episode> {
    episodes
        .iter()
        .filter(|e| e.season > 0 && playable(e))
        .filter(|e| (e.season, e.number) > (season, number))
        .min_by_key(|e| (e.season, e.number))
}

/// The playable episode before (`season`, `number`), same rules.
pub fn prev_episode(episodes: &[api::Episode], season: u16, number: u16) -> Option<&api::Episode> {
    episodes
        .iter()
        .filter(|e| e.season > 0 && playable(e))
        .filter(|e| (e.season, e.number) < (season, number))
        .max_by_key(|e| (e.season, e.number))
}

/// `S01E03`, zero-padded.
pub fn episode_code(season: u16, number: u16) -> String {
    format!("S{season:02}E{number:02}")
}

/// Below this, resuming is noise rather than help (matches the phone).
const MIN_RESUME_S: f64 = 15.0;
/// Within this of the end, treat it as watched (matches the phone).
const END_SLACK_S: f64 = 60.0;

/// Where to resume, given a stored position and the media duration.
pub fn resume_at(saved: Option<f64>, duration: f64) -> Option<f64> {
    let pos = saved?;
    if pos < MIN_RESUME_S {
        return None;
    }
    if duration > 0.0 && pos > duration - END_SLACK_S {
        return None;
    }
    Some(pos)
}

/// localStorage key for the viewer's subtitle choice (SKADI-T-0664):
/// `off`, `forced` (the default) or a language code.
const SUB_PREF_KEY: &str = "skadi.watch.subtitles";

/// Which of `tracks` to show for the preference `pref` (SKADI-T-0664):
/// `off` shows none; `forced` the first forced one; a language its first
/// unforced file, else any file in it.
#[must_use]
pub fn subtitle_to_show(pref: &str, tracks: &[api::SubtitleTrack]) -> Option<usize> {
    match pref {
        "off" => None,
        "forced" | "" => tracks.iter().position(|t| t.forced),
        lang => tracks
            .iter()
            .position(|t| t.language.as_deref() == Some(lang) && !t.forced)
            .or_else(|| {
                tracks
                    .iter()
                    .position(|t| t.language.as_deref() == Some(lang))
            }),
    }
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn storage_key(key: &str) -> String {
    format!("skadi.watch.{key}")
}

fn load_position(key: &str) -> Option<f64> {
    storage()?.get_item(&storage_key(key)).ok()??.parse().ok()
}

fn save_position(key: &str, secs: f64) {
    if let Some(s) = storage() {
        let _ = s.set_item(&storage_key(key), &format!("{secs:.1}"));
    }
}

fn clear_position(key: &str) {
    if let Some(s) = storage() {
        let _ = s.remove_item(&storage_key(key));
    }
}

/// Ask the browser. A `<video>` element answers `canPlayType` for any MIME;
/// "" means no, "maybe"/"probably" mean it will try.
fn browser_can_play(mime: &str) -> bool {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return true;
    };
    let Ok(el) = doc.create_element("video") else {
        return true;
    };
    let Ok(media) = el.dyn_into::<web_sys::HtmlMediaElement>() else {
        return true;
    };
    !media.can_play_type(mime).is_empty()
}

use wasm_bindgen::JsCast;

/// What the player needs, resolved by the movie/episode wrappers below.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchItem {
    pub title: String,
    pub subtitle: Option<String>,
    pub back_href: String,
    pub back_label: String,
    pub src: String,
    /// `src` without the key: the base the subtitle routes hang off.
    pub video_path: String,
    /// Resume key, shared with the phone's scheme.
    pub key: String,
    pub media_info: Option<api::MediaInfo>,
    /// (href, label) of the previous / next playable episode, for series.
    pub prev: Option<(String, String)>,
    pub next: Option<(String, String)>,
}

/// The player itself, once the item is known.
#[component]
fn Watch(item: WatchItem) -> impl IntoView {
    let key = item.key.clone();
    let subs = RwSignal::new(Vec::<api::SubtitleTrack>::new());
    let sub_pref = RwSignal::new(
        storage()
            .and_then(|s| s.get_item(SUB_PREF_KEY).ok().flatten())
            .unwrap_or_else(|| "forced".into()),
    );
    let video_path = item.video_path.clone();
    {
        let vp = item.video_path.clone();
        spawn_local(async move { subs.set(api::video_subtitles(&vp).await) });
    }
    let verdict = playability(item.media_info.as_ref(), browser_can_play);
    let note = match &verdict {
        Playability::Ok => None,
        Playability::Warn(m) => Some(("watch-note warn", m.clone())),
        Playability::Blocked(m) => Some(("watch-note bad", m.clone())),
    };
    let video = NodeRef::<leptos::html::Video>::new();
    // Restore once the duration is known, so the end-of-file refusal has
    // something to compare against.
    let restore_key = key.clone();
    // Show the preferred subtitle and hide the rest. The browser numbers its
    // text tracks in `<track>` order, which is `subs` order.
    let apply_subs = move || {
        let Some(v) = video.get_untracked() else {
            return;
        };
        let show = subtitle_to_show(&sub_pref.get_untracked(), &subs.get_untracked());
        let Some(list) = v.text_tracks() else { return };
        for i in 0..list.length() {
            if let Some(t) = list.get(i) {
                t.set_mode(if Some(i as usize) == show {
                    web_sys::TextTrackMode::Showing
                } else {
                    web_sys::TextTrackMode::Disabled
                });
            }
        }
    };
    Effect::new(move |_| {
        let _ = (subs.get(), sub_pref.get());
        apply_subs();
    });
    let on_loaded = move |_| {
        apply_subs();
        if let Some(v) = video.get()
            && let Some(at) = resume_at(load_position(&restore_key), v.duration())
        {
            v.set_current_time(at);
        }
    };
    // Save on a coarse cadence: `timeupdate` fires several times a second and
    // localStorage writes are synchronous.
    let last_saved = RwSignal::new(0.0_f64);
    let save_key = key.clone();
    let on_time = move |_| {
        if let Some(v) = video.get() {
            let t = v.current_time();
            if (t - last_saved.get_untracked()).abs() >= 5.0 {
                last_saved.set(t);
                save_position(&save_key, t);
            }
        }
    };
    let end_key = key.clone();
    // When an episode ends, go to the next one. Exiting the player, finding
    // the show and clicking through was the whole reason this exists.
    let navigate = use_navigate();
    let next_href = item.next.as_ref().map(|(h, _)| h.clone());
    let prev_href = item.prev.as_ref().map(|(h, _)| h.clone());
    let advance = {
        let navigate = navigate.clone();
        let next_href = next_href.clone();
        move || {
            if let Some(h) = &next_href {
                navigate(h, Default::default());
            }
        }
    };
    let on_ended = {
        let advance = advance.clone();
        move |_| {
            clear_position(&end_key);
            advance();
        }
    };
    // `n` / `p` from anywhere on the page. No text inputs live here, so a
    // bare key is safe to claim.
    let handle = window_event_listener(leptos::ev::keydown, {
        let navigate = navigate.clone();
        move |ev| match ev.key().as_str() {
            "n" | "N" => advance(),
            "p" | "P" => {
                if let Some(h) = &prev_href {
                    navigate(h, Default::default());
                }
            }
            _ => {}
        }
    });
    on_cleanup(move || handle.remove());

    let meta = item.media_info.as_ref().map(|mi| {
        let mut parts = Vec::new();
        if let Some(v) = &mi.video {
            if let (Some(c), Some(h)) = (&v.codec, v.height) {
                parts.push(format!("{c} {h}p"));
            } else if let Some(c) = &v.codec {
                parts.push(c.clone());
            }
        }
        let audio = mi.audio_codecs();
        if !audio.is_empty() {
            parts.push(audio.join(" / "));
        }
        if let Some(c) = &mi.container {
            parts.push(c.clone());
        }
        parts.join(" · ")
    });

    view! {
        <div class="watch-page">
            <div class="page-head">
                <A href=item.back_href.clone() attr:class="btn-link">{format!("← {}", item.back_label)}</A>
            </div>
            <div class="watch-head">
                <h2>{item.title.clone()}</h2>
                {item.subtitle.clone().map(|s| view! { <span class="muted">{s}</span> })}
                <span class="watch-nav">
                    {item.prev.clone().map(|(h, l)| view! {
                        <A href=h attr:class="btn-link" attr:title="Previous episode (p)">{format!("◀ {l}")}</A>
                    })}
                    {item.next.clone().map(|(h, l)| view! {
                        <A href=h attr:class="btn-link watch-next" attr:title="Next episode (n)">{format!("{l} ▶")}</A>
                    })}
                </span>
            </div>
            {note.map(|(cls, m)| view! { <div class=cls>{m}</div> })}
            <video
                class="watch-video"
                node_ref=video
                src=item.src.clone()
                controls=true
                autoplay=true
                playsinline=true
                preload="metadata"
                on:loadedmetadata=on_loaded
                on:timeupdate=on_time
                on:ended=on_ended
            >
                // Subtitle files beside the video (SKADI-T-0663), as WebVTT
                // tracks the browser's own captions menu offers.
                {move || subs.get().into_iter().map(|t| {
                    let src = api::video_subtitle_vtt_url(&video_path, t.index);
                    view! {
                        <track
                            kind="subtitles"
                            src=src
                            srclang=t.language.clone().unwrap_or_default()
                            label=t.label.clone()
                        />
                    }
                }).collect_view()}
            </video>
            // The viewer's subtitle choice, remembered for every video
            // (SKADI-T-0664). Shown only when the video has subtitle files.
            {move || (!subs.get().is_empty()).then(|| {
                let mut langs: Vec<(String, String)> = Vec::new();
                for t in subs.get() {
                    if let Some(l) = t.language.clone()
                        && !langs.iter().any(|(c, _)| *c == l)
                    {
                        let name = t.label.split(" (").next().unwrap_or(&t.label).to_string();
                        langs.push((l, name));
                    }
                }
                view! {
                    <label class="watch-subs">
                        "Subtitles "
                        <select on:change=move |ev| {
                            let v = event_target_value(&ev);
                            if let Some(s) = storage() {
                                let _ = s.set_item(SUB_PREF_KEY, &v);
                            }
                            sub_pref.set(v);
                        }>
                            <option value="off" selected=move || sub_pref.get() == "off">"Off"</option>
                            <option value="forced" selected=move || sub_pref.get() == "forced">"Forced only"</option>
                            {langs.into_iter().map(|(code, name)| {
                                let c = code.clone();
                                view! { <option value=code selected=move || sub_pref.get() == c>{name}</option> }
                            }).collect_view()}
                        </select>
                    </label>
                }
            })}
            {meta.map(|m| view! { <p class="watch-meta mono">{m}</p> })}
        </div>
    }
}

/// `/watch/movie/:id/:eid`
#[component]
pub fn WatchMoviePage() -> impl IntoView {
    let params = use_params_map();
    let item = RwSignal::new(None::<Result<WatchItem, String>>);
    Effect::new(move |_| {
        let p = params.get();
        let (id, eid) = (
            p.get("id").unwrap_or_default(),
            p.get("eid").unwrap_or_default(),
        );
        spawn_local(async move {
            let r = api::get_movie(&id)
                .await
                .map_err(|e| e.to_string())
                .and_then(|m| {
                    let ed =
                        m.editions.iter().find(|e| e.id == eid).ok_or_else(|| {
                            "That edition isn't in the library any more.".to_string()
                        })?;
                    Ok(WatchItem {
                        title: m.title.clone(),
                        subtitle: m.year.map(|y| y.to_string()),
                        back_href: format!("/movies/{id}"),
                        back_label: "Movie".into(),
                        src: api::movie_video_url(&id, &eid),
                        video_path: format!("{}/movies/{id}/editions/{eid}/video", api::API_BASE),
                        key: format!("movie:{id}:{eid}"),
                        media_info: ed.media_info.clone(),
                        prev: None,
                        next: None,
                    })
                });
            item.try_set(Some(r));
        });
    });
    view! {
        {move || match item.get() {
            None => view! { <p class="muted">"Loading…"</p> }.into_any(),
            Some(Err(e)) => view! { <p class="bad">{e}</p> }.into_any(),
            Some(Ok(it)) => view! { <Watch item=it/> }.into_any(),
        }}
    }
}

/// `/watch/tv/:id/:eid`
#[component]
pub fn WatchEpisodePage() -> impl IntoView {
    let params = use_params_map();
    let item = RwSignal::new(None::<Result<WatchItem, String>>);
    Effect::new(move |_| {
        let p = params.get();
        let (sid, eid) = (
            p.get("id").unwrap_or_default(),
            p.get("eid").unwrap_or_default(),
        );
        spawn_local(async move {
            let r = api::get_series(&sid)
                .await
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    let ep =
                        s.episodes.iter().find(|e| e.id == eid).ok_or_else(|| {
                            "That episode isn't in the library any more.".to_string()
                        })?;
                    let code = episode_code(ep.season, ep.number);
                    let link = |e: &api::Episode| {
                        (
                            format!("/watch/tv/{sid}/{}", e.id),
                            episode_code(e.season, e.number),
                        )
                    };
                    Ok(WatchItem {
                        title: format!("{} · {code}", s.title),
                        subtitle: ep.title.clone(),
                        back_href: format!("/tv/{sid}"),
                        back_label: "Series".into(),
                        src: api::episode_video_url(&sid, &eid),
                        video_path: format!("{}/series/{sid}/episodes/{eid}/video", api::API_BASE),
                        key: format!("episode:{sid}:{eid}"),
                        media_info: ep.media_info.clone(),
                        prev: prev_episode(&s.episodes, ep.season, ep.number).map(link),
                        next: next_episode(&s.episodes, ep.season, ep.number).map(link),
                    })
                });
            item.try_set(Some(r));
        });
    });
    view! {
        {move || match item.get() {
            None => view! { <p class="muted">"Loading…"</p> }.into_any(),
            Some(Err(e)) => view! { <p class="bad">{e}</p> }.into_any(),
            Some(Ok(it)) => view! { <Watch item=it/> }.into_any(),
        }}
    }
}
