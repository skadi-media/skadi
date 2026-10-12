//! Keyboard and screen-reader helpers shared by the views (SKADI-T-0700).
//!
//! A `<button>` or `<a href>` is keyboard-operable for free. Where a view must
//! use another element as a control (a poster tile, an accordion header, a row
//! that expands), give it `tabindex="0"`, a `role`, and route `keydown`
//! through [`activates`], so Enter and Space do what a click does.

/// Whether `key` (a `KeyboardEvent.key`) activates a control: Enter or Space.
pub fn is_activation_key(key: &str) -> bool {
    matches!(key, "Enter" | " " | "Spacebar")
}

/// True when this keydown should run the control's click action. It is the
/// control's own key (not one that bubbled from a child control), and it is
/// Enter or Space. Space would scroll the page, so the default is prevented.
pub fn activates(ev: &web_sys::KeyboardEvent) -> bool {
    if !is_activation_key(&ev.key()) || ev.alt_key() || ev.ctrl_key() || ev.meta_key() {
        return false;
    }
    if ev.target() != ev.current_target() {
        return false;
    }
    ev.prevent_default();
    true
}

/// The `aria-expanded` value for a disclosure.
pub fn expanded(open: bool) -> &'static str {
    if open { "true" } else { "false" }
}

/// Whether a key press may run a page-wide one-key shortcut: no Ctrl / Cmd /
/// Alt, and the focus is not in a text field (where the key types text).
pub fn shortcut_allowed(ctrl: bool, meta: bool, alt: bool, editable_target: bool) -> bool {
    !(ctrl || meta || alt || editable_target)
}

/// Whether `el` takes typed text: an `<input>` (not a checkbox, radio, button
/// or range), a `<textarea>`, a `<select>`, or a contenteditable element.
pub fn is_editable(el: &web_sys::Element) -> bool {
    use wasm_bindgen::JsCast;
    match el.tag_name().to_ascii_lowercase().as_str() {
        "textarea" | "select" => true,
        "input" => {
            let kind = el
                .get_attribute("type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            !matches!(
                kind.as_str(),
                "checkbox" | "radio" | "button" | "submit" | "reset" | "range" | "color" | "file"
            )
        }
        _ => el
            .dyn_ref::<web_sys::HtmlElement>()
            .is_some_and(|h| h.is_content_editable()),
    }
}

/// [`shortcut_allowed`] for a live event.
pub fn shortcut_event(ev: &web_sys::KeyboardEvent) -> bool {
    use wasm_bindgen::JsCast;
    let editable = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|e| is_editable(&e));
    shortcut_allowed(ev.ctrl_key(), ev.meta_key(), ev.alt_key(), editable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_space_activate() {
        assert!(is_activation_key("Enter"));
        assert!(is_activation_key(" "));
        assert!(is_activation_key("Spacebar"));
        assert!(!is_activation_key("a"));
        assert!(!is_activation_key("Tab"));
    }

    #[test]
    fn a_modifier_or_a_text_field_blocks_a_one_key_shortcut() {
        assert!(shortcut_allowed(false, false, false, false));
        assert!(!shortcut_allowed(true, false, false, false));
        assert!(!shortcut_allowed(false, true, false, false));
        assert!(!shortcut_allowed(false, false, true, false));
        assert!(!shortcut_allowed(false, false, false, true));
    }

    /// The selector of the rule around byte `at` in `css`.
    fn selector_at(css: &str, at: usize) -> &str {
        let open = css[..at].rfind('{').expect("inside a rule");
        let start = css[..open].rfind(['}', '/']).map_or(0, |i| i + 1);
        css[start..open].trim()
    }

    /// Keyboard focus is always visible: one global `:focus-visible` ring in
    /// Aurora's ice, and no bare `outline: none` left on a focusable element.
    /// The one kept is not a Tab stop (`<main>`, the skip target). The ring
    /// loads after Aurora's stylesheet, so its fields keep it too
    /// (SKADI-T-0719).
    #[test]
    fn style_css_keeps_a_visible_focus_ring() {
        let css = include_str!("../style.css");
        assert!(
            css.contains(
                "\n:focus-visible { outline: 2px solid var(--ice); outline-offset: 2px; }"
            ),
            "the global ring rule"
        );
        let allowed = [".main:focus-visible"];
        for (at, _) in css.match_indices("outline: none") {
            let sel = selector_at(css, at);
            assert!(
                allowed.contains(&sel),
                "bare `outline: none` on `{sel}`: style :focus-visible instead"
            );
        }
    }

    /// Skadi's text tones meet WCAG AA (4.5:1) on every surface they sit on,
    /// in each Aurora theme (SKADI-T-0700; the tokens are Aurora's since
    /// SKADI-T-0717). Light and dark are the two halves of tokens.css'
    /// `light-dark()`; hyper is hyper.css.
    #[test]
    fn faint_text_tokens_meet_aa_contrast() {
        fn lum(hex: &str) -> f64 {
            let c = |i: usize| {
                let v = u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f64 / 255.0;
                if v <= 0.039_28 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * c(1) + 0.7152 * c(3) + 0.0722 * c(5)
        }
        fn ratio(a: &str, b: &str) -> f64 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        /// The value of `--name` in `css`: the text after `--name: ` up to `;`.
        fn value<'a>(css: &'a str, name: &str) -> &'a str {
            let key = format!("  --{name}: ");
            let at = css.find(&key).unwrap_or_else(|| panic!("--{name}"));
            let rest = &css[at + key.len()..];
            &rest[..rest.find(';').unwrap()]
        }
        /// The colour of `--name` in `theme`.
        fn token(theme: &str, name: &str) -> String {
            let pair = || value(aurora_leptos::TOKENS_CSS, name);
            match theme {
                "light" => {
                    let start = "light-dark(".len();
                    pair()[start..start + 7].to_string()
                }
                "dark" => {
                    let v = pair();
                    let comma = v.find(", ").unwrap();
                    v[comma + 2..comma + 9].to_string()
                }
                _ => value(aurora_leptos::HYPER_CSS, name)[..7].to_string(),
            }
        }
        for theme in ["light", "dark", "hyper"] {
            for fg in ["muted", "faint"] {
                for bg in ["bg", "sidebar", "panel", "panel-2", "inset", "control"] {
                    let r = ratio(&token(theme, fg), &token(theme, bg));
                    assert!(r >= 4.5, "{theme}: --{fg} on --{bg} is {r:.2}:1");
                }
            }
        }
    }
}
