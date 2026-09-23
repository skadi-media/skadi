//! Settings → Household (SKADI-T-0613): the operator manages who can pair
//! the app and what each of them may see. Members and policies are the
//! SKADI-T-0611 API; the pairing QR is `/pair/app?member=` so a phone paired
//! from here carries that member's own token.
//!
//! Pure helpers (`rating_options`, `policy_summary`, `PolicyForm`,
//! `search_titles`) are `pub` so `tests/logic.rs` can pin them.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

use crate::api::{self, MaxRating, Member, Policy};
use crate::settings::window_confirm;

/// The ordered US scales (skadi-core `rating`): a ceiling is one of these.
#[must_use]
pub fn rating_options(kind: &str) -> &'static [&'static str] {
    match kind {
        "movie" => &["G", "PG", "PG-13", "R", "NC-17"],
        "series" => &["TV-Y", "TV-Y7", "TV-G", "TV-PG", "TV-14", "TV-MA"],
        _ => &[],
    }
}

#[must_use]
pub fn role_label(role: &str) -> &'static str {
    match role {
        "admin" => "Operator",
        "member" => "Member",
        "contributor" => "Contributor",
        "kid" => "Kid",
        _ => "Member",
    }
}

/// One line per rule the policy actually carries, in the order the server
/// applies them; an admin's or an empty policy summarises to "Sees everything".
#[must_use]
pub fn policy_summary(role: &str, p: &Policy) -> Vec<String> {
    if role == "admin" {
        return vec!["Everything, and the controls".to_string()];
    }
    if role == "contributor" {
        return vec!["Everything, and can add and search".to_string()];
    }
    let mut out = Vec::new();
    let all = ["movie", "series", "audiobook"];
    let missing: Vec<&str> = all
        .iter()
        .copied()
        .filter(|k| !p.kinds.iter().any(|x| x == k))
        .collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing
            .iter()
            .map(|k| match *k {
                "movie" => "movies",
                "series" => "TV",
                _ => "audiobooks",
            })
            .collect();
        out.push(format!("No {}", names.join(", ")));
    }
    if let Some(m) = p.max_rating.movie.as_deref().filter(|s| !s.is_empty()) {
        out.push(format!("Movies up to {m}"));
    }
    if let Some(s) = p.max_rating.series.as_deref().filter(|s| !s.is_empty()) {
        out.push(format!("TV up to {s}"));
    }
    if !p.blocked_genres.is_empty() {
        out.push(format!("No {}", p.blocked_genres.join(", ")));
    }
    if !p.blocked_items.is_empty() {
        out.push(format!("{} title(s) blocked", p.blocked_items.len()));
    }
    if !p.allowed_items.is_empty() {
        out.push(format!("{} title(s) always allowed", p.allowed_items.len()));
    }
    if role == "kid" {
        out.push(match p.allowed_books.len() {
            0 => "No audiobooks".to_string(),
            n => format!("{n} audiobook(s)"),
        });
    }
    if out.is_empty() {
        out.push("Sees everything".to_string());
    }
    out
}

/// A title the pickers can offer: id, label, and its kind for the chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRef {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
}

/// Case-insensitive substring search over the library, at most `limit` hits,
/// already-picked ids excluded. Empty queries find nothing (the list would be
/// the whole library).
#[must_use]
pub fn search_titles<'a>(
    query: &str,
    items: &'a [TitleRef],
    picked: &[String],
    limit: usize,
) -> Vec<&'a TitleRef> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    items
        .iter()
        .filter(|t| !picked.iter().any(|p| p == &t.id))
        .filter(|t| t.label.to_lowercase().contains(&q))
        .take(limit)
        .collect()
}

/// The policy editor's state: the same shape as [`Policy`] but with the
/// selects as plain strings ("" = no ceiling) so the form binds directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyForm {
    pub movies: bool,
    pub series: bool,
    pub audiobooks: bool,
    pub max_movie: String,
    pub max_series: String,
    pub blocked_genres: BTreeSet<String>,
    pub blocked_items: Vec<String>,
    pub allowed_items: Vec<String>,
    pub allowed_books: Vec<String>,
}

