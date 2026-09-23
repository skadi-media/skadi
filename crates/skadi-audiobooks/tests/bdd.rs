//! BDD runner for `skadi-audiobooks` (SKADI-I-0057 component review).
//!
//! Features live under `tests/features/<Cxx>-<name>/*.feature`; step definitions
//! under `tests/bdd_support/steps/`. Every scenario carries `@Cxx` and one of
//! `@passing` (verified), `@gap` (expected *arr behaviour the code lacks — steps
//! fail honestly), `@bug` (code is wrong; red until fixed; ticket id tag added once
//! filed) — both skipped unless `SKADI_BDD_GAPS=1`, `@lab` (needs the lab stack; skipped unless
//! `SKADI_BDD_LAB=1`). Run: `angreal test bdd --crate skadi-audiobooks [--gaps] [--lab]`.
// Support module lives in tests/bdd_support/ (a `bdd/` dir would clash with this file).
mod bdd_support;

use cucumber::World as _;

#[tokio::main]
async fn main() {
    let include_gaps = std::env::var_os("SKADI_BDD_GAPS").is_some();
    let include_lab = std::env::var_os("SKADI_BDD_LAB").is_some();
    bdd_support::World::cucumber()
        .fail_on_skipped()
        .filter_run_and_exit("tests/features", move |_feature, _rule, scenario| {
            let has = |t: &str| scenario.tags.iter().any(|x| x == t);
            (include_gaps || (!has("gap") && !has("bug"))) && (include_lab || !has("lab"))
        })
        .await;
}
