//! Shared section navigation (SKADI-T-0627).
//!
//! The sidebar had grown to ten entries, five of them under "System" — two of
//! which (`/indexers`, `/downloaders`) were single-section pages — while the
//! per-domain config lived at `/movies/config` and `/audiobooks/config`,
//! reachable only from inside those library pages. So "where do I change a
//! setting" had seven answers.
//!
//! Rather than merge the pages into one enormous view, each keeps its route and
//! they share a strip of links rendered at the top. The sidebar then needs one
//! entry per *area* instead of one per page, and a deep link to any section
//! still works.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_location;

/// One entry in a section strip: where it goes and what it is called.
pub type Section = (&'static str, &'static str);

/// Everything that configures skadi, in one place.
pub const SETTINGS_SECTIONS: &[Section] = &[
    ("/indexers", "Indexers"),
    ("/downloaders", "Downloads"),
    ("/config", "Library"),
    ("/naming", "Naming"),
    ("/movies/config", "Movies"),
    ("/audiobooks/config", "Audiobooks"),
    ("/household", "Household"),
    // Two jobs that used to share one page (operator, 2026-09-23): setting a
    // phone up is a once-per-device task, while the shelf answers "where did
    // that book go" on the browser you are sitting at. Separate pills, and
    // one word each — a pill is a label, not a sentence.
    ("/players", "Players"),
    ("/listen", "Device"),
];

/// "What is skadi doing, and what is it still missing" — one question, and it
/// used to be two top-level pages. Upload sits here rather than under Settings
/// because sending a file in is something you *do*, like asking skadi to find
/// one, not something you configure (SKADI-T-0631).
pub const ACTIVITY_SECTIONS: &[Section] = &[
    ("/activity", "Running"),
    ("/wanted", "Wanted"),
    ("/upload", "Upload"),
];

/// The signed-in role, published by the shell so every strip can filter itself
/// without its own `/me` round trip.
#[derive(Clone, Copy)]
pub struct RoleCtx(pub RwSignal<Option<String>>);

/// Which of `sections` this role can actually open.
///
/// The strings are what `GET /me` returns: `household::Role` carries
/// `#[serde(rename_all = "lowercase")]`, so they are `admin`, `member`,
/// `contributor`, `kid`. This crate is a wasm target and cannot depend on
/// `skadi-api` to share the enum, so an unknown string is treated as a role
/// with no extra surface rather than being trusted.
///
/// Mirrors `household::path_allowed` on the API side: everything under
/// Settings is admin-only, and of the Activity pages a contributor gets Wanted
/// but not the hunter's running jobs. A link that only ever 403s is worse than
/// no link, so it is not drawn.
pub fn visible(role: Option<&str>, sections: &'static [Section]) -> Vec<Section> {
    match role {
        // Unknown role (still loading, or open mode with no household): show
        // everything rather than flash a truncated strip.
        None | Some("admin") => sections.to_vec(),
        // A contributor may see what is missing and send a file in, but not the
        // hunter's running jobs. Mirrors `path_allowed` (SKADI-T-0630).
        Some("contributor") => sections
            .iter()
            .copied()
            .filter(|(href, _)| matches!(*href, "/wanted" | "/upload"))
            .collect(),
        Some(_) => Vec::new(),
    }
}

/// Every strip, in the order they are searched.
const TABLES: &[(&str, &[Section])] =
    &[("Settings", SETTINGS_SECTIONS), ("Activity", ACTIVITY_SECTIONS)];

/// The strip the page at `path` belongs to, or `None` if it is in no section.
///
/// **The route decides, not the page.** The first cut had each page name its
/// own table, and `/wanted` named the Settings one by mistake — so following
/// "Wanted" from Activity landed on a page headed "Settings" and read as being
/// thrown into the settings area (operator, 2026-09-23). A page cannot get
/// this wrong if it never says which strip it is in.
///
/// Matching is exact rather than by prefix: `/movies/config` must not light up
/// `/movies`, and `/audiobooks` must not light up while you are on
/// `/audiobooks/config`.
pub fn table_for(path: &str) -> Option<(&'static str, &'static [Section])> {
    TABLES
        .iter()
        .find(|(_, sections)| sections.iter().any(|(href, _)| *href == path))
        .map(|(title, sections)| (*title, *sections))
}