impl PolicyForm {
    #[must_use]
    pub fn from_policy(p: &Policy) -> Self {
        Self {
            movies: p.kinds.iter().any(|k| k == "movie"),
            series: p.kinds.iter().any(|k| k == "series"),
            audiobooks: p.kinds.iter().any(|k| k == "audiobook"),
            max_movie: p.max_rating.movie.clone().unwrap_or_default(),
            max_series: p.max_rating.series.clone().unwrap_or_default(),
            blocked_genres: p.blocked_genres.iter().cloned().collect(),
            blocked_items: p.blocked_items.clone(),
            allowed_items: p.allowed_items.clone(),
            allowed_books: p.allowed_books.clone(),
        }
    }

    #[must_use]
    pub fn to_policy(&self) -> Policy {
        let mut kinds = Vec::new();
        if self.movies {
            kinds.push("movie".to_string());
        }
        if self.series {
            kinds.push("series".to_string());
        }
        if self.audiobooks {
            kinds.push("audiobook".to_string());
        }
        let ceiling = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        Policy {
            kinds,
            max_rating: MaxRating {
                movie: ceiling(&self.max_movie),
                series: ceiling(&self.max_series),
            },
            blocked_genres: self.blocked_genres.iter().cloned().collect(),
            blocked_items: self.blocked_items.clone(),
            allowed_items: self.allowed_items.clone(),
            allowed_books: self.allowed_books.clone(),
        }
    }

    /// The JSON the API takes (`policy` on create and PATCH).
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self.to_policy()).unwrap_or(Value::Null)
    }
}

impl Default for PolicyForm {
    fn default() -> Self {
        Self::from_policy(&Policy::default())
    }
}

/// The library, flattened for the pickers and the genre list.
#[derive(Debug, Clone, Default)]
struct Library {
    videos: Vec<TitleRef>,
    books: Vec<TitleRef>,
    genres: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Editor {
    Closed,
    Add,
    Edit(String),
}

#[component]
pub fn HouseholdPage() -> impl IntoView {
    let members = RwSignal::new(Vec::<Member>::new());
    let load_error = RwSignal::new(None::<String>);
    let library = RwSignal::new(Library::default());
    // The open editor survives a refresh (SKADI-T-0596 rule).
    let editor = crate::persist::persisted("household.editor".to_string(), Editor::Closed);
    let name = RwSignal::new(String::new());
    let role = RwSignal::new("kid".to_string());
    let pin = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let username = RwSignal::new(String::new());
    let policy = RwSignal::new(PolicyForm::default());
    let form_error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // A token shown once: (member name, token).
    let fresh_token = RwSignal::new(None::<(String, String)>);
    // Which member's pairing QR is open, plus the fetched QR / URL override.
    let qr_for = RwSignal::new(None::<String>);
    let qr = RwSignal::new(None::<api::PairQr>);
    let qr_err = RwSignal::new(None::<String>);
    let qr_url = RwSignal::new(String::new());

    let refresh = move || {
        spawn_local(async move {
            match api::list_members().await {
                Ok(list) => {
                    members.set(list);
                    load_error.set(None);
                }
                Err(e) => load_error.set(Some(e.to_string())),
            }
        });
    };
    Effect::new(move |_| {
        refresh();
        spawn_local(async move {
            let movies = api::list_movies().await;
            let series = api::list_series().await;
            let books = api::list_books(None, None).await;
            let mut lib = Library::default();
            let mut genres: Vec<&[String]> = Vec::new();
            let movies = movies.unwrap_or_default();
            let series = series.unwrap_or_default();
            for m in &movies {
                let year = m.year.map(|y| format!(" ({y})")).unwrap_or_default();
                lib.videos.push(TitleRef {
                    id: m.id.clone(),
                    label: format!("{}{year}", m.title),
                    kind: "movie",
                });
                genres.push(&m.genres);
            }
            for s in &series {
                let year = s.year.map(|y| format!(" ({y})")).unwrap_or_default();
                lib.videos.push(TitleRef {
                    id: s.id.clone(),
                    label: format!("{}{year}", s.title),
                    kind: "series",
                });
                genres.push(&s.genres);
            }
            lib.videos.sort_by(|a, b| a.label.cmp(&b.label));
            for b in books.unwrap_or_default() {
                let by = if b.authors.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", b.authors.join(", "))
                };
                lib.books.push(TitleRef {
                    id: b.id.clone(),
                    label: format!("{}{by}", b.title),
                    kind: "audiobook",
                });
            }
            lib.books.sort_by(|a, b| a.label.cmp(&b.label));
            lib.genres = crate::genre_counts(genres).into_iter().map(|(g, _)| g).collect();
            lib.genres.sort();
            library.set(lib);
        });
    });

