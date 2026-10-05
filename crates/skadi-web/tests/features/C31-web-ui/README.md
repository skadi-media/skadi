# C31 web UI — executable-spec features

`skadi-web` is a Leptos/wasm crate outside the cargo workspace, so there is no
cucumber runner here. These `.feature` files are the executable spec text; the
steps are implemented as Playwright Given/When/Then helpers in
`web-e2e/tests/steps.ts` and driven by `web-e2e/tests/c31-web-ui.spec.ts`
against the mock-provider harness (`angreal test e2e-web`). Each scenario's
tag (`@passing` / `@gap` / `@bug`) mirrors the spec's `test`/`test.fixme`
status. Pure-logic assertions (formatters, roll-ups) already live in
`crates/skadi-web/tests/logic.rs` and DOM ones in `tests/dom.rs`
(`angreal test web`, wasm-bindgen-test in headless Chrome).