/// Render the section strip for whatever page is on screen, marking it.
#[component]
pub fn SubNav() -> impl IntoView {
    let location = use_location();
    let role = use_context::<RoleCtx>();
    move || {
        let Some((title, sections)) = table_for(&location.pathname.get()) else {
            return ().into_any();
        };
        let role = role.and_then(|r| r.0.get());
        let shown = visible(role.as_deref(), sections);
        // One reachable page is not a section; drawing a strip of one is noise.
        if shown.len() < 2 {
            return ().into_any();
        }
        let links = shown
            .into_iter()
            .map(|(href, label)| {
                let here = move || location.pathname.get() == href;
                view! {
                    <A href=href attr:class=move || {
                        if here() { "subnav-link is-here" } else { "subnav-link" }
                    }>
                        {label}
                    </A>
                }
            })
            .collect_view();
        view! {
            <div class="subnav">
                <span class="subnav-title">{title}</span>
                <nav class="subnav-links">{links}</nav>
            </div>
        }
        .into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hrefs(role: Option<&str>, sections: &'static [Section]) -> Vec<&'static str> {
        visible(role, sections).into_iter().map(|(h, _)| h).collect()
    }

    #[test]
    fn an_admin_sees_every_section() {
        assert_eq!(visible(Some("admin"), SETTINGS_SECTIONS).len(), SETTINGS_SECTIONS.len());
        assert_eq!(
        visible(Some("admin"), ACTIVITY_SECTIONS).len(),
        ACTIVITY_SECTIONS.len()
    );
    }

    #[test]
    fn a_contributor_gets_wanted_but_not_the_hunters_internals() {
        assert_eq!(
            hrefs(Some("contributor"), ACTIVITY_SECTIONS),
            vec!["/wanted", "/upload"],
            "a contributor may see what is wanted and send a file in, not the running hunts"
        );
        assert!(hrefs(Some("contributor"), SETTINGS_SECTIONS).is_empty());
    }

    #[test]
    fn a_member_or_a_kid_gets_no_strip_at_all() {
        for role in ["member", "kid"] {
            assert!(hrefs(Some(role), ACTIVITY_SECTIONS).is_empty(), "{role}");
            assert!(hrefs(Some(role), SETTINGS_SECTIONS).is_empty(), "{role}");
        }
    }

    #[test]
    fn an_unknown_role_shows_everything_rather_than_flashing_a_short_strip() {
        // `/me` has not answered yet, or this is open mode with no household.
        assert_eq!(visible(None, SETTINGS_SECTIONS).len(), SETTINGS_SECTIONS.len());
    }

    #[test]
    fn a_page_gets_the_strip_its_own_route_is_listed_in() {
        // The regression: /wanted rendered the Settings strip, so arriving from
        // Activity read as being thrown into Settings.
        assert_eq!(table_for("/wanted").map(|(t, _)| t), Some("Activity"));
        assert_eq!(table_for("/activity").map(|(t, _)| t), Some("Activity"));
        for (href, _) in SETTINGS_SECTIONS {
            assert_eq!(table_for(href).map(|(t, _)| t), Some("Settings"), "{href}");
        }
    }

    #[test]
    fn a_page_in_no_section_draws_no_strip() {
        for path in ["/", "/movies", "/tv", "/add", "/audiobooks", "/listen/abc/def"] {
            assert!(table_for(path).is_none(), "{path}");
        }
    }

    #[test]
    fn every_strip_contains_each_of_its_own_pages() {
        // Whatever route a strip offers, following it must land somewhere that
        // draws that same strip — otherwise the header changes under you.
        for (title, sections) in TABLES {
            for (href, _) in *sections {
                assert_eq!(table_for(href).map(|(t, _)| t), Some(*title), "{href}");
            }
        }
    }

    #[test]
    fn every_pill_label_is_one_word() {
        // A pill is a label, not a sentence (operator, 2026-09-23).
        for (href, label) in SETTINGS_SECTIONS.iter().chain(ACTIVITY_SECTIONS) {
            assert!(!label.contains(' '), "{href} is labelled {label:?}");
        }
    }

    #[test]
    fn every_settings_section_points_somewhere_distinct() {
        let mut seen = std::collections::HashSet::new();
        for (href, label) in SETTINGS_SECTIONS.iter().chain(ACTIVITY_SECTIONS) {
            assert!(href.starts_with('/'), "{href} is not a route");
            assert!(!label.is_empty());
            assert!(seen.insert(*href), "{href} listed twice");
        }
    }
}
