//! Provider settings UI (SKADI-T-0066).
//!
//! One [`ProviderSection`] per provider entity kind (indexers / notifiers):
//! list existing entries with a `has_secret` badge, add/edit via a typed form,
//! test-connection, and delete. A section is described by a [`KindSpec`]
//! holding one or more [`VariantSpec`]s — each a concrete config `kind`
//! (`torznab`, `webhook`) with its own field table. (Downloaders are **not** a
//! `ProviderSection`: Skadi ships one zero-config built-in downloader, so its
//! page is a one-click register — see `DownloadersPage` in `main.rs`.)
//!
//! Secret fields are **write-only**: never prefilled from the server, omitted
//! from the request body when left blank (so an edit keeps the stored
//! credential), and sent only when the user types a new value.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Map, Value};

use crate::api;
use crate::confirm::{ConfirmSpec, confirm};
use crate::loading::{ListLoad, LoadError, Skeleton, SkeletonKind};

/// The NotificationKind values a webhook can subscribe to (serde snake_case in
/// skadi-notify). Kept in sync with `NotificationKind`.
const CHANNELS: &[&str] = &["grabbed", "imported", "upgraded", "failed", "health"];

/// How a single config field is entered and (de)serialized.
#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    /// Plain string (e.g. name, base_url).
    Text,
    /// Write-only secret (api_key / password / secret).
    Secret,
    /// Comma-separated list of integers (e.g. torznab categories).
    NumberList,
    /// Checkbox set over [`CHANNELS`] (webhook channels).
    ChannelSet,
}

#[derive(Clone)]
struct FieldSpec {
    /// JSON key in the config body.
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
    placeholder: &'static str,
    /// Value prefilled into a fresh Add form (so e.g. the skadi path fields are
    /// non-empty and don't override their server-side defaults with `""`).
    default_value: &'static str,
    /// Shown under the input (SKADI-T-0699). Empty = no help line.
    help: &'static str,
    /// The form refuses to save while this field is blank (SKADI-T-0699). The
    /// API refuses it too (`required_provider_fields` in skadi-api settings.rs);
    /// keep the two lists the same.
    required: bool,
}

impl FieldSpec {
    /// A plain text field with no prefill.
    const fn text(key: &'static str, label: &'static str, placeholder: &'static str) -> Self {
        FieldSpec {
            key,
            label,
            kind: FieldKind::Text,
            placeholder,
            default_value: "",
            help: "",
            required: false,
        }
    }

    /// A write-only secret field.
    const fn secret(key: &'static str, label: &'static str, placeholder: &'static str) -> Self {
        FieldSpec {
            key,
            label,
            kind: FieldKind::Secret,
            placeholder,
            default_value: "",
            help: "",
            required: false,
        }
    }

    /// The same field with a help line under its input.
    const fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }

    /// The same field, required.
    const fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// One concrete config `kind` within a section.
#[derive(Clone)]
pub(crate) struct VariantSpec {
    /// Inner config `kind` tag stored in the body ("torznab" | "skadi" |
    /// "webhook").
    config_kind: &'static str,
    /// Human label for the kind selector.
    label: &'static str,
    /// Field with this key is the one-line summary shown under the name.
    summary_key: &'static str,
    fields: Vec<FieldSpec>,
}

#[derive(Clone)]
pub(crate) struct KindSpec {
    /// Settings entity kind in the URL ("indexers" | "downloaders" | "notifiers").
    settings_kind: &'static str,
    title: &'static str,
    description: &'static str,
    /// One or more concrete config kinds. The first is the default on Add.
    variants: Vec<VariantSpec>,
    /// Whether to offer the native Cardigann "Add tracker" catalog flow
    /// (indexers only — SKADI-I-0036).
    catalog: bool,
    /// The prefix of the daemon health check of each entry
    /// (`indexer` → `indexer:<name>`), when the daemon checks this kind. Then
    /// the card shows the last test from the check, and Test runs that check
    /// (SKADI-T-0699). `None` (notifiers: a test sends a real notification, so
    /// there is no check): Test calls the settings test, and its result lasts
    /// until the page reloads.
    health_prefix: Option<&'static str>,
}

impl KindSpec {
    /// The variant for a stored `kind`, falling back to the first (default).
    fn variant(&self, kind: &str) -> &VariantSpec {
        self.variants
            .iter()
            .find(|v| v.config_kind == kind)
            .unwrap_or(&self.variants[0])
    }
}

pub(crate) fn indexer_spec() -> KindSpec {
    KindSpec {
        settings_kind: "indexers",
        title: "Indexers",
        description: "Torznab endpoints (Jackett / Prowlarr / native) used to search for releases.",
        variants: vec![VariantSpec {
            config_kind: "torznab",
            label: "Torznab",
            summary_key: "base_url",
            fields: vec![
                FieldSpec::text("name", "Name", "my-indexer")
                    .required()
                    .help("The name in lists, logs and health checks."),
                // The client adds `/api` itself (TorznabClient), so the
                // placeholder stops before it.
                FieldSpec::text("base_url", "Base URL", "http://prowlarr:9696/1")
                    .required()
                    .help("The Torznab URL of the indexer, without /api at the end. Skadi adds /api."),
                FieldSpec {
                    key: "categories",
                    label: "Categories",
                    kind: FieldKind::NumberList,
                    placeholder: "2000, 2040",
                    default_value: "",
                    help: "Newznab category numbers to search, separated by commas: 2000 is movies, 5000 is TV, 3030 is audiobooks. Empty searches all categories.",
                    required: false,
                },
                FieldSpec::secret("api_key", "API key", "leave blank to keep current")
                    .help("The API key of the indexer (in Prowlarr: Settings → General). Leave it blank to keep the stored key."),
            ],
        }],
        catalog: true,
        health_prefix: Some("indexer"),
    }
}

