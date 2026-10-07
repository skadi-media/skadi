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
            ..Default::default()
        },
        HealthCheck {
            name: "indexer:dead".into(),
            status: "fail".into(),
            detail: "connection refused".into(),
            severity: Some("error".into()),
            ..Default::default()
        },
        HealthCheck {
            name: "disk-space".into(),
            status: "warn".into(),
            detail: "80 % used".into(),
            severity: Some("warn".into()),
            ..Default::default()
        },
        HealthCheck {
            name: "domain:movies".into(),
            status: "warn".into(),
            detail: "not checked yet".into(),
            severity: Some("pending".into()),
            ..Default::default()
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

// --- System page (SKADI-T-0684) ---------------------------------------------

fn sys_check(id: &str, severity: &str, remediation: Option<&str>) -> HealthCheck {
    HealthCheck {
        name: id.into(),
        status: "ok".into(),
        detail: format!("{id} says {severity}"),
        severity: Some(severity.into()),
        label: Some(format!("Label {id}")),
        remediation: remediation.map(str::to_string),
        checked_at: (severity != "pending").then(|| "2026-10-07T05:49:48.1Z".to_string()),
    }
}

#[wasm_bindgen_test]
fn system_check_table_shows_every_check_with_its_fix_under_a_banner() {
    use skadi_web::system::CheckTable;
    let checks = vec![
        sys_check("database", "ok", None),
        sys_check(
            "disk-space",
            "warn",
            Some("Free some space on the library disk."),
        ),
        sys_check("vpn", "error", Some("Start gluetun.")),
        sys_check("worker", "pending", None),
    ];
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <CheckTable checks=checks.clone()/> },
    );
    assert_eq!(count(&host, "tbody tr.check-row"), 4, "one row per check");
    assert_eq!(
        count(&host, ".system-banner.bad"),
        1,
        "an error raises the red banner"
    );
    assert_eq!(count(&host, ".status-dot.warn"), 1);
    assert_eq!(count(&host, ".status-dot.bad"), 1);
    assert_eq!(count(&host, ".status-dot.pending"), 1);
    let text = host.text_content().unwrap();
    assert!(text.contains("Start gluetun."), "remediation shown: {text}");
    assert!(text.contains("Label disk-space"), "label shown");
    assert!(text.contains("2026-10-07 05:49:48"), "checked_at shown");
    assert!(text.contains("—"), "a pending check has no time: {text}");
}

#[wasm_bindgen_test]
fn system_check_table_has_no_banner_when_all_is_well() {
    use skadi_web::system::CheckTable;
    let checks = vec![
        sys_check("database", "ok", None),
        sys_check("worker", "pending", None),
    ];
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <CheckTable checks=checks.clone()/> },
    );
    assert_eq!(count(&host, ".system-banner"), 0);
    let checks = vec![
        sys_check("database", "ok", None),
        sys_check("disk-space", "warn", Some("x")),
    ];
    let host = self::host();
    let _handle2 = mount_to(
        host.clone(),
        move || view! { <CheckTable checks=checks.clone()/> },
    );
    assert_eq!(
        count(&host, ".system-banner.warn"),
        1,
        "a warning raises the amber banner"
    );
}

#[wasm_bindgen_test]
fn system_tasks_keep_the_run_columns_with_a_dash() {
    use skadi_web::api::SystemTask;
    use skadi_web::system::TaskTable;
    let tasks = vec![SystemTask {
        name: "rss-sweep".into(),
        interval_seconds: 900,
        what: "fast pass over RSS".into(),
        last_run: None,
        next_run: None,
    }];
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <TaskTable tasks=tasks.clone()/> },
    );
    assert_eq!(
        count(&host, "thead th"),
        4,
        "last and next run columns are there"
    );
    assert_eq!(count(&host, "tbody tr.task-row"), 1);
    let text = host.text_content().unwrap();
    assert!(text.contains("every 15 min"), "{text}");
    assert_eq!(
        text.matches('—').count(),
        2,
        "last + next run are dashes: {text}"
    );
}

