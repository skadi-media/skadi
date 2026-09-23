//! Harness smoke test: proves the runner, world and tag filtering work for `skadi-core`.
use cucumber::{given, then, when};

use crate::bdd_support::World;

#[given("the BDD harness for this crate")]
fn harness(w: &mut World) {
    w.notes.push("harness".into());
}

#[when("a scenario runs")]
fn runs(w: &mut World) {
    w.notes.push("ran".into());
}

#[then("it passes")]
fn passes(w: &mut World) {
    assert_eq!(w.notes, vec!["harness", "ran"]);
}
