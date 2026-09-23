//! skadi-web library surface — the page/component modules and the typed API
//! client, exposed as a `lib` (alongside the `main.rs` bin) so they can be
//! exercised by `wasm-bindgen-test` (SKADI-T-0118). The bin (`main.rs`) is a
//! thin shell that mounts [`App`]; everything testable lives here.
//!
//! Only the modules the test suites reach (`api`, `movies`, `dashboard`) are
//! `pub`; the rest stay crate-internal so the `#[component]` macro's generated
//! `pub` items don't leak crate-private helper types.

pub mod activity;
pub mod api;
pub mod audiobook_import;
pub mod audiobooks;
pub mod dashboard;
pub mod household;
pub mod import_common;
pub mod movies;
pub mod tv;
pub mod tv_import;
pub mod wanted;
pub mod watch;

mod add;
mod app;
pub mod config;
mod import;
mod naming;
pub mod offline;
pub mod path_picker;
pub mod persist;
pub mod player;
mod settings;
mod setup;
pub mod subnav;
pub mod upload;

pub use app::App;

/// Genre facet over a wall (SKADI-T-0605): each genre with how many items
/// carry it, most common first then by name — one chip per entry. A genre
/// listed twice on one item counts once. Pure, so `tests/logic.rs` can pin it.
#[must_use]
pub fn genre_counts<'a, I>(items: I) -> Vec<(String, usize)>
where
    I: IntoIterator<Item = &'a [String]>,
{
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for genres in items {
        let mut seen = std::collections::HashSet::new();
        for g in genres {
            let g = g.trim();
            if !g.is_empty() && seen.insert(g) {
                *counts.entry(g.to_string()).or_insert(0) += 1;
            }
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}
