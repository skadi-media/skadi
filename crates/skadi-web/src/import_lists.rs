//! The import-list editor (SKADI-T-0733): create, edit, sync and delete the
//! lists the daemon syncs into the library (SKADI-T-0511), and keep the items
//! no list may add.
//!
//! The form knows each provider's settings ([`KINDS`]); the server keeps the
//! id, the creation time and the sync history, so an edit cannot wipe them.

use aurora_leptos::components::{
    Alert, Button, Card, Panel, PasswordInput, Pill, Select, Switch, TextInput,
};
use aurora_leptos::tokens::token;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Map, Value};

use crate::api::{self, ImportList, ImportListExclusion, ImportListSyncReport};
use crate::confirm::{ConfirmSpec, confirm};

/// One setting a provider reads from the list's `settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    /// A credential: drawn as a password field.
    pub secret: bool,
}

/// A provider the daemon registers, with the domain it adds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindSpec {
    pub slug: &'static str,
    pub label: &'static str,
    /// `movie` or `series`: what the provider's ids can add.
    pub domain: &'static str,
    pub fields: &'static [FieldSpec],
    pub help: &'static str,
}

const LIST_ID: FieldSpec = FieldSpec {
    key: "list_id",
    label: "TMDB id",
    secret: false,
};
const BASE_URL: FieldSpec = FieldSpec {
    key: "base_url",
    label: "Server URL",
    secret: false,
};
const API_KEY: FieldSpec = FieldSpec {
    key: "api_key",
    label: "API key",
    secret: true,
};

/// The providers the daemon registers (`skadi-cli` main). The TMDB two need
/// the daemon's TMDB key; without it a sync says so.
pub const KINDS: &[KindSpec] = &[
    KindSpec {
        slug: "tmdb_collection",
        label: "TMDB collection",
        domain: "movie",
        fields: &[LIST_ID],
        help: "Every film of a TMDB collection, such as a franchise. The id is the number in the collection's TMDB address.",
    },
    KindSpec {
        slug: "tmdb_list",
        label: "TMDB list",
        domain: "movie",
        fields: &[LIST_ID],
        help: "The films of a public TMDB list. The id is the number in the list's TMDB address.",
    },
    KindSpec {
        slug: "radarr",
        label: "Radarr library",
        domain: "movie",
        fields: &[BASE_URL, API_KEY],
        help: "Every movie of a Radarr server, to move a library over.",
    },
    KindSpec {
        slug: "sonarr",
        label: "Sonarr library",
        domain: "series",
        fields: &[BASE_URL, API_KEY],
        help: "Every series of a Sonarr server, to move a library over.",
    },
];

/// The sync intervals the form offers, in minutes.
pub const INTERVALS: &[(i32, &str)] = &[
    (360, "Every 6 hours"),
    (720, "Every 12 hours"),
    (1440, "Daily"),
    (10_080, "Weekly"),
];

/// The spec of a provider slug.
#[must_use]
pub fn kind_spec(slug: &str) -> Option<&'static KindSpec> {
    KINDS.iter().find(|k| k.slug == slug)
}

/// The label of a provider slug, or the slug itself for one the form does not
/// know.
#[must_use]
pub fn kind_label(slug: &str) -> String {
    kind_spec(slug).map_or_else(|| slug.to_string(), |k| k.label.to_string())
}

/// The label of a domain.
#[must_use]
pub fn domain_label(domain: &str) -> &'static str {
    match domain {
        "movie" | "movies" => "Movies",
        "series" | "tv" => "TV",
        "audiobook" | "audiobooks" => "Audiobooks",
        _ => "Unknown",
    }
}

/// The id namespace a domain's items carry.
#[must_use]
pub fn default_id_kind(domain: &str) -> &'static str {
    match domain {
        "series" => "tvdb",
        "audiobook" => "asin",
        _ => "tmdb",
    }
}

