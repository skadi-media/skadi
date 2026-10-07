//! Web UI component/DOM tests (SKADI-T-0119) — mount the presentational Leptos
//! components into a detached node in a headless browser and assert on the
//! rendered DOM. Offline by design: these components take data via props +
//! callbacks (no `fetch` on mount), so the assertions are deterministic.
//! Run with `angreal test web`.

use leptos::mount::mount_to;
use leptos::prelude::*;
use leptos_router::components::Router;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;
use web_sys::{Document, HtmlButtonElement, HtmlElement};

use skadi_web::api::{HealthCheck, ReleaseCandidate};
use skadi_web::audiobooks::{AudiobooksConfigPage, BookFileActions};
use skadi_web::dashboard::{Dashboard, HealthChecks};
use skadi_web::movies::{EditionActions, ReleasesTable};

wasm_bindgen_test_configure!(run_in_browser);

fn document() -> Document {
    web_sys::window().unwrap().document().unwrap()
}

/// A fresh detached host element appended to `<body>` for one test's mount.
fn host() -> HtmlElement {
    let el = document()
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    document().body().unwrap().append_child(&el).unwrap();
    el
}

/// Count elements matching `selector` within `host`.
fn count(host: &HtmlElement, selector: &str) -> u32 {
    host.query_selector_all(selector).unwrap().length()
}

/// Buttons under `host` whose text matches `label`.
fn buttons_with_text(host: &HtmlElement, label: &str) -> usize {
    let list = host.query_selector_all("button").unwrap();
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter(|n| n.text_content().as_deref() == Some(label))
        .count()
}

fn candidate(title: &str, accepted: bool, reason: &str, key: &str) -> ReleaseCandidate {
    ReleaseCandidate {
        relevance: 0.0,
        season_pack: false,
        release: json!({ "title": title, "size": 1_073_741_824u64, "seeders": 10 }),
        release_key: key.into(),
        quality: if accepted {
            "Bluray-1080p".into()
        } else {
            String::new()
        },
        age_days: 2,
        accepted,
        reason: reason.into(),
    }
}

#[wasm_bindgen_test]
fn releases_table_renders_rows_grab_and_unblock() {
    let candidates = vec![
        candidate("The.Matrix.1080p", true, "Bluray-1080p", "k1"),
        candidate("The.Matrix.720p", false, "blocklisted", "k2"),
    ];
    let host = host();
    let _handle = mount_to(host.clone(), move || {
        view! {
            <ReleasesTable
                candidates=candidates.clone()
                on_grab=Callback::new(|_| {})
                on_block=Callback::new(|_| {})
                on_unblock=Callback::new(|_| {})
            />
        }
    });

    // Two data rows.
    assert_eq!(count(&host, "tbody tr"), 2, "two candidate rows");

    // Accepted row → Grab + Block; blocklisted row → Unblock (exactly one each).
    assert_eq!(buttons_with_text(&host, "Grab"), 1);
    assert_eq!(buttons_with_text(&host, "Block"), 1);
    assert_eq!(buttons_with_text(&host, "Unblock"), 1);

    // Display fields render: title + human size.
    let text = host.text_content().unwrap();
    assert!(text.contains("The.Matrix.1080p"), "title rendered: {text}");
    assert!(text.contains("1.0 GB"), "human size rendered: {text}");
    // The blocklisted row shows the reason as its verdict.
    assert!(text.contains("blocklisted"), "blocklisted verdict shown");
}

#[wasm_bindgen_test]
fn health_checks_render_status_dots() {
    let checks = vec![
        HealthCheck {
            name: "daemon".into(),
            status: "ok".into(),
            detail: "skadi 0.0.1".into(),
            severity: Some("ok".into()),
        },
        HealthCheck {
            name: "indexer:dead".into(),
            status: "fail".into(),
            detail: "connection refused".into(),
            severity: Some("error".into()),
        },
        HealthCheck {
            name: "disk-space".into(),
            status: "warn".into(),
            detail: "80 % used".into(),
            severity: Some("warn".into()),
        },
        HealthCheck {
            name: "domain:movies".into(),
            status: "warn".into(),
            detail: "not checked yet".into(),
            severity: Some("pending".into()),
        },
    ];
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <HealthChecks checks=checks.clone()/> },
    );

    assert_eq!(count(&host, ".metric"), 4, "one badge per check");
    // Each severity has its own dot: a warning is not shown as pending.
    assert_eq!(count(&host, ".status-dot.ok"), 1);
    assert_eq!(count(&host, ".status-dot.bad"), 1);
    assert_eq!(count(&host, ".status-dot.warn"), 1);
    assert_eq!(count(&host, ".status-dot.pending"), 1);
    assert!(host.text_content().unwrap().contains("connection refused"));
}