    let open_add = move |_| {
        name.set(String::new());
        role.set("kid".to_string());
        pin.set(String::new());
        password.set(String::new());
        username.set(String::new());
        policy.set(PolicyForm::default());
        form_error.set(None);
        editor.set(Editor::Add);
    };
    let open_edit = move |m: Member| {
        name.set(m.name.clone());
        role.set(m.role.clone());
        pin.set(String::new());
        password.set(String::new());
        username.set(m.username.clone().unwrap_or_default());
        policy.set(PolicyForm::from_policy(&m.policy));
        form_error.set(None);
        editor.set(Editor::Edit(m.id.clone()));
    };
    let cancel = move |_| {
        editor.set(Editor::Closed);
        form_error.set(None);
    };

    let submit = move |_| {
        let mode = editor.get_untracked();
        let n = name.get_untracked().trim().to_string();
        if n.is_empty() {
            form_error.set(Some("A name is needed.".into()));
            return;
        }
        let r = role.get_untracked();
        let p = pin.get_untracked();
        let pw = password.get_untracked();
        let un = username.get_untracked();
        let pol = policy.get_untracked().to_json();
        busy.set(true);
        form_error.set(None);
        spawn_local(async move {
            let result = match &mode {
                Editor::Edit(id) => {
                    let mut body = json!({ "name": n, "policy": pol });
                    if r != "admin" {
                        body["role"] = Value::String(r);
                    }
                    if !p.trim().is_empty() {
                        body["pin"] = Value::String(p.trim().to_string());
                    }
                    if !pw.trim().is_empty() {
                        body["password"] = Value::String(pw.trim().to_string());
                    }
                    // Only sent when the operator actually edited it, so a
                    // plain rename lets the login name follow the display name.
                    if !un.trim().is_empty() && un.trim() != n {
                        body["username"] = Value::String(un.trim().to_string());
                    }
                    api::update_member(id, &body).await.map(|_| None)
                }
                _ => {
                    let mut body = json!({ "name": n, "role": r, "policy": pol });
                    if !p.trim().is_empty() {
                        body["pin"] = Value::String(p.trim().to_string());
                    }
                    if !pw.trim().is_empty() {
                        body["password"] = Value::String(pw.trim().to_string());
                    }
                    api::create_member(&body)
                        .await
                        .map(|c| Some((c.member.name, c.token)))
                }
            };
            busy.set(false);
            match result {
                Ok(token) => {
                    if token.is_some() {
                        fresh_token.set(token);
                    }
                    editor.set(Editor::Closed);
                    refresh();
                }
                Err(e) => form_error.set(Some(e.to_string())),
            }
        });
    };