#[wasm_bindgen_test]
fn system_status_shows_the_build_commit() {
    use skadi_web::api::SystemStatus;
    use skadi_web::system::StatusFacts;
    let status = SystemStatus {
        version: "0.1.4".into(),
        commit: Some("0fd05ce3cf9d".into()),
        start_time: "2026-10-07T05:00:00+00:00".into(),
        uptime_seconds: 3 * 3600 + 120,
        database: "postgres".into(),
        library_root: "/data/library".into(),
        domains: vec![],
    };
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <StatusFacts status=status.clone()/> },
    );
    let commit = host.query_selector(".system-commit").unwrap().unwrap();
    assert_eq!(commit.text_content().as_deref(), Some("0fd05ce3cf9d"));
    let text = host.text_content().unwrap();
    for want in ["0.1.4", "3h 2m", "postgres", "/data/library"] {
        assert!(text.contains(want), "{want} in {text}");
    }
}

#[wasm_bindgen_test]
fn system_status_says_unknown_for_a_build_without_a_commit() {
    use skadi_web::api::SystemStatus;
    use skadi_web::system::StatusFacts;
    let status = SystemStatus {
        version: "0.1.4".into(),
        commit: None,
        ..Default::default()
    };
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <StatusFacts status=status.clone()/> },
    );
    let commit = host.query_selector(".system-commit").unwrap().unwrap();
    assert_eq!(commit.text_content().as_deref(), Some("unknown"));
}

#[wasm_bindgen_test]
fn system_log_rows_carry_their_level_tone() {
    use skadi_web::api::LogLine;
    use skadi_web::system::LogTable;
    let line = |level: &str| LogLine {
        time: "2026-10-07T05:49:48.1+00:00".into(),
        level: level.into(),
        target: "skadi_api".into(),
        message: format!("a {level} line"),
    };
    let lines = vec![line("ERROR"), line("WARN"), line("INFO")];
    let host = host();
    let _handle = mount_to(
        host.clone(),
        move || view! { <LogTable lines=lines.clone()/> },
    );
    assert_eq!(count(&host, "tbody tr.log-row"), 3);
    assert_eq!(count(&host, ".log-level.bad"), 1);
    assert_eq!(count(&host, ".log-level.warn"), 1);
    assert!(host.text_content().unwrap().contains("a WARN line"));
}

/// A non-admin, or a role not known yet, gets no panels, and so no fetch: the
/// panels and their requests live in a child only the admin mounts.
#[wasm_bindgen_test]
fn the_system_page_draws_no_panels_for_anyone_but_the_admin() {
    use skadi_web::subnav::RoleCtx;
    use skadi_web::system::SystemPage;
    for (role, says) in [
        (Some("member"), "household admin"),
        (Some("contributor"), "household admin"),
        (Some("kid"), "household admin"),
        (None, "Loading"),
    ] {
        let host = host();
        let _handle = mount_to(host.clone(), move || {
            provide_context(RoleCtx(RwSignal::new(role.map(str::to_string))));
            view! { <SystemPage/> }
        });
        let text = host.text_content().unwrap();
        assert!(text.contains(says), "{role:?}: {text}");
        assert_eq!(count(&host, ".provider-section"), 0, "{role:?} got panels");
        assert_eq!(count(&host, "button"), 0, "{role:?} got a Run now button");
    }
}

// ---------------------------------------------------------------------------
// ConfirmDialog (SKADI-T-0694)
// ---------------------------------------------------------------------------

/// Let the dialog's view and its first-focus effect run.
async fn settle_dom() {
    gloo_timers::future::TimeoutFuture::new(0).await;
    gloo_timers::future::TimeoutFuture::new(0).await;
}

fn active_element() -> Option<web_sys::Element> {
    document().active_element()
}

/// Mount a trigger button plus the dialog host, focus the trigger, and ask
/// `spec`; the answer lands in the returned cell.
async fn ask(
    trigger_id: &'static str,
    spec: skadi_web::confirm::ConfirmSpec,
) -> (
    HtmlElement,
    HtmlElement,
    std::rc::Rc<std::cell::Cell<Option<bool>>>,
    Box<dyn std::any::Any>,
) {
    use skadi_web::confirm::{ConfirmDialogHost, confirm};
    let host = host();
    let handle = mount_to(host.clone(), move || {
        view! {
            <button id=trigger_id>"Trigger"</button>
            <ConfirmDialogHost/>
        }
    });
    let trigger = host
        .query_selector(&format!("#{trigger_id}"))
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    trigger.focus().unwrap();
    let answer = std::rc::Rc::new(std::cell::Cell::new(None));
    let slot = answer.clone();
    wasm_bindgen_futures::spawn_local(async move { slot.set(Some(confirm(spec).await)) });
    settle_dom().await;
    (host, trigger, answer, Box::new(handle))
}

