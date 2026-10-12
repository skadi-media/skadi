//! Every Aurora theme must work (SKADI-T-0719): the web UI holds no colour of
//! its own. Aurora's tokens.css is the only place with raw colours; style.css
//! and the inline styles in src/ use tokens (`var(--ice)`, `var(--ok-bg)`,
//! `color-mix()` over tokens). Modelled on Brokkr's tests/theme.rs.
//!
//! The files are read with `include_str!`, so this runs as wasm under
//! `angreal test web` and natively under `cargo test --test theme`. SOURCES
//! must name every module in src/lib.rs; a test checks that.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;

#[cfg(target_arch = "wasm32")]
wasm_bindgen_test_configure!(run_in_browser);

macro_rules! sources {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../src/", $name, ".rs")))),*]
    };
}

/// Every file in src/, by module name.
const SOURCES: &[(&str, &str)] = sources![
    "a11y",
    "activity",
    "add",
    "api",
    "app",
    "audiobook_import",
    "audiobooks",
    "config",
    "confirm",
    "dashboard",
    "downloads",
    "household",
    "import",
    "import_common",
    "lib",
    "library_select",
    "library_toolbar",
    "live_poll",
    "loading",
    "main",
    "movies",
    "naming",
    "offline",
    "path_picker",
    "persist",
    "player",
    "preferences",
    "settings",
    "setup",
    "shortcuts",
    "subnav",
    "system",
    "tv",
    "tv_import",
    "ui",
    "upload",
    "wanted",
    "watch",
];

const STYLE_CSS: &str = include_str!("../style.css");

/// A char that can continue an identifier: `white-space`, `fg_bright`, `rgb2`.
fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// `#abc`, `#abcd`, `#aabbcc` or `#aabbccdd` followed by a non-word char, and
/// the same after `%23` (a `#` inside a data URI).
fn hex_colours(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut hits = Vec::new();
    for (i, mark) in text
        .match_indices('#')
        .chain(text.match_indices("%23"))
        .collect::<Vec<_>>()
    {
        let start = i + mark.len();
        let n = b[start..]
            .iter()
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        let next = b.get(start + n).copied().unwrap_or(b' ');
        if matches!(n, 3 | 4 | 6 | 8) && !(next.is_ascii_alphanumeric() || next == b'_') {
            hits.push(text[i..start + n].to_string());
        }
    }
    hits
}

/// A CSS colour function, `rgb(` to `oklch(`, as a whole word: `color-mix(`
/// and `var(--lab(` are not colours.
fn colour_functions(text: &str) -> Vec<String> {
    const FNS: [&str; 10] = [
        "rgb(", "rgba(", "hsl(", "hsla(", "hwb(", "lab(", "lch(", "oklab(", "oklch(", "color(",
    ];
    let b = text.as_bytes();
    let mut hits = Vec::new();
    for f in FNS {
        for (i, _) in text.match_indices(f) {
            let prev = if i == 0 { b' ' } else { b[i - 1] };
            if !is_word(prev) {
                hits.push(f.to_string());
            }
        }
    }
    hits
}

const NAMES: [&str; 22] = [
    "red", "white", "black", "blue", "green", "yellow", "orange", "purple", "pink", "brown",
    "gray", "grey", "cyan", "magenta", "silver", "navy", "maroon", "olive", "lime", "aqua",
    "fuchsia", "indigo",
];

/// A common named colour where CSS reads a value, as a whole word (not
/// `white-space`, `--gold-fg`). In Rust (`css == false`): right after `:`,
/// `,`, `(`, `=` or a quote, as in an inline style. In CSS (`css == true`):
/// anywhere in the value of a declaration, so `border: 1px solid white` is
/// found too.
fn named_colours(text: &str, css: bool) -> Vec<String> {
    let b = text.as_bytes();
    let mut hits = Vec::new();
    for name in NAMES {
        for (i, _) in text.match_indices(name) {
            let end = i + name.len();
            let prev = if i == 0 { b' ' } else { b[i - 1] };
            let next = b.get(end).copied().unwrap_or(b' ');
            if is_word(prev) || is_word(next) {
                continue;
            }
            let before = &text[..i];
            let in_value = if css {
                // After the last `:` of a declaration, before its `;`.
                before
                    .rfind(':')
                    .is_some_and(|c| !before[c..].contains([';', '{', '}']))
            } else {
                matches!(
                    before.trim_end_matches(' ').as_bytes().last(),
                    Some(b':' | b',' | b'(' | b'=' | b'"' | b'\'')
                )
            };
            if in_value {
                hits.push(name.to_string());
            }
        }
    }
    hits
}