    let fetch_qr = move |member: String| {
        qr.set(None);
        qr_err.set(None);
        // Default to the address this browser reached the server by: the
        // operator's browser knows a working host, the container does not
        // (its configured advertise host can go stale when the LAN changes).
        if qr_url.get_untracked().trim().is_empty()
            && let Some(w) = web_sys::window()
            && let Ok(host) = w.location().host()
            && !host.is_empty()
        {
            qr_url.set(host);
        }
        let url = qr_url.get_untracked();
        spawn_local(async move {
            match api::pair_app_qr_for(&member, &url).await {
                Ok(p) => {
                    let _ = qr.try_set(Some(p));
                }
                Err(e) => {
                    let _ = qr_err.try_set(Some(e.to_string()));
                }
            }
        });
    };
    let toggle_qr = move |id: String| {
        if qr_for.get_untracked().as_deref() == Some(id.as_str()) {
            qr_for.set(None);
            return;
        }
        qr_for.set(Some(id.clone()));
        fetch_qr(id);
    };
    let reissue = move |m: Member| {
        if !window_confirm(&format!(
            "Re-issue {}'s token? Their phone will need to pair again.",
            m.name
        )) {
            return;
        }
        spawn_local(async move {
            match api::reissue_member_token(&m.id).await {
                Ok(c) => {
                    fresh_token.set(Some((c.member.name, c.token)));
                    qr_for.set(None);
                    refresh();
                }
                Err(e) => load_error.set(Some(e.to_string())),
            }
        });
    };
    let signout = move |m: Member| {
        if !window_confirm(&format!(
            "Sign out all {} of {}'s devices? They will need to sign in again.",
            m.devices, m.name
        )) {
            return;
        }
        spawn_local(async move {
            match api::signout_member(&m.id).await {
                Ok(_) => refresh(),
                Err(e) => load_error.set(Some(e.to_string())),
            }
        });
    };
    let revoke = move |m: Member| {
        if !window_confirm(&format!(
            "Remove {} from the household? Their phone stops working immediately.",
            m.name
        )) {
            return;
        }
        spawn_local(async move {
            match api::delete_member(&m.id).await {
                Ok(()) => {
                    if editor.get_untracked() == Editor::Edit(m.id.clone()) {
                        editor.set(Editor::Closed);
                    }
                    refresh();
                }
                Err(e) => load_error.set(Some(e.to_string())),
            }
        });
    };