// The downloaders settings page is **not** a `ProviderSection`: Skadi ships a
// single built-in DB-queue downloader (the VPN-isolated worker) with no
// user-facing configuration, so its page is a one-click register
// (`DownloadersPage` in `main.rs`) rather than a kind selector + form. Since
// SKADI-T-0517 it is also the *only* downloader kind, so there is nothing for a
// selector to choose between.

pub(crate) fn notifier_spec() -> KindSpec {
    KindSpec {
        settings_kind: "notifiers",
        title: "Notifiers",
        description: "Webhook receivers POSTed a JSON envelope on the events you select.",
        variants: vec![VariantSpec {
            config_kind: "webhook",
            label: "Webhook",
            summary_key: "url",
            fields: vec![
                FieldSpec::text("name", "Name", "my-hook")
                    .required()
                    .help("The name in lists and logs."),
                FieldSpec::text("url", "URL", "https://example.com/hook")
                    .required()
                    .help("Skadi sends a JSON POST to this URL for each event that you select."),
                FieldSpec {
                    key: "channels",
                    label: "Channels",
                    kind: FieldKind::ChannelSet,
                    placeholder: "",
                    default_value: "",
                    help: "The events to send. With none selected, the webhook gets no events.",
                    required: false,
                },
                FieldSpec::secret(
                    "secret",
                    "HMAC secret",
                    "optional; leave blank to keep current",
                )
                .help("When set, each request has a signature made with this secret, so the receiver can make sure that it came from Skadi."),
            ],
        }],
        catalog: false,
        health_prefix: None,
    }
}

// The provider sections are mounted on their own routes by the shell
// (SKADI-T-0071): `/indexers`, `/downloaders`, and notifiers under system
// `/config`. `ProviderSection` + the `*_spec()` builders above are the reusable
// pieces those pages compose.

/// Editable form state: text-ish fields by key, plus the channel checkbox set.
#[derive(Clone, Default)]
struct FormState {
    fields: HashMap<String, String>,
    channels: HashSet<String>,
}

/// What the form is doing: hidden, adding a new entry, or editing an existing id.
#[derive(Clone, PartialEq)]
enum Editor {
    Closed,
    Add,
    Edit(String),
}

/// Per-entry test-connection state.
#[derive(Clone)]
enum TestState {
    Running,
    Ok,
    Failed(String),
}

fn empty_form() -> FormState {
    FormState::default()
}

/// A fresh Add form for a variant: text/secret fields seeded from their
/// `default_value` (secrets stay blank).
fn form_for_variant(variant: &VariantSpec) -> FormState {
    let mut fields = HashMap::new();
    for f in &variant.fields {
        if !f.default_value.is_empty() {
            fields.insert(f.key.to_string(), f.default_value.to_string());
        }
    }
    FormState {
        fields,
        channels: HashSet::new(),
    }
}

/// Prefill a form from a stored body (secrets are never prefilled).
fn form_from(fields: &[FieldSpec], body: &Value) -> FormState {
    let mut form_fields = HashMap::new();
    let mut channels = HashSet::new();
    for f in fields {
        match f.kind {
            FieldKind::Secret => {}
            FieldKind::Text => {
                if let Some(s) = body.get(f.key).and_then(|v| v.as_str()) {
                    form_fields.insert(f.key.to_string(), s.to_string());
                }
            }
            FieldKind::NumberList => {
                if let Some(arr) = body.get(f.key).and_then(|v| v.as_array()) {
                    let s = arr
                        .iter()
                        .filter_map(|n| n.as_u64())
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    form_fields.insert(f.key.to_string(), s);
                }
            }
            FieldKind::ChannelSet => {
                if let Some(arr) = body.get(f.key).and_then(|v| v.as_array()) {
                    for c in arr {
                        if let Some(s) = c.as_str() {
                            channels.insert(s.to_string());
                        }
                    }
                }
            }
        }
    }
    FormState {
        fields: form_fields,
        channels,
    }
}

/// Build the request body (config JSON) from the current form state, tagged with
/// the selected variant's `config_kind`. Secret fields are included only when
/// non-empty (write-only / omit-to-keep).
fn build_body(variant: &VariantSpec, form: &FormState) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "kind".into(),
        Value::String(variant.config_kind.to_string()),
    );
    for f in &variant.fields {
        match f.kind {
            FieldKind::Text => {
                let v = form.fields.get(f.key).cloned().unwrap_or_default();
                obj.insert(f.key.to_string(), Value::String(v));
            }
            FieldKind::Secret => {
                let v = form.fields.get(f.key).cloned().unwrap_or_default();
                if !v.is_empty() {
                    obj.insert(f.key.to_string(), Value::String(v));
                }
            }
            FieldKind::NumberList => {
                let raw = form.fields.get(f.key).cloned().unwrap_or_default();
                let nums: Vec<Value> = raw
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse::<u64>().ok())
                    .map(|n| Value::Number(n.into()))
                    .collect();
                obj.insert(f.key.to_string(), Value::Array(nums));
            }
            FieldKind::ChannelSet => {
                let arr: Vec<Value> = CHANNELS
                    .iter()
                    .filter(|c| form.channels.contains(**c))
                    .map(|c| Value::String((*c).to_string()))
                    .collect();
                obj.insert(f.key.to_string(), Value::Array(arr));
            }
        }
    }
    Value::Object(obj)
}

/// Per-field errors of a form: field key → message.
type FieldErrors = HashMap<String, String>;

/// The message for a required field that is blank.
fn required_message(label: &str) -> String {
    format!("{label} is required.")
}