/// A setting as the form shows it: a string as is, a number as digits.
#[must_use]
pub fn field_value(settings: &Value, key: &str) -> String {
    match settings.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// The `settings` of a provider from the form's values, trimmed. A value the
/// provider does not read is dropped, so a list switched from Radarr to TMDB
/// does not keep the old API key.
#[must_use]
pub fn settings_for(kind: &str, value: impl Fn(&str) -> String) -> Value {
    let mut map = Map::new();
    if let Some(spec) = kind_spec(kind) {
        for f in spec.fields {
            map.insert(f.key.into(), Value::String(value(f.key).trim().to_string()));
        }
    }
    Value::Object(map)
}

/// Why the form cannot be saved, if it cannot.
#[must_use]
pub fn form_error(name: &str, kind: &str, value: impl Fn(&str) -> String) -> Option<String> {
    if name.trim().is_empty() {
        return Some("Give the list a name.".into());
    }
    let Some(spec) = kind_spec(kind) else {
        return Some("Choose a provider.".into());
    };
    spec.fields
        .iter()
        .find(|f| value(f.key).trim().is_empty())
        .map(|f| format!("{} is required.", f.label))
}

/// One line for a sync's outcome.
#[must_use]
pub fn sync_summary(name: &str, r: &ImportListSyncReport) -> String {
    let mut s = format!(
        "{name}: {} offered, {} added, {} already in the library, {} excluded",
        r.offered, r.added, r.existing, r.excluded
    );
    if !r.failed.is_empty() {
        s.push_str(&format!(", {} failed", r.failed.len()));
        if let Some((id, why)) = r.failed.first() {
            s.push_str(&format!(" (first: {id}: {why})"));
        }
    }
    s.push('.');
    s
}

/// When a list last synced, for its row.
#[must_use]
pub fn last_sync_label(list: &ImportList, now_ms: f64) -> String {
    match &list.last_synced_at {
        Some(at) => format!("Synced {}", crate::activity::ago(at, now_ms)),
        None => "Never synced".into(),
    }
}

/// The label of an interval in minutes.
#[must_use]
pub fn interval_label(minutes: i32) -> String {
    INTERVALS
        .iter()
        .find(|(m, _)| *m == minutes)
        .map_or_else(|| format!("Every {minutes} min"), |(_, l)| (*l).to_string())
}

/// The editor's state: one signal per field, so the form is one set for both
/// a new list and an edit.
#[derive(Clone, Copy)]
struct Form {
    /// `None`: closed. `Some("")`: a new list. `Some(id)`: editing that list.
    editing: RwSignal<Option<String>>,
    name: RwSignal<String>,
    kind: RwSignal<String>,
    list_id: RwSignal<String>,
    base_url: RwSignal<String>,
    api_key: RwSignal<String>,
    add_monitored: RwSignal<bool>,
    enabled: RwSignal<bool>,
    interval: RwSignal<String>,
}

impl Form {
    fn new() -> Self {
        Self {
            editing: RwSignal::new(None),
            name: RwSignal::new(String::new()),
            kind: RwSignal::new(KINDS[0].slug.to_string()),
            list_id: RwSignal::new(String::new()),
            base_url: RwSignal::new(String::new()),
            api_key: RwSignal::new(String::new()),
            add_monitored: RwSignal::new(false),
            enabled: RwSignal::new(true),
            interval: RwSignal::new("720".into()),
        }
    }

    fn field(self, key: &str) -> RwSignal<String> {
        match key {
            "base_url" => self.base_url,
            "api_key" => self.api_key,
            _ => self.list_id,
        }
    }

    fn value(self, key: &str) -> String {
        self.field(key).get_untracked()
    }

    fn open(self, list: Option<&ImportList>) {
        let blank = ImportList {
            kind: KINDS[0].slug.into(),
            enabled: true,
            interval_minutes: 720,
            ..ImportList::default()
        };
        let l = list.unwrap_or(&blank);
        self.name.set(l.name.clone());
        self.kind.set(l.kind.clone());
        self.list_id.set(field_value(&l.settings, "list_id"));
        self.base_url.set(field_value(&l.settings, "base_url"));
        self.api_key.set(field_value(&l.settings, "api_key"));
        self.add_monitored.set(l.add_monitored);
        self.enabled.set(l.enabled);
        self.interval.set(l.interval_minutes.to_string());
        self.editing.set(Some(l.id.clone()));
    }

    fn to_list(self) -> ImportList {
        let kind = self.kind.get_untracked();
        ImportList {
            id: self.editing.get_untracked().unwrap_or_default(),
            name: self.name.get_untracked().trim().to_string(),
            target_domain: kind_spec(&kind).map_or("movie", |k| k.domain).to_string(),
            settings: settings_for(&kind, |k| self.value(k)),
            kind,
            add_monitored: self.add_monitored.get_untracked(),
            enabled: self.enabled.get_untracked(),
            interval_minutes: self.interval.get_untracked().parse().unwrap_or(720),
            ..ImportList::default()
        }
    }
}

#[component]
pub fn ImportListsPage() -> impl IntoView {
    let lists = RwSignal::new(Vec::<ImportList>::new());
    let exclusions = RwSignal::new(Vec::<ImportListExclusion>::new());
    let reload = RwSignal::new(0u32);
    let error = RwSignal::new(None::<String>);
    let notice = RwSignal::new(None::<String>);
    let busy = RwSignal::new(None::<String>);
    let form = Form::new();
    let saving = RwSignal::new(false);
    let form_msg = RwSignal::new(String::new());

    Effect::new(move |_| {
        reload.get();
        spawn_local(async move {
            match api::list_import_lists().await {
                Ok(l) => {
                    lists.set(l);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.0)),
            }
            if let Ok(e) = api::list_import_exclusions().await {
                exclusions.set(e);
            }
        });
    });

    let save = move |()| {
        if let Some(why) = form_error(
            &form.name.get_untracked(),
            &form.kind.get_untracked(),
            |k| form.value(k),
        ) {
            form_msg.set(why);
            return;
        }
        let list = form.to_list();
        saving.set(true);
        form_msg.set(String::new());
        spawn_local(async move {
            let r = if list.id.is_empty() {
                api::create_import_list(&list).await
            } else {
                api::update_import_list(&list).await
            };
            saving.set(false);
            match r {
                Ok(saved) => {
                    notice.set(Some(format!("Saved {}.", saved.name)));
                    form.editing.set(None);
                    reload.update(|n| *n += 1);
                }
                Err(e) => form_msg.set(e.0),
            }
        });
    };

    let sync = move |l: ImportList| {
        busy.set(Some(l.id.clone()));
        spawn_local(async move {
            match api::sync_import_list(&l.id).await {
                Ok(r) => notice.set(Some(sync_summary(&l.name, &r))),
                Err(e) => notice.set(Some(format!("{}: {}", l.name, e.0))),
            }
            busy.set(None);
            reload.update(|n| *n += 1);
        });
    };

    let remove = move |l: ImportList| {
        spawn_local(async move {
            let body = format!(
                "Delete the list {}? What it already added stays in the library.",
                l.name
            );
            if !confirm(ConfirmSpec::destructive("Delete import list", body)).await {
                return;
            }
            match api::delete_import_list(&l.id).await {
                Ok(()) => notice.set(Some(format!("Deleted {}.", l.name))),
                Err(e) => notice.set(Some(e.0)),
            }
            reload.update(|n| *n += 1);
        });
    };

    let rows = move || {
        let now = js_sys::Date::now();
        lists
            .get()
            .into_iter()
            .map(|l| {
                let (l_sync, l_edit, l_del) = (l.clone(), l.clone(), l.clone());
                let id = l.id.clone();
                let syncing = Signal::derive(move || busy.get().as_deref() == Some(id.as_str()));
                let state = if l.enabled { "Enabled" } else { "Paused" };
                let meta = format!(
                    "{} · {} · {} · {}",
                    domain_label(&l.target_domain),
                    interval_label(l.interval_minutes),
                    if l.add_monitored { "adds monitored" } else { "adds unmonitored" },
                    last_sync_label(&l, now),
                );
                let last_error = l.last_error.clone();
                view! {
                    <Card class:import-list-row=true>
                        <div class="import-list-main">
                            <div class="import-list-title">
                                <strong>{l.name.clone()}</strong>
                                <Pill color=token::ICE>{kind_label(&l.kind)}</Pill>
                                <span class="muted">{state}</span>
                            </div>
                            <span class="muted import-list-meta">{meta}</span>
                            {last_error.map(|e| view! { <Alert color=token::BAD>{e}</Alert> })}
                        </div>
                        <div class="import-list-actions">
                            <Button variant="default" size="xs" loading=syncing
                                on_click=Callback::new(move |()| sync(l_sync.clone()))>"Sync now"</Button>
                            <Button variant="subtle" size="xs"
                                on_click=Callback::new(move |()| form.open(Some(&l_edit)))>"Edit"</Button>
                            <Button variant="subtle" size="xs" bad=true
                                on_click=Callback::new(move |()| remove(l_del.clone()))>"Delete"</Button>
                        </div>
                    </Card>
                }
            })
            .collect_view()
    };

    let kind_pairs: Vec<(String, String)> = KINDS
        .iter()
        .map(|k| {
            (
                k.slug.to_string(),
                format!("{} ({})", k.label, domain_label(k.domain)),
            )
        })
        .collect();
    let interval_pairs: Vec<(String, String)> = INTERVALS
        .iter()
        .map(|(m, l)| (m.to_string(), (*l).to_string()))
        .collect();

    let editor = move || {
        let kind_pairs = kind_pairs.clone();
        let interval_pairs = interval_pairs.clone();
        form.editing.get().map(move |id| {
            let title = if id.is_empty() { "New import list" } else { "Edit import list" };
            let fields = move || {
                kind_spec(&form.kind.get())
                    .map(|spec| {
                        let help = spec.help;
                        let inputs = spec
                            .fields
                            .iter()
                            .map(|f| {
                                let v = form.field(f.key);
                                if f.secret {
                                    view! { <PasswordInput label=f.label value=v/> }.into_any()
                                } else {
                                    view! { <TextInput label=f.label value=v mono=true spellcheck=false/> }
                                        .into_any()
                                }
                            })
                            .collect_view();
                        view! { <p class="muted">{help}</p>{inputs} }
                    })
            };
            view! {
                <Panel title=title>
                    <div class="import-list-form">
                        <TextInput label="Name" value=form.name/>
                        <Select label="Provider" option_pairs=kind_pairs value=form.kind/>
                        {fields}
                        <Select label="Sync" option_pairs=interval_pairs value=form.interval/>
                        <Switch checked=form.add_monitored label="Add items monitored, so they are searched for"/>
                        <Switch checked=form.enabled label="Enabled"/>
                        <div class="import-list-form-actions">
                            <Button button_type="button" loading=saving on_click=Callback::new(save)>"Save"</Button>
                            <Button variant="subtle" button_type="button"
                                on_click=Callback::new(move |()| form.editing.set(None))>"Cancel"</Button>
                            <span class="muted">{move || form_msg.get()}</span>
                        </div>
                    </div>
                </Panel>
            }
        })
    };

    view! {
        <crate::subnav::SubNav/>
        <div class="page-head">
            <h2>"Import lists"</h2>
            <Button on_click=Callback::new(move |()| form.open(None))>"Add list"</Button>
        </div>
        <p class="muted">
            "A list adds what it offers to the library on each sync. Items arrive unmonitored unless the list says otherwise; an excluded item is never added."
        </p>
        {move || error.get().map(|e| view! { <Alert color=token::BAD>{e}</Alert> })}
        {move || notice.get().map(|n| view! { <Alert>{n}</Alert> })}
        {editor}
        <div class="import-lists">
            {rows}
            {move || (lists.with(Vec::is_empty) && error.get().is_none())
                .then(|| view! { <p class="muted">"No import lists yet."</p> })}
        </div>
        <Exclusions exclusions=exclusions reload=reload/>
    }
}

