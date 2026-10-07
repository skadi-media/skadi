Feature: C34 health & diagnostics
  `/health` is the unauthenticated liveness probe, `/health/ready` the
  unauthenticated readiness probe deploy gates poll; `/health/checks` is the
  dashboard's badge list (daemon, database, enabled domains, each provider's
  reachability). Sonarr's System → Status/Health also lists the download
  client, root folders, disk space, update and scheduled-task state.

  @C34 @passing
  Scenario: the liveness probe names the build
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/health"
    Then the response status is 200
    And the response field "status" is "ok"
    And the response field "version" is present

  @C34 @passing @SKADI-T-0475
  Scenario: readiness is not ready until the supervisor has published providers
    # The bug this pins: deploy gates polled bare `/health`, which the SPA
    # fallback answers 200 as soon as the socket binds — so `angreal deploy up`
    # called the stack healthy before migrations or providers existed.
    Given a daemon running in open mode
    When the client requests GET "/api/v1/health/ready"
    Then the response status is 503
    And the response field "status" is "not_ready"
    And the response field "database" is "true"
    And the response field "providers" is "false"
    And the response field "detail" is present

  @C34 @passing @SKADI-T-0475
  Scenario: readiness reports ready once the database answers and providers are published
    Given a daemon running in open mode
    And the supervisor has published providers
    When the client requests GET "/api/v1/health/ready"
    Then the response status is 200
    And the response field "status" is "ready"
    And the response field "database" is "true"
    And the response field "providers" is "true"

  @C34 @passing @SKADI-T-0475
  Scenario: the readiness probe needs no token, but the diagnostics checks still do
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/health/ready"
    Then the response status is 503
    When the client requests GET "/api/v1/health/checks"
    Then the response status is 401

  @C34 @passing
  Scenario: the health checks always include the daemon and the database
    Given a daemon running in open mode
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the response status is 200
    And the health check named "daemon" has status "ok"
    And the health check named "database" has status "ok"

  @C34 @passing
  Scenario: only enabled domains appear in the health checks
    Given a daemon running in open mode
    And the daemon compiles in the "movies" domain
    And the daemon compiles in the "television" domain
    And the "television" domain is enabled
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check named "domain:television" has status "ok"
    And there is no health check named "domain:movies"

  @C34 @passing
  Scenario: an unreachable indexer is a failing check with a reason, never an HTTP error
    Given a daemon running in open mode
    And a stored indexers setting remembered as "dead" with body:
      """
      { "kind": "torznab", "name": "deadixr", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "k" }
      """
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the response status is 200
    And the health check named "indexer:deadixr" has status "fail"

  @C34 @passing
  Scenario: the download worker is reported down until it heartbeats
    Given a daemon running in open mode
    When the client requests GET "/api/v1/downloads/worker"
    Then the response field "running" is "false"
    And the response field "stale_after_secs" is "45"
    Given the download worker "worker-a" heartbeated just now
    When the client requests GET "/api/v1/downloads/worker"
    Then the response field "running" is "true"
    And the response field "worker_id" is "worker-a"

  @C34 @passing @serial
  Scenario: an unreachable gluetun degrades the VPN panel instead of failing
    Given a daemon running in open mode
    And the environment variable "SKADI_GLUETUN_CONTROL_URL" is "http://127.0.0.1:1"
    When the client requests GET "/api/v1/downloads/vpn"
    Then the response status is 200
    And the response field "reachable" is "false"
    And the response field "connected" is "false"
    Given the environment variable "SKADI_GLUETUN_CONTROL_URL" is unset

  @C34 @passing @SKADI-T-0467
  Scenario: the library root is part of the health checks (SKADI-T-0430)
    Given a daemon running in open mode
    And the library root points at the missing path "/no/such/skadi/library"
    When the client requests GET "/api/v1/health/checks"
    Then some health check name starts with "root"

  @C34 @passing @SKADI-T-0467
  Scenario: a silent download worker is a failing health check
    Given a daemon running in open mode
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check named "worker" has status "fail"

  @C34 @passing @SKADI-T-0467
  Scenario: System → Status reports runtime, database and paths
    Given a daemon running in open mode
    When the client requests GET "/api/v1/system/status"
    Then the response status is 200
    And the response field "version" is present
    And the response field "startTime" is present
    And the response field "commit" is present

  @C34 @passing @SKADI-T-0467
  Scenario: System → Tasks lists the scheduled jobs with their next run
    Given a daemon running in open mode
    When the client requests GET "/api/v1/system/task"
    Then the response status is 200

  @C34 @passing @SKADI-T-0467
  Scenario: System → Logs exposes recent log lines over the API
    Given a daemon running in open mode
    When the client requests GET "/api/v1/log?pageSize=10"
    Then the response status is 200
