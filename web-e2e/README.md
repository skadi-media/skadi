# web-e2e

Playwright tests of the built web UI against `skadi-e2e-harness`, a daemon
with mock providers. `angreal test e2e-web` installs the npm packages and
Chromium on the first run, starts the harness through `run-harness.sh`, and
runs `tests/`.

- The specs share one harness for the whole run. A step that sets state up
  (`steps.givenMovieAdded`, `steps.givenDownloads`) makes it fresh rather than
  assuming a clean daemon.
- The harness's downloader is a mock that writes no download rows. Download
  rows for the Downloads and Activity pages come from the harness-only
  `POST /api/v1/e2e/downloads` (`steps.givenDownloads`).
- `role-ui.mjs` is a separate sweep, run by hand against a live stack, of
  what each household role is offered. Read its header before running it: it
  creates and deletes accounts on the stack it points at.

## The shared build cache

Worktrees of this repository share one `target/` when `CARGO_TARGET_DIR`
points at it. Building the UI or the harness from another worktree then
overwrites the binaries this run would use, and `cargo` may not rebuild them
for this checkout. Before a run in a worktree, touch
`crates/skadi-web/src/lib.rs` and `crates/skadi-e2e-harness/src/main.rs`,
or use a target directory of the worktree's own.