/// The items no list may add, and a form to add one.
#[component]
fn Exclusions(
    exclusions: RwSignal<Vec<ImportListExclusion>>,
    reload: RwSignal<u32>,
) -> impl IntoView {
    let domain = RwSignal::new("movie".to_string());
    let id_kind = RwSignal::new("tmdb".to_string());
    let external_id = RwSignal::new(String::new());
    let title = RwSignal::new(String::new());
    let msg = RwSignal::new(String::new());

    let domain_pairs: Vec<(String, String)> = ["movie", "series", "audiobook"]
        .iter()
        .map(|d| ((*d).to_string(), domain_label(d).to_string()))
        .collect();
    let id_pairs: Vec<(String, String)> = ["tmdb", "tvdb", "imdb", "asin"]
        .iter()
        .map(|k| ((*k).to_string(), k.to_uppercase()))
        .collect();

    let add = move |()| {
        let e = ImportListExclusion {
            target_domain: domain.get_untracked(),
            id_kind: id_kind.get_untracked(),
            external_id: external_id.get_untracked().trim().to_string(),
            title: Some(title.get_untracked().trim().to_string()).filter(|t| !t.is_empty()),
            ..ImportListExclusion::default()
        };
        if e.external_id.is_empty() {
            msg.set("Give the item's id.".into());
            return;
        }
        spawn_local(async move {
            match api::add_import_exclusion(&e).await {
                Ok(_) => {
                    msg.set(String::new());
                    external_id.set(String::new());
                    title.set(String::new());
                    reload.update(|n| *n += 1);
                }
                Err(err) => msg.set(err.0),
            }
        });
    };

    let rows = move || {
        exclusions
            .get()
            .into_iter()
            .map(|e| {
                let id = e.id.clone();
                let shown = e.title.clone().unwrap_or_else(|| e.external_id.clone());
                view! {
                    <li class="import-exclusion">
                        <span>{shown}</span>
                        <span class="muted mono">
                            {format!("{} · {}:{}", domain_label(&e.target_domain), e.id_kind, e.external_id)}
                        </span>
                        <Button variant="subtle" size="xs" on_click=Callback::new(move |()| {
                            let id = id.clone();
                            spawn_local(async move {
                                let _ = api::delete_import_exclusion(&id).await;
                                reload.update(|n| *n += 1);
                            });
                        })>"Remove"</Button>
                    </li>
                }
            })
            .collect_view()
    };

    view! {
        <Panel title="Exclusions">
            <p class="muted">"Items no list may add, however many lists offer them."</p>
            <ul class="import-exclusions">{rows}</ul>
            <div class="import-exclusion-form">
                <Select label="Library" option_pairs=domain_pairs value=domain
                    on_change=Callback::new(move |d: String| id_kind.set(default_id_kind(&d).into()))/>
                <Select label="Id kind" option_pairs=id_pairs value=id_kind/>
                <TextInput label="Id" value=external_id mono=true spellcheck=false/>
                <TextInput label="Title (optional)" value=title/>
                <Button variant="default" on_click=Callback::new(add)>"Exclude"</Button>
                <span class="muted">{move || msg.get()}</span>
            </div>
        </Panel>
    }
}

