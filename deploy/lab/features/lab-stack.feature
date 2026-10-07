# UNEXECUTED — documentation of expected behaviour (see deploy/lab/features/README.md).
@C37
Feature: Lab stack (compose project skadi-lab, SKADI-T-0393)
  A second, isolated copy of the deploy stack on the same Docker host for
  experiments — no VPN, local storage, its own subnet/ports/images.

  @passing
  Scenario: Lab is pinned away from production
    Then docker-compose.lab.yml sets `name: skadi-lab` and task_lab.py passes `-p skadi-lab`
    And `angreal lab reset` is the only `down -v` and asserts the project is `skadi-lab`
    And the lab uses deploy/.env.lab (no secrets) via `--env-file`, never deploy/.env
    And images are tagged `:lab`, the subnet is 172.29/16, the API is on 127.0.0.1:8091, postgres on 127.0.0.1:5434

  @passing
  Scenario: Lab overlay renders (verified client-side in this pass)
    When `docker compose -p skadi-lab --env-file deploy/.env.lab -f docker-compose.yml -f docker-compose.lab.yml config` runs
    Then gluetun, flaresolverr, autoheal and vpn-watchdog are behind the `vpn` profile
    And the worker has no `network_mode` and depends only on postgres
    And the `storage` named volume is removed and /mnt/storage is a bind of deploy/.lab/storage
    And three "STORAGE_NFS_* not set" warnings are printed and are harmless (the base file interpolates before the merge)

  @passing @lab
  Scenario: Lab bring-up (exercised repeatedly on 2026-09-06; not re-run here — Docker was down)
    When the operator runs `angreal lab up`
    Then postgres, skadi:lab and skadi-downloader-worker:lab start
    And prod's `skadi` project is unchanged (`docker compose ls` shows both)

  @gap @lab
  Scenario: Clean-checkout bring-up (SKADI-T-0393 criterion 4, still open)
    Given a fresh clone with no `:lab` images
    When the operator runs `angreal lab up`
    Then both images build, the daemon answers /api/v1/health on :8091 and the worker claims a lab download

  @passing
  Scenario: Lab-only knobs are inert in production
    Then LAB_BLOCKING_BURST → SKADI_WORKER_DEBUG_BLOCKING_BURST is set only by the lab overlay
    And LAB_DISABLE_DHT defaults to 1 in the lab (no public DHT announces) while prod's SKADI_WORKER_DISABLE_DHT defaults to empty (on)

  @bug
  Scenario: `angreal lab up --from-prod` must fail loudly if a tag is missing
    # task_lab.py:90-93 tags both images; if `docker tag` fails the task exits, but a
    # missing `:lab` tag with `--build` absent makes compose silently start a full build.
    Given `skadi:lab` exists but `skadi-downloader-worker:lab` does not
    When the operator runs `angreal lab up`
    Then the task says which image is missing instead of starting a multi-minute build
