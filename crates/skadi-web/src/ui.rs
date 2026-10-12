//! Small adapters from Skadi's data to Aurora's components (SKADI-T-0719).
//!
//! Skadi names a state with a level class (`ok`, `warn`, `bad`, `pending`,
//! `muted`); [`tone`] turns it into the Aurora token, so a dot or a pill gets
//! its colour from the theme. [`LevelDot`] is Aurora's `Dot` in a span that
//! keeps the class hooks (`health-dot ok`) the tests and the layout read.
//! [`mirror`] binds an Aurora field, which takes an `RwSignal<String>`, to a
//! value that lives in a larger form state.

use aurora_leptos::components::Dot;
use aurora_leptos::tokens::token;
use leptos::prelude::*;

/// The Aurora token for a Skadi level class. Unknown levels are muted.
#[must_use]
pub fn tone(level: &str) -> &'static str {
    match level {
        "ok" | "owned" => token::OK,
        "warn" | "wanted" | "gold" => token::GOLD,
        "bad" | "fail" | "error" => token::BAD,
        "pending" | "dl" | "ice" => token::ICE,
        "teal" => token::TEAL,
        "violet" | "upgrade" => token::VIOLET,
        _ => token::MUTED,
    }
}

/// Aurora's `Dot` in the colour of `level`. The span around it carries
/// `class` plus the level (`health-dot ok`), and `title` when given.
#[component]
pub fn LevelDot(
    /// The level class: `ok`, `warn`, `bad`, `pending` or `muted`.
    #[prop(into)]
    level: String,
    /// The hook class of the span (`health-dot`, `status-dot`).
    #[prop(default = "health-dot".to_string(), into)]
    class: String,
    /// The dot's diameter in px.
    #[prop(default = 9)]
    size: i32,
    /// A hover text (the raw status).
    #[prop(optional, into)]
    title: String,
) -> impl IntoView {
    let color = tone(&level);
    let hidden = title.is_empty().then_some("true");
    let title = (!title.is_empty()).then_some(title);
    view! {
        <span class=format!("{class} {level}") title=title aria-hidden=hidden>
            <Dot color=color size=size/>
        </span>
    }
}

/// An `RwSignal` for an Aurora field that shows `get()` and follows it when
/// the source changes (a form reset, a new item). Write the field's input
/// back to the source with the field's `on_input`.
pub fn mirror(get: impl Fn() -> String + Send + Sync + 'static) -> RwSignal<String> {
    let value = RwSignal::new(untrack(&get));
    Effect::new(move |_| {
        let v = get();
        if value.get_untracked() != v {
            value.set(v);
        }
    });
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_level_has_its_token() {
        assert_eq!(tone("ok"), token::OK);
        assert_eq!(tone("warn"), token::GOLD);
        assert_eq!(tone("bad"), token::BAD);
        assert_eq!(tone("pending"), token::ICE);
        assert_eq!(tone("muted"), token::MUTED);
        assert_eq!(tone("something else"), token::MUTED);
    }
}
