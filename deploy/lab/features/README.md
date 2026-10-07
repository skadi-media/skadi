# Ops feature files (C37 Deploy Stack)

**Status: documentation only — NOT executed by any runner.**

These `.feature` files describe the *expected* operational behaviour of the
deploy stack (`deploy/docker-compose.yml` + overlays, the `angreal deploy|lab`
task groups, CI/GHCR). They follow the BDD conventions
(`@C37`, exactly one of `@passing | @gap | @bug`, `@lab` for scenarios that need
a Docker host) but there is no cucumber runner for ops in this pass: nothing under
`deploy/lab/features/` is compiled, collected by `angreal test bdd`, or run in CI.

Tag meaning here:

- `@passing` — behaviour verified by reading the artefacts and/or by the
  2026-09-06 cut-over/rollback evidence; where a claim
  could not be re-run in this pass (Docker Desktop was down) it says so.
- `@gap` — expected servarr/LSIO-parity behaviour the stack does not have.
- `@bug` — the stack does the wrong thing; the scenario asserts the correct
  behaviour and stays red until the proposed ticket lands.

`deploy/lab/ops-smoke.sh` is the checklist-style, read-only smoke script that a
future runner would wrap; it is scoped to compose project `skadi-lab` by default
and refuses `skadi` (production) unless explicitly allowed.
