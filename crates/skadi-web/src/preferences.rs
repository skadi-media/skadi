//! Your own settings (SKADI-T-0723): the page behind the gear in the top bar.
//!
//! Every role gets it. The admin Settings pages are the operator's and the
//! server refuses them to anyone else, so a choice that is only about you (the
//! theme) cannot live there. As on the Android app, a gear opens this page.
//! It calls no admin API; each section here is kept in this browser or is
//! open to every role.

use aurora_leptos::components::{Card, PageHeader};
use aurora_leptos::theme::ThemeToggle;
use leptos::prelude::*;

/// Where the gear goes. Not under any Settings strip route.
pub const PREFERENCES_PATH: &str = "/preferences";

/// The personal settings page. One section a heading, so more can follow.
#[component]
pub fn PreferencesPage() -> impl IntoView {
    view! {
        <section class="prefs-page">
            <PageHeader title="Settings" class:page-header=true/>
            <p class="muted">"Your own choices, for this browser."</p>
            <AppearanceSection/>
        </section>
    }
}

/// The theme choice (SKADI-T-0720): Aurora's Light / Dark / Hyper / System
/// toggle. Aurora stores the choice in this browser under the key the init
/// script in `index.html` reads, so a reload paints the chosen theme at once.
/// No choice stored means System: the page follows the operating system.
#[component]
fn AppearanceSection() -> impl IntoView {
    view! {
        <section class="prefs-section" aria-labelledby="prefs-appearance">
            <h2 id="prefs-appearance" class="prefs-section-title">"Appearance"</h2>
            <Card>
                <div class="prefs-row">
                    <span class="prefs-row-label" aria-hidden="true">"Theme"</span>
                    <ThemeToggle hyper=true/>
                </div>
                <p class="prefs-note">"Kept in this browser."</p>
            </Card>
        </section>
    }
}