#[wasm_bindgen_test]
fn health_checks_empty_shows_placeholder() {
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <HealthChecks checks=vec![]/> },
    );
    assert_eq!(count(&host, ".metric"), 0);
    assert!(host.text_content().unwrap().contains("No checks reported"));
}

/// The Reset button is disabled unless the edition is in a resettable state.
fn reset_disabled_for(resettable: bool) -> bool {
    let host = host();
    let _handle = mount_to(host.clone(), move || {
        view! {
            <EditionActions
                resettable=resettable
                on_acquire=Callback::new(|_| {})
                on_reset=Callback::new(|_| {})
            />
        }
    });
    host.query_selector("button.secondary")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlButtonElement>()
        .unwrap()
        .disabled()
}

#[wasm_bindgen_test]
fn edition_actions_reset_enabled_state() {
    assert!(
        reset_disabled_for(false),
        "Reset disabled when not resettable"
    );
    assert!(!reset_disabled_for(true), "Reset enabled when resettable");
}

// --- Audiobooks (SKADI-T-0133) ---

/// The book-file Reset button is disabled unless the file is in a resettable
/// state (mirrors the movies `EditionActions` test).
fn book_reset_disabled_for(resettable: bool) -> bool {
    let host = host();
    let _handle = mount_to(host.clone(), move || {
        view! {
            <BookFileActions
                resettable=resettable
                on_acquire=Callback::new(|_| {})
                on_reset=Callback::new(|_| {})
            />
        }
    });
    host.query_selector("button.secondary")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlButtonElement>()
        .unwrap()
        .disabled()
}

#[wasm_bindgen_test]
fn book_file_actions_reset_enabled_state() {
    assert!(
        book_reset_disabled_for(false),
        "Reset disabled when not resettable"
    );
    assert!(
        !book_reset_disabled_for(true),
        "Reset enabled when resettable"
    );
    // Acquire is always present.
    let host = host();
    let _handle = mount_to(host.clone(), || {
        view! {
            <BookFileActions
                resettable=false
                on_acquire=Callback::new(|_| {})
                on_reset=Callback::new(|_| {})
            />
        }
    });
    assert_eq!(buttons_with_text(&host, "Acquire"), 1);
}

#[wasm_bindgen_test]
fn audiobooks_config_renders_static_links_offline() {
    // The config page fires fetches on mount (quality ladder, root folders,
    // domains); offline they fail into empty signals but the section structure
    // renders regardless. Its `<A>` links need a `<Router>` ancestor.
    let host = host();
    let _handle = mount_to(host.clone(), || {
        view! { <Router><AudiobooksConfigPage/></Router> }
    });
    let text = host.text_content().unwrap();
    assert!(text.contains("Audiobooks · Config"));
    // Displays the built-in quality ladder (SKADI-T-0142), no longer the "shared
    // profiles" framing. Root-folder management moved to Config → Library root
    // (SKADI-T-0302), so this page links there via a "Library" section rather
    // than embedding a "Root folders" editor.
    assert!(text.contains("Quality ladder"));
    assert!(text.contains("Library root"));
    assert!(!text.contains("share the system quality profiles"));
    // Links to /config are present (manage root folders there).
    let links = host.query_selector_all("a[href=\"/config\"]").unwrap();
    assert!(links.length() >= 1, "links operators to Config");
}

#[wasm_bindgen_test]
fn dashboard_renders_overview_static_structure_offline() {
    // The Overview fires data fetches on mount; with no daemon they fail into the
    // empty signals. Its static structure renders regardless (SKADI-T-0246).
    let host = host();
    // The Overview uses router navigation (`use_navigate`, `<A>`), so mount it
    // inside a Router context like the real app does.
    let _handle = mount_to(host.clone(), || view! { <Router><Dashboard/></Router> });

    let text = host.text_content().unwrap();
    assert!(text.contains("Overview"));
    assert!(text.contains("Active hunt"));
    assert!(text.contains("Recently imported"));
    // The four metric cards render regardless of data.
    assert_eq!(
        count(&host, ".ov-metric"),
        4,
        "Library/Monitored/Wanted/Downloading"
    );
    // Offline → the active-hunt list shows its empty state.
    assert!(text.contains("No active transfers"));
}