    let rows = move || {
        members
            .get()
            .into_iter()
            .map(|m| {
                let is_admin = m.role == "admin";
                let summary = policy_summary(&m.role, &m.policy);
                let seen = m
                    .last_seen_at
                    .as_deref()
                    .map(|t| format!("seen {}", t.get(..10).unwrap_or(t)))
                    .unwrap_or_else(|| "never paired".to_string());
                let role_class = format!("pill role-{}", m.role);
                let m_edit = m.clone();
                let m_qr = m.id.clone();
                let m_re = m.clone();
                let m_rv = m.clone();
                let qr_open = move || qr_for.get().as_deref() == Some(m_qr.as_str());
                let qr_id = m.id.clone();
                let qr_panel = move || {
                    if !qr_open() {
                        return ().into_any();
                    }
                    let body = match (qr.get(), qr_err.get()) {
                        (Some(p), _) => {
                            let link = p.url.clone();
                            let link_copy = link.clone();
                            let copied = RwSignal::new(false);
                            view! {
                                <div class="pair-qr" inner_html=p.qr_svg.clone()></div>
                                // The same payload as a link: send it to the phone by
                                // any message and tap it there — no scanning, no typing.
                                <div class="pair-link-row">
                                    <code class="pair-link">{link}</code>
                                    <button type="button" on:click=move |_| {
                                        if let Some(w) = web_sys::window() {
                                            let _ = w.navigator().clipboard().write_text(&link_copy);
                                            copied.set(true);
                                        }
                                    }>{move || if copied.get() { "Copied" } else { "Copy link" }}</button>
                                </div>
                            }
                            .into_any()
                        }
                        (None, Some(e)) => {
                            view! { <p class="bad">"Couldn't generate the QR: " {e}</p> }.into_any()
                        }
                        (None, None) => view! { <p class="muted">"Generating…"</p> }.into_any(),
                    };
                    let refetch_id = qr_id.clone();
                    let refetch_id2 = qr_id.clone();
                    view! {
                        <div class="pair-card">
                            {body}
                            <div class="pair-url-row">
                                <input
                                    class="path-field mono"
                                    r#type="text"
                                    placeholder="host override, e.g. http://<tailscale-ip>:8080"
                                    prop:value=move || qr_url.get()
                                    on:input=move |ev| qr_url.set(event_target_value(&ev))
                                    on:change=move |_| fetch_qr(refetch_id.clone())
                                />
                                <button type="button" on:click=move |_| fetch_qr(refetch_id2.clone())>"Update"</button>
                            </div>
                            <p class="muted">"Scan it from the app's pairing screen, or send the link to the phone and open it there. Either way the phone pairs as this member."</p>
                        </div>
                    }
                    .into_any()
                };
                view! {
                    <div class="card member-card">
                        <div class="card-main">
                            <strong>{m.name.clone()}</strong>
                            <span class=role_class>{role_label(&m.role)}</span>
                            {m.has_pin.then(|| view! { <span class="pill">"PIN"</span> })}
                            {(!m.has_password && !is_admin).then(|| view! {
                                <span class="pill" title="Set a password so they can sign in">
                                    "no password"
                                </span>
                            })}
                            <span class="muted summary">{seen}</span>
                            {(m.devices > 0).then(|| view! {
                                <span class="muted summary">{format!("{} device(s)", m.devices)}</span>
                            })}
                        </div>
                        <div class="card-actions">
                            <button on:click=move |_| open_edit(m_edit.clone())>"Edit"</button>
                            {(!is_admin).then(|| {
                                let id = m.id.clone();
                                view! { <button on:click=move |_| toggle_qr(id.clone())>"Pair phone"</button> }
                            })}
                            {(!is_admin).then(|| view! {
                                <button on:click=move |_| reissue(m_re.clone())>"Re-issue token"</button>
                            })}
                            {(m.devices > 0).then(|| {
                                let m_so = m.clone();
                                view! {
                                    <button on:click=move |_| signout(m_so.clone())>"Sign out devices"</button>
                                }
                            })}
                            {(!is_admin).then(|| view! {
                                <button class="danger" on:click=move |_| revoke(m_rv.clone())>"Remove"</button>
                            })}
                        </div>
                        <div class="member-policy">
                            {m.username.clone().map(|u| view! {
                                <span>{format!("signs in as {u}")}</span>
                            })}
                            {summary.into_iter().map(|s| view! { <span>{s}</span> }).collect_view()}
                        </div>
                        {qr_panel}
                    </div>
                }
            })
            .collect_view()
    };

    let token_panel = move || {
        fresh_token.get().map(|(who, tok)| {
            view! {
                <div class="token-once">
                    <strong>{format!("{who}'s token")}</strong>
                    <code>{tok}</code>
                    <p class="muted">"Shown once. Pair the phone with the QR on their row, or type the server address and this token on the app's pairing screen."</p>
                    <div class="form-actions">
                        <button on:click=move |_| fresh_token.set(None)>"Done"</button>
                    </div>
                </div>
            }
        })
    };