fn find(host: &HtmlElement, selector: &str) -> HtmlElement {
    host.query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no {selector}"))
        .dyn_into::<HtmlElement>()
        .unwrap()
}

fn key(target: &HtmlElement, key: &str, shift: bool) {
    let init = web_sys::KeyboardEventInit::new();
    init.set_key(key);
    init.set_shift_key(shift);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    target.dispatch_event(&ev).unwrap();
}

#[wasm_bindgen_test]
async fn confirm_dialog_confirm_resolves_true_and_gives_focus_back() {
    use skadi_web::confirm::ConfirmSpec;
    let spec = ConfirmSpec::destructive("Delete the movie?", "Dune and its files go.")
        .confirm_label("Delete");
    let (host, trigger, answer, _handle) = ask("confirm-trigger-ok", spec).await;

    let dialog = find(&host, ".confirm-dialog");
    assert_eq!(dialog.get_attribute("role").as_deref(), Some("alertdialog"));
    assert_eq!(dialog.get_attribute("aria-modal").as_deref(), Some("true"));
    assert_eq!(
        find(&host, ".confirm-title").text_content().as_deref(),
        Some("Delete the movie?")
    );
    assert!(
        dialog
            .text_content()
            .unwrap()
            .contains("Dune and its files go.")
    );
    let ok = find(&host, ".confirm-ok");
    assert!(ok.class_list().contains("danger"));
    assert_eq!(ok.text_content().as_deref(), Some("Delete"));
    // Destructive: the first focus is on Cancel, so a stray Enter cancels.
    let cancel = find(&host, ".confirm-cancel");
    assert_eq!(active_element().as_ref(), Some(cancel.as_ref()));
    assert_eq!(answer.get(), None, "answered before a click");

    ok.click();
    settle_dom().await;
    assert_eq!(answer.get(), Some(true));
    assert_eq!(count(&host, ".confirm-dialog"), 0, "dialog stayed open");
    assert_eq!(active_element().as_ref(), Some(trigger.as_ref()));
}

#[wasm_bindgen_test]
async fn confirm_dialog_cancel_button_and_esc_resolve_false() {
    use skadi_web::confirm::ConfirmSpec;
    // The Cancel button.
    let spec = ConfirmSpec::destructive("Remove the member?", "Ann's phone stops working.");
    let (host, trigger, answer, handle) = ask("confirm-trigger-cancel", spec).await;
    find(&host, ".confirm-cancel").click();
    settle_dom().await;
    assert_eq!(answer.get(), Some(false));
    assert_eq!(count(&host, ".confirm-dialog"), 0);
    assert_eq!(active_element().as_ref(), Some(trigger.as_ref()));
    drop(handle);

    // Esc, pressed inside the dialog.
    let spec = ConfirmSpec::destructive("Remove the member?", "Ann's phone stops working.");
    let (host, trigger, answer, _handle) = ask("confirm-trigger-esc", spec).await;
    key(&find(&host, ".confirm-dialog"), "Escape", false);
    settle_dom().await;
    assert_eq!(answer.get(), Some(false));
    assert_eq!(count(&host, ".confirm-dialog"), 0);
    assert_eq!(active_element().as_ref(), Some(trigger.as_ref()));
}

#[wasm_bindgen_test]
async fn confirm_dialog_traps_tab_and_a_backdrop_click_cancels() {
    use skadi_web::confirm::ConfirmSpec;
    // Not destructive: the first focus is on the confirm button.
    let spec = ConfirmSpec::new("Grab a season pack?", "It satisfies every episode.");
    let (host, _trigger, answer, _handle) = ask("confirm-trigger-tab", spec).await;
    let ok = find(&host, ".confirm-ok");
    let cancel = find(&host, ".confirm-cancel");
    assert!(ok.class_list().contains("primary"));
    assert_eq!(active_element().as_ref(), Some(ok.as_ref()));

    // Tab from the last button wraps to the first, Shift+Tab back again.
    key(&ok, "Tab", false);
    assert_eq!(active_element().as_ref(), Some(cancel.as_ref()));
    key(&cancel, "Tab", true);
    assert_eq!(active_element().as_ref(), Some(ok.as_ref()));
    key(&ok, "Tab", true);
    assert_eq!(active_element().as_ref(), Some(cancel.as_ref()));
    assert_eq!(answer.get(), None);

    // A click on the dialog itself does nothing; one on the backdrop cancels.
    find(&host, ".confirm-body").click();
    settle_dom().await;
    assert_eq!(answer.get(), None);
    find(&host, ".confirm-backdrop").click();
    settle_dom().await;
    assert_eq!(answer.get(), Some(false));
}