fn raw_colours(line: &str, css: bool) -> Vec<String> {
    let mut found = hex_colours(line);
    found.extend(colour_functions(line));
    found.extend(named_colours(line, css));
    found
}

/// CSS with its comments blanked (newlines kept, so line numbers hold).
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find("*/")
            .map_or(rest.len(), |e| start + e + 2);
        out.extend(rest[start..end].chars().filter(|c| *c == '\n'));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The raw colours in one file, as `file:line: colour`.
fn scan(file: &str, text: &str, css: bool) -> Vec<String> {
    let text = if css {
        strip_css_comments(text)
    } else {
        text.to_string()
    };
    let mut bad = Vec::new();
    for (n, line) in text.lines().enumerate() {
        // A Rust comment may name a colour; only code can paint one.
        if !css && line.trim_start().starts_with("//") {
            continue;
        }
        for h in raw_colours(line, css) {
            bad.push(format!("{file}:{}: {h}", n + 1));
        }
    }
    bad
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn style_css_has_no_raw_colour() {
    let bad = scan("style.css", STYLE_CSS, true);
    assert!(bad.is_empty(), "raw colours:\n{}", bad.join("\n"));
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn src_has_no_raw_colour() {
    let mut bad = Vec::new();
    for (name, text) in SOURCES {
        bad.extend(scan(&format!("src/{name}.rs"), text, false));
    }
    assert!(bad.is_empty(), "raw colours:\n{}", bad.join("\n"));
}

/// SOURCES names every module that src/lib.rs declares, plus lib and main.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn sources_cover_every_module() {
    let lib = SOURCES.iter().find(|(n, _)| *n == "lib").expect("lib").1;
    let mut modules: Vec<&str> = lib
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("pub ").unwrap_or(l);
            l.strip_prefix("mod ")?.strip_suffix(';')
        })
        .collect();
    modules.extend(["lib", "main"]);
    modules.sort_unstable();
    let mut listed: Vec<&str> = SOURCES.iter().map(|(n, _)| *n).collect();
    listed.sort_unstable();
    assert_eq!(
        listed, modules,
        "SOURCES and the modules of src/lib.rs differ"
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn hex_scanner_finds_colours_and_skips_fragments() {
    assert_eq!(hex_colours("stroke=\"#0b0d10\""), vec!["#0b0d10"]);
    assert_eq!(hex_colours("a #fff; b"), vec!["#fff"]);
    assert_eq!(hex_colours("stroke='%237f8b9e'"), vec!["%237f8b9e"]);
    assert!(hex_colours("href=\"#main\" #{id} #settings-theme").is_empty());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn function_scanner_finds_every_colour_space() {
    for f in [
        "rgb(", "rgba(", "hsl(", "hsla(", "hwb(", "lab(", "lch(", "oklab(", "oklch(", "color(",
    ] {
        let line = format!("color: {f}1 2 3);");
        assert_eq!(colour_functions(&line), vec![f.to_string()], "{line}");
    }
    assert!(
        colour_functions("background: color-mix(in srgb, var(--ice) 12%, transparent);").is_empty()
    );
    assert!(colour_functions("let slab(x) = 1;").is_empty());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn name_scanner_finds_values_and_skips_words() {
    assert_eq!(named_colours("color: red;", true), vec!["red"]);
    assert_eq!(named_colours("color:red", false), vec!["red"]);
    assert_eq!(
        named_colours("border: 1px solid white;", true),
        vec!["white"]
    );
    assert_eq!(named_colours("fill=\"black\"", false), vec!["black"]);
    assert_eq!(
        named_colours("background: linear-gradient(blue, green)", true),
        vec!["blue", "green"]
    );
    assert!(named_colours("white-space: nowrap;", true).is_empty());
    assert!(named_colours("color: var(--gold-fg);", true).is_empty());
    assert!(named_colours(".tag.red { color: var(--bad); }", true).is_empty());
    assert!(named_colours("the red dot pulses", false).is_empty());
    assert!(named_colours("let greyhound = 1;", false).is_empty());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn css_comments_do_not_count() {
    let css = "/* was #3a4250 and rgba(0, 0, 0, 0.5) */\n.a { color: var(--fg); }\n";
    assert!(scan("x.css", css, true).is_empty());
    assert_eq!(
        scan("x.css", "/* a */\n.b { color: #fff; }\n", true),
        vec!["x.css:2: #fff"]
    );
}
