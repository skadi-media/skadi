# UNEXECUTED — documentation of expected behaviour (see deploy/lab/features/README.md).
@C37
Feature: Day-2 operations on the Mac/Docker Desktop host (angreal deploy)
  Evidence for the @passing/@bug scenarios comes from the 2026-09-06/07
  cut-over and rollback (SKADI-I-0056, T-0396/T-0397) and from reading
  .angreal/task_deploy.py. Docker Desktop was down during this review pass, so
  none of the @lab scenarios were re-run.

  @passing
  Scenario: Redeploy one service after a code change
    When the operator runs `angreal deploy redeploy --service skadi-downloader-worker`
    Then only that service's image is rebuilt and its container force-recreated
    And gluetun is never restarted (VPN reconnects are rate-limited by the provider)
    And the builder cache is pruned afterwards (a full Docker VM disk crashes postgres)

  @passing
  Scenario: Rollback to the previous image
    # Done 2026-09-06 21:35Z: the old image was kept as `:prev`, the tested build promoted to `:latest`.
    Given the previous worker image is tagged `skadi-downloader-worker:prev`
    When the operator tags `:prev` back to `:latest` and runs `docker compose up -d --force-recreate skadi-downloader-worker` from deploy/
    Then the worker restores its librqbit session from the shared state dir and re-attaches seeds without a full re-hash

  @bug
  Scenario: `angreal deploy up` must load deploy/.env explicitly and preflight the NFS variables
    # task_deploy.py:25-29 runs compose with cwd=<repo> and no --env-file. On
    # Compose v5.0.0 the project-directory .env is loaded regardless of cwd
    # (verified in an isolated test), so the 21:35Z "STORAGE_NFS_* not set"
    # warning is NOT reproducible via cwd — root cause unverified. The task
    # should not depend on compose's default resolution at all.
    When the operator runs `angreal deploy up` from any directory
    Then compose is invoked with `--env-file deploy/.env` and `--project-directory deploy`
    And if STORAGE_NFS_ADDR or STORAGE_NFS_PATH is empty the task refuses before `up`
      (an empty NFS address would create/mount the `storage` volume against a blank export)

  @gap
  Scenario: Ghost network guard before `up`
    # 2026-09-06 21:35Z: "Pool overlaps with other one on this address space" — an
    # orphaned network still held the project's subnet after a network teardown.
    Given a Docker network other than `skadi_default` owns the configured COMPOSE_SUBNET
    When the operator runs `angreal deploy up`
    Then the task stops before `compose up` and names the orphan network
    And prints the recovery: `docker network inspect <name>` (must have 0 containers), `docker network rm <name>`, re-run `up`

  @passing
  Scenario: Ghost network recovery (manual runbook)
    Given `compose up` fails with "Pool overlaps with other one on this address space"
    When the operator lists networks and inspects the one holding the subnet
    And removes it with `docker network rm` once it has no containers attached
    And runs `docker compose down` then `up -d` from deploy/
    Then the stack comes up on `skadi_default` with the pinned subnet

  @passing
  Scenario: A container stuck "Dead" / "removal already in progress"
    # Seen 2026-09-06 21:35Z on flaresolverr after a 10-minute `compose down`.
    Given a project container is in state Dead and `docker rm -f` reports "removal already in progress"
    Then the only reliable fix is a Docker Desktop restart (the daemon drops Dead containers on restart)
    And until then that service stays down (Cloudflare-fronted indexers report NoSuitableRelease)

  @passing
  Scenario: Restarting Docker Desktop from the CLI without racing the VM
    # `osascript quit` immediately followed by `open -a Docker` raced and left the VM down.
    When the operator quits Docker Desktop
    Then they wait until `docker info` fails AND the Docker process has exited before relaunching
    When they relaunch with `open -a Docker` (or from the GUI if the VM does not come back)
    Then they wait for `docker info` to succeed, then verify with `angreal deploy status`
    And containers with `restart: unless-stopped` return on their own

  @bug
  Scenario: `docker stop` on the worker or the daemon should be graceful
    # Neither binary handles SIGTERM: the daemon only listens for Ctrl-C/SIGINT
    # (crates/skadi-cli/src/main.rs:572); the worker installs no handler
    # (crates/skadi-downloader-worker/src/main.rs). Docker sends SIGTERM, waits
    # the stop timeout (10 s default), then SIGKILLs — every stop takes the full timeout.
    When Docker sends SIGTERM to the worker
    Then the worker cancels its loop, lets librqbit flush its session, and exits 0 within the grace period
    And the compose file sets `stop_grace_period` long enough for that flush

  @passing
  Scenario: Worker restart over NFS-on-Wi-Fi is slow but safe
    # 2026-09-06 22:05Z: 366 torrents re-added serially at ~4.5 s each = 28 min before the claim loop ran; ~1 min on the NAS.
    Given the worker state dir lives on the NFS `storage` volume
    When the worker container is (re)created
    Then `librqbit session up … restored=N fastresume=true` appears only after every torrent's files were opened over NFS
    And no download is claimed until then — the operator should expect tens of minutes on Wi-Fi NFS, not a hang

  @gap
  Scenario: Session restore off the critical path
    When the worker starts with hundreds of persisted torrents
    Then the heartbeat, orphan re-queue and claim loop start within seconds
    And seeds are re-attached in the background

  @gap
  Scenario: Scheduled database backup (Sonarr/Radarr "Backups" parity)
    Then a `pg_dump -Fc` of the `skadi` database is taken on a schedule to a retained location
    And the README documents the one-line manual backup and restore

  @passing
  Scenario: Manual database backup
    When the operator runs `docker compose exec -T postgres pg_dump -U skadi -Fc skadi > skadi-<date>.dump` from deploy/
    Then a restorable custom-format dump exists (the 2026-09-06 cut-over used exactly this, 141 MB, restore 2 m 39 s)
