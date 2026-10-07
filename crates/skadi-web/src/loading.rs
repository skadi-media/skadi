//! Loading skeletons and the failed-load notice (SKADI-T-0698).
//!
//! Two primitives every list view uses, so a first load and a failed load
//! look the same on every page:
//!
//! - [`Skeleton`]: grey placeholder shapes (a poster grid, table rows or
//!   cards) while the first fetch is in flight. They shimmer, except under
//!   `prefers-reduced-motion: reduce`, where they hold still (style.css).
//! - [`LoadError`]: the "Load failed: …" line with a Retry button. Retry runs
//!   the page's own load function again; it does not reload the page.
//!
//! [`ListLoad`] holds the two states a list load needs (loaded once? last
//! error?) and renders the right primitive for them with
//! [`ListLoad::status`]. Only this module spells "Load failed".

use std::fmt::Display;

use leptos::prelude::*;

/// The prefix of every failed-load message.
pub const LOAD_FAILED: &str = "Load failed";

/// The text [`LoadError`] shows for `message`.
#[must_use]
pub fn load_failed_text(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        format!("{LOAD_FAILED}.")
    } else {
        format!("{LOAD_FAILED}: {message}")
    }
}

/// The shape a [`Skeleton`] stands in for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkeletonKind {
    /// A library wall: poster tiles in the `.poster-grid` layout.
    Posters,
    /// A table or a list of one-line rows.
    Rows,
    /// A stack of settings cards (`.cards`).
    Cards,
}

impl SkeletonKind {
    /// The classes of the skeleton's root element.
    #[must_use]
    pub fn class(self) -> &'static str {
        match self {
            SkeletonKind::Posters => "skeleton sk-posters poster-grid",
            SkeletonKind::Rows => "skeleton sk-rows",
            SkeletonKind::Cards => "skeleton sk-cards",
        }
    }

    /// How many placeholders to draw when the caller does not say.
    #[must_use]
    pub fn default_count(self) -> usize {
        match self {
            SkeletonKind::Posters => 12,
            SkeletonKind::Rows => 6,
            SkeletonKind::Cards => 3,
        }
    }
}

/// Placeholder shapes for a list that is still loading. Announced once as
/// "Loading" (`role=status`); the shapes themselves are hidden from
/// assistive tech.
#[component]
pub fn Skeleton(
    kind: SkeletonKind,
    /// Number of placeholders; defaults to [`SkeletonKind::default_count`].
    #[prop(optional)]
    count: Option<usize>,
) -> impl IntoView {
    let n = count.unwrap_or_else(|| kind.default_count());
    let items = (0..n)
        .map(|_| match kind {
            SkeletonKind::Posters => view! {
                <div class="sk-item sk-tile" aria-hidden="true">
                    <div class="sk-block sk-poster"></div>
                    <div class="sk-line"></div>
                    <div class="sk-line sk-short"></div>
                </div>
            }
            .into_any(),
            SkeletonKind::Rows => view! {
                <div class="sk-item sk-row" aria-hidden="true">
                    <span class="sk-block sk-dot"></span>
                    <span class="sk-line"></span>
                    <span class="sk-line sk-short"></span>
                </div>
            }
            .into_any(),
            SkeletonKind::Cards => view! {
                <div class="sk-item sk-card" aria-hidden="true">
                    <div class="sk-line sk-title"></div>
                    <div class="sk-line"></div>
                    <div class="sk-line sk-short"></div>
                </div>
            }
            .into_any(),
        })
        .collect_view();
    view! {
        <div class=kind.class() role="status" aria-busy="true" aria-label="Loading">
            {items}
        </div>
    }
}

/// A failed list load: the message and a Retry button that calls `retry`.
#[component]
pub fn LoadError(
    /// What went wrong (the API error text).
    #[prop(into)]
    message: String,
    /// Fetch the list again.
    retry: Callback<()>,
) -> impl IntoView {
    view! {
        <div class="notice bad load-error" role="alert">
            <span class="load-error-text">{load_failed_text(&message)}</span>
            <button type="button" class="secondary load-error-retry" on:click=move |_| retry.run(())>
                "Retry"
            </button>
        </div>
    }
}

/// Where a list load is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadPhase {
    /// No answer yet, and no error: show the skeleton.
    Loading,
    /// The last fetch failed: show [`LoadError`] (over any rows already shown).
    Failed(String),
    /// Loaded at least once and the last fetch did not fail.
    Ready,
}

/// The phase for a list that has (or has not) `loaded` once and whose last
/// fetch failed with `error`. An error wins, so a failed poll shows Retry
/// above the rows from an earlier poll.
#[must_use]
pub fn phase_of(loaded: bool, error: Option<&str>) -> LoadPhase {
    match (loaded, error) {
        (_, Some(e)) => LoadPhase::Failed(e.to_string()),
        (false, None) => LoadPhase::Loading,
        (true, None) => LoadPhase::Ready,
    }
}

