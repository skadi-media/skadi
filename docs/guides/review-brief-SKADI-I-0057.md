
## Harness gotchas (learned in P3)
- Do NOT hold a `wiremock::MockServer` in the cucumber `World`: its `Drop` calls `futures::executor::block_on(verify)` on a tokio worker and with concurrent scenarios every worker parks (deadlock). Hand servers to a plain OS thread in `Drop` (see `crates/skadi-indexers/tests/bdd_support/mod.rs`).
- The support module is `tests/bdd_support/`, never `tests/bdd/` (E0761 against the `tests/bdd.rs` runner).