// --- Library toolbar (SKADI-T-0695) ---

fn mount_toolbar(
    storage_key: &'static str,
    sort_hidden: RwSignal<bool>,
) -> (
    HtmlElement,
    RwSignal<skadi_web::library_toolbar::SortState>,
    RwSignal<&'static str>,
) {
    use skadi_web::library_toolbar::{LibCounts, LibraryToolbar, VIDEO_SORT_KEYS, stored_sort};
    let host = host();
    let sort = stored_sort(storage_key, VIDEO_SORT_KEYS);
    let filter = RwSignal::new("all");
    let text = RwSignal::new(String::new());
    let counts = Signal::stored(LibCounts {
        total: 3,
        owned: 1,
        wanted: 1,
        downloading: 1,
    });
    let handle = mount_to(host.clone(), move || {
        view! {
            <LibraryToolbar
                chips=skadi_web::movies::MOVIE_CHIPS
                filter=filter
                counts=counts
                text=text
                placeholder="Filter"
                sort=sort
                sort_keys=VIDEO_SORT_KEYS
                storage_key=storage_key
                sort_hidden=Signal::derive(move || sort_hidden.get())
                bulk=|| view! { <span class="test-bulk">"2 selected"</span> }
            />
        }
    });
    std::mem::forget(handle);
    (host, sort, filter)
}

fn local_storage() -> web_sys::Storage {
    web_sys::window().unwrap().local_storage().unwrap().unwrap()
}

/// Picking a key and flipping the direction writes the sort to localStorage,
/// and a fresh mount (a reload) starts from it.
#[wasm_bindgen_test]
async fn library_sort_choice_persists_across_a_remount() {
    let key = "test_library_sort";
    local_storage().remove_item(key).unwrap();
    let (host, sort, _) = mount_toolbar(key, RwSignal::new(false));
    let select = host
        .query_selector(".lib-sort-key")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlSelectElement>()
        .unwrap();
    assert_eq!(select.value(), "title");
    assert_eq!(select.length(), 4); // title, added, year, status
    select.set_value("year");
    select
        .dispatch_event(&web_sys::Event::new("change").unwrap())
        .unwrap();
    settle_dom().await;
    assert_eq!(
        local_storage().get_item(key).unwrap().as_deref(),
        Some("year:desc")
    );
    host.query_selector(".lib-sort-dir")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlButtonElement>()
        .unwrap()
        .click();
    settle_dom().await;
    assert_eq!(
        local_storage().get_item(key).unwrap().as_deref(),
        Some("year:asc")
    );
    assert!(sort.get_untracked().asc);

    // "Reload": a new toolbar on the same key starts from the stored sort.
    let (host2, sort2, _) = mount_toolbar(key, RwSignal::new(false));
    settle_dom().await;
    assert_eq!(
        sort2.get_untracked().encode(),
        "year:asc",
        "the remount restores the stored sort"
    );
    let select2 = host2
        .query_selector(".lib-sort-key")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::HtmlSelectElement>()
        .unwrap();
    assert_eq!(select2.value(), "year");
    local_storage().remove_item(key).unwrap();
}

/// The chips show live counts and set the filter; the bulk slot renders in
/// the toolbar; `sort_hidden` hides the sort control.
#[wasm_bindgen_test]
async fn library_toolbar_chips_bulk_slot_and_hidden_sort() {
    let hidden = RwSignal::new(false);
    let (host, _, filter) = mount_toolbar("test_library_sort_2", hidden);
    assert_eq!(count(&host, ".filter-chip"), 4);
    assert_eq!(count(&host, ".lib-toolbar-bulk .test-bulk"), 1);
    let chips = host.query_selector_all(".filter-chip").unwrap();
    let owned = chips.item(1).unwrap().dyn_into::<HtmlElement>().unwrap();
    assert_eq!(owned.text_content().as_deref(), Some("Owned1"));
    owned.click();
    settle_dom().await;
    assert_eq!(filter.get_untracked(), "owned");
    assert!(owned.class_name().contains("active"));

    let sort_box = find(&host, ".lib-sort");
    let display = |el: &HtmlElement| el.get_attribute("style").unwrap_or_default();
    assert!(!display(&sort_box).contains("none"));
    hidden.set(true);
    settle_dom().await;
    assert!(display(&sort_box).contains("display: none"));
}

