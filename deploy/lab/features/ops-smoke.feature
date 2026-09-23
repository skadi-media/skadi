# UNEXECUTED — documentation of expected behaviour (see deploy/lab/features/README.md).
@C37
Feature: Deploy stack bring-up and smoke checks
  The compose stack (deploy/docker-compose.yml) is an appliance: one `up -d`
  brings up postgres, gluetun, flaresolverr, autoheal, vpn-watchdog, the skadi
  daemon and the download worker, with the worker structurally behind the VPN.

  Background:
    Given a Docker host with Compose >= 2.24 (base + lab overlay use `!reset`/`!override`)
    And deploy/.env filled in from deploy/.env.example with the three secrets set

  @passing @lab
  Scenario: Full stack comes up in dependency order
    When the operator runs `docker compose up -d` from deploy/
    Then postgres becomes healthy (pg_isready) before the skadi daemon starts
    And gluetun becomes healthy (tunnel up) before flaresolverr and the worker start
    And the skadi daemon starts once gluetun's container exists, without waiting for the tunnel
    And `GET /api/v1/health` on the published port returns 200 with {"status":"ok"}

  @passing @lab
  Scenario: Kill switch — worker egress is the VPN exit
    Given the stack is up and gluetun is healthy
    When the operator runs `docker compose exec skadi-downloader-worker curl -s ifconfig.me`
    Then the reported address is the VPN exit, not the host's public address
    When the operator runs `docker compose stop gluetun`
    Then the same curl from the worker times out (no route outside the tunnel)

  @passing
  Scenario: Container logs are bounded
    Then every service carries `logging: json-file, max-size 10m, max-file 3`
    And gluetun's control-server access log is off (HTTP_CONTROL_SERVER_LOG=off)

  @passing
  Scenario: gluetun cannot starve the host
    Then gluetun has `mem_limit: 512m` in the base file
    And an OOM-kill of gluetun is followed by `restart: unless-stopped` and a watchdog bounce

  @bug
  Scenario: The angreal health gate must probe the API, not the SPA fallback
    # .angreal/task_deploy.py:22, task_lab.py:26, task_nas.py:93 poll `/health`;
    # the daemon only serves `/api/v1/health` (skadi-api/src/serve.rs:22) and
    # answers ANY unknown GET with index.html + 200 (skadi-api/src/assets.rs:36).
    Given the skadi container is listening but the database is unreachable
    When `angreal deploy up` waits for health
    Then it must NOT report "skadi healthy"
    And the probe URL must be `/api/v1/health`

  @gap
  Scenario: The daemon has a compose healthcheck
    # Only gluetun, flaresolverr and postgres have healthchecks (docker-compose.yml:145,225,310).
    Then `docker compose ps` shows skadi as healthy/unhealthy
    And a readiness probe that pings the database exists (e.g. /api/v1/health/checks or `skadi health`)
    And the runtime image contains a probe binary (curl or the CLI) so the healthcheck can run

  @gap
  Scenario: .env.example enumerates every variable the compose file reads (REQ-DEPLOY.9)
    Then SKADI_ADVERTISE_HOST is listed
    And SKADI_WORKER_DISABLE_DHT is listed
    And SKADI_BIND is listed (currently only as a comment)

  @gap
  Scenario: LSIO PUID/PGID parity for skadi and the worker
    # PUID/PGID/TZ in .env reach no service any more: skadi and the worker run
    # as the image's fixed uid 1000 unless the NAS overlay sets `user:`.
    Given PUID/PGID are set in .env
    Then files written by skadi and the worker are owned by PUID:PGID
