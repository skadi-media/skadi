# UNEXECUTED — documentation of expected behaviour (see deploy/lab/features/README.md).
@C37
Feature: CI, images and release workflows

  @passing
  Scenario: CI gates on fmt, clippy (workspace, no --tests) and check
    Then ci.yml `check` runs `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo check --workspace`
    And `angreal check all` runs the same three plus `angreal db schema --check` (a no-op without the diesel-dualdb CLI)

  @passing
  Scenario: The BDD job runs the workspace and worker cucumber suites
    Then ci.yml `test-bdd` runs `cargo test --workspace --test bdd` and the worker's `--test bdd`
    And @gap/@bug/@lab scenarios are skipped by default (SKADI_BDD_GAPS / SKADI_BDD_LAB unset)

  @bug
  Scenario: test-bdd should be a gate like the other suites
    # test-bdd has no `needs: check` and `coverage` does not depend on it; the workflow is uncommitted.
    Then `test-bdd` depends on `check` and `coverage` depends on `test-bdd`

  @gap
  Scenario: Test code is linted
    # `cargo clippy --tests` fails on pre-existing code in several crates (T-0429/0443/0457).
    Then CI and `angreal check all` run `cargo clippy --workspace --tests -- -D warnings`

  @bug
  Scenario: The worker crate is covered by CI lint/test jobs
    # The worker is excluded from the workspace; `--workspace` jobs (clippy, unit, integration, functional, coverage) never touch it. Only test-bdd builds it.
    Then `check`, `test-unit` and `test-integration` also run against `--manifest-path crates/skadi-downloader-worker/Cargo.toml`

  @bug
  Scenario: Release binaries include the web UI
    # release.yml runs `cargo build --release` (no `--features embed-ui`, no trunk), so the tarballs serve a placeholder at `/`.
    When a `v*` tag is pushed
    Then the packaged `skadi` binary was built with `--features embed-ui` after `trunk build --release`
    And a worker binary is packaged too (or the release notes say images are the supported artefact)

  @gap
  Scenario: Images job is gated and multi-arch
    Then images.yml has `needs` on CI (or is triggered by CI success)
    And it builds linux/amd64 and linux/arm64 so the Mac host can pull instead of building

  @gap
  Scenario: Compose files are validated somewhere
    # Nothing in CI runs `docker compose config` on base + overlays.
    Then a CI step renders base, base+lab and base+nas with placeholder env and fails on errors