/// Check `form` before it is saved (SKADI-T-0699): every required text field
/// has a value that is not only spaces, and each item of a number list is a
/// number. Empty = the form can be saved. Secrets are never required here: an
/// edit leaves a blank secret as it is. Pure.
fn validate_form(variant: &VariantSpec, form: &FormState) -> FieldErrors {
    let mut errors = FieldErrors::new();
    for f in &variant.fields {
        let raw = form.fields.get(f.key).map(String::as_str).unwrap_or("");
        match f.kind {
            FieldKind::Text if f.required && raw.trim().is_empty() => {
                errors.insert(f.key.to_string(), required_message(f.label));
            }
            FieldKind::NumberList => {
                let bad: Vec<&str> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.parse::<u64>().is_err())
                    .collect();
                if !bad.is_empty() {
                    errors.insert(
                        f.key.to_string(),
                        format!(
                            "Not a number: {}. Use numbers separated by commas.",
                            bad.join(", ")
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    errors
}

/// The health check id of a provider entry: `{prefix}:{name}`
/// (`indexer:Knaben`), as the daemon names it.
fn check_id(prefix: &str, name: &str) -> String {
    format!("{prefix}:{name}")
}

/// The "last tested" line of a provider card, from its health check
/// (SKADI-T-0699). `age_secs` is the age of `checked_at`, `None` when it does
/// not parse. Returns the CSS class and the text; `None` when the daemon has
/// no check for the entry (an older daemon, or the list and the checks were
/// read at different times). Pure.
fn last_test_line(
    check: Option<&api::HealthCheck>,
    age_secs: Option<f64>,
) -> Option<(&'static str, String)> {
    let check = check?;
    let level = check.level();
    if level == "pending" || check.checked_at.is_none() {
        return Some(("pending", "Not tested yet.".into()));
    }
    let when = match age_secs.map(|s| crate::downloads::age_label(Some(s))) {
        Some(a) if a == "now" => "Last tested just now".to_string(),
        Some(a) if a != "—" => format!("Last tested {a} ago"),
        _ => format!(
            "Last tested {}",
            crate::system::time_label(check.checked_at.as_deref())
        ),
    };
    let verdict = match level {
        "ok" => "ok",
        "warn" => "warning",
        _ => "failed",
    };
    let text = if check.detail.is_empty() {
        format!("{when}: {verdict}.")
    } else {
        format!("{when}: {verdict} — {}", check.detail)
    };
    Some((crate::dashboard::severity_class(level), text))
}

/// Seconds since an RFC 3339 time, by the browser clock. `None` when it does
/// not parse.
fn age_secs(t: Option<&str>) -> Option<f64> {
    let ms = js_sys::Date::parse(t?);
    (!ms.is_nan()).then(|| (js_sys::Date::now() - ms) / 1000.0)
}

#[component]
pub(crate) fn ProviderSection(spec: KindSpec) -> impl IntoView {
    let spec = Arc::new(spec);
    let kind = spec.settings_kind;
    let default_kind = spec.variants[0].config_kind;

    let items = RwSignal::new(Vec::<api::Setting>::new());
    // First load, failure and Retry (SKADI-T-0698).
    let load = ListLoad::new();
    let editor = RwSignal::new(Editor::Closed);
    let form = RwSignal::new(empty_form());
    // The selected config `kind` (which variant's fields are shown). Separate
    // from `form` so typing in a field doesn't re-render the whole field set.
    let selected = RwSignal::new(default_kind.to_string());
    let form_error = RwSignal::new(None::<String>);
    // Errors next to their inputs (SKADI-T-0699).
    let field_errors = RwSignal::new(FieldErrors::new());
    let busy = RwSignal::new(false);
    let tests = RwSignal::new(HashMap::<String, TestState>::new());
    // The daemon's health check per provider, by check id, so a failing
    // provider shows at a glance (SKADI-T-0164) and each card says when it was
    // last tested and how that went, also after a reload (SKADI-T-0699). Checks
    // are named `{prefix}:{provider name}`, e.g. `indexer:Knaben (Prowlarr)`.
    let checks = RwSignal::new(HashMap::<String, api::HealthCheck>::new());
    let health_prefix = spec.health_prefix;

    // (Re)load the list for this kind.
    let refresh = move || {
        spawn_local(async move {
            if let Some(list) = load.settle(api::list_settings(kind).await) {
                let _ = items.try_set(list);
            }
        });
    };
    // The stored checks. Reads only: `GET /health/checks` runs nothing.
    let reload_checks = move || {
        if health_prefix.is_some() {
            spawn_local(async move {
                if let Ok(list) = api::health_checks().await {
                    set_checks(checks, list);
                }
            });
        }
    };
    // Initial load + health.
    Effect::new(move |_| {
        refresh();
        reload_checks();
    });

    let open_add = {
        let spec = spec.clone();
        move |_| {
            selected.set(default_kind.to_string());
            form.set(form_for_variant(&spec.variants[0]));
            form_error.set(None);
            field_errors.set(FieldErrors::new());
            editor.set(Editor::Add);
        }
    };

    let cancel = move |_| {
        editor.set(Editor::Closed);
        form_error.set(None);
        field_errors.set(FieldErrors::new());
    };

    // Submit create or update depending on the editor mode.
    let submit = {
        let spec = spec.clone();
        move |_| {
            let spec = spec.clone();
            let variant = spec.variant(&selected.get_untracked()).clone();
            // Refused in the form, with the message next to the field; the
            // API still checks the same fields (SKADI-T-0699).
            let errors = form.with_untracked(|f| validate_form(&variant, f));
            if !errors.is_empty() {
                field_errors.set(errors);
                form_error.set(None);
                return;
            }
            field_errors.set(FieldErrors::new());
            let body = build_body(&variant, &form.get_untracked());
            let mode = editor.get_untracked();
            busy.set(true);
            form_error.set(None);
            spawn_local(async move {
                let result = match &mode {
                    Editor::Edit(id) => api::update_setting(spec.settings_kind, id, &body).await,
                    _ => api::create_setting(spec.settings_kind, &body).await,
                };
                busy.set(false);
                match result {
                    Ok(_) => {
                        editor.set(Editor::Closed);
                        refresh();
                        // A new or renamed entry has a new check id.
                        reload_checks();
                    }
                    Err(e) => form_error.set(Some(e.to_string())),
                }
            });
        }
    };

    let spec_for_rows = spec.clone();
    let rows = move || {
        let spec = spec_for_rows.clone();
        let by_id = checks.get();
        items
            .get()
            .into_iter()
            .map(|item| {
                let spec = spec.clone();
                // Match this row to its health check by `{prefix}:{name}`.
                let name = item
                    .body
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let check = health_prefix.and_then(|p| by_id.get(&check_id(p, name)).cloned());
                row_view(
                    spec,
                    item,
                    check,
                    RowSignals {
                        editor,
                        form,
                        field_errors,
                        selected,
                        tests,
                        checks,
                        busy,
                    },
                    refresh,
                )
            })
            .collect_view()
    };

    let spec_for_form = spec.clone();
    let form_panel = move || {
        let mode = editor.get();
        if mode == Editor::Closed {
            return ().into_any();
        }
        // The kind selector is only meaningful when adding (the kind is fixed
        // once a row exists) and the section has more than one variant.
        let show_selector = mode == Editor::Add && spec_for_form.variants.len() > 1;
        form_view(
            spec_for_form.clone(),
            show_selector,
            selected,
            form,
            form_error,
            field_errors,
            busy,
            submit.clone(),
            cancel,
        )
    };

    let show_catalog = spec.catalog;
    let catalog_panel = show_catalog.then(|| {
        let on_added = Callback::new(move |()| refresh());
        view! { <CardigannAddPanel on_added=on_added/> }
    });

    view! {
        <section class="provider-section">
            <div class="section-head">
                <div>
                    <h3>{spec.title}</h3>
                    <p class="muted">{spec.description}</p>
                </div>
                <button on:click=open_add>"+ Add"</button>
            </div>
            {catalog_panel}
            {load.status(SkeletonKind::Cards, Callback::new(move |()| refresh()))}
            <div class="cards">{rows}</div>
            {form_panel}
        </section>
    }
}

/// The native-Cardigann "Add tracker" flow: browse the synced definition catalog,
/// pick a tracker, fill its definition-driven settings, and create a `cardigann`
/// indexer row (SKADI-T-0261). `on_added` reloads the parent list on success.
#[component]
fn CardigannAddPanel(on_added: Callback<()>) -> impl IntoView {
    let open = RwSignal::new(false);
    let defs = RwSignal::new(Vec::<api::CatalogDef>::new());
    let loading = RwSignal::new(false);
    let load_err = RwSignal::new(None::<String>);
    let filter = RwSignal::new(String::new());
    let selected = RwSignal::new(None::<api::CatalogDef>);
    let form = RwSignal::new(HashMap::<String, String>::new());
    let busy = RwSignal::new(false);
    let form_err = RwSignal::new(None::<String>);
    // The Name error, next to the Name input (SKADI-T-0699).
    let name_err = RwSignal::new(None::<String>);
    let refreshing = RwSignal::new(false);

    let load = move || {
        loading.set(true);
        spawn_local(async move {
            match api::list_definitions().await {
                Ok(list) => {
                    defs.set(list);
                    load_err.set(None);
                }
                Err(e) => load_err.set(Some(e.to_string())),
            }
            loading.set(false);
        });
    };

    let open_panel = move |_| {
        open.set(true);
        selected.set(None);
        if defs.get_untracked().is_empty() {
            load();
        }
    };
    let close = move |_| {
        open.set(false);
        selected.set(None);
        form_err.set(None);
    };

    // Pick a definition → seed the form (name + each setting's default).
    let pick = move |d: api::CatalogDef| {
        let mut f = HashMap::new();
        f.insert("name".to_string(), d.name.clone());
        for s in &d.settings {
            if let Some(def) = &s.default {
                f.insert(s.name.clone(), def.clone());
            }
        }
        form.set(f);
        form_err.set(None);
        name_err.set(None);
        selected.set(Some(d));
    };
    let back = move |_| selected.set(None);

    let save = move |_| {
        let Some(def) = selected.get_untracked() else {
            return;
        };
        let f = form.get_untracked();
        let name = f.get("name").cloned().unwrap_or_default();
        if name.trim().is_empty() {
            name_err.set(Some(required_message("Name")));
            return;
        }
        name_err.set(None);
        // Non-secret settings go in the body; secret (password) settings ride in
        // `api_key` as a JSON blob — the settings API strips + seals that field
        // into the credential store, and `build_cardigann` merges it back for login.
        let mut settings = Map::new();
        let mut secrets = Map::new();
        for s in &def.settings {
            if let Some(v) = f.get(&s.name).filter(|v| !v.is_empty()) {
                if s.kind == "password" {
                    secrets.insert(s.name.clone(), Value::String(v.clone()));
                } else {
                    settings.insert(s.name.clone(), Value::String(v.clone()));
                }
            }
        }
        let mut body = serde_json::json!({
            "kind": "cardigann",
            "name": name,
            "definition_id": def.id,
            "settings": Value::Object(settings),
        });
        if !secrets.is_empty() {
            body["api_key"] =
                Value::String(serde_json::to_string(&Value::Object(secrets)).unwrap_or_default());
        }
        busy.set(true);
        form_err.set(None);
        spawn_local(async move {
            match api::create_setting("indexers", &body).await {
                Ok(_) => {
                    busy.set(false);
                    open.set(false);
                    selected.set(None);
                    on_added.run(());
                }
                Err(e) => {
                    busy.set(false);
                    form_err.set(Some(e.to_string()));
                }
            }
        });
    };

    let refresh_catalog = move |_| {
        refreshing.set(true);
        spawn_local(async move {
            let _ = api::refresh_definitions().await;
            match api::list_definitions().await {
                Ok(l) => defs.set(l),
                Err(e) => load_err.set(Some(e.to_string())),
            }
            refreshing.set(false);
        });
    };

    // The filtered, clickable definition list.
    let def_list = move || {
        let q = filter.get().to_lowercase();
        let list = defs.get();
        list.into_iter()
            .filter(|d| {
                q.is_empty()
                    || d.name.to_lowercase().contains(&q)
                    || d.id.to_lowercase().contains(&q)
            })
            .take(60)
            .map(|d| {
                let d_click = d.clone();
                let privacy = d.privacy.clone();
                let cats = d.categories.len();
                view! {
                    <button class="catalog-item" on:click=move |_| pick(d_click.clone())>
                        <span class="catalog-name">{d.name.clone()}</span>
                        <span class=format!("pill privacy-{privacy}")>{d.privacy.clone()}</span>
                        <span class="catalog-meta mono">{format!("{cats} cats")}</span>
                    </button>
                }
            })
            .collect_view()
    };

    // The definition-driven settings form.
    let settings_form = move || {
        let Some(def) = selected.get() else {
            return ().into_any();
        };
        let fields = def
            .settings
            .iter()
            .cloned()
            .map(|s| catalog_field_view(s, form))
            .collect_view();
        let title = def.name.clone();
        let needs_login = def.needs_login;
        view! {
            <div class="form">
                <div class="catalog-form-head">
                    <button class="btn-link" on:click=back>"‹ back"</button>
                    <strong>{title}</strong>
                    {needs_login.then(|| view! { <span class="pill privacy-private">"login required"</span> })}
                </div>
                <div class="field">
                    <label for="catalog-field-name">
                        "Name"
                        <span class="field-req" title="Required">" *"</span>
                    </label>
                    <input
                        id="catalog-field-name"
                        type="text"
                        required=true
                        aria-required="true"
                        aria-invalid=move || name_err.with(Option::is_some).to_string()
                        prop:value=move || form.with(|f| f.get("name").cloned().unwrap_or_default())
                        on:input=move |ev| {
                            let v = event_target_value(&ev);
                            form.update(|f| { f.insert("name".into(), v); });
                            name_err.set(None);
                        }
                    />
                    <p class="field-help muted">"The name in lists, logs and health checks."</p>
                    {move || name_err.get().map(|e| view! { <p class="field-error bad" role="alert">{e}</p> })}
                </div>
                {fields}
                {move || form_err.get().map(|e| view! { <p class="bad">{e}</p> })}
                <div class="form-actions">
                    <button on:click=save disabled=move || busy.get()>
                        {move || if busy.get() { "Adding…" } else { "Add tracker" }}
                    </button>
                    <button class="secondary" on:click=close>"Cancel"</button>
                </div>
            </div>
        }
        .into_any()
    };

    let body = move || {
        if !open.get() {
            return ().into_any();
        }
        if selected.get().is_some() {
            return settings_form().into_any();
        }
        view! {
            <div class="catalog-panel">
                <div class="catalog-toolbar">
                    <input
                        class="catalog-filter"
                        type="text"
                        placeholder="Filter trackers…"
                        prop:value=move || filter.get()
                        on:input=move |ev| filter.set(event_target_value(&ev))
                    />
                    <button class="secondary" on:click=refresh_catalog disabled=move || refreshing.get()>
                        {move || if refreshing.get() { "Syncing…" } else { "Sync catalog" }}
                    </button>
                    <button class="btn-link" on:click=close>"Close"</button>
                </div>
                {move || load_err.get().map(|e| {
                    let retry = Callback::new(move |()| {
                        load_err.set(None);
                        load();
                    });
                    view! { <LoadError message=e retry=retry/> }
                })}
                {move || (loading.get() && defs.with(Vec::is_empty))
                    .then(|| view! { <Skeleton kind=SkeletonKind::Rows/> })}
                <div class="catalog-list">{def_list}</div>
            </div>
        }
        .into_any()
    };

    view! {
        <div class="catalog-add">
            <button class="catalog-add-btn" on:click=open_panel>"+ Add tracker (native)"</button>
            {body}
        </div>
    }
}

/// One definition-setting input (checkbox or text) for the catalog add form.
fn catalog_field_view(s: api::CatalogSetting, form: RwSignal<HashMap<String, String>>) -> AnyView {
    let key = s.name.clone();
    let help = s
        .help
        .clone()
        .filter(|h| !h.trim().is_empty())
        .map(|h| view! { <p class="field-help muted">{h}</p> });
    let label = if s.label.is_empty() {
        s.name.clone()
    } else {
        s.label.clone()
    };
    if s.kind == "checkbox" {
        let key_checked = key.clone();
        let key_toggle = key.clone();
        view! {
            <label class="check">
                <input
                    type="checkbox"
                    prop:checked=move || form.with(|f| f.get(&key_checked).map(|v| v == "true").unwrap_or(false))
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        form.update(|f| { f.insert(key_toggle.clone(), on.to_string()); });
                    }
                />
                {label}
            </label>
            {help}
        }
        .into_any()
    } else {
        let key_val = key.clone();
        let key_in = key.clone();
        let is_secret = s.kind == "password";
        view! {
            <div class="field">
                <label>{label}</label>
                <input
                    type=if is_secret { "password" } else { "text" }
                    prop:value=move || form.with(|f| f.get(&key_val).cloned().unwrap_or_default())
                    on:input=move |ev| {
                        let v = event_target_value(&ev);
                        form.update(|f| { f.insert(key_in.clone(), v); });
                    }
                />
                {help}
            </div>
        }
        .into_any()
    }
}

/// Replace the stored checks with `list` (a whole `GET /health/checks` or
/// `POST /health/checks/run` answer), keyed by check id.
fn set_checks(checks: RwSignal<HashMap<String, api::HealthCheck>>, list: Vec<api::HealthCheck>) {
    let _ = checks.try_set(list.into_iter().map(|c| (c.name.clone(), c)).collect());
}

/// The section's signals that a card reads or writes.
#[derive(Clone, Copy)]
struct RowSignals {
    editor: RwSignal<Editor>,
    form: RwSignal<FormState>,
    field_errors: RwSignal<FieldErrors>,
    selected: RwSignal<String>,
    tests: RwSignal<HashMap<String, TestState>>,
    checks: RwSignal<HashMap<String, api::HealthCheck>>,
    busy: RwSignal<bool>,
}

/// One list card for a stored entry. `check` is its daemon health check, when
/// the section has them.
fn row_view(
    spec: Arc<KindSpec>,
    item: api::Setting,
    check: Option<api::HealthCheck>,
    sig: RowSignals,
    refresh: impl Fn() + Copy + 'static,
) -> AnyView {
    let RowSignals {
        editor,
        form,
        field_errors,
        selected,
        tests,
        checks,
        busy,
    } = sig;
    let id = item.id.clone();
    // Resolve the variant from the row's stored `kind`.
    let row_kind = item
        .body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or(spec.variants[0].config_kind)
        .to_string();
    let variant = spec.variant(&row_kind).clone();
    // Native cardigann rows aren't a static variant — they render from the stored
    // `definition_id` and are edited via the catalog flow (so no inline Edit).
    let is_cardigann = row_kind == "cardigann";
    let label = if is_cardigann {
        "Cardigann"
    } else {
        variant.label
    };
    let name = item
        .body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("(unnamed)")
        .to_string();
    let summary_key = if is_cardigann {
        "definition_id"
    } else {
        variant.summary_key
    };
    let summary = item
        .body
        .get(summary_key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let has_secret = item.has_secret.unwrap_or(false);

    let edit_id = id.clone();
    let edit_body = item.body.clone();
    let edit_variant = variant.clone();
    let edit_kind = row_kind.clone();
    let on_edit = move |_| {
        selected.set(edit_kind.clone());
        form.set(form_from(&edit_variant.fields, &edit_body));
        field_errors.set(FieldErrors::new());
        editor.set(Editor::Edit(edit_id.clone()));
    };

    let del_id = id.clone();
    let del_kind = spec.settings_kind;
    let del_name = name.clone();
    let on_delete = move |_| {
        let title = format!("Delete \"{del_name}\"?");
        let del_id = del_id.clone();
        spawn_local(async move {
            if !confirm(ConfirmSpec::destructive(title, "This can't be undone.")).await {
                return;
            }
            busy.set(true);
            let _ = api::delete_setting(del_kind, &del_id).await;
            busy.set(false);
            refresh();
        });
    };

    // Test: where the daemon checks this kind, run that one check, so the card
    // and the health check never disagree and the result outlives a reload
    // (SKADI-T-0699). Else the settings test, whose result lasts until reload.
    let test_id = id.clone();
    let test_kind = spec.settings_kind;
    let test_check = spec.health_prefix.map(|p| check_id(p, &name));
    let on_test = move |_| {
        let test_id = test_id.clone();
        let test_check = test_check.clone();
        tests.update(|m| {
            m.insert(test_id.clone(), TestState::Running);
        });
        spawn_local(async move {
            let state = match &test_check {
                Some(cid) => match api::run_health_check(cid).await {
                    Ok(list) => {
                        set_checks(checks, list);
                        None
                    }
                    Err(e) => Some(TestState::Failed(e.to_string())),
                },
                None => Some(match api::test_setting(test_kind, &test_id).await {
                    Ok(r) if r.ok => TestState::Ok,
                    Ok(r) => TestState::Failed(r.error.unwrap_or_else(|| "failed".into())),
                    Err(e) => TestState::Failed(e.to_string()),
                }),
            };
            let _ = tests.try_update(|m| match state {
                Some(st) => {
                    m.insert(test_id.clone(), st);
                }
                None => {
                    m.remove(&test_id);
                }
            });
        });
    };

    let test_id_view = id.clone();
    let last_check = check.clone();
    let test_view = move || {
        tests.with(|m| match m.get(&test_id_view) {
            Some(TestState::Running) => {
                view! { <span class="pending">"testing…"</span> }.into_any()
            }
            Some(TestState::Ok) => view! { <span class="ok">"✓ connection ok"</span> }.into_any(),
            Some(TestState::Failed(e)) => {
                let e = e.clone();
                view! { <span class="bad">"✗ " {e}</span> }.into_any()
            }
            None => match last_test_line(
                last_check.as_ref(),
                age_secs(last_check.as_ref().and_then(|c| c.checked_at.as_deref())),
            ) {
                Some((cls, text)) => {
                    let at = crate::system::time_label(
                        last_check.as_ref().and_then(|c| c.checked_at.as_deref()),
                    );
                    view! { <span class=format!("last-test {cls}") title=at>{text}</span> }
                        .into_any()
                }
                None => ().into_any(),
            },
        })
    };

    // Persistent health dot (from the daemon's health checks), with the raw status
    // as a tooltip. Absent when there's no matching check (e.g. a notifier).
    let health_dot = check.map(|c| {
        let level = c.level().to_string();
        let cls = crate::dashboard::severity_class(&level);
        view! { <span class=format!("health-dot {cls}") title=level></span> }
    });

    view! {
        <div class="provider-card">
            <div class="provider-row">
                <div class="provider-main">
                    {health_dot}
                    <strong class="provider-name">{name.clone()}</strong>
                    <span class="protocol-badge">{label}</span>
                    {has_secret
                        .then(|| view! { <span class="provider-flag mono">"secret stored"</span> })}
                    // Inline, not a second line: nineteen two-line cards were a
                    // page of scrolling for one glance's worth of information
                    // (SKADI-T-0580).
                    {(!summary.is_empty())
                        .then(|| view! { <span class="provider-sub mono">{summary}</span> })}
                </div>
                <div class="provider-actions">
                    // Each card has the same three buttons: the label names the
                    // card too (SKADI-T-0700).
                    <button on:click=on_test aria-label=format!("Test {name}")>"Test"</button>
                    {(!is_cardigann).then(|| {
                        let edit_label = format!("Edit {name}");
                        view! { <button on:click=on_edit aria-label=edit_label>"Edit"</button> }
                    })}
                    <button class="danger" on:click=on_delete aria-label=format!("Delete {name}")>"Delete"</button>
                </div>
            </div>
            <div class="card-test">{test_view}</div>
        </div>
    }
    .into_any()
}

/// The add/edit form panel.
#[allow(clippy::too_many_arguments)]
fn form_view(
    spec: Arc<KindSpec>,
    show_selector: bool,
    selected: RwSignal<String>,
    form: RwSignal<FormState>,
    form_error: RwSignal<Option<String>>,
    field_errors: RwSignal<FieldErrors>,
    busy: RwSignal<bool>,
    submit: impl Fn(()) + Clone + 'static,
    cancel: impl Fn(leptos::ev::MouseEvent) + 'static,
) -> AnyView {
    // The kind selector (Add + multi-variant only): switching kind reseeds the
    // form with that variant's defaults.
    let selector = if show_selector {
        let spec_for_change = spec.clone();
        let on_change = move |ev: leptos::ev::Event| {
            let k = event_target_value(&ev);
            selected.set(k.clone());
            form.set(form_for_variant(spec_for_change.variant(&k)));
            field_errors.set(FieldErrors::new());
        };
        let options = spec
            .variants
            .iter()
            .map(|v| {
                let (ck, label) = (v.config_kind, v.label);
                view! { <option value=ck>{label}</option> }
            })
            .collect_view();
        let current = move || selected.get();
        view! {
            <div class="field">
                <label>"Kind"</label>
                <select prop:value=current on:change=on_change>{options}</select>
            </div>
        }
        .into_any()
    } else {
        ().into_any()
    };

    // The field set depends only on the selected kind, so it re-renders on a
    // kind change but not on every keystroke (those touch `form`, not
    // `selected`).
    let spec_for_fields = spec.clone();
    let inputs = move || {
        let variant = spec_for_fields.variant(&selected.get()).clone();
        variant
            .fields
            .into_iter()
            .map(|f| field_view(f, form, field_errors))
            .collect_view()
    };

    let submit_click = move |_| submit(());

    view! {
        <div class="form">
            {selector}
            {inputs}
            {move || form_error.get().map(|e| view! { <p class="bad">{e}</p> })}
            <div class="form-actions">
                <button on:click=submit_click disabled=move || busy.get()>
                    {move || if busy.get() { "Saving…" } else { "Save" }}
                </button>
                <button class="secondary" on:click=cancel>"Cancel"</button>
            </div>
        </div>
    }
    .into_any()
}

/// The label of a field, with a `*` when it is required.
fn field_label(label: &'static str, required: bool, for_id: Option<String>) -> AnyView {
    view! {
        <label for=for_id>
            {label}
            {required.then(|| view! {
                <span class="field-req" title="Required">" *"</span>
            })}
        </label>
    }
    .into_any()
}

/// The help line under an input, if the field has one (SKADI-T-0699).
fn help_line(help: &str, id: String) -> Option<AnyView> {
    (!help.is_empty()).then(|| {
        let help = help.to_string();
        view! { <p class="field-help muted" id=id>{help}</p> }.into_any()
    })
}

/// The error of the field `key`, next to its input.
fn error_line(key: &'static str, field_errors: RwSignal<FieldErrors>) -> impl IntoView {
    move || {
        field_errors.with(|e| e.get(key).cloned()).map(|msg| {
            view! { <p class="field-error bad" role="alert">{msg}</p> }
        })
    }
}

/// One labelled input, dispatched on the field kind, with its help line and
/// its error.
fn field_view(
    f: FieldSpec,
    form: RwSignal<FormState>,
    field_errors: RwSignal<FieldErrors>,
) -> AnyView {
    let key = f.key;
    let input_id = format!("provider-field-{key}");
    let help_id = format!("{input_id}-help");
    let help = help_line(f.help, help_id.clone());
    let described_by = help.is_some().then_some(help_id);
    match f.kind {
        FieldKind::ChannelSet => {
            let boxes = CHANNELS
                .iter()
                .map(|c| {
                    let c = c.to_string();
                    let c_for_checked = c.clone();
                    let checked = move || form.with(|st| st.channels.contains(&c_for_checked));
                    let c_for_toggle = c.clone();
                    let toggle = move |_| {
                        form.update(|st| {
                            if st.channels.contains(&c_for_toggle) {
                                st.channels.remove(&c_for_toggle);
                            } else {
                                st.channels.insert(c_for_toggle.clone());
                            }
                        });
                    };
                    view! {
                        <label class="check">
                            <input type="checkbox" prop:checked=checked on:change=toggle/>
                            {c}
                        </label>
                    }
                })
                .collect_view();
            view! {
                <div class="field">
                    {field_label(f.label, f.required, None)}
                    <div class="checks" aria-describedby=described_by>{boxes}</div>
                    {help}
                    {error_line(key, field_errors)}
                </div>
            }
            .into_any()
        }
        kind => {
            let input_type = if kind == FieldKind::Secret {
                "password"
            } else {
                "text"
            };
            let value = move || form.with(|st| st.fields.get(key).cloned().unwrap_or_default());
            let on_input = move |ev| {
                let v = event_target_value(&ev);
                form.update(|st| {
                    st.fields.insert(key.to_string(), v);
                });
                // The message goes once the field is edited; Save checks again.
                if field_errors.with_untracked(|e| e.contains_key(key)) {
                    field_errors.update(|e| {
                        e.remove(key);
                    });
                }
            };
            let invalid = move || field_errors.with(|e| e.contains_key(key)).to_string();
            view! {
                <div class="field">
                    {field_label(f.label, f.required, Some(input_id.clone()))}
                    <input
                        id=input_id
                        type=input_type
                        placeholder=f.placeholder
                        required=f.required
                        aria-required=f.required.to_string()
                        aria-invalid=invalid
                        aria-describedby=described_by
                        prop:value=value
                        on:input=on_input
                    />
                    {help}
                    {error_line(key, field_errors)}
                </div>
            }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(&str, &str)]) -> FormState {
        FormState {
            fields: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            channels: HashSet::new(),
        }
    }

    fn torznab() -> VariantSpec {
        indexer_spec().variants[0].clone()
    }

    #[test]
    fn a_blank_required_field_is_refused_with_a_message_for_that_field() {
        let errors = validate_form(&torznab(), &form(&[("name", "  "), ("base_url", "")]));
        assert_eq!(
            errors.get("name").map(String::as_str),
            Some("Name is required.")
        );
        assert_eq!(
            errors.get("base_url").map(String::as_str),
            Some("Base URL is required.")
        );
        assert!(
            !errors.contains_key("api_key"),
            "a secret is never required"
        );
        assert!(!errors.contains_key("categories"), "optional");
    }

    #[test]
    fn a_filled_form_has_no_errors_and_a_blank_secret_is_fine() {
        let ok = form(&[
            ("name", "x"),
            ("base_url", "http://p:9696/1"),
            ("categories", "2000, 5000"),
        ]);
        assert!(validate_form(&torznab(), &ok).is_empty());
    }

    #[test]
    fn a_category_that_is_not_a_number_is_named() {
        let f = form(&[
            ("name", "x"),
            ("base_url", "u"),
            ("categories", "2000, movies, 50x0"),
        ]);
        let errors = validate_form(&torznab(), &f);
        let msg = errors.get("categories").expect("categories error");
        assert!(msg.contains("movies, 50x0"), "{msg}");
    }

    #[test]
    fn the_required_fields_match_the_api_list() {
        // skadi-api settings.rs `required_provider_fields`: torznab
        // name + base_url, webhook name + url.
        let required = |spec: KindSpec| -> Vec<&'static str> {
            spec.variants[0]
                .fields
                .iter()
                .filter(|f| f.required)
                .map(|f| f.key)
                .collect()
        };
        assert_eq!(required(indexer_spec()), ["name", "base_url"]);
        assert_eq!(required(notifier_spec()), ["name", "url"]);
    }

    #[test]
    fn the_fields_that_most_need_it_have_help() {
        for spec in [indexer_spec(), notifier_spec()] {
            for f in &spec.variants[0].fields {
                assert!(
                    !f.help.is_empty(),
                    "{} {} has no help",
                    spec.settings_kind,
                    f.key
                );
            }
        }
    }

    #[test]
    fn only_the_indexers_read_a_health_check() {
        assert_eq!(indexer_spec().health_prefix, Some("indexer"));
        assert_eq!(notifier_spec().health_prefix, None);
        assert_eq!(
            check_id("indexer", "Knaben (Prowlarr)"),
            "indexer:Knaben (Prowlarr)"
        );
    }

    fn check(severity: &str, detail: &str, at: Option<&str>) -> api::HealthCheck {
        api::HealthCheck {
            name: "indexer:x".into(),
            status: "ok".into(),
            detail: detail.into(),
            severity: Some(severity.into()),
            checked_at: at.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_card_says_when_it_was_last_tested_and_how_it_went() {
        let at = Some("2026-10-07T12:00:00Z");
        assert_eq!(
            last_test_line(Some(&check("ok", "reachable", at)), Some(300.0)),
            Some(("ok", "Last tested 5m ago: ok — reachable".into()))
        );
        assert_eq!(
            last_test_line(
                Some(&check("error", "connection refused", at)),
                Some(7_200.0)
            ),
            Some((
                "bad",
                "Last tested 2h ago: failed — connection refused".into()
            ))
        );
        assert_eq!(
            last_test_line(Some(&check("warn", "slow", at)), Some(10.0)),
            Some(("warn", "Last tested just now: warning — slow".into()))
        );
        // A time the browser cannot read: the time itself.
        assert_eq!(
            last_test_line(Some(&check("ok", "", at)), None),
            Some(("ok", "Last tested 2026-10-07 12:00:00: ok.".into()))
        );
    }

    #[test]
    fn a_check_that_never_ran_says_so_and_no_check_shows_nothing() {
        assert_eq!(
            last_test_line(Some(&check("pending", "not run yet", None)), None),
            Some(("pending", "Not tested yet.".into()))
        );
        assert_eq!(last_test_line(None, None), None);
    }
}