// ---------------------------------------------------------------------------
// Library bulk bar (SKADI-T-0696)
// ---------------------------------------------------------------------------

fn button_with_text(host: &HtmlElement, label: &str) -> HtmlButtonElement {
    let list = host.query_selector_all("button").unwrap();
    (0..list.length())
        .filter_map(|i| list.item(i))
        .find(|n| n.text_content().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("no button {label:?}"))
        .dyn_into::<HtmlButtonElement>()
        .unwrap()
}

/// Mount the bar over a wall of `wall` ids with the dialog host.
fn mount_bulk_bar(
    wall: &[&str],
) -> (
    HtmlElement,
    skadi_web::library_select::Selection,
    RwSignal<usize>,
    Box<dyn std::any::Any>,
) {
    use skadi_web::confirm::ConfirmDialogHost;
    use skadi_web::library_select::{BulkBar, MOVIES, Selection};
    let host = host();
    let sel = Selection::new();
    let ids: Vec<String> = wall.iter().map(|s| (*s).to_string()).collect();
    let done = RwSignal::new(0usize);
    let on_done = Callback::new(move |_| done.update(|n| *n += 1));
    let handle = mount_to(host.clone(), move || {
        let visible = Signal::derive({
            let ids = ids.clone();
            move || ids.clone()
        });
        view! {
            <BulkBar sel=sel visible=visible collection="movies" noun=MOVIES on_done=on_done/>
            <ConfirmDialogHost/>
        }
    });
    (host, sel, done, Box::new(handle))
}

#[wasm_bindgen_test]
async fn bulk_bar_select_all_takes_the_wall_and_clears_again() {
    let (host, sel, _done, _h) = mount_bulk_bar(&["c", "a", "b"]);
    assert_eq!(buttons_with_text(&host, "Monitor"), 0, "off until Select");
    button_with_text(&host, "Select").click();
    settle_dom().await;
    assert!(sel.mode.get_untracked());
    assert_eq!(
        find(&host, ".bulk-count").text_content().as_deref(),
        Some("0 selected")
    );
    assert!(
        button_with_text(&host, "Unmonitor").disabled(),
        "nothing selected yet"
    );

    button_with_text(&host, "Select all (3)").click();
    settle_dom().await;
    assert_eq!(
        find(&host, ".bulk-count").text_content().as_deref(),
        Some("3 selected")
    );
    assert!(!button_with_text(&host, "Unmonitor").disabled());
    button_with_text(&host, "Clear").click();
    settle_dom().await;
    assert_eq!(
        find(&host, ".bulk-count").text_content().as_deref(),
        Some("0 selected")
    );

    button_with_text(&host, "Done").click();
    settle_dom().await;
    assert!(!sel.mode.get_untracked());
    assert_eq!(buttons_with_text(&host, "Select"), 1);
}

/// Delete names the count and asks once; cancelling sends nothing.
#[wasm_bindgen_test]
async fn bulk_delete_names_the_count_and_asks_once() {
    let (host, sel, done, _h) = mount_bulk_bar(&["a", "b", "c", "d"]);
    sel.mode.set(true);
    // "z" is selected but no longer on the wall (a filter hid it): not counted.
    sel.ids.set(
        ["a", "b", "d", "z"]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    );
    settle_dom().await;
    assert_eq!(
        find(&host, ".bulk-count").text_content().as_deref(),
        Some("3 selected")
    );

    button_with_text(&host, "Delete + files").click();
    settle_dom().await;
    assert_eq!(count(&host, ".confirm-dialog"), 1, "one question");
    assert_eq!(
        find(&host, ".confirm-title").text_content().as_deref(),
        Some("Delete 3 movies?")
    );
    assert!(
        find(&host, ".confirm-dialog")
            .text_content()
            .unwrap()
            .contains("deleted from disk")
    );
    assert_eq!(
        find(&host, ".confirm-ok").text_content().as_deref(),
        Some("Delete 3")
    );
    find(&host, ".confirm-cancel").click();
    settle_dom().await;
    assert_eq!(count(&host, ".confirm-dialog"), 0);
    assert_eq!(done.get_untracked(), 0, "a cancelled delete sends nothing");
    assert_eq!(sel.ids.get_untracked().len(), 4, "the selection is kept");
}