    let form_panel = move || {
        let mode = editor.get();
        if mode == Editor::Closed {
            return ().into_any();
        }
        let editing = matches!(mode, Editor::Edit(_));
        let editing_admin = editing && role.get() == "admin";
        let title = if editing { "Edit member" } else { "Add member" };
        view! {
            <div class="form policy-form">
                <h4>{title}</h4>
                <div class="policy-grid">
                    <div class="field">
                        <label>"Name"</label>
                        <input
                            r#type="text"
                            prop:value=move || name.get()
                            on:input=move |ev| name.set(event_target_value(&ev))
                        />
                    </div>
                    {(!editing_admin).then(|| view! {
                        <div class="field">
                            <label>"Role"</label>
                            <select
                                prop:value=move || role.get()
                                on:change=move |ev| role.set(event_target_value(&ev))
                            >
                                <option value="kid" selected=move || role.get() == "kid">"Kid — sees only what the policy allows"</option>
                                <option value="member" selected=move || role.get() == "member">"Member — sees and plays everything"</option>
                                <option value="contributor" selected=move || role.get() == "contributor">"Contributor — also adds media and starts searches"</option>
                            </select>
                        </div>
                    })}
                    {editing.then(|| view! {
                        <div class="field">
                            <label>"Signs in as"</label>
                            <input
                                r#type="text"
                                prop:value=move || username.get()
                                on:input=move |ev| username.set(event_target_value(&ev))
                            />
                            <p class="muted">"Renaming them changes this too, unless you set it apart."</p>
                        </div>
                    })}
                    <div class="field">
                        <label>
                            {if editing { "New password (leave blank to keep)" } else { "Password" }}
                        </label>
                        <input
                            r#type="password"
                            autocomplete="new-password"
                            prop:value=move || password.get()
                            on:input=move |ev| password.set(event_target_value(&ev))
                        />
                        {move || (editing && !password.get().trim().is_empty()).then(|| view! {
                            <p class="muted">
                                "Saving signs their devices out — which is the point when a password
                                 has reached someone it should not have."
                            </p>
                        })}
                    </div>
                    <div class="field">
                        <label>{if editing { "New PIN (leave blank to keep)" } else { "PIN (optional)" }}</label>
                        <input
                            r#type="password"
                            prop:value=move || pin.get()
                            on:input=move |ev| pin.set(event_target_value(&ev))
                        />
                    </div>
                </div>
                {(!editing_admin).then(|| policy_editor(policy, library))}
                {move || form_error.get().map(|e| view! { <p class="bad">{e}</p> })}
                <div class="form-actions">
                    <button on:click=submit disabled=move || busy.get()>
                        {if editing { "Save" } else { "Create" }}
                    </button>
                    <button on:click=cancel>"Cancel"</button>
                </div>
            </div>
        }
        .into_any()
    };

    view! {
        <crate::subnav::SubNav/>
        <div class="page-head">
            <h2 class="page-title">"Household"</h2>
        </div>
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>"Members"</h3>
                    <p class="muted">"Everyone who can pair the app. Members see and play everything; kids see only what their policy allows; only the operator changes the library."</p>
                </div>
                <button on:click=open_add>"+ Add member"</button>
            </div>
            {move || load_error.get().map(|e| view! { <p class="bad">{e}</p> })}
            {token_panel}
            <div class="cards">{rows}</div>
            {form_panel}
        </section>
    }
}