/// The exclusion a library item's "Exclude from lists" button posts, or `None`
/// when the item has no id a list could offer it by.
#[must_use]
pub fn exclusion_for(
    domain: &str,
    id_kind: &str,
    external_id: Option<String>,
    title: &str,
) -> Option<ImportListExclusion> {
    let id = external_id?.trim().to_string();
    (!id.is_empty()).then(|| ImportListExclusion {
        target_domain: domain.to_string(),
        id_kind: id_kind.to_string(),
        external_id: id,
        title: Some(title.to_string()).filter(|t| !t.trim().is_empty()),
        ..ImportListExclusion::default()
    })
}

/// "Exclude from lists" on a library item's page (SKADI-T-0733): no import list
/// adds the item again, even after it is deleted. Drawn only for a role the
/// API lets add an exclusion, and only when the item has an id.
#[component]
pub fn ExcludeFromListsButton(exclusion: Option<ImportListExclusion>) -> impl IntoView {
    let role = use_context::<crate::subnav::RoleCtx>().and_then(|r| r.0.get_untracked());
    let allowed = crate::surface::may_send(
        role.as_deref(),
        "POST",
        &format!("{}/importlists/exclusions", api::API_BASE),
    );
    let done = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let msg = RwSignal::new(String::new());
    exclusion.filter(|_| allowed).map(|e| {
        let on_click = move |()| {
            let e = e.clone();
            busy.set(true);
            spawn_local(async move {
                match api::add_import_exclusion(&e).await {
                    Ok(_) => {
                        done.set(true);
                        msg.set(String::new());
                    }
                    Err(err) => msg.set(err.0),
                }
                busy.set(false);
            });
        };
        view! {
            <Button variant="default" size="xs" loading=busy disabled=done
                title="No import list will add this again, even after it is deleted"
                on_click=Callback::new(on_click)>
                {move || if done.get() { "Excluded from lists" } else { "Exclude from lists" }}
            </Button>
            <span class="muted">{move || msg.get()}</span>
        }
    })
}
