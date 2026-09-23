//! Shared BDD world + step modules. Reviewers add fields to `World` and modules
//! under `steps/` (register them in `steps/mod.rs`).
//!
//! The naming engine is pure, so the world is just a token map, the chosen space
//! replacement, and the last rendered string / path.
pub mod steps;

use std::path::PathBuf;

use skadi_naming::Tokens;

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    /// Token name → value, in declaration order.
    pub tokens: Vec<(String, String)>,
    /// The configured whitespace replacement (`naming.space`); `_` is the default.
    pub space: Option<char>,
    /// Last `render` / `tidy` / `sanitize` / `kebab` / `render_component` result.
    pub rendered: Option<String>,
    /// The raw string a sanitize step was given, so a later colon-style
    /// assertion can re-sanitize it (SKADI-T-0420).
    pub subject: Option<String>,
    /// Last `render_library_path` result.
    pub path: Option<PathBuf>,
    /// The root the last path was rendered under.
    pub root: Option<PathBuf>,
}

impl World {
    /// The token map borrowed from the world (the engine's `Tokens<'_>` keys are `&str`).
    pub fn tokens(&self) -> Tokens<'_> {
        self.tokens
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect()
    }

    pub fn space(&self) -> char {
        self.space.unwrap_or('_')
    }

    pub fn rendered(&self) -> &str {
        self.rendered.as_deref().expect("something was rendered")
    }

    pub fn path(&self) -> &std::path::Path {
        self.path.as_deref().expect("a path was rendered")
    }
}