/// The policy editor: kinds, ceilings, blocked genres and the three pickers.
fn policy_editor(policy: RwSignal<PolicyForm>, library: RwSignal<Library>) -> AnyView {
    let kind_check = |label: &'static str, get: fn(&PolicyForm) -> bool, set: fn(&mut PolicyForm, bool)| {
        view! {
            <label class="check">
                <input
                    r#type="checkbox"
                    prop:checked=move || get(&policy.get())
                    on:change=move |ev| policy.update(|p| set(p, event_target_checked(&ev)))
                />
                {label}
            </label>
        }
    };
    let ceiling = |label: &'static str, kind: &'static str, get: fn(&PolicyForm) -> String, set: fn(&mut PolicyForm, String)| {
        let options = rating_options(kind);
        view! {
            <div class="field">
                <label>{label}</label>
                <select
                    prop:value=move || get(&policy.get())
                    on:change=move |ev| policy.update(|p| set(p, event_target_value(&ev)))
                >
                    <option value="" selected=move || get(&policy.get()).is_empty()>"No ceiling"</option>
                    {options.iter().map(|o| {
                        let o = *o;
                        view! { <option value=o selected=move || get(&policy.get()) == o>{format!("Up to {o}")}</option> }
                    }).collect_view()}
                </select>
            </div>
        }
    };
    let genres = move || {
        library
            .get()
            .genres
            .into_iter()
            .map(|g| {
                let g2 = g.clone();
                let g3 = g.clone();
                view! {
                    <label class="check">
                        <input
                            r#type="checkbox"
                            prop:checked=move || policy.get().blocked_genres.contains(&g2)
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                let g = g3.clone();
                                policy.update(|p| {
                                    if on { p.blocked_genres.insert(g); } else { p.blocked_genres.remove(&g); }
                                })
                            }
                        />
                        {g}
                    </label>
                }
            })
            .collect_view()
    };
    let videos = Signal::derive(move || library.get().videos);
    let books = Signal::derive(move || library.get().books);
    view! {
        <div class="policy-grid">
            <div class="field">
                <label>"Can see"</label>
                <div class="checks">
                    {kind_check("Movies", |p| p.movies, |p, v| p.movies = v)}
                    {kind_check("TV", |p| p.series, |p, v| p.series = v)}
                    {kind_check("Audiobooks", |p| p.audiobooks, |p, v| p.audiobooks = v)}
                </div>
            </div>
            {ceiling("Movies rated", "movie", |p| p.max_movie.clone(), |p, v| p.max_movie = v)}
            {ceiling("TV rated", "series", |p| p.max_series.clone(), |p, v| p.max_series = v)}
        </div>
        <div class="field">
            <label>"Blocked genres"</label>
            <div class="checks">{genres}</div>
        </div>
        <div class="policy-grid">
            {picker("Always hidden", "Search movies and TV…", videos,
                Signal::derive(move || policy.get().blocked_items),
                Callback::new(move |ids: Vec<String>| policy.update(|p| p.blocked_items = ids)))}
            {picker("Always allowed (overrides rating and genre)", "Search movies and TV…", videos,
                Signal::derive(move || policy.get().allowed_items),
                Callback::new(move |ids: Vec<String>| policy.update(|p| p.allowed_items = ids)))}
            {picker("Audiobooks a kid may see", "Search audiobooks…", books,
                Signal::derive(move || policy.get().allowed_books),
                Callback::new(move |ids: Vec<String>| policy.update(|p| p.allowed_books = ids)))}
        </div>
        <p class="muted">"Unrated titles are hidden from kids unless listed under \"Always allowed\". Kids see no audiobooks except those listed."</p>
    }
    .into_any()
}

/// A title picker: a search box over `items`, hits below it, picked ids as
/// removable chips.
fn picker(
    label: &'static str,
    placeholder: &'static str,
    items: Signal<Vec<TitleRef>>,
    picked: Signal<Vec<String>>,
    set: Callback<Vec<String>>,
) -> AnyView {
    let query = RwSignal::new(String::new());
    let hits = move || {
        let items = items.get();
        let picked_now = picked.get();
        search_titles(&query.get(), &items, &picked_now, 12)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
    };
    let chips = move || {
        let items = items.get();
        picked
            .get()
            .into_iter()
            .map(|id| {
                let label = items
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.label.clone())
                    .unwrap_or_else(|| id.clone());
                let id2 = id.clone();
                view! {
                    <span class="chip">
                        {label}
                        <button type="button" title="Remove" on:click=move |_| {
                            let mut now = picked.get_untracked();
                            now.retain(|x| x != &id2);
                            set.run(now);
                        }>"×"</button>
                    </span>
                }
            })
            .collect_view()
    };
    view! {
        <div class="field picker">
            <label>{label}</label>
            <input
                r#type="text"
                placeholder=placeholder
                prop:value=move || query.get()
                on:input=move |ev| query.set(event_target_value(&ev))
            />
            <div class="picker-hits">
                {move || hits().into_iter().map(|t| {
                    let id = t.id.clone();
                    let kind = match t.kind { "movie" => "film", "series" => "tv", _ => "book" };
                    view! {
                        <button type="button" class="picker-hit" on:click=move |_| {
                            let mut now = picked.get_untracked();
                            if !now.contains(&id) { now.push(id.clone()); }
                            set.run(now);
                            query.set(String::new());
                        }>
                            <span>{t.label.clone()}</span>
                            <span class=format!("tag {kind}")>{t.kind}</span>
                        </button>
                    }
                }).collect_view()}
            </div>
            <div class="picked">{chips}</div>
        </div>
    }
    .into_any()
}
