# UNEXECUTED — documentation of expected behaviour (see deploy/lab/features/README.md).
@C37
Feature: NAS overlay and `angreal nas` (SKADI-I-0056)
  The stack runs on the storage host under Container Manager: the library is a
  local bind mount, every service has a mem_limit, images come from GHCR or are
  shipped from the build host.

  @passing
  Scenario: NAS overlay renders without `!reset` (Container Manager compose 2.20)
    When `docker compose -f docker-compose.yml -f docker-compose.nas.yml config` runs with any env file
    Then `storage`, `nas-library`, `pg-data`, `cardigann-defs` are local bind volumes under NAS_MEDIA_DIR / NAS_DATA_DIR
    And skadi and the worker run as NAS_PUID:NAS_PGID with HOME=/tmp
    And every service has a mem_limit (skadi 768m, postgres 512m, worker 1g fuse, flaresolverr 512m, watchdogs 64m, gluetun 512m from base)
    And the images are ghcr.io/skadi-media/<image>:latest unless NAS_IMAGE_PREFIX is set empty

  @passing
  Scenario: `angreal nas push` renders one self-contained project file on the NAS
    When the operator runs `angreal nas push`
    Then <NAS_DATA_DIR>/deploy/src/ holds the two compose files, deploy/.env is deploy/.env + .env.nas (mode 600)
    And deploy/compose.yaml is rendered ON the NAS with `compose config`, the top-level `name:` stripped for Container Manager
    And pg-data is `chattr +C` while empty and cardigann-defs is chowned to NAS_PUID:NAS_PGID

  @passing
  Scenario: Cut-over data move
    Given the Mac's skadi and worker containers are stopped
    When the operator runs `angreal nas up --service postgres` then `angreal nas db-migrate`
    Then the Mac database is only read (pg_dump -Fc, kept under deploy/.nas-dump/) and restored with --clean into the NAS
    And row counts match (verified 2026-09-06 18:16Z)

  @passing
  Scenario: One worker and one VPN session at a time
    # 2026-09-06 17:50Z: the NAS worker restored the Mac's live session from the shared state dir.
    Given the worker state dir is under the shared library
    Then the worker must run on exactly one host
    And gluetun must run on exactly one host (a second NordLynx session gets throttled — 17:55Z)

  @passing
  Scenario: Relaxed flaresolverr healthcheck on the NAS
    # Base: 8 s timeout / 30 s interval; autoheal killed Chromium mid-solve on the V1500B.
    Then the overlay sets timeout 30s, interval 60s, retries 5, start_period 180s on flaresolverr

  @bug
  Scenario: Rollback must carry NAS-side writes back to the Mac
    # README says "Rollback = angreal nas down and start the Mac containers"; the
    # real rollback (21:35Z) also dumped the NAS DB and restored it into the Mac.
    When the operator rolls back after the NAS has accepted writes
    Then the runbook includes: freeze NAS (all but postgres) → dump NAS DB → restore into the Mac → `angreal nas down` → start Mac containers

  @gap
  Scenario: GHCR rollout is live
    # .github/workflows/images.yml is uncommitted; the overlay's default image ref does not exist yet.
    Given main is pushed
    Then ghcr.io/skadi-media/skadi:latest and skadi-downloader-worker:latest exist for linux/amd64
    And `angreal nas pull && angreal nas up --recreate` rolls them out
    And the images job runs only after CI's check/test jobs pass (today it has no `needs`)

  @passing @SKADI-T-0489
  Scenario: Bound concurrent downloads on the 4 GB box
    # 38 concurrent downloads pushed the worker to ~1 GiB + swap (T-0392 19:45Z).
    Then a `worker.max_active` setting caps simultaneous downloads and the NAS .env sets it