/// The state of one list load: loaded once, and the error of the last fetch.
/// `Copy`, so closures and spawned futures can hold it.
#[derive(Clone, Copy)]
pub struct ListLoad {
    loaded: RwSignal<bool>,
    error: RwSignal<Option<String>>,
}

impl Default for ListLoad {
    fn default() -> Self {
        Self::new()
    }
}

impl ListLoad {
    #[must_use]
    pub fn new() -> Self {
        Self {
            loaded: RwSignal::new(false),
            error: RwSignal::new(None),
        }
    }

    /// Record the answer of one fetch: `Ok` marks the list loaded and clears
    /// the error, `Err` keeps its text. Returns the value on success. Safe
    /// after the page is gone (`try_set`).
    pub fn settle<T, E: Display>(&self, result: Result<T, E>) -> Option<T> {
        match result {
            Ok(v) => {
                let _ = self.loaded.try_set(true);
                let _ = self.error.try_set(None);
                Some(v)
            }
            Err(e) => {
                let _ = self.error.try_set(Some(e.to_string()));
                None
            }
        }
    }

    /// Mark the list loaded with no error (for a load that cannot fail).
    pub fn done(&self) {
        let _ = self.loaded.try_set(true);
        let _ = self.error.try_set(None);
    }

    /// Whether the list has loaded at least once (reactive).
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.loaded.get()
    }

    /// The current phase (reactive).
    #[must_use]
    pub fn phase(&self) -> LoadPhase {
        let loaded = self.loaded.get();
        self.error.with(|e| phase_of(loaded, e.as_deref()))
    }

    /// The skeleton while the first load runs, [`LoadError`] after a failed
    /// fetch, nothing once loaded. Retry clears the error first, so the
    /// skeleton comes back while a first load is fetched again, then calls
    /// `retry` (the page's load function).
    pub fn status(self, kind: SkeletonKind, retry: Callback<()>) -> impl IntoView {
        self.status_n(kind, None, retry)
    }

    /// [`Self::status`] with a set number of placeholders.
    pub fn status_n(
        self,
        kind: SkeletonKind,
        count: Option<usize>,
        retry: Callback<()>,
    ) -> impl IntoView {
        let error = self.error;
        let again = Callback::new(move |()| {
            error.set(None);
            retry.run(());
        });
        move || match self.phase() {
            LoadPhase::Loading => match count {
                Some(n) => view! { <Skeleton kind=kind count=n/> }.into_any(),
                None => view! { <Skeleton kind=kind/> }.into_any(),
            },
            LoadPhase::Failed(message) => {
                view! { <LoadError message=message retry=again/> }.into_any()
            }
            LoadPhase::Ready => ().into_any(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phase_is_loading_until_the_first_answer() {
        assert_eq!(phase_of(false, None), LoadPhase::Loading);
        assert_eq!(phase_of(true, None), LoadPhase::Ready);
    }

    #[test]
    fn an_error_wins_over_rows_already_loaded() {
        assert_eq!(phase_of(false, Some("x")), LoadPhase::Failed("x".into()));
        assert_eq!(phase_of(true, Some("x")), LoadPhase::Failed("x".into()));
    }

    #[test]
    fn the_message_carries_the_prefix_once() {
        assert_eq!(
            load_failed_text("movies -> HTTP 500"),
            "Load failed: movies -> HTTP 500"
        );
        assert_eq!(load_failed_text("  "), "Load failed.");
    }

    #[test]
    fn each_kind_has_its_own_class_and_some_placeholders() {
        for kind in [
            SkeletonKind::Posters,
            SkeletonKind::Rows,
            SkeletonKind::Cards,
        ] {
            assert!(kind.class().starts_with("skeleton "));
            assert!(kind.default_count() > 0);
        }
        // The wall skeleton uses the real grid, so the tiles line up.
        assert!(SkeletonKind::Posters.class().contains("poster-grid"));
    }

    /// The shimmer stops under reduced motion: style.css has a
    /// `prefers-reduced-motion: reduce` block that turns the animation off on
    /// the skeleton shapes.
    #[test]
    fn style_css_stops_the_shimmer_under_reduced_motion() {
        let css = include_str!("../style.css");
        let start = css
            .find("/* Skeletons (SKADI-T-0698)")
            .expect("skeleton block in style.css");
        let block = &css[start..];
        assert!(block.contains("animation: sk-shimmer"), "no shimmer");
        let reduce = block
            .find("@media (prefers-reduced-motion: reduce)")
            .expect("reduced-motion block for the skeletons");
        let rule = &block[reduce
            ..block[reduce..]
                .find('}')
                .map_or(block.len(), |i| reduce + i)];
        assert!(
            rule.contains(".sk-block") && rule.contains(".sk-line"),
            "{rule}"
        );
        assert!(rule.contains("animation: none"), "{rule}");
    }
}
